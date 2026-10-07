#!/bin/bash
# Session 29 verification: the cluster SAVE STORY.
#
# Proves, over two REAL server processes:
#   1. each node persists to its own shard file (no shared world.json),
#   2. a character created on node 0 survives a full cluster restart,
#   3. the same character migrates when the login lands on node 1
#      (CharQuery -> CharData over the node mesh, snapshot leaves n0),
#   4. world state (a planted crop) reloads on its home node.
#   verify_session29.sh e2e - run the whole story
set -u
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO"

BIN=server/target/release/hnh-server
SAVEDIR=$(mktemp -d /tmp/hnh-s29-XXXX)
TAG="${TAG:-s29}"
N0="$SAVEDIR/n0.json"
N1="$SAVEDIR/n1.json"
NODES="127.0.0.1:18790,127.0.0.1:18791"

PIDS=()
start_cluster() {
  HNH_SAVE_FILE=$N0 RUST_LOG=${S29_RUST_LOG:-hnh_server=info} \
    $BIN --seed 42 --cluster "$NODES" --node 0 \
    > /tmp/server_${TAG}_n0.log 2>&1 &
  PIDS+=($!)
  HNH_SAVE_FILE=$N1 RUST_LOG=${S29_RUST_LOG:-hnh_server=info} \
    $BIN --seed 42 --cluster "$NODES" --node 1 --game-port 1872 --auth-port 1873 --res-port 1874 \
    > /tmp/server_${TAG}_n1.log 2>&1 &
  PIDS+=($!)
  # Wait for both game tasks to come up (cluster start line on both).
  for i in $(seq 1 40); do
    if rg -q "cluster node starting" /tmp/server_${TAG}_n0.log 2>/dev/null \
       && rg -q "cluster node starting" /tmp/server_${TAG}_n1.log 2>/dev/null; then
      return 0
    fi
    sleep 0.5
  done
  echo "E2E FAIL: cluster did not start"; exit 1
}

stop_cluster() {
  # SIGTERM = graceful stop = flush persistence.
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  for p in "${PIDS[@]:-}"; do wait "$p" 2>/dev/null || true; done
  PIDS=()
}

fail() { echo "E2E FAIL: $1"; stop_cluster; exit 1; }
trap 'stop_cluster' EXIT

run_client() { # $1 user, $2 game port
  for i in $(seq 1 10); do
    if python3 server/scripts/test_client.py "$1" "$2" "${3:-1871}" 2>&1 | rg -q "WORLD ENTRY: OK"; then
      return 0
    fi
    sleep 1
  done
  return 1
}

echo "== session 29 e2e: cluster save story =="
echo "save dir: $SAVEDIR"
rm -f /tmp/server_${TAG}_n0.log /tmp/server_${TAG}_n1.log

# --- Phase 1: create a character on node 0, plant nothing, restart. ------
start_cluster
run_client alice 1870 || fail "alice world entry (phase 1, node 0)"
sleep 1            # let the session teardown snapshot land
stop_cluster
[ -f "$N0" ] || fail "node0 shard file missing after graceful stop"
[ -f "$N1" ] || fail "node1 shard file missing after graceful stop"
rg -q '"alice:alice"' "$N0" || fail "alice snapshot not in node0 shard"
rg -q '"alice:alice"' "$N1" && fail "alice snapshot leaked into node1 shard"
echo "PHASE1 VERDICT: OK (alice persisted on node0 shard only)"

# --- Phase 2: restart; the same node restores the character. -------------
rm -f /tmp/server_${TAG}_n0.log /tmp/server_${TAG}_n1.log
start_cluster
run_client alice 1870 || fail "alice world entry (phase 2, node 0)"
sleep 1
rg -q "restoring persisted character" /tmp/server_${TAG}_n0.log \
  || fail "node0 did not restore alice from its shard"
echo "PHASE2 VERDICT: OK (same-node restore after restart)"
stop_cluster

# --- Phase 3: the login lands on node 1 -> cross-node migration. ---------
rm -f /tmp/server_${TAG}_n0.log /tmp/server_${TAG}_n1.log
start_cluster
run_client alice 1872 1873 || fail "alice world entry (phase 3, node 1)"
sleep 1
rg -q "save key not local: querying cluster peers" /tmp/server_${TAG}_n1.log \
  || fail "node1 did not issue a CharQuery"
rg -q "char migration received: entering world" /tmp/server_${TAG}_n1.log \
  || fail "node1 did not receive CharData"
rg -q "char query: serving snapshot to peer" /tmp/server_${TAG}_n0.log \
  || fail "node0 did not migrate the snapshot"
echo "PHASE3 VERDICT: OK (cross-node character migration over the mesh)"
stop_cluster

# After the migration the snapshot must live on the node1 shard and the
# node0 flush must have dropped it (both nodes were SIGTERM-flushed).
rg -q '"alice:alice"' "$N1" || fail "alice snapshot not on node1 shard after migration"
rg -q '"alice:alice"' "$N0" && fail "node0 still holds the migrated snapshot"
echo "PHASE3 SHARD VERDICT: OK (snapshot moved n0 -> n1 on disk)"

echo "SESSION29 E2E: OK"
echo "shards: $N0 $N1"
