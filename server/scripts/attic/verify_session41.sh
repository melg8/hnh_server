#!/usr/bin/env bash
# Session 41 verification: packed cell-indexed movement fan-out + batched
# guest pose streaming + LINSTEP cadence. Phases:
#   units     - move_batch unit battery
#   full      - complete test battery + clippy -D warnings + fmt
#   boot      - release binary boots and the wire client enters the world
#   guest-opt - the session-41 unit battery + boot proof for the gates
#   load      - load windows: cluster 2x300 (the regression scenario) and
#               single-node 1000 (the frontier), measured numbers printed
#   all (default) - units + full + boot
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

BIN=target/release/hnh-server
TAG=${TAG:-s41}
SAVE=/tmp/hnh_${TAG}.json
LOG=/tmp/server_${TAG}.log
PID=""

fail() { echo "E2E FAIL: $1"; stop; exit 1; }
stop() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; [ -n "$PID" ] && wait "$PID" 2>/dev/null; PID=""; }
trap 'stop' EXIT

phase="${1:-all}"
run_units() {
  echo "== phase units: move_batch battery =="
  M=$(cargo test --bin hnh-server move_batch 2>&1 | rg -c '^test .* ok$' || echo 0)
  [ "$M" -ge 3 ] || fail "move_batch battery ($M/3)"
  echo "SESSION41 UNITS: OK (move_batch=$M)"
}
run_full() {
  echo "== phase full: complete battery + lint =="
  cargo test 2>&1 | rg -q "0 failed" || fail "full battery"
  N=$(cargo test 2>&1 | rg -o '^test result: ok\. ([0-9]+) passed' -r '$1' | awk '{s+=$1} END {print s+0}')
  [ "$N" -ge 240 ] || fail "test count ($N < 240)"
  cargo clippy --all-targets -- -D warnings >/tmp/${TAG}_clippy.log 2>&1 \
    || { tail -10 /tmp/${TAG}_clippy.log; fail "clippy"; }
  cargo fmt --all -- --check || fail "fmt"
  echo "SESSION41 FULL: OK ($N tests, clippy, fmt)"
}
run_boot() {
  echo "== phase boot: release binary + wire client =="
  cargo build --release || fail "release build"
  rm -f "$SAVE" "$LOG"
  HNH_SAVE_FILE=$SAVE RUST_LOG=hnh_server=info \
    $BIN --seed 42 --perf > "$LOG" 2>&1 &
  PID=$!
  ok=0
  for _ in $(seq 1 60); do
    if rg -q "listening" "$LOG" 2>/dev/null; then ok=1; break; fi
    sleep 1
  done
  [ "$ok" = 1 ] || { tail -5 "$LOG"; fail "server boot"; }
  python3 scripts/test_client.py "s41boot$$" >/tmp/${TAG}_client.log 2>&1 \
    || { tail -10 /tmp/${TAG}_client.log; fail "test_client"; }
  rg -q "WORLD ENTRY: OK" /tmp/${TAG}_client.log || fail "world entry verdict"
  python3 scripts/probe_walk.py "s41walk$$" >/tmp/${TAG}_walk.log 2>&1 \
    || { tail -10 /tmp/${TAG}_walk.log; fail "walk probe"; }
  rg -q "MOVE PROBE: OK" /tmp/${TAG}_walk.log || fail "move verdict"
  echo "SESSION41 BOOT: OK (world entry + movement under the 5 Hz LINSTEP cadence)"
  stop
}
case "$phase" in
units)
  run_units ;;
guest-opt)
  run_units
  run_boot ;;
full)
  run_full ;;
boot)
  run_boot ;;
all)
  run_units
  run_full
  run_boot ;;
load)
  echo "== phase load: cluster 2x300 + single-node 1000 windows =="
  bash scripts/verify_session40.sh load-cluster 2>&1 | tail -3
  bash scripts/verify_session40.sh load-1000 2>&1 | tail -3
  echo "SESSION41 LOAD: MEASURED (numbers above go into HANDOFF.md)"
  exit 0 ;;
*)
  fail "unknown phase: $phase" ;;
esac
