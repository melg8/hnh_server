# Gates: session 20 - movement fidelity + charlist portrait

OWNS: server/crates/**, scripts/jogl/**, docs/mechanics/**, AGENTS.md, HANDOFF.md

Scope: Fix the four reported defects (empty charlist portrait, teleport on
rapid clicks, no walking animation, too-fast movement) server-side, with
unit gates plus real-client verification through the committed harness.

Root causes identified from client source reading (LinMove.ctick 60ms/step
model, setl monotonic-up, server set_pos-to-destination teleports, c computed
in 100ms ticks, BASE_SPEED=44 subtile/s vs wiki walk 3 tiles/s, static
standing layers only, charlist portrait unverified on real client).

- [ ] G1: Server computes client-consistent move timing (c derived from total_ms so the client arrives exactly when the server thinks it does)
  CHECK: cargo test -p hnh-server movement_timing
  EXPECT: test result: ok
  EVIDENCE: pending

- [ ] G2: No teleport on rapid re-click (player_walk starts from the interpolated position when already moving)
  CHECK: cargo test -p hnh-server movement_reclick
  EXPECT: test result: ok
  EVIDENCE: pending

- [ ] G3: Walk speed matches docs (gait system crawl/walk/run/sprint = 16/33/50/66 subtile/s, speedget set selects it)
  CHECK: cargo test -p hnh-server gait_speeds
  EXPECT: test result: ok
  EVIDENCE: pending

- [ ] G4: Walking-pose layers swap while moving and revert on stop
  CHECK: cargo test -p hnh-server walk_layers
  EXPECT: test result: ok
  EVIDENCE: pending

- [ ] G5: Full quality gate: fmt clean, clippy -D warnings clean, all tests green
  CHECK: bash -c "cd server && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings >/tmp/clippy_s20.log 2>&1 && cargo test 2>&1 | tail -5"
  EXPECT: test result: ok
  EVIDENCE: pending

- [ ] G6: Real client: portrait visible on the charlist screen (screenshot read by the agent, head+torso+legs composited)
  CHECK: bash scripts/jogl/verify_charlist_portrait.sh
  EXPECT: PORTRAIT: OK
  EVIDENCE: pending

- [ ] G7: Real client: movement ~3 tiles/s, no teleport on rapid clicks, walking pose mid-walk
  CHECK: bash scripts/jogl/run-real-client-e2e.sh driveuser36 s20walk
  EXPECT: MOVEMENT: MOVED
  EVIDENCE: pending

- [ ] G8: Committed and pushed to master
  CHECK: git -C /home/z/my-project/hnh_server log --oneline origin/master -1
  EXPECT: s20
  EVIDENCE: pending
