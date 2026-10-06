# Session 44 Gates

Scope: the session-43 NEXT top item - batched move starts (the
session-41 packed-batch pattern applied to LINBEG starts and FX
overlays: encode once, one datagram per session per tick) plus the
hit-tail trim (per-hit info log rate-limited into an aggregate).

## G1: full unit battery + clippy + fmt stay green

Runnable.
  CHECK: cd server && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test --release
  EXPECT: test result: ok

## G2: batch fan-out semantics (encode-once + per-session wire patch + unacked recording)

Runnable.
  CHECK: cd server && cargo test --release move_batch && cargo test --release batch_fx
  EXPECT: test result: ok (both)

## G3: wire behavior unchanged on the release binary

Runnable.
  CHECK: cd server && bash scripts/verify_session41.sh boot && python3 scripts/probe_walk.py && python3 scripts/probe_melee.py
  EXPECT: SESSION41 BOOT: OK; MOVE PROBE: OK; MELEE WIRE: OK

## G4: the chase/hit tail measurably shrinks under the 1000-bot cohort

Runnable.
  CHECK: cd server && DUR=40 bash scripts/load43.sh
  EXPECT: perf log shows mv_viewers_us and mv_pose_us per start at or
  below session-43 levels, combat chase share of the tick reduced;
  numbers land in HANDOFF session 44.

## G5: HANDOFF.md session entry + commits pushed

Manual. Session 44 entry appended with measured numbers; commits on
origin/master.
