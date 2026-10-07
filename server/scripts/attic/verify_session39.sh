#!/usr/bin/env bash
# Session 39 verification: melee PvP between players (local openings
# duel + cross-node PvpSwing/PvpSwingResult relay). Phases:
#   melee-units - the session-39 battery (duel open through the flower
#                 menu, chip-until-opening, lethal knockout, mutual
#                 duel, relay ship, authority apply+answer, resync)
#   full        - the whole workspace test battery
#   lint        - fmt --check + clippy -D warnings
#   boot        - release binary boots with melee PvP live
#   wire        - probe_melee.py against a live server (flower menu ->
#                 Fight petal -> frv windows -> landed hit -> HP drop)
#   all (default) - melee-units + full + lint + boot.
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

fail() { echo "SESSION39 VERIFY: FAIL ($1)"; exit 1; }

phase_melee_units() {
  echo "== phase melee-units: session-39 battery =="
  local OUT U
  OUT=$(cargo test --bin hnh-server -- melee 2>&1) \
    || { echo "$OUT" | tail -30; fail "melee battery"; }
  U=$(echo "$OUT" | grep -c '^test .* ok$' || true)
  [ "${U:-0}" -ge 7 ] || fail "melee battery ($U/7)"
  echo "MELEE UNITS: OK ($U tests)"
}

phase_full() {
  echo "== phase full: whole battery =="
  local OUT T
  OUT=$(cargo test 2>&1) || { echo "$OUT" | tail -30; fail "full battery"; }
  T=$(echo "$OUT" | grep -o 'test result: ok' | wc -l)
  [ "$T" -ge 5 ] || fail "battery result count ($T)"
  echo "FULL BATTERY: OK"
}

phase_lint() {
  echo "== phase lint: fmt + clippy =="
  cargo fmt --all -- --check || fail "fmt"
  cargo clippy --all-targets -- -D warnings >/dev/null 2>&1 \
    || fail "clippy"
  echo "LINT: OK"
}

phase_boot() {
  echo "== phase boot: release binary + melee PvP =="
  cargo build --release >/dev/null 2>&1 || fail "release build"
  local LOG=/tmp/hnh_s39_boot.log
  rm -f "$LOG" /tmp/hnh_s39_boot.json
  HNH_SAVE_FILE=/tmp/hnh_s39_boot.json RUST_LOG=hnh_server=info \
    timeout 12 target/release/hnh-server --seed 42 --perf > "$LOG" 2>&1 || true
  grep -q "game server listening\|auth tcp\|listening" "$LOG" \
    || { tail -20 "$LOG"; fail "server boot"; }
  echo "BOOT: OK"
}

phase_wire() {
  echo "== phase wire: melee PvP probe =="
  python3 scripts/probe_melee.py >/tmp/hnh_s39_wire.log 2>&1 \
    || { tail -30 /tmp/hnh_s39_wire.log; fail "melee wire probe"; }
  grep -q "MELEE WIRE: OK" /tmp/hnh_s39_wire.log || fail "melee wire marker"
  echo "WIRE: OK"
}

phase="${1:-all}"
case "$phase" in
melee-units) phase_melee_units ;;
full)      phase_full ;;
lint)      phase_lint ;;
boot)      phase_boot ;;
wire)      phase_wire ;;
all)
  phase_melee_units
  phase_full
  phase_lint
  phase_boot
  ;;
*)
  echo "usage: $0 [melee-units|full|lint|boot|wire|all]" >&2
  exit 2
  ;;
esac
echo "SESSION39 VERIFY: OK"
