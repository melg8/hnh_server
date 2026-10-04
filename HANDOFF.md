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


## Session end 1791063200 (Windows compile fix)

- Windows release build failed: `tokio::signal::unix` import (main.rs)
  and `set_reuse_port` (net.rs) are Unix-only and were not cfg-gated.
- Fixed: `wait_sigterm` split into `#[cfg(unix)]` (SIGTERM via tokio)
  and `#[cfg(not(unix))]` (pending future; ctrl_c covers Windows).
  `bind_shard_socket` now sets `SO_REUSEPORT` under `#[cfg(unix)]` and
  `SO_REUSEADDR` under `#[cfg(windows)]` (no REUSEPORT on Windows:
  rebind still succeeds, kernel may funnel peers to one socket;
  correctness holds since all shards feed the same game channel).
- Verified: Linux `cargo test` 32 green, clippy -D warnings clean,
  fmt clean; the exact Windows branches compile via a mirror crate
  checked against x86_64-pc-windows-gnu (aws-lc-sys blocks a full
  cross-check in this sandbox, but all deps already built on the
  user's Windows host per their error log — only our two spots were
  broken).
- Next: `git pull` + `windows\start-server.bat` should now build and
  run on Windows. Remaining roadmap unchanged (dirty-cell visibility,
  farming/livestock, party/chat, station crafting).

## Session end 1791063500 (JOGL natives fix)

- Windows client launch failed: `UnsatisfiedLinkError: no jogl in
  java.library.path` (the later `Shutdown in progress` resource error
  was a cascade). JOGL 1.1 loads natives via System.loadLibrary which
  searches java.library.path only; the committed x86-64 DLLs in
  `build/` were never on it.
- Fix: `windows/run-client.bat` passes
  `-Djava.library.path=%CD%\build`, prepends `build` to PATH (dependent
  DLL resolution), and adds `--enable-native-access=ALL-UNNAMED`
  (JDK 21+ restricted-method warning). README Notes document the
  manual-launch variant.
- Verified: all 4 DLLs are git-tracked in `build/` and `dll/64/`;
  `file` confirms PE32+ x86-64, matching x64 JDKs.
- Next: `git pull` + `windows\run-client.bat` should reach the login
  screen and connect to the local server.

---

## Session end 1791066034

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791066083

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


## Session end 1791064200 (resource server hardening)

- Client-side symptom: after successful login, resource fetches from
  http://127.0.0.1:1872/ got Connection refused while auth/game worked;
  the Window.<clinit> NPE on gfx/hud/fbtn was a cascade.
- Root causes covered: (1) res bind failure only logged a background
  error - now fatal at startup with actionable messages (missing gameres
  dir, taken port); (2) transient accept/recv errors (Windows
  WSAECONNRESET class) killed service loops - auth accept, res accept
  and UDP shard recv_from now log-and-continue; (3) new startup
  self-check TCP-probes 1871+1872 on loopback and aborts if unreachable,
  so a half-alive server is impossible.
- res_http unit tests added (200/404/403-traversal over loopback).
- Verified: 35 tests green, clippy -D warnings, fmt clean; live boot
  shows both self-check OK lines, curl serves gfx/hud/fbtn (HTTP 200);
  test_client WORLD ENTRY OK, test_craft EAT FLOW OK. Commit 89b0881.
- Next: user git pull + rebuild via start-server.bat; if the server
  console previously showed "resource server failed"/"resource accept
  error", this commit removes that failure mode.

## Session end 1791064800 (repo hygiene)

- User-side git status noise blocked git pull: the server auto-appends
  a Session end entry to HANDOFF.md on every shutdown (by design), and
  build/haven.jar + docs/javadoc churn with every local JDK build.
- Fixed: untracked build/haven.jar (ant jar), docs/javadoc/ (ant
  javadoc) and save/world.json (runtime persistence committed by
  accident in session 2); gitignore now also covers root /certs/ and
  /data.bin and fixes a broken *.iml + server/target/ line.
- start-server.bat now always runs cargo build (incremental no-op when
  fresh) so a git pull can never launch a stale binary.
- Commits 2af0ddd, 5353e8e. No Rust code changes.

---

## Session end 1791067281

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791067291

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791067297

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791067319

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791067378

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791067433

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791067510

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791067638

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791067721

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791067844

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


## Session end 1791068300 (cwd-independent paths)

- User boot failure: start-server.bat runs from repo root while default
  paths (../gameres, ../save/world.json, certs/) assumed cwd=server/ -
  everything resolved outside the repo and the gameres check aborted.
