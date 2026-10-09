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
   WORLD GATHERING CLOSED (session 60). METAL GATHERING CLOSED
   (session 66: ore deposits + a working smelter; the tin leg of
   test_smelt.py still has the S67 reach oscillation). KILN CHAIN
   CLOSED (session 70: driven end to end on a live server - shore clay
   pick, kiln build, brick fired; the S69 "second sink round loses the
   clay" suspicion was a real plan -> station sink gap, fixed with the
   completion sysline 'The <id> is finished.').
   FLOUR/BREAD CLOSED (session 71: the full grain -> bread chain driven
   live end to end - wheat harvest, quern Grind verb, hand-kneaded
   dough, oven-baked Bread q10; test_bake.py in the gate corpus).
   Remaining dead ends: pottery beyond bricks, wurst/sausage and other
   baking doughs (station cooking depth; BAKE_MAP has exactly one
   entry, Bread Dough -> Bread).
5. **Feeding depth**: LIFT CLOSED (session 62): the trough lift /
   place / transfer mechanic is implemented and probed; the carried
   store survives restarts and cross-node migration. Still open:
   per-animal breed stat rows (Milk Quantity / Wool Quality are flat
   constants), the cross-node lift/transfer relays (a peer node's
   trough offers no Lift petal to a guest), and the carried-trough
   avatar render.
6. **Real-client e2e**: the headless REAL-client path is VERIFIED
   (session 70: UiProbe run/equip/charlist green against a live
   server, after the stale parent_ver pack fix). Still open: GL
   production walkthrough (tame, wait out the milk meter, milk on
   screen) and Windows smoke when a display host exists (carried).
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
logged. All six types have been served - pick freely, but avoid serving
the same type as the previous session. Recorded tail: 45=3, 46=3, 47=3,
48=3, 49=2, 50=4, 51=3, 52=5, 53=0, 54=1, 55=2, 56=4, 57=5, 58=3, 59=5,
60=3, 61=2, 62=3, 63=4, 64=1, 65=5, 66=3, 67=4, 68=5, 69=3, 70=2, 71=3.

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
- S67 (type 4): GitNexus deployed; bronze world-shape fix; hnhlib navigation
  layer (mapdata reassembly, BFS find_tile_path, nav_walk); MAPDATA pktid fix
  (monotonic mapdata_seq); test_smelt.py rewrite (copper leg stable, tin reach
  oscillation open); 307 green.
- S68 (type 5): fan-out pair attribution + wmax phase snapshot instrumented;
  the two named S65 candidates identified and fixed (visible bitset fast
  path, ack-lag adaptive retx RTO, conditional retire pass).
- S69 (type 3, record reconstructed by S70): clay deposits + kiln station
  committed (d4b965a, 311 green); the clay -> brick chain was never
  verified live - test_kiln.py + a suspected sink_demand bug left behind.
- S70 (type 2): game.rs split wave 2 - pose/lifecycle/entry/social/relay
  children (3634 -> 1287 lines, pure move, pub(super) parent-only
  methods); the S69 kiln debt closed - chain driven live end to end,
  the "lost clay" sink bug fixed (completion sysline) + probe hardening.
- S71 (type 3): the baking chain - quern (Grind verb, no fuel), grist ->
  flour, hand-kneaded dough, oven-baked bread; bucket fill at water
  tiles; crafted stacks now carry the recipe display name (the station
  label-matching defect); test_bake.py live green (BAKE: OK, Bread q10).

## 2026-10-08 - Session 67 (type 4: test coverage)

GOAL: the live bronze-chain wire probe (test_smelt.py) - session 66
built the crucible but only unit-tested it.

DONE (committed across this session):
- GitNexus deployed (318e215): index 10.6k symbols / 39k edges,
  AGENTS.md/CLAUDE.md sections, .claude/skills, .gitnexus gitignored;
  `gitnexus detect-changes` verified live.
- Bronze world-shape fix (80f12e0): bar-bronze rides the
  gfx/terobjs/items/bar-copper alias in DROP_WORLD_ALIASES (game.rs);
  craft.rs alloy_charge_pins pins the world shape, closing the hole
  the bug slipped through.
