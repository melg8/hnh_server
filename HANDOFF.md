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
6. Multi-node: `cd server && ./scripts/cluster-up.sh up` boots the
   local 2..4-node grid-owner cluster (node 0 keeps the default
   client-facing ports); `CLUSTER_SPEC=host:mesh,... SELF=i
   cluster-up.sh remote` is the per-machine deployment shape. The
   wire format itself is documented in the archive (sessions 27/34).
7. Cluster e2e gates: `./scripts/test_cluster.sh` must print
   `CLUSTER E2E: OK` and `./scripts/test_remote_cluster.sh` must
   print `REMOTE CLUSTER: OK` (mesh, guest walk over peer cells,
   cross-node migration) - run them after any cluster/stream/net
   change.
8. Client: `ant jar` (JDK 21 + Ant 1.10), run
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
## 2026-10-10 - Session 87 (type 4: the remote-profile e2e + the client move-semantics fix)

SESSION TYPE ROTATION LOG: 84=4, 85=5, 86=3, 87=4. Pick the next
type freely - just not 4.

GOAL: the carried "remote-CLUSTER_SPEC smoke on two loopback
addresses" - prove `cluster-up.sh remote` (the real per-machine
deployment shape) end to end inside the sandbox. The smoke failed on
its FIRST run; the session turned into root-causing that failure, and
the root cause was a genuine semantics bug in the probe client
library, not in the cluster code.

DONE:

- test_remote_cluster.sh (server/scripts): the REMOTE-profile e2e
  gate. machine A = 127.0.0.1, machine B = 127.0.0.2; each runs the
  exact operator command (`CLUSTER_SPEC=127.0.0.1:18790,127.0.0.2:
  18791 SELF=i cluster-up.sh remote`). Legs: REMOTE MESH (dial link
  up on both), GUEST WALK through machine A over machine B's cells
  (probe_guest_walk), and REMOTE MIGRATION - the character is created
  through machine A, then re-entered through machine B's OWN address
  (127.0.0.2:1873/1874; probe_cluster_entry.py gained the host
  argument): A serves the snapshot, B receives the migration.
  Verdict line: REMOTE CLUSTER: OK.
- THE BUG (first run): leg 1 of the guest walk reported "never
  started" - no LINBEG for the player gob - while legs 2-4 arrived.
  The one-shot instrumented dump (diag87_dump.py, since deleted with
  its runner scripts) captured the wire truth: LINBEG (frame 1) DID
  arrive 100 ms after the click - and 200 ms later a RETRANSMITTED
  entry block (frame 0, OD_MOVE) arrived after it, and hnhlib's
  on_objdata wiped "linbeg" on every OD_MOVE. The movement was
  therefore deleted client-side the moment it started; legs 2-4
  survived only because by then the retransmit wave had drained. The
  retransmits existed at all because probes never echoed OBJACK, so
  the ~700 bootstrap blocks stayed unconfirmed for the full 10 s
  retirement window.
- THE FIX (hnhlib.py, matched to the Java client, verified in
  src/haven/OCache.java): OD_MOVE updates the base pos only and NEVER
  cancels a live LinMove (OCache.move() sets rc, nothing else); the
  FINAL LINSTEP (l >= c) is the arrival marker and ends the move
  (OCache.linstep -> delattr(Moving)) - it now snaps pos to the
  target and clears "linbeg". And send_objacks now defaults True:
  the real client's SWorker acks within <=320 ms, so the server's
  unacked table drains and the retransmit sweep stays idle - probes
  model the real client instead of pathological silence.
- Diagnostics cleaned up: diag87.sh / diag87b.sh / diag87_dump.py
  were one-shot and are deleted (root cause closed); the permanent
  artifacts are test_remote_cluster.sh + the hnhlib fix.

MEASURED/EVIDENCE: REMOTE CLUSTER: OK under RUST_LOG=debug (mesh
machineA=1 machineB=1, guest walk 4/4 legs, guest ingested=2 on
machine B, migration served=1 received=1) - and the failure mode is
now pinned by a gate, not just observed. Regressions green: CLUSTER
E2E: OK (test_cluster.sh, all 4 legs + migration), MOVE PROBE: OK
(probe_walk: LINBEG seen right after the click, arrival snap exactly
on the clicked target), WORLD ENTRY: OK + CATTR ORDER: OK
(test_client.py), DIRECTION WIRE: OK (probe_direction). Rust
untouched: 339 workspace tests green (11 proto + 309 unit [2 ign] +
7 wire [1 ign] + 12 world).

