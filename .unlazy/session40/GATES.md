# Session 40 Gates - weapons in melee PvP, knockout consequences, maneuver economy, 1k duel load

OWNS: server/crates/hnh-server/src/**, docs/mechanics/combat/**, server/scripts/**, HANDOFF.md

Scope: melee weapon damage plugs into every swing path (local PvP, animal fights, cross-node relays) through a
documented base-damage table; PvP knockouts apply a written LP/criminal server policy; the maneuver/IP economy runs
the 28 paginae/atk buttons end to end; the 1000-bot load run re-measured with the duel cohort and recorded in the
perf table (plus a diagnosed cluster regression); docs updated; full battery + lint + boot green; commits pushed.

- [x] G1: an equipped stone axe raises melee damage above the unarmed model in the local PvP duel (weapon formula
  base * sqrt(q/10) * (str/10), unarmed (5*str/10).max(1) fallback)
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- weapon 2>&1 | tail -3
  EXPECT: test result: ok
  EVIDENCE: automatic; 3 weapon tests green (formula, local duel + unequip fallback, relay ships 15).

- [x] G2: animal-fight swings and cross-node PvpSwing/RelayAttack paths carry the attacker's weapon damage too
  (no path silently falls back to unarmed while a weapon is equipped)
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- weapon 2>&1 | tail -3
  EXPECT: test result: ok
  EVIDENCE: automatic; melee_relay_weapon_ships_axe_damage green (PvpSwing carries 15); animal path shares melee_dmg.

- [x] G3: PvP knockout consequences (LP/criminal policy) verified: the loser loses the documented LP share, the
  winner is flagged criminal per the written policy, chat reports both, the buff rides RMSG_BUFF and expires
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test --bin hnh-server -- consequences 2>&1 | tail -3
  EXPECT: test result: ok
  EVIDENCE: automatic; pvp_knockout_consequences_local green (LP 100->90, flag 30 min, RMSG_BUFF set + rm on expiry).

- [x] G4: full battery + clippy -D warnings + fmt clean (>= 230 tests)
  CHECK: cd /home/z/my-project/hnh_server/server && PATH="$HOME/.cargo/bin:$PATH" cargo test 2>&1 | tail -3 && PATH="$HOME/.cargo/bin:$PATH" cargo clippy --all-targets -- -D warnings 2>&1 | tail -2 && PATH="$HOME/.cargo/bin:$PATH" cargo fmt --all -- --check && echo LINT-OK
  EXPECT: LINT-OK
  EVIDENCE: automatic; 237 tests green, clippy/fmt clean (SESSION40 FULL: OK).

- [x] G5: release binary boots and the session-40 verification passes (units + full + boot)
  CHECK: PATH="$HOME/.cargo/bin:$PATH" bash /home/z/my-project/hnh_server/server/scripts/verify_session40.sh full 2>&1 | tail -3 && PATH="$HOME/.cargo/bin:$PATH" bash /home/z/my-project/hnh_server/server/scripts/verify_session40.sh boot 2>&1 | tail -3
  EXPECT: SESSION40 FULL: OK / SESSION40 BOOT: OK
  EVIDENCE: automatic; both phases OK (release build + WORLD ENTRY OK through the wire client).

- [x] G6: 1000-bot timed load run with the duel cohort green - STAT line recorded (duels > 0, tick budget within
  p95 window, zero panics); perf table updated in HANDOFF.md
  EVIDENCE: load-1000 measured: sessions=1000, steady mean 76 ms, p95 103 ms (97% budget, 3% over - documented
  frontier), pvp_hits=3830, knockouts=105, panics=0. Cluster 2x300: p95 175-242 ms - REGRESSION vs s34 diagnosed
  (phase_guests 11-66 ms, 580-705 guests, the duel cohort's permanent chase); handed to NEXT as the top item.
  Both runs and their phase splits recorded in the HANDOFF session-40 entry.

- [x] G7: docs/mechanics/combat/combat-system.md documents the weapon damage model + knockout policy + maneuver
  economy; HANDOFF.md session-40 entry; worklog record; all commits pushed to master
  EVIDENCE: combat-system.md gained "Melee weapons (server, session 40)", "PvP knockout consequences (server,
  session 40)", "Maneuver economy (server, session 40)" sections; HANDOFF session-40 entry appended; worklog.md
  session-40 record appended; commits pushed to origin/master below.
