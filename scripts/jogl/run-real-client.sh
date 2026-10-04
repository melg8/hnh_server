#!/bin/bash
# Run the REAL Haven client (GL render path) against the local server under
# Xvfb with a driving agent, and print the movement verdict.
# Usage: run_real_client.sh <username> [logtag]
set -u
USERNAME="${1:-driveuser}"
TAG="${2:-run}"
REPO="${REPO:-$(cd "$(dirname "$0")/../.." && pwd)}"
TOOLS="${TOOLS:-/home/z/tools}"
J8="${J8:-$TOOLS/jogl-extract/jdk8u504-b01}"
JOGL="${JOGL:-$TOOLS/jogl-extract/jni/usr/lib/jni}"
X11="${X11:-$TOOLS/x11libs/usr/lib/x86_64-linux-gnu}"

# Fresh display in this shell only (background procs die at call end).
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

cd $REPO/build
CP="haven.jar:../lib/jogl.jar:../lib/gluegen-rt.jar:../lib/haven-res.jar:../lib/js-14.jar:../lib/jogg.jar:../lib/jorbis.jar:../lib/antlr-3.2.jar:../lib/jnlp.jar"
DISPLAY=:99 LD_LIBRARY_PATH=$JOGL:$X11 LIBGL_ALWAYS_SOFTWARE=1 \
  $J8/bin/java -cp "$CP" \
  -Djava.library.path=$JOGL \
  -Dhaven.resdir=$REPO/gameres \
  -Dhaven.resdir2=$REPO/build \
  -Dhaven.autoplay=$USERNAME \
  -javaagent:$REPO/scripts/jogl/agent/driveagent.jar \
  haven.MainFrame > /tmp/client_$TAG.log 2>&1 &
JPID=$!

# Wait up to 90 s for a verdict, then stop both.
for i in $(seq 1 90); do
  if rg -q "MOVEMENT2|MOVEMENT:|AGENT ERROR|no mapview|no player gob" /tmp/client_$TAG.log 2>/dev/null; then
    break
  fi
  sleep 1
done
echo "=== client log (agent + errors) ==="
rg "AGENT|MOVEMENT|Error|Exception|error" /tmp/client_$TAG.log | head -40
echo "=== last lines ==="
tail -5 /tmp/client_$TAG.log
kill $JPID $XVPID 2>/dev/null
wait 2>/dev/null
exit 0
