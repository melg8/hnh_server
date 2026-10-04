#!/bin/bash
# Run the REAL Haven client (GL render path) against a freshly booted local
# server under Xvfb, drive a map click through the real widget chain, and
# print the movement verdict. Everything runs in ONE shell so background
# processes survive until the end.
# Usage: run_real_client_e2e.sh <username> [logtag] [extra-server-args]
set -u
USERNAME="${1:-driveuser}"
TAG="${2:-run}"
EXTRA="${3:-}"
REPO="${REPO:-$(cd "$(dirname "$0")/../.." && pwd)}"
TOOLS="${TOOLS:-/home/z/tools}"
J8="${J8:-$TOOLS/jogl-extract/jdk8u504-b01}"
JOGL="${JOGL:-$TOOLS/jogl-extract/jni/usr/lib/jni}"
X11="${X11:-$TOOLS/x11libs/usr/lib/x86_64-linux-gnu}"

cleanup() {
  kill ${JPID:-0} ${XVPID:-0} ${SPID:-0} 2>/dev/null
  wait 2>/dev/null
}
trap cleanup EXIT

# --- server (fresh save per run: no cross-run state) ---
cd $REPO
rm -f $REPO/save/e2e_$TAG.json
HNH_SAVE_FILE=$REPO/save/e2e_$TAG.json RUST_LOG=hnh_server=trace \
  server/target/release/hnh-server --seed 42 $EXTRA > /tmp/server_$TAG.log 2>&1 &
SPID=$!
sleep 2
if ! kill -0 $SPID 2>/dev/null; then
  echo "SERVER FAILED"; tail -5 /tmp/server_$TAG.log; exit 1
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

# --- client ---
cd $REPO/build
CP="haven.jar:../lib/jogl.jar:../lib/gluegen-rt.jar:../lib/haven-res.jar:../lib/js-14.jar:../lib/jogg.jar:../lib/jorbis.jar:../lib/antlr-3.2.jar:../lib/jnlp.jar"
DISPLAY=:99 LD_LIBRARY_PATH=$JOGL:$X11 LIBGL_ALWAYS_SOFTWARE=1 \
  $J8/bin/java -cp "$CP" \
  -Djava.library.path=$JOGL \
  -Dhaven.resdir=$REPO/gameres \
  -Dhaven.driveuser=$USERNAME \
  -javaagent:$REPO/scripts/jogl/agent/driveagent.jar \
  haven.MainFrame > /tmp/client_$TAG.log 2>&1 &
JPID=$!

# --- wait for a verdict ---
for i in $(seq 1 120); do
  if rg -q "MOVEMENT2|AGENT ERROR|no mapview|no player gob|no UI instance" /tmp/client_$TAG.log 2>/dev/null; then
    break
  fi
  sleep 1
done
sleep 3
echo "=== agent + client errors ==="
rg "AGENT|MOVEMENT|Exception|error" /tmp/client_$TAG.log | rg -v "meat|wood" | head -30
exit 0
