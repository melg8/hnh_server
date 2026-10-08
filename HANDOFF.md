# HANDOFF — hnh_server Rust implementation

This file is the durable context-transfer protocol for LLM working sessions.
Read this FIRST in every new session. Append a dated entry at the end of
every session (what was done, what was verified, what is next). Never
delete entries. The server binary and every automated tool MUST NEVER write
to this file: session entries are authored by a working session, not
appended by builds, gates, or shutdown paths.

## Archive policy (session 53)

To stop this file from growing unboundedly (it reached 204 KB / 3596
lines by session 52), only the LAST TWO session entries stay here in
full. At the end of every session, move everything older verbatim to
HANDOFF_ARCHIVE.md and keep this file as the living context only. The
archive is read-only history; nothing is ever condensed or edited when
moving, so no information is lost - grep both files when researching
how something came to be.

## How to continue work in the next session

1. `git pull` the repo; read HANDOFF.md top to bottom (consult
   HANDOFF_ARCHIVE.md only for historical detail).
2. Build and test: `cd server && cargo test` (must be green) and
   `cargo build --release`.
3. Run: `cd server && ./target/release/hnh-server --seed 42`
   (ports: 1871/tcp auth TLS, 1870/udp game, 1872/tcp resources HTTP;
   `../gameres/` must exist - it is generated from `lib/haven-res.jar`,
   see "Resource pack" below). Tick workers default to
   available_parallelism; `--workers N` overrides.
4. Verify end-to-end: `python3 server/scripts/test_client.py testuser`
   must print `WORLD ENTRY: OK`.
5. Load check: `./target/release/hnh-server --seed 42 --bots 300 --perf`
   - perf logs should show `tick_us` well below 100000. Add
   `--saturated` for the worst case (3300+ animals, live fights) and
   `--bots 1000` for the documented 1k target. Scenario probes:
   `python3 server/scripts/probe_melee.py` etc. (see server/scripts/README.md).
6. Multi-node: `--cluster <nodes> --node N` starts the grid-owner
   cluster (wire format documented in the archive, sessions 27/34).
7. Client: `ant jar` (JDK 21 + Ant 1.10), run
   `java -cp build/haven.jar:lib/* haven.MainFrame` - it connects to
   127.0.0.1 automatically (auth 1871, game 1870, resources 1872).
   `-Dhaven.autoplay=Player` skips charselect. `-Dhaven.pinnedcert`
   restores legacy certificate pinning.
   On Windows: `windows\run-client.bat` is the one-command entry (it
   auto-starts the server, waits for the ports and launches the
   client); `windows\collect-logs.bat` bundles a one-file bug report;
   `windows\README.md` documents every script.

## Architecture (as implemented)

- `server/crates/hnh-proto` - byte-exact wire protocol: `MessageBuf`
  (LE primitives, NUL strings, typed lists), MSG_*/RMSG_*/OD_* consts,
  reliability layer (`RelSender`/`RelReceiver`: 16-bit seq per direction,
  cumulative ACK, legacy backoff table 80/200/620/2000 ms, hold-back
  buffer), zlib MAPDATA assembly + MTU fragmentation. Auth frame codec.
- `server/crates/hnh-world` - `JavaRandom` (bit-exact 48-bit LCG port,
  verified against a real JDK 21), seed-fixed value-noise worldgen
  (tile_at is a pure function of seed+x+y; grids generate independently =
  grid-shard ready), `GridStore` (10 KB per 100x100 grid, LRU eviction,
  copy-on-write tile mutation).