OPERATIONAL NOTES: a canceled bash tool call does NOT kill the
script-wrapper shape (test_remote_cluster.sh finished as an orphan
and its output file held the full green verdict) - poll the output
file with short calls instead of relaunching. HANDOFF hygiene: this
session found S81/S82/S85 still living in HANDOFF.md above the
keep-last-two window; they moved verbatim to the archive in
chronological order (S81/S82 inserted before S83, S85 appended) -
grep the archive for them from now on.

NOT DONE / next session carries (S88 must NOT be type 4): GL e2e +
Windows smoke of the fresh release binary; CI push retry (the PAT
still lacks the workflow scope); the remaining NOT-DONE dough legs
from S79/S82 if any surface on a fresh world; a Java-client live
pass over the REMOTE profile (the hnhlib fix mirrors OCache, but the
real client has not walked the 127.0.0.2 leg yet).

COMMITS: (this session) scripts: the remote-profile e2e gate + the
Java-exact move semantics for probes, this handoff.

## 2026-10-10 - Session 88 (type 0: the cluster surfaces land in the agent/user docs)

SESSION TYPE ROTATION LOG: 85=5, 86=3, 87=4, 88=0. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: type 0 (docs hygiene) - the S86/S87 cluster surfaces existed
only in scripts/README.md and the session records; AGENTS.md (the
agent contract) and the root README (the user contract) never
mentioned them. Also the standing CI-retry rule was exercised.

DONE:

- AGENTS.md: new "Multi-Node Cluster Surfaces (run + verify)" section
  - cluster-up.sh up/remote/stop/status, the node-i port formula
  (auth 1871+2i, game 1870+4i, res 1872+4i, mesh 18790+i), the two
  e2e gates and WHEN they are mandatory (grid_owner.rs, nodes.rs,
  game/cluster.rs, hnh-proto node messages, session/stream changes).
  The probe-client parity contract is pinned in prose: hnhlib mirrors
  src/haven/OCache.java (OD_MOVE never cancels a live LinMove; the
  final LINSTEP l >= c is the arrival marker) and echoes OBJACK like
  the real SWorker.
- AGENTS.md GitNexus block: explicit fallback rule for environments
  with no .gitnexus index (fresh sandbox / env restart): rebuild, or
  text search + reading for that one session, stated in the handoff -
  never silently skip impact analysis, never treat a grep count as a
  graph verdict. (This sandbox itself has no index - the rule now
  covers that case instead of it being folklore in session notes.)
- README.md: "Quick start (multi-machine cluster)" section (one
  command locally; the per-machine CLUSTER_SPEC/SELF commands; the
  port formula; migration on re-login), both cluster gates in the
  verification block, and the stale "316 tests" refreshed to 339.
- HANDOFF.md header ("How to continue work"): step 6 now leads with
  cluster-up.sh (raw --cluster/--node stays referenced via the
  archive); new step 7 makes the two cluster e2e gates part of the
  standing next-session checklist.
- CI push retry (once per session, per AGENTS.md): restored
  .github/workflows/rust.yml from the session-50 archive snippet
  (fixing its corrupted `branches: aster]` line to [master]) as
  67a8cb9 and pushed - REFUSED again ("refusing to allow a Personal
  Access Token to create or update workflow without workflow
  scope"). The commit was rolled back and the file deleted so master
  stays pushable with this PAT; the snippet lives on in
  HANDOFF_ARCHIVE.md. The PAT owner must add the workflow scope (or
  push that one commit manually) to ever make CI real.

MEASURED/EVIDENCE: docs-only session - no Rust, no scripts touched;
the 339-test count and the gate verdict lines quoted in the docs come
straight from the S87 run. master pushable (8490fc3 on the remote;
the rollback verified with a follow-up push).

NOT DONE / next session carries (S89 must NOT be type 0): GL e2e +
Windows smoke of the fresh release binary; a Java-client live pass
over the REMOTE profile; the remaining NOT-DONE dough legs from
S79/S82; the PAT workflow-scope fix is a human action, not a session
task.

COMMITS: 8490fc3 (docs: the cluster surfaces land in AGENTS/README,
the handoff header points at the gates), this handoff.