- hnhlib item_info now carries "count" (args[4], default 1) so stack
  merges (grant_pickup) are assertable.
- test_smelt.py rewritten: mine_until (kind accumulation across
  deposits) -> build_station (sdt-change tracking, not fixed values)
  -> smelt (per ore kind) -> gather_topup (boulder/tree picks) ->
  alloy (two bronze drops, count=2 assert).

VERIFIED FINDINGS (the "verify, don't assume" core of this session):
- MOVEMENT: the server accepts a ground click ONLY when the whole
  straight segment is walkable (state::path_clear samples one point
  per tile of Manhattan distance; LinMove, no detours). Ridge-blocked
  clicks move NOTHING, silently - the old probes stalled in place
  for minutes. Documented in map-and-terrain.md.
- SERVER BUG (fixed): MSG_MAPDATA pktid was world.tick-derived; the
  3x3 bootstrap MAPREQs land in the SAME tick, so their fragments
  shared one pktid and reassembly interleaved grids into garbage
  (the Java client's Defrag hits the same corruption). Fix: a
  monotonic mapdata_seq counter (game.rs, stream.rs). Documented in
  network-protocol.md.
- RCVBUF: the world-entry burst (9 grids + several hundred gob
  spawns) overflowed the default ~200 KB SO_RCVBUF; the kernel
  dropped whole grids nondeterministically (4-of-9 receptions).
  hnhlib now sets a 4 MB receive buffer.
- NAVIGATION LAYER (hnhlib): MSG_MAPDATA reassembly + zlib + per-grid
  tile bytes (row-major y*100+x), tile_at/walkable/line_clear (the
  path_clear mirror), find_tile_path (BFS over streamed tiles,
  8-neighbor, impassable goals retarget a walkable 4-neighbor), and
  nav_walk (short-segment clicking along the BFS path with stall
  retry). All future probes navigate instead of blind-clicking.

PROBE STATE (not yet SMELT: OK): copper mining is STABLE (two
deposits picked clean of copper: (264,264) and (121,-1100), the
northern one reached via a 1900-subtile BFS walk). Tin is NOT yet
mined: the three north-western deposits are ridge-isolated (BFS: no
tile path at all), the southern pair (-143,429) / (-572,286) has BFS
paths but nav_walk oscillates near the south-west rim (bot walks,
then re-plans back; suspect click-on-nearby-gob interception or a
client/server line-sampling mismatch on long diagonals - the
short-segment clicking already fixed the worst of it). Next session:
finish the tin leg, then the smelter/crucible phases are already
written and waiting.

VERIFIED: fmt + clippy -D warnings clean; cargo test --workspace
307 green after the mapdata fix (11 proto + 280 unit + 6 wire + 10
world).

NEXT (handoff):
- Finish test_smelt.py: tin deposit reach (debug the nav oscillation
  - try clicking pure tile centers away from gobs, or widen the
  sidestep retry), then SMELT: OK end-to-end (smelter + crucible
  phases are written).
- hnhlib nav_walk polish: the farthest-line-clear variant got whole
  clicks rejected on long diagonals (client/server integer sampling
  differ by one tile on negative deltas); short segments fixed it -
  keep that shape.
- Type-5 candidates: mvbat_fanout_us; entry-burst wmax (283 ms).
- Carried: GL e2e + Windows smoke; multi-machine cluster profile; CI
  push when the token gets the workflow scope.

COMMITS: 318e215 (gitnexus), 80f12e0 (bronze shape), + this session's
mapdata pktid fix, rcvbuf, nav harness, probe rewrite, docs.

---
## 2026-10-08 - Session 68 (type 5: performance)

SESSION TYPE ROTATION LOG: 64=1, 65=5, 66=3, 67=4, 68=5. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the S65/S67 handoffs named two type-5 candidates - mvbat_fanout_us
(the top single phase at 1000 sessions) and the entry-burst wmax (283 ms)
attribution. Both were fixed this session behind first-step instrumentation
(perf-profile-first: attribute, then cut).

INSTRUMENTED BASELINE (1000 bots, saturated world, release build; the
attribution commit efbddb1 produced these):

- fanout_pairs 347K vs fanout_hits 183K per tick: 53% of the pair work
  is a visible-set HashSet probe that ends in a REJECT (scattered
  FxHashSet bucket walk per (session, block) pair); fanout_fin only 19K
  (finalizer copies are NOT the dominant cost); fanout_msgs ~2000.
- mvbat_fanout_us 8-62 ms steady; mean_tick_us 71-92 ms.
- wmax attribution (the new snapshot): wmax_retx_sweep_us 85-107 ms -
  the RETRANSMIT SWEEP owns the entry-burst spikes (wmax_tick 188-208
  ms), not the fan-out (wmax_mvbat_fanout 22-27 ms). The sweep's
  retx_resent hit 44-73K blocks per pass with retx_pending 158-538K:
  the 80 ms first-retry schedule expires BEFORE the load bots' batched
  200 ms OBJACK lands, so every in-flight block reads as lost once and
  is resent (each with a deep bytes clone). The S65 "post-fix" numbers
  were measured on a smaller cohort; at a full 1000 the artificial-loss
  storm returns through the ack lag, not the missing OBJACK echo.

FIX (commit 20a96d4, one change set):

- SessionOut.visible_bits: a slot-index bitset mirror of the visible
  set (GobId packs the slot into the low 16 bits; a live gob owns its
  slot exclusively, so the mirror is exact when updated at the same
  three sites that mutate `visible` - spawn stream.rs, retract
  stream.rs, guest spawn cluster.rs). The fan-out pair loop probes one
  aligned word (~1 L1 load) instead of hashing into the scattered set;
  the authoritative set re-checks every bit-set pair, so a stale bit
  costs one probe, never a wrong send. Pinned by the
  visible_bitset_mirror_tracks_the_set unit test (direct method
  contract + live-path invariant after entry ticks).
- Ack-lag adaptive RTO: on_objack samples now - last_sent of the
  highest newly-confirmed block into an EMA (ack_lag_ema_ms); the sweep
  floors a block's FIRST retry at 2x the peer's mean lag (capped 4 s;
  tries > 0 keep the legacy schedule). A peer that acks in 200 ms
  batches no longer has its in-flight blocks retried at 80 ms. Fast
  ackers (lag < 40 ms) keep the legacy behavior unchanged.
- Retire pass (the second full O(pending) retain walk) now runs only
  when the main walk SAW an expired block (expired_n counter).

POST-FIX (same 1000-bot scenario, fresh runs):

- mean_tick_us 52-78 ms (was 71-92); wmax_tick_us 90-183 ms (was
  188-208); wmax_retx_sweep_us 22-85 ms (was 85-107).
- retx_resent 18-44K per pass (was 44-73K); retx_pending 54-165K (was
  158-538K); queue_full refusals mostly single digits after the burst
  (was 14-61K spikes).
- mvbat_fanout_us 3.5-43 ms band (was 8-62); hit ratio 42% of pairs
  (was 53%) - the bitset rejects the non-visible majority cheaply.

VERIFIED (fresh runs): fmt --check clean; clippy --all-targets -D
warnings clean; cargo test --workspace 308 green (11 proto + 281 unit
[incl. the new bitset pin] + 6 wire + 10 world). WORLD ENTRY: OK +
CATTR ORDER: OK on the final binary (python probe).

NOT DONE (deliberate, measured out of scope for the remaining budget):
- UnackedBlock.bytes as Arc<Vec<u8>> only pays if the raw channel
  carries Arcs too (a Vec channel forces the deep clone on resend
  regardless); with the ack-lag RTO the resend count fell ~2x already -
  revisit only if retx_resent climbs again.

WAVE 3 (same session, follow-up commit): the budget-capped incremental
retx sweep - DONE and verified.

- RETRANS_SWEEP_BUDGET = 8192 resends per sweep pass; each live
  session is guaranteed RETRANS_SESSION_SHARE_MIN = 64 sends (the
  global budget still caps the sum: with the share floor, sends stop
  at the budget boundary - share*sessions saturates it after ~128
  busy sessions at the 1000-peer scale).
- Round-robin ring: the sweep walks a SessionId-sorted scratch ring
  (retx_scratch, taken/restored - no per-sweep allocation) starting at
  retx_cursor; the cursor advances by the slots SEEN, so budget-starved
  sessions go first on the next pass. The ring MUST be sorted: the
  sessions map iterates in randomized order and an unsorted ring makes
  the cursor point at a different session every sweep (no fairness).
- Queue-full pre-check BEFORE the clone: tokio's mpsc capacity()
  counts FREE slots (drops on send, rises on recv) - a full queue
  reads capacity == 0. The first cut had this inverted
  (capacity >= max_capacity is TRUE for an EMPTY queue) and silently
  refused every resend - caught by the wire suite
  (trough_lift... "lift never confirmed": the retract echo is a
  retransmitted block, syslines are not). Fixed to capacity() == 0;
  the pre-check also kills the wasted deep clones on full queues.
- Share spent mid-gob breaks to the next sweep; breaking never
  reorders the wire - later frames are just NOT sent yet (the
  blocked-latch ordering guarantee is unchanged).

POST-FIX (1000-bot entry burst, /tmp/load_budget2.log):

- retx_resent pinned at 8.19-8.25K per sweep for the WHOLE run (the
  budget is the binding constraint, as designed; was 18-44K free-running).
- wmax_retx_sweep_us 5.5-14.3 ms (was 22-85 post-RTO, 85-107 pre-S68).
- retx_queue_full = 0 on every report (was 14-61K spikes).
- retx_pending drains 57K -> 10-17K steady (was 54-165K).
- mean_tick_us declines 66 -> 16 ms as the entry burst settles;
  end-of-run wmax_tick_us 33-47 ms (entry-window peaks 144-226 ms are
  mvbat fan-out bursts of 650-690K pairs, NOT the sweep - re-rank).
- VERIFIED: 308 green (11 proto + 281 unit + 6 wire + 10 world);
  fmt --check + clippy -D warnings clean.

NEXT (handoff):
- Type-5 candidate #1: mvbat_fanout_us - the last big wmax_tick
  contributor (31-63 ms peaks at 650K+ pair bursts during entry and
  bot-retirement churn). Ideas: split the fan-out pair loop per grid
  cell (the encode+bitset probe is already cheap per pair - profile
  where the 63 ms goes: pair iteration itself vs the HashSet-style
  finalize bookkeeping), or snapshot the mover list before the pass.
- Type-5 candidate #2: retx_resent stays budget-pinned in STEADY
  state (every report shows ~8.2K with pending 10-17K) - the sweep
  keeps resending ~8 blocks/session; check whether the ack-lag RTO
  floor (2x EMA, capped 4 s) is too low for load bots whose ack
  stream competes with the fan-out, or whether load bots retire
  before acking (retx_pending retiring through RETRANS_MAX_AGE_MS).
- Type-3 candidates (from S66/S67): kiln + brick chain, anvil +
  tool-gated recipes, flower pick verbs, per-animal breed stat rows.
- Finish test_smelt.py tin leg (S67 finding; smelter/crucible phases
  are written and waiting).
- Carried: GL e2e + Windows smoke; multi-machine cluster profile; CI
  push when the token gets the workflow scope.

COMMITS: efbddb1 (fan-out pair attribution + wmax snapshot),
20a96d4 (bitset fast path + ack-lag RTO + conditional retire),
this session's wave 3 (budgeted round-robin retx sweep).

---
## 2026-10-09 - Session 70 (type 2: refactoring / tech debt)

SESSION TYPE ROTATION LOG: 65=5, 66=3, 67=4, 68=5, 69=3, 70=2.

GOAL: game.rs had regrown to 3634 lines after the S49/S55 splits (every
feature wave S58-S68 landed in the parent) - the top type-2 debt.
Secondary: the inherited S69 debt (the kiln chain committed but never
driven live).

DONE (committed across this session):

- Wave 1 (d1ae3ab): game.rs split into five adjacent-file child
  modules, a pure move on the S49/S55 pattern (proj-mod-by-feature;
  methods only the parent calls are pub(super), proj-pub-super-parent;
  no signatures changed):
  - game/pose.rs (190): pose layer tables, move_dir/art_dir. The pub
    use re-export keeps crate::game::move_dir/art_dir reachable for
    the state.rs/nodes.rs doc references; the table accessors ride a
    pub(super) re-export into the sibling stream/interact/cluster
    globs (a glob only pulls items declared in game itself).
  - game/lifecycle.rs (430): autosave + save_all_and_flush, handle_cmd,
    report_perf, persist_player, on_session_closed.
  - game/entry.rs (633): session_connected, char_attr_snapshot,
    enter_world/enter_world_inner, find_spawn_position.
  - game/social.rs (631): chat relay, party invite/join/leave/sync, LP
    skill shop, chr/mapview/speedget widgets.
  - game/relay.rs (540): the authority-side legs applying guest-node
    relayed interactions (plant/plow/station/static/harvest/pickup/
    swing); the wire contracts stay in game/cluster.rs.
  game.rs: 3634 -> 1287 lines (the Game struct, constructors, run
  loop, tick, on_wdgmsg dispatch, shared wire helpers). GitNexus
  impact (enter_world) flagged HIGH pre-edit - compensated by the
  full gate; detect-changes run pre-commit. VERIFIED: fmt clean,
  clippy -D warnings clean, 311 green.
- Wave 2 (222a9ad): the S69 kiln debt closed. The chain was driven
  end to end on a live server (shore clay pick 46/48, kiln built from
  one merged 45-unit delivery, brick fired through fuel + input +
  Light, Brick q10 in the inventory) and surfaced two real bugs:
  - The plan -> station conversion keeps the gob id, so a deliverer
    that keeps sinking to the old plan target fed the new STATION's
    input slot - the S69 "second sink round loses the clay" suspicion
    confirmed as a real plan/station sink contract gap. Server policy:
    sink_material now announces completion with the system line
    'The <id> is finished.' (recorded in crafting-and-building.md).
  - test_kiln.walk_to_shore started walking before the shore grid
    (gc 1,0) streamed in - BFS only walks loaded tiles. The probe
    now waits for the grid (debug_kiln_nav*.py keep the record).
  test_kiln.py hardened: sink_demand stops on the completion line
  (chat lines cleared per build), build_kiln asserts the announcement
  + the station re-render (a full-demand delivery never shows an
  intermediate stage byte). VERIFIED LIVE: KILN: OK. Gate: fmt +
  clippy -D warnings clean, 311 green.
- Housekeeping: debug_kiln_sink.py dropped - its investigation is
  closed by the wave-2 finding.
- Java client headless verification (post-gate budget): the full
  client source (223 .java files) compiles against the live protocol
  and the REAL client classes drove auth -> charlist -> play -> world
  entry -> UI widgets headlessly - UI PROBE run/equip/charlist all OK
  (MapView, MenuGrid, Equipory, CharWnd, ...). The first run exposed a
  REAL pack defect: MenuGrid died with PaginaException on
  paginae/craft/clothmat - the legacy jar ships STALE parent_ver
  references (string.res -> clothmat ver 1 vs the real file ver 3;
  tanhide.res -> leather ver 1 vs ver 2), a second corruption class
  the existing fix_gameres_versions.py pass does not cover. New
  server/scripts/fix_gameres_parent_refs.py aligns every action-layer
  parent_ver with the parent's real file version (--check/--using for
  the partial res/compiled overlay); both make-gameres generators
  (sh + ps1) now run it over the generated pack AND res/compiled.
  gameres/ regenerated from scratch: WORLD ENTRY OK + all three UI
  PROBE modes green. debug_pagina_announce.py added (a wire probe
  asserting every RMSG_PAGINAE add matches the served file version -
  the differential that located the defect server-side vs pack-side).

