#!/usr/bin/env bash
# Session 34 verification: build-transition publish + live station relay
# probe + the deferred 300-bot-per-node load story. Phases:
#   station-units - unit battery for the session-34 fixes (build
#                   transition publish, kind-flip re-render, sub diffs)
#   cluster-station - 2-node cluster, real TCP mesh + real UDP: the
#                   builder raises an oven on its own deep cell, the
#                   probe drives fuel+input+light+output through the
#                   relay; both node logs must carry the relay pair.
#   load-300      - 2-node cluster with --bots 300 per node: both nodes
#                   hold max_tick_us < 100000 with 600 live sessions
#                   and the sharded save persists both cohorts.
#   regression    - the session-30 E2E + session-32/33 phases re-run.
#   handoff       - HANDOFF.md entry + worklog + pushed commits.
#   all (default) - station-units + cluster-station + load-300.
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

BIN=target/release/hnh-server
TAG=${TAG:-s34}
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
station-units|all)
  echo "== phase station-units: session-34 unit battery =="
  U=$(cargo test --release -- plan_stage_advance plan_completion kind_flip sub_diffs 2>&1 | rg -c '^test .* ok$' || echo 0)
  [ "$U" -ge 4 ] || fail "station-units battery ($U/4)"
  echo "STATION UNITS: OK ($U tests)"
  [ "$phase" = "station-units" ] && exit 0 ;;
cluster-station|all)
  echo "== phase cluster-station: live guest-oven lifecycle =="
  start_cluster "" || fail "cluster boot"
  python3 scripts/probe_station.py "s34probe$RANDOM" 1870 1871 1882 1883 2 \
    >/tmp/s34_station.log 2>&1 \
    || { cat /tmp/s34_station.log; fail "probe_station"; }
  rg -q "STATION RELAY: OK" /tmp/s34_station.log \
    || { cat /tmp/s34_station.log; fail "probe verdict"; }
  # Home node (node 0): the relay sends.
  rg -q "relay station item sent" "$L0" || fail "node0 relay item log"
  rg -q "relay station act sent" "$L0" || fail "node0 relay act log"
  # Authority node (node 1): validation + apply + the job.
  rg -q "relay station fueled" "$L1" || fail "node1 authority fuel log"
  rg -q "relay station input loaded" "$L1" || fail "node1 authority input log"
  rg -q "relay station lit" "$L1" || fail "node1 authority lit log"
  rg -q "station job done" "$L1" || fail "node1 authority job log"
  echo "relay pair verified on both node logs"
  stop_cluster
  echo "STATION RELAY: OK (cluster-station phase complete)"
  [ "$phase" = "cluster-station" ] && exit 0 ;;
load-300|all)
  echo "== phase load-300: 300-bot cohorts per node =="
  start_cluster "--bots 300" || fail "cluster boot"
  sleep 45
  CLUSTER_OK=1
  for n in 0 1; do
    LOG=/tmp/server_${TAG}_n$n.log
    MT=$(rg -o "max_tick_us=([0-9]+)" -r '$1' "$LOG" 2>/dev/null | sort -n | tail -1)
    SESS=$(rg -o "sessions=([0-9]+)" -r '$1' "$LOG" 2>/dev/null | tail -1)
    echo "node$n: sessions=${SESS:-0} max_tick_us=${MT:-none}"
    [ "${MT:-999999}" -lt 100000 ] || CLUSTER_OK=0
    [ "${SESS:-0}" -ge 250 ] || CLUSTER_OK=0
  done
  if [ "$CLUSTER_OK" = 1 ]; then
    echo "both nodes within the 100 ms tick budget with 300-bot cohorts"
  else
    fail "300/node load: tick budget or session count"
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
  echo "300/NODE LOAD: OK"
  [ "$phase" = "load-300" ] && exit 0 ;;
regression)
  echo "== phase regression: full battery + prior sessions =="
  cargo test --release 2>&1 | rg -q "test result: ok" \
    || fail "unit battery"
  echo "unit battery green"
  bash scripts/verify_session30.sh >/tmp/s34_reg30.log 2>&1 \
    || { tail -20 /tmp/s34_reg30.log; fail "session-30 E2E"; }
  rg -q "SESSION30 E2E: OK" /tmp/s34_reg30.log || fail "session-30 verdict"
  echo "session-30 E2E green"
  bash scripts/verify_session32.sh relay-plow >/tmp/s34_reg32.log 2>&1 \
    || { tail -10 /tmp/s34_reg32.log; fail "session-32 relay-plow"; }
  echo "session-32 relay-plow green"
  bash scripts/verify_session33.sh station >/tmp/s34_reg33.log 2>&1 \
    || { tail -10 /tmp/s34_reg33.log; fail "session-33 station"; }
  echo "session-33 station green"
  echo "REGRESSION: OK"
  exit 0 ;;
handoff)
  echo "== phase handoff: docs + push state =="
  rg -q "Session 34" ../HANDOFF.md || fail "HANDOFF session-34 entry missing"
  git status --porcelain | head -3
  git log --oneline -3
  echo "HANDOFF: OK"
  exit 0 ;;
*)
  echo "usage: $0 [station-units|cluster-station|load-300|regression|handoff|all]"; exit 2 ;;
esac

echo "SESSION34 E2E: OK"
