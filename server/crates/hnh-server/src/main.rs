//! hnh-server: an efficient, seed-fixed Haven & Hearth world server.
//!
//! One command on a developer machine:
//!   cargo run --release -- --seed 42
//! Ports: 1871/tcp TLS auth, 1870/udp game, 1872/tcp resource HTTP.

mod auth;
mod bots;
mod build;
mod chat;
mod craft;
mod farm;
mod fight;
mod game;
mod grid_owner;
mod net;
mod party;
mod persist;
mod res_http;
mod resources;
mod skills;
mod state;
mod visidx;

use std::sync::Arc;
use std::time::Duration;

use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer as _};

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
    let mut cands = Vec::new();
    // Exe-anchored candidates first: the built exe lives inside the repo
    // (server/target/<profile>), so this anchor is exact for every
    // build-based launch regardless of cwd. Cwd-relative candidates follow
    // as the fallback for a standalone exe copy. Cwd-first ordering was a
    // real bug: launched from the repo root with a stale ../save from an
    // older layout, the server loaded and persisted world.json outside the
    // repository.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // target/release -> server/target/release/../../.. -> repo root
            cands.push(dir.join(format!("../../../{name}")));
            cands.push(dir.join(format!("../../{name}")));
        }
    }
    cands.push(std::path::PathBuf::from(format!("../{name}")));
    cands.push(std::path::PathBuf::from(name));
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
    // Keep the writer guard alive for the whole process: dropping it stops
    // the background file writer and can lose the log tail on exit.
    let _log_guard = init_logging();
    info!(
        seed = args.seed,
        rev = ?std::env::var("HNH_REV").ok(),
        "hnh-server starting"
    );

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
    // Version announcements read the actual .res headers from this dir.
    resources::init_res_dir(res_dir.clone());
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

    // Then keep watching those listeners for the whole process lifetime.
    tokio::spawn(health_watchdog());

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
    Ok(())
}

/// stdout + append-only file logging (`logs/server.log`). The file is the
/// support channel: it survives the console closing and is what
/// `windows/collect-logs.bat` bundles into a single bug report. Returns the
/// writer guard that must outlive every log call, or None when only stdout
/// could be set up (log dir not creatable).
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let Some(dir) = log_dir() else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
        return None;
    };
    let appender = tracing_appender::rolling::never(&dir, "server.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stdout)
                .with_filter(filter.clone()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter),
        )
        .init();
    info!(dir = %dir.display(), "file logging enabled (append)");
    Some(guard)
}

/// Pick the log directory, exe-anchored first so the location is stable
/// across launch cwd: `server/target/<profile>/../../..` is the repo root
/// for both `cargo run` and the packaged exe.
fn log_dir() -> Option<std::path::PathBuf> {
    let mut cands = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("../../../logs"));
            cands.push(dir.join("../../logs"));
        }
    }
    cands.push(std::path::PathBuf::from("../logs"));
    cands.push(std::path::PathBuf::from("logs"));
    for c in &cands {
        // Use the first candidate we can create (or that already exists);
        // creation is idempotent for an existing dir.
        if std::fs::create_dir_all(c).is_ok() && c.is_dir() {
            return Some(c.clone());
        }
    }
    None
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

/// Periodically verify every TCP listener still accepts on the loopback.
/// A service that dies mid-session then produces a timestamped server-side
/// error instead of only a client-side "connection refused" long after the
/// actual failure. Probe connections are plain TCP (no TLS handshake),
/// which auth logs at debug level - expected noise.
async fn health_watchdog() {
    let targets = [
        ("auth", auth::AUTH_PORT),
        ("resource http", res_http::RES_PORT),
    ];
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // interval fires once immediately; skip that one
    loop {
        tick.tick().await;
        for (name, port) in targets {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err()
            {
                tracing::error!(
                    service = name,
                    port,
                    "health probe failed: listener is down"
                );
            }
        }
    }
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
