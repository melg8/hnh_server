#!/usr/bin/env bash
# Session 16 leaf-1/leaf-2 gates: headless client-side probe. It drives the
# REAL client classes (Session + UI + RemoteUI, the exact post-play receive
# path, no GL) against a live server; any throwable there is precisely what
# kills the real client and produces the reported black screen.
# Usage: verify_ui_probe.sh compile|run|equip
set -uo pipefail
cd "$(dirname "$0")/../.."

fail() { echo "FAIL: $1"; exit 1; }

JAVAC="java -m jdk.compiler/com.sun.tools.javac.Main"
JARS="lib/js-14.jar:lib/jogl.jar:lib/gluegen-rt.jar:lib/jogg.jar:lib/jorbis.jar:lib/jnlp.jar:lib/antlr-3.2.jar:lib/haven-res.jar"
PROBE_LOG=/tmp/probe-out.log

stop_server() { pkill -TERM -f hnh-server 2>/dev/null; sleep 2; }

compile_probe() {
  java -version >/dev/null 2>&1 || fail "no java runtime (JDK 21+ required)"
  mkdir -p build-classes
  find src -name "*.java" > /tmp/probe-sources.txt
  find client-probe -name "*.java" >> /tmp/probe-sources.txt
  # shellcheck disable=SC2086
  $JAVAC -encoding UTF-8 -nowarn -d build-classes -cp "$JARS" \
    @/tmp/probe-sources.txt > /tmp/probe-compile.log 2>&1
  grep -q "error:" /tmp/probe-compile.log && {
    head -20 /tmp/probe-compile.log; fail "client+probe compile"; }
}

case "${1:-all}" in
  compile)
    compile_probe
    echo "UI PROBE COMPILE: OK"
    ;;
  run)
    compile_probe
    [ -x server/target/release/hnh-server ] \
      || fail "server binary missing (cd server && cargo build --release)"
    [ -d gameres ] || fail "gameres/ missing (see HANDOFF 'Resource pack')"
    rm -f server/target/probe-ui-save.json
    (cd server && HNH_SAVE_FILE=target/probe-ui-save.json \
      ./target/release/hnh-server --seed 42 > /tmp/probe-server.log 2>&1 &)
    sleep 3
    HAVEN_RESDIR="$PWD/res/compiled" java -Djava.awt.headless=true -cp "build-classes:$JARS" UiProbe probeuser probepass run \
      > "$PROBE_LOG" 2>&1
    rc=$?
    stop_server
    [ $rc -eq 0 ] || { tail -30 "$PROBE_LOG"; fail "probe exited $rc"; }
    grep -q "UI PROBE: OK" "$PROBE_LOG" || {
      tail -30 "$PROBE_LOG"; fail "probe did not pass"; }
    echo "UI PROBE RUN: OK"
    ;;
  equip)
    compile_probe
    [ -x server/target/release/hnh-server ] \
      || fail "server binary missing (cd server && cargo build --release)"
    [ -d gameres ] || fail "gameres/ missing (see HANDOFF 'Resource pack')"
    rm -f server/target/probe-ui-save.json
    (cd server && HNH_SAVE_FILE=target/probe-ui-save.json \
      ./target/release/hnh-server --seed 42 > /tmp/probe-server.log 2>&1 &)
    sleep 3
    HAVEN_RESDIR="$PWD/res/compiled" java -Djava.awt.headless=true -cp "build-classes:$JARS" UiProbe probeuser probepass equip \
      > "$PROBE_LOG" 2>&1
    rc=$?
    stop_server
    [ $rc -eq 0 ] || { tail -30 "$PROBE_LOG"; fail "probe exited $rc"; }
    grep -q "UI PROBE EQUIP: OK" "$PROBE_LOG" || {
      tail -30 "$PROBE_LOG"; fail "probe equip did not pass"; }
    echo "UI PROBE EQUIP: OK"
    ;;
  *)
    fail "unknown gate: $1"
    ;;
esac
