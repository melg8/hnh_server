# GATES — Session 33: cross-node station menus, owner-filtered populate

## Gates

### G1: station publish carries the readiness snapshot
A station publishes as StaticClass::Station with a StationView
{spec, lit, fuel, has_input} piggybacked on the guest payload; the
subscriber re-renders the lit sprite from the sdt byte on every
re-published GuestUpdate (same wire shape as crop stages).
CHECK: bash server/scripts/verify_session33.sh station
EXPECT: ALL PASS

### G2: guest station interaction round-trip
A player homed on node N clicks an oven on node M: the flower menu
opens LOCALLY from the snapshot (no round trip), the choice relays
RelayStationAct, M re-validates against its own StationState (stale ->
silent), applies the same transitions as the local path and answers
StationAck; refusals render the EXACT system lines the local path
emits. Fuel/input clicks relay the held stack; the cursor is consumed
one unit at a time ONLY on the ok ack (seed-safe).
CHECK: bash server/scripts/verify_session33.sh station
EXPECT: ALL PASS

### G3: owner-filtered populate (no shadow statics)
on_mapreq spawns statics/animals only for cells THIS node owns; a
non-owner node never carries a copy (its rng would place different
animals than the owner's roll). A peer's Sub materializes the
authority's part of every touched grid and announces every gob it
holds in the subscribed cells.
CHECK: bash server/scripts/verify_session33.sh populate
EXPECT: ALL PASS

### G4: wire regression (single-node populate unchanged)
The real UDP path still boots a client into the world and the farming
flow passes (single-node populate passes None - identical behavior).
CHECK: bash server/scripts/verify_session33.sh wire
EXPECT: ALL PASS

### G5: cluster + load regression (Sub-driven populate on the real mesh)
The session-30 story passes on this tree: 2-node real cluster, 60-bot
cohorts, tick budget, sharded save, restart restore - bots near cell
boundaries now render guest statics from the owner through the
Sub-driven populate instead of local shadows.
CHECK: bash server/scripts/verify_session33.sh cluster
EXPECT: SESSION30 E2E: OK

### G6: unit battery green (regression)
All existing unit tests plus the 11 new session-33 tests pass
(187 total).
CHECK: cargo test --release --quiet
EXPECT: 0 failed

### G7: zero-warning build
CHECK: cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
EXPECT: exit 0

### G8: committed and pushed to master
CHECK: git status clean; HEAD pushed to origin/master
EXPECT: ALL PASS
