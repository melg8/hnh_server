#!/usr/bin/env bash
# Session 59 multi-node scaling profile.
#
# Measures the per-node tick cost of the grid-owner cluster against the
# single-node baseline at the same TOTAL bot population, answering the
# two carried profiling questions from HANDOFF gaps #2 and #7:
#   - is the movement fan-out actually pair-capped per node (sessions x
#     visible movers) once grids are split across nodes;
#   - what the cluster maintenance + guest GC walk costs at load.
#
# Usage:
#   MODE=single BOTS=600 ./profile_multinode.sh
#   MODE=cluster BOTS=300 ./profile_multinode.sh   # 2 nodes x 300 bots
#
# Both modes use --saturated --perf and a 60 s measurement window after
# a settle period, so numbers are directly comparable.
set -u
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
BIN=target/release/hnh-server
MODE=${MODE:-single}
BOTS=${BOTS:-300}
WINDOW=${WINDOW:-60}
SETTLE=${SETTLE:-45}
TAG=${TAG:-s59}
MESH0=${MESH0:-18790}
MESH1=${MESH1:-18791}
G1=${G1:-1882}   # node 1 game port
A1=${A1:-1883}   # node 1 auth port
R1=${R1:-1884}   # node 1 res port

LOGS=()
PIDS=()
stop_all() { for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
            for p in "${PIDS[@]:-}"; do wait "$p" 2>/dev/null || true; done; PIDS=(); }
# TERM/INT must also tear the nodes down: a bare EXIT trap does not run
# when bash dies from a signal (orphaned nodes poison the next run's
# ports - seen in session 59).
trap 'stop_all' EXIT
trap 'stop_all; exit 143' TERM
trap 'stop_all; exit 130' INT

if [ "$MODE" = "cluster" ]; then
  L0=/tmp/server_${TAG}_n0.log
  L1=/tmp/server_${TAG}_n1.log
  LOGS=("$L0" "$L1")
  rm -f "$L0" "$L1" /tmp/hnh_${TAG}_n0.json /tmp/hnh_${TAG}_n1.json
  HNH_SAVE_FILE=/tmp/hnh_${TAG}_n0.json RUST_LOG=hnh_server=info \
    $BIN --seed 42 --cluster "127.0.0.1:$MESH0,127.0.0.1:$MESH1" --node 0 \
    --bots "$BOTS" --saturated --perf > "$L0" 2>&1 &
  PIDS+=($!)
  HNH_SAVE_FILE=/tmp/hnh_${TAG}_n1.json RUST_LOG=hnh_server=info \
    $BIN --seed 42 --cluster "127.0.0.1:$MESH0,127.0.0.1:$MESH1" --node 1 \
    --bots "$BOTS" --saturated --perf \
    --game-port "$G1" --auth-port "$A1" --res-port "$R1" > "$L1" 2>&1 &
  PIDS+=($!)
else
  L0=/tmp/server_${TAG}_single.log
  LOGS=("$L0")
  rm -f "$L0" /tmp/hnh_${TAG}_single.json
  HNH_SAVE_FILE=/tmp/hnh_${TAG}_single.json RUST_LOG=hnh_server=info \
    $BIN --seed 42 --bots "$BOTS" --saturated --perf > "$L0" 2>&1 &
  PIDS+=($!)
fi

# Wait until every node reports live sessions (boot complete).
ok=0
for _ in $(seq 1 120); do
  all=1
  for L in "${LOGS[@]}"; do
    rg -q "sessions=" "$L" 2>/dev/null || all=0
  done
  [ "$all" = 1 ] && { ok=1; break; }
  sleep 1
done
[ "$ok" = 1 ] || { echo "boot failed"; for L in "${LOGS[@]}"; do tail -3 "$L"; done; exit 1; }

echo "booted (mode=$MODE bots/node=$BOTS); settling ${SETTLE}s..."
sleep "$SETTLE"

# Measure from a clean line offset so the ramp-up is excluded.
MARKS=()
for L in "${LOGS[@]}"; do MARKS+=("$(wc -l < "$L")"); done
echo "measuring ${WINDOW}s..."
sleep "$WINDOW"

for i in "${!LOGS[@]}"; do
  L=${LOGS[$i]}; MK=${MARKS[$i]}
  echo "== node$i (from line $MK) =="
  seg() { tail -n +"$MK" "$L"; }
  pct() { sort -n | awk '{a[NR]=$1} END {if (NR) printf "p50=%s p95=%s max=%s n=%d", a[int(NR*0.5)], a[int(NR*0.95)], a[NR], NR}'; }
  echo "  tick_us      $(seg | rg -o 'tick_us=([0-9]+)' -r '$1' | pct)"
  echo "  wmax_tick_us $(seg | rg -o 'wmax_tick_us=([0-9]+)' -r '$1' | sort -n | tail -1)"
  echo "  mean_tick_us $(seg | rg -o 'mean_tick_us=([0-9]+)' -r '$1' | sort -n | awk '{a[NR]=$1} END {if (NR) print a[NR]}')"
  for f in phase_mv_us phase_ai_us phase_combat_us phase_vis_us phase_cluster_us \
           phase_guests_us mvbat_scan_us mvbat_encode_us mvbat_fanout_us \
           guests_encode_us guests_fanout_us guests_pose_us; do
    echo "  $f $(seg | rg -o "$f=([0-9]+)" -r '$1' | pct)"
  done
  echo "  mvbat_movers $(seg | rg -o 'mvbat_movers=([0-9]+)' -r '$1' | pct)"
  echo "  move_blocks  $(seg | rg -o 'move_blocks=([0-9]+)' -r '$1' | sort -n | awk '{a[NR]=$1} END {if (NR) print "p50=" a[int(NR*0.5)] " max=" a[NR]}')"
  echo "  move_cells   $(seg | rg -o 'move_cells=([0-9]+)' -r '$1' | sort -n | awk '{a[NR]=$1} END {if (NR) print "p50=" a[int(NR*0.5)] " max=" a[NR]}')"
  for f in sessions guests gobs animals; do
    V=$(seg | rg -o "$f=([0-9]+)" -r '$1' | sort -n | tail -1)
    echo "  $f=$V"
  done
done
stop_all
echo "done."
