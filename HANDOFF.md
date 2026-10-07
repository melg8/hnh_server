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
  - `game.rs` + `game/` (session-49 split; game.rs keeps the core:
    construction, world entry, session lifecycle, tick dispatcher,
    movement, player interaction, party/skills, PvP vitals):
    `game/animals.rs` (wildlife AI, quell/taming, production+feeding
    sweep, starvation), `game/building.rs` (plans, stations, trough),
    `game/craft.rs` (make widget, recipes, roast chain),
    `game/farming.rs` (plow/mutate/plant/crop menus/harvest),
    `game/items.rs` (drops, inventory/equipment windows, drag cursor,
    food menu, eating), `game/stream.rs` (mapreq, gob block encoder,
    spawn/retract, visibility pass), `game/cluster.rs` (node messages,
    guest mirroring/republishing, subscriptions, authority transfer),
    `game/tests.rs` (the unit battery).
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

## Known gaps / next steps (consolidated, session 53)

1. **CI**: `.github/workflows/rust.yml` (fmt, clippy -D warnings, cargo
   test --workspace on push/PR) could not be pushed - the PAT lacks the
   `workflow` scope (session 50; retried in 53, see its entry). The
   full file content is preserved in the archive (session-50 addendum).
   Retry the push every session; a green local run stays mandatory.
2. **Wire-test flake**: movement_click_walks_with_linstep_progress is
   flaky under parallel cargo test load (UDP timing). Give the wire
   suite the pump_until treatment (type-4 session; same class as the
   session-51 world_entry de-flake).
3. **Perf fields**: cumulative max-tick counter never resets - add a
   per-window max to attribute the 197-210 ms ramp-up spikes.
4. **Guest scan**: scan_visible's guest loop is O(guests) per full
   rescan - fine single-node, a landmine at multi-node 10k. Add a
   per-cell guest bucket (type 1 or 5).
5. **move_batch**: profile batch_move_broadcast (mv phase is now #2:
   3-32 ms windows).
6. **Probe migration**: move the five legacy self-contained probes onto
   hnhlib.py (mechanical; recipe in server/scripts/README.md).
7. **game.rs split leftovers** (type 2): game/interact.rs (map click/
   walk/interact/movement/batch) and game/combat.rs (fights, arrows,
   vitals, criminal); game.rs is ~5.3k lines after those.
8. **Recipe breadth**: ~150 shipped paginae; oven-gated tools/
   furniture/containers; flour/bread blocked (the 2009 pack has no
   grain item - sprout/grist only, see farm.rs).
9. **Feeding depth**: trough-to-trough fodder transfer needs the lift
   mechanic; per-animal breed stat rows (Milk Quantity / Wool Quality
   are flat constants).
10. **Real-client e2e**: GL production walkthrough (tame, wait out the
    milk meter, milk on screen) and Windows smoke when a display host
    exists (carried).

## Session type rotation log (consolidated)

Per the alternating-goal rule (one goal per session; the user prompt
re-lists it every time). Sessions 1-44 predate the rule and were not
logged. Recorded tail: 45=3, 46=3, 47=3, 48=3, 49=2, 50=4, 51=3, 52=5,
53=0 (docs hygiene). Type 1 (architecture review) is the only
never-served type - next sessions should pick 1 before another 3/4/5.

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

---

## 2026-10-07 - Session 51 (type 3: mechanics - the session-50 TOP fix)

SESSION TYPE ROTATION LOG: 46=3, 47=3, 48=3, 49=2, 50=4, 51=3 (the
type-3 slot was explicitly sanctioned by session 50: "the build-flow
regression needs a type-3 session to root-cause and fix"; next session
should pick 5 (performance), 0 (docs hygiene) or 1 (architecture
review) before another 2/3).

TOP ITEM CLOSED: the build-flow branch-sink regression is root-caused,
reproduced, fixed, and pinned on both requested tiers.