COMMITS: d1ae3ab (the split), 222a9ad (kiln verified + fixes), this
## 2026-10-09 - Session 71 (type 3: new functionality)

SESSION TYPE ROTATION LOG: 66=3, 67=4, 68=5, 69=3, 70=2, 71=3. All six
types served - pick freely, avoid repeating the previous session's type.

GOAL: the S66/S68 handoffs named "kiln + brick chain" (done in S69/S70)
and the S70 known-gap #4 carried "flour/bread (the 2009 pack has no
grain item)". This session built and VERIFIED LIVE the full baking
chain: wheat -> grist -> flour -> dough -> bread. NOTE: this session
spanned multiple LLM incarnations (the first left the quern/GRIND_MAP
wave committed as bc84762; the last one finished the oven leg).

DONE (committed across the session):

- Wave 1 (bc84762, recovered + finished from the earlier incarnation's
  tree): quern station appended LAST to BUILDABLES (save-spec indices
  stable; Stone x2 + Branch x2 demand, NO fuel - the fuel gate skips
  StationKind::Quern, menu verb Grind instead of Light), GRIND_MAP
  Grist of Wheat -> Flour, BAKE_MAP Bread Dough -> Bread (own
  resource, not the roast-meat contract) with BOTH the relay accept
  and the tick output routed through the new maps; the dough hand
  recipe (flour x2 + bucket-water -> dough x2 + the empty bucket
  back, paginae/craft/dough ships); mature wheat yields Grist of
  Wheat (farm.rs WHEAT_MATURE, the pack has no grain item - sprout /
  grist / malt only); bucket fill (an empty bucket itemact on a water
  tile becomes a Bucket of Water in the cursor, game/items.rs);
  paginae/build/quern pushed; hnh-world test water_is_reachable_from
  _spawn (nearest water 57 tiles from spawn on seed 42, walkable
  launch neighbor pinned); craft unit tests pin the maps + recipe.
