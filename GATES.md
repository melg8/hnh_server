# Session 45 Gates

Scope: close the session-44 NEXT trust gap (the real GL client e2e
against the post-wire-fix batched datagram format), the crafting
gear-chain gap, the carried Windows smoke, and the post-fix load
re-measurement.

## G1: full unit battery + clippy + fmt stay green

Runnable.
  CHECK: cd server && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test --release
  EXPECT: test result: ok (247 tests)

## G2: the REAL GL client passes the full e2e loop on the session-44 wire format

Runnable.
  CHECK: cd /home/z/my-project/hnh_server && bash scripts/jogl/run-real-client-e2e.sh driveuser s45g
  EXPECT: client log has MOVEMENT: MOVED, WALKDIR legs ARRIVED,
  EQUIPVIS VERDICT: OK, GROUNDDROP DUMP, and zero
  Exception|PaginaException lines; screenshots saved under /tmp.

## G3: wire probes on the final release binary

Runnable.
  CHECK: cd server && python3 scripts/test_client.py g3 && python3 scripts/probe_walk.py && python3 scripts/probe_melee.py
  EXPECT: WORLD ENTRY: OK; MOVE PROBE: OK; MELEE WIRE: OK

## G4: the gear-chain recipes are wired and pack-verified

Runnable.
  CHECK: cd server && cargo test --release gear_chain
  EXPECT: gear_chain_recipes_are_wired ... ok

## G5: Windows gameres smoke under real PowerShell Core

Runnable.
  CHECK: cd /home/z/my-project/hnh_server && bash server/scripts/verify_windows_gameres.sh
  EXPECT: WIN GAMERES SMOKE: OK (pwsh 7.4.6; pack-repair step ran,
  corrupt=0)

## G6: cluster load re-measurement inside the tick budget

Runnable.
  CHECK: cd server && bash scripts/verify_session40.sh load-cluster
  EXPECT: both nodes report steady_p95 below 100000us and panics=0

## G7: HANDOFF.md session entry + commits pushed

Manual. Session 45 entry appended with measured numbers; commits on
origin/master.
