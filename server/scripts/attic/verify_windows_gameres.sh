#!/usr/bin/env bash
# Windows gameres generator smoke (session 38): run the REAL
# windows/make-gameres.ps1 under PowerShell Core on Linux, with the
# backslash path literals converted to forward slashes, proving the
# script's logic end to end: extract jar -> wipe output -> copy res ->
# overlay res/compiled -> report the file count.
#
# PowerShell Core is optional in this sandbox: when pwsh is absent the
# smoke reports SKIP (the structural gates in verify_windows_launch.sh
# still cover the script shape); when present it must pass.
set -uo pipefail
cd "$(dirname "$0")/../.."   # repo root

fail() { echo "WIN GAMERES SMOKE: FAIL ($1)"; exit 1; }

PWSH="$(command -v pwsh || true)"
[ -z "$PWSH" ] && [ -x "$HOME/pwsh/pwsh" ] && PWSH="$HOME/pwsh/pwsh"
if [ -z "$PWSH" ]; then
  echo "WIN GAMERES SMOKE: SKIP (no pwsh in this environment)"
  exit 0
fi

SRC=windows/make-gameres.ps1
[ -f "$SRC" ] || fail "make-gameres.ps1 missing"
[ -f lib/haven-res.jar ] || fail "lib/haven-res.jar missing"

# Path-literal conversion for the Linux pwsh run: the script's only
# backslashes are Windows path separators inside string literals, and
# TEMP is a Windows environment variable (Linux uses TMPDIR).
SMOKE=windows/.smoke-make-gameres.ps1
sed -e 's|\\|/|g' -e 's|\$env:TEMP|"/tmp"|g' "$SRC" > "$SMOKE" || fail "sed conversion"

OUT="$("$PWSH" -NoProfile -ExecutionPolicy Bypass -File "$SMOKE" 2>&1)" \
  || { echo "$OUT" | tail -20; rm -f "$SMOKE"; fail "pwsh run"; }
rm -f "$SMOKE"

echo "$OUT" | tail -2
echo "$OUT" | grep -q "gameres ready:" || fail "no ready line"
COUNT="$(echo "$OUT" | grep -o 'gameres ready: [0-9]*' | grep -o '[0-9]*')"
[ "${COUNT:-0}" -ge 1000 ] || fail "file count too low ($COUNT)"

# Key resources the client needs on the very first screen.
for f in gameres/gfx/borka/hair.res gameres/gfx/invobjs/bow.res; do
  [ -f "$f" ] || fail "missing $f after generation"
done

# The wipe-first semantics must not leave the rev stamp behind.
[ ! -e gameres/.genrev ] || fail ".genrev survived the wipe"

echo "WIN GAMERES SMOKE: OK ($COUNT files)"
