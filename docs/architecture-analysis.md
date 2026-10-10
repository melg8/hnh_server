# Architecture Analysis — the 1k → 10k scaling path

Session 95 (type 1, architecture analysis). This document maps the server
architecture as it stands, measures where the capacity actually goes, and
ranks the improvement opportunities on the road from 1k to 10k concurrent
players. Every number below was measured on this repository's tree at
commit `6bdb5c7` (session 94), on the 2-worker sandbox, with the release
binary — nothing is assumed that was not run. Re-run any measurement with
the commands in the "Verification" section.

## 1. The architecture as-is

### 1.1 Crates and ownership

| Crate | Owns | Why it is separate |
| --- | --- | --- |
| `hnh-proto` | Wire framing, per-session reliability (RelSender/RelReceiver), auth frame codec, protocol constants | The byte-level contract shared by client and server; no state |
| `hnh-world` | Terrain, seed-deterministic grid generation, tile model | Pure function of `--seed` — nothing mutable, safe to share |
| `hnh-server` | Simulation, networking, auth, persistence, load bots | The mutable world owner |

The split matches the dependency direction: proto has no deps on world or
server; world depends on nothing of the two; server depends on both. The
seed-determinism of `hnh-world` is what makes persistence cheap: only
mutable player-owned state is saved (see §1.6).

### 1.2 Task topology (single process, default mode)

```
                    cmd_tx (unbounded)      net_tx (unbounded)
auth task ──┐   ┌──────────────────────►  Game task ──────────────► net tasks
(TLS, 1871) │   │  (single owner of World,  (10 Hz fixed tick)      (N UDP shards,
res http ──┤    │   9-phase tick, rayon                              SO_REUSEPORT,
(1872)     │    │   fan-out when workers>1)                          1870)
bots task ─┘    │
cluster mesh ──┘ (TCP, only with --cluster; inbound frames
                 forward into the game command channel)
health watchdog (probes auth + res every 10 s)
```

Everything mutable flows through ONE game task — a single-writer
discipline: no locks on world state, no data races by construction.
Networking tasks never touch the SoA tables; they exchange encoded
`RMSG` payloads through per-session queues. The command channels are
unbounded (back-pressure-free by design; the 1 MiB frame caps and the
session timeouts bound the worst case).

### 1.3 The tick pipeline (game.rs, 9 attributed phases)

Fixed 10 Hz (`TICK_MS = 100`). Each phase is individually timed every
tick (`state/perf.rs`), so regressions localize to a phase, not just the
total:

| # | Phase | What it does | Scaling driver |
| --- | --- | --- | --- |
| 0 | `tick_movement` | LinMove progress, collision blocks, pose | movers (not sessions) |
| 1 | `tick_animals` | AI intents (IDLE/WANDER/CHASE/FLEE...) | animals + rayon partitions |
| 2 | `tick_combat` | swings, hits, duels, knockouts | fighting pairs |
| 3 | `tick_vitals` | FEP/food/HP decay per player | sessions |
| 4 | `update_visibility` | per-session scan + stream deltas | sessions × view population |
| 5 | `tick_farming` | crop growth, tilth decay | live crop set |
| 6 | `tick_stations` | production jobs | live station set |
| 7 | `tick_cluster` | guest subs/publish, migrations | cluster mode only |
| 8 | tail | startbat fan-out, retransmit sweep, finalizers | sessions |

### 1.4 The data model (data-oriented core)

`state/gobs.rs`: struct-of-arrays gob table — `pos`, `frame`, `kind`,
`hp`, `vel`, pose columns — with generational (gen, slot) ids packed into
one `i32`. All hot loops iterate columns, not per-gob structs (cache
rule `mem-`/`coll-` in AGENTS.md). Cluster mode partitions the SLOT
RANGE per node (node i owns slots `[i*per, (i+1)*per)`), which makes gob
ids globally unique across processes with zero coordination.

### 1.5 Visibility (the hot path)

