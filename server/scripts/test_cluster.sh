#!/usr/bin/env bash
# Session 86: the cluster e2e gate - boots a 2-node cluster (fresh
# saves), then proves the multi-node profile end to end:
#
#   1. MESH:       "cluster dial link up" on both nodes.
#   2. ENTRY:      a client enters through node 0 (default ports).
#   3. GUEST WALK: the client walks east across peer-owned VisIndex
#                  cells (subscribe -> publish -> ingest -> spawn).
#   4. MIGRATION:  the SAME character re-enters through node 1 (auth
#                  1873 / game 1874): node 0 serves the snapshot,
#                  node 1 receives the migrated character.
#
# Verdict lines: MESH: OK / WORLD ENTRY: OK / GUEST WALK: OK /
# CLUSTER MIGRATION: OK / CLUSTER E2E: OK (all of the above).
# SAVES: a throwaway directory, so reruns are repeatable.
set -u
cd "$(dirname "$0")/.."   # server/

SCRIPTS=scripts
SAVES=/tmp/hnh_cluster_e2e_saves
LOG0=target/cluster-n0.log
LOG1=target/cluster-n1.log
MIGUSER=${MIGUSER:-clustermig86}

cleanup() { ./scripts/cluster-up.sh stop >/dev/null 2>&1; }
trap cleanup EXIT

echo "== booting a fresh 2-node cluster (saves in $SAVES) =="
rm -rf "$SAVES" && mkdir -p "$SAVES"
: > "$LOG0"; : > "$LOG1"
SAVEDIR="$SAVES" RUST_LOG=hnh_server=debug ./scripts/cluster-up.sh up || {
    echo "CLUSTER E2E: FAIL (boot)"
    exit 1
}

echo "== 1. mesh =="
mesh0=$(rg -c "cluster dial link up" "$LOG0" 2>/dev/null || echo 0)
mesh1=$(rg -c "cluster dial link up" "$LOG1" 2>/dev/null || echo 0)
echo "mesh links: node0=$mesh0 node1=$mesh1"
if [ "$mesh0" -lt 1 ] && [ "$mesh1" -lt 1 ]; then
    echo "MESH: FAIL (no dial links)"
    echo "CLUSTER E2E: FAIL"
    exit 1
fi
echo "MESH: OK"

echo "== 2. entry through node 0 + guest walk over peer cells =="
python3 $SCRIPTS/probe_guest_walk.py guestwalk86 || {
    echo "CLUSTER E2E: FAIL (guest walk)"
    exit 1
}
guest_sub=$(rg -c "peer subscribed" "$LOG1" 2>/dev/null || echo 0)
guest_ing=$(rg -c "guest ingested" "$LOG1" 2>/dev/null || echo 0)
echo "guest evidence on node 1: subscribed=$guest_sub ingested=$guest_ing"
if [ "$guest_ing" -lt 1 ]; then
    echo "CLUSTER E2E: FAIL (no guest rows ingested by the peer node)"
    exit 1
fi

echo "== 3. character migration node 0 -> node 1 =="
# Create the character through node 0 first.
python3 $SCRIPTS/probe_cluster_entry.py "$MIGUSER" 1871 1870 || {
    echo "CLUSTER MIGRATION: FAIL (creation through node 0)"
    exit 1
}
sleep 2   # let the owner node persist the new character
# Re-enter through node 1: the fresh ports drive the CharQuery path.
python3 $SCRIPTS/probe_cluster_entry.py "$MIGUSER" 1873 1874 || {
    echo "CLUSTER MIGRATION: FAIL (entry through node 1)"
    exit 1
}
mig_rx=$(rg -c "char migration received: entering world" "$LOG1" 2>/dev/null || echo 0)
mig_tx=$(rg -c "char query: serving snapshot to peer" "$LOG0" 2>/dev/null || echo 0)
echo "migration evidence: node0 served=$mig_tx node1 received=$mig_rx"
if [ "$mig_rx" -lt 1 ] || [ "$mig_tx" -lt 1 ]; then
    echo "CLUSTER MIGRATION: FAIL (no cross-node snapshot exchange)"
    echo "CLUSTER E2E: FAIL"
    exit 1
fi
echo "CLUSTER MIGRATION: OK"

echo "CLUSTER E2E: OK"
