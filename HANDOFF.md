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

## Session type rotation log (consolidated)

Per the alternating-goal rule (one goal per session; the user prompt
re-lists it every time). Sessions 1-44 predate the rule and were not
logged. Recorded tail: 45=3, 46=3, 47=3, 48=3, 49=2, 50=4, 51=3, 52=5,
53=0, 54=1, 55=2, 56=4, 57=5, 58=3, 59=5, 60=3, 61=2, 62=3, 63=4, 64=1. All six
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

---

---

---

## 2026-10-08 - Session 63 (type 4: test coverage / test pyramid)

SESSION TYPE ROTATION LOG: 59=5, 60=3, 61=2, 62=3, 63=4. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the last three feature sessions (S58 recipes, S60 gathering,
S62 trough lift) shipped mechanics with unit pins and python probes
but ZERO coverage in the black-box wire tier - the only test layer
that boots the real binary and runs inside `cargo test` without
python. The gate could regress any of those mechanics silently.

WIRE TIER 4 -> 6:

- trough_lift_place_back_and_fodder_transfer_contract (IN the gate):
  build pagina "trough" -> plan spawn -> branch sink (whole starter
  stack, remainder rides the cursor back - the session-51 contract) ->
  fodder delivery as the completion signal (a 1-stage build keeps sdt
  at 0, so the delivery IS the only visible completion) -> 5 wheat
  units one itemact each -> the one-petal "Lift" flower menu ->
  gob retraction + the carry system line -> place-back with the store
  -> second trough (2 carrot units) lifted -> the "like a liquid"
  transfer ("Transferred 2 fodder units."). Every step asserts the
  system line.