Three stacked optimizations (each verified by a past session's A/B):

1. **Dirty-cell index** (`visidx.rs`, session 15): 250-subtile cells; a
   view square touches ≤ ~60 cells. Only cells touched since the last
   tick are scanned. Cell size exceeds the per-tick movement bound, so a
   gob leaving a view always dirties a boundary cell the view still
   intersects.
2. **Per-session result caching** (session 30): a session whose exact
   position is unchanged reuses its last scan's result list; touched
   records patch the list, provably identical when nothing was touched.
3. **Delta scans** (session 85): a session that moved within one view
   radius of its last scan position scans only the newly-entered cells.

A dense-mover guard caps patch work at 128 touched gobs — beyond that a
full rescan is cheaper (the 1000-bot herd case). The S93 animal stream
window (animals stream only within 2×VIEW_RADIUS of the requester)
bounds what a far herd can cost a viewer.

### 1.6 Persistence

`persist.rs`: a single JSON save of per-character snapshots
(`save/world.json`), written atomically (tmp + rename). The world itself
is seed-deterministic, so nothing else needs saving. Cluster shards use
one file per node (`cluster_n{N}.json`). Item resources are saved by
name, not process-local index, so ids stay stable across restarts.

### 1.7 The horizontal path (grid-owner cluster)

`grid_owner.rs` + `nodes.rs`: the world partitions into the same VisIndex
cell lattice; each cell's owner is picked by rendezvous hashing
(deterministic, minimal migration on scale-out, no ring state). With
`--cluster`, every node simulates ONLY the gobs in its own cells;
players are authored by their HOME node and published to foreign cell
owners; sessions see foreign-authority gobs as guests streamed over a
TCP mesh (1 MiB frame cap, Hello handshake, retrying dialer). The SAME
partition drives the in-process rayon fan-out, so the single-process
tick exercises the exact unit a separate node would own.

## 2. Measured baseline (the 1k contract, verified)

Run: `--seed 42 --bots 1000 --saturated --perf`, release binary, 2
workers (sandbox `available_parallelism`), steady state after the login
storm. Log: sessions=1000, gobs=2945 (1417 spawned statics + 1000 bots +
wildlife).

| Metric | Steady value | Budget | Headroom |
| --- | --- | --- | --- |
| `tick_us` | 11.4–13.7 ms (mean 16.3 ms incl. ramp) | 100 ms | ~6x |
| `wmax_tick_us` | 27–31 ms | 100 ms | ~3x |
| `phase_vis_us` | 6.8–7.2 ms (~55% of the tick) | — | the dominant phase |
| `phase_ai_us` | 0.41–0.44 ms | — | animals are cheap |
| `phase_mv_us` | 0.35–0.55 ms | — | movers only |
| `phase_combat_us` | ~0.06 ms | — | fighting pairs only |
| tail (`retx_sweep` + `mvbat_fanout` + `startbat_fanout`) | 6–12 ms, alternating halves | — | the #2 cost |
| Login storm | 1000 sessions in 48 s (~21/s) | — | max tick 117 ms (storm outlier) |
| `retx_pending` | ~10k unacked blocks (10/session) | — | watch at 10k |

The bots drive the REAL client path — UDP handshake, widget bootstrap,
character select, then walk / fight / harvest / pick-up loops — so the
numbers above include the full per-session encode and fan-out cost, not
a synthetic loop.

### 2.1 The fd ceiling found at 1011 sessions (a measured limit)

A 2500-bot run with the default sandbox `ulimit -n` (soft 1024) STOPPED
accepting sessions at 1011: each in-process bot owns a client-side UDP
socket in the same process as the server, so bot mode pays the fd cost
of BOTH ends. The process hit exactly 1024 fds (1017 sockets). Two
robustness findings surfaced, both real bugs-in-waiting beyond the
harness itself:

1. `persist: background save write failed error=Too many open files` —
   the periodic save FAILED with no retry; under fd pressure characters
   can silently lose progress between saves.
2. The health watchdog reported `listener is down service="auth"` — a
   false alarm: the watchdog's own probe socket could not be created.
   The listeners were alive; the diagnosis was wrong.

Raising the limit (`ulimit -n 16384`) before the run removes the
ceiling; see §4 item O1 for the fixes.

### 2.2 The 2500-bot point (G1, crowd regime — measured)

Re-run with `ulimit -n 16384`: the login ramp completed to ~2480
sessions (~6/s under load, slower than the 1k run's 21/s — the crowd
itself slows the entry scans). The verdict is the headline finding of
this session:

**The first wall on the 1k→10k road is not the dispersed steady state —
it is CROWD DENSITY.** While the bots cluster (login spawn area), the
tick runs 58–307 ms against the 100 ms budget, with two measured
drivers:

- `mvbat_fanout_us` up to 95 ms (window max): the movement fan-out is
  O(movers × viewers-in-span). In a dense crowd every mover is in
  every viewer's span: `fanout_pairs` hit 380k pairs per tick at
  ~1400 clustered bots, versus 11.5k pairs at 1000 DISPERSED bots —
  the same session count, 33x the fan-out work.
- `vis_spawn_us` up to ~14 ms/tick: entry scans while hundreds of bots
  enter per 10 s window (the `spawned` full-scan path).

As the bots disperse (fanout_pairs fell from 380k to 36k late in the
run), the tick recovers toward the dispersed cost. Real 10k gameplay
WILL sit in the crowd regime regularly (spawn points, markets, sieges,
world events), so the crowd case is a capacity case, not an edge case.

The measured implication for the ranking: sharding/parallelizing the
fan-out (O2) and bounding its per-pair cost become the top
single-process priority; the horizontal cluster splits crowds only if
a crowd happens to straddle a grid-owner boundary — rendezvous hashing
scatters cells, so a single-cell mega-crowd stays on ONE node and the
node absorbs the full O(n²) pairs. That is a real 10k risk (see O8).

## 3. Scaling analysis: what breaks at 10k, in what order

Extrapolating the phase mix (single process, same 2-worker sandbox, no
code change):

| Cost | At 1k | Linear at 10k | Verdict |
| --- | --- | --- | --- |
| `phase_vis` | ~7 ms | ~70 ms | dominates the budget — must parallelize or shard |
| tail (fanout + sweep) | 6–12 ms | 60–120 ms | the second wall; alternation halves it, not enough |
| `phase_ai` / `phase_mv` / `phase_combat` | <0.6 ms each | <6 ms each | fine |
| auth login storm | 21/s | ~8 min to seat 10k | acceptable for a world opener; not for reconnect storms |
| persist save | single JSON | serialization grows with chars | needs measurement; see gap G3 |

Three mitigating forces, all already in the tree:

1. **Vertical**: the vis phase and AI intents already fan out across
   rayon partitions grouped by grid-owner cell (`--workers N`). On a
   16-core machine the ~70 ms projection divides by the practical
   parallelism of the cell lattice (the partitions are the same units a
   cluster node would own — measured partition quality, not a guess).
   The serial tail (encode + sweep) stays serial — see O2.
2. **Horizontal**: the cluster profile splits by grid ownership. A 10k
   deployment is N nodes × ~1k sessions each; every number in §2 then
   applies per node. The guest overhead (guest_pub/guest_ingests
   counters) is the per-node tax — cluster-up.sh exists to measure it.
3. **The vis cache absorbs most scans already**: at 1k the cumulative
   mix was cached=758k / full=439k / delta=15.6k. The full scans are
   dominated by login-time entry scans; the steady-state per-tick scan
   volume is what the cache leaves behind.

**The honest 10k conclusion**: TWO measured walls stand between the
verified 1k contract and 10k, both on the 2-worker sandbox. The
dispersed steady state would exceed the 100 ms budget in the vis phase
and the tail by linear extrapolation; the CROWD regime (§2.2) already
exceeds it at ~2.5k sessions MEASURED — the fan-out's O(movers ×
viewers-in-span) is the binding constraint. The architecture's answer
is the two levers it already has — more workers (the partition fan-out)
and the grid-owner cluster — plus the fan-out work itself (O2/O8):
parallelism divides the cost, but only bounding the pair count removes
the quadratic. The 2.5x/10x measurement points at those configurations
are the gaps (G1, G2 below), not verified numbers yet.

