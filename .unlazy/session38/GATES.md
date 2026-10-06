# Session 38 Gates - player-vs-player archery

OWNS: server/crates/hnh-server/src/**, docs/mechanics/combat/**, scripts/**, HANDOFF.md

Scope: bow carriers can aim at and shoot OTHER PLAYERS (local and cross-node), the victim's home node applies armor/HP/knockout through the existing hurt_player path, and both sides get chat feedback; docs updated; full battery + lint + boot green; commits pushed to master.

- [x] G1: PvP aim opens on player targets (local Kind::Player and guest GuestKind::Player) when a bow is equipped; a self-click never opens the aim; non-bow clicks keep the party-invite menu
  CHECK: cd server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- pvp 2>&1 | tail -3
  EXPECT: test result: ok
  EVIDENCE: automatic-evidence=v1; definition-sha256=017d7a79135ba3c2c3128b8afe2ae8d39116e3e606e17188aab1b24a6b7b5d25; exit=0; EXPECT=matched; output-sha256=28b488edb4a683a798c18c76d23983acb077e8548fbd06056ac87310ce9a81bf; output-bytes=98; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries

- [x] G2: a hit shot applies damage to a local victim through armor (hurt_player), streams the hit FX, re-arms the aim, and a lethal shot knocks the victim out (hp reset + fight teardown)
  CHECK: cd server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- pvp 2>&1 | tail -3
  EXPECT: test result: ok
  EVIDENCE: automatic-evidence=v1; definition-sha256=017d7a79135ba3c2c3128b8afe2ae8d39116e3e606e17188aab1b24a6b7b5d25; exit=0; EXPECT=matched; output-sha256=28b488edb4a683a798c18c76d23983acb077e8548fbd06056ac87310ce9a81bf; output-bytes=98; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries

- [x] G3: a cross-node shot ships PvpArrow to the victim's home node (node_of_gob), the home node applies hurt_player + victim chat + hit FX, and answers PvpArrowResult so the shooter learns the outcome
  CHECK: cd server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- pvp 2>&1 | tail -3
  EXPECT: test result: ok
  EVIDENCE: automatic-evidence=v1; definition-sha256=017d7a79135ba3c2c3128b8afe2ae8d39116e3e606e17188aab1b24a6b7b5d25; exit=0; EXPECT=matched; output-sha256=28b488edb4a683a798c18c76d23983acb077e8548fbd06056ac87310ce9a81bf; output-bytes=98; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries

- [x] G4: full battery + clippy -D warnings + fmt clean (>= 200 tests)
  CHECK: cd server && PATH="$HOME/.cargo/bin:$PATH" cargo test 2>&1 | tail -3 && PATH="$HOME/.cargo/bin:$PATH" cargo clippy --all-targets -- -D warnings 2>&1 | tail -2 && PATH="$HOME/.cargo/bin:$PATH" cargo fmt --all -- --check && echo LINT-OK
  EXPECT: LINT-OK
  EVIDENCE: automatic-evidence=v1; definition-sha256=34de944c25683c7858912a7bd981b65c7defaf5d28ad4e4f363152ae6acac00a; exit=0; EXPECT=matched; output-sha256=c1234d71efba80420fd3e3f1caa92e18843e4c42276f14a23ca390757ff560e3; output-bytes=176; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries

- [x] G5: release binary boots and the full session-38 verification passes (pvp-units + full + lint + boot)
  CHECK: PATH="$HOME/.cargo/bin:$PATH" bash server/scripts/verify_session38.sh 2>&1 | tail -3
  EXPECT: SESSION38 VERIFY: OK
  EVIDENCE: automatic-evidence=v1; definition-sha256=0e7aba74bf35f3e76f85f5c00e365a8d9064c44cd88126822034120128632e07; exit=0; EXPECT=matched; output-sha256=f912ff4b93090e7152aa4dbbb8f18eb87ea8818317c4ef042362463e7934aace; output-bytes=77; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries

- [x] G6: docs/mechanics/combat/combat-system.md documents the implemented PvP archery model and removes the stale "melee relay fight between players exists" claim
  EVIDENCE: grep -c 'melee relay fight between players exists' docs/mechanics/combat/combat-system.md -> 0; grep -c 'Player-versus-player archery (server, session 38)' -> 1; the corrected paragraph lives in the 'Implemented ranged model' section (commit d42eebe).

- [x] G7: HANDOFF.md session-38 entry + worklog record + all commits pushed to master
  EVIDENCE: HANDOFF.md session-38 entry appended (commit da2862a); worklog.md session-38 record appended; git push origin master -> 8339661..da2862a master -> master.
