//! hnh-server: an efficient, seed-fixed Haven & Hearth world server.
//!
//! One command on a developer machine:
//!   cargo run --release -- --seed 42
//! Ports: 1871/tcp TLS auth, 1870/udp game, 1872/tcp resource HTTP.

mod auth;
mod bots;
mod game;
mod handoff;
mod net;
mod res_http;
mod resources;
mod state;

use std::sync::Arc;
use std::time::Duration;

use tracing::info;
use tracing_subscriber::EnvFilter;

use auth::AuthServer;
use game::Game;

/// Global auth handle: cookies are issued by the TLS listener and consumed
/// by the UDP session accept; both are process-local.
static AUTH: std::sync::OnceLock<Arc<AuthServer>> = std::sync::OnceLock::new();

pub fn auth() -> Arc<AuthServer> {
    Arc::clone(AUTH.get().expect("BUG: auth initialized in main"))
}

struct Args {
    seed: u64,
    bots: usize,
    saturated: bool,
    shards: usize,
    res_dir: String,
    cert: String,
    key: String,
    perf: bool,
}

fn usage() -> &'static str {
    "hnh-server [--seed N] [--bots N] [--saturated] [--shards N] [--perf] [--res-dir DIR] [--cert P] [--key P]\n"
}

fn parse_args() -> Args {
    let mut a = Args {
        seed: 42,
        bots: 0,
        saturated: false,
        shards: 1,
        res_dir: "../gameres".to_owned(),
        cert: "certs/authsrv.crt.pem".to_owned(),
        key: "certs/authsrv.key.pem".to_owned(),
        perf: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--seed" => a.seed = it.next().and_then(|v| v.parse().ok()).unwrap_or(42),
            "--bots" => a.bots = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--saturated" => a.saturated = true,
            "--shards" => a.shards = it.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--perf" => a.perf = true,
            "--res-dir" => a.res_dir = it.next().unwrap_or_else(|| a.res_dir.clone()),
            "--cert" => a.cert = it.next().unwrap_or_else(|| a.cert.clone()),
            "--key" => a.key = it.next().unwrap_or_else(|| a.key.clone()),
            "--help" | "-h" => {
                print!("{}", usage());
                std::process::exit(0);
            }
            other => eprintln!("unknown arg: {other}"),
        }
    }
    a
}

fn main() -> anyhow::Result<()> {
    let args = parse_args();
    // Select the ring crypto provider explicitly: dependency features make
    // automatic detection ambiguous (rustls 0.23 requirement).
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("BUG: crypto provider install");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    info!(seed = args.seed, "hnh-server starting");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async_main(args))
}

async fn async_main(args: Args) -> anyhow::Result<()> {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<game::Cmd>();
    let (net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel::<net::NetCmd>();

    let auth = Arc::new(AuthServer::new());
    if AUTH.set(Arc::clone(&auth)).is_err() {
        return Err(anyhow::anyhow!("auth initialized twice"));
    }

    // Game task.
    let game = Game::new(args.seed, cmd_rx, net_rx, args.saturated);
    let game_handle = tokio::spawn(game.run());

    // Network tasks: shard_count UDP sockets share the port via SO_REUSEPORT.
    let shard_count = args.shards.max(1);
    info!(shards = shard_count, "network sharding");
    tokio::spawn(async move {
        if let Err(e) = net::spawn(net_tx, shard_count).await {
            tracing::error!(error = %e, "net failed");
        }
    });
    let (cert, key) = (args.cert.clone(), args.key.clone());
    let auth_handle = {
        let auth_inner = Arc::clone(&auth);
        tokio::spawn(async move { auth_inner.run(&cert, &key).await })
    };

    let res_dir = std::path::PathBuf::from(&args.res_dir);
    let res_handle = tokio::spawn(async move {
        if let Err(e) = res_http::spawn(res_dir).await {
            tracing::error!(error = %e, "resource server failed");
        }
    });

    // Optional in-process bots (load testing).
    if args.bots > 0 {
        tokio::spawn(bots::run(args.bots));
    }

    // Perf reporter.
    if args.perf {
        let cmd_tx = cmd_tx.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if cmd_tx.send(game::Cmd::ReportPerf {}).is_err() {
                    break;
                }
            }
        });
    }

    tokio::signal::ctrl_c().await?;
    info!("shutting down");
    game_handle.abort();
    auth_handle.abort();
    res_handle.abort();
    handoff::refresh(args.seed);
    Ok(())
}
