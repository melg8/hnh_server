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
  master: 44-48 ms mean tick with workers=auto on 2 cores.
- Real-client e2e: scripts/jogl/ boots the real GL client under Xvfb
  (login, Robot map clicks, MOVEMENT/portrait/equipment verdicts;
  sessions 21/25/45). Windows: windows/ one-command scripts (fix log
  in the archive).

## Known gaps / next steps (consolidated)

1. **CI**: `.github/workflows/rust.yml` (fmt, clippy -D warnings, cargo
   test --workspace on push/PR) could not be pushed - the PAT lacks the
   `workflow` scope (session 50; retried in 53, see its entry). The
   full file content is preserved in the archive (session-50 addendum).
   Retry the push every session; a green local run stays mandatory.
2. **Perf fields**: cumulative max-tick counter never resets - add a
   per-window max to attribute the 197-210 ms ramp-up spikes.
3. **move_batch**: profile batch_move_broadcast (mv phase is now #2:
   3-32 ms windows).
4. **Probe migration**: move the five legacy self-contained probes onto
   hnhlib.py (mechanical; recipe in server/scripts/README.md).
5. **Recipe breadth**: ~150 shipped paginae; oven-gated tools/
   furniture/containers; flour/bread blocked (the 2009 pack has no
   grain item - sprout/grist only, see farm.rs).
6. **Feeding depth**: trough-to-trough fodder transfer needs the lift
   mechanic; per-animal breed stat rows (Milk Quantity / Wool Quality
   are flat constants).
7. **Real-client e2e**: GL production walkthrough (tame, wait out the
   milk meter, milk on screen) and Windows smoke when a display host
   exists (carried).
8. **Guest GC at scale**: the 50-tick guest GC walks the whole guest
    table per node (session-35 design); bounded by the subscribed
    population, so fine at 1k - revisit only if multi-node profiling
    says otherwise (session 54 review note).

## Session type rotation log (consolidated)

Per the alternating-goal rule (one goal per session; the user prompt
re-lists it every time). Sessions 1-44 predate the rule and were not
logged. Recorded tail: 45=3, 46=3, 47=3, 48=3, 49=2, 50=4, 51=3, 52=5,
53=0, 54=1, 55=2, 56=4. All six types have been served - pick freely,
but avoid serving the same type as the previous session.

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

---

---

## 2026-10-08 - Session 55 (type 2: refactoring / tech debt)

SESSION TYPE ROTATION LOG: 51=3, 52=5, 53=0, 54=1, 55=2. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: finish the session-49 split leftovers flagged in the gaps list -
extract game/interact.rs and game/combat.rs from game.rs.

WHAT:

- game/interact.rs (new): the Map section (on_map_click, player_walk,
  player_interact) and the movement/fan-out block (tick_movement,
  broadcast_batch, record_unacked, fx_overlay_broadcast, stream_pose,
  stream_avatar, interpolated_pos, start_move). tile_at stayed in
  game.rs (the farming/station sweeps use it too).
- game/combat.rs (new): the openings duel + archery + frv protocol
  (start_fight, start_pvp_melee, start_aim, tick_aim, shoot_arrow,
  fight_uimsg, fight_open, fight_del, on_maneuver, on_frv_msg) and the
  PvP consequences (armor_totals, melee_dmg, hurt_player,
  knockout_lp_loss, flag_criminal, stream_criminal_buff,
  tick_criminal_expiry, tick_vitals; CRIMINAL_MS/CRIMINAL_BUFF_ID).
- Pure move, no behavior changes. Cross-module methods widened to
  pub(super) exactly like the existing game/ pattern; every caller was
  grep-verified before the move.
- game.rs: 5325 -> 3465 lines; the game/ tree is now 10 feature
  modules + the test battery.

PROCESS NOTE (self-inflicted, recorded so it is not repeated): the
per-session CI push retry ran BEFORE the refactor was committed, and
the `git reset --hard` rollback of the (expectedly rejected) workflow
commit silently reverted the uncommitted game.rs. The first
verification round then ran against the OLD tree with the new files
ignored as dead code - clippy and tests still passed. Re-applied the
split, re-verified, amended the commit. Lesson: commit first, THEN do
the CI retry.

