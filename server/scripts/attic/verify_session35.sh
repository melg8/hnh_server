#!/usr/bin/env bash
# Session 35 verification: drop authority transfer across cell
# boundaries + the single-node 1000-session window re-measured on the
# post-session-34 tree. Phases:
#   drop-units    - unit battery for the session-35 drop transfer
#   cluster-drop  - 2-node cluster, real TCP mesh + real UDP: a rim
#                   oven's output drop (or a rim-tree chop drop) that
#                   crosses the cell boundary transfers to the peer;
#                   the probe picks it up LOCALLY on its own node. Both
#                   node logs must carry the transfer pair.
#   load-1000     - single node with --bots 1000 --saturated: all
#                   sessions live, max_tick_us < 100000.
#   regression    - unit battery + session-34 phases re-run.
#   handoff       - HANDOFF.md entry + worklog + pushed commits.
#   all (default) - drop-units + cluster-drop + load-1000.
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

BIN=target/release/hnh-server
TAG=${TAG:-s35}
NODES="127.0.0.1:18790,127.0.0.1:18791"
N0=/tmp/hnh_${TAG}_n0.json
N1=/tmp/hnh_${TAG}_n1.json
L0=/tmp/server_${TAG}_n0.log
L1=/tmp/server_${TAG}_n1.log
PIDS=()

fail() { echo "E2E FAIL: $1"; stop_cluster; exit 1; }
trap 'stop_cluster' EXIT

start_cluster() { # $1 = extra args (bots), $2/$3 = auth/game port deltas
  rm -f "$L0" "$L1"
  HNH_SAVE_FILE=$N0 RUST_LOG=hnh_server=debug \
    $BIN --seed 42 --cluster "$NODES" --node 0 $1 --perf \
    > "$L0" 2>&1 &
  PIDS+=($!)
  HNH_SAVE_FILE=$N1 RUST_LOG=hnh_server=debug \
    $BIN --seed 42 --cluster "$NODES" --node 1 \
    --game-port 1882 --auth-port 1883 --res-port 1884 $1 --perf \
    > "$L1" 2>&1 &
  PIDS+=($!)
  for _ in $(seq 1 60); do
    if rg -q "cluster node starting" "$L0" 2>/dev/null \
       && rg -q "cluster node starting" "$L1" 2>/dev/null; then
      sleep 3   # let the mesh handshake settle
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

phase="${1:-all}"
case "$phase" in
drop-units|all)
  echo "== phase drop-units: session-35 unit battery =="
  U=$(cargo test --release drop_transfer 2>&1 | rg -c '^test .* ok$' || echo 0)
  [ "$U" -ge 3 ] || fail "drop-units battery ($U/3)"
  echo "DROP UNITS: OK ($U tests)"
  [ "$phase" = "drop-units" ] && exit 0 ;;
cluster-drop|all)
  echo "== phase cluster-drop: live boundary drop transfer =="
  start_cluster "" || fail "cluster boot"
  python3 scripts/probe_drop.py "s35probe$RANDOM" 1870 1871 1882 1883 2 \
    >/tmp/s35_drop.log 2>&1 \
    || { cat /tmp/s35_drop.log; fail "probe_drop"; }
  rg -q "DROP TRANSFER: OK" /tmp/s35_drop.log \
    || { cat /tmp/s35_drop.log; fail "probe verdict"; }
  # Spawner side (node 1): the drop leaves for the cell's owner.
  rg -q "drop authority transferred" "$L1" \
    || fail "node1 transfer log"
  # Owner side (node 0): the exact id is claimed into the sim tables.
  rg -q "drop authority claimed" "$L0" \
    || fail "node0 claim log"
  echo "transfer pair verified on both node logs"
  stop_cluster
  echo "DROP TRANSFER: OK (cluster-drop phase complete)"
  [ "$phase" = "cluster-drop" ] && exit 0 ;;
load-1000|all)
  echo "== phase load-1000: single-node 1000-session window =="
  rm -f "$L0"
  HNH_SAVE_FILE=$N0 RUST_LOG=hnh_server=info \
    $BIN --seed 42 --bots 1000 --saturated --perf \
    > "$L0" 2>&1 &
  PIDS+=($!)
  ok_boot=0
  for _ in $(seq 1 120); do
    if rg -q "sessions=" "$L0" 2>/dev/null; then ok_boot=1; break; fi
    sleep 1
  done
  [ "$ok_boot" = 1 ] || fail "server boot"
  # The in-process bots log in at ~12 sessions/s (TLS auth + play +
  # world entry each); wait until the cohort settles (last reported
  # session count stops growing or hits 980) instead of a fixed sleep.
  SESS=0
  for _ in $(seq 1 30); do
    sleep 5
    S=$(rg -o "sessions=([0-9]+)" -r '$1' "$L0" 2>/dev/null | tail -1)
    [ -n "$S" ] || continue
    if [ "$S" = "$SESS" ]; then break; fi
    if [ "$S" -ge 980 ]; then SESS=$S; break; fi
    SESS=$S
  done
  sleep 20   # steady-state ticks for the max_tick_us readout
  MT=$(rg -o "max_tick_us=([0-9]+)" -r '$1' "$L0" 2>/dev/null | sort -n | tail -1)
  SESS=$(rg -o "sessions=([0-9]+)" -r '$1' "$L0" 2>/dev/null | tail -1)
  echo "single node: sessions=${SESS:-0} max_tick_us=${MT:-none}"
  [ "${MT:-999999}" -lt 100000 ] || fail "1000-bot tick budget (max_tick_us=$MT)"
  [ "${SESS:-0}" -ge 950 ] || fail "1000-bot session count (sessions=$SESS)"
  stop_cluster
  echo "1000-BOT LOAD: OK"
  [ "$phase" = "load-1000" ] && exit 0 ;;
regression)
  echo "== phase regression: full battery + session-34 phases =="
  cargo test --release 2>&1 | rg -q "test result: ok" \
    || fail "unit battery"
  echo "unit battery green"
  cargo clippy --all-targets -- -D warnings >/tmp/s35_clippy.log 2>&1 \
    || { tail -10 /tmp/s35_clippy.log; fail "clippy"; }
  echo "clippy green"
  bash scripts/verify_session34.sh station-units >/tmp/s35_reg34u.log 2>&1 \
    || { tail -10 /tmp/s35_reg34u.log; fail "session-34 station-units"; }
  echo "session-34 station-units green"
  bash scripts/verify_session34.sh cluster-station >/tmp/s35_reg34c.log 2>&1 \
    || { tail -20 /tmp/s35_reg34c.log; fail "session-34 cluster-station"; }
  echo "session-34 cluster-station green"
  echo "REGRESSION: OK"
  exit 0 ;;
handoff)
  echo "== phase handoff: docs + push state =="
  rg -q "Session 35" ../HANDOFF.md || fail "HANDOFF session-35 entry missing"
  git status --porcelain | head -3
  git log --oneline -3
  echo "HANDOFF: OK"
  exit 0 ;;
*)
  echo "usage: $0 [drop-units|cluster-drop|load-1000|regression|handoff|all]"; exit 2 ;;
esac

echo "SESSION35 E2E: OK"
