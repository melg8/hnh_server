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
84=4, 85=5, 86=3, 87=4, 88=0, 89=4, 90=3, 91=0. Pick type freely - NOT 0 next.

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
- S85 (type 5): the vis delta-scan (gap #10b closed) - per-CELL deltas, not per-gob.
- S86 (type 3): the multi-machine cluster profile - cluster-up.sh + two e2e gates.
- S87 (type 4): remote-profile e2e + Java-exact move semantics for probes (OCache parity).
- S88 (type 0): the cluster surfaces land in AGENTS/README + the handoff header points at the gates.
- S89: true 60s session timeout + own-address game bind (Java source filter); real GL client e2e over the REMOTE profile, staged run-remote-e2e.sh; -Dhaven.authport/gameport client overrides.
- S90: the breeding pipeline - sexes, breed rows, scaled gestation, calves with inherited rows, maturation gate, honest milk refusals, v8 persist + restart reload; MCache-parity grid streaming for probes (the nav-flake root).
- S91: documentation de-pollution - the livestock doc consolidated (dup + stale chronicle removed, breeding notes landed), AGENTS.md session-protocol + Windows sections; GitNexus index re-bootstrapped.
  slices, milk draw <-> butter churn alternation, raisins quota,
  streamed-map grass spirals, tree-exhaustion switching; 338
  tests green.

## 2026-10-10 - Session 90 (type 3: the breeding pipeline - sexes, breed rows, gestation, birth, maturation, v8 persist)

SESSION TYPE ROTATION LOG: 87=4, 88=0, 89=4, 90=3. Pick the next type
freely - just NOT 3 (0/1/2/4/5 all open).

WHAT SHIPPED (the livestock breeding vertical, server + live e2e):
- state.rs: Sex on TameState (Female/Male, index roundtrip), the
  per-animal breed rows (prod_quantity - the milk/wool quantity row,
  breed_ql - the quality row), pregnant_acc + juvenile_acc
  accumulators, MATURATION/GESTATION legacy thresholds, the
  HNH_BREED_SCALE knob (x43200 for the live probe: ~9 s pregnancies
  and ~20 s childhoods against the doc-true tick terms).
- animals.rs: the sire sweep (a leashed male inside SIRE_RADIUS banks
  a pregnancy accumulator on a fed female), birth (a domestic-born
  calf gob beside the dam, INHERITED rows: sex roll, averaged breed
  rows, juvenile age 0), the maturation gate (juveniles graze and
  grow but open no production petal), the honest milk refusal lines
  (the bull gives no milk / not yet grown / no milk yet), the
  tamed_owner map so rows re-bind on the tamer's next login after a
  restart, follow/leash hygiene for calves beside their dam.
- persist.rs v8 (additive): sex, prod_quantity, breed_ql,
  pregnant_acc, juvenile_acc on SavedAnimal; pre-v8 saves load with
  documented defaults (female, legacy row, adult); boot restores the
  rows and remembers the tamer_key; the sigterm shutdown flush was
  verified to write the full herd (10 animals in the trace save).
- main.rs: every dev knob (no_aggro/fast_tame/breed_scale/
  leash_ticks/milk_rate/lp_rate) logs its effective value at startup
  so each probe log carries its own configuration.
- test_breeding.py (the live gate): quell-tame the cow + aurochs
  pair, walk the herd to the pasture, scaled gestation, CALF BORN
  beside the dam, maturation, the sex verdict through the flower
  menu (a heifer is milked through the inherited production row, a
  bull calf answers the refusal line and the dam re-conceives - up
  to six calves), then the restart leg: SIGTERM -> flush -> a second
  server boots on the SAME save and the SAME character re-enters on
  its persisted position beside the reloaded herd -> BREEDING: OK.
- hnhlib.py: MCache-parity grid streaming (the ROOT fix of the
  session - see LESSONS) + ensure_server keep_save for restart legs.

ROOT CAUSES FIXED (all live-traced, not guessed):
1. Restart leg loaded an EMPTY world: ensure_server deleted the save
   file before every boot - including the restart boot. The first
   server HAD flushed the herd on SIGTERM; the probe itself erased
   it (saved_chars=0 with the herd parked on the save). Fix:
   keep_save=True on the restart boot only.
2. The restart probe entered a FRESH character (name+"r") which
   spawns 3000 subtiles away at (555,555) and sees only wild cows;
   the herd reloads beside the ORIGINAL character's persisted
   position. Fix: re-enter the same username (the S89 instant-persist
   contract makes the offline char available at once).
3. Nav flakes (stalled chase, "nav to the grass field failed"):
   the probe client only ever requested the spawn 3x3 + one fixed
   5x5 grid ring; any pursuit that crossed into an unrequested grid
   walked BLIND - tile_at returns None, line_clear refuses,
   find_tile_path returns None, the probe stands still while the
   beast flees (live trace: the pursuit crossed into grid (1,3),
   BFS (111,296)->(125,348): None; the same pair on a client that
   streamed the grid: path len 7). Fix: every pump requests the 3x3
   grids around the avatar's CURRENT position (Java MCache parity),
   pending set + 5 s re-request for UDP loss.

LESSONS (probe-client parity, hard-won):
- A probe that stops moving is a MAP STREAMING bug first, a movement
  bug second: the server happily walks clicks into unstreamed tiles,
  the client-side planner just cannot aim there.
- Long sync test runs die with the canceled tool call; a bare
  nohup+setsid python also died minutes into the run (sandbox
  process eviction after context restarts) - the only reliable shape
  in this session was ONE foreground call (timeout 560) holding the
  whole ~4 min probe.
- faulthandler watchdog (500 s, exit=True) in the long e2e probes:
  a silent hang dumps its stack instead of stalling the gate.

EVIDENCE: BREEDING: OK (breedfinal5: tame x2, calf born from the
scaled gestation, heifer milked through the inherited row, herd
persisted and reloaded from the v8 save). Gate: cargo fmt clean,
clippy --all-targets -D warnings clean, 354 workspace tests green
(11 proto + 324 unit [2 ign] + 7 wire [1 ign] + 12 world). The
save-file trace: 10 tamed animals with v8 rows (sex/rows/preg/
juvenile) written by the SIGTERM flush and reloaded by the restart
boot. test_dairy.py + test_dough.py regressions exercised inside
the probe (quell/milk/plow/grass flows shared).

NOT DONE / next session carries (S91 must NOT be type 3): GL e2e +
Windows smoke of the fresh release binary on the WINDOWS port side;
the remaining NOT-DONE dough legs (S79/S82: oven-fired other doughs);
CI push still blocked on the PAT workflow scope (human action);
breeding depth beyond the vertical (wool/sheep rows exist in the
schema but no sheep e2e; cross-breed quality drift is a single
average, not the doc's mutation roll - revisit against docs/).

COMMITS: 043bcb5 (animals: the breeding pipeline + v8 persist),
f547ea9 (e2e: the breeding probe green live + MCache-parity grid
streaming + keep_save), this handoff.

## 2026-10-10 - Session 91 (type 0: documentation de-pollution - the livestock doc consolidated, the breeding notes landed, AGENTS.md gaps closed)

SESSION TYPE ROTATION LOG: 88=0, 89=4, 90=3, 91=0. Pick the next type
freely - just NOT 0 (1/2/3/4/5 all open).

GOAL (type 0, per the rotation): close the documentation debt S90 left
(the breeding vertical landed WITHOUT its domain-doc update, violating
the AGENTS.md same-changeset rule) and de-pollute the polluted docs
that keep costing every fresh context their re-read budget.

DONE:

- docs/mechanics/livestock/animals-and-husbandry.md: the six
  per-session chronicle sections (45/45-duplicate/46/47/48/62/77) are
  consolidated into ONE current-state "Server implementation notes
  (this repo)" section with domain subsections (taming, roster/morph,
  production/feeding/starvation, breeding, persistence). The chronicle
  had a VERBATIM DUPLICATE of the session-45 entry (254 lines of
  chronicle replaced; the doc went 693 -> 679 lines while GAINING the
  breeding notes). Old contradictions ("animals are not persisted",
  "breeding is out of scope") are gone.
- The session-90 breeding notes landed as their own subsection,
  cross-checked against the code (state.rs constants, game.rs Phase D,
  animals.rs spawn_calf/refusals, persist.rs v8): sexes, per-animal
  rows (wild 9..11), gestation 3 888 000 ticks / maturation
  8 640 000 ticks, BREED_SEEK_RADIUS 55, twins 5%, the nursing +
  BREED_CLUSTER_CAP=8 runaway guards, inheritance (average + spread
  +0..=2/-0..=1, sire softcap), HNH_BREED_SCALE, v8 persist with
  tamer-key rebinding, the SIGTERM flush.
- Open questions refreshed: the stale "breeding out of scope" bullet
  became the honest gap list (no sheep e2e, the calf milk-drinking
  reservation flow not modeled, lactation gated on adulthood+sex
  instead of the doc's never-calved heifer gate, no calf/bull
  drawable, the single-average quality drift vs the doc's mutation
  roll, fodder-average product quality still open).
- The same de-pollution pass over the two other chronicle-carrying
  docs: crafting-and-building.md (SEVEN session-note H2 sections, and
  a skeleton bug - sessions 71/77/81 had appended their notes AFTER
  "## Open questions") and food-and-fep.md (two sections) are each
  consolidated under one "## Server implementation notes (this repo)"
  H2 with the per-session chronicle demoted to H3 subsections and
  Open questions restored to the end; content preserved verbatim
  (plus two em-dashes -> ASCII and one half-resolved deviation line
  refreshed: bricks became craftable when the S70 kiln landed, but
  the oven/smelter demands keep the stone/branch substitute pending a
  balance pass).
- AGENTS.md: added the two sections a fresh context was missing -
  "Session Protocol (read HANDOFF.md FIRST)" (rotation rule, keep-
  last-two, commit discipline) and "Windows quick start (the
  user-facing contract)" (windows/run-client.bat as THE one command;
  .bat flow must keep working). CI paragraph refreshed (session 53 ->
  session 91: the workflow file still has not landed).
- GitNexus: the .gitnexus/ index was MISSING in this environment
  (sandbox eviction); re-bootstrapped via `bunx gitnexus@latest
  analyze --index-only` - the tool call reported canceled but the
  index actually landed and `node .gitnexus/run.cjs status` verifies:
  registered index at commit 51ebff2 (10/10/2026). The 130 MB .gitnexus
  tree is fully git-ignored (internal `*` .gitignore). This session
  edited documentation only - no code symbols - so no impact analysis
  was required; the index is ready for the next session's code work.

NOT DONE / next session carries: the reconstructed
.github/workflows/rust.yml ships UNTRACKED in the working tree (this
session's once-per-session push retry was refused: the PAT still
lacks the workflow scope - human action); everything else unchanged
from S90 except the doc debt - GL e2e + Windows smoke of the fresh
release binary;
the remaining dough legs (S79/S82); CI push still blocked on the PAT
workflow scope (human action); the breeding-depth items now honestly
listed in the livestock doc's Open questions (sheep e2e, heifer gate,
calf drawables, quality mutation roll).

COMMITS: this handoff records the docs commit (livestock
consolidation + breeding notes + AGENTS.md sections).
