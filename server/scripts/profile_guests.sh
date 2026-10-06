#!/usr/bin/env bash
# Session 41 guest-phase profile: 2-node cluster, short window, the
# guests-phase sub-attribution (encode/fanout/pose) goes to the logs.
set -u
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
BIN=target/release/hnh-server
TAG=${TAG:-s41p}
BOTS=${BOTS:-200}
WINDOW=${WINDOW:-45}
L0=/tmp/server_${TAG}_n0.log
L1=/tmp/server_${TAG}_n1.log
CPIDS=()
stop_c() { for p in "${CPIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
           for p in "${CPIDS[@]:-}"; do wait "$p" 2>/dev/null || true; done; CPIDS=(); }
trap 'stop_c' EXIT
rm -f "$L0" "$L1" /tmp/hnh_${TAG}_n0.json /tmp/hnh_${TAG}_n1.json
HNH_SAVE_FILE=/tmp/hnh_${TAG}_n0.json RUST_LOG=hnh_server=info \
  $BIN --seed 42 --cluster "127.0.0.1:18790,127.0.0.1:18791" --node 0 --bots "$BOTS" --saturated --perf \
  > "$L0" 2>&1 &
CPIDS+=($!)
HNH_SAVE_FILE=/tmp/hnh_${TAG}_n1.json RUST_LOG=hnh_server=info \
  $BIN --seed 42 --cluster "127.0.0.1:18790,127.0.0.1:18791" --node 1 --bots "$BOTS" --saturated --perf \
  --game-port 1882 --auth-port 1883 --res-port 1884 > "$L1" 2>&1 &
CPIDS+=($!)
ok=0
for _ in $(seq 1 90); do
  if rg -q "sessions=" "$L0" 2>/dev/null && rg -q "sessions=" "$L1" 2>/dev/null; then ok=1; break; fi
  sleep 1
done
[ "$ok" = 1 ] || { echo "boot failed"; tail -3 "$L0" "$L1"; exit 1; }
echo "booted; settling..."
sleep $((WINDOW / 2))
for L in "$L0" "$L1"; do
  for _ in $(seq 1 30); do
    sleep 3
    rg -q "guests=[1-9]" "$L" 2>/dev/null && break
  done
done
echo "measuring ${WINDOW}s..."
M0=$(wc -l < "$L0"); M1=$(wc -l < "$L1")
sleep "$WINDOW"
for pair in "0:$M0:$L0" "1:$M1:$L1"; do
  N=${pair%%:*}; REST=${pair#*:}; MK=${REST%%:*}; L=${REST#*:}
  echo "== node$N (from line $MK) =="
  tail -n +"$MK" "$L" | rg -o "tick_us=([0-9]+)" -r '$1' | sort -n | \
    awk '{a[NR]=$1} END {if (NR) print "tick p50=" a[int(NR*0.5)] " p95=" a[int(NR*0.95)] " max=" a[NR]}'
  for f in phase_mv_us phase_vis_us phase_combat_us phase_guests_us phase_cluster_us \
           vis_scan_us vis_spawn_us guests_encode_us guests_fanout_us guests_pose_us; do
    V=$(tail -n +"$MK" "$L" | rg -o "$f=([0-9]+)" -r '$1' | sort -n | \
      awk '{a[NR]=$1} END {if (NR) print "p50=" a[int(NR*0.5)] " p95=" a[int(NR*0.95)]}')
    echo "  $f $V"
  done
  G=$(tail -n +"$MK" "$L" | rg -o "guests=([0-9]+)" -r '$1' | sort -n | tail -1)
  S=$(tail -n +"$MK" "$L" | rg -o "sessions=([0-9]+)" -r '$1' | sort -n | tail -1)
  echo "  sessions=$S guests=$G"
done
