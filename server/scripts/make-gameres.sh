#!/bin/bash
# Linux mirror of windows/make-gameres.ps1: generate the gameres resource
# pack for the server's HTTP resource port.
# Overlay order: lib/haven-res.jar first, then res/compiled on top.
# Idempotent; safe to re-run after a sandbox reset.
set -eu

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
JAR="$REPO/lib/haven-res.jar"
OUT="$REPO/gameres"
TMP="$(mktemp -d /tmp/hnh-gameres-XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

if [ ! -f "$JAR" ]; then
  echo "make-gameres: haven-res.jar not found at $JAR" >&2
  exit 1
fi

echo "Extracting $(basename "$JAR") ..."
mkdir -p "$TMP/res"
unzip -qo "$JAR" -d "$TMP/res"

rm -rf "$OUT"
mkdir -p "$OUT"
cp -a "$TMP/res/res/." "$OUT/"

if [ -d "$REPO/res/compiled" ]; then
  echo "Overlaying res/compiled ..."
  cp -a "$REPO/res/compiled/." "$OUT/"
fi

COUNT=$(find "$OUT" -type f | wc -l)
echo "gameres ready: $COUNT files in $OUT"
