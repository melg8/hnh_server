#!/bin/bash
# Session 30 verification: relay static acts, cursor merge, vis cache,
# and the sharded-save load story (per-node bot cohorts persisting).
#
# Phases:
#   relay-static  - the cross-node pickup/chop/mine contract, proven by
#                   the five relay-static unit tests (mesh roundtrips in
#                   the codec tests + home/authority/ack state machines).
#   cursor-merge  - pickup redirection + stack merging, unit proof.
#   vis-cache     - result-cache patch correctness (unit) + a 25 s
#                   600-bot load window with the <100 ms tick budget.
#   cluster-load  - a 2-node cluster with 60-bot cohorts per node
#                   through the REAL UDP path, perf-checked, then a
#                   graceful SIGTERM flush; both shard files must carry
#                   the bot characters; after a full cluster restart the
#                   bot logins must RESTORE their snapshots (log proof).
# Usage: server/scripts/verify_session30.sh [logtag]
set -u
# Dev-box robustness: cargo installs to ~/.cargo/bin by default, which is
# not on PATH in every non-interactive shell (cron, CI, agent sandboxes).
export PATH="$HOME/.cargo/bin:$PATH"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO"
TAG="${1:-s30}"
BIN=server/target/release/hnh-server
SAVEDIR=$(mktemp -d /tmp/hnh-s30-XXXX)
N0="$SAVEDIR/n0.json"
N1="$SAVEDIR/n1.json"
NODES="127.0.0.1:18890,127.0.0.1:18891"
PIDS=()

fail() { echo "E2E FAIL: $1"; stop_cluster; exit 1; }
trap 'stop_cluster' EXIT

start_cluster() { # $1 = extra args (bots), $2/$3 = log suffixes
  rm -f /tmp/server_${TAG}_n0.log /tmp/server_${TAG}_n1.log
  HNH_SAVE_FILE=$N0 RUST_LOG=${S30_RUST_LOG:-hnh_server=info} \
    $BIN --seed 42 --cluster "$NODES" --node 0 $1 --perf \
    > /tmp/server_${TAG}_n0.log 2>&1 &
  PIDS+=($!)
  HNH_SAVE_FILE=$N1 RUST_LOG=${S30_RUST_LOG:-hnh_server=info} \
    $BIN --seed 42 --cluster "$NODES" --node 1 \
    --game-port 1882 --auth-port 1883 --res-port 1884 $1 --perf \
    > /tmp/server_${TAG}_n1.log 2>&1 &
  PIDS+=($!)
  for _ in $(seq 1 40); do
    if rg -q "cluster node starting" /tmp/server_${TAG}_n0.log 2>/dev/null \
       && rg -q "cluster node starting" /tmp/server_${TAG}_n1.log 2>/dev/null; then
      return 0
    fi
    sleep 0.5
  done
  echo "cluster did not start"; return 1
}

stop_cluster() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  for p in "${PIDS[@]:-}"; do wait "$p" 2>/dev/null || true; done
  PIDS=()
}

echo "== session 30: save dir $SAVEDIR =="

# --- Phase 1: relay static acts (unit proof) -----------------------------
echo "== phase 1: relay static acts (unit tests) =="
RELAY_TESTS=$( (cd server && cargo test --bin hnh-server -- \
  relay_pickup_authority relay_chop_authority static_ack_grants \
  guest_drop_click relay_static_mismatch) 2>&1 \
  | rg -o "([0-9]+) passed" | awk '{s+=$1} END {print s+0}')
echo "relay-static tests passed: ${RELAY_TESTS:-0}"
if [ "${RELAY_TESTS:-0}" -ge 5 ]; then
  echo "RELAY-STATIC VERDICT: OK"
else
  echo "RELAY-STATIC VERDICT: FAIL"
fi

# --- Phase 2: cursor pickup redirection (unit proof) ---------------------
echo "== phase 2: cursor merge (unit tests) =="
MERGE_TESTS=$( (cd server && cargo test --bin hnh-server -- \
  pickup_merges_onto inv_drop_merges_into different_resource_pickup) 2>&1 \
  | rg -o "([0-9]+) passed" | awk '{s+=$1} END {print s+0}')
echo "cursor-merge tests passed: ${MERGE_TESTS:-0}"
if [ "${MERGE_TESTS:-0}" -ge 3 ]; then
  echo "CURSOR-MERGE VERDICT: OK"
