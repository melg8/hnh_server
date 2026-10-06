#!/usr/bin/env bash
# Session 36 verification: the bow chain (woodbow / stonearrow /
# bonearrow recipes with RoB type-weighted quality), animal bone loot,
# and the bow carrying pose on equip. Phases:
#   bow-units   - the session-36 unit battery (quality math, bundles,
#                 carrying pose, starter kit, bone loot consistency)
#   full        - the whole workspace test battery (200 tests)
#   lint        - fmt --check + clippy -D warnings
#   boot        - release binary boots with the extended tables
#   all (default) - bow-units + full + lint + boot.
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

fail() { echo "SESSION36 VERIFY: FAIL ($1)"; exit 1; }

phase_bow_units() {
  echo "== phase bow-units: session-36 battery =="
  local OUT U
  OUT=$(cargo test bow 2>&1) || { echo "$OUT" | tail -30; fail "bow tests"; }
  U=$(echo "$OUT" | grep -c '^test .* ok$' || true)
  [ "${U:-0}" -ge 4 ] || fail "bow battery ($U/4)"
  echo "BOW UNITS: OK ($U tests)"
  OUT=$(cargo test bow_chain_recipes 2>&1) || { echo "$OUT" | tail -30; fail "recipe consistency"; }
  echo "RECIPE CONSISTENCY: OK"
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
  echo "== phase boot: release binary + new paginae =="
  cargo build --release >/dev/null 2>&1 || fail "release build"
  local LOG=/tmp/hnh_s36_boot.log
  rm -f "$LOG" /tmp/hnh_s36_boot.json
  HNH_SAVE_FILE=/tmp/hnh_s36_boot.json RUST_LOG=hnh_server=info \
    timeout 12 target/release/hnh-server --seed 42 --perf > "$LOG" 2>&1 || true
  grep -q "game server listening\|auth tcp\|listening" "$LOG" \
    || { tail -20 "$LOG"; fail "server boot"; }
  # The starter kit and recipes are unit-verified; the boot phase only
  # proves the release binary comes up with the extended tables.
  echo "BOOT: OK"
}

phase="${1:-all}"
case "$phase" in
bow-units) phase_bow_units ;;
full)      phase_full ;;
lint)      phase_lint ;;
boot)      phase_boot ;;
all)       phase_bow_units; phase_full; phase_lint; phase_boot ;;
*) echo "usage: $0 [bow-units|full|lint|boot|all]"; exit 2 ;;
esac
echo "SESSION36 VERIFY: OK"
