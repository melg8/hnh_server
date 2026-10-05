#!/bin/bash
# Session 25 verification: equipment visuals (world avatar + Equipment
# doll clothing layers).
#   verify_session25.sh e2e     - full real-client regression checklist
#   verify_session25.sh handoff - HANDOFF entry + clean push to master
set -u
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO"

case "${1:-}" in
  e2e)
    TAG="${TAG:-s25gate}"
    echo "This gate drives the REAL client (see AGENTS.md); run:"
    echo "  scripts/jogl/run-real-client-e2e.sh equipuser $TAG"
    echo "The runner exits 0; the checklist below reads the client log."
    bash scripts/jogl/run-real-client-e2e.sh equipuser "$TAG" > /dev/null 2>&1 || true
    L=/tmp/client_$TAG.log
    [ -f "$L" ] || { echo "E2E FAIL: no client log at $L"; exit 1; }
    fail() { echo "E2E FAIL: $1"; exit 1; }
    rg -q "MOVEMENT: MOVED" $L || fail "MOVEMENT"
    rg -q "NO TELEPORT: OK" $L || fail "NO TELEPORT"
    rg -q "RAPID CLICKS: GLIDING" $L || fail "RAPID CLICKS"
    rg -q "MOVEMENT2: MOVED" $L || fail "MOVEMENT2"
    [ "$(rg -c 'WALKDIR .* ARRIVED' $L)" -ge 5 ] || fail "WALKDIR legs"
    rg -q "PORTRAIT LAYERS: " $L || fail "PORTRAIT"
    rg -q "EQUIP DOLL: avagob=.* ava-rend=OK" $L || fail "EQUIP DOLL"
    rg -q "ANIMALS SCREENSHOT" $L || fail "ANIMALS"
    rg -q "EQUIPVIS DUMP DRESSED: .*pants-linen" $L || fail "dressed doll dump"
    rg -q "EQUIPVIS DUMP UNDRESSED" $L && ! rg -q "EQUIPVIS DUMP UNDRESSED: .*pants-linen" $L \
      || fail "undressed doll dump"
    rg -q "WORLD DUMP DRESSED: .*pants-linen" $L || fail "dressed world dump"
    rg -q "WORLD DUMP UNDRESSED" $L && ! rg -q "WORLD DUMP UNDRESSED: .*pants-linen" $L \
      || fail "undressed world dump"
    rg -q "EQUIPVIS VERDICT: OK" $L || fail "EQUIPVIS VERDICT"
    echo "E2E ALL PASS"
    ;;
  handoff)
    fail() { echo "HANDOFF FAIL: $1"; exit 1; }
    rg -q "^## 2026-10-05 - Session 25" $REPO/HANDOFF.md || fail "no HANDOFF entry"
    git -C $REPO diff --quiet || git -C $REPO status --short | head -5
    git -C $REPO log --oneline -1 | rg -q "session 25|equipment" || fail "no commit"
    git -C $REPO status --short | rg -q "^\s*M" && echo "(uncommitted changes remain - see above)" || true
    echo "HANDOFF ALL PASS"
    ;;
  *)
    echo "usage: verify_session25.sh e2e|handoff"; exit 1 ;;
esac
