#!/usr/bin/env bash
# Session 17 leaf-3 gate: the charlist portrait layers are logged when a
# session reaches the character-selection screen, so a user bug report's
# server.log always carries the exact layer names and versions sent.
# Usage: verify_charlist_log.sh
set -uo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.cargo/bin:$PATH"

fail() { echo "FAIL: $1"; exit 1; }

[ -x server/target/release/hnh-server ] || fail "server binary missing"

rm -f /tmp/charlist-log-save.json /tmp/charlist-log.log
(cd server && HNH_SAVE_FILE=/tmp/charlist-log-save.json \
  ./target/release/hnh-server --seed 42 > /tmp/charlist-log.log 2>&1 &)
sleep 3

python3 server/scripts/test_client.py gateuser > /dev/null 2>&1
kill -TERM $(pgrep -f hnh-server) 2>/dev/null; sleep 2

# The log line must name the standing-frame layers (not the pose routers).
grep -q "charlist portrait layers announced" /tmp/charlist-log.log \
  || fail "portrait log line missing from server log"
grep -q "gfx/borka/body/standing/torso/male-0" /tmp/charlist-log.log \
  || fail "log line does not carry the portrait layer names"
echo "CHARLIST LOG GATE: OK"