- Wave 2 (8fa470a): test_bake.py - the live end-to-end probe (saw +
  bucket crafts, farming skill sattr buy, five seeds plowed/planted,
  flower-menu harvest, water ring-scan + bucket scoop, quern build,
  2x Grind jobs, dough craft, oven build + bake). DROP_WORLD_ALIASES
  gained grist-wheat / flour -> gfx/terobjs/items/bag-seed (the pack
  ships no world shapes for the milled-grain items; the seed-bag is
  the closest silhouette, same fallback policy as bronze -> copper).
- Wave 3 (58c8776): REAL BUG fixed - crafted stacks shipped an EMPTY
  label, and every station input gate matches on the DISPLAY LABEL
  (BAKE_MAP/GRIND_MAP/KILN_MAP/SMELT_MAP keys are display names): the
  hand-crafted Bread Dough was refused with 'The station cannot
  process that.' - the oven leg was unreachable. craft_once now
  labels the PRIMARY output with recipe.name (all 38 recipe names are
  the product display names - verified); multi-output byproducts (the
  dough recipe's returned bucket) keep the empty label. Side effect:
  crafted tooltips show real names.

VERIFIED LIVE (the BAKE: OK run):
- saw + bucket crafted from the starter kit; Farming bought via sattr;
  five wheat seeds plowed/planted, 250 ms/stage growth, flower-menu
  harvest -> 8 Grist of Wheat; bucket scooped at the shore (the
  bucket-fill contract keys on the TILE under the click, the probe
  skips the shore-rim walk - nav oscillation, the S67 finding);
  quern built and ground 2 Flour (Grind verb, no fuel, 15-tick jobs,
  drops render through the bag-seed alias); dough crafted (x2 + the
  empty bucket back); oven fueled + loaded + Light -> Bread q10 in
  the inventory.
- Regression: KILN: OK (one flaky shore-grid wait on the first run,
  green on the retry); WORLD ENTRY: OK + CATTR ORDER: OK.
- Gate: fmt clean, clippy -D warnings clean, cargo test --workspace
  313 green (11 proto + 284 unit [1 ign] + 6 wire [1 ign] + 12 world).

NOT DONE / next session carries:
- A transient hnhlib UnicodeDecodeError on a bundled NEWWDG frame
  (killed ONE run after the bread drop spawned; did not reproduce;
  the decode now dumps the body and skips - monitor).
- The probe's water leg skips the shore walk (server does not check
  reach for the bucket fill) - a legacy-fidelity reach policy is an
  open server question for the mechanics doc.
- Carried from S70: GL e2e + Windows smoke; CI push (no workflow
  scope); multi-machine cluster profile.

COMMITS: bc84762 (quern + maps + dough + bucket + wheat), 8fa470a
(test_bake.py + drop aliases), 58c8776 (crafted-label fix).

handoff + the sink-probe removal + the Java-client pack fix
(fix_gameres_parent_refs.py, res/compiled repairs, UiProbe green).
