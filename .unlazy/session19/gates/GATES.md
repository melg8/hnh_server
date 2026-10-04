# GATES — session 19

All gates parent-verified with the evidence listed; run order followed
the PLAN waves (leaf-1 -> leaf-2 -> leaf-3 -> node-1).

### leaf-1:G1 Session-reader hardening (client)
OCache.cres catches sprite-init RuntimeExceptions; the RWorker thread
survives a bad gob resource.
CHECK: rg -n "catch\\(RuntimeException" src/haven/OCache.java
EXPECT: exit 0 (verified in commit dea3f67)

### leaf-1:G2 Ground-drop items carry the neg layer
meat.res and wood.res in res/compiled parse with a 17-byte neg layer
(cc from the embedded PNG IHDR) and load client-side without
"Load error"/"No negative found" (was reproducible before the fix -
LoadException ArrayIndexOutOfBounds for the first bad blob shape).
CHECK: python3 scripts/add_neg_layer.py - gameres/gfx/invobjs/meat.res /dev/null
EXPECT: "already has a neg layer" (verified on the built pack)

### leaf-2:G1 Real-client movement e2e
scripts/jogl/run-real-client-e2e.sh boots server+Xvfb+real GL client,
logs in through the real widget chain, clicks the map with a real Robot
click twice.
CHECK: scripts/jogl/run-real-client-e2e.sh driveuser40 g40
EXPECT: MOVEMENT: MOVED (verified: "MOVEMENT: MOVED from 555,555 to
575,575", run r36/r38/r40; MOVEMENT2: MOVED)

### leaf-2:G2 Wire-level movement probe
CHECK: python3 server/scripts/probe_walk.py <user>
EXPECT: MOVE PROBE: OK (LINBEG + >=1 LINSTEP for the player gob from
mapview args; gob-target click produces no walk LINBEG) - verified.

### leaf-3:G1 Avatar visual evidence
Screenshot of the real client in-world: avatar renders head/torso/legs,
world/minimap/menu/chat render. Evidence: /tmp/client_world_*.png
reviewed in-session (driveuser38 run).

### node-1:G1 Unit + lint
CHECK: cargo fmt --all -- --check && cargo clippy --all-targets -- -D
warnings && cargo test
EXPECT: all green - verified (85 tests: 11+66+8).

### node-1:G2 E2E flows in one generation
WORLD ENTRY + CATTR ORDER (test_client), CRAFT/EAT (test_craft),
FARMING (test_farming), CHAT+PARTY (test_party_chat), E2E EQUIP
(verify_equip.sh e2e) - all PASS against one server binary.

### node-1:G3 Load regression
CHECK: ./target/release/hnh-server --seed 42 --bots 1000 --saturated --perf
EXPECT: 1000/1000 sessions, steady mean_tick_us < 100000 - verified
(~42 ms steady, fights live, RSS ~820 MB on 2 cores/4 GB).

### node-1:G4 Committed and pushed
All session work committed to master (07e9abc..be7d1e7); HANDOFF.md has
the session 19 entry.
