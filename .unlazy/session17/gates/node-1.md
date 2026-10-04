# Gates: node-1 session 17 integration

OWNS: HANDOFF.md

Scope: everything integrated, verified, recorded, pushed.

- [ ] G1: unit tests green
  CHECK: bash server/scripts/verify_build.sh cargo-test
  CWD: ../../..
  EXPECT: CARGO TEST: ALL PASS
- [ ] G2: fmt + clippy green
  CHECK: bash server/scripts/verify_build.sh fmt-clippy
  CWD: ../../..
  EXPECT: FMT CLIPPY: ALL PASS
- [ ] G3: e2e battery in one server generation
  CHECK: bash server/scripts/verify_build.sh battery
  CWD: ../../..
  EXPECT: BATTERY: ALL PASS
- [ ] G4: handoff invariants hold (no server-written session-end
  blocks, protocol note present, history intact)
  CHECK: bash server/scripts/verify_handoff.sh file
  CWD: ../../..
  EXPECT: HANDOFF FILE: OK
- [ ] G5: committed and pushed: clean tracked tree, HEAD == origin/master
  CHECK: bash server/scripts/verify_session2.sh committed
  CWD: ../../..
  EXPECT: G7: ALL PASS
