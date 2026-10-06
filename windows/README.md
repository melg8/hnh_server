# Windows launch tools

One-command start for a developer machine, no Linux assumptions.

## Prerequisites

| Tool | Used by | Install |
| --- | --- | --- |
| Rust 1.75+ | `build-server.bat` | <https://rustup.rs> |
| JDK 21 (Temurin) + Ant 1.10.x | `run-client.bat` | <https://adoptium.net>, <https://ant.apache.org> |

Nothing else. The server generates its TLS certificate and the resource
pack automatically on first start.

## Scripts

| Script | What it does |
| --- | --- |
| `run-client.bat` | **The one command.** Rebuilds `build/haven.jar` whenever it is missing or the checked-out HEAD moved since the last build (stale-jar guard: a `git pull` can never silently run old client code), generates `gameres/` when stale, auto-starts the server in its own window when it is not answering, waits until auth (1871) and resources (1872) actually answer, then starts the Java client. The client loads its base resources from `gameres/` on disk (`-Dhaven.resdir`), so a res-server hiccup cannot crash it. Client console output is captured to `logs/client.log`. |
| `start-server.bat` | Server only: builds, regenerates `gameres/` when it is missing or older than the checked-out tree (rev-stamped `gameres/.genrev`), creates `save/`, runs the seed-42 server. Extra arguments pass through. Log file: `logs/server.log` (append). |
| `start-cluster.bat` | Two-node cluster on one machine, one command. Same build/gameres guards as `start-server.bat`; node 0 keeps the client-facing ports (client connects normally via `run-client.bat`), node 1 listens on auth 1873 / game 1874 / res 1876. Each node persists to its own shard (`save\cluster_n0.json`, `save\cluster_n1.json`); a character created on one node migrates to whichever node the session lands on. Logs: `logs\cluster-n0.log`, `logs\cluster-n1.log`. To log in through node 1 start the client with `-Dhaven.authserv=127.0.0.1:1873`. |
| `collect-logs.bat` | Bundles `logs/*.log` + environment info (git rev, java/cargo versions, server port status) into `logs/bugreport-<timestamp>.zip`. Safe to run while the server is up - logs are copied even while the server writes them. Run this and send the single zip when something breaks. |
| `build-server.bat` | `cargo build --release` for the Rust server. |
| `make-gameres.ps1` | Extracts `lib/haven-res.jar` into `gameres/` and overlays `res/compiled/` (auto-invoked by the start script when the pack is stale). |
| `wait-server.ps1` | Helper used by `run-client.bat`: blocks until auth (1871) accepts TCP **and** the resource server (1872) answers a real HTTP GET for `gfx/hud/fbtn` (prevents the client racing a still-booting or half-serving server). |
| `loadtest.bat [bots]` | Saturated world + a full bot cohort (default 1000) that walks, fights, harvests and loots through the real UDP path, with the 5-second `--perf` report. |

## Typical flow

One command - the server starts automatically if it is not running:

```bat
windows\run-client.bat
```

Prefer to control the server yourself? Keep using two terminals:

```bat
:: 1. terminal 1 - the server
windows\start-server.bat

:: 2. terminal 2 - the client
windows\run-client.bat
```

## When something breaks

```bat
windows\collect-logs.bat
```

then send the single `logs/bugreport-<timestamp>.zip` it prints. The zip
contains the server log (append-only across restarts, every line stamped
with the source revision), the last client run log, and version info.

Log in with any username/password: the dev auth auto-provisions
characters. Fresh characters carry a starter kit (branches, stones,
meat) so the full loop - craft a stone axe, roast/eat food - works
immediately.

## Useful server arguments

```bat
windows\start-server.bat --seed 42          REM default; fixed-seed reproducible world
windows\start-server.bat --workers 4        REM data-parallel tick fan-out
windows\start-server.bat --shards 2         REM 2 UDP sockets (SO_REUSEPORT), kernel load spread
windows\start-server.bat --bots 300 --perf  REM in-process load test + 5 s perf report
windows\loadtest.bat 1000                   REM 1000 bots walk/fight/harvest/loot, perf report
```

`--perf` prints `tick_us` every 5 seconds; the 10 Hz simulation budget is
100 000 us. Expect ~4 000-40 000 us depending on bot count and host.

## Verify the wire protocol without the GUI

```
cd server
python scripts\test_client.py testuser   REM world-entry smoke test
python scripts\test_craft.py crafttest   REM craft + eat (FEP) flow
```

Both print `OK` lines and exit 0 on success.

## Notes

- `save/world.json` holds character persistence (seed-bound); delete it
  for a fresh character database. The world itself is a pure function of
  `--seed`, so terrain regenerates identically.
- The self-signed `server/certs/authsrv.key.pem` + `.crt.pem` are
  generated at boot when absent and are gitignored.
- `gameres/` is a generated artifact (gitignored); regenerate any time
  with `powershell -File windows\make-gameres.ps1`.
- JOGL 1.1 natives (`jogl.dll`, `jogl_awt.dll`, `jogl_cg.dll`,
  `gluegen-rt.dll`, x86-64, committed in `build/`) are loaded through
  `java.library.path`, which `run-client.bat` sets for you. If you
  start the client manually, pass
  `-Djava.library.path=build` (and prepend `build` to `PATH`) or the
  JVM fails with `UnsatisfiedLinkError: no jogl`. On JDK 21+ add
  `--enable-native-access=ALL-UNNAMED` to silence the restricted-method
  warning from JOGL's `System::loadLibrary` call.
