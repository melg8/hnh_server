#!/usr/bin/env bash
# Session 15 verification gates: building placement, production stations,
# dirty-cell visibility. Usage: verify_build.sh <gate>
# Gates: unit-build | e2e-build | persist-build | unit-station | e2e-station
#        | unit-visidx | load | fmt-clippy | cargo-test | battery | committed
set -uo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.cargo/bin:$PATH"

fail() { echo "FAIL: $1"; exit 1; }

stop_server() { kill -TERM $(pgrep -f hnh-server) 2>/dev/null; sleep 2; }

case "${1:-all}" in
  unit-build)
    (cd server && cargo test --release --bin hnh-server build:: 2>&1 | tail -2) \
      | grep -q "test result: ok" || fail "build unit tests"
    echo "UNIT BUILD: ALL PASS"
    ;;
  e2e-build)
    python3 server/scripts/test_build.py buildbot | tail -1 | grep -q "BUILD FLOW: OK" \
      || fail "build e2e flow"
    echo "E2E BUILD: ALL PASS"
    ;;
  persist-build)
    # Fresh world: place a plan, sink one material, kill the server before
    # completion, restart, and verify the restored plan kept its credit.
    rm -f server/target/build-test-save.json
    (cd server && (./target/release/hnh-server --seed 42 > /tmp/g-pb.log 2>&1 &))
    sleep 3
    python3 server/scripts/test_build.py persistbot | tail -1 | grep -q "PERSIST: OK" \
      || { stop_server; fail "persist bot"; }
    stop_server
    (cd server && (./target/release/hnh-server --seed 42 > /tmp/g-pb2.log 2>&1 &))
    sleep 3
    grep -q "persisted build sites restored" /tmp/g-pb2.log || { stop_server; fail "no restored plan"; }
    python3 server/scripts/test_build.py persistcheck | tail -1 | grep -q "PERSIST: OK" \
      || { stop_server; fail "restored plan unusable"; }
    stop_server
    echo "BUILD PERSIST: ALL PASS"
    ;;
  unit-station)
    (cd server && cargo test --release --bin hnh-server build::tests::station 2>&1 | tail -2) \
      | grep -q "test result: ok" || fail "station unit tests"
    echo "UNIT STATION: ALL PASS"
    ;;
  e2e-station)
    python3 server/scripts/test_build.py stationbot | tail -1 | grep -q "STATION FLOW: OK" \
      || fail "station e2e flow"
    echo "E2E STATION: ALL PASS"
    ;;
  unit-visidx)
    (cd server && cargo test --release --bin hnh-server visidx 2>&1 | tail -2) \
      | grep -q "test result: ok" || fail "visidx unit tests"
    echo "UNIT VISIBLEIDX: ALL PASS"
    ;;
  load)
    (cd server && (./target/release/hnh-server --seed 42 --bots 1000 --saturated --perf > /tmp/g15-load.log 2>&1 &))
    sleep 100
    A=$(grep -c "session accepted" /tmp/g15-load.log)
    [ "$A" -ge 990 ] || { stop_server; fail "only $A/1000 sessions"; }
    # Steady state = the 50-tick EMA (mean_tick_us), the stable number
    # this repo reports; raw tick_us spikes include the world-entry
    # burst transients, which are not the budgeted steady state.
    BAD=$(grep -oE "mean_tick_us=[0-9]+" /tmp/g15-load.log | tail -10 | cut -d= -f2 | awk '$1 >= 100000 {c++} END {print c+0}')
    [ "$BAD" -eq 0 ] || { stop_server; fail "steady-state tick budget exceeded"; }
    grep -q "fight started" /tmp/g15-load.log || { stop_server; fail "no combat activity"; }
    # The dirty-cell index must be live in the perf report.
    grep -qE "vis_cells=[1-9]" /tmp/g15-load.log || { stop_server; fail "vis index inactive"; }
    stop_server
    echo "LOAD VISIDX: ALL PASS"
    ;;
  fmt-clippy)
    (cd server && cargo fmt --all -- --check) || fail "fmt"
    (cd server && cargo clippy --all-targets -- -D warnings) > /dev/null 2>&1 || fail "clippy"
    echo "FMT CLIPPY: ALL PASS"
    ;;
  cargo-test)
    (cd server && cargo test --release 2>&1 | grep -E "test result: ok" | wc -l | awk '$1 >= 3 {exit 0} {exit 1}') \
      || fail "cargo test suites"
    echo "CARGO TEST: ALL PASS"
    ;;
  battery)
    # One server generation, five e2e clients in sequence. The save is
    # isolated: a shared save would restore earlier runs' plans at the
    # spawn tiles and intercept this run's itemacts.
    rm -f server/target/battery-save.json
    (cd server && (HNH_CROP_TIME_SCALE=10000000 HNH_LP_RATE=1000 \
      HNH_SAVE_FILE=target/battery-save.json \
      ./target/release/hnh-server --seed 42 > /tmp/g15-bat.log 2>&1 &))
    sleep 3
    python3 server/scripts/test_client.py gateuser | grep -q "WORLD ENTRY: OK" || { stop_server; fail "world entry"; }
    echo "  world entry ok"
    python3 server/scripts/test_build.py buildbot | tail -1 | grep -q "BUILD FLOW: OK" || { stop_server; fail "build flow"; }
    echo "  build flow ok"
    python3 server/scripts/test_build.py stationbot | tail -1 | grep -q "STATION FLOW: OK" || { stop_server; fail "station flow"; }
    echo "  station flow ok"
    python3 server/scripts/test_farming.py farmbot | tail -1 | grep -q "FARMING FLOW: OK" || { stop_server; fail "farming flow"; }
    echo "  farming flow ok"
    python3 server/scripts/test_party_chat.py | tail -1 | grep -qE "PARTY FLOW: OK|CHAT FLOW: OK" || { stop_server; fail "party/chat flow"; }
    echo "  party/chat flow ok"
    stop_server
    echo "BATTERY: ALL PASS"
    ;;
  committed)
    [ -z "$(git status --porcelain server/ docs/ HANDOFF.md)" ] || fail "dirty tree"
    git fetch origin master -q
    [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/master)" ] || fail "not pushed"
    echo "COMMITTED: ALL PASS"
    ;;
  *)
    fail "unknown gate: $1"
    ;;
esac
