#!/usr/bin/env bash
# Session 17 leaf-1 gates: the Windows launchers cannot silently run a
# stale client jar or a stale gameres pack after `git pull`.
# Usage: verify_windows_launch.sh leaf1-g1|leaf1-g2
set -uo pipefail
cd "$(dirname "$0")/../.."

fail() { echo "FAIL: $1"; exit 1; }

case "${1:-all}" in
  leaf1-g1)
    RC=windows/run-client.bat
    SS=windows/start-server.bat
    [ -f "$RC" ] || fail "run-client.bat missing"
    [ -f "$SS" ] || fail "start-server.bat missing"
    # 1. The old build-once short-circuit must be gone from run-client.
    grep -q 'if not exist "build\\haven.jar" (' "$RC" \
      && fail "run-client still builds only when the jar is missing"
    # 2. The rev-stamp rebuild decision must be present.
    grep -q 'build\\.clientrev' "$RC" || fail "no client rev stamp"
    grep -q 'git rev-parse HEAD' "$RC" || fail "no HEAD rev in run-client"
    grep -q 'if defined NEEDBUILD' "$RC" || fail "no NEEDBUILD rebuild gate"
    grep -q 'call ant jar' "$RC" || fail "rebuild does not call ant jar"
    # 3. gameres regeneration is stamped the same way.
    grep -q 'gameres\\.genrev' "$SS" || fail "no gameres rev stamp"
    grep -q 'if not "%GITREV%"=="%GENREV%" set "GENRES=1"' "$SS" \
      || fail "gameres not regenerated on rev change"
    grep -q 'call "%~dp0build-server.bat"' "$SS" \
      || fail "server build step missing"
    # 4. The user docs mention the auto-rebuild behavior.
    tr '\n' ' ' < windows/README.md \
      | grep -qi 'rebuild' || fail "README does not document rebuilds"
    echo "WINLAUNCH LEAF1 G1: OK"
    ;;
  leaf1-g2)
    # Structural sanity: every goto label exists, every called script
    # exists, and no line still references a removed helper.
    for f in windows/run-client.bat windows/start-server.bat; do
      # 1. goto/call :label targets must be defined in the same file.
      for lbl in $(grep -io 'goto[ :] *[a-z0-9_-]*' "$f" | sed 's/.*[ :] *//I'); do
        grep -qi "^:$lbl" "$f" || fail "$f: goto target :$lbl undefined"
      done
      # 2. called batch scripts must exist.
      for t in $(grep -o 'call "%~dp0[0-9A-Za-z_.-]*' "$f" | sed 's/.*%~dp0//'); do
        [ -f "windows/$t" ] || fail "$f: call target windows/$t missing"
      done
      # 3. referenced powershell helpers must exist.
      for p in $(grep -o '%~dp0[0-9A-Za-z_.-]*\.ps1' "$f" | sed 's/%~dp0//'); do
        [ -f "windows/$p" ] || fail "$f: helper windows/$p missing"
      done
    done
    echo "WINLAUNCH LEAF1 G2: OK"
    ;;
  *)
    fail "unknown gate: $1"
    ;;
esac
