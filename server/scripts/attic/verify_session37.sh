#!/usr/bin/env bash
# Session 37 verification: bow ranged combat (the Shoot action - aim
# meter, arrow economy, Fandom damage formula, hit chance, chase and
# cancel semantics). Phases:
#   archery-units - the session-37 battery (formulae + engagement flow)
#   full          - the whole workspace test battery
#   lint          - fmt --check + clippy -D warnings
#   boot          - release binary boots with the ranged combat live
#   all (default) - archery-units + full + lint + boot.
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

fail() { echo "SESSION37 VERIFY: FAIL ($1)"; exit 1; }

phase_archery_units() {
  echo "== phase archery-units: session-37 battery =="
  local OUT U
  OUT=$(cargo test archery 2>&1) || { echo "$OUT" | tail -30; fail "archery units"; }
  U=$(echo "$OUT" | grep -c '^test .* ok$' || true)
  [ "${U:-0}" -ge 4 ] || fail "archery battery ($U/4)"
  echo "ARCHERY UNITS: OK ($U tests)"
  OUT=$(cargo test --bin hnh-server -- bow_ arrow_ aim_ walk_cancels lethal 2>&1) \
    || { echo "$OUT" | tail -30; fail "engagement flow"; }
  U=$(echo "$OUT" | grep -c '^test .* ok$' || true)
  [ "${U:-0}" -ge 8 ] || fail "engagement battery ($U/8)"
  echo "ENGAGEMENT FLOW: OK ($U tests)"
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
  echo "== phase boot: release binary + ranged combat =="
  cargo build --release >/dev/null 2>&1 || fail "release build"
  local LOG=/tmp/hnh_s37_boot.log
  rm -f "$LOG" /tmp/hnh_s37_boot.json
  HNH_SAVE_FILE=/tmp/hnh_s37_boot.json RUST_LOG=hnh_server=info \
    timeout 12 target/release/hnh-server --seed 42 --perf > "$LOG" 2>&1 || true
  grep -q "game server listening\|auth tcp\|listening" "$LOG" \
    || { tail -20 "$LOG"; fail "server boot"; }
  echo "BOOT: OK"
}

phase="${1:-all}"
case "$phase" in
archery-units) phase_archery_units ;;
full)      phase_full ;;
lint)      phase_lint ;;
boot)      phase_boot ;;
all)       phase_archery_units; phase_full; phase_lint; phase_boot ;;
*) echo "usage: $0 [archery-units|full|lint|boot|all]"; exit 2 ;;
esac
echo "SESSION37 VERIFY: OK"
