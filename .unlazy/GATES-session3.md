# Session 3 Gates — hnh_server

## G1: Crafting mechanics implemented and verified
- Crafting recipes parsed from a data table; Makewindow (make widget)
  flow works end-to-end: paginae craft action -> make list -> make 0/1
  -> ingredients consumed -> result item created in inventory.
- Eating flow: food item -> eat -> FEP applied to attributes per
  docs/mechanics/character/food-and-fep.md (gluttony, eventuality).
- CHECK: cd server && cargo test --release crafting 2>&1 | grep -q "test result: ok"
  EXPECT: crafting unit tests pass (recipe parse + craft flow + fep eat)

## G2: Grid-owner parallel simulation
- The game tick splits gob update work by grid region across worker
  tasks (tokio), SoA columns partitioned; single-owner game task remains
  the sequencer. `--workers N` flag controls worker count.
- Perf: with --bots 1000 --perf, steady-state tick_us mean stays below
  100 ms budget at 1000 bots; scaling with workers >= 1 verified.
- CHECK: cd server && cargo test --release --lib 2>&1 | grep -q "test result: ok"
  EXPECT: all lib tests pass after partitioning refactor

## G3: Windows launch tooling
- Windows one-command start exists and is documented: a .bat/.ps1 pair
  that builds (if needed), generates gameres/, and starts the server;
  plus a client run .bat. No Linux-only assumptions in the launch path.
- CHECK: ls windows/start-server.bat windows/run-client.bat >/dev/null 2>&1 && echo PASS
  EXPECT: PASS

## G4: Full verification suite green, pushed to master
- cargo fmt --check, clippy -D warnings, cargo test all pass.
- Integration: scripts/test_client.py prints WORLD ENTRY: OK against a
  fresh server boot.
- Load: 1000 bot sessions with --perf; tick budget respected.
- All work committed and pushed to origin/master.
- CHECK: cd server && cargo clippy --all-targets -- -D warnings 2>&1 | tail -1 | grep -q "Finished"
  EXPECT: clippy clean
