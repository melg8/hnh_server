# Gates: session 20 - movement fidelity + charlist portrait

OWNS: server/crates/**, scripts/jogl/**, docs/mechanics/**, AGENTS.md, HANDOFF.md

Scope: Fix the four reported defects (empty charlist portrait, teleport on
rapid clicks, no walking animation, too-fast movement) server-side, with
unit gates plus real-client verification through the committed harness.

Root causes identified from client source reading (LinMove.ctick 60ms/step
model, setl monotonic-up, server set_pos-to-destination teleports, c computed
in 100ms ticks, BASE_SPEED=44 subtile/s vs wiki walk 3 tiles/s, static
standing layers only, charlist portrait unverified on real client).

- [x] G1: Server computes client-consistent move timing (c derived from total_ms so the client arrives exactly when the server thinks it does)
  CHECK: bash -c "export PATH=$HOME/.cargo/bin:$PATH; cd /home/z/my-project/hnh_server/server && cargo test -p hnh-server movement_timing 2>&1 | tail -2"
  EXPECT: test result: ok
  EVIDENCE: automatic-evidence=v1; definition-sha256=2e59b6fcaa9d8a188ba397aeafcd70c7fc49a4e2d868712e061a69ca740b2f0e; exit=0; EXPECT=matched; output-sha256=966a7b9b648f4e4fc6c187396bd8e8303f51800856495f5df7b87ee2b376aa4f; output-bytes=96; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session20; path=cc94915413e1/11 entries

- [x] G2: No teleport on rapid re-click (player_walk starts from the interpolated position when already moving)
  CHECK: bash -c "export PATH=$HOME/.cargo/bin:$PATH; cd /home/z/my-project/hnh_server/server && cargo test -p hnh-server movement_reclick 2>&1 | tail -2"
  EXPECT: test result: ok
  EVIDENCE: automatic-evidence=v1; definition-sha256=b6bad53d0c30be7722f51872b70d97466e4b7e4310098830828597d3ce62fa17; exit=0; EXPECT=matched; output-sha256=075b52844e9f1f10782ed1ef8371e08f4b6926246839dbf7d354ab76d9672c40; output-bytes=96; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session20; path=cc94915413e1/11 entries

- [x] G3: Walk speed matches docs (gait system crawl/walk/run/sprint = 16/33/50/66 subtile/s, speedget set selects it)
  CHECK: bash -c "export PATH=$HOME/.cargo/bin:$PATH; cd /home/z/my-project/hnh_server/server && cargo test -p hnh-server gait_speeds 2>&1 | tail -2"
  EXPECT: test result: ok
  EVIDENCE: automatic-evidence=v1; definition-sha256=7818f265117a901bb7183df5ccecd71f44f3a8cdccb01803cb41dd438a7c5c10; exit=0; EXPECT=matched; output-sha256=075b52844e9f1f10782ed1ef8371e08f4b6926246839dbf7d354ab76d9672c40; output-bytes=96; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session20; path=cc94915413e1/11 entries

- [x] G4: Walking-pose layers swap while moving and revert on stop
  CHECK: bash -c "export PATH=$HOME/.cargo/bin:$PATH; cd /home/z/my-project/hnh_server/server && cargo test -p hnh-server walk_layers 2>&1 | tail -2"
  EXPECT: test result: ok
  EVIDENCE: automatic-evidence=v1; definition-sha256=042c0a2603b6e12ed0b30b09878ec7af2e1abb147551224125192dd437f55aea; exit=0; EXPECT=matched; output-sha256=eab172ac7c0298f7270e0799a338c6ee75bcef92ad8f2450e981d927b0e0525e; output-bytes=96; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session20; path=cc94915413e1/11 entries

- [x] G5: Full quality gate: fmt clean, clippy -D warnings clean, all tests green
  CHECK: bash -c "export PATH=$HOME/.cargo/bin:$PATH; cd /home/z/my-project/hnh_server/server && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings >/tmp/clippy_s20.log 2>&1 && cargo test 2>&1 | rg 'test result: ok' | head -1"
  EXPECT: test result: ok
  EVIDENCE: automatic-evidence=v1; definition-sha256=add4a9b1c8acce2364bc0650451b840089d0b4fbb8f8d7ff7914f8d9a7e3ec87; exit=0; EXPECT=matched; output-sha256=2c07f0a50574922424c491ec40d2a364ada2ad650cb922a2036d326c49b26e45; output-bytes=95; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session20; path=cc94915413e1/11 entries

- [x] G6: Real client: portrait visible on the charlist screen (screenshot read by the agent, head+torso+legs composited)
  CHECK: bash /home/z/my-project/hnh_server/scripts/jogl/verify_charlist_portrait.sh s20gate
  EXPECT: PORTRAIT: OK
  EVIDENCE: automatic-evidence=v1; definition-sha256=5e7a7b25761be4f9239aad07217c9f842b11750e74ef5713a72b86e5cbce8441; exit=0; EXPECT=matched; output-sha256=4e120fb99b680060e1a6726d0ef32ad55f7d89fb971e3a179aa58f27330d2e07; output-bytes=47; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session20; path=cc94915413e1/11 entries

- [x] G7: Real client: movement ~3 tiles/s, no teleport on rapid clicks, walking pose mid-walk
  CHECK: bash /home/z/my-project/hnh_server/scripts/jogl/run-real-client-e2e.sh driveuser36 s20gate
  EXPECT: MOVEMENT: MOVED
  EVIDENCE: automatic-evidence=v1; definition-sha256=58017a8baa7d97da74b3120220a21ae9a9a75af590c820c6bad66ef9665bf9b9; exit=0; EXPECT=matched; output-sha256=aeefafcbc7897afdc1aadf90d7e2369b7a329c3de5231c396448a8192e437e75; output-bytes=508; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session20; path=cc94915413e1/11 entries

- [x] G8: Committed and pushed to master
  CHECK: git -C /home/z/my-project/hnh_server log --oneline origin/master -1
  EXPECT: portrait
  EVIDENCE: automatic-evidence=v1; definition-sha256=ec9f9f8aa4c1ab4d3d3230614864d6195bfab314c152eb888b5451c0a323de18; exit=0; EXPECT=matched; output-sha256=49a3867b6d8a253b1b2afaf184d8afecf8f59acbef6582c2118a3cfe7b5df5e8; output-bytes=114; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session20; path=cc94915413e1/11 entries