- world_gathering_picks_yield_drops_exhaust_and_land_in_inventory
  (#[ignore]; run `cargo test --test wire -- --ignored`): 5 boulder
  picks each spawn a stone drop, the fifth retracts the boulder, a
  tree pick drops a branch and the tree survives, clicked drops land
  in the inventory (stack counts). A walking scenario: the harness
  approaches REACHABLE candidates one axis per hop (diagonal clicks
  hit water the axis path avoids) with a LINSTEP-based walk-start
  detector (a refused path emits no own-gob LINSTEP at all -
  load-independent).

HARNESS (tests/common/mod.rs):
- sm flower-menu tracking (petal strings per wid, DSTWDG-pruned),
  Area Chat lines (the "log" uimsg path system lines ride),
  click_gob (the (c0, mc, button, modflags, gobid, gobrc) shape),
  flower_choice, gob candidate scans (prefix/nearest, position
  verified), cursor_held + take-to-cursor retry loop (an inventory
  refresh retires wids mid-phase; the take must resolve the LIVE
  widget or the server refuses silently).
- OD_REM DECODE FIX: a flag-0 block carrying OD_REM is the removal
  (bots.rs ObjOp::Remove semantics) - the harness had only ever met
  flag-1 removals, so retract never registered and the trough lift
  was invisible. The OD_END byte after OD_REM must still be consumed
  or the next block parse desyncs.
- 2-slot concurrency governor (RAII): six servers on the two-core
  sandbox starve each other's tick loops and lose raw OBJDATA/
  MAPDATA datagrams (the session-56 localhost-UDP finding); the
  governor keeps the gate deterministic.
- movement contract REFINED: the re-click (the real client's
  behavior when a walk does not start - the mv-phase LINBEG batch
  rides RAW UDP) retargets from the interpolated position, so the
  LINBEG target is the clicked MAP POINT (tx ~= 775) and the segment
  length (tx - sx) is the remaining walk, not 220.

SERVER (observability only): debug! on the harvest paths
(tree pick / boulder pick / drop spawned) - debug level, no hot-path
cost, follows the obs-tracing rule.

VERIFIED (fresh runs):
- Gate: cargo fmt --all -- --check, clippy -D warnings, cargo test
  --workspace: 300 tests green (11 proto + 274 unit + 5 wire + 1
  ignored + 9 world) - repeated green runs at the end of the session.
- test_feeding.py choreography parity: the wire test walks the same
  steps the S62 probe drives (pagina, sink, load, lift, place,
  transfer) - both green against the same binary.
- Gathering wire test: green solo runs recorded mid-session (41s);
  flaky even solo late in the session (see gap #8) - hence #[ignore]
  with the documented explicit-run command. The root cause is
  server-side (no statics re-stream), not test-side.

COMMITS: wire tests + harness + handoff.

NEXT (handoff):
- Type-1/5 candidate: statics re-stream mechanism (MAPREQ-like
  re-request or OBJACK-triggered resend) - closes gap #8 and makes
  the gathering wire test gate-ready.
- Type-3 candidates: metal chain groundwork (ore + smelter), flower
  pick verbs, per-animal breed stat rows.
- Carried: cross-node lift/transfer relays, GL e2e + Windows smoke
  (no display host), multi-machine cluster profile, CI push when the
  token gets the scope.
## 2026-10-08 - Session 64 (type 1: architecture review)

SESSION TYPE ROTATION LOG: 60=3, 61=2, 62=3, 63=4, 64=1. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the retransmission architecture promised by the wire contract
but never built. Reading stream.rs / net.rs against Session.java
exposed the largest architectural defect of the raw UDP path: the
`unacked` table was WRITE-ONLY. Spawns, movement finalizers, hp ticks
and fx blocks were all recorded for "OBJACK retransmission", on_objack
only cleaned it - and NOT ONE LINE anywhere ever resent a recorded
block. Raw OBJDATA was fire-and-forget UDP wearing a reliability
costume. That is the root cause of gap #8 ("a lost spawn is
unrecoverable - the client never re-requests statics"), of the
session-56 flake family, and of the wire-test #[ignore] the last
sessions carried.

DESIGN (per-session, OBJACK-driven, in-order):

- `SessionOut.unacked` became a per-gob `BTreeMap<frame,
  UnackedBlock>` with `UnackedBlock { bytes, last_sent, tries,
  critical }`. The ordered map IS the in-order guarantee: the sweep
  walks frames ascending, so resent datagrams keep the wire order a
  spawn -> move -> retract sequence had on the socket; a resent stale
  frame must never overtake a newer one (an out-of-order OD_REM would
  phantom-delete a fresh spawn client-side).
- `SessionOut.gob_acked: GobId -> Option<u32>` - the client's acked
  high-water mark (it echoes its max decoded frame). The Option is
  load-bearing: frame 0 is a REAL wire frame (an untouched static's
  spawn) and the first implementation gated `frame <= acked(=0)`
  against it - the wire probe caught statics never retransmitting.
- `stream::retransmit_unacked()` runs every 3rd tick (~300 ms) as a
  rare-event pass: block delay = 250 ms fast window (5 attempts for
  critical, 3 for self-healing), then 1 s slow (4 / 2), then retire.
  Critical = spawn, retract, full re-render (build transitions, crop
  stages, guest kind changes) - blocks the client cannot recover any
  other way. Self-healing = movement finalizers, hp ticks - the next
  tick's frame supersedes a lost one. Retirement keeps sessions that
  never ack (load bots, dead peers) from accumulating retransmit
  state - the measured OOM shape from the 1000-session scale.
- A failed `try_send` (raw queue full under burst fan-out) does NOT
  burn an attempt: the next sweep retries while the queue drains.
- `on_objack` merges the ack high-water mark (max) and retains frames
  > acked - the old code was correct here, just incomplete.

RETRACT-FRAME FIX (caught by the full-suite trough-lift contract the
same session): an OD_REM must ride max(server frame, acked mark,
highest pending frame) + 1. The first implementation retracted at the
gob's current frame - for a killed gob that is a frame the client's
ack already covered, so the gate silently skipped the removal forever
and the client rendered a phantom. Second fix, same session: a
retract SUPERSEDES all pending blocks of the gob (a spawn resent
after OD_REM would resurrect a phantom).

CLIENT CONTRACT (verified against src/haven/Session.java): the
SWorker repeats its batched MSG_OBJACK every 200 ms while an entry is
under ~120 ms idle; a repeat OBJDATA frame merely updates objacks and
OC state (getgob is idempotent), so duplicate blocks are safe; a
block that stays unacked is unrecoverable client-side - hence the
whole mechanism.

WIRE PROBE (`lost_static_spawn_wave_is_retransmitted`, IN the gate,
~1.7 s): boots the real binary, arms a 1.2 s inbound-OBJDATA drop
window right after world entry (the whole statics burst plus the
first fast retransmits land inside it), then demands (a) a live
in-view static arriving - only the sweep can deliver it - and (b) the
deterministic proof: OBJACKs held, a duplicate (id, frame) MUST
appear on the wire. The harness gained drop_objdata_until /
dropped_objdata / hold_objacks / saw_retransmitted_spawn and a
seen-frames ledger; ServerGuard honors HNH_KEEP_WORKDIR=1 to preserve
a failed run's server log.

TEST TIER NOTE: the gathering walking scenario stays #[ignore]d, now
for a DOCUMENTED non-loss reason (40-90 s of wall-clock-bound walk
hops starve under two concurrent boots on the 2-core sandbox;
observed once in the full parallel suite). Gap #8 is
server-resolved and re-scoped; network-protocol.md "Server
implementation notes" gained the retransmission regime (frames,
schedule, the removal-frame rule).

VERIFIED (fresh runs):
- cargo fmt --all -- --check clean; clippy --all-targets
  -D warnings clean.
- cargo test --workspace: 301 green (11 proto + 274 unit + 6 wire in
  the gate + 1 ignored + 9 world).
- lost_static_spawn_wave: 3/3 consecutive green runs (~1.7 s each);
  trough-lift and the movement contract re-verified against the
  retract-frame change.
- Perf sanity (release, 300 bots, saturated world): steady-state tick
  7-12 ms, mean 28 ms, per-window max 69 ms only during the bot-entry
  burst - the 100 ms budget holds with the sweep live.

PROCESS NOTE: a `git reset --hard` while reverting the CI-push retry
wiped this session's uncommitted tree mid-session; everything was
re-applied from the session's own edit scripts and re-verified (the
full-suite green above is POST-restore). Commit early - uncommitted
work is unrecoverable work.

COMMITS: retransmission core + wire probe + harness loss simulation
(one change set), docs + handoff (second).

NEXT (handoff):
- Type-3 candidates: metal chain groundwork (ore + smelter), flower
  pick verbs, per-animal breed stat rows.
- Type-5 candidate: profile the retransmit sweep at the 1000-bot
  scale (it is O(pending) per ~300 ms; expect near-zero in the
  steady state - verify, do not assume).
- Carried: cross-node lift/transfer relays, GL e2e + Windows smoke
  (no display host), multi-machine cluster profile, CI push when the
  token gets the workflow scope.
