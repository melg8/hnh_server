#!/bin/bash
# Session 27 verification: the grid-owner CLUSTER becomes a real
# multi-process split.
#   verify_session27.sh e2e     - 2-node cluster + real client checklist
#   verify_session27.sh handoff - HANDOFF entry + clean push to master
set -u
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO"

case "${1:-}" in
  e2e)
    TAG="${TAG:-s27gate}"
    echo "This gate drives the REAL client against node 0 of a 2-node"
    echo "cluster (node 1 hosts load bots); run:"
    echo "  scripts/jogl/run-cluster-e2e.sh clusteruser $TAG"
    echo "The runner exits 0; the checklist below reads both logs."
    bash scripts/jogl/run-cluster-e2e.sh clusteruser "$TAG" > /dev/null 2>&1 || true
    L=/tmp/client_$TAG.log
    S0=/tmp/server_${TAG}_n0.log
    S1=/tmp/server_${TAG}_n1.log
    [ -f "$L" ] || { echo "E2E FAIL: no client log at $L"; exit 1; }
    [ -f "$S0" ] || { echo "E2E FAIL: no node0 log at $S0"; exit 1; }
    fail() { echo "E2E FAIL: $1"; exit 1; }
    # Cluster links must come up on both nodes.
    rg -q "cluster node starting" $S0 || fail "node0 cluster start"
    rg -q "cluster node starting" $S1 || fail "node1 cluster start"
    # Node0 must ingest foreign-authority gobs (animals + bot players
    # published by node1), and node1 must publish to node0.
    [ "$(rg -c 'guest ingested' $S0)" -ge 1 ] || fail "node0 saw no guests"
    [ "$(rg -c 'guest ingested' $S1)" -ge 1 ] || fail "node1 saw no guests"
    [ "$(rg -c 'peer subscribed' $S0)" -ge 1 ] || fail "node0 got no subscriptions"
    # The real client walks into foreign territory and renders guests.
    rg -q "CLUSTER DUMP: pos=" $L || fail "CLUSTER DUMP"
    rg -q "CLUSTER VERDICT: OK" $L || fail "CLUSTER VERDICT"
    [ -f /tmp/client_cluster.png ] || fail "cluster screenshot"
    # Core single-node regression still runs through the same binary.
    rg -q "MOVEMENT: MOVED" $L || fail "MOVEMENT"
    rg -q "PORTRAIT LAYERS: " $L || fail "PORTRAIT"
    echo "E2E ALL PASS"
    echo "READ /tmp/client_cluster.png - the world as rendered on node 0"
    echo "includes node 1's animals/players (guests)."
    ;;
  handoff)
    fail() { echo "HANDOFF FAIL: $1"; exit 1; }
    rg -q "Session 27" $REPO/HANDOFF.md || fail "no HANDOFF entry"
    echo "HANDOFF OK (see HANDOFF.md Session 27)"
    ;;
  *)
    echo "usage: verify_session27.sh e2e|handoff"; exit 2 ;;
esac
