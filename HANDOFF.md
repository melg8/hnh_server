# HANDOFF — hnh_server Rust implementation

This file is the durable context-transfer protocol for LLM working sessions.
Read this FIRST in every new session. Append a dated entry at the end of
every session (what was done, what was verified, what is next). Never
delete entries.

## How to continue work in the next session

1. `git pull` the repo; read this file top to bottom.
2. Build and test: `cd server && cargo test` (must be green) and
   `cargo build --release`.
3. Run: `cd server && ./target/release/hnh-server --seed 42`
   (ports: 1871/tcp auth TLS, 1870/udp game, 1872/tcp resources HTTP;
   `../gameres/` must exist — it is generated from `lib/haven-res.jar`,
   see "Resource pack" below).
4. Verify end-to-end: `python3 server/scripts/test_client.py testuser`
   must print `WORLD ENTRY: OK`.
5. Load check: `./target/release/hnh-server --seed 42 --bots 300 --perf`
   — perf logs should show `tick_us` well below 100000.
6. Client: `ant jar` (JDK 21 + Ant 1.10), run
   `java -cp build/haven.jar:lib/* haven.MainFrame` — it connects to
   127.0.0.1 automatically (auth 1871, game 1870, resources 1872).
   `-Dhaven.autoplay=Player` skips charselect. `-Dhaven.pinnedcert`
   restores legacy certificate pinning.

## Architecture (as implemented)

- `server/crates/hnh-proto` — byte-exact wire protocol: `MessageBuf`
  (LE primitives, NUL strings, typed lists), MSG_*/RMSG_*/OD_* consts,
  reliability layer (`RelSender`/`RelReceiver`: 16-bit seq per direction,
  cumulative ACK, legacy backoff table 80/200/620/2000 ms, hold-back
  buffer), zlib MAPDATA assembly + MTU fragmentation. Auth frame codec.
- `server/crates/hnh-world` — `JavaRandom` (bit-exact 48-bit LCG port,
  verified against a real JDK 21), seed-fixed value-noise worldgen
  (tile_at is a pure function of seed+x+y; grids generate independently =
  grid-shard ready), `GridStore` (10 KB per 100x100 grid, LRU eviction,
  copy-on-write tile mutation).
- `server/crates/hnh-server` — binary:
  - `auth.rs`: TLS auth server (rustls, dev cert in `server/certs/`),
    AuthClient frame protocol, SHA-256 password digests, single-use
    cookies (5 min TTL), reusable tokens (30 d TTL). Dev policy: any
    username/password auto-provisions.
  - `net.rs`: UDP 1870. MSG_SESS handshake (PVER check, cookie consume,
    idempotent re-accept). Per-session driver tasks own reliability state;
    two outbound channels: reliable RMSG stream + raw MAPDATA/OBJDATA
    datagrams (matching the legacy split). MAPREQ/OBJACK/WDGMSG in.
  - `state.rs`: SoA gob storage (pos/res/frame/alive/kind/hp/speed/mv
    columns, generational ids packed into i32), Species table with
    per-species hp/speed/loot, tile speed rules, 10 Hz tick.
  - `game.rs`: single-owner game task. Bootstrap sequence per
    docs/mechanics/network/session-lifecycle.md (RESID before charlist,
    TILES before MAPDATA, HUD -> mapview -> GLOBLOB -> CATTR -> PAGINAE).
    Visibility manager (500 subtile radius, spawn/update/retract,
    ack-gated retransmit buffers per session). Movement (LINBEG/LINSTEP,
    per-tile speed, server-validated paths), wildlife AI (wander, flee,
    chase, attack), combat (auto-attack, damage = 5*str/10, HP quarters
    via OD_HEALTH, death -> loot drops + LP), vitals (hp/energy/stamina,
    starvation, respawn), inventory widgets (inv/item/xfer/drop), GLOBLOB
    (8 real hours per in-game day, 365-day year).
  - `res_http.rs`: HTTP file server for `gameres/` (GET `<name>.res`).
  - `bots.rs`: in-process load-test bots through the REAL UDP path.
  - `handoff.rs`: this file's appender.

## Resource pack

`gameres/` = `lib/haven-res.jar` (6301 .res files) overlaid with
`res/compiled/` (fork resources). Regenerate with:
```bash
unzip -o -q lib/haven-res.jar 'res/*' -d /tmp/hx && cp -rn /tmp/hx/res/* gameres/ \
  && cp -r res/compiled/* gameres/
```
`gameres/` is NOT in git (repo size); it is reproducible from the repo.

## Verified (session 2)

- Unit tests: protocol roundtrips, reliability in-order/out-of-order/dup,
  MAPDATA shape (reinflate == 10000 tiles + plots), fragmentation,
  JavaRandom == JDK 21 reference values, mkrandoom == JDK, worldgen
  determinism/variety, SHA-256 vectors, bootstrap RESID ordering,
  persistence roundtrip + seed-mismatch reset, frv uimsg encoding,
  predator engagement in saturated worlds. 25 tests green.
