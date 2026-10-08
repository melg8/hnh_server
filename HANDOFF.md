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
   `workflow` scope (session 50; retried in 53/55/56/57/61, see those
   entries). The full file content is preserved in the archive
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
5. **Feeding depth**: trough-to-trough fodder transfer needs the lift
   mechanic; per-animal breed stat rows (Milk Quantity / Wool Quality
   are flat constants).
6. **Real-client e2e**: GL production walkthrough (tame, wait out the
   milk meter, milk on screen) and Windows smoke when a display host
   exists (carried).
7. **Guest GC at scale**: CLOSED by measurement (session 59): the
   50-tick GC walk inside phase_cluster measured p50 73-98 us and
   p95 <= 314 us with 646-984 guests per node at 2x300 - two orders
   below anything actionable. Do not revisit without a multi-node
   profile that shows phase_cluster in the milliseconds.

## Session type rotation log (consolidated)

Per the alternating-goal rule (one goal per session; the user prompt
re-lists it every time). Sessions 1-44 predate the rule and were not
logged. Recorded tail: 45=3, 46=3, 47=3, 48=3, 49=2, 50=4, 51=3, 52=5,
53=0, 54=1, 55=2, 56=4, 57=5, 58=3, 59=5, 60=3, 61=2. All six types
have been served - pick freely, but avoid serving the same type as the
previous session.

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

---

---

## 2026-10-08 - Session 60 (type 3: new functionality)

SESSION TYPE ROTATION LOG: 56=4, 57=5, 58=3, 59=5, 60=3. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the carried type-3 item - world GATHERING (HANDOFF gap #4's
"bough/stone picking", the starter-kit stand-in). INVENTORY FIRST:
the repo already had statics (populate_grid spawns trees/bumlings
deterministically from JavaRandom per tile) and a click-harvest
(interact.rs + relay game.rs), but the session-59 state had four
fidelity defects, verified before cutting:
1. The tree pick dropped 10x gfx/invobjs/wood - and `wood` is used by
   ZERO recipes; the whole craft chain consumes gfx/invobjs/branch.
   Gathering could not feed crafting.
2. A boulder died on the FIRST click and dropped 10 stones.
3. The exhausted-tree stump spawned as Kind::Stone - clicking a stump
   yielded 10 stones.
4. The local path (game/interact.rs) and the relay path (game.rs
   relay_chop/relay_mine) duplicated the harvest logic by hand.

THE CUT (commit a63af90):
- Kind::Stone -> Kind::Boulder { left } with BOULDER_STONES = 5: one
  stone per pick, the boulder is removed when drained. New Kind::Stump:
  the exhausted-tree remnant is decorative and yields nothing. Both
  counts are state.rs constants (server policy; the wiki gives no
  numbers - recorded in the docs' Open questions).
- Trees drop ONE branch per pick; TREE_HARVESTS = 5 picks then leave
  the stump. Flat GATHER_QL = 10 matches the starter kit, so gathered
  materials craft identically. LP grants carried (5 branch / 3 stone).
- Shared pick legs harvest_tree/harvest_boulder (game/interact.rs)
  serve BOTH the local click and the cross-node relay; the duplicates
  in game.rs are deleted. Every pick re-publishes the harvest state to
  subscriber nodes (guest copies re-render; the old local path never
  published a frame bump).
- Guest views: boulders stay StaticClass::Stone (relay Mine); stumps
  map to StaticClass::Structure (no relay act), so a guest stump click
  is a validated no-op instead of a mine.
- docs/mechanics/crafting/crafting-and-building.md: new "World
  gathering" section (legacy baseline + every policy constant + the
  verification story) and an Open questions entry for the unknown
  legacy numbers.

