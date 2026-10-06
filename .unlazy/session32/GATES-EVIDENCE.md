# GATES evidence — session 32 (2026-10-06)

## G1: cross-node plow relay round-trip — MET
- `verify_session32.sh relay-plow`: units 3 + 1 OK.
- `verify_session32.sh cluster-plow` on the REAL 2-node cluster (TCP mesh +
  real UDP): probe_plow.py picked tile (tx,ty) with cell owner node 1,
  client observed the PLOWED tile byte in the re-sent MAPDATA
  ("PLOW RELAY: OK"), node0 log carries "relay plow act sent" +
  "remote tile mutation applied", node1 log carries "relay plow applied".
- Two verification defects found and fixed while proving it (see the
  session log): a stale release binary (cargo test does not rebuild the
  bin target) and a python owner_of port missing the 64-bit truncation
  of the multiply stage.

## G2: tilth decay reverts the furrow to grass — MET
- `verify_session32.sh decay-revert`: units 2 OK (decay revert broadcast +
  resident/non-resident remote mutation paths).
- Fuzz inside the unit battery: re-plow after revert succeeds (no stuck
  dead end), override map reverted to GRASS.

## G3: wire-protocol regression — MET
- `verify_session32.sh wire`: WORLD ENTRY: OK + FARMING FLOW: OK.
- Harness fix included: test_farming.py now uses a per-run isolated save
  (the shared fixed path accumulated restored crops across runs and the
  first-seen-crop click made the yield assertion flaky - server-side
  acts were correct per the debug logs; documented in HANDOFF).

## G4: cluster + load regression — MET
- `verify_session30.sh` end to end: relay-static 5/5, cursor-merge 3/3,
  vis-cache unit + 600-bot window p50=10.3ms max=46ms (budget 100ms),
  cluster load n0 10.9ms / n1 12.7ms max ticks, 60+60 shard persistence,
  restart restore 60+60. SESSION30 E2E: OK.

## G5: unit battery — MET
- cargo test --release: 176 passed (11 hnh-proto + 156 hnh-server incl. 5
  new session-32 tests + 9 hnh-world incl. 1 new), 0 failed.

## G6: zero-warning build — MET
- cargo clippy --all-targets -- -D warnings: Finished, no warnings.
- cargo fmt --all -- --check: clean.

ALL GATES MET.
