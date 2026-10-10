#!/bin/bash
# Session 89: the REAL Haven client (GL render path) against the REMOTE
# cluster profile - the actual multi-machine deployment shape - inside
# one sandbox using two loopback addresses as two "machines":
#
#   machine A = 127.0.0.1 (node 0: auth 1871 / game 1870, mesh 18790)
#   machine B = 127.0.0.2 (node 1: auth 1873 / game 1874, mesh 18791)
#
# The gate is STAGED (subcommands) because sandboxed CI environments can
# evict long-lived idle wrapper scripts: every stage is one short
# foreground invocation, the nodes survive detached (cluster-up.sh
# nohup+setsid), and the Xvfb+client pair lives exactly as long as the
# java process that renders through it.
#
#   run-remote-e2e.sh up      boot the two "machines" (waits for NODE UP)
#   run-remote-e2e.sh legA    real GL client through machine A: the FULL
#                             agent corpus (movement, 5 walk directions
#                             across cell boundaries - the client becomes
#                             a GUEST on machine B's cells, equipment,
#                             ground drop, cluster verdict, clean exit ->
#                             MSG_CLOSE -> instant persist on machine A)
#   run-remote-e2e.sh legB    the SAME character re-enters through
#                             machine B's OWN address 127.0.0.2:1873/1874
#                             (-Dhaven.authport/gameport overrides):
#                             machine A serves the snapshot, machine B
#                             receives the migration, the client walks
#                             (quick mode), clean exit
#   run-remote-e2e.sh down    stop both machines
#   run-remote-e2e.sh all     up + legA + legB + down (for hosts where a
#                             single long invocation is fine)
#
# Verdict lines: REMOTE GL MESH / REMOTE GL WALK / REMOTE GL RENDER /
# REMOTE GL MIGRATION / REMOTE GL CLIENT.
# Usage: run-remote-e2e.sh <stage> [username] [logtag]
set -u
STAGE="${1:-all}"
USERNAME="${2:-glremote89}"
TAG="${3:-remote89}"
REPO="${REPO:-$(cd "$(dirname "$0")/../.." && pwd)}"
TOOLS="${TOOLS:-/home/z/tools}"
J8="${J8:-$TOOLS/jogl-extract/jdk8u504-b01}"
JOGL="${JOGL:-$TOOLS/jogl-extract/jni/usr/lib/jni}"
X11="${X11:-$TOOLS/x11libs/usr/lib/x86_64-linux-gnu}"
SPEC="127.0.0.1:18790,127.0.0.2:18791"
SAVES=/tmp/hnh_remote_gl_saves
CLUSTER="$REPO/server/scripts/cluster-up.sh"
LOG0="$REPO/server/target/cluster-n0.log"
LOG1="$REPO/server/target/cluster-n1.log"
CA="/tmp/client_${TAG}_a.log"
CB="/tmp/client_${TAG}_b.log"

cd "$REPO"

boot_nodes() {
  "$CLUSTER" stop >/dev/null 2>&1
  rm -rf "$SAVES" && mkdir -p "$SAVES"
  : > "$LOG0"; : > "$LOG1"
  echo "== booting two REMOTE nodes (127.0.0.1 + 127.0.0.2 as machines) =="
  ( cd "$REPO/server" && SAVEDIR="$SAVES" RUST_LOG=hnh_server=debug \
      CLUSTER_SPEC="$SPEC" SELF=0 ./scripts/cluster-up.sh remote \
      >> "$REPO/server/target/remote-gl-n0.out" 2>&1 ) &
  ( cd "$REPO/server" && SAVEDIR="$SAVES" RUST_LOG=hnh_server=debug \
      CLUSTER_SPEC="$SPEC" SELF=1 ./scripts/cluster-up.sh remote \
      >> "$REPO/server/target/remote-gl-n1.out" 2>&1 ) &
  local ok0=0 ok1=0
  for _ in $(seq 1 150); do
      rg -q "NODE UP" "$REPO/server/target/remote-gl-n0.out" 2>/dev/null && ok0=1
      rg -q "NODE UP" "$REPO/server/target/remote-gl-n1.out" 2>/dev/null && ok1=1
      [ "$ok0" = 1 ] && [ "$ok1" = 1 ] && break
      sleep 2
  done
  if [ "$ok0" != 1 ] || [ "$ok1" != 1 ]; then
      echo "REMOTE GL CLIENT: FAIL (NODE UP timeout)"
      exit 1
  fi
  echo "REMOTE GL MESH: OK (both machines joined $SPEC)"
}