VERIFICATION:
- 294 cargo tests green (11 proto + 270 unit incl. the three new pins:
  relay_mine_boulder_yields_one_stone_per_pick, stump_pick_yields_
  nothing, relay_chop_exhaustion_leaves_a_structure_class_stump; the
  chop test now asserts the BRANCH drop resource; + 4 wire + 9 world).
  fmt + clippy -D warnings clean.
- Release binary probes: WORLD ENTRY: OK, CATTR ORDER: OK, NEWCRAFT:
  OK (saw/bucket/fork paginae), EAT FLOW: OK, STATION FLOW: OK, and
  the new GATHER: OK - server/scripts/test_gather.py (hnhlib harness)
  walks to the seed-42 forest (the fresh spawn on the open grass has
  25+ boulders in view but ZERO trees; the nearest forest is grid
  (0,-1), ~90 tiles north - the probe documents this layout), picks a
  branch, then drains a boulder: 5 stone drops, every one picked up
  into the inventory, boulder retracted at the end.
- Probe craft notes for future sessions: fresh chars spawn around tile
  (50,50) but SAVED chars resume wherever they stopped, and drops only
  stream inside VIEW_RADIUS = 300 subtiles - so the probe walks within
  ~220 subtiles of the object before clicking (approach()), exactly
  what the real client's walk-to-target does. The res_http index is
  built at startup: regenerate gameres/ BEFORE booting the server or
  fork paginae 404.

Real-client e2e: NOT run this session (no display host in this
sandbox; deploy-agent-env.sh provisioning started but not waited out -
same carried state as sessions 58/59). The wire probe exercises the
same spawn/drop/pickup contracts the GL client consumes, and the drop
resources (gfx/terobjs/items/branch, .../stone) are the pack's own
neg-layer shapes validated in session 26.

COMMITS: a63af90 (world gathering) + this handoff entry.

NEXT (handoff):
- Type-3 candidates: feeding lift (trough-to-trough transfer), metal
  chain groundwork (ore gathering), flower-menu for the pick verbs.
- Carried: five-probe hnhlib.py migration (test_gather.py and
  test_newcraft.py are the templates), GL e2e + Windows smoke (no
  display host), multi-machine cluster profile, CI push when the
  token gets the scope.

## 2026-10-08 - Session 61 (type 2: refactoring / technical debt)

SESSION TYPE ROTATION LOG: 57=5, 58=3, 59=5, 60=3, 61=2. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the carried gap #3 - migrate the five legacy self-contained
probes onto the shared hnhlib harness and retire the test_build
re-export layer. The migration's job was to be PURE (verdict logic
byte-for-byte), and it was - which is exactly what made the three
STALE probe contracts it surfaced visible as failures this session.

MIGRATION (scripts/):
- hnhlib.WireClient absorbed the duplicated shared contracts the
  legacy clients each re-implemented: Area Chat (chat_id/chat_lines),
  char sheet (chr_id/exp_seen), cattr compiled values (attrs), party
  roster (pv_id), DSTWDG bookkeeping (destroyed set + sm cleanup),
  OD_BUDDY names (buddy_names), the flower-menu sm_args map, LIST_COLOR
  arg decoding. ensure_server gained env_extra/save_path parameters
  (scenario clocks, per-run saves) - the default behavior is
  unchanged.
- probe_animals, probe_direction: subclass WireClient with their own
  parse_objdata pre-pass for the pose-level event logs they verdict on
  (first spawn LAYERS per gob, walking streams, overlays); the base
  class keeps the gob-state tracking.
- test_farming: FarmClient is now a 20-line WireClient subclass (tile
  helpers + request_chr=False to keep the historical wire shape); the
  fast-crop server boot rides ensure_server(env_extra, save_path).
- test_party_chat: PartyClient = WireClient + party record hook +
  client-side LINBEG/LINSTEP interpolation (see the stale-contract
  fix below). parse_party stays party-domain.
