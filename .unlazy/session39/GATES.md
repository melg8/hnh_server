# Session 39 Gates - melee PvP between players

OWNS: server/crates/hnh-server/src/**, docs/mechanics/combat/**, server/scripts/**, HANDOFF.md

Scope: two session players can engage each other in melee (unarmed openings duel) locally and across nodes - the frv openings economy (offence/defence bars, openings, IP) applies to player-vs-player swings, the victim's home node owns HP/knockout through hurt_player, both sides get chat feedback and the frv window relations; docs updated; full battery + lint + boot green; commits pushed to master.

- [ ] G1: a melee click on another player (no bow equipped) opens the frv openings duel between the two players instead of the party-invite menu (self-click still ignored)
  CHECK: cd server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- melee_local 2>&1 | tail -3
  EXPECT: test result: ok

- [ ] G2: player-vs-player swings run the openings economy - offence spend, defence chip, damage through openings only, IP accrual - and a broken defence lethal hit knocks the victim out with both-side chat
  CHECK: cd server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- melee_local 2>&1 | tail -3
  EXPECT: test result: ok

- [ ] G3: a cross-node melee attack ships the existing relay path (PvpArrow-style authority split) to the victim's home node which applies hurt_player and answers the outcome
  CHECK: cd server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- melee_relay 2>&1 | tail -3
  EXPECT: test result: ok

- [ ] G4: full battery + clippy -D warnings + fmt clean (>= 200 tests)
  CHECK: cd server && PATH="$HOME/.cargo/bin:$PATH" cargo test 2>&1 | tail -3 && PATH="$HOME/.cargo/bin:$PATH" cargo clippy --all-targets -- -D warnings 2>&1 | tail -2 && PATH="$HOME/.cargo/bin:$PATH" cargo fmt --all -- --check && echo LINT-OK
  EXPECT: LINT-OK

- [ ] G5: release binary boots and the full session-39 verification passes (melee-units + full + lint + boot)
  CHECK: PATH="$HOME/.cargo/bin:$PATH" bash server/scripts/verify_session39.sh 2>&1 | tail -3
  EXPECT: SESSION39 VERIFY: OK

- [ ] G6: docs/mechanics/combat/combat-system.md documents the implemented melee PvP model (engagement, openings, authority split, knockout policy)

- [ ] G7: HANDOFF.md session-39 entry + worklog record + all commits pushed to master