# One GL client leg: Xvfb and the JVM live and die inside THIS call - the
# client renders through the display, so both stay busy, and nothing is
# left behind to be reaped. $1 = extra java -D flags.
run_client() {
  local LOG="$1"; shift
  pkill -f "Xvfb :99" 2>/dev/null; pkill -f "haven.MainFrame" 2>/dev/null
  sleep 1
  rm -f /tmp/.X99-lock; rm -rf /tmp/.X11-unix/X99
  local CP="haven.jar:../lib/jogl.jar:../lib/gluegen-rt.jar:../lib/haven-res.jar:../lib/js-14.jar:../lib/jogg.jar:../lib/jorbis.jar:../lib/antlr-3.2.jar:../lib/jnlp.jar"
  # NOTE: the cd must be a standalone statement - `cd build && Xvfb &`
  # would background the WHOLE `cd && Xvfb` chain and run java from the
  # repo root, where the relative classpath cannot resolve haven.jar.
  (
    cd "$REPO/build" || exit 1
    Xvfb :99 -screen 0 1024x768x24 -nolisten tcp > /tmp/xvfb_${TAG}.log 2>&1 &
    XV=$!
    sleep 2
    DISPLAY=:99 LD_LIBRARY_PATH=$JOGL:$X11 LIBGL_ALWAYS_SOFTWARE=1 \
    timeout ${CLIENT_SECS:-500} $J8/bin/java -Dhaven.avadebug=1 -cp "$CP" \
      -Djava.library.path=$JOGL \
      -Dhaven.resdir=$REPO/gameres \
      -Dhaven.driveuser=$USERNAME \
      -Dhaven.driveexit=true \
      "$@" \
      -javaagent:$REPO/scripts/jogl/agent/driveagent.jar \
      haven.MainFrame > "$LOG" 2>&1
    rc=$?
    kill $XV 2>/dev/null
    exit $rc
  )
}

case "$STAGE" in
up)
  boot_nodes
  ;;
legA)
  run_client "$CA" -Dhaven.defserv=127.0.0.1
  echo "== leg A: real client through machine A (full agent corpus) =="
  movedA=$(rg -c "^MOVEMENT: MOVED" "$CA" 2>/dev/null || echo 0)
  teleA=$(rg -c "^NO TELEPORT: OK" "$CA" 2>/dev/null || echo 0)
  walkdirs=$(rg -c "WALKDIR .* attempt 0: ARRIVED" "$CA" 2>/dev/null || echo 0)
  clusterA=$(rg -c "^CLUSTER VERDICT: OK" "$CA" 2>/dev/null || echo 0)
  guestB=$(rg -c "guest ingested" "$LOG1" 2>/dev/null || echo 0)
  echo "leg A evidence: movement=$movedA noteleport=$teleA walkdirs=$walkdirs/5 cluster=$clusterA guest-ingested-on-B=$guestB"
  if [ "$movedA" -lt 1 ] || [ "$teleA" -lt 1 ] || [ "$walkdirs" -lt 3 ] \
     || [ "$clusterA" -lt 1 ] || [ "$guestB" -lt 1 ]; then
      echo "REMOTE GL WALK: FAIL (see $CA)"; echo "REMOTE GL CLIENT: FAIL"; exit 1
  fi
  echo "REMOTE GL WALK: OK (moved + no teleport + $walkdirs/5 directions + guest on machine B)"
  echo "REMOTE GL RENDER: OK (foreign-authority gobs render - CLUSTER VERDICT)"
  ;;
legB)
  echo "== leg B: the same character re-enters through 127.0.0.2:1873/1874 =="
  run_client "$CB" -Dhaven.defserv=127.0.0.2 \
    -Dhaven.authport=1873 -Dhaven.gameport=1874 -Dhaven.drivequick=true
  movedB=$(rg -c "^MOVEMENT: MOVED" "$CB" 2>/dev/null || echo 0)
  mig_tx=$(rg -c "char query: serving snapshot to peer" "$LOG0" 2>/dev/null || echo 0)
  mig_rx=$(rg -c "char migration received: entering world" "$LOG1" 2>/dev/null || echo 0)
  echo "leg B evidence: movement=$movedB served-by-A=$mig_tx received-by-B=$mig_rx"
  if [ "$movedB" -lt 1 ] || [ "$mig_tx" -lt 1 ] || [ "$mig_rx" -lt 1 ]; then
      echo "REMOTE GL MIGRATION: FAIL (see $CB $LOG0 $LOG1)"
      echo "REMOTE GL CLIENT: FAIL"
      exit 1
  fi
  echo "REMOTE GL MIGRATION: OK (entered through machine B's own address, walked)"
  echo "REMOTE GL CLIENT: OK"
  ;;
down)
  "$CLUSTER" stop >/dev/null 2>&1
  echo "== machines stopped =="
  ;;
all)
  boot_nodes
  "$0" legA "$USERNAME" "$TAG" || exit 1
  "$0" legB "$USERNAME" "$TAG" || exit 1
  "$CLUSTER" stop >/dev/null 2>&1
  ;;
*)
  echo "usage: $0 {up|legA|legB|down|all} [username] [logtag]" >&2
  exit 2
  ;;
esac