else
  echo "CURSOR-MERGE VERDICT: FAIL"
fi

# --- Phase 3: vis cache (unit + single-node load window) -----------------
echo "== phase 3: vis cache (unit + 600-bot load window) =="
(cd server && cargo test --bin hnh-server -- vis_cache_patches) 2>&1 \
  | rg -q "test result: ok" \
  && echo "VIS-CACHE UNIT: OK" || echo "VIS-CACHE UNIT: FAIL"
rm -f /tmp/server_${TAG}_load.log
HNH_SAVE_FILE=$SAVEDIR/load.json RUST_LOG=hnh_server=info \
  $BIN --seed 42 --bots 600 --perf > /tmp/server_${TAG}_load.log 2>&1 &
LOAD_PID=$!
sleep 30
kill -TERM $LOAD_PID 2>/dev/null || true
wait $LOAD_PID 2>/dev/null || true
MAXTICK=$(rg -o "max_tick_us=([0-9]+)" -r '$1' /tmp/server_${TAG}_load.log | sort -n | tail -1)
P50=$(rg -o " tick_us=([0-9]+)" -r '$1' /tmp/server_${TAG}_load.log | sort -n | awk '{a[NR]=$1} END {print a[int(NR/2)]}')
echo "600-bot window: p50=${P50:-?}us max=${MAXTICK:-?}us (budget 100000us)"
if [ "${MAXTICK:-999999}" -lt 100000 ]; then
  echo "VIS-CACHE LOAD VERDICT: OK (tick within budget)"
else
  echo "VIS-CACHE LOAD VERDICT: FAIL (tick overrun)"
fi

# --- Phase 4: cluster load + sharded save story ---------------------------
echo "== phase 4: cluster load + sharded save =="
start_cluster "--bots 60" || fail "cluster boot"
sleep 30
CLUSTER_OK=1
for n in 0 1; do
  MT=$(rg -o "max_tick_us=([0-9]+)" -r '$1' /tmp/server_${TAG}_n$n.log 2>/dev/null | sort -n | tail -1)
  SESS=$(rg -o "sessions=([0-9]+)" -r '$1' /tmp/server_${TAG}_n$n.log 2>/dev/null | tail -1)
  echo "node$n: sessions=${SESS:-0} max_tick_us=${MT:-none}"
  [ "${MT:-999999}" -lt 100000 ] || CLUSTER_OK=0
  [ "${SESS:-0}" -ge 50 ] || CLUSTER_OK=0
done
if [ "$CLUSTER_OK" = 1 ]; then
  echo "CLUSTER LOAD VERDICT: OK (both nodes within budget, cohorts live)"
else
  echo "CLUSTER LOAD VERDICT: FAIL"
fi
stop_cluster
[ -f "$N0" ] || fail "node0 shard file missing"
[ -f "$N1" ] || fail "node1 shard file missing"
BOTS0=$(rg -o '"bot[0-9]{5}' "$N0" 2>/dev/null | sort -u | wc -l)
BOTS1=$(rg -o '"bot[0-9]{5}' "$N1" 2>/dev/null | sort -u | wc -l)
echo "shard bot characters: n0=$BOTS0 n1=$BOTS1"
if [ "$BOTS0" -lt 1 ] || [ "$BOTS1" -lt 1 ]; then
  fail "bot cohorts did not persist into both shards"
fi
echo "SHARD PERSIST VERDICT: OK (cohorts on both nodes)"

# --- Phase 5: full cluster restart; bot logins restore snapshots ----------
echo "== phase 5: restart; bot snapshots restore =="
start_cluster "--bots 60" || fail "cluster restart"
sleep 25
RESTORED0=$(rg -c "restoring persisted character" /tmp/server_${TAG}_n0.log 2>/dev/null || echo 0)
RESTORED1=$(rg -c "restoring persisted character" /tmp/server_${TAG}_n1.log 2>/dev/null || echo 0)
echo "restored bot characters after restart: n0=$RESTORED0 n1=$RESTORED1"
stop_cluster
TOTAL=$((RESTORED0 + RESTORED1))
if [ "$TOTAL" -ge 2 ]; then
  echo "SESSION30 E2E: OK"
else
  echo "SESSION30 E2E: FAIL (no snapshot restore after restart)"
  exit 1
fi
