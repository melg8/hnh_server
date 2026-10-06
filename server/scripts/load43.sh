#!/usr/bin/env bash
# Session 43: measure the combat-phase sub-attribution at the 1000-bot
# load scale (duel cohort). Boots the release binary with in-process
# saturated bots, waits for the steady state, then prints the combat
# sub-phase histogram from the perf log.
set -u
cd "$(dirname "$0")/.."   # server/
export PATH="$HOME/.cargo/bin:$PATH"

BIN=target/release/hnh-server
TAG=${TAG:-s43}
SAVE=/tmp/hnh_${TAG}.json
LOG=/tmp/server_${TAG}.log
DUR=${DUR:-90}

fail() { echo "LOAD43 FAIL: $1"; stop; exit 1; }
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
[ "$ok_boot" = 1 ] || { tail -5 "$LOG"; fail "server boot"; }

# Wait until the cohort settles (~12 sessions/s login rate).
SESS=0
for _ in $(seq 1 30); do
  sleep 5
  S=$(rg -o "sessions=([0-9]+)" -r '$1' "$LOG" 2>/dev/null | tail -1)
  [ -n "$S" ] || continue
  if [ "$S" = "$SESS" ]; then break; fi
  if [ "$S" -ge 980 ]; then SESS=$S; break; fi
  SESS=$S
done
echo "cohort settled: sessions=$SESS; measuring ${DUR}s steady state"
sleep "$DUR"

# Combat sub-phase histogram (p95/mean over the whole window).
python3 - "$LOG" <<'EOF'
import re, sys

log = sys.argv[1]
keys = [
    "tick", "combat", "combat_index", "combat_players", "combat_animals",
    "combat_relay", "combat_chase", "combat_hit", "vis", "mv", "guests",
]
samples = {k: [] for k in keys}
sess = 0
pat = re.compile(
    r"tick_us=(\d+).*?sessions=(\d+).*?"
    r"phase_mv_us=(\d+).*?phase_combat_us=(\d+).*?phase_vis_us=(\d+).*?"
    r"phase_guests_us=(\d+).*?"
    r"combat_index_us=(\d+).*?combat_players_us=(\d+).*?"
    r"combat_animals_us=(\d+).*?combat_relay_us=(\d+).*?"
    r"combat_chase_n=(\d+).*?combat_chase_us=(\d+).*?"
    r"combat_swing_n=(\d+).*?combat_hit_n=(\d+).*?combat_hit_us=(\d+)"
)
with open(log) as f:
    for line in f:
        m = pat.search(line)
        if not m:
            continue
        (tick, s, mv, combat, vis, guests, ix, pl, an, re_,
         cn, cu, sn, hn, hu) = m.groups()
        samples["tick"].append(int(tick))
        samples["mv"].append(int(mv))
        samples["combat"].append(int(combat))
        samples["vis"].append(int(vis))
        samples["guests"].append(int(guests))
        samples["combat_index"].append(int(ix))
        samples["combat_players"].append(int(pl))
        samples["combat_animals"].append(int(an))
        samples["combat_relay"].append(int(re_))
        # Per-event microseconds (mean over the tick's event count).
        samples["combat_chase"].append(int(cu))
        samples["combat_hit"].append(int(hu))
        sess = int(s)
        # Stash event counts for the report.
        samples.setdefault("chase_n", []).append(int(cn))
        samples.setdefault("swing_n", []).append(int(sn))
        samples.setdefault("hit_n", []).append(int(hn))

print(f"sessions={sess} samples={len(samples['tick'])}")
if len(samples["tick"]) < 50:
    print("not enough perf lines")
    sys.exit(1)

def pctl(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * p))]

print(f"{'phase':>16} {'mean':>8} {'p50':>8} {'p95':>8} {'max':>8}")
for k in keys:
    xs = samples[k]
    if not xs:
        continue
    mean = sum(xs) // len(xs)
    print(f"{k:>16} {mean:>8} {pctl(xs,0.5):>8} {pctl(xs,0.95):>8} {max(xs):>8}")

n = len(samples["chase_n"])
for label, cnt, us in [
    ("chase starts", samples["chase_n"], samples["combat_chase"]),
    ("pvp swings", samples["swing_n"], None),
    ("landed hits", samples["hit_n"], samples["combat_hit"]),
]:
    mc = sum(cnt) / n
    mu = sum(us) / n if us else 0.0
    print(f"{label:>16}: mean/tick {mc:8.1f}  us/tick {mu:8.0f}")
EOF

echo "LOAD43: MEASURED"