- `server/crates/hnh-server` - binary (~31k lines, 23 top-level modules
  + the session-49 game/ feature split):
  - `auth.rs`: TLS auth server (rustls, self-signed dev cert generated
    at boot), AuthClient frame protocol, SHA-256 password digests,
    single-use cookies (5 min TTL), reusable tokens (30 d TTL). Dev
    policy: any username/password auto-provisions.
  - `net.rs`: UDP 1870, `--shards N` SO_REUSEPORT shard sockets
    (kernel 4-tuple hash pins each peer to one shard). MSG_SESS
    handshake (PVER check, cookie consume, idempotent re-accept).
    Per-session driver tasks own reliability state; two outbound
    channels: reliable RMSG stream + raw MAPDATA/OBJDATA datagrams.
    MAPREQ/OBJACK/WDGMSG in.
  - `state.rs`: SoA gob storage (pos/res/frame/alive/kind/hp/speed/mv
    columns, generational ids packed into i32), Species table with
    per-species hp/speed/loot, tile speed rules, 10 Hz tick.
  - `game.rs` + `game/` (session-49 split, extended by session 55;
    game.rs keeps the core: construction, world entry, session
    lifecycle, tick dispatcher, party/skills, interaction relays):
    `game/animals.rs` (wildlife AI, quell/taming, production+feeding
    sweep, starvation), `game/building.rs` (plans, stations, trough),
    `game/craft.rs` (make widget, recipes, roast chain),
    `game/farming.rs` (plow/mutate/plant/crop menus/harvest),
    `game/items.rs` (drops, inventory/equipment windows, drag cursor,
    food menu, eating), `game/stream.rs` (mapreq, gob block encoder,
    spawn/retract, visibility pass), `game/interact.rs` (map clicks,
    walk/interact routing, the movement tick + packed fan-out,
    start_move), `game/combat.rs` (openings fights, archery, frv
    protocol, PvP consequences + vitals), `game/cluster.rs` (node
    messages, guest mirroring/republishing, subscriptions, authority
    transfer), `game/tests.rs` (the unit battery).
  - `fight.rs`: fightview openings combat (relations, balance, IP,
    offence/defence, damage through openings). `archery.rs`: bow
    combat (Shoot action, aim meter, arrow economy). `armor.rs`:
    armor class.
  - `grid_owner.rs` + `nodes.rs`: true multi-node process split -
    node-link mesh, per-grid ownership, cross-node relays
    (interactions, farming, stations, drops, archery, PvP swings,
    maneuver IP), cluster save story (session 34).
  - `visidx.rs`: dirty-cell spatial index for the visibility scan;
    `move_batch.rs`: packed movement-block fan-out (5 Hz LINSTEP
    cadence, batched poses); `fxhash.rs`: multiply-rotate hasher for
    SERVER-INTERNAL id containers only (attacker-controlled keys keep
    SipHash).
  - `craft.rs` (fep.conf parser, recipe registry, roast map) and
    `farm.rs` (crop simulation), `build.rs` (placement pipeline +
    stations), `equip.rs` (avatar clothing layers), `chat.rs`,
    `party.rs`, `skills.rs` (LP/curio economy), `resources.rs`
    (session-local RESID tables).
  - `persist.rs`: `SaveStore` per-character snapshots in
    `../save/world.json` (save v7: characters, tamed animals, troughs;
    atomic tmp+rename writes; autosave 30 s + graceful SIGINT/SIGTERM
    flush; seed mismatch or corrupt file starts fresh).
  - `res_http.rs`: HTTP file server for `gameres/` (GET `<name>.res`),
    30 s per-request timeout, startup self-check.
  - `bots.rs`: in-process load-test bots through the REAL UDP path,
    fully async tokio tasks.

## Resource pack

`gameres/` = `lib/haven-res.jar` (6301 .res files) overlaid with
`res/compiled/` (fork resources). Regenerate with:
```bash
unzip -o -q lib/haven-res.jar 'res/*' -d /tmp/hx && cp -rn /tmp/hx/res/* gameres/ \
  && cp -r res/compiled/* gameres/
```
`gameres/` is NOT in git (repo size); it is reproducible from the repo.

## Verified (recorded evidence; details in the archive)