ROOT CAUSE (not the session-48 itemact churn; it moved earlier): the
session-36 starter-kit bump (stone 2 -> 4, branch 2 -> 6 for the bow
chain) silently broke every build choreography. The plan sink caps at
the demand line (oven: stone x2, branch x1), the undelivered remainder
STAYS on the drag cursor (legacy behavior), and the next inv take is
refused ("one cursor item at a time"), so the branch itemact carried
stones (remaining(stone)=0 -> silent "does not need that") and the plan
stalled at sdt=1 forever. test_build.py buildbot simply was not run
between session 36 and session 50, so the "regression" waited there the
whole time. Server behavior was never wrong: the inventory `drop`
wdgmsg path (the legacy drag release) returns the held stack, exactly
like the legacy client.

FIXES:
- hnhlib.py: WireClient.return_cursor() - the inventory `drop` wdgmsg
  that stows the held stack; widget_by_name helper.
- test_build.py buildbot: return the stone and branch remainders before
  the next take. stationbot: return the stone remainder after the
  stage-1 sink; keep the legacy leftover-as-fuel itemact after
  completion, then stow the rest before taking the meat.
- game.rs: the starter-kit comment now matches the actual kit sizes
  (6 branch + 4 stone + 2 string) and states the build headroom.
- NEW WIRE TEST (session-50 request: "the python probe AND a new cargo
  wire test"): `build_flow_sinks_partial_stack_then_completes_after_
  cursor_return` boots the real binary and drives pagina arm -> place
  uimsg (res + on-tile flag) -> plan spawn (OD_RES sdt 0) -> stone sink
  (sdt 1, partial) -> cursor-remainder visible -> inventory drop ->
  branch take -> completion (sdt 0, in-place station conversion).
- Harness growth (tests/common/mod.rs): ArgVal/parse_args typed-list
  mirror, OD_RES res+sdt decoding, RMSG_RESID/WDGMSG/DSTWDG handling,
  building-flow Session helpers (menu_act, send_place, item_by_res,
  inv_take, map_itemact, inv_drop, last_wdgmsg, gob_pos), and the
  legacy-cadence 1 s MAPREQ re-request in pump_until; world_entry now
  WAITS for the nine MAPDATA datagrams (de-flaked under parallel test
  load: raw MAPDATA is lossy UDP, the old immediate assert was latent).

EVIDENCE: 279 cargo tests green (255 unit + 11 proto + 4 wire + 9
world), fmt clean, clippy -D warnings clean, release binary rebuilt;
probes on the release binary: BUILD FLOW OK, STATION FLOW OK, WORLD
ENTRY OK, CATTR ORDER OK, MOVE PROBE OK.

PUSH BLOCKED (GitHub side, not local): both commits (7e8fc40 probe
fix, 6f32474 wire test) are LOCAL on master; every push attempt
returns `remote rejected: Internal Server Error` (5 retries over ~5
minutes, also --no-thin and a throwaway branch: all rejected; ls-remote
and the API work fine, rate limit full). NEXT SESSION MUST: `git push
origin master` first thing; if the local clone is gone, apply
/home/z/my-project/scripts/session51-patches/*.patch (format-patch of
e8360f4..HEAD).

NEXT (handoff):
- The five legacy probes onto hnhlib.py (mechanical; README recipe).
- Session 49 leftovers: game/interact.rs + game/combat.rs split.
- Session 48 leftovers: recipe breadth, feeding transfer (lift), GL
  e2e production walkthrough.
- Windows smoke + GL client e2e when a display host exists (carried).

### Session 51 addendum: push succeeded on retry

The GitHub receive-pack 500 was transient: minutes after the seven
rejections, `git push origin master` succeeded and the remote tip is
now 61a3403 (probe fix 7e8fc40 + wire test 6f32474 + this handoff).
Nothing to re-push; the saved patches in
/home/z/my-project/scripts/session51-patches/ are now redundant.

## 2026-10-07 - Session 52 (type 5: performance)

SESSION TYPE ROTATION LOG: 47=3, 48=3, 49=2, 50=4, 51=3, 52=5 (5 was
the last never-served type besides 0/1; next session should pick 0
(docs hygiene) or 1 (architecture review) before another 2/3/4/5).

GOAL: re-baseline the 1000-bot saturated-world budget on current
master (~30k lines since session 2's 37 ms measurement) and spend it
down. All runs: 2-core/4 GB sandbox, seed 42, --saturated, all bots
walking/fighting (the documented worst case), --perf 5 s reports.

MEASURED (mean tick EWMA, steady state, 1000 sessions):

- base2  (master 29fb4c6, workers=1): 65-67 ms mean, 197 ms max,
  phase_vis 24-52 ms dominant (scan 8-35 + phase B 12-39), mv 5-12.
- w2     (same, --workers 2):         53-57 ms mean - the existing
  rayon scan fan-out already paid; vis_scan_us collapsed to 3-4 ms.
- ab1    (fxhash only, workers=1):    17-50 ms - the id hasher is the
  single biggest win (visibility membership + cell lookups).
- after4 (all three commits):         44-48 ms mean, 0 panics, spawns
  back to the 150-210/window steady state, retract ~0.

WHAT (three behavior-preserving commits + one self-caught fix):

- 51d9dd6 fxhash.rs: multiply-rotate hasher (no deps) for
  SERVER-INTERNAL id containers only (session visible/unacked/sessions,
  VisIndex cells/cell_of_gob/dirty/touched, World gob-id tables,
  player_abroad). Attacker-controlled keys (usernames, client strings)
  keep SipHash - HashDoS surface unchanged. Documented in the module.
- d587ae7 allocation-free vis scan pass: per-session result lists append
  into ONE flat GobId buffer with (start,len) ranges; each rayon task
  reuses one (seg, scratch) pair per partition; scan_visible compacts
  cell buckets in place; Phase B recycles the previous vis_cache Vec
  (take -> clear -> refill). Replaced ~2000-3000 heap allocations/tick
  at the 1000-session scale with ~5. The allocating wrappers are gone;
  tests drive the _into forms (visidx keeps gobs_in_view for the
  cluster subscription path).
- 497c389 --workers defaults to available_parallelism (vertical scaling
  out of the box); 0/malformed = auto; explicit N overrides.
- da5cb21 CRITICAL FIX (self-caught by the load run, never pushed
  broken to users): my first flat-buffer merge sorted task segments by
  first index and appended ranges in concat order, while Phase B reads
  ranges[i] as to_scan[i]'s entry. partition_by_owner does not
  guarantee segment concat == to_scan order, so sessions got OTHER
  sessions' candidate lists -> 11k-37k spawns/window churn, 220-296 ms
  retract ticks, mean tick 140 ms (worse than baseline). Checkout A/B
  (ab1 worktree run) isolated the flat-buffer commit; fix writes ranges
  BY to_scan index (each range self-contained). Checkout-A/B is now the
  documented way to bisect perf regressions in this repo.

ALSO OBSERVED: the movement wire test (movement_click_walks_with_
linstep_progress) is FLAKY under parallel cargo test load (UDP timing;
passed twice isolated + full-suite rerun). Not a master regression;
same class as session 51's world_entry de-flake. A type-4 session
should give the wire suite the same pump_until treatment.

EVIDENCE: 282 cargo tests green (258 unit incl. 3 fxhash + 11 proto +
4 wire + 9 world), fmt clean, clippy -D warnings clean, release binary
rebuilt; python probes on the release binary: WORLD ENTRY OK, CATTR
ORDER OK. Load runs summarized above; scripts/perf_run.sh (session
sandbox) + git worktree A/B recipe recorded here. All commits pushed
to origin/master.

NEXT (handoff):
- The 197-210 ms max_tick spikes are ramp-up-phase (cumulative max
  counter never resets); a per-window max would localize them - cheap
  perf-field addition for the next session.
- Guests still scan O(guests) per full rescan (scan_visible_into's
  guest loop): fine single-node, a landmine at multi-node 10k - add a
  per-cell guest bucket (type 1 or 5).
- move_batch mv phase is now #2 (3-32 ms windows): profile
  batch_move_broadcast next.
- Sandbox-only artifact: scripts/perf_run.sh (outside the repo).
- Carried: five legacy probes onto hnhlib.py; game/interact.rs +
  game/combat.rs split; recipe breadth, feeding transfer; Windows
  smoke + GL client e2e when a display host exists.
