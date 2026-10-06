# Session 39 Gates - melee PvP between players

OWNS: server/crates/hnh-server/src/**, docs/mechanics/combat/**, server/scripts/**, HANDOFF.md

Scope: two session players can engage each other in melee (unarmed openings duel) locally and across nodes - the frv openings economy (offence/defence bars, openings, IP) applies to player-vs-player swings, the victim's home node owns HP/knockout through hurt_player, both sides get chat feedback and the frv window relations; docs updated; full battery + lint + boot green; commits pushed to master.

- [x] G1: a melee click on another player (no bow equipped) opens the frv openings duel between the two players instead of the party-invite menu (self-click still ignored)
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- melee_local 2>&1 | tail -3
  EXPECT: test result: ok
  EVIDENCE: automatic-evidence=v1; definition-sha256=67c0b7802b5e2bbe43aaedb081cbad621a18aa1f04dbd1331e3e983229d9396e; exit=0; EXPECT=matched; output-sha256=9cdf6735ae427f5a5be5e1b7934bc0213a227cf8b1e61ea9efa73c5793a1e3cd; output-bytes=98; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session39; path=cc94915413e1/11 entries

- [x] G2: player-vs-player swings run the openings economy - offence spend, defence chip, damage through openings only, IP accrual - and a broken defence lethal hit knocks the victim out with both-side chat
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- melee_local 2>&1 | tail -3
  EXPECT: test result: ok
  EVIDENCE: automatic-evidence=v1; definition-sha256=67c0b7802b5e2bbe43aaedb081cbad621a18aa1f04dbd1331e3e983229d9396e; exit=0; EXPECT=matched; output-sha256=9cdf6735ae427f5a5be5e1b7934bc0213a227cf8b1e61ea9efa73c5793a1e3cd; output-bytes=98; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session39; path=cc94915413e1/11 entries

- [x] G3: a cross-node melee attack ships the existing relay path (PvpArrow-style authority split) to the victim's home node which applies hurt_player and answers the outcome
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- melee_relay 2>&1 | tail -3
  EXPECT: test result: ok
  EVIDENCE: automatic-evidence=v1; definition-sha256=2a91c53b9ce3ba6c8cd26a76c8de4ab43b7f3dc876555bfd2db84210c4032106; exit=0; EXPECT=matched; output-sha256=132435e85aa4c14af68f29cef61b77e8bcb56a1d6c3a1f13096413afb480fa6b; output-bytes=98; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session39; path=cc94915413e1/11 entries

- [x] G4: full battery + clippy -D warnings + fmt clean (>= 200 tests)
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test 2>&1 | tail -3 && PATH="$HOME/.cargo/bin:$PATH" cargo clippy --all-targets -- -D warnings 2>&1 | tail -2 && PATH="$HOME/.cargo/bin:$PATH" cargo fmt --all -- --check && echo LINT-OK
  EXPECT: LINT-OK
  EVIDENCE: automatic-evidence=v1; definition-sha256=cc9ee7235e97f6a9bc480312a5bbb0f8dc3f068076f6c40c6f91991b49231e79; exit=0; EXPECT=matched; output-sha256=c1234d71efba80420fd3e3f1caa92e18843e4c42276f14a23ca390757ff560e3; output-bytes=176; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session39; path=cc94915413e1/11 entries

- [x] G5: release binary boots and the full session-39 verification passes (melee-units + full + lint + boot)
  CHECK: PATH="$HOME/.cargo/bin:$PATH" bash /home/z/my-project/hnh_server/server/scripts/verify_session39.sh 2>&1 | tail -3
  EXPECT: SESSION39 VERIFY: OK
  EVIDENCE: automatic-evidence=v1; definition-sha256=be234be77286e2cc955d21fbbaef66a93f1a6a886bd0167b01db319b8354e30d; exit=0; EXPECT=matched; output-sha256=fb5ae6a997aa948b775656fc9d7da427ac195011f621363d62dcdd52fee7c935; output-bytes=75; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/.unlazy/session39; path=cc94915413e1/11 entries

- [x] G6: docs/mechanics/combat/combat-system.md documents the implemented melee PvP model (engagement, openings, authority split, knockout policy)
  EVIDENCE: combat-system.md 'Implemented melee PvP model (server, session 39)' section (commit 4462688) + the stale 'melee PvP between players is NOT implemented' claim removed; communication.md documents the Fight petal and the guest Fight-only menu.

- [x] G7: HANDOFF.md session-39 entry + worklog record + all commits pushed to master
  EVIDENCE: HANDOFF.md session-39 entry appended (commit b398a81); worklog.md session-39 record appended; git push origin master below.
