# GATES — session 32: cross-node plowing (TileMutation broadcast), tilth decay revert

## Gates

### G1: cross-node plow relay round-trip
A player homed on node N plows a furrow whose tile lives in node M's cell:
the act relays to M (the tile authority), M validates against ITS state,
mutates, answers PlowAck (stamina drains on the home node only on the ok
ack), and broadcasts TileMutation so EVERY node holding that grid renders
the furrow and re-sends MAPDATA to its local holders. The home node never
mutates its own grid while relaying (no shadow furrow).
CHECK: bash server/scripts/verify_session32.sh relay-plow
EXPECT: ALL PASS

### G2: tilth decay reverts the furrow to grass
An unplanted furrow whose decay deadline passes reverts its tile to GRASS
(both the live grid and the persisted override), re-sends the grid to
holders, and in cluster mode broadcasts TileMutation so peers converge.
The tile is re-plowable after the revert (no stuck-furrow dead end).
CHECK: bash server/scripts/verify_session32.sh decay-revert
EXPECT: ALL PASS

### G3: wire-protocol regression (farming + world entry)
The real UDP path still boots a client into the world and the farming
wire flow (plow -> plant -> harvest) still passes end to end.
CHECK: bash server/scripts/verify_session32.sh wire
EXPECT: ALL PASS

### G4: cluster + load regression (session 30 story)
The two-node cluster load/restart story still passes on this tree
(relay-static, cursor-merge, vis-cache, 600-bot budget window, cluster
load, graceful flush, shard persistence, full-restart restore).
CHECK: bash server/scripts/verify_session30.sh
EXPECT: SESSION30 E2E: OK

### G5: unit battery green (regression)
All existing unit tests plus the new session-32 tests pass.
CHECK: cargo test --all --quiet
EXPECT: test result: ok (exit 0)

### G6: zero-warning build
CHECK: cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
EXPECT: exit 0
