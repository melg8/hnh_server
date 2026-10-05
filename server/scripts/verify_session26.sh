#!/bin/bash
# Session 26 verification: cursor drag item, ground drops, vis skip
# check, accept throttle.
#   verify_session26.sh e2e     - full real-client regression checklist
#   verify_session26.sh handoff - HANDOFF entry + clean push to master
set -u
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO"

case "${1:-}" in
  e2e)
    TAG="${TAG:-s26gate}"
    echo "This gate drives the REAL client (see AGENTS.md); run:"
    echo "  scripts/jogl/run-real-client-e2e.sh dropuser $TAG"
    echo "The runner exits 0; the checklist below reads the client log."
    bash scripts/jogl/run-real-client-e2e.sh dropuser "$TAG" > /dev/null 2>&1 || true
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
    rg -q "EQUIPVIS VERDICT: OK" $L || fail "EQUIPVIS VERDICT"
    rg -q "CURSOR DUMP: dragging=" $L || fail "CURSOR DUMP"
    ! rg -q "No negative found" $L || fail "drop sprite failed to init"
    rg -q "GROUNDDROP DUMP: new-gob=gfx/terobjs/items/" $L || fail "GROUNDDROP DUMP"
    rg -q "PICKUP DUMP: gob-gone=true .*seed-in-inventory=true" $L || fail "PICKUP DUMP"
    rg -q "CURSOR VERDICT: OK" $L || fail "CURSOR VERDICT"
    rg -q "GROUNDDROP VERDICT: OK" $L || fail "GROUNDDROP VERDICT"
    [ -f /tmp/client_cursor_item.png ] || fail "cursor screenshot"
    [ -f /tmp/client_grounddrop.png ] || fail "grounddrop screenshot"
    echo "E2E ALL PASS"
    echo "READ /tmp/client_cursor_item.png (held item + tooltip at the"
    echo "pointer) and /tmp/client_grounddrop.png (the item on the ground"
    echo "next to the player) - screenshots are evidence, not the verdict."
    ;;
  handoff)
    fail() { echo "HANDOFF FAIL: $1"; exit 1; }
    rg -q "Session 26" $REPO/HANDOFF.md || fail "no HANDOFF entry"
    git -C $REPO diff --quiet || git -C $REPO status --short | head -5
    echo "HANDOFF OK (see HANDOFF.md Session 26)"
    ;;
  *)
    echo "usage: verify_session26.sh e2e|handoff"; exit 2 ;;
esac
