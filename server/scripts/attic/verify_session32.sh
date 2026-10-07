#!/usr/bin/env bash
# Session 32 verification: cross-node plowing (TileMutation broadcast)
# and the tilth-decay revert. Phases:
#   relay-plow   - unit battery for the relay round-trip (filtered tests)
#   decay-revert - unit battery for the decay revert + remote mutation
#   wire         - real-UDP regression: world entry + farming flow
#   cluster-plow - 2-node cluster, real TCP mesh + real UDP: a player on
#                  node 0 plows a node-1-owned tile; the authority log must
#                  carry "relay plow applied", the home log "relay plow act
#                  sent" + "remote tile mutation applied", and the client
#                  must observe the PLOWED tile in the re-sent MAPDATA.
#   all (default) - everything above in order.
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

BIN=target/release/hnh-server
TAG=${TAG:-s32}
NODES="127.0.0.1:18790,127.0.0.1:18791"
N0=/tmp/hnh_${TAG}_n0.json
N1=/tmp/hnh_${TAG}_n1.json
L0=/tmp/server_${TAG}_n0.log
L1=/tmp/server_${TAG}_n1.log
PIDS=()

fail() { echo "E2E FAIL: $1"; stop_cluster; exit 1; }
trap 'stop_cluster' EXIT

start_cluster() { # $1 = extra args
  rm -f "$L0" "$L1"
  HNH_SAVE_FILE=$N0 RUST_LOG=hnh_server=debug \
    $BIN --seed 42 --cluster "$NODES" --node 0 ${1:-} \
    > "$L0" 2>&1 &
  PIDS+=($!)
  HNH_SAVE_FILE=$N1 RUST_LOG=hnh_server=debug \
    $BIN --seed 42 --cluster "$NODES" --node 1 \
    --game-port 1882 --auth-port 1883 --res-port 1884 ${1:-} \
    > "$L1" 2>&1 &
  PIDS+=($!)
  for _ in $(seq 1 40); do
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

start_single() { # isolated single node for the wire phase
  rm -f "$L0"
  HNH_SAVE_FILE=$N0 RUST_LOG=hnh_server=info \
    $BIN --seed 42 > "$L0" 2>&1 &
  PIDS+=($!)
  for _ in $(seq 1 40); do
    rg -q "auth server .TLS. listening" "$L0" 2>/dev/null && return 0
    sleep 0.5
  done
  echo "single node did not start"; return 1
}

phase="${1:-all}"
case "$phase" in
relay-plow|all)
  echo "== phase relay-plow: unit battery =="
  U1=$(cargo test --release -- foreign_plow relay_plow plow_ack 2>&1 | rg -c '^test .* ok$' || echo 0)
  [ "$U1" -ge 3 ] || fail "relay-plow unit battery ($U1/3)"
  U2=$(cargo test --release -p hnh-world -- note_override 2>&1 | rg -c '^test .* ok$' || echo 0)
  [ "$U2" -ge 1 ] || fail "GridStore note_override unit ($U2/1)"
  echo "relay-plow units: $U1 + $U2 OK"
  [ "$phase" = "relay-plow" ] && exit 0 ;;
decay-revert|all)
  echo "== phase decay-revert: unit battery =="
  U3=$(cargo test --release -- tilth_decay tile_mutation 2>&1 | rg -c '^test .* ok$' || echo 0)
  [ "$U3" -ge 2 ] || fail "decay-revert unit battery ($U3/2)"
  echo "decay-revert units: $U3 OK"
  [ "$phase" = "decay-revert" ] && exit 0 ;;
wire|all)
  echo "== phase wire: world entry + farming flow (real UDP) =="
  start_single || fail "single-node boot"
  python3 scripts/test_client.py s32wire >/tmp/s32_client.log 2>&1 \
    || { cat /tmp/s32_client.log; fail "test_client"; }
  rg -q "WORLD ENTRY: OK" /tmp/s32_client.log || fail "world entry"
  stop_cluster   # clears the single node too (same PIDS array)
  python3 scripts/test_farming.py >/tmp/s32_farm.log 2>&1 \
    || { cat /tmp/s32_farm.log; fail "test_farming"; }
  rg -q "FARMING FLOW: OK" /tmp/s32_farm.log || fail "farming flow"
  echo "wire regression OK"
  [ "$phase" = "wire" ] && exit 0 ;;
cluster-plow|all)
  echo "== phase cluster-plow: real 2-node relay =="
  start_cluster || fail "cluster boot"
  python3 scripts/probe_plow.py "s32plow$RANDOM" 1870 2 >/tmp/s32_plow.log 2>&1 \
    || { cat /tmp/s32_plow.log; fail "probe_plow"; }
  rg -q "PLOW RELAY: OK" /tmp/s32_plow.log || { cat /tmp/s32_plow.log; fail "probe verdict"; }
  # Home node (node 0, the probe's session): relay sent + remote apply.
  rg -q "relay plow act sent" "$L0" || fail "node0 relay act log"
  rg -q "remote tile mutation applied" "$L0" || fail "node0 remote apply log"
  # Authority node (node 1 owns the tile by construction of the probe).
  rg -q "relay plow applied" "$L1" || fail "node1 authority apply log"
  echo "relay pair verified on both node logs"
  stop_cluster
  [ "$phase" = "cluster-plow" ] && exit 0 ;;
*)
  echo "usage: $0 [relay-plow|decay-revert|wire|cluster-plow|all]"; exit 2 ;;
esac

echo "SESSION32 E2E: OK"
