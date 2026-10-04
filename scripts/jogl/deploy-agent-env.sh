#!/bin/bash
# Deploy the REAL Haven client environment for an AI-agent sandbox
# (Debian/Ubuntu class, no sudo): Temurin JDK 8, JOGL 1.1.1 natives,
# X11 client libraries, Apache Ant. Idempotent: skips what already exists.
#
# The REAL client (GL render path) is the only trustworthy end-to-end
# verification of login, world entry, rendering, clicks and movement.
# AGENTS.md makes running it mandatory before claiming a client-visible
# fix; this script provisions everything that needs internet access.
#
# Usage: deploy-agent-env.sh [tools-dir]   (default: /home/z/tools)
set -u
TOOLS="${1:-/home/z/tools}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$TOOLS"
cd "$TOOLS"

fail() { echo "DEPLOY FAILED: $*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

J8="$TOOLS/jogl-extract/jdk8u504-b01"
JOGL="$TOOLS/jogl-extract/jni/usr/lib/jni"
X11="$TOOLS/x11libs/usr/lib/x86_64-linux-gnu"

# --- Temurin JDK 8 (JDK 21's XRender GC breaks JOGL 1.1 visual selection) ---
if [ ! -x "$J8/bin/java" ]; then
  echo "== downloading Temurin JDK 8"
  curl -sL -o temurin8.tar.gz \
    "https://api.adoptium.net/v3/binary/latest/8/ga/linux/x64/jdk/hotspot/normal/eclipse" ||
    fail "jdk8 download"
  mkdir -p jogl-extract
  tar xzf temurin8.tar.gz -C jogl-extract || fail "jdk8 extract"
fi
[ -x "$J8/bin/java" ] || fail "jdk8 not found at $J8"
"$J8/bin/java" -version 2>&1 | head -1

# --- JOGL 1.1.1 natives (old-releases.ubuntu.com, amd64) ---
if [ ! -f "$JOGL/libjogl.so" ]; then
  echo "== downloading JOGL 1.1.1 natives"
  cd "$TOOLS"
  for f in libjogl-java_1.1.1+dak1-13_all.deb libjogl-jni_1.1.1+dak1-13_amd64.deb; do
    curl -sL -O "https://old-releases.ubuntu.com/ubuntu/pool/universe/libj/libjogl-java/$f" ||
      fail "jogl download $f"
  done
  mkdir -p jogl-extract/jni
  dpkg-deb -x libjogl-jni_1.1.1+dak1-13_amd64.deb jogl-extract/jni ||
    fail "jogl-jni extract"
fi
[ -f "$JOGL/libjogl.so" ] || fail "libjogl.so not found"
echo "JOGL natives: $JOGL"

# --- X11 libs the AWT JNI needs (libXtst, libXi) ---
if [ ! -f "$X11/libXtst.so.6" ]; then
  echo "== downloading X11 client libraries"
  mkdir -p "$TOOLS/x11libs"
  cd "$TOOLS/x11libs"
  curl -sL -O "https://archive.ubuntu.com/ubuntu/pool/main/libx/libxtst/libxtst6_1.2.3-1build4_amd64.deb" ||
    fail "libxtst download"
  dpkg-deb -x libxtst6_1.2.3-1build4_amd64.deb . || fail "libxtst extract"
fi
[ -f "$X11/libXtst.so.6" ] || fail "libXtst.so.6 not found"

# --- Ant (client jar build) ---
if [ ! -x "$TOOLS/apache-ant-1.10.15/bin/ant" ]; then
  echo "== downloading Apache Ant"
  cd "$TOOLS"
  curl -sL -o ant.tar.gz \
    "https://archive.apache.org/dist/ant/binaries/apache-ant-1.10.15-bin.tar.gz" ||
    fail "ant download"
  tar xzf ant.tar.gz || fail "ant extract"
fi

# --- DriveAgent javaagent (drives login + a real Robot click) ---
cd "$SCRIPT_DIR/agent"
"$J8/bin/javac" DriveAgent.java || fail "agent compile"
"$J8/bin/jar" cfm driveagent.jar manifest.txt DriveAgent.class || fail "agent jar"
echo "DriveAgent built: $(pwd)/driveagent.jar"

# --- Xvfb + Mesa must be present as system packages ---
have Xvfb || fail "Xvfb missing: install xvfb (system package)"
dpkg -l 2>/dev/null | grep -q libgl1-mesa-dri || \
  echo "WARNING: libgl1-mesa-dri (Mesa llvmpipe) not detected"

echo "DEPLOY OK"
echo "  JDK8:  $J8"
echo "  JOGL:  $JOGL"
echo "  X11:   $X11"
echo "Run the full loop: scripts/jogl/run-real-client-e2e.sh <username> <tag>"
