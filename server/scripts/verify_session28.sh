#!/bin/bash
# Session 28 gate probe: cross-node interaction relay.
# Runs the 2-node cluster e2e (real client on node 0, bot cohort on
# node 1) and greps BOTH node logs for the relay fight chain:
#   home node  : "relay fight started"          (interact on a guest)
#   authority  : "relay swing applied"          (RelayAttack landed)
#   home node  : "relay fight bars synced"      (FightBars answer)
#   authority  : "relay bite applied"           (PlayerHurt retaliation)
# The client-side proof is the DriveAgent RELAYFIGHT VERDICT line
# (Fightview widget against a foreign-authority animal, screenshot in
# /tmp/client_relayfight.png). The chase is best-effort: wildlife may
# be off-screen; NO-TARGET is a soft verdict, the WIRE proof lives in
# the node logs (bots fight guests too).
# Usage: server/scripts/verify_session28.sh [logtag]
set -u
TAG="${1:-relay}"
REPO="${REPO:-$(cd "$(dirname "$0")/../.." && pwd)}"

echo "== running cluster e2e (tag $TAG) =="
$REPO/scripts/jogl/run-cluster-e2e.sh relayuser "$TAG" > /tmp/runner_$TAG.log 2>&1

echo "== client verdicts =="
rg "CLUSTER VERDICT|RELAYFIGHT VERDICT|MOVEMENT:|SPEED VERDICT" /tmp/client_$TAG.log || echo "NO CLIENT LOG"

echo "== node0 (home) relay chain =="
RELAY_OPEN=$(rg -c "relay fight started" /tmp/server_${TAG}_n0.log 2>/dev/null || echo 0)
SWINGS=$(rg -c "relay swing applied" /tmp/server_${TAG}_n0.log 2>/dev/null || echo 0)
BARS=$(rg -c "relay fight bars synced" /tmp/server_${TAG}_n0.log 2>/dev/null || echo 0)
BITES=$(rg -c "relay bite applied" /tmp/server_${TAG}_n0.log 2>/dev/null || echo 0)
echo "n0: relay_fights=$RELAY_OPEN swings_authority=$SWINGS fightbars_synced=$BARS bites_home=$BITES"

echo "== node1 (peer) relay chain =="
RELAY_OPEN1=$(rg -c "relay fight started" /tmp/server_${TAG}_n1.log 2>/dev/null || echo 0)
SWINGS1=$(rg -c "relay swing applied" /tmp/server_${TAG}_n1.log 2>/dev/null || echo 0)
BARS1=$(rg -c "relay fight bars synced" /tmp/server_${TAG}_n1.log 2>/dev/null || echo 0)
BITES1=$(rg -c "relay bite applied" /tmp/server_${TAG}_n1.log 2>/dev/null || echo 0)
echo "n1: relay_fights=$RELAY_OPEN1 swings_authority=$SWINGS1 fightbars_synced=$BARS1 bites_home=$BITES1"

TOTAL_RELAY=$((RELAY_OPEN + RELAY_OPEN1))
TOTAL_SWING=$((SWINGS + SWINGS1))
TOTAL_BARS=$((BARS + BARS1))
if [ "$TOTAL_RELAY" -gt 0 ] && [ "$TOTAL_SWING" -gt 0 ] && [ "$TOTAL_BARS" -gt 0 ]; then
  echo "SESSION28 RELAY WIRE VERDICT: OK (fights=$TOTAL_RELAY swings=$TOTAL_SWING bars=$TOTAL_BARS)"
elif [ "$TOTAL_RELAY" -gt 0 ]; then
  echo "SESSION28 RELAY WIRE VERDICT: PARTIAL (fight opened, no swing cycle this run)"
else
  echo "SESSION28 RELAY WIRE VERDICT: NO-CONTACT (wildlife placement gave no guest fight; rerun — placement is random)"
fi
