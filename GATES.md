# Session 41 Gates

Scope: guest-phase optimization (cluster duel regression), vis spawn
churn, cross-node maneuver IP relay, full verification battery, handoff.

## G1: guest-phase optimization lands with measured improvement

Runnable.
  CHECK: bash server/scripts/verify_session41.sh guest-opt
  EXPECT: GUESTOPT: OK

## G2: full unit battery + clippy + fmt stay green after changes

Runnable.
  CHECK: bash server/scripts/verify_session41.sh full
  EXPECT: SESSION41 FULL: OK

## G3: release binary boots and the wire client enters the world

Runnable.
  CHECK: bash server/scripts/verify_session41.sh boot
  EXPECT: SESSION41 BOOT: OK

## G4: load windows re-measured after optimization (single node + cluster)

Runnable.
  CHECK: bash server/scripts/verify_session41.sh load
  EXPECT: SESSION41 LOAD: OK

## G5: Java client jar builds and connects to the local server

Runnable.
  CHECK: bash server/scripts/verify_session41.sh java
  EXPECT: SESSION41 JAVA: OK

## G6: HANDOFF.md session entry + mechanics docs updated

Manual. Session entry appended with measured numbers, next items set;
docs/mechanics updated if implementation revealed a nuance.

## G7: commits pushed to origin/master

Manual. Small focused commits in English imperative mood; no secrets;
git log shows the session-41 chain on master.