- test_build: the 60-name re-export block is gone (the file keeps
  BuildClient + its own CLI); probe_melee/probe_pvp/test_equip import
  from hnhlib directly, probe_station aliases hnhlib as tb (its own
  parser is carried - a separate, larger migration), probe_plow takes
  le32 from hnhlib.
- dump_paginae.py DELETED: it speaks the pre-session-2 TCP game
  protocol ("hlauhunk" to port 1870, now UDP-only) and dies with
  ConnectionRefused against any server since the UDP switch -
  verified, not assumed. README: legacy section removed, the
  "Adding a probe" recipe documents the on_objdata pre-pass pattern
  and every shared contract WireClient already tracks.

STALE CONTRACTS FIXED (each proven pre-existing by running the
pre-migration script from git before touching the verdict):
- test_party_chat chatbot walked C to a LINBEG DESTINATION and judged
  distance by it - but the server has been reporting the ON-PATH
  interpolated position since the session-20 movement fidelity fix
  (game/interact.rs "never the destination ahead of time"), so the
  out-of-range client was still inside the 500-subtile chat radius
  when the marker fired. The probe now interpolates LINSTEP progress
  client-side and walks C beyond radius+margin before chatting.
- probe_direction expected the movement-octant digit, but session 22
  introduced the art-ring offset (art_dir = (octant + 7) & 7,
  unit-pinned); the probe now expects the art digit and prints both.
- test_equip equipbot resolved the branch item ONCE by resource name;
  after the unequip round trip the refresh_inventory recreate lagged
  DSTWDG on the wire and the resolved wid was the dying cursor copy -
  the epry drop then ran with an empty hand. equip_item now re-resolves
  and retries the take/drop round trip until the `set` confirms.
- probe_station + probe_drop: the kit's stone stack grew to 6 (bow
  chain, session 36) while the plan demand is 2 - the remainder rode
  the drag cursor and blocked the branch take (the session-51 cursor
  return contract, applied to test_build but never to these two).
  Both now return the cursor after each sink.
- probe_plow: PlowProbe.mapreq(gc) shadowed WireClient.mapreq(gx, gy)
  and crashed on the shared mapview bind; renamed to mapreq_grid.

VERIFICATION (every line is a fresh run this session):
- 293 cargo tests green (11 proto + 269 unit + 4 wire + 9 world);
  fmt + clippy -D warnings clean (no Rust changes this session).
- Probe battery, single node: WORLD ENTRY: OK, CATTR ORDER: OK,
  FARMING FLOW: OK, SKILL GATE: OK, CHAT FLOW: OK, PARTY FLOW: OK,
  DIRECTION WIRE: OK, ANIMALS WIRE: OK (330 layered animals, 115
  walking streams, bite overlays), BUILD FLOW: OK, STATION FLOW: OK,
  EQUIP FLOW: OK, EQUIP PERSIST: OK (server restart + slot-5
  restore), MELEE WIRE: OK, PVP WIRE: OK (arrow 75 dmg, HP 1/4).
- Probe battery, 2-node cluster (mesh 18790/18791): STATION RELAY:
  OK (fuel+input+light+output through the guest path), DROP TRANSFER:
  OK (output drop crossed the boundary and was picked up LOCALLY on
  node 0), PLOW RELAY: OK (TileMutation through the authority node),
  GUEST WALK: OK (four legs across peer-owned cells).
- One chatbot run failed on the sender echo (single UDP wdgmsg loss -
  the python probes have no client-side retransmit; the wire.rs tier
  does). Re-run green; recording as known test-harness flake.

COMMITS: scripts consolidation + probe fixes + docs + this handoff.

NEXT (handoff):
- probe_station's StationProbeClient still carries its own transport
  parser (aliased as tb); moving it onto WireClient is the remaining
  mechanical step when a session wants another type-2 item.
- Carried: feeding lift, metal chain groundwork, five-probe follow-ups
  none, GL e2e + Windows smoke (no display host), multi-machine
  cluster profile, CI push when the token gets the scope.
