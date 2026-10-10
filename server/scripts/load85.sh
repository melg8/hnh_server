#!/usr/bin/env bash
# Session 85: A/B the vis delta-scan (gap #10b) at the 1000-bot load
# scale. Boots the release binary with in-process saturated walking
# bots, holds the steady state, then summarizes the tick budget, the
# tail attribution (as in load82.sh) and the VISIBILITY phase: scan
# time, per-tick scan mix (full / cached / delta) and the gob-scan
# volume. Run once per side of the A/B:
#   TAG=s85     ./scripts/load85.sh   # with the delta-scan
#   TAG=s85base ./scripts/load85.sh   # baseline (git stash the tree)
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

BIN=target/release/hnh-server
TAG=${TAG:-s85}
SAVE=/tmp/hnh_${TAG}.json
LOG=/tmp/perf_${TAG}.log
DUR=${DUR:-90}

stop() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; [ -n "$PID" ] && wait "$PID" 2>/dev/null; PID=""; }
trap 'stop' EXIT

rm -f "$SAVE" "$LOG"
HNH_SAVE_FILE=$SAVE RUST_LOG=hnh_server=info \
  $BIN --seed 42 --bots 1000 --saturated --perf > "$LOG" 2>&1 &
PID=$!

ok_boot=0
for _ in $(seq 1 120); do
  if rg -q "sessions=" "$LOG" 2>/dev/null; then ok_boot=1; break; fi
  sleep 1
done
[ "$ok_boot" = 1 ] || { tail -5 "$LOG"; echo "LOAD85 FAIL: server boot"; exit 1; }

# steady state: skip the entry burst, judge the walking plateau
sleep "$DUR"
stop

python3 - "$LOG" <<'PY'
import re, sys
rows = []
for line in open(sys.argv[1], errors="replace"):
    if "perf players=" not in line:
        continue
    d = dict(re.findall(r"(\w+)=([\d.]+)", line))
    rows.append({k: int(float(v)) for k, v in d.items()})
if not rows:
    print("NO PERF ROWS"); sys.exit(1)
ss = rows[30:] or rows   # skip the ramp
def stat(k):
    v = [r.get(k, 0) for r in ss]
    v.sort()
    n = len(v)
    return (v[0] + v[n // 2] + v[-1]) / 3, v[n // 2], v[-1]
print("rows=%d (steady=%d)" % (len(rows), len(ss)))
for k in ("tick_us", "wmax_tick_us", "mean_tick_us",
          "tail_us", "retx_retire_us", "startbat_fanout_us",
          "retx_sweep_us", "sweeps_us", "retx_busy_sessions",
          "retx_retired_gobs", "phase_vis_us", "phase_mv_us",
          "vis_scan_us", "vis_gob_scans", "vis_cells"):
    mean, p50, mx = stat(k)
    print("  %-22s mean=%8.1f p50=%8.1f max=%10.1f" % (k, mean, p50, mx))
# Cumulative scan-mix counters (monotonic): report the per-steady-tick
# average from the delta over the steady window, plus the totals.
def delta_of(k):
    if len(ss) < 2:
        return 0
    a, b = ss[0].get(k, 0), ss[-1].get(k, 0)
    return b - a
n = max(1, len(ss) - 1)
full = delta_of("vis_skipped"); cached = delta_of("vis_cached")
delt = delta_of("vis_delta")
tot = full + cached + delt
print("scan mix (steady window, per tick avg):")
print("  full=%7.1f  cached=%7.1f  delta=%7.1f  (issued/tick=%.1f)"
      % (full / n, cached / n, delt / n, tot / n))
# attribution check: the four counters should sum to the tail
worst = max(ss, key=lambda r: r.get("tail_us", 0))
s = (worst.get("retx_sweep_us", 0) + worst.get("startbat_fanout_us", 0)
     + worst.get("retx_retire_us", 0) + worst.get("sweeps_us", 0))
print("worst tail tick: tail=%d owned=%d (%.0f%%)" %
      (worst.get("tail_us", 0), s,
       100.0 * s / max(1, worst.get("tail_us", 1))))
PY
