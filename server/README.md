# hnh_server — Rust implementation

A seed-fixed, horizontally-scalable Haven & Hearth world server speaking
the legacy session protocol. See `../HANDOFF.md` for the full state and
`../docs/mechanics/` for the mechanics reference.

## Quick start (one command)

```bash
# from repo root; generates gameres/ on first run (needs unzip)
./server/run.sh
```

or manually:

```bash
cd server
cargo run --release -- --seed 42            # world seed; same seed = same world
# optional flags:
#   --bots N        spawn N in-process load-test bots (real UDP path)
#   --saturated     dense objects + wildlife around spawn for visual load tests
#   --perf          periodic perf stats
```

Ports: 1871/tcp TLS auth, 1870/udp game session, 1872/tcp resource HTTP.

## Scaling

- `--workers N` sets the data-parallel tick fan-out (vertical scaling).
  Work items group by grid-owner partition (see `src/grid_owner.rs`):
  each rayon task processes one owner's slice of the cell lattice, the
  same unit a separate node process would own in a multi-node layout.
  300 moving bots tick in ~4.5-5 ms (100 ms budget) at workers=4.
- `--shards N` opens N UDP sockets sharing port 1870 via SO_REUSEPORT
  (session accept fan-out on one host).
- Ownership uses rendezvous hashing: deterministic, no shared state,
  and scale-out moves only ~1/(N+1) of the lattice to the joiner while
  existing nodes keep their cells.

## Client

```bash
ant jar    # JDK 21 + Ant
java -cp build/haven.jar:lib/* haven.MainFrame
```

The client connects to 127.0.0.1 by default (auth 1871, game 1870) and
fetches resources from http://127.0.0.1:1872/. `-Dhaven.autoplay=Player`
skips the character screen; `-Dhaven.pinnedcert` restores legacy cert
pinning.

## Tests

```bash
cd server && cargo test
python3 scripts/test_client.py testuser   # end-to-end protocol check
```
