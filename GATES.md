# Session 42 Gates

Scope: the session-41 PaginaException blocker (legacy pack repair), the
vis spawn-churn debounce, the cross-node maneuver IP relay, full
verification, handoff.

## G1: the legacy pack ships no dropped AButton parent-versions

Runnable (run after any make-gameres regeneration).
  CHECK: python3 server/scripts/fix_gameres_versions.py gameres
  EXPECT: corrupt=0

## G2: the real-client probe passes in all modes

Runnable.
  CHECK: bash server/scripts/verify_ui_probe.sh run
  EXPECT: UI PROBE RUN: OK
  CHECK: bash server/scripts/verify_ui_probe.sh equip
  EXPECT: UI PROBE EQUIP: OK

## G3: full unit battery + clippy + fmt stay green

Runnable.
  CHECK: cd server && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check && cargo test
  EXPECT: test result: ok

## G4: the release binary boots and the wire client enters the world

Runnable.
  CHECK: bash server/scripts/verify_session41.sh boot
  EXPECT: SESSION41 BOOT: OK

## G5: movement wire probe stays green under the debounced retract sweep

Runnable.
  CHECK: bash server/scripts/verify_session41.sh units && python3 server/scripts/probe_walk.py
  EXPECT: MOVE PROBE: OK

## G6: the cross-node maneuver relay round-trips on the wire

Runnable.
  CHECK: cd server && cargo test --release maneuver_delta
  EXPECT: test result: ok

## G7: HANDOFF.md session entry + mechanics docs updated

Manual. Session 42 entry appended with measured numbers; combat-system.md
documents the ManeuverDelta relay; commits pushed to origin/master.
