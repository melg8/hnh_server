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
| `run-client.bat` | **The one command.** Builds `build/haven.jar` if missing, auto-starts the server in its own window when it is not answering, waits until auth (1871) and resources (1872) respond, then starts the Java client. Client console output is captured to `logs/client.log`. |
| `start-server.bat` | Server only: builds, generates `gameres/` if needed, creates `save/`, runs the seed-42 server. Extra arguments pass through. Log file: `logs/server.log` (append). |
| `collect-logs.bat` | Bundles `logs/*.log` + environment info (git rev, java/cargo versions, server port status) into `logs/bugreport-<timestamp>.zip`. Safe to run while the server is up - logs are copied even while the server writes them. Run this and send the single zip when something breaks. |
| `build-server.bat` | `cargo build --release` for the Rust server. |
| `make-gameres.ps1` | Extracts `lib/haven-res.jar` into `gameres/` and overlays `res/compiled/` (auto-invoked by the start script when `gameres/` is missing). |
| `wait-server.ps1` | Helper used by `run-client.bat`: blocks until ports 1871+1872 answer (prevents the client racing a still-booting server). |
| `loadtest.bat` | Same as start but with 600 in-process bots in a saturated world and the 5-second `--perf` report. |

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
windows\loadtest.bat                        REM 600 bots, saturated world, perf report
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