VERIFICATION (on the split tree):

- fmt + clippy -D warnings clean; 285 cargo tests green (261 unit incl.
  the moved combat/movement batteries, 11 proto, 4 wire, 9 world).
- Release binary: python probes WORLD ENTRY: OK + CATTR ORDER: OK.
- 300-bot load smoke: mean tick ~7-11 ms (budget 100 ms), live animal
  fights flowing through game/combat.rs.
- CI workflow push retried once per the session-53 rule: REJECTED
  again (PAT lacks the `workflow` scope), commit rolled back, token
  unchanged - do not retry until the scope exists.

COMMITS: 96e8824 (the split) + this handoff entry.

NEXT (handoff):
- Wire-test de-flake (type 4) and batch_move_broadcast profiling
  (type 5) are the top carried items; recipe breadth (type 3) and the
  five-probe hnhlib.py migration are mechanical.
- Windows smoke + GL client e2e still carried (no display host here).

---

## 2026-10-08 - Session 56 (type 4: test coverage / test pyramid)

SESSION TYPE ROTATION LOG: 52=5, 53=0, 54=1, 55=2, 56=4. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: de-flake the wire suite (the top carried item): reproduce
movement_click_walks_with_linstep_progress's parallel-load flake,
root-cause it, fix it in the HARNESS (or the server - whichever the
evidence points at), and prove the fix under stress.

REPRODUCED: full-suite run 3 of 8 (default cargo parallelism, no added
load) - the test panicked at wire.rs:182, "no own-gob LINBEG after
ground click": the own LINBEG never appeared within 10 s of the map
click.

ROOT CAUSE (harness, not server): the harness sent every reliable
datagram (the WDGMSG click, play, take, itemact, ...) exactly ONCE.
On localhost, parallel-test CPU load overflows kernel receive
buffers and silently drops datagrams; a lost click is never resent,
the server never starts the walk, and the failure is indistinguish-
able from a server bug. The legacy client owns this duty (Session.java
RWorker retransmits unacked on the 80/200/620/2000 ms backoff until
the server's cumulative MSG_ACK covers them) - the harness had simply
never implemented that half of the contract. The server side was
verified sound: RelSender retransmits its own stream on a 20 ms
scheduler, and RelReceiver dedups resent client datagrams by seq.

FIX (tests/common/mod.rs, the black-box client side):

- pending_rel: every sent MSG_REL datagram is tracked (last submessage
  seq, next retry time, attempt) until its cumulative ACK arrives.
- MSG_ACK handling drops covered datagrams with the same
  wrapping-window compare the server's RelSender::on_ack uses.
- pump_until retransmits due datagrams each cycle (granularity = the
  200 ms recv timeout; the legacy cadence tolerates that).
- The LINBEG assert now reports the unacked-datagram count.

The build-flow test inherits the protection (all take/itemact/place
traffic rides the same path). Server code untouched.

VERIFICATION:

- 15/15 green full-suite runs after the fix: 10 standard + 5 with two
  busy-loop CPU hogs added (the unfixed baseline failed on run 3 of
  8). fmt + clippy -D warnings clean; 285 tests green.
- CI workflow push retried once (rule): REJECTED again - the PAT still
  lacks the `workflow` scope. Rolled back AFTER the fix commit was
  already safe on its own commit (the session-55 lesson, applied).

COMMITS: 742871a (the de-flake) + this handoff entry.

NEXT (handoff):
- Remaining gaps (8 items): per-window max-tick perf field (cheap),
  batch_move_broadcast profiling (type 5), probe migration
  (mechanical), recipe breadth (type 3), feeding lift, GL e2e +
  Windows smoke (carried), guest GC at scale (revisit with multi-node
  profiling only).
- The duplicate-ACK note: the server's RelReceiver drops duplicates
  without re-ACKing (legacy-faithful); a lost ACK therefore keeps a
  harness entry pending until the next send - harmless, recorded for
  anyone debugging pending_rel growth.