- Integration: `scripts/test_client.py` performs TLS auth -> MSG_SESS ->
  charlist -> play -> full bootstrap -> 9x MAPREQ -> MAPDATA (9 fragments)
  -> OBJDATA stream (player + trees + animals) -> walk click. Prints
  `WORLD ENTRY: OK`.
- Load (session 2): 1000/1000 async bot sessions in a saturated world
  (3300+ animals) with live fights; steady-state tick ~37 ms vs 100 ms
  budget (max 76 ms) at worst-case single-area crowding; RSS ~190 MB.
  Bots run as tokio tasks (thread-per-bot hit the 748-thread ulimit at
  547 sessions in session 1's design).
- Sharding: `--shards N` opens N SO_REUSEPORT UDP sockets; kernel
  4-tuple hash pins each peer to one shard; `WORLD ENTRY: OK` passes
  through a shard.

## Architecture changes (session 2)

- `net.rs`: free fn `spawn(game_tx, shards)` binds N UDP sockets with
  SO_REUSEPORT via socket2; each shard owns a private recv loop and
  session table; accept log carries `shard=` for occupancy histograms.
- `persist.rs`: `SaveStore` keeps per-character snapshots in
  `../save/world.json` (version 1, atomic tmp+rename writes). Snapshots
  update on session close + 30 s autosave; SIGINT/SIGTERM follow a
  graceful shutdown path (`Cmd::Shutdown`) that flushes the file. Seed
  mismatch or corrupt file starts fresh characters. `HNH_SAVE_FILE`
  env var overrides the path.
- `fight.rs`: Fightview (frv) openings combat. One frv widget per
  session created on first engagement (destroyed when relations empty).
  Per-opponent relations carry balance -5..+5, intensity, two-bit give,
  IP both sides, offence/defence bars scaled x100. uimsg: new/del/upd/
  updod/cur/atkc/offdef; client wdgmsg click/give handled. Damage lands
  only through openings (defence broken), scaled by str and advantage.
- `bots.rs`: fully async (tokio tasks, no threads). Bots spread over
  home tile areas near the spawn, request their 3x3 grids with raw
  MAPREQ datagrams, walk, and get engaged by predators under
  `--saturated` (aggro radius widened to 900 subtiles there).
- `auth.rs`: self-signed dev certificate generated with rcgen at boot
  when certs/authsrv.key.pem is absent (fresh-clone one-command start).
- `main.rs`: `--shards N` flag; graceful SIGTERM/SIGINT shutdown.

## Known gaps / next steps (priority order)

1. **Crafting**: paginae actions and the Makewindow flow
   (`act("craft", ...)` -> make widget -> `make 0/1`) are stubbed
   (`on_menu_action` logs); fep.conf/curio.conf parsing not done.
2. **Grid-owner partitioning**: UDP shards are done; the game task is
   still the single simulation owner. Next scale-out step is splitting
   visibility/AI into grid-owner tasks (SoA layout ready, see state.rs).
3. **Farming/livestock**: crop growth stages via sdt + OD_RES are
   designed (see encode_gob_block) but no planting flow yet.
4. **Combat polish**: frv relations stream balance/intensity only
   passively (no move selection UI: attack/maneuver resources are not
   settable yet); IP accrues per swing, no move costs.
5. **Headless client GL**: the client runs on a real display; under
   Xvfb JOGL 1.1 needs Linux natives that this repo does not ship
   (Windows .dll only; Debian headless JRE also lacks libawt_xawt --
   use a full Temurin JDK for client smoke tests). Wire-protocol
   correctness is covered by scripts/test_client.py.
6. **flavor objects**: client-side flavor replication needs the tileset
   flavobjs tables + randoom parity; server gobs already cover clickable
   objects, so this is cosmetic-only for now.
7. **Party/buffs/chat**: wire builders exist (`wdg::` helpers), gameplay
   wiring pending.

## Session log

---
### Session 1 (2026-10-03, UTC+8)
- Read AGENTS.md + all docs/mechanics; implemented the full server per
  "Architecture" above; verified via unit + integration tests.
- Client: trustAll SSL, localhost defaults, autoplay hook, jar builds
  with JDK 21 + Ant 1.10.15.
- Load: 724 bots / 19 ms tick. Numbers above.
- Commits: "Add Rust server...", "Remove build artifacts...",
  "Client: local dev server support".

---

## Session end 1791053403

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791053436

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791053595

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055108

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055108

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055198

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055265

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055376

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055525

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055637

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055726

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055845

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055940

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791056136

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791056246

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791056388

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791056611

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791057259

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791057444

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791057583

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791057610

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791057624

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791057639

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791057653

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791057761

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---
### Session 2 (2026-10-04, UTC+8)
- One-command start fixed: self-signed dev cert generated at boot
  (rcgen); the removed private key no longer wedges the auth server.
- Zero-warning build: clippy --all-targets -D warnings clean; cargo fmt
  applied repo-wide.
- SO_REUSEPORT UDP shard sockets (--shards N): kernel 4-tuple hash pins
  peers to shards; private recv loop + session table per shard.
- Character persistence: ../save/world.json (atomic writes, seed-bound,
  autosave 30 s + graceful SIGINT/SIGTERM shutdown flush); restore on
  login verified end-to-end.
- Fightview (frv) openings combat implemented per combat-system.md:
  widget lifecycle, relations, upd/updod/offdef/cur/atkc wire messages,
  client click/give handled; animals fight back; kill cycle proven by
  unit test (stationary_player_kills_predator). Combat tempo constants
  (OFF_REGEN 625/tick, ATKC 8) tuned so swings land between client
  movement bursts; real players drive them normally.
- Bots rewritten as tokio tasks (thread-per-bot hit the 748-thread
  ulimit at 547 sessions); 1000/1000 sessions in a saturated world,
  live predator fights, steady tick ~40 ms / 100 ms budget, RSS ~300 MB.
- Bot home areas fixed to the spawn grid (was 100x off: tile vs grid
  coords) so MAPREQ population and wildlife overlap the bots.
- Verified: G1 one-command entry OK; G3 persistence roundtrip; G4 four
  shards + entry OK; G5 1000 bots/100 ms budget/no tick overrun; 26
  unit tests green; clippy -D warnings clean; ant jar builds with
  Temurin 21 (client GUI still needs JOGL Linux natives, see gaps).
- Commits: pushed to master through 9b4381f+ (see git log).

---

## Session end 1791061355

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791061402

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791061432

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791061470

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791061513

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791061565

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791061631

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791061662

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791061678

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

### Session 3 (2026-10-04, UTC+8)

- Crafting implemented per crafting-and-building.md: `craft.rs` module
  (fep.conf parser mirroring Config.loadFEP, recipe registry, roast map,
  FEP accumulator state), makewindow choreography
  (act("craft",id) -> make widget + pop with (wire,count) pairs
  terminated by -1, make 0/1 batch loop, lowest-ql-first consumption,
  loftar weighted-average quality with attribute softcap). Recipes:
  stone axe (branch+stone -> axe), dynamic roast (any raw meat ->
  roasted variant via fep-name mapping). Paginae pushed at login
  (paginae/craft/axe, paginae/craft/roastmeat).
- Food/FEP per food-and-fep.md: item iact -> FlowerMenu("sm") with
  "Eat" -> cl 0 -> energy fill, HHP hard-pool heal, FEP grant scaled by
  sqrt(q/10) in tenths, weighted attribute draw at cap = max base attr,
  cattr push. `food` uimsg on chr (id, tenths, RGBA color triples)
  verified against CharWnd.FoodMeter.update. Per-species meat labels
  match fep.conf keys (Cow->Beef, Deer->Raw Deer Meat, ...); Wolf has
  no entry and grants nothing (no invented numbers).
- Parallel tick phases: tick_animals split into a pure intent pass
  (rayon par_chunks over SoA, --workers N) and serial apply;
  update_visibility split into parallel scan_visible + serial apply.
  Deterministic (tick,slot) splitmix hash replaces the shared RNG in
  the decision pass. Grid-region buckets are the seam for future
  cross-process grid owners.
- Load (this box: 2 cores / 4 GB cgroup): --bots 1000 -> tick 35-40 ms
  vs 100 ms budget, RSS ~194 MB, all sessions stable. --saturated
  --bots 600 -> 10890 fights, tick 13-15 ms. NOTE: 1000 bots +
  --saturated SIGKILLs on this 4 GB sandbox (memory cgroup), not on
  larger hosts (session 2 ran 1000 saturated bots at ~300 MB).
- scripts/test_craft.py: wire-level end-to-end craft+eat verification
  (auth -> entry -> starter kit -> act(craft,axe) -> make+pop -> make 0
  -> item iact -> Eat -> food uimsg). Both OK against a fresh boot.
- Starter kit for fresh characters (2 branch, 2 stone, 1 beef) so the
  craft/eat loop works immediately; dev policy, documented.
- windows/: build-server.bat, start-server.bat (build + gameres
  generation via make-gameres.ps1 + save dir + run), run-client.bat
  (ant jar + java), loadtest.bat (600 bots saturated + --perf), README.
- fep.conf resolved through path candidates (cwd + exe-relative) so the
  binary boots from any working directory; gitignore now covers the
  generated gameres/ and boot-generated dev certificate.
- Session end 1791063100: verify with `cargo test` (32 green),
  `python3 server/scripts/test_client.py testuser` (WORLD ENTRY: OK),
  `python3 server/scripts/test_craft.py crafttest` (CRAFT/EAT: OK).

---

## Session end 1791062978

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.

