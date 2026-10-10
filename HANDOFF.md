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
  cache misses the old walk also paid); session-75: RSS ~830 MB at
  1000 saturated sessions (~0.8 MB/session, 10k ≈ 8 GB) and the
  autosave boundary spikes gone after the background-flush move.
- Real-client e2e: scripts/jogl/ boots the real GL client under Xvfb
  (login, Robot map clicks, MOVEMENT/portrait/equipment verdicts;
  sessions 21/25/45). Windows: windows/ one-command scripts (fix log
  in the archive).

## Known gaps / next steps (consolidated)

1. **CI**: `.github/workflows/rust.yml` (fmt, clippy -D warnings, cargo
   test --workspace on push/PR) could not be pushed - the PAT lacks the
   `workflow` scope (session 50; retried in 53/55/56/57/61/63...73, see
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
   that session (see S59). CONFIRMED at the S73 sub-attribution: the
   pair walk IS the fan-out floor (probe/append/unacked/send stages
   sum to 3-8 ms of the 27-38 ms total at 204-288k pairs/tick) - and
   per-pair Instant timers were REMOVED after measuring (their own
   ~150-200 ns/pair x 270k pairs out-weighed every stage; do not
   re-add per-pair timers, profile via the phase counters only).
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
   SAUSAGES CLOSED (session 77: 12 of the 13 shipped wurst pages
   craft over the labeled-meat-slot gate; Piglet Wursts needs the pig
   morph, Bierwurst/Chicken Chorizo have no item resources).
   POTTERY CLOSED (session 79: four moldings + four fired kiln wares,
   live-driven end to end by test_pottery.py). BAKING BREADTH CLOSED
   (session 81: the five dough recipes + their ingredient sources -
   the forageable world, apple trees, wild hives, the raisin hand
   recipe - landed together, live-driven by test_dough.py; every
   dough output was already BAKE_MAP-keyed). Remaining dead ends:
   butter/carrot-cake lack a live chain drive (milk needs a tamed
   cow; carrots need the crop walk), and no dough OTHER than Bread
   has been oven-fired live yet (the new doughs are BAKE_MAP-pinned
   but not oven-driven).
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
10. **Perf follow-ups** (the S73 profile's named candidates): (a)
   CLOSED (session 78): the `unacked` layout went cache-friendly -
   inline BlockBytes over the raw channel (zero-alloc resends) and a
   flat frame-ordered per-gob Vec (no BTreeMap node per insert);
   measured retx_sweep mean -25..-27%, wmax -46..-48% at 1000
   saturated bots; (b) the vis full rescan per moving session (a cell
   crossing re-scans the whole rectangle) - a delta-scan (new cell
   strip only) is a bigger refactor; re-measure whether it still pays
   after the S73 parallel-probe move and the S78 allocator relief.
11. **10k memory footprint** (S75 measured): ~0.8 MB RSS per saturated
   session (830 MB at 1000) - the 10k single-process extrapolation is
   ~8 GB plus the per-node game/world overhead. Fits a 16-32 GB node;
   the horizontal split stays the real lever beyond that (gap 2).

## Session type rotation log (consolidated)

Per the alternating-goal rule (one goal per session; the user prompt
re-lists it every time). Sessions 1-44 predate the rule and were not
logged. All six types have been served - pick freely, but avoid serving
the same type as the previous session. Recorded tail: 45=3, 46=3, 47=3,
48=3, 49=2, 50=4, 51=3, 52=5, 53=0, 54=1, 55=2, 56=4, 57=5, 58=3, 59=5,
60=3, 61=2, 62=3, 63=4, 64=1, 65=5, 66=3, 67=4, 68=5, 69=3, 70=2, 71=3,
72=4, 73=5, 74=0, 75=1, 76=2, 77=3, 78=5, 79=3, 80=4, 81=3, 82=5, 83=3,
84=4.

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
- S72 (type 4): the craft + station contracts moved into the cargo
  gate - wire craft_flow (saw/bucket from the starter kit: the tool
  gate, the recipe label, the exact input consumption) + white-box
  oven bake and quern no-fuel pins; the tool-gate refusal now names
  the display name, not the resource path; 316 green.
- S73 (type 5): the vis-phase serial is-new probe walk moved into the
  parallel scan pass (fresh flags); A/B 1000-bot stationary tick
  30.1 -> 25.6 ms, vis spawn tail 4.1 -> 1.9 ms; the fan-out
  sub-attribution probe was built, measured, and removed (its own
  Instant cost out-weighed the stages); 316 green.
- S74 (type 0): the root README created (the missing user entry
  point); test_craft.py added to the probe table; the closed S70/S71
  debug diagnostics attic'ed; Known gaps consolidated with the S73
  verdicts + the named perf follow-ups; CLAUDE.md index numbers
  refreshed; the 13th CI push retry failed on the PAT scope.
- S75 (type 1): 10k-readiness audit - the blocking world flush found
  on the game loop and moved to a blocking thread (bench: 95 ms stall
  per autosave at 1k players, 519 ms at 10k; after: clone-only 8/55
  ms); live A/B confirmed the boundary spikes gone; RSS footprint
  ~0.8 MB/session (10k ≈ 8 GB); the 14th CI push retry failed on the
  same PAT scope (probed via a side branch, master untouched).
- S76 (type 2): the 8.4k-line game/tests.rs battery split into 12
  per-theme modules + common.rs (pure move, 154 tests + 35 helpers
  redistributed, the largest file now <=1091 lines); the #[path] hack
  dropped; 317 green.
- S78 (type 5): the unacked table went cache-friendly - inline
  BlockBytes over the raw channel (zero-alloc resends) + a flat
  frame-ordered per-gob Vec (was BTreeMap); retx sweep mean -25%,
  wmax -46..48%, tick band 42->29-33 ms at 1000 saturated bots; 323
  green.
- S77 (type 3): the sausage chain - bear + hen join the roster,
  Intestines enter the butcher loot, 12 of 13 wurst paginae become
  labeled-meat-slot recipes (Piglet Wursts deferred: no pork source);
  fep.conf reaches the unit tier; 320 green, live-verified.
- S79 (type 3): the pottery/butter/cake legs - four clay moldings +
  four fired kiln wares, the butter churn, carrot-cake dough; BAKE_MAP
  grew to the full shipped dough set; plain Bread gained its missing
  fep.conf row; the Python harness learned to parse bundled REL
  sub-messages (mid-bundle widgets dropped silently since ever);
  POTTERY live-verified; 326 green.

- S80 (type 4): the client resource-source order fix - addurl()
  chains the server HTTP pack ahead of the offline jar (the jar's
  historical AButton parent_ver, dodge->blk 28484 vs the pack's blk
  ver 1, killed the menu on world entry); the stale tracked
  server/gameres snapshot dropped (43 MB, outdated overlay, the
  fresh-clone landmine); UiProbe tri-mode green, 326 green.
- S81 (type 3): the dough ingredient chains - five forageable
  kinds + apple trees + wild hives + raisins hand recipe close
  the S79 baking dead ends; multi-unit Drop stacks; the fork
  pagina generator learns real parent versions; test_dough.py
  live probe.
- S82 (type 5): the tick tail fully attributed (tail_us +
  startbat_fanout_us + retx_retire_us + retx_retired_gobs), the
  targeted retire pass (37 -> 7 ms max), zero-alloc finalizer
  records in the fan-out; the dough pie cycle driven live end to
  end; load82.sh A/B harness.
- S83 (type 3): the dairy vertical - honeybun and pirozhki doughs
  live; the one-Fightview-per-player gate; directed panic-flee
  and the aggro leash (a surrendered chase walks home, the forced
  duel torn down server-side); the dairy e2e probe with fightview
  tracking.
- S84 (type 4): the dairy chain green end to end - 0.25 s pursuit
  slices, milk draw <-> butter churn alternation, raisins quota,
  streamed-map grass spirals, tree-exhaustion switching; 338
  tests green.
## 2026-10-09/10 - Session 81 (type 3: the dough ingredient chains)

SESSION TYPE ROTATION LOG: 77=3, 78=5, 79=3, 80=4, 81=3. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: close the S79 "five baking doughs without ingredient sources"
dead end in one move: five dough recipes (apple/blueberry/honey/
raisin/pirozhki), the forageable world that feeds them, and the live
probe that drives every new leg. The session was interrupted
mid-feature (all code landed in the working tree, tests already
green, but nothing committed); this continuation verified the tree
live, committed, and pushed.

DONE:

- Five dough recipes over the flour-and-water base invariant
  (apdough, dough_blueberrypie, hbdough, rbcdough, dough_pirozhki),
  all Cooking/Per-softcapped, every output label keying the BAKE_MAP
  oven dispatch. Ratios are server policy where RoB Legacy records
  no counts (the ccdough x2-fruit + x1-butter shape); the honeybun
  returns TWO empty buckets.
- state::ForageKind registry + Kind::Forage gobs: Blueberries
  (forest/heath), Chantrelles (forest), wild Grapevines, Yellow
  Onion (grass). One pick = FORAGE_YIELD (3) units in a single
  multi-unit Drop, then the plant is consumed (legacy single-pick).
  Visibility gating DEVIATION from legacy Per*Exp recorded in the
  farming doc.
- Kind::FruitTree (appletree res): APPLE_PICKS (5) picks, one Apple
  each, then degrades to a plain branch-yielding tree (the legacy
  stage-6 behavior compressed into one gob).
- Kind::BeeHive (bhive res): HIVE_HONEY_UNITS (3), bucket-gated
  harvest exactly like the session-47 milking contract (empty bucket
  in -> Bucket of Honey out; refusal system lines when empty-handed
  or drained). Permanent fixture. Guest nodes publish it as a
  no-act Structure (the bucket check lives on the home node - the
  S62 guest-trough state).
- Raisins: grapes x2 -> raisins x1 hand recipe (the S79 butter-
  precedent deviation; legacy dries on a frame over two days).
- make_fork_paginae.py resolves the parent's REAL on-disk version
  into the AButton layer (the session-80 "Wrong res version" root
  cause made hardcoded parent_ver=1 untrustworthy). The raisins
  fork page (paginae/craft/raisins, parented into the baking
  family) ships in res/compiled.
- Kind::Drop grew a count field (serde default 1; persisted queues
  and fixtures unchanged); the relay carries it through DropView
  and StaticStack; pickup grants the whole stack.
- fep.conf: Apple=CON:1 and Bucket of Honey=AGI:1 (any label the
  server can produce must resolve a row or eating silently bails -
  enforced by the eat path, documented in food-and-fep.md).
- Roll-band spawn table design: every new broadleaf/forest/grass
  static draws from the SAME per-tile roll (no extra draws), so all
  pre-existing spawns stay bit-identical and old saves keep their
  surroundings.
- test_dough.py: the live five-leg probe (four forage handfuls,
  apple budget + degradation, hive refusal + harvest, raisins
  craft, raw apple eat). DOUGH: OK live. White-box pin
  dough_chain_recipes_are_wired (every dough recipe keys BAKE_MAP,
  paginas ship, fep rows resolve, world sprites exist).
- diag_statics.py (the spawn-table dump used while tuning the roll
  bands) attic'ed.

MEASURED/EVIDENCE: DOUGH: OK live run (dough79555: 3 grapes, 5
apples, honey, raisins, eat). fix_gameres_parent_refs.py --check
gameres: stale=0 over 6385 resources. Gate: fmt + clippy -D warnings
clean; 327 workspace tests green (11 proto + 297 unit [2 ign] +
7 wire [1 ign] + 12 world). make-gameres already runs both repair
passes (the S80 follow-up is closed by inspection).

NOT DONE / next session carries: the butter + carrot-cake live
chain drive (milk needs a tamed cow - the taming walkthrough is on
the GL e2e list); a full dough->bake->eat pie cycle live (needs the
farm/quern flour chain - test_bake covers Bread end to end, the new
doughs are BAKE_MAP-pinned but not oven-driven); the vis delta-scan
re-measure (gap #10b); multi-machine cluster profile; GL e2e +
Windows smoke; CI push retry (the PAT still lacks the workflow
scope as of S74/S75 retries).

COMMITS: a3c3394 (world: the five dough ingredient chains close the
baking breadth), this handoff.

## 2026-10-10 - Session 82 (type 5: the tick-tail attribution and retirement)

SESSION TYPE ROTATION LOG: 79=3, 80=4, 81=3, 82=5. All six types served -
pick freely, avoid repeating the previous session's type.

GOAL: close the S81 PIE tail first (the dough->bake->eat pie cycle), then
serve type 5 against the named gap: the perf log carried 45-70 ms of
tick time on burst ticks that no phase counter owned. The session was
interrupted twice by context loss mid-measurement; the final
continuation verified the tree, committed, measured, and closed.

DONE:

- PIE LEG LIVE (the S81 NOT-DONE tail): test_dough.py grew
  blueberry_pie_leg - branches -> bucket (craft) -> water fill ->
  walk_to_grass ring scan -> farm wheat -> grind flour -> knead
  blueberry dough -> oven pie -> EAT. Two probe bugs fixed on the
  way: gather_branches waited for the whole want in one pickup
  (inventory grows one pick at a time - incremental before/after
  wait) and the farm leg ran deep in the broadleaf forest where
  plow_tile refuses everything but grass (farming.rs) - a
  TILE_SPAN ring scan now walks to a real grass field first.
  DOUGHPIE: OK live, committed 18adc43.
- TAIL ATTRIBUTION: tail_us now names the post-phase gap (the
  retransmit sweep's retire walk, the rare sweeps, the dirty-index
  clear, the tick-end start/FX batch fan-out), and
  startbat_fanout_us splits the start/FX batch share out of
  mvbat_fanout_us (which sums both fan-outs). retx_retire_us is
  timed separately because the retire walk runs AFTER
  retx_sweep_us is recorded. The four counters now own the whole
  tail (worst steady tick: owned 100-111%, >100% is cross-tick
  boundary wrap). Committed f18aac8 + 1df83e0.
- TARGETED RETIRE: the untargeted expire-retire pass rescanned
  EVERY session's unacked table after each sweep (O(sessions x
  pending), retain) and measured 37 ms max on burst ticks at 1000
  bots. The sweep already knows which sessions it saw expired
  blocks on, so the pass now walks that sid list
  (retire_scratch, a session-id ring like retx_scratch). A
  budget-skipped session retires on a later sweep - the ring
  cursor rotates through all sessions and the 10 s age ceiling
  dwarfs the 3-tick sweep period. retx_retired_gobs confirms the
  scope. Measured (load82.sh, 1000 saturated walking bots,
  steady): retire max 37013 -> 6964 us (-81%), tail max 61258 ->
  43947 (-28%), wmax p50 89811 -> 68178 (-24%), mean tick 49070 ->
  37961 (-23%). Committed 6caa31f.
- ZERO-ALLOC FINALIZER RECORDS: the fan-out's shared-block path
  recorded finalizer hits with bytes.to_vec() (malloc + copy on
  tick thread, then a second copy into BlockBytes' inline buffer
  and a free) - ~24k hits/tick at the 1000-bot scale, movement
  finalizer blocks are ~24 bytes, well inside the 144-byte inline
  width. BlockBytes::from_slice copies straight in; oversize
  re-render blocks pay one allocation, same as before.
  record_unacked split into owned/slice wrappers over one inner.
  Unit pin block_bytes_from_slice_matches_from_vec (0/1/8/144/145
  boundary + variant choice). Measured honestly: the tick-level
  effect is BELOW the sandbox run-to-run noise (three post-fix
  runs: startbat p50 11.7/11.7/15.2 vs 11.7 pre-fix; identical
  code spreads tick p50 27-64 ms across runs) - the malloc is
  gone by construction, structural relief in the S78 family.
  Committed fe9388d.
- load82.sh: the A/B harness - boots the release binary with 1000
  in-process saturated walking bots, holds the steady state
  (DUR=90 default), summarizes tail attribution + tick budget
  and the worst-tick ownership check from the perf log.
- scripts/analyze_perf.py kept OUT of the repo (session-local log
  comparison helper) in /home/z/my-project/scripts/.

MEASURED/EVIDENCE: gate green throughout - fmt, clippy -D
warnings, 328 workspace tests (11 proto + 298 unit [2 ign] + 7
wire [1 ign] + 12 world; one wire run flaked on the documented
2-core concurrent-boot starvation, rerun green); DOUGHPIE live;
load82.sh A/B numbers above. GitNexus: the AGENTS.md record
stands (deployed S67); no local .gitnexus index in this sandbox -
impact analysis done by grep + reading, per the standing note.

NOT DONE / next session carries: the vis delta-scan re-measure
(gap #10b) - untouched this session, the time went to the
tail-attribution family; butter + carrot-cake live chain drive
(milk needs a tamed cow, carrots need the crop walk); no dough
OTHER than Bread and the blueberry pie oven-fired live yet;
multi-machine cluster profile; GL e2e + Windows smoke; CI push
retry (the PAT still lacks the workflow scope).

COMMITS: 18adc43 (dough: the pie leg live end to end), f18aac8
(perf: tail attribution), 1df83e0 (perf: retire pass and start
batch own the tail), 6caa31f (perf: targeted retire), fe9388d
(perf: zero-alloc finalizer records), this handoff.

## 2026-10-10 - Session 85 (type 5: the vis delta-scan, gap #10b closed)

SESSION TYPE ROTATION LOG: 82=5, 83=3, 84=4, 85=5. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the carried gap #10b - "the full rescan per moving session is
the remaining scan cost" (named S73, untouched since). A session
that crossed a cell re-scanned its whole view square every scan;
serve short walks from the cached result instead. The session began
mid-implementation (the context loss left the working tree holding
the whole feature, tests included, uncommitted); this continuation
verified it, measured it honestly - including REVERTING a variant
that lost - and closed it.

DONE:

- DELTA SCAN (stream.rs): a viewer that moved within one
  VIEW_RADIUS of its last scan is served by re-filtering the cached
  ids by their CURRENT positions, plus the touched enterers near
  the new square, plus the STRIP cells the walk opened
  (visidx.rs strip_in_view_into: new-square cells NOT fully
  embedded in the old square - a naive overlap test misses static
  enterers living on the edge cells). Beyond the window (teleport,
  grid handoff) the full scan stays; a touched set near the view
  population (a dense mover herd) still flips to Full. ScanKind
  (Full/Patch/Delta) replaces the old bool; vis_delta joins the
  perf counters (vis_skipped + vis_cached + vis_delta = issued).
- UNIT PINS: vis_delta_scan_matches_full_scan_for_a_walking_viewer
  (static strip enterer, mover enterer, leaver, death, chained
  steps, a quiet step - the delta result must EQUAL a full rescan
  every step) and vis_delta_falls_back_to_full_after_a_teleport.
- THE REVERTED VARIANT (recorded in the patch doc comment): a
  binary-search-skip patch (skip the position lookup for cached
  ids absent from the touched list) was measured WORSE at the
  1000-bot saturated herd - phase_vis p50 8845 us over two runs vs
  7526 baseline. In a dense herd the touched lists approach the
  cache size, and ~700 cache-warm position lookups beat ~700
  binary searches over a cold ~300-entry scratch. Reverted to the
  linear re-filter; the numbers stay in the comment so the idea is
  not re-tried blind.
- load85.sh: the A/B harness (load82.sh's shape + the vis scan-mix
  and vis_scan_us/vis_cells columns; the perf line is a 5 s
  snapshot, so the per-snapshot counters read as per-5s rates).

MEASURED/EVIDENCE (load85.sh, 1000 saturated walking bots, 5 s
snapshots, seed 42): phase_vis p50 7526 -> 6763 us, mean 15582 ->
9398 (-40%), max 35912 -> 18530 (-48%); vis_scan_us max 26-32 ms
-> 10.4 ms. Scan mix stable across runs (issued ~42-43k/5 s: full
~23-26k, cached ~15-20k, delta 0.6-0.9k - the herd is dense, so
delta serves the few sparse walkers and the burst tail). Gate
green: fmt + clippy -D warnings, 339 workspace tests (11 proto +
309 unit [2 ign] + 7 wire [1 ign] + 12 world). Live smoke on the
release binary: WORLD ENTRY: OK + CATTR ORDER: OK.

OPERATIONAL NOTES: a server launched bare (&) inside a bash tool
call dies with the call's cancel - only the script-wrapper shape
(load85.sh) survives a canceled call as an orphan and finishes
itself; poll with SHORT calls (sleep 30), never sleep 240. The
perf line fires every 5 s: rows=19 is a full DUR=90 steady window.

NOT DONE / next session carries (S86 must NOT be type 5): GL e2e +
Windows smoke of the fresh release binary; the multi-machine
cluster profile; CI push retry (the PAT still lacks the workflow
scope); the remaining NOT-DONE dough legs from S79/S82 if any
surface on a fresh world.

COMMITS: 82ffc0f (perf: the vis delta-scan for a walking viewer,
gap 10b - implementation + tests + load85.sh + the A/B numbers),
this handoff.

## 2026-10-10 - Session 86 (type 3: the multi-machine cluster profile)

SESSION TYPE ROTATION LOG: 83=3, 84=4, 85=5, 86=3. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the long-carried "multi-machine cluster profile" - take the
cluster machinery that existed (mesh, grid ownership, guests, cross-
node relays - all verified in-process by unit tests and profiled by
profile_multinode.sh) and give it the PRODUCT surface: one command to
boot it, one command per machine for real multi-machine, and an e2e
gate that proves the profile against two live node processes.

DONE:

- cluster-up.sh (server/scripts): `up` boots a local 2..4-node
  cluster with the windows/start-cluster.bat port layout (node i =
  auth 1871+2i, game 1870+4i, res 1872+4i, mesh 18790+i; node 0
  keeps the client-facing defaults, so the Java client, run-client
  and every existing probe work unchanged). `remote` boots ONLY this
  machine's node from CLUSTER_SPEC=host:mesh,... + SELF=N - one copy
  per machine, same seed; the rendezvous-hash grid ownership splits
  the world across machines (the actual multi-machine deployment).
  `stop`/`status` manage both modes; per-node shard saves
  (save/cluster_n<i>.json) stay stable across restarts; readiness =
  the UDP game shard listening (NOT "sessions=" - that line only
  appears once clients connect, which silently broke the first boot
  wait).
- test_cluster.sh: the cluster e2e gate. Boots a fresh 2-node
  cluster (throwaway saves in /tmp), then proves MESH (cluster dial
  link up), WORLD ENTRY through node 0, GUEST WALK across
  peer-owned cells (node-1 log: peer-subscribed x4, guest-ingested
  x2), and CHARACTER MIGRATION. Verdict: CLUSTER E2E: OK (all legs
  seen green in the final run).
- probe_cluster_entry.py + hnhlib.WireClient(host, auth_port,
  game_port): one client now drives a session through ANY node - the
  migration leg enters through node 1's own ports (auth 1873 / game
  1874). auth_cookie gained the host parameter.
- THE MIGRATION FIX (root cause, not a retry loop): the first
  re-entry attempt answered "char query: nack (missing or online)"
  - the creation session was STILL LIVE on node 0 (UDP silence times
  out only after 60 s). The probe now sends MSG_CLOSE (twice - UDP
  is fire-and-forget): the server persists the character on
  SessionClosed immediately, and the re-entry takes the CharQuery
  path the same second. Evidence lines: node 0 "char query: serving
  snapshot to peer", node 1 "char migration received: entering
  world".
- probe_guest_walk.py: stale port docstring (9801/9800) refreshed to
  the real profile; scripts/README.md documents the new surface.

MEASURED/EVIDENCE: CLUSTER E2E: OK - mesh links up (node1 dials,
node0 accepts), WORLD ENTRY: OK, 4/4 walk legs arrived over
peer-owned cells (subscribed=4, ingested=2 on node 1), migration
served=1 received=1. Gate: 339 workspace tests green (Rust code
untouched this session - python/bash surface only); the final run's
full output in the session log.

OPERATIONAL NOTES: a leftover single-node server (S85 smoke) held
port 1872 and killed node 0's boot with "cannot bind resource http
port" - cluster-up.sh stop + pid files now clean that class; check
`pgrep -af hnh-server` before booting a cluster. Canceled tool calls
still orphan the script wrappers (they finish themselves); poll with
short sleeps.

NOT DONE / next session carries (S87 must NOT be type 3): GL e2e +
Windows smoke of the fresh release binary; CI push retry (the PAT
still lacks the workflow scope); the remaining NOT-DONE dough legs
from S79/S82 if any surface on a fresh world; a remote-CLUSTER_SPEC
smoke on two loopback addresses (127.0.0.1 + 127.0.0.2) to exercise
the remote code path end to end in the sandbox.

COMMITS: 7cd65c1 (cluster: the multi-machine profile, one command -
cluster-up.sh, test_cluster.sh, probe_cluster_entry.py, WireClient
node binding, the MSG_CLOSE migration fix, README), this handoff.
