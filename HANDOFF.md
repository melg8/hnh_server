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
  EQUIP, PARTY/CHAT on the shared hnhlib.py harness.
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
   `workflow` scope (session 50; retried in 53/55/56/57, see those
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
3. **Probe migration**: move the five legacy self-contained probes onto
   hnhlib.py (mechanical; recipe in server/scripts/README.md).
4. **Recipe breadth**: MOSTLY CLOSED (session 58): 35 recipes total;
   the stone/bone tools, farm headwear, fishing gear, linen tier and
   the leather tier (via the tanhide/string fork pages) now craft.
   Remaining dead ends: metal chain (no ore gathering/smelter numbers),
   pottery/kiln (clay items exist, no station), wurst/sausage and
   baking doughs (station cooking depth), flour/bread (the 2009 pack
   has no grain item - sprout/grist only, see farm.rs), world GATHERING
   of branch/stone (bough/stone picking - the starter kit stands in).
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
53=0, 54=1, 55=2, 56=4, 57=5, 58=3, 59=5. All six types have been
served - pick freely, but avoid serving the same type as the previous
session.

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

---

## 2026-10-08 - Session 58 (type 3: new functionality)

SESSION TYPE ROTATION LOG: 54=1, 55=2, 56=4, 57=5, 58=3. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the top carried type-3 item - recipe breadth. HANDOFF gap #4
(read: "~150 shipped paginae, 16 implemented"). Scope for this session:
decode the full shipped paginae tree, implement the largest coherent
batch, verify on all three tiers.

INVENTORY FIRST (never assume - scan):
- server/scripts/scan_paginae.py: STATIC AButton decode of every
  paginae/craft/*.res (165 pages) straight from lib/haven-res.jar.
  Layer framing + the AButton layout were verified against
  src/haven/Resource.java (Resource.load layer loop at :1304,
  AButton(byte[]) at :1023) and reproduce the documented rustroot
  decode byte for byte. Result: ~140 leaf recipes with ad ids, parent
  categories, prereq codes; 19 were implemented (S36/S45/S46 batches).
- Cross-checked ingredient/output resources against gfx/invobjs and
  the ECONOMY (state.rs loot rows, farm.rs yields, starter kit): the
  pack is rich but most metal/pottery inputs have no source yet.

THE BATCH (19 new recipes, 35 total) - commits 5d06095:
- Stone/bone tools: saw (branch 2 + stone 1), bonesaw, pickaxe (pagina
  paxe.res carries ad ["craft","pickaxe"]), scythe.
- Farm headwear: straw hat (straw from the wheat early harvest),
  pumpkin hat, sprucecap.
- Woodwork: kuksa - the first recipe that CONSUMES a crafted tool
  (saw), deepening the S46 tool plumbing.
- Fishing gear: fishing pole, bone hook (fishing itself stays future
  work; the gear pages ship).
- Linen tier: toga (linencloth x4), cylinder hat (x3), gauze (x1).
- Fork pages: the pack has NO page whose ad is ["craft","string"] or
  ["craft","tanhide"], yet String/Leather are real invobjs the shipped
  pages consume. res/compiled/paginae/craft/{string,tanhide}.res are
  composed by server/scripts/make_fork_paginae.py (donor image layer
  from the invobj icon + a new AButton layer; layout verified by the
  same scanner; the res framing bug - a double length header - was
  caught by exactly that cross-check). string: flax fibres x2 -> string
  (flax/hemp early harvest). tanhide: hide-raw-cow x2 -> leather, the
  hand-tier stand-in for the unimplemented tanning tub - unlocks the
  shipped lboots/lpants/lcloak/waterflask pages.
- Starter kit: branch 6->10, stone 4->6 so the stone-tool batch is
  craftable without world gathering (bough/stone picking recorded as a
  new gap; gathering-with-nothing-to-hit was NOT invented here).

TEST PYRAMID (all three tiers, AGENTS.md rule):
- Unit (+4): leather_chain_tans_and_consumes (tanhide -> leather ->
  lboots with the full quality math: hides q40 -> leather q25 -> boots
  q15 through the [2,1] type weights and the sewing softcap),
  string_spins_from_flax_fibres, saw_crafts_from_starter_and_unlocks_
  bucket (the S46 saw-gap loop closes), recipe_registry_is_consistent.
- Wire: build_flow... de-hardcoded - the stone remainder expectation
  now derives from the actual starter count (starter - demand), so kit
  bumps cannot break it again; 4/4 wire green.
- Live wire probe server/scripts/test_newcraft.py ON hnhlib.py (the
  migration exemplar): act("craft","saw") -> make widget -> pop ->
  make 0 -> saw item; bucket with the CRAFTED saw; fork paginae served
  by res_http with a valid signature. NEWCRAFT: OK on the release
  binary; WORLD ENTRY/CRAFT/EAT base probes green.

VERIFICATION: 289 cargo tests green (11 proto + 265 unit + 4 wire +
9 world); fmt + clippy -D warnings clean; release binary probes green.

INCIDENT (recorded): the routine CI-workflow retry was run BEFORE the
main commit with a dirty tree; the push was rejected (PAT still lacks
the `workflow` scope) and the follow-up `git reset --hard` wiped the
uncommitted tracked-file edits (untracked scripts survived). Restored
byte-identically from the session transcript and re-verified (289
green + NEWCRAFT: OK re-run) BEFORE committing. Rule for future
sessions: commit the session's work FIRST, run the CI retry LAST.
CI workflow push retried once per the session-53 rule: REJECTED again
(no `workflow` scope).

COMMITS: 5d06095 (recipe breadth batch) + this handoff entry.

NEXT (handoff):
- World GATHERING (bough/stone picking from trees/rocks) - the natural
  next type-3 item; makes the starter-kit stand-in unnecessary and
  feeds the metal chain.
- Feeding lift (trough-to-trough fodder transfer), GL e2e + Windows
  smoke (carried), five-probe hnhlib.py migration (test_newcraft.py is
  the template now), CI push when the token gets the scope.

## 2026-10-08 - Session 59 (type 5: performance)

SESSION TYPE ROTATION LOG: 55=2, 56=4, 57=5, 58=3, 59=5. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the two carried profiling questions (HANDOFF gaps #2 and #7) -
the session-57 fan-out work said "only a multi-node profile can
justify more here". This session produced that profile and fixed what
the data pointed at.

PROFILE FIRST (new harness, no production changes initially):

- server/scripts/profile_multinode.sh: MODE=single|cluster BOTS=<per
  node> boots the baseline or a 2-node cluster with BOTH nodes loaded
  (--saturated --perf), settles, then prints per-node percentiles for
  tick/phase/mvbat/guests over a 60 s window. TERM/INT trap tears the
  nodes down - the first runs proved a bare EXIT trap does not run
  when bash dies from a signal, and orphaned nodes poison the next
  run's ports.
- Run A (single 600 bots): tick p50 68 ms, mvbat_fanout p50 12.4 ms,
  guests 0 - the pair-bound fan-out baseline at this population.
- Run B (cluster 2x300): PAIR-CAP CONFIRMED. mvbat_fanout p50 fell
  to 3.9/7.4 ms per node at the same total population; sessions=300
  per node, guests=646/984 (players in foreign cells + roaming
  animals both mirror). phase_cluster (subs + abroad + GC) p50
  73-98 us, p95 <= 314 us. Node tick stayed ~60 ms because both
  processes share this box's 2 cores - the pair savings are real
  but scheduler-masked locally.
- Run C (cluster 2x500 on one 2-core box): BOTH NODES STARVE - mean
  ticks 170-210 ms, p95 300-360 ms, perf reports skip. The cluster
  path carries work the single node does not: guest mirroring puts
  ~250 movers in front of ~500 sessions per node (bots spawn
  clustered, so most pairs survive the cell filter), and the fan-out
  runs twice (mvbat_fanout + guests_fanout at 90-130 ms each). The
  honest verdict: 1k clustered needs nodes on separate machines or
  more cores; on one 2-core box the single-node 1k (30-60 ms band)
  remains the better shape.

CUT (the one profiled cluster excess):

- guest pose finalizers: tick_guests encoded one OD_LAYERS block PER
  (session, guest) pair - intern lookups + layer allocations per
  pair; the 2x500 profile measured guests_pose_us at 22-52 ms (vs
  0.5 ms at 2x300). queue_guest_pose now encodes ONCE per guest with
  global-index placeholders (Patch::Many) and pushes into the packed
  start batch, so broadcast_batch resolves per-session wire ids,
  first-announces unseen resources, filters visibility and records
  the block in unacked - the session-44 local-pose machinery. The
  ingest pose-flip path rides it too. Two process notes: the pose
  block is now retransmittable (fin=true, matching local poses),
  and it ships at tick end instead of immediately (<= 100 ms lag,
  same as local start/FX blocks).
- Verified against the same 2x300 profile: guests_pose_us p50
  528-608 us -> 10-11 us, p95 6.9-10.9 ms -> 14-16 us; phase_guests
  p95 11.9/5.9 -> 6.1/2.1 ms; everything else in its old band.

VERIFICATION:

- 290 cargo tests green (11 proto + 266 unit incl. the new
  guest_pose_finalizer_fans_out_patched_layers wire pin + 4 wire +
  9 world); fmt + clippy -D warnings clean.
- Release binary: WORLD ENTRY: OK + CATTR ORDER: OK + EAT FLOW: OK.
- profile_multinode.sh re-run after the cut (see numbers above).

COMMITS: e10b144 (pose cut + test) + 73fe043 (profile harness +
README) + this handoff entry.

NEXT (handoff):
- The 10k path's next honest step is a MULTI-MACHINE cluster profile;
  single-box cluster runs now have a recorded ceiling. If a bigger
  dev box appears, rerun profile_multinode.sh with BOTS=1000+.
- Carried: world gathering (bough/stone picking - type 3), feeding
  lift, five-probe hnhlib.py migration, GL e2e + Windows smoke (no
  display host), CI push when the token gets the scope.
