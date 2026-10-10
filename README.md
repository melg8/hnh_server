# hnh_server — a Rust server for Haven & Hearth

A from-scratch, byte-exact reimplementation of the Haven & Hearth game
server (Rust), paired with a minimally patched legacy Java client that
connects to it out of the box. One developer machine is enough to run
the whole game; the same server architecture scales horizontally
(grid-owner clusters) toward the 10k-player target.

- **Rust server** (`server/`): auth (TLS), game (UDP), resource HTTP
  services; seed-fixed seamless world generation; persistence;
  multi-node grid-owner clustering; the full survival loop (gathering,
  crafting, farming, livestock, combat, building).
- **Java client** (`src/`): the legacy Haven & Hearth client, patched
  to talk to the local server (auth/ports/resources); no launcher
  account needed - any username/password provisions a character.
- **Game mechanics reference** (`docs/mechanics/`): the verified,
  source-cited behavioral blueprint the server implements.

## Quick start (Linux / macOS)

```bash
# 1. Build the server (cargo 1.75+)
cd server && cargo build --release

# 2. Generate the resource pack once (needs lib/haven-res.jar present)
server/scripts/make-gameres.sh

# 3. Run the server (ports: 1871/tcp auth, 1870/udp game, 1872/tcp resources)
./server/target/release/hnh-server --seed 42

# 4. Build and start the client (JDK 21 + Ant 1.10)
ant jar
java -cp build/haven.jar:lib/* haven.MainFrame
```

Log in with any username and password - the dev server auto-provisions
it. The world persists across restarts (`server/save/world.json`,
autosave every 30 s + graceful shutdown flush).

## Quick start (Windows)

`windows\run-client.bat` is the one command: it builds the server when
needed, regenerates a stale resource pack, auto-starts the server,
waits for the ports, and launches the client. `start-cluster.bat`
boots a two-node cluster on one machine. See `windows/README.md` for
every script, and `windows/collect-logs.bat` for one-file bug reports.

## Quick start (multi-machine cluster)

One command boots a local 2..4-node cluster (grid ownership splits by
a stable hash over the nodes):

```bash
cd server && ./scripts/cluster-up.sh up      # node 0 keeps ports 1871/1870
```

For real multi-machine deployment, run on each host (same seed):

```bash
CLUSTER_SPEC=hostA:18790,hostB:18791 SELF=0 ./scripts/cluster-up.sh remote   # host A
CLUSTER_SPEC=hostA:18790,hostB:18791 SELF=1 ./scripts/cluster-up.sh remote   # host B
```

Characters migrate across nodes on re-login through any node's ports
(node i: auth 1871+2i, game 1870+4i, res 1872+4i, mesh 18790+i).

## Verifying and load-testing

```bash
cd server
cargo test --workspace        # unit + wire integration (339 tests)
./target/release/hnh-server --seed 42 --bots 1000 --saturated --perf
                              # 1000 walking/fighting bots, 5 s perf reports
python3 scripts/test_client.py testuser   # WORLD ENTRY: OK (live smoke)
./scripts/test_cluster.sh              # CLUSTER E2E: OK  (local 2-node)
./scripts/test_remote_cluster.sh       # REMOTE CLUSTER: OK (per-machine,
                                        # mesh + guest walk + migration)
```

The richer scenario probes (crafting, farming, stations, melee, the
bake chain) live in `server/scripts/` - see
`server/scripts/README.md` for the probe table and verdict lines.

## Repository map

| Path | What it is |
| --- | --- |
| `server/` | The Rust server (crates: `hnh-proto`, `hnh-world`, `hnh-server`) |
| `src/`, `build.xml` | The legacy Java client and its build |
| `docs/mechanics/` | Verified game-mechanics reference (start at its README) |
| `server/scripts/` | Python wire probes + the shared `hnhlib.py` harness |
| `windows/` | One-command Windows launch tools |
| `AGENTS.md` | Mandatory guidelines for AI coding agents |
| `HANDOFF.md` | The cross-session engineering log (living context) |