- Fix: default_repo_dir() resolves gameres/save/certs across all launch
  layouts (cwd=server, cwd=root, exe-relative), converging on repo-root
  copies; explicit --res-dir/--cert/--key and HNH_SAVE_FILE stay
  verbatim; Game::new takes the resolved save path from main.
- Wire-test drift: tests hardcoded play("Player"), inheriting stale
  saved inventory (2 axes, no kit) - craft/eat checks failed through no
  server fault. Both tests now play a per-run character name.
- Verified: 37 tests, clippy -D warnings, fmt clean; boots from three
  cwds converge on identical gameres/save/fep.conf; CRAFT/EAT flows OK.
  Commit 6b09503.
- Next: user git pull; start-server.bat now self-corrects. Remaining
  roadmap unchanged (dirty-cell visibility, farming, party/chat).

---

## Session end 1791069287

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791069316

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


## 2026-10-04 - One command for the user + single-file bug reports

- User report: client crashed after login with "Delayed error in resource
  gfx/hud/fbtn ... Connection refused (1872)". Root cause: the client was
  started before the server answered; the resource loader cached the
  refusal and the deferred error surfaced in Window.<clinit> post-login.
  The server side was healthy (self-check OK at boot, session accepted).
- Fix: run-client.bat is now the single entry point - it auto-starts
  start-server.bat in a new window when ports 1871/1872 do not answer,
  waits for both ports (windows/wait-server.ps1), and only then launches
  Java. The startup race is gone; server-first ordering is enforced by
  the script instead of the user's memory.
- Logging for one-file bug reports: server logs to logs/server.log
  (append across restarts, exe-anchored dir, stdout kept; HNH_REV stamps
  the source rev, set by start-server.bat). Client console output is
  captured to logs/client.log. windows/collect-logs.bat bundles both
  plus environment info into logs/bugreport-<ts>.zip - the user sends
  one file instead of copy-pasting console output.
- Diagnostics: res_http logs every request (peer/path/bytes; 404/403 at
  warn); TLS-handshake-EOF probes demoted to debug (self-check/watchdog
  no longer warn); UDP recv error spam (WSAECONNRESET after a client
  death, once per keepalive) collapsed by a 30 s deduper with a
  suppressed count; new health watchdog probes both TCP listeners every
  10 s and errors loudly if a listener dies mid-session.
- Verified: 38 tests, clippy -D warnings, fmt clean; live run from repo
  root (curl fbtn 200 logged with peer/bytes, 404 path logged, watchdog
  silent over a 12 s window), test_client.py WORLD ENTRY: OK, graceful
  SIGTERM flush.
- Next: user git pull, then windows\run-client.bat is the only command
  needed. Roadmap unchanged (dirty-cell visibility, farming/livestock,
  party/chat, station crafting).

## 2026-10-04 - Fix: collect-logs.ps1 aborted by native stderr (PS 5.1)

- User report: collect-logs.bat died at `java -version` with
  NativeCommandError. Windows PowerShell 5.1 converts native stderr
  lines redirected via 2>&1 into error records, which terminate the
  script under -ErrorAction Stop - and java/cargo print their version
  to stderr by design.
- Fix: $ErrorActionPreference = 'Continue' and every native call runs
  through cmd /c so stderr merges into stdout at the cmd level
  (Get-MergedOutput helper); a missing tool degrades to an empty line
  instead of aborting the bug report. make-gameres.ps1 audited - no
  native calls, safe under Stop.
- Next: user git pull, re-run windows\collect-logs.bat.

## 2026-10-04 - Fix: collect-logs broken for real (git -C mangling, locked logs, false success)

- User report: after the stderr fix the bat still failed twice. Three
  distinct causes in collect-logs.ps1:
  1) `cmd /c "git -C `"$root`" ..."` - PS 5.1 mangles embedded quotes
     when passing arguments to native commands, git received -C with no
     directory ("no directory given for '-C' option") and the info file
     got no git data.
  2) Compress-Archive opened the live server.log with FileShare.Read
     while the server holds it open for writing -> IOException "file is
     used by another process", zip never built. The normal bug-report
     scenario is "server still running", so this had to work.
  3) "Bug report ready" printed unconditionally - a missing/corrupt zip
     was announced as success.
