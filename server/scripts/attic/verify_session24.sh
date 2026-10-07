#!/usr/bin/env bash
# Session 24 gate verifier: fighting/harvesting bot cohort at the 1000-session scale.
# Usage: bash server/scripts/verify_session24.sh load1k|windows|handoff
set -u
export PATH="$HOME/.cargo/bin:$PATH"
cd "$(dirname "$0")/../.." || exit 1
ROOT=$(pwd)
case "${1:-}" in
  load1k)
    LOG=/tmp/s24_gate.log
    echo "[load1k] building release..."
    (cd server && cargo build --release >/dev/null 2>&1) || { echo "BUILD FAIL"; exit 1; }
    echo "[load1k] running 1000 bots for 90 s..."
    timeout 420 ./server/target/release/hnh-server \
      --seed 42 --bots 1000 --bot-secs 90 --saturated --workers 4 --perf \
      > "$LOG" 2>&1
    RC=$?
    if [ $RC -ne 0 ] && [ $RC -ne 124 ]; then echo "SERVER EXIT $RC"; exit 1; fi
    LINE=$(grep "bot cohort finished" "$LOG" | tail -1)
    echo "$LINE"
    CONNECTED=$(echo "$LINE" | grep -o "connected=[0-9]*" | cut -d= -f2)
    FIGHTS=$(echo "$LINE" | grep -o "fights=[0-9]*" | cut -d= -f2)
    HARVESTS=$(echo "$LINE" | grep -o "harvests=[0-9]*" | cut -d= -f2)
    PICKUPS=$(echo "$LINE" | grep -o "pickups=[0-9]*" | cut -d= -f2)
    # Steady-state mean tick: worst 5 s report window of the run.
    WORST=$(grep -o "mean_tick_us=[0-9]*" "$LOG" | cut -d= -f2 | sort -rn | head -1)
    PEAK_PLAYERS=$(grep -o "players=[0-9]*" "$LOG" | cut -d= -f2 | sort -rn | head -1)
    echo "stats: connected=$CONNECTED/1000 fights=$FIGHTS harvests=$HARVESTS pickups=$PICKUPS worst_mean_tick_us=$WORST peak_players=$PEAK_PLAYERS"
    FAIL=0
    [ "${CONNECTED:-0}" -lt 950 ] && { echo "FAIL: connected < 950"; FAIL=1; }
    [ "${FIGHTS:-0}" -lt 100 ] && { echo "FAIL: fights < 100"; FAIL=1; }
    [ "${HARVESTS:-0}" -lt 20 ] && { echo "FAIL: harvests < 20"; FAIL=1; }
    [ "${PICKUPS:-0}" -lt 100 ] && { echo "FAIL: pickups < 100"; FAIL=1; }
    [ "${WORST:-999999}" -ge 100000 ] && { echo "FAIL: mean tick >= 100 ms"; FAIL=1; }
    [ $FAIL -eq 0 ] && echo "LOAD1K ALL PASS"
    exit $FAIL
    ;;
  windows)
    FAIL=0
    W=$ROOT/windows
    grep -q 'set BOT_COUNT=%1' "$W/loadtest.bat" 2>/dev/null || { echo "FAIL: loadtest.bat lacks BOT_COUNT"; FAIL=1; }
    grep -q "bot-secs" "$W/loadtest.bat" 2>/dev/null || { echo "FAIL: loadtest.bat lacks bot-secs"; FAIL=1; }
    grep -q "loadtest.bat \[bots\]" "$W/README.md" 2>/dev/null || { echo "FAIL: README lacks loadtest doc"; FAIL=1; }
    [ $FAIL -eq 0 ] && echo "WINDOWS ALL PASS"
    exit $FAIL
    ;;
  handoff)
    FAIL=0
    grep -q "Session 24" "$ROOT/HANDOFF.md" || { echo "FAIL: HANDOFF lacks Session 24"; FAIL=1; }
    git -C "$ROOT" diff --quiet || { echo "FAIL: uncommitted changes"; FAIL=1; }
    [ $FAIL -eq 0 ] && echo "HANDOFF ALL PASS"
    exit $FAIL
    ;;
  *)
    echo "usage: $0 load1k|windows|handoff"; exit 2 ;;
esac
