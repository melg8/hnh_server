#!/usr/bin/env bash
# Session 33 verification: cross-node station menus (relay act +
# piggybacked snapshot) and the owner-filtered populate fix (no shadow
# statics). Phases:
#   station  - unit battery: publish snapshot, guest click -> menu +
#              relay, authority validation order, item relay + cursor
#              safety, ack consumption, refusal system lines.
#   populate - unit battery: owner-filtered spawn (no static/animal on
#              a non-owner node), Sub-driven populate announces within
#              the subscribed cell + materializes touched grids,
#              grids_touching_cell coverage.
#   wire     - real-UDP regression: world entry + farming flow (the
#              single-node populate path must be unchanged).
#   cluster  - the session-30 story on this tree: 2-node real cluster,
#              60-bot cohorts per node, perf budget, sharded save,
#              restart restore (this exercises the Sub-driven populate
#              against the REAL mesh: bots near cell boundaries render
#              guest statics from the owner, not local shadows).
#   all (default) - everything above in order.
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

fail() { echo "E2E FAIL: $1"; exit 1; }

phase_station() {
  echo "== phase station: relay + snapshot unit battery =="
  local out
  out=$(cargo test --release station 2>&1 | grep -c "^test .* ok$")
  [ "$out" -ge 8 ] || fail "station tests: expected >=8 green, got $out"
  echo "station units: $out OK"
}

phase_populate() {
  echo "== phase populate: owner-filtered + Sub-driven unit battery =="
  local own sub grids
  own=$(cargo test --release owner_filtered_populate 2>&1 | grep -c " ok$")
  [ "$own" -ge 1 ] || fail "owner-filtered test missing"
  sub=$(cargo test --release sub_driven_populate 2>&1 | grep -c " ok$")
  [ "$sub" -ge 1 ] || fail "Sub-driven populate test missing"
  grids=$(cargo test --release grids_touching_cell 2>&1 | grep -c " ok$")
  [ "$grids" -ge 1 ] || fail "grids_touching_cell test missing"
  echo "populate units: $own + $sub + $grids OK"
}

phase_wire() {
  echo "== phase wire: world entry + farming flow (real UDP) =="
  bash scripts/verify_session32.sh wire >/dev/null 2>&1 \
    || fail "wire regression (session 32 phase) failed"
  echo "wire regression OK"
}

phase_cluster() {
  echo "== phase cluster: session-30 story (real mesh + Sub populate) =="
  bash scripts/verify_session30.sh s33 >/dev/null 2>&1 \
    || fail "session-30 e2e failed on this tree"
  echo "cluster story OK (see verify_session30.sh s33 output for the"
  echo "per-phase evidence: relay-static, cursor-merge, vis-cache load,"
  echo "cluster load, shard persist, restart restore)"
}

case "${1:-all}" in
  station)  phase_station ;;
  populate) phase_populate ;;
  wire)     phase_wire ;;
  cluster)  phase_cluster ;;
  all)
    phase_station
    phase_populate
    phase_wire
    phase_cluster
    echo "SESSION33 E2E: OK"
    ;;
  *) echo "usage: $0 [station|populate|wire|cluster|all]"; exit 2 ;;
esac