- Rewrite (windows/collect-logs.ps1): no git -C and no quoted cmd
  arguments at all (git runs after Push-Location; the only strings
  reaching cmd are fixed literals like "java -version 2>&1"); every
  logs/*.log is copied into a staging dir with FileShare ReadWrite|Delete
  before zipping, so open files read cleanly while the server writes
  them; the zip is built from the copies; success is only printed after
  verifying the zip exists and is non-empty (exit 0/1, honest FAILED
  message with the raw info file path on failure); repo root resolves
  from -RepoRoot (passed by the bat), then $PSScriptRoot, then cwd; the
  info file now also records OS/PS versions, ant version and whether
  anything actually listens on 1870/1871/1872 ("server is DOWN" hint);
  stale _stage-* leftovers are cleaned at start.
- Verified under real PowerShell 7.4.6 on Linux (first tested -not
  reviewed - version of this script): happy path with a background
  writer actively appending to server.log while the report is built
  (zip contains the live lines), rerun in the same repo, empty logs dir
  (info-only zip), missing repo root (friendly FAILED + exit 1),
  PSScriptRoot fallback without -RepoRoot.
- Next: user git pull, re-run windows\collect-logs.bat (server may keep
  running), send the printed zip.

---

## Session end 1791073363

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


## 2026-10-04 - Fix: client crash root-caused to a hard HTTP dependency + path fixes

- Bug report (now arriving as a single zip - collect-logs works) showed
  the same crash as before: Delayed error gfx/hud/fbtn -> Connection
  refused on http://127.0.0.1:1872/ -> Window.<clinit> NPE. This time
  the server had been up for 12 minutes, watchdog silent (listeners
  healthy every 10 s probe), auth session accepted fine, and the server
  log contained zero res requests: none of the client's HTTP fetches
  ever reached the server, while same-process probes kept succeeding.
- Client-side root cause: the client's base-resource chain is
  custom_res -> ./res -> haven.resdir -> JarSource -> HTTP. The repo's
  res/ lacks gfx/hud/fbtn (it is a custom resource living in
  res/compiled -> gameres), JarSource double-prefixes /res/ so the
  build\res classpath dir never matches, leaving HTTP as the only
  source for half the UI. Any transient HTTP refusal = delayed crash
  after login. The loopback refusal itself (java refused while
  same-host probes passed) remains environmental - likely a WFP/AV
  filter on the user's box - so the fix is to remove the dependency.
- Fix (client): run-client.bat passes -Dhaven.resdir=<repo>/gameres
  (full pack: jar extract + custom overlay) and generates gameres if
  missing. The client now boots fully from disk; HTTP is a fallback
  only. wait-server.ps1 upgraded: 1872 must answer a real HTTP GET for
  gfx/hud/fbtn, not just accept TCP.
- Fix (server): res_http wraps every request in a 30 s timeout so
  silent clients cannot hold tasks/sockets forever (unit test added);
  default_repo_dir now prefers exe-anchored candidates over
  cwd-relative ones - the user's server was loading/saving
  ../save/world.json OUTSIDE the repo (stale E:\work\legacy_hnh\save
  from an older layout beat the repo-root save dir).
- Verified: 39 tests, clippy -D warnings, fmt clean; live boot: fbtn
  HTTP 200, 32/32 parallel fetches OK, save path exe-anchored, wire
  test WORLD ENTRY OK.
- Next: user git pull (they were 2 commits behind - the report came
  from the old collect-logs), then run-client.bat as usual. If the
  crash ever repeats, the client no longer needs 1872 to boot.

---

## Session end 1791076247

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


## 2026-10-04 - Fix: post-login crash (CharWnd cattr NPE) + headless avatar (missing base resources)

Bug report `bugreport-20261004-034010.zip` (rev 7972d7a): "avatar not
shown; client falls right after entering". Two independent defects,
both root-caused from the report's client.log + server.log pairing.

Root cause 1 (crash): `CharWnd$Attr.<init>` does
`ui.sess.glob.cattr.get(nm)` and dereferences the result unchecked.
SlenHud.binded() requests the char sheet (`wdgmsg "chr"` on slen) the
moment the HUD binds - right after entering. The server answered with
the `chr` newwidget while its cattr stream used SHORT internal names
(agi/int/con/per/cha/dex) and carried no expmod, no skills, no beliefs.
`cattr.get("agil")` -> null -> NPE -> client death ~2 s after entering
(exactly the report's minimap GL NPE then fatal CharWnd NPE).

Root cause 2 (avatar/tiles): the pack stores base tilesets NESTED
(gfx/tiles/grass/grass.res) but both the client FileSource and res_http
resolved names FLAT only -> every base tileset (grass/water/moor/...)
404'd/refused -> tileless ground, minimap GL NPE, and gfx/borka/hair +
gfx/borka/head do not exist in the 2009-era pack at all (only named
variants) -> headless avatar.

- Fix (server/game.rs): new `char_attr_snapshot()` builds the exact
  26-name cattr set CharWnd requires (str/agil/intel/cons/perc/csm/dxt/
  psy mapped from internal keys, expmod=100, 11 skills, 6 beliefs).
  Sent in enter_world (replacing the short-name block) and again in
  open_char_sheet immediately before the `chr` newwidget.
- Fix (server/res_http.rs): canonical nested lookup - try
  `<name>.res` then `<name>/<basename>.res` (what the original game's
  resource server did); 200 logs say "(nested)". Unit test added.
- Fix (client/Resource.java): FileSource gets the same nested fallback
  so the client resolves base tilesets from disk without HTTP at all;
  HttpSource retries ConnectException 3x with backoff and logs the
  exact URL on final failure (diagnostics for the unexplained loopback
  refusals from the previous report).
- Fix (pack): res/compiled/gfx/borka/{hair,head}.res added (copies of
  the working variants hair-karin/head-spectacles; ver 6 and 1 >= the
  wired v1; their plalay references resolve to existing subdirs).
  run-client.bat + start-server.bat guards now probe
  gameres/gfx/borka/hair.res and regenerate the pack when absent
  (self-heals stale packs).
- Fix (server/game.rs): tree-chop drop used gfx/invobjs/log which the
  pack lacks entirely -> switched to gfx/invobjs/wood (exists).
- Verified: 40 tests green, clippy -D warnings, fmt clean; live server
  + extended scripts/test_client.py: WORLD ENTRY OK **and**
  CATTR ORDER OK (all 26 names present before the chr widget; the test
  now simulates SlenHud.binded and asserts the ordering); all 12
  previously failing resources (hair/head/10 base tiles) return HTTP
  200 with real bytes; audit script scripts/audit_resources.py
  resolves every referenced resource with pack ver >= wire ver.
- Next for user: `git pull`, restart the server (start-server.bat
  rebuilds), then run-client.bat (the stale gameres regenerates
  automatically). Character sheet must open without a crash and the
  avatar/head/hair must render. If any refusal lines appear in
  client.log ("res http: connect refused for <url>"), send the next
  bugreport - the log now names the exact URL after retries.

---

## Session end 1791079026

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079050

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079087

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079110

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079132

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079170

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079188

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079217

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079243

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079261

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079275

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079312

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079345

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079454

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079472

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079520

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079538

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079560

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079766

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079787

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079829

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079855

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079877

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079888

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079913

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079936

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079945

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791079964

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791080087

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791080280

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791080435

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791080778

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791080810

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791080851

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791080889

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791080907

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791080933

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791080995

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791081058

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791081079

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791081209

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791081343

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791081429

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791081466

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791081487

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791081529

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791081549

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791081561

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791081664

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


## 2026-10-04 - Crop farming implemented (plow/plant/grow/harvest) + two critical wire fixes

Leaf-1.1.1 of the .unlazy/session13 plan (tree depth 99 requested; honest
decomposition is depth 3, stated in the plan). Commit f1e6f38.

- Implemented full crop farming per farming-and-plants.md: farm.rs crop
  registry (8 crops), quality roll, soil quality; plow pagina ->
  furrowed tile (PLOWED tile id, grid overrides persisted in save v2,
  MAPDATA re-send to holders); take -> cursor -> itemact planting;
  staged growth tick (OD_RES sdt updates); harvest flower menu with
  per-stage yields; tilth decay; crops/tilth/overrides persisted
  (save v1 files stay readable). HNH_CROP_TIME_SCALE env (default 60)
  scales legacy hours; tests use 1e7.
- CRITICAL fix 1 (interactive clicks): on_map_click read modflags as
  button and used the screen coord c0 as the world target - every map
  click only worked when Shift was held and moved the player to
  screen-space coords. Now button = first int, target = second coord
  (mc). Same fix for mapview itemact.
- CRITICAL fix 2 (the reported missing avatar): update_visibility
  double-inserted gobs into the session visible set, so stream_spawn
  skipped the spawn block - the client NEVER received its own gob
  (no avatar layers). Regression test added
  (player_gob_is_streamed_with_buddy).
- Also fixed: item "take" was routed to the inv widget (client sends it
  from the item widget); cursor stacks were lost on log-out (now
  returned to inventory before persist); crop_at registry cleanup on
  harvest.
- Starter kit now includes 5 Wheat + 5 Carrot seeds so the farming loop
  is playable immediately; plow pagina pushed to every session.
- Verified: 47 cargo tests green, clippy -D warnings clean, fmt clean;
  wire e2e server/scripts/test_farming.py: FARMING FLOW: OK (auto-starts
  a fast-clock isolated server, plants/grows/harvests, asserts yields).
- NEXT (handed off, leaf-1.1.2/1.1.3 WAITING in .unlazy/session13):
  party + chat relay (wire e2e test_party_chat.py planned), skill/LP
  economy gating planting (farming skill cost table), then building
  placement/stations and the dirty-cell visibility optimization.
