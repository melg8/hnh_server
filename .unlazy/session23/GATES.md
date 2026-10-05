# session23 — grid-owner partitioning (the multi-node 10k-target unit)

Continues the 10k-player backlog from session 22. The tick's parallel
phases previously chunked work items by count (par_chunks), which shares
nothing with the future multi-node layout. Work now groups by
VisIndex-cell OWNER (grid_owner.rs, rendezvous hashing): each partition
is the exact unit a separate node process would own in a multi-node
deployment, so the in-process fan-out exercises the partitioning
contract the cluster mode will rely on.

## Gates

### G1 server-builds-clean
cargo fmt check, clippy -D warnings, and all tests pass.

CHECK: cd server && cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace 2>&1 | grep "test result"
EXPECT: test result: ok (100 tests total, 0 failed)

### G2 rendezvous-owner-unit-tested
The ownership function is deterministic, in-range, single-node-owned,
balanced, and scale-out-safe: joining a node moves only the share the
NEW node wins (never between existing nodes); partitions cover every
item exactly once and preserve input order within a partition.

CHECK: cd server && cargo test grid_owner 2>&1 | grep "test result"
EXPECT: test result: ok (6 passed; 0 failed)

### G3 tick-partitioned-in-code
The parallel phases group by grid-owner partition, not by count: animal
intents (tick_animals) and session visibility scans (update_visibility
phase A2) both route through grid_owner::partition_by_owner.

CHECK: rg -n "partition_by_owner" server/crates/hnh-server/src/game.rs | wc -l
EXPECT: ^[2-9]$

### G4 load-budget-holds
300 moving bots at workers=4 keep mean tick well inside the 100 ms
budget with the partitioned fan-out (session 21-22 baseline was
~4.5-4.8 ms).

CHECK: server with --bots 300 --perf --workers 4; read the perf lines
EXPECT: mean_tick_us < 10000 (measured ~4400-4900)

### G5 real-client-regression
The full real-client e2e (GL render path) passes with the partitioned
tick at workers=4 AND at the default serial path: login portrait
layers, movement, five directional walk legs, equipment doll, animals
in view.

CHECK: (manual) run scripts/jogl/run-real-client-e2e.sh <user> <tag> --workers 4 and without extra args; read /tmp/client_<tag>.log
EXPECT: PORTRAIT LAYERS standing set; MOVEMENT/MOVEMENT2 MOVED; WALKDIR
east/north/south/up/left ARRIVED; EQUIP DOLL ava-rend=OK; ANIMALS
screenshot (s23a run) or hunt fallback (s23b run - hunt outcome depends
on predator placement; wire spawn coverage stays in G6 of session 21).
