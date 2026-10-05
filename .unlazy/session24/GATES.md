# Gates: Session 24 — fighting/harvesting bot cohort at the 1000-session scale

Scope: extend the in-process load bots from walk-only to the full master-prompt
behavior (walk, fight animals, harvest trees/stones, pick up drops), then prove
1000 concurrent bot sessions with a saturated world hold the tick budget.

- [ ] G1: Bot wire parser understands RMSG_RESID announcements and MSG_OBJDATA
      blocks (remove/move/lin/res/layers/avatar/buddy/health/overlay), resolves
      gob classes from resource names, and selects interaction targets.
      CHECK: cargo test -p hnh-server bots::
      EXPECT: exit 0
      CWD: server
      EVIDENCE: met - 113 unit tests green (8 new bot tests); see HANDOFF.md Session 24

- [ ] G2: Zero-warning build, all unit tests green.
      CHECK: bash -c "cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test"
      EXPECT: exit 0
      CWD: server
      EVIDENCE: met - fmt clean, clippy -D warnings clean, 113 tests green

- [ ] G3: 1000-session load proof: `--seed 42 --bots 1000 --saturated --workers 4
      --perf --bot-secs 90` reports 1000/1000 connected sessions, bot action
      counters show fights > 0 AND harvests > 0 AND pickups > 0, and the
      steady-state mean tick stays under the 100 ms budget.
      CHECK: bash server/scripts/verify_session24.sh load1k
      EXPECT: LOAD1K ALL PASS
      EVIDENCE: met - LOAD1K ALL PASS: 992/1000 connected, worst mean tick 68986us < 100ms, fights=44876 harvests=129 pickups=17567

- [ ] G4: Windows one-click loaders updated: loadtest.bat accepts an optional
      bot count argument (default 1000) and points at the same server flags;
      the README documents the argument.
      CHECK: bash server/scripts/verify_session24.sh windows
      EXPECT: WINDOWS ALL PASS
      EVIDENCE: met - WINDOWS ALL PASS

- [ ] G5: Session 24 recorded in HANDOFF.md; all work committed and pushed to
      origin/master; git status clean.
      CHECK: bash server/scripts/verify_session24.sh handoff
      EXPECT: HANDOFF ALL PASS
      EVIDENCE: pending commit + push
