#!/usr/bin/env bash
# Session 16 leaf-3 gates: HANDOFF.md stays free of auto-generated
# "Session end" boilerplate and the server has no shutdown appender.
# Usage: verify_handoff.sh file|source
set -uo pipefail
cd "$(dirname "$0")/../.."

fail() { echo "FAIL: $1"; exit 1; }

case "${1:-all}" in
  file)
    # 1. Zero boilerplate blocks in HANDOFF.md.
    if grep -Eq '^## Session end [0-9]+$' HANDOFF.md; then
      fail "boilerplate session-end block still present"
    fi
    if grep -q 'Server exited cleanly (seed' HANDOFF.md; then
      fail "boilerplate session-end body still present"
    fi
    # 2. Real session history must survive the cleanup: both the early
    #    "### Session N" entries and the later dated/annotated entries.
    c=$(grep -cE '^(### Session |## Session end [0-9]+ \(|## 20[0-9]{2}-)' HANDOFF.md)
    [ "$c" -ge 5 ] || fail "session history lost (only $c entries)"
    # 3. The protocol note must forbid server-side writes (reflow-safe).
    tr '\n' ' ' < HANDOFF.md | grep -q 'MUST NEVER write to this file' \
      || fail "protocol note missing"
    echo "HANDOFF FILE: OK"
    ;;
  source)
    [ ! -e server/crates/hnh-server/src/handoff.rs ] \
      || fail "handoff.rs still exists"
    if grep -rn "Session end" server/ --include="*.rs" >/dev/null 2>&1; then
      fail "a source file still writes Session end blocks"
    fi
    if grep -rn "handoff" server/crates/hnh-server/src/main.rs >/dev/null 2>&1; then
      fail "main.rs still references the handoff appender"
    fi
    echo "HANDOFF SOURCE: OK"
    ;;
  *)
    fail "unknown gate: $1"
    ;;
esac