- Test pyramid (session 50): 282 cargo tests green - unit battery incl.
  the game/ tests, 11 hnh-proto, 9 hnh-world, and the black-box wire
  tier `server/crates/hnh-server/tests/wire.rs` (boots the REAL binary
  on ephemeral ports, speaks the real protocol: auth, bootstrap,
  REQUIRED_CATTR set, MAPDATA, movement LINBEG/LINSTEP, build flow).
  Needs no gameres, no python. fmt + clippy -D warnings clean.
  Session 64: 301 tests (11 proto + 274 unit + 7 wire [6 in the gate
  + 1 #[ignore]d] + 9 world) - the wire tier gained
  lost_static_spawn_wave_is_retransmitted (drop-window loss
  simulation with a deterministic held-objack proof).
- Python scenario probes (server/scripts/): WORLD ENTRY, CATTR ORDER,
  MOVE, DIRECTION, ANIMALS, MELEE/PVP, STATION, BUILD FLOW, FARMING,
  EQUIP, PARTY/CHAT, NEWCRAFT, GATHER on the shared hnhlib.py harness
  (test_gather.py, session 60: branch pick off a seed-42 forest tree
  + 5 stone picks draining a boulder, every drop lands in inventory).
- Load (recorded): 1000/1000 async bot sessions in a saturated world
  (3300+ animals, live fights) at tick ~37-40 ms vs the 100 ms budget
  (session 2); 300 bots/node across the cluster (session 34); 1000
  duelist window (session 40); session-52 re-baseline on current
  master: 44-48 ms mean tick with workers=auto on 2 cores;
  session-57: same wall-time band reproduced with the dense fan-out
  index (the 1k wall time is scheduler-bound on 2 cores, not index
  bound) plus per-phase sub-attribution (mv scan 22-40 us, encode
  3-14 us, fan-out the rest) and a dense-vs-full-scan micro-bench
  (324 vs 472 ns per fan-out scan, ratio before counting the HashMap
  cache misses the old walk also paid).
- Real-client e2e: scripts/jogl/ boots the real GL client under Xvfb
  (login, Robot map clicks, MOVEMENT/portrait/equipment verdicts;
  sessions 21/25/45). Windows: windows/ one-command scripts (fix log
  in the archive).

## Known gaps / next steps (consolidated)

1. **CI**: `.github/workflows/rust.yml` (fmt, clippy -D warnings, cargo
   test --workspace on push/PR) could not be pushed - the PAT lacks the
   `workflow` scope (session 50; retried in 53/55/56/57/61/63, see
   those entries). The full file content is preserved in the archive
   (session-50 addendum). Retry the push every session; a green local
   run stays mandatory.
2. **Pair-work fan-out at scale**: MEASURED (session 59). The fan-out
   is pair-bound and the multi-node path really caps it per node:
   2x300 bots against single-node 600 cut mvbat_fanout p50 12.4 ->
   3.9/7.4 ms with the same total population. At 2x500 on ONE 2-core
   box both nodes starve (mean 170-210 ms ticks) and the cluster
   carries EXTRA work the single node does not: guest mirroring
   doubles the fan-out (mvbat_fanout + guests_fanout both at
   90-130 ms) and 500 sessions see ~250 movers each because bots
   spawn clustered. Verdict: the 1k cluster target needs nodes on
   SEPARATE machines (or more cores), not a bigger single box; no
   further single-index work is justified here. Guest pose
   finalizers (the one profiled cluster excess) were already fixed
   this session (see S59).
3. **Probe migration**: CLOSED (session 61): all five legacy
   self-contained scripts (probe_animals, probe_direction,
   test_farming, test_party_chat, dump_paginae) either subclass
   WireClient now or are gone; test_build no longer re-exports the
   harness names. Three stale probe contracts the migration surfaced
   were also fixed (see S61).
4. **Recipe breadth**: MOSTLY CLOSED (session 58): 35 recipes total;
   the stone/bone tools, farm headwear, fishing gear, linen tier and
   the leather tier (via the tanhide/string fork pages) now craft.
   WORLD GATHERING CLOSED (session 60): trees yield branch picks and
   boulders yield stone picks (see the crafting-and-building.md
   "World gathering" section). Remaining dead ends: metal chain (no
   ore gathering/smelter numbers), pottery/kiln (clay items exist, no
   station), wurst/sausage and baking doughs (station cooking depth),
   flour/bread (the 2009 pack has no grain item - sprout/grist only,
   see farm.rs).
5. **Feeding depth**: LIFT CLOSED (session 62): the trough lift /
   place / transfer mechanic is implemented and probed; the carried
   store survives restarts and cross-node migration. Still open:
   per-animal breed stat rows (Milk Quantity / Wool Quality are flat
   constants), the cross-node lift/transfer relays (a peer node's
   trough offers no Lift petal to a guest), and the carried-trough
   avatar render.
6. **Real-client e2e**: GL production walkthrough (tame, wait out the
   milk meter, milk on screen) and Windows smoke when a display host
   exists (carried).
7. **Guest GC at scale**: CLOSED by measurement (session 59): the
   50-tick GC walk inside phase_cluster measured p50 73-98 us and
   p95 <= 314 us with 646-984 guests per node at 2x300 - two orders
   below anything actionable. Do not revisit without a multi-node
   profile that shows phase_cluster in the milliseconds.
8. **Gathering wire test stability**: RESOLVED server-side (session
   64), re-scoped. The session-56 "lost spawn is unrecoverable"
   finding is fixed by the OBJACK-driven retransmission sweep (see the
   S64 entry and `lost_static_spawn_wave_is_retransmitted` in the
   gate). The walking scenario STAYS #[ignore]d for a new, documented
   reason: 40-90 s of wall-clock-bound walk hops starve under two
   concurrent boots on the 2-core sandbox. Run explicitly: `cargo
   test --test wire -- --ignored`.
9. **Load bots never OBJACKed**: CLOSED (session 65). The S64 sweep's
   "near-zero in the steady state" assumption was FALSE at the 1000-bot
   scale: the cohort pinned every spawn/finalizer block until retirement,
   but queue-full refusals never burn attempts, so pending grew to 1.6M
   and the sweep hit 426 ms (see the S65 entry). The load cohort now
   echoes batched MSG_OBJACK like the real client, a hard age ceiling
   and a queue-full throttle bound any peer's table regardless.

## Session type rotation log (consolidated)

Per the alternating-goal rule (one goal per session; the user prompt
re-lists it every time). Sessions 1-44 predate the rule and were not
logged. Recorded tail: 45=3, 46=3, 47=3, 48=3, 49=2, 50=4, 51=3, 52=5,
53=0, 54=1, 55=2, 56=4, 57=5, 58=3, 59=5, 60=3, 61=2, 62=3, 63=4, 64=1, 65=5, 66=3. All six
types have been served - pick freely, but avoid serving the same type as
the previous session.

## Session index (one line each; full entries in the archive)

- S1: full Rust server per architecture; client local-dev support; 724 bots/19 ms tick.
- S2: one-command start (dev cert), SO_REUSEPORT shards, world.json persistence, fightview combat, 1k-bot load.
- S3: crafting + fep.conf + food/FEP eating, parallel tick phases, windows/*.bat, starter kit.
- Unnumbered (2026-10-04): Windows compile fix; JOGL natives fix;
  resource server hardening; repo hygiene; cwd-independent paths;
  run-client.bat one-command entry + collect-logs bug reports (+ two
  PS 5.1 fixes); client HTTP dependency removed; post-login cattr NPE +
  headless avatar; crop farming (plow/plant/grow/harvest).
- S14: parties + chat, skill/LP economy (unlazy tree).
- S15: building pipeline + oven station + dirty-cell visibility.
- S16: black screen fixed (client-side probe), equipment paperdoll, handoff de-clutter.
- S17: login portrait fixed (standing-frame layers), stale-jar guard.
- S18: real-GL-client reproduction killed the post-enter freeze.
- S19: frozen character root-caused and fixed (real-client verified).
- S20: movement fidelity + charlist portrait (real-client verified).
- S21: directional animations, visible+animated animals, bite FX, equipment doll.
- S22: one-octant walk direction offset (art ring vs movement ring).
- S23: grid-owner partitioning (the multi-node 10k unit) + follow-up: armor class end to end.
- S24: fighting/harvesting bot cohort at the 1000-session scale.
- S25: equipment visuals (clothing on the world avatar and the Equipment doll).
- S26: cursor item, ground drops, vis skip check, accept throttle.
- S27: true multi-node process split (grid-owner cluster).
- S28: cross-node interaction relay (guest fights live).
- S29: cluster save story + 1k load evidence + vis sub-phase attribution.
- S30: relay static acts, cursor merge, vis cache, sharded load story.
- S31: cross-node crop harvest + planting relay.
- S32: cross-node plowing (TileMutation), tilth decay revert.
- S33: station menus relay, owner-filtered populate.
- S34: build-transition publish, live station probe, 300/node load.
- S35: drop authority transfer, 1000-bot window, per-node certs.
- S36: bow chain (woodbow/stonearrow/bonearrow recipes, bone loot, carrying pose).
- S37: bow shooting (Shoot action) + addendum: cross-node archery (guest animals).
- S38: PvP archery end to end (local + cross-node + wire probe).
- S39: melee PvP between players (openings duel + cross-node PvpSwing relay).
- S40: melee weapons, knockout consequences, maneuver economy, 1000-duelist load.
- S41: guest-phase optimization, batched poses, 5 Hz LINSTEP (addendum: UiProbe PaginaException).
- S42: PaginaException pack bug, vis spawn-churn debounce, cross-node maneuver IP relay.
- S43: combat-phase attribution + slot/viewer indexes.
- S44: batched move starts + FX wire patch (addendum: batched pose fan-out, ViewerIndex retired).
- S45: real-client e2e green, gear-chain recipes, Windows smoke (addendum: taming MVP Quell the Beast).
- S46: taming depth (AH skill, battle intensity, species morph), cloth chain + tool plumbing.
- S47: tamed-animal persistence (save v6) + production (milk/wool), tile_overrides bugfix.
- S48: Food Trough (fodder, quality averaging, feeding preference, starvation), save v7.
- S49 (type 2): game.rs 19k-line monolith split into game/ feature modules; pure move.
- S50 (type 4): black-box wire integration layer, hnhlib.py corpus consolidation, CI prep (push blocked).
- S51 (type 3): build-flow branch-sink regression root-caused and fixed (cursor return contract).
- S52 (type 5): perf re-baseline - fxhash id hasher, allocation-free vis scan, --workers auto.
- S53 (type 0): docs hygiene - HANDOFF split (living file + verbatim archive), stale sections rebuilt, scripts attic.
- S54 (type 1): architecture review - guest scan O(node guests) per rescan removed (view-cell-bounded), idempotent VisIndex insert (promote double-insert bug), probe_guest_walk; live 2-node cluster evidence.
- S55 (type 2): game/interact.rs + game/combat.rs extracted from game.rs (pure move, 5.3k -> 3.5k lines); split-verification process note.
- S56 (type 4): wire-test de-flake - the harness now retransmits unacked reliable datagrams on the legacy RWorker backoff (lost WDGMSG click root-caused); 15/15 green full-suite runs.
- S57 (type 5): mv-phase profile first (new mvbat_* attribution), then dense sorted cell index for the fan-out, allocation-free movement encode, per-window max-tick perf field; 1k wall time confirmed scheduler-bound on 2 cores.
- S58 (type 3): recipe breadth batch - 19 recipes (35 total), static paginae scanner (scan_paginae.py), fork pages string/tanhide unlock the leather tier, test_newcraft.py wire probe.
- S59 (type 5): multi-node scaling profile (profile_multinode.sh) - pair-cap confirmed per node, guest GC cleared by measurement, guest pose finalizers moved to the packed patched batch (p95 10.9 ms -> 16 us).
- S60 (type 3): world gathering - trees yield branch picks (TREE_HARVESTS = 5, then a decorative Stump), boulders yield stone picks (BOULDER_STONES = 5, then gone); the dead 'wood x10' drop and the clickable-stump bug are gone; shared pick legs for local + relay paths; test_gather.py wire probe + crafting-and-building.md "World gathering" section.
- S61 (type 2): probe corpus consolidated on hnhlib - the five legacy self-contained scripts migrated/deleted, test_build re-exports removed; three stale probe contracts surfaced and fixed (chatbot walk-away distance, direction art-ring offset, equipbot item-wid staleness, station/drop cursor return); dead dump_paginae removed.
- S62 (type 3): Food Trough lift mechanic - lift menu / carry / place-back / fodder transfer "like a liquid"; carried store persists (save v7 field) and rides cross-node migration (CharData boxed); test_feeding.py wire probe + 5 new unit pins; livestock doc session-62 section.
- S63 (type 4): wire tier 4 -> 6 - trough lift contract IN the default gate; gathering walking scenario (#[ignore], explicit run); harness: sm menus, chat lines, click_gob/flower_choice, candidate scans, 2-slot concurrency governor, OD_REM decode fix; movement contract retargeted to the clicked point.
- S64 (type 1): the missing OBJACK retransmission half built - unacked blocks now BTreeMap<frame, UnackedBlock> + per-gob acked high-water mark (Option: frame 0 is real); in-order ~300 ms sweep resends lost spawn/retract/re-render (critical) and finalizer/hp (self-healing) blocks and retires exhausted ones; OD_REM rides max-seen-frame+1 and supersedes the gob history; probe lost_static_spawn_wave_is_retransmitted in the gate; gathering #[ignore] re-labeled (walk length, not spawn loss).
- S65 (type 5): retransmit death spiral measured and broken - the 1000-bot run showed 1.6M pending blocks and a 426 ms sweep (load bots never OBJACK + no-burn on queue-full refusals = retirement never fires); fixes: retx_* perf attribution, hard age ceiling (10 s), per-session queue-full throttle (1 s skip), expired blocks latch the in-order walk, load bots echo batched MSG_OBJACK like the real client; post-fix mean tick 31-86 ms at 1000 sessions (was 226 ms).
- S66 (type 3): metal chain groundwork - ore deposits (Copper/Tin/Iron) on the
  rocky belt, the smelter becomes a working station (StationKind dispatch, SMELT_MAP
  ore -> bars), refinement tier from the shipped bloom2wrought/shammer paginae
  (castiron -> wrought iron -> smithy's hammer); test_smelt.py wire probe, 306 green.

---
## 2026-10-08 - Session 65 (type 5: performance)

SESSION TYPE ROTATION LOG: 61=2, 62=3, 63=4, 64=1, 65=5. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the S64 handoff named the one unverified perf claim - "the
retransmit sweep is O(pending) per ~300 ms; expect near-zero in the
steady state - verify, do not assume". First step was honesty
instrumentation: the sweep had no perf attribution at all, only a
debug! line that fires on resend work.

MEASURED (1000 bots, saturated world, release build, `--perf`):

- `retx_pending` 1 616 372 - NOT near-zero. `retx_sweep_us` 426 860 -
  the sweep alone cost more than four ticks of budget.
- `retx_queue_full` 1 382 303 of ~1.6M attempts (85% refusals);
  `mean_tick_us` 226 529 - the tick budget was broken 2.2x over.
- Root cause chain: the load-bot cohort never echoed MSG_OBJACK
  (grep bots.rs: zero hits), so every spawn/finalizer block lived in
  its session's unacked table until the try-count retirement; BUT the
  S64 no-burn rule on queue-full refusals meant a saturated session's
  blocks never retired at all (positive feedback: more pending ->
  bigger walk -> more refusals -> still no retirement). The S64
  "near-zero" assumption held for acking clients only.

FIX (one change set, measured first, re-measured after):

- Perf attribution: `retx_sweep_us / retx_pending / retx_resent /
  retx_queue_full / retx_busy_sessions` per sweep in the perf report -
  the sweep's cost is now a first-class number, not an inference.
- Hard age ceiling `RETRANS_MAX_AGE_MS` (10 s from first send): every
  block retires deterministically regardless of send attempts. A
  throttled or dead session drains its table; the sweep walk is bounded
  by the recent past.
- Per-session backpressure throttle: after a raw-queue-full refusal the
  session's retransmit pass is skipped for 1 s (`retx_throttle_until`);
  retries stop firing into a saturated channel, and the walk skips the
  session wholesale (`busy_sessions` count shows the real depth).
- Expired blocks latch the gob's in-order walk (`blocked = true`), so
  no later frame of that gob escapes through the retirement hole.
- Load bots mirror the real client's SWorker: `parse_objdata` tracks
  the max decoded frame per gob and the bot echoes one batched
  MSG_OBJACK datagram every 200 ms (the wire shape the sweep is keyed
  on). The load cohort now exercises the same retransmission contract
  the wire harness does - which is what a 1k-player load test MEANS.

POST-FIX (same 1000-bot scenario):

- `retx_pending` 10 010-47 782 (~30x down); `retx_sweep_us`
  3 355-41 063 (~10-100x down); `retx_queue_full` 98-12 446 (~200x
  down, and the throttle keeps refusals from compounding).
- `mean_tick_us` 30 808-86 092 - back INSIDE the 100 ms budget
  (window max spikes 62-283 ms are the bot-entry spawn burst, already
  known and windowed by `wmax_tick_us`).
- `WORLD ENTRY: OK` python probe green against the new binary.

VERIFIED (fresh runs): fmt --check clean; clippy -D warnings clean;
cargo test --workspace 300 green (11 proto + 274 unit + 6 wire +
9 world, gathering scenario #[ignore]d as documented). The
`lost_static_spawn_wave_is_retransmitted` probe still passes - the age
ceiling (10 s) sits an order above the test's 1.2 s loss window, and
the throttle cannot fire on a single session with an empty queue.

NOT DONE (deliberate): `Arc<Vec<u8>>` for block bytes would remove the
deep clone on resend - deferred until the retx_* fields show the clone
matters (post-fix resends are ~1-20k per sweep, not 114k; verify first,
do not optimize on vibes).

NEXT (handoff):
- Type-3 candidates: metal chain groundwork (ore + smelter), flower
  pick verbs, per-animal breed stat rows.
- Type-5 candidates: mvbat_fanout_us remains the top single phase at
  1000 sessions (14-38 ms in the post-fix run) - profile the
  per-session fan-out walk; entry-burst wmax (283 ms) attribution.
- Carried: cross-node lift/transfer relays, GL e2e + Windows smoke
  (no display host), multi-machine cluster profile, CI push when the
  token gets the workflow scope.

---
## 2026-10-08 - Session 66 (type 3: new functionality)

SESSION TYPE ROTATION LOG: 62=3, 63=4, 64=1, 65=5, 66=3. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the S64/S65 handoffs named "metal chain groundwork (ore +
smelter)" as the first type-3 candidate. The smelter had stood as a
plain structure since session 15 ("until the metal chain exists"), and
no metal item could enter the economy. TWO implementation waves landed
under this session number (the second rebased on the first's push).

WAVE 1 (d91dcb0): ore deposits + a working smelter.

- ORE DEPOSITS: MOUNTAIN/CAVE tiles with a walkable 4-neighbor spawn
  gfx/terobjs/mining/heap ore deposits (3% roll), each carrying
  Copper/Tin/Iron (OreKind::from_roll, 5:3:2 per-tile mix), ORE_PICKS
  = 4 picks of the ore item (display label), ORE_PICK_LP per pick.
  Deposits are pure seed-derived statics; hnh-world gained find_tile +
  GridStore::terrain_at and pins the dev-seed rocky belt within ~26
  tiles of the spawn area.
- SMELTER STATION: StationSpec.kind: StationKind (Oven/Smelter)
  dispatches itemact input matching, refusals and the job output;
  tick_stations drops whatever the kind rolls (the hardcoded meat drop
  is gone). craft::SMELT_MAP melts ore labels: copper nugget ->
  bar-copper, tin nugget -> bar-tin, iron ore -> bar-castiron. Branch
  fuel, one ore per 30-tick job (server policy; legacy ~55 min per
  25-ore load), station quality formula on the output.
  DROP_WORLD_ALIASES renders pack-missing tin/cast-iron world shapes
  through sibling metals. Station wording neutralized on ALL paths
  (local, relay-ack, menu) to "The station ...".

WAVE 2 (this session's commit): the refinement tier.

- The SHIPPED paginae come alive: bloom2wrought (ad "wroughtiron")
  refines bar-castiron x1 -> bar-wroughtiron x1 (the finery-forge leg
  stand-in, tanhide pattern), shammer makes the smithy's hammer
  (bar-wroughtiron + branch -> hammer-smithys), the first metal tool.
  Unit counts are server policy; both paginae and both items ship in
  the pack.
- Unit pins: wrought_iron_and_hammer_recipes_are_wired (pack-aware),
  metal_refinement_chain_crafts_bar_and_hammer (craft_once end to
  end: q40 cast iron -> 25 wrought -> 13 hammer through the str
  softcap).
- Wire probe fix: test_smelt.py missed the second pickup hop of the
  gathering shape (click the deposit -> the ORE DROP spawns -> click
  the drop -> the inventory stack) and matched tin's aliased world
  shape wrong; both fixed (ORE_WORLD mapping + nearest-few deposit
  approach retries). SMELT: OK green (copper leg measured; the iron
  leg is probabilistic - 2/10 deposits are iron).

VERIFIED (fresh runs): cargo fmt --all -- --check clean; clippy
--all-targets -D warnings clean; cargo test --workspace 306 green
(11 proto + 279 unit + 6 wire + 10 world, 1 gathering #[ignore]).
oven regression STATION FLOW: OK and WORLD ENTRY: OK re-proved on the
wave-1 binary; SMELT: OK on the wave-2 binary.

PROCESS NOTE: this session initially rebuilt the whole metal chain
independently (ore boulders + sdt flag + its own StationKind + probe)
without noticing the parallel push until the non-fast-forward reject;
the local duplicate work was discarded at reset and re-based as the
smaller refinement wave. Lesson: re-check origin/master right before
pushing ANY session-shaped work - the repo has parallel writers.

NEXT (handoff):
- Type-3 candidates: kiln + brick chain (restores the legacy
  smelter/oven demands), anvil + smithy's-hammer tool-gated recipes,
  flower pick verbs, per-animal breed stat rows.
- Type-5 candidates: mvbat_fanout_us remains the top single phase at
  1000 sessions; entry-burst wmax (283 ms) attribution.
- Carried: test_smelt.py walk phase can exceed 2 min on unlucky
  pathing (bounded retry, runs green on retry); GL e2e + Windows
  smoke (no display host); multi-machine cluster profile; CI push
  when the token gets the workflow scope.

COMMITS: wave 1 = d91dcb0 (parallel writer); wave 2 = the refinement
tier + probe fix + docs + handoff (this session's two commits).
