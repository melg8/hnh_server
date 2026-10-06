# Session 43 Gates

Scope: the combat-phase attribution (session-42 NEXT top item), the
combat slot indexes, the cell-indexed viewer fan-out, full
verification, handoff.

## G1: full unit battery + clippy + fmt stay green

Runnable.
  CHECK: cd server && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test
  EXPECT: test result: ok (243 tests)

## G2: combat index semantics (shared target winner + stale-row guard)

Runnable.
  CHECK: cd server && cargo test --release combat_indexes && cargo test --release knockout_in_player_phase
  EXPECT: test result: ok (both)

## G3: the release binary boots and the wire client enters the world

Runnable.
  CHECK: cd server && bash scripts/verify_session41.sh boot
  EXPECT: SESSION41 BOOT: OK

## G4: movement + melee wire probes stay green on the new fan-out

Runnable.
  CHECK: cd server && python3 scripts/probe_walk.py && python3 scripts/probe_melee.py
  EXPECT: MOVE PROBE: OK; MELEE WIRE: OK

## G5: the combat sub-attribution numbers are reproducible

Runnable.
  CHECK: cd server && DUR=40 bash scripts/load43.sh
  EXPECT: perf log shows combat_index_us < 30, combat_chase_us <
  combat_players_us, ix_cand_n > 0; numbers land in HANDOFF session 43.

## G6: HANDOFF.md session entry + commits pushed

Manual. Session 43 entry appended with measured numbers; commits
7e42828, 5e833df, 6a06a92 + handoff on origin/master.
