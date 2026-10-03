//! hnh-server: an efficient, seed-fixed Haven & Hearth world server.
//!
//! One command on a developer machine:
//!   cargo run --release -- --seed 42
//! Ports: 1871/tcp TLS auth, 1870/udp game, 1872/tcp resource HTTP.

mod auth;
mod bots;
mod craft;
mod fight;
mod game;
mod handoff;
mod net;
mod persist;
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

/// First candidate that exists on disk, for default-path resolution.
fn first_existing(cands: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    cands.iter().find(|c| c.exists()).cloned()
}

/// Resolve a repo-root directory name (e.g. "gameres", "save", "certs")
/// against every launch layout so the server works from any cwd:
/// cargo run (cwd = server/), start-server.bat (cwd = repo root), or a
/// direct exe launch from anywhere. All candidates converge on the
/// repo-root copy. When nothing exists yet, fall back to the cwd-relative
/// name; creators (the start script, dev-cert generation, persistence)
/// own making the directory.
fn default_repo_dir(name: &str) -> std::path::PathBuf {
    let mut cands = vec![
        std::path::PathBuf::from(format!("../{name}")),
        std::path::PathBuf::from(name),
    ];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // target/release -> server/target/release/../../.. -> repo root
            cands.push(dir.join(format!("../../../{name}")));
            cands.push(dir.join(format!("../../{name}")));
        }
    }
    first_existing(&cands).unwrap_or_else(|| std::path::PathBuf::from(name))
}

struct Args {
    seed: u64,
    bots: usize,
    saturated: bool,
    shards: usize,
    workers: usize,
    res_dir: Option<String>,
    cert: Option<String>,
    key: Option<String>,
    perf: bool,
}

fn usage() -> &'static str {
    "hnh-server [--seed N] [--bots N] [--saturated] [--shards N] [--workers N] [--perf] [--res-dir DIR] [--cert P] [--key P]\n"
}

fn parse_args() -> Args {
    let mut a = Args {
        seed: 42,
        bots: 0,
        saturated: false,
        shards: 1,
        workers: 1,
        res_dir: None,
        cert: None,
        key: None,
        perf: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--seed" => a.seed = it.next().and_then(|v| v.parse().ok()).unwrap_or(42),
            "--bots" => a.bots = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--saturated" => a.saturated = true,
            "--shards" => a.shards = it.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--workers" => a.workers = it.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            "--perf" => a.perf = true,
            "--res-dir" => a.res_dir = it.next(),
            "--cert" => a.cert = it.next(),
            "--key" => a.key = it.next(),
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

    // Game task. Worker count sets the data-parallel tick fan-out (animal
    // AI intents + visibility scans run on rayon's pool when > 1).
    let workers = args.workers.max(1);
    info!(workers, "tick workers");
    let save_path = match std::env::var("HNH_SAVE_FILE") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => default_repo_dir("save").join("world.json"),
    };
    let game = Game::new(args.seed, cmd_rx, net_rx, args.saturated, save_path);
    let mut game = game;
    game.workers = workers;
    let game_handle = tokio::spawn(game.run());

    // Network tasks: shard_count UDP sockets share the port via SO_REUSEPORT.
    // Awaiting here makes shard bind failures fatal at startup instead of a
    // background log line the user only notices when clients misbehave.
    let shard_count = args.shards.max(1);
    info!(shards = shard_count, "network sharding");
    net::spawn(net_tx, shard_count).await?;
    let (cert, key) = match (&args.cert, &args.key) {
        (Some(c), Some(k)) => (c.clone(), k.clone()),
        (None, None) => {
            // Default dev pair: keep one cert per repo regardless of cwd.
            let dir = default_repo_dir("certs");
            (
                dir.join("authsrv.crt.pem").display().to_string(),
                dir.join("authsrv.key.pem").display().to_string(),
            )
        }
        _ => return Err(anyhow::anyhow!("--cert and --key must be set together")),
    };
    let auth_handle = {
        let auth_inner = Arc::clone(&auth);
        tokio::spawn(async move { auth_inner.run(&cert, &key).await })
    };

    // Resource HTTP server: fail fast on a missing resource pack or a taken
    // port. A half-alive server is worse than no server - the client's only
    // symptom would be a cascade of resource load errors after login.
    let res_dir = match &args.res_dir {
        Some(dir) => std::path::PathBuf::from(dir),
        None => default_repo_dir("gameres"),
    };
    if !res_dir.is_dir() {
        return Err(anyhow::anyhow!(
            "resource dir '{}' not found; generate it with \
             powershell -File windows/make-gameres.ps1 (extracts lib/haven-res.jar) \
             or pass --res-dir DIR",
            res_dir.display()
        ));
    }
    let res_listener = res_http::bind().await.map_err(|e| {
        anyhow::anyhow!(
            "cannot bind resource http port {}: {e:#}",
            res_http::RES_PORT
        )
    })?;
    tokio::spawn(res_http::serve(res_listener, res_dir));

    // Optional in-process bots (load testing).
    if args.bots > 0 {
        tokio::spawn(bots::run(args.bots));
    }

    // Startup self-check: reach every TCP listener once before announcing
    // readiness. Catches silent service death (bad cert, blocked port) at
    // the console instead of as confusing client-side connection refusals.
    startup_probe().await?;

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

    // Stop on SIGINT or SIGTERM; both follow the graceful path so the game
    // loop can flush character persistence.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = wait_sigterm() => {}
    }
    info!("shutting down");
    // Graceful path: let the game loop flush character persistence, then
    // stop the remaining listeners.
    let _ = cmd_tx.send(game::Cmd::Shutdown {});
    let _ = game_handle.await;
    auth_handle.abort();
    handoff::refresh(args.seed);
    Ok(())
}

/// Verify every TCP listener is actually reachable on the loopback before
/// the server announces readiness. A service that died at bind time (port
/// taken, bad certificate) otherwise only surfaces as client-side
/// "connection refused" errors long after startup.
async fn startup_probe() -> anyhow::Result<()> {
    let targets = [
        ("auth", auth::AUTH_PORT),
        ("resource http", res_http::RES_PORT),
    ];
    for (name, port) in targets {
        let mut reachable = false;
        for _ in 0..20 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                reachable = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if !reachable {
            return Err(anyhow::anyhow!(
                "startup self-check failed: {name} tcp/{port} not reachable"
            ));
        }
        info!(service = name, port, "startup self-check OK");
    }
    Ok(())
}

/// Resolve when SIGTERM arrives.
#[cfg(unix)]
async fn wait_sigterm() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            term.recv().await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "sigterm handler unavailable");
            std::future::pending::<()>().await;
        }
    }
}

/// Windows has no SIGTERM; the ctrl_c branch of the select covers the
/// console Ctrl+C path, so this future simply never resolves.
#[cfg(not(unix))]
async fn wait_sigterm() {
    std::future::pending::<()>().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_existing_prefers_earlier_candidates() {
        let dir = std::env::temp_dir().join(format!("hnh-paths-{}", std::process::id()));
        let hit = dir.join("present");
        std::fs::create_dir_all(&hit).unwrap();
        let cands = vec![dir.join("absent"), hit.clone(), dir.join("also-absent")];
        assert_eq!(first_existing(&cands), Some(hit));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn first_existing_none_when_all_missing() {
        let cands = vec![std::path::PathBuf::from("definitely-missing-dir-xyz")];
        assert_eq!(first_existing(&cands), None);
    }
}
