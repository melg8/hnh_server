# GATES — session 30: relay static acts, cursor merge, vis cache, sharded load

## Gates

### G1: relay pickup/build against guest static gobs
Clicking a foreign-authority drop/structure (pickup, build-adjacent checks)
routes through the mesh: the authority node applies the state change, both
nodes' views converge, and a loot item can never be duplicated across nodes.
CHECK: bash server/scripts/verify_session30.sh relay-static
EXPECT: ALL PASS

### G2: cursor pickup redirection (merge stacks)
Picking up a ground drop while the cursor holds a same-type stack merges
into the cursor stack; same-type inventory<->cursor transfers merge; no
silent item loss or duplication (counts conserved).
CHECK: bash server/scripts/verify_session30.sh cursor-merge
EXPECT: ALL PASS

### G3: vis-scan result caching
Unchanged visibility queries skip the full rescan; behavior unchanged
(all existing tests green) and tick time at 1000 bots improves or holds
against a recorded pre-change baseline.
CHECK: bash server/scripts/verify_session30.sh vis-cache
EXPECT: ALL PASS

### G4: sharded-save load-test story (2-node cluster, restart under load)
A 2-node cluster runs bot cohorts on both nodes through the real UDP path;
nodes are restarted; the bot characters' snapshots persisted (positions and
inventories restored on the correct node).
CHECK: bash server/scripts/verify_session30.sh cluster-load
EXPECT: SESSION30 E2E: OK

### G5: unit battery green (regression)
All existing unit tests plus new session-30 tests pass.
CHECK: cargo test --all --quiet
EXPECT: test result: ok (exit 0)

### G6: zero-warning build
CHECK: cargo clippy --all-targets -- -D warnings
EXPECT: exit 0

### G7: fmt clean
CHECK: cargo fmt --all -- --check
EXPECT: exit 0

### G8: wire-level e2e regression (world entry + craft/eat)
CHECK: python3 server/scripts/test_client.py s30user | grep -q "WORLD ENTRY: OK" && python3 server/scripts/test_craft.py s30craft | grep -q "CRAFT/EAT: OK"
EXPECT: exit 0

### G9 (manual): real-client e2e after sandbox re-provision
The JOGL/Xvfb environment was wiped with the sandbox; if
deploy-agent-env.sh finishes in time, run the real-client e2e and record the
verdicts (MOVEMENT: MOVED etc.) in HANDOFF. Manual: honest report either way.

### G10 (stretch): craft pagina wiring for remaining recipes
Verified (wiki-cited, doc-updated) recipes beyond axe/hcloak/roast resolve
through act("craft", id) and craft end to end. Skipped honestly if the
2-hour budget is consumed by leaf-1..4 + verification.
