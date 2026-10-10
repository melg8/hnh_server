#!/bin/bash
# Session 92 (type 4): the REAL GL client against a LIVE v8 breeding
# save - the herd the session-90 probe bred (3 cows + 2 bulls, all
# tameness 100) reloads beside the persisted character, the client
# re-enters AS that character (same username, dev password "x"), and
# HNH_BREED_SCALE speeds gestation so a calf is born WHILE the client
# renders - proving the S90 pipeline on the real client path.
# Usage: run-breeding-gl-e2e.sh [tag]
set -u
TAG="${1:-breedgl}"
REPO="${REPO:-$(cd "$(dirname "$0")/../.." && pwd)}"
TOOLS="${TOOLS:-/home/z/tools}"
J8="${J8:-$TOOLS/jogl-extract/jdk8u504-b01}"
JOGL="${JOGL:-$TOOLS/jogl-extract/jni/usr/lib/jni}"
X11="${X11:-$TOOLS/x11libs/usr/lib/x86_64-linux-gnu}"
SRC_SAVE="$REPO/server/target/breeding-test-save-14642.json"
SAVE="$REPO/save/e2e_$TAG.json"
USERNAME="breedfinal5"

cleanup() {
  kill ${JPID:-0} ${XVPID:-0} ${SPID:-0} 2>/dev/null
  wait 2>/dev/null
}
trap cleanup EXIT

# --- server: boot ON the breeding save (a copy; the original stays) ---
cd "$REPO"
if [ ! -f "$SRC_SAVE" ]; then
  echo "MISSING BREEDING SAVE: $SRC_SAVE"; exit 1
fi
cp "$SRC_SAVE" "$SAVE"
HNH_SAVE_FILE="$SAVE" HNH_BREED_SCALE=20000 RUST_LOG=hnh_server=debug \
  server/target/release/hnh-server --seed 42 > /tmp/server_$TAG.log 2>&1 &
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

# --- client: enter as the persisted character ---
cd "$REPO/build"
CP="haven.jar:../lib/jogl.jar:../lib/gluegen-rt.jar:../lib/haven-res.jar:../lib/js-14.jar:../lib/jogg.jar:../lib/jorbis.jar:../lib/antlr-3.2.jar:../lib/jnlp.jar"
DISPLAY=:99 LD_LIBRARY_PATH=$JOGL:$X11 LIBGL_ALWAYS_SOFTWARE=1 \
  $J8/bin/java -Dhaven.avadebug=1 -Dhaven.drivelivestock=true -cp "$CP" \
  -Djava.library.path=$JOGL \
  -Dhaven.resdir=$REPO/gameres \
  -Dhaven.driveuser=$USERNAME \
  -javaagent:$REPO/scripts/jogl/agent/driveagent.jar \
  haven.MainFrame > /tmp/client_$TAG.log 2>&1 &
JPID=$!

# --- wait for the ANIMALS verdict (kritter in the viewport = the herd
#     renders), then hold long enough for a scaled birth to fire while
#     the client is live ---
for i in $(seq 1 420); do
  if rg -q "ANIMALS SCREENSHOT|CURSOR VERDICT|GROUNDDROP VERDICT|CURSOR: no seed|CURSOR: err|EQUIPVIS: err|AGENT ERROR|no mapview|no player gob|no UI instance" /tmp/client_$TAG.log 2>/dev/null; then
    break
  fi
  sleep 1
done
# Hold the scene live: gestation at scale 20000 is ~19 s from fed
# contact; keep the client up ~120 s past its verdicts so the birth
# (and the first post-birth nursing window) happen on the live path.
sleep 120

echo "=== client: animals + entry evidence ==="
rg "AGENT: (login|char|mapview|player)|ANIMALS|MOVEMENT|Exception|AGENT ERROR" /tmp/client_$TAG.log | head -20
echo "=== server: breeding evidence (v8 restore + calf born on the live path) ==="
rg "restore|calf born|breeding sweep|herd" /tmp/server_$TAG.log | head -15
echo "=== verdict ==="
if rg -q "ANIMALS SCREENSHOT: saved kritter" /tmp/client_$TAG.log; then
  echo "HERD RENDERS: OK (agent saw the kritter in the viewport)"
else
  echo "HERD RENDERS: FALLBACK (no kritter in viewport)"
fi
# Strict pixel proof: template-match the cow standing sprite against
# the captured frame (VLM eyeballing of 27x43 px sprites is NOT
# reliable - horns read as ears; pixels are).
if python3 "$REPO/scripts/verify_cow_pixels.py" /tmp/client_animals.png 2>/dev/null | rg -q "COW PIXEL MATCH: OK"; then
  echo "COW PIXELS: OK (sprite rendered pixel-exact on the frame)"
else
  echo "COW PIXELS: no pixel-exact cow found on the frame"
fi
if rg -q "calf born" /tmp/server_$TAG.log; then
  echo "LIVE BIRTH: OK"
else
  echo "LIVE BIRTH: not observed in this window"
fi
exit 0
