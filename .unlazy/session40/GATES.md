# Session 40 Gates - weapons in melee PvP, knockout consequences, 1k duel load run

OWNS: server/crates/hnh-server/src/**, docs/mechanics/combat/**, server/scripts/**, HANDOFF.md

Scope: melee weapon damage plugs into every swing path (local PvP, animal fights, cross-node relays) through a
documented base-damage table; PvP knockouts apply a written LP/criminal server policy; the 1000-bot load run
re-measured with the duel cohort and recorded in the perf table; docs updated; full battery + lint + boot green;
commits pushed to master.

- [ ] G1: an equipped stone axe raises melee damage above the unarmed model in the local PvP duel (weapon formula
  base * sqrt(q/10) * (str/10), unarmed (5*str/10).max(1) fallback)
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- weapon 2>&1 | tail -3
  EXPECT: test result: ok

- [ ] G2: animal-fight swings and cross-node PvpSwing/RelayAttack paths carry the attacker's weapon damage too
  (no path silently falls back to unarmed while a weapon is equipped)
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- weapon 2>&1 | tail -3
  EXPECT: test result: ok

- [ ] G3: PvP knockout consequences (LP/criminal policy) verified: the loser loses the documented LP share, the
  winner is flagged criminal per the written policy, chat reports both
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- consequences 2>&1 | tail -3
  EXPECT: test result: ok

- [ ] G4: full battery + clippy -D warnings + fmt clean (>= 210 tests)
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test 2>&1 | tail -3 && PATH="$HOME/.cargo/bin:$PATH" cargo clippy --all-targets -- -D warnings 2>&1 | tail -2 && PATH="$HOME/.cargo/bin:$PATH" cargo fmt --all -- --check && echo LINT-OK
  EXPECT: LINT-OK

- [ ] G5: release binary boots and the full session-40 verification passes
  CHECK: PATH="$HOME/.cargo/bin:$PATH" bash /home/z/my-project/hnh_server/server/scripts/verify_session40.sh 2>&1 | tail -3
  EXPECT: SESSION40 VERIFY: OK

- [ ] G6: 1000-bot timed load run with the duel cohort green - STAT line recorded (duels > 0, tick_us within
  budget, zero panics); perf table updated in HANDOFF.md
  EVIDENCE: run log line pasted into the gate + HANDOFF perf table entry.

- [ ] G7: docs/mechanics/combat/combat-system.md documents the weapon damage model + knockout policy;
  HANDOFF.md session-40 entry; worklog record; all commits pushed to master
