#!/usr/bin/env bash
# Session 38 verification: player-versus-player archery (local and
# cross-node arrow fights, PvpArrow/PvpArrowResult relay). Phases:
#   pvp-units - the session-38 battery (aim vs party click, local hit,
#               knockout, guest shot relay, authority-side apply)
#   full      - the whole workspace test battery
#   lint      - fmt --check + clippy -D warnings
#   boot      - release binary boots with the PvP archery live
#   all (default) - pvp-units + full + lint + boot.
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

fail() { echo "SESSION38 VERIFY: FAIL ($1)"; exit 1; }

phase_pvp_units() {
  echo "== phase pvp-units: session-38 battery =="
  local OUT U
  OUT=$(cargo test --bin hnh-server -- pvp 2>&1) \
    || { echo "$OUT" | tail -30; fail "pvp battery"; }
  U=$(echo "$OUT" | grep -c '^test .* ok$' || true)
  [ "${U:-0}" -ge 5 ] || fail "pvp battery ($U/5)"
  echo "PVP UNITS: OK ($U tests)"
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
  echo "== phase boot: release binary + PvP archery =="
  cargo build --release >/dev/null 2>&1 || fail "release build"
  local LOG=/tmp/hnh_s38_boot.log
  rm -f "$LOG" /tmp/hnh_s38_boot.json
  HNH_SAVE_FILE=/tmp/hnh_s38_boot.json RUST_LOG=hnh_server=info \
    timeout 12 target/release/hnh-server --seed 42 --perf > "$LOG" 2>&1 || true
  grep -q "game server listening\|auth tcp\|listening" "$LOG" \
    || { tail -20 "$LOG"; fail "server boot"; }
  echo "BOOT: OK"
}

phase="${1:-all}"
case "$phase" in
pvp-units) phase_pvp_units ;;
full)      phase_full ;;
lint)      phase_lint ;;
boot)      phase_boot ;;
all)       phase_pvp_units; phase_full; phase_lint; phase_boot ;;
*) echo "usage: $0 [pvp-units|full|lint|boot|all]"; exit 2 ;;
esac
echo "SESSION38 VERIFY: OK"