## 4. Ranked improvement opportunities

Ranked by impact ÷ effort; each is traceable to code and to a counter.

| # | Opportunity | Impact | Effort | Evidence |
| --- | --- | --- | --- | --- |
| O2 | **Shard and bound the movement fan-out**: encode+send runs serially on the game task (`mvbat_fanout` up to 95 ms in a crowd, the measured #1 cost). Two levers: (a) route each session's fan-out through its owning UDP shard task (the shards exist); (b) bound per-pair work — cell-batched interest sets instead of per-pair sends, so a mega-crowd's O(n²) pairs degrade to O(n × cells). | Removes the measured crowd wall | Medium | §2.2: 380k pairs/tick clustered vs 11.5k dispersed at the same session count |
| O1 | **Persist retry + watchdog honesty under fd pressure**: retry the background save on transient `EMFILE` with a short backoff; the watchdog must distinguish "probe socket creation failed" (resource exhaustion) from "listener down". Also: load scripts should `ulimit -n` proactively before >1k-bot runs. | Saves character progress under pressure; kills a false-alarm class | Small | §2.1: the failed save + the false alarms, both observed |
| O3 | **The 2.5x and 10x measurement points (G1/G2)**: raise fd, run 2500 and 10000 bots at workers=2 and workers=16; record the phase mix. Then the cluster A/B at the same scale. | Turns §3's extrapolation into measurement | Medium (machine time) | §3: all verdicts are extrapolated |
| O4 | **Per-cell bucket packing in visidx**: cell buckets are `Vec<GobId>` per cell; the scan walks ids then re-reads `pos` per id (random access into the SoA). Packing cell buckets as `(id, x, y)` triples (or a per-cell SoA) makes the span prefilter sequential. | Cuts the dominant phase's inner loop cost | Medium | `scan_visible_into` (game/stream.rs): per-id `gobs.get` + `pos[slot]` random reads |
| O5 | **Save format segmentation**: single JSON file serializes the whole char table per flush; at 10k chars the flush cost and the EMFILE window both grow. Segment per char (or a small KV) so a flush is incremental. | Protects the 10k persist path | Medium | persist.rs design; the EMFILE save failure |
| O6 | **Auth sharding for login storms**: one TLS listener measured at 21 entries/s. A 10k world-opener is fine (~8 min); a mass-reconnect event is not. SO_REUSEPORT auth shards mirror the game UDP shards. | Only matters at reconnect storms | Medium | §2 login storm rate |
| O7 | **Memory measurement harness**: no RSS numbers exist for the 1k/10k profiles. Add a periodic `rss_kb` counter (or /proc poll in the load scripts) so capacity planning includes memory. | Visibility into the last unplanned axis | Small | no data — flagged as gap G4 |
| O8 | **Crowd-aware grid ownership**: rendezvous hashing scatters cells, so a single-cell mega-crowd lands entirely on one node and that node absorbs the full O(n²) fan-out (§2.2). Options: cell splitting under load (a hot cell partitions into sub-cells with their own owners), or a load-aware ownership override with migration. | The cluster's answer to the crowd wall | Large | §2.2 fan-out regime; grid_owner.rs owner_of is density-blind |

Not opportunities (checked and sound): the single-writer game task (no
lock contention by construction); the unbounded command channels (bounded
in practice by session timeouts + frame caps); SoA layout + generational
ids (cache-friendly, verified by past A/B sessions); rendezvous grid
ownership (deterministic, minimal migration); the 1 MiB mesh frame cap
(a garbage peer cannot force allocations).

## 5. Gaps (what this analysis could NOT verify)

- **G1**: the 2500-bot point is measured in the CROWD/login regime
  (§2.2); the DISPERSED steady state at 2.5k sessions was not captured
  (the run's 420 s window ended while bots were still spreading) —
  re-run with a ~10 min hold for that point.
- **G2**: no 10k-bot point at any worker count; §3's steady-state
  verdicts are extrapolations from the 1k mix, and the crowd-regime
  wall (§2.2) is the measured binding constraint.
- **G3**: no measured save-flush latency at >1k characters (the flush
  cadence design is sound; the cost at 10k chars is unmeasured).
- **G4**: no memory (RSS) numbers at any scale.
- **G5**: the cluster guest overhead at 1k+ sessions per node is
  designed-for and counter-exposed, but not re-measured since session 86.

## 6. Verification (how to re-run every number)

```bash
cd server
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release

# The 1k baseline (§2):
ulimit -n 16384   # bots pay both ends' fd cost in-process
HNH_SAVE_FILE=/tmp/hnh_s95.json \
  ./target/release/hnh-server --seed 42 --bots 1000 --saturated --perf \
  2>&1 | tee /tmp/perf_1k.log
# steady state: tick_us 11-14 ms, phase_vis ~7 ms, sessions=1000

# The fd ceiling (§2.1) — reproduce with the DEFAULT limit:
ulimit -n 1024
HNH_SAVE_FILE=/tmp/hnh_fd.json \
  ./target/release/hnh-server --seed 42 --bots 2500 --saturated --perf
# expected: sessions stall at ~1011, "Too many open files" on the save

# The crowd-regime 2.5x point (§2.2):
ulimit -n 16384
HNH_SAVE_FILE=/tmp/hnh_crowd.json \
  timeout 420 ./target/release/hnh-server --seed 42 --bots 2500 --saturated --perf
# expected: tick 58-307 ms while clustered, mvbat_fanout_us up to 95 ms,
# fanout_pairs ~380k; recovers as the bots disperse (36k late-run)

# Phase attribution is in every perf line; the harnesses:
#   scripts/load85.sh  — the A/B skeleton (1k bots)
#   scripts/cluster-up.sh — the N-node horizontal profile
```

The numbers in §2 were taken on 2026-10-10 with the tree at `6bdb5c7`
(session 94: the state module split), 2 workers, Linux 6.x sandbox.
