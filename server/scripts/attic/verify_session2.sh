#!/usr/bin/env bash
# Session 2 verification gates. Usage: verify_session2.sh <gate>
# Gates: cert-and-entry | persistence | shards | load | committed
set -uo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.cargo/bin:$PATH"

fail() { echo "FAIL: $1"; exit 1; }

case "${1:-all}" in
  cert-and-entry)
    rm -f server/certs/authsrv.crt.pem server/certs/authsrv.key.pem
    (cd server && (./target/release/hnh-server --seed 42 > /tmp/g1.log 2>&1 &))
    sleep 3
    grep -q "auth server (TLS) listening" /tmp/g1.log || fail "auth not listening"
    python3 server/scripts/test_client.py gateuser | grep -q "WORLD ENTRY: OK" || fail "world entry"
    kill -TERM $(pgrep -f hnh-server) 2>/dev/null; sleep 2
    echo "G1: ALL PASS"
    ;;
  persistence)
    rm -rf save
    (cd server && (./target/release/hnh-server --seed 42 > /tmp/g3a.log 2>&1 &))
    sleep 3
    python3 server/scripts/test_client.py gateuser | grep -q "WORLD ENTRY: OK" || fail "entry a"
    kill -TERM $(pgrep -f hnh-server); sleep 2
    [ -f save/world.json ] || fail "no save file"
    (cd server && (./target/release/hnh-server --seed 42 > /tmp/g3b.log 2>&1 &))
    sleep 3
    python3 server/scripts/test_client.py gateuser | grep -q "WORLD ENTRY: OK" || fail "entry b"
    grep -q "restoring persisted character" /tmp/g3b.log || fail "no restore"
    kill -TERM $(pgrep -f hnh-server) 2>/dev/null; sleep 2
    echo "G3: ALL PASS"
    ;;
  shards)
    (cd server && (./target/release/hnh-server --seed 42 --shards 4 > /tmp/g4.log 2>&1 &))
    sleep 3
    N=$(grep -c "shard listening" /tmp/g4.log)
    [ "$N" -eq 4 ] || fail "expected 4 shards, got $N"
    python3 server/scripts/test_client.py gateuser | grep -q "WORLD ENTRY: OK" || fail "entry"
    grep -qE "session accepted.*shard=" /tmp/g4.log || fail "no shard in accept log"
    kill -TERM $(pgrep -f hnh-server) 2>/dev/null; sleep 2
    echo "G4: ALL PASS"
    ;;
  load)
    (cd server && (./target/release/hnh-server --seed 42 --bots 1000 --saturated --perf > /tmp/g5.log 2>&1 &))
    sleep 100
    A=$(grep -c "session accepted" /tmp/g5.log)
    [ "$A" -ge 990 ] || fail "only $A/1000 sessions"
    # Steady-state tick must stay under the 100 ms budget.
    BAD=$(grep -oE "tick_us=[0-9]+" /tmp/g5.log | tail -10 | cut -d= -f2 | awk '$1 >= 100000 {c++} END {print c+0}')
    [ "$BAD" -eq 0 ] || fail "tick budget exceeded"
    grep -q "fight started" /tmp/g5.log || fail "no combat activity"
    kill -TERM $(pgrep -f hnh-server) 2>/dev/null; sleep 2
    echo "G5: ALL PASS"
    ;;
  committed)
    [ -z "$(git status --porcelain server/ docs/ HANDOFF.md)" ] || fail "dirty tree"
    git fetch origin master -q
    [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/master)" ] || fail "not pushed"
    echo "G7: ALL PASS"
    ;;
  all)
    "$0" cert-and-entry && "$0" persistence && "$0" shards && "$0" committed
    ;;
  *) echo "usage: $0 <gate>"; exit 2 ;;
esac
