#!/bin/bash
# Run the REAL Haven client (GL render path) against node 0 of a TWO-NODE
# cluster under Xvfb, drive long walk legs across VisIndex cell boundaries
# (the rendezvous partition scatters cell owners), and print the cluster
# verdict. Node 1 hosts in-process load bots so node 0's client has
# foreign-authority gobs (guests) to see.
# Usage: run-cluster-e2e.sh <username> [logtag]
set -u
USERNAME="${1:-clusteruser}"
TAG="${2:-cluster}"
REPO="${REPO:-$(cd "$(dirname "$0")/../.." && pwd)}"
TOOLS="${TOOLS:-/home/z/tools}"
J8="${J8:-$TOOLS/jogl-extract/jdk8u504-b01}"
JOGL="${JOGL:-$TOOLS/jogl-extract/jni/usr/lib/jni}"
X11="${X11:-$TOOLS/x11libs/usr/lib/x86_64-linux-gnu}"
NODES="127.0.0.1:7300,127.0.0.1:7301"

cleanup() {
  kill ${N1PID:-0} ${JPID:-0} ${XVPID:-0} ${SPID:-0} 2>/dev/null
  wait 2>/dev/null
}
trap cleanup EXIT

# --- node 0 (the client's home node; client-facing ports) ---
cd $REPO
rm -f $REPO/save/e2e_${TAG}_n0.json
HNH_SAVE_FILE=$REPO/save/e2e_${TAG}_n0.json RUST_LOG=hnh_server=debug \
  server/target/release/hnh-server --seed 42 \
  --cluster "$NODES" --node 0 > /tmp/server_${TAG}_n0.log 2>&1 &
SPID=$!
# --- node 1 (owns the other half of the cells; hosts the bots) ---
rm -f $REPO/save/e2e_${TAG}_n1.json
HNH_SAVE_FILE=$REPO/save/e2e_${TAG}_n1.json RUST_LOG=hnh_server=debug \
  server/target/release/hnh-server --seed 42 --bots 2 --bot-secs 600 \
  --cluster "$NODES" --node 1 --game-port 1872 --auth-port 1873 --res-port 1874 \
  > /tmp/server_${TAG}_n1.log 2>&1 &
N1PID=$!
sleep 3
if ! kill -0 $SPID 2>/dev/null; then
  echo "NODE0 FAILED"; tail -5 /tmp/server_${TAG}_n0.log; exit 1
fi
if ! kill -0 $N1PID 2>/dev/null; then
  echo "NODE1 FAILED"; tail -5 /tmp/server_${TAG}_n1.log; exit 1
fi

# --- display ---
pkill -f "Xvfb :99" 2>/dev/null
pkill -f "haven.MainFrame" 2>/dev/null
sleep 1
rm -f /tmp/.X99-lock
rm -rf /tmp/.X11-unix/X99
Xvfb :99 -screen 0 1024x768x24 -nolisten tcp > /tmp/xvfb_$TAG.log 2>&1 &
XVPID=$!
sleep 2
if ! kill -0 $XVPID 2>/dev/null; then
  echo "XVFB FAILED"; cat /tmp/xvfb_$TAG.log; exit 1
fi

# --- client against node 0 ---
cd $REPO/build
CP="haven.jar:../lib/jogl.jar:../lib/gluegen-rt.jar:../lib/haven-res.jar:../lib/js-14.jar:../lib/jogg.jar:../lib/jorbis.jar:../lib/antlr-3.2.jar:../lib/jnlp.jar"
DISPLAY=:99 LD_LIBRARY_PATH=$JOGL:$X11 LIBGL_ALWAYS_SOFTWARE=1 \
  $J8/bin/java -Dhaven.avadebug=1 -cp "$CP" \
  -Djava.library.path=$JOGL \
  -Dhaven.resdir=$REPO/gameres \
  -Dhaven.driveuser=$USERNAME \
  -javaagent:$REPO/scripts/jogl/agent/driveagent.jar \
  haven.MainFrame > /tmp/client_$TAG.log 2>&1 &
JPID=$!

# --- wait for the cluster verdict (the last agent phase) ---
for i in $(seq 1 480); do
  if rg -q "RELAYFIGHT VERDICT|AGENT ERROR|no mapview|no player gob|no UI instance" /tmp/client_$TAG.log 2>/dev/null; then
    break
  fi
  sleep 1
done
sleep 3
echo "runner done; see /tmp/client_$TAG.log /tmp/server_${TAG}_n0.log /tmp/server_${TAG}_n1.log"
