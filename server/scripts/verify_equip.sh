#!/usr/bin/env bash
# Session 16 leaf-2 gates: the equipment paperdoll (epry).
# Usage: verify_equip.sh unit|e2e
set -uo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.cargo/bin:$PATH"

fail() { echo "FAIL: $1"; exit 1; }

stop_server() { pkill -TERM -f hnh-server 2>/dev/null; sleep 2; }

case "${1:-all}" in
  unit)
    (cd server && cargo test --release --bin hnh-server epry 2>&1 | tail -2) \
      | grep -q "test result: ok" || fail "equipment unit tests"
    echo "UNIT EQUIP: ALL PASS"
    ;;
  e2e)
    [ -x server/target/release/hnh-server ] \
      || fail "server binary missing (cd server && cargo build --release)"
    rm -f server/target/equip-test-save.json
    (cd server && HNH_SAVE_FILE=target/equip-test-save.json \
      ./target/release/hnh-server --seed 42 > /tmp/equip-server.log 2>&1 &)
    sleep 3
    python3 server/scripts/test_equip.py equipbot | tail -1 \
      | grep -q "EQUIP FLOW: OK" || { stop_server; fail "equip bot flow"; }
    # Graceful shutdown snapshots the equipped character; the restarted
    # server must restore slot 5.
    stop_server
    (cd server && HNH_SAVE_FILE=target/equip-test-save.json \
      ./target/release/hnh-server --seed 42 > /tmp/equip-server2.log 2>&1 &)
    sleep 3
    python3 server/scripts/test_equip.py persistcheck | tail -1 \
      | grep -q "EQUIP PERSIST: OK" || { stop_server; fail "equip persistence"; }
    stop_server
    echo "E2E EQUIP: ALL PASS"
    ;;
  *)
    fail "unknown gate: $1"
    ;;
esac
