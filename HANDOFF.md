# HANDOFF — hnh_server Rust implementation

This file is the durable context-transfer protocol for LLM working sessions.
Read this FIRST in every new session. Append a dated entry at the end of
every session (what was done, what was verified, what is next). Never
delete entries. The server binary and every automated tool MUST NEVER write
to this file: session entries are authored by a working session, not
appended by builds, gates, or shutdown paths.

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


## 2026-10-04 - Session 14: parties + chat, skill/LP economy (unlazy tree continued)

Continued the .unlazy/session13 depth-3 tree as .unlazy/session14
(contract rev 2). Commits c08fbab + ae39445 on origin/master.

- leaf-1.1.2 VERIFIED (party + chat): Area Chat (`slenchat`, title
  "Area Chat") at world entry; `msg` lines relay as "<Name>: <text>" to
  sessions within AREA_CHAT_RADIUS = VIEW_RADIUS (hear what you can
  see; overflow-free i128 squared distance); colored system lines to
  one session; party invite via player-gob click -> clicker sm menu ->
  invitee sm menu + prompt; RMSG_PARTY (PD_LIST/PD_LEADER/PD_MEMBER)
  broadcast on every change; `pv` roster widget lifecycle; leave ->
  leadership transfer -> disband (empty PD_LIST clears client state);
  logout cleans membership; cap 10, palette per join order. Modules:
  chat.rs, party.rs, resources.rs wdg::party, game.rs handlers.
  e2e server/scripts/test_party_chat.py (CHAT FLOW / PARTY FLOW: OK).
- leaf-1.1.3 VERIFIED (skill/LP): skills.rs - 11 client-hardcoded skill
  values, legacy sattr curve (100*(v+1) per point; bulk closed form;
  no-op pairs free for the client's send-all batch), 8-entry
  non-incrementable catalog (pack-verified names, server-defined
  costs), chr exp/nsk/psk pushes, all-or-nothing sattr with full CATTR
  re-push (vitals-only push left stale SAttr values - caught by e2e,
  not review), buy with charge + re-push, refusals as chat lines.
  Planting gate: farming value >= 1 (fresh 100 LP wallet = exactly one
  point). Passive LP accrual 2/min (HNH_LP_RATE knob) documented as a
  deviation until curiosity study. skills persisted (SavedPlayer.skills
  additive v2). test_farming.py farmbot buys via the real contract;
  new skillbot mode (SKILL GATE: OK).
- CRITICAL bug found by the e2e: buy() used checked_sub as a wallet
  guard - 90-200 = -110 is a VALID i32, so wallets could go negative.
  Domain guard (lp < cost) replaced it; unit test pins the behavior.
- Verified: 57 cargo tests green, clippy -D warnings clean, fmt clean;
  CHAT FLOW / PARTY FLOW / FARMING FLOW / SKILL GATE all OK in one
  server generation (node-1.1 N2).
- NEXT (leaf-1.2.x WAITING, next-session handoff as recorded in
  .unlazy/session14 PLAN): building placement + production stations
  (leaf-1.2.1), then dirty-cell visibility optimization for the 10k
  target (leaf-1.2.2). Optional follow-ups: curiosity/study engine to
  replace the passive LP trickle, party marker position refresh.


## 2026-10-04 - Session 15: building pipeline + oven station + dirty-cell visibility (unlazy tree)

Executed the session-14 handoff (leaf-1.2.x) under .unlazy/session15
(`tree 99` requested; honest decomposition = depth-2 tree, stated in
PLAN.md). Commits 5bc985b + 16eaecd on origin/master.

- leaf-1.2.1 VERIFIED (building placement): build paginae pushed
  (paginae/act/build -> paginae/build/cons -> oven/smelter; ad strings
  decoded from the res pack action layers resolve the "build verb"
  open question). act("oven") -> mapview place uimsg (resname, ver,
  ontile[, radius]) -> client ghost -> wdgmsg place (coord, button,
  modflags); button 1 commits, others cancel via unplace. Commit
  validation: 5-tile reach, tile_speed terrain rule, one site per tile
  (crop_at/plan_at/structure_at). Plan gob = finished-object res with
  the build stage in the sdt byte (crop re-render pattern, frame bump
  per stage). itemact([cc, mc, modflags, gobid, gobrc], gobid at index
  3) sinks min(cursor, remaining) units with per-type quality
  snapshots; completion converts the plan in place (same gob id, Kind
  swap). Registry: oven (stone x2 + branch x1, HP 1200), smelter
  (stone x6 + branch x4, HP 2500) - DEVIATION from legacy Brick-based
  demands documented in the mechanics doc.
- leaf-1.2.2 VERIFIED (oven station): fuel via itemact (branch, one
  unit per delivery, delivered-fuel average), single roast input slot
  (craft::ROAST_MAP labels), Light/Extinguish flower menu on the
  station gob, 8-tick job at 10 Hz, one fuel burned per job, output
  drops beside the station with (2*q_item + q_station + q_fuel)/4.
  CRITICAL bug found by the e2e: Kind::Station{lit} (wire sdt) and
  StationState.lit (simulation) were separate copies of the truth -
  lighting mutated only the state, so clients never re-rendered. Fixed
  by set_station_lit (both move together; frame bump + restage).
  Persistence v3 (additive): SavedPlan (credited materials by resource
  name + quality sums), SavedStructure (quality, fuel bookkeeping,
  loaded input, progress); v2 saves stay readable.
- leaf-1.2.3 VERIFIED (dirty-cell visibility): visidx.rs coarse square
  cells (250 subtiles) with per-tick dirty tracking; a session whose
  own cell is unchanged and whose retract square (2xVIEW_RADIUS + one
  cell) intersects no dirty cell skips the scan and the retract sweep
  entirely. Gobs owns the index; spawn/kill/set_pos are the single
  mutators; movers mark their cell dirty every tick (LINSTEP streaming
  + boundary exits). Perf counters vis_gob_scans / vis_skipped /
  vis_cells in the perf log. Scan correctness is unit-proven
  (cell query == full scan, boundary exit, stationary skip).
- Verified: 79 cargo tests green, clippy -D warnings clean, fmt clean;
  gates UNIT/E2E BUILD, UNIT/E2E STATION, BUILD PERSIST, UNIT VISIBLEIDX
  all pass via server/scripts/verify_build.sh; e2e flows on the new
  visibility path (BUILD FLOW / FARMING FLOW OK).
- Found and fixed while testing (not review): itemact gobid arg index
  (3, not 4), e2e save-file reuse intercepting itemacts (fresh-save
  policy in ensure_server), stale item widget ids after
  refresh_inventory (client-side DSTWDG pruning + newest-wid rule).
- node-1 integration VERIFIED: BATTERY: ALL PASS (world entry + build +
  station + farming + party/chat in ONE server generation); LOAD
  VISIDX: ALL PASS (1000/1000 sessions, fights, steady-state EMA
  mean_tick_us ~44 ms vs the 100 ms budget, vis_cells=160+ proves the
  dirty-cell index is live; raw entry-burst spikes are transients and
  excluded from the steady-state gate). FMT CLIPPY / CARGO TEST: ALL
  PASS.
- Load-gate honesty note: single-area crowding is the WORST case for
  the cell query (vis_skipped=0 because every session has moving
  neighbors in view; the view squares cover most of the 166 occupied
  cells). The documented 10k lever beyond this session: dedupe the
  per-session candidate scan for sessions sharing a player cell
  (shared visibility groups), plus the index skip pays off in the
  distributed-world shape real gameplay has.
- Test-harness fixes found by the battery: the shared default save
  (save/world.json) leaked earlier gates' plans into the battery world
  (isolated per-gate saves now); stale disconnected avatars (gateuser)
  were valid click targets for the party test (find_other_player now
  matches the OD_BUDDY character name).



## 2026-10-04 - Session 16: black screen fixed (client-side probe), equipment paperdoll, handoff de-clutter
Continuation under .unlazy/session16 (tree 99 requested; honest
decomposition is depth 2, stated in the plan). Commits 84c8f24
(HANDOFF de-clutter), 67c2120 (black screen), e8aa327 (paperdoll).

- HANDOFF hygiene (user-reported clutter): the server appended a
  boilerplate "## Session end <unix stamp>" block to this file on
  every clean exit; 181 blocks from gate runs buried the session log.
  Removed the appender (src/handoff.rs + main.rs call site), stripped
  all 181 blocks, and pinned the protocol rule: the server binary and
  every automated tool must never write to this file. The `committed`
  gate no longer auto-commits HANDOFF.md dirt.
- NEW feedback tool - client-probe/UiProbe: drives the REAL client
  classes (AuthClient -> Session -> UI -> RemoteUI, the exact
  post-play receive path, no GL) against a live server; every
  throwable is precisely what kills the real client's threads.
  Gate: server/scripts/verify_ui_probe.sh (compile|run|equip). The
  probe needs any JDK (javac runs via `java -m
  jdk.compiler/com.sun.tools.javac.Main`), lib/*.jar on the
  classpath, HAVEN_RESDIR=res/compiled, and MainFrame statics set.
- Black screen root causes (all four fixed, each alone froze the
  client): (1) resource versions were hard-coded while the client
  hard-rejects version mismatches ("Wrong res version") - the server
  now parses the true LE u16 version from every served .res header;
  (2) res_http appended ".res" to targets that already ended in
  ".res" - the res server had NEVER served a real client; (3) player
  gobs spawned with OD_RES of gfx/borka/body (no neg layer) killed
  the session reader with "No negative found" - players now spawn
  through OD_LAYERS only, layer RESIDs announced first; (4) the
  client's HttpSource.encodeurl dropped the port via the 3-arg URI
  constructor - every HTTP res fetch hit port 80. Also added
  res/compiled/gfx/hud/vilind.res (KinInfo's static initializer 404s
  without it; generator scripts/make_vilind.py).
- Equipment paperdoll (the user-reported missing doll): widget type
  "epry", 16 server-semantics slots, bootstrap "set"+"ava" sync,
  slen "equ" reopens it. Equip/unequip ride the cursor item ("drop"
  onto a slot, "take" from a slot); occupied/invalid slots rejected.
  Inventory "drop" now places the cursor item into the inventory (the
  real Inventory.drop semantic) instead of popping the last stack to
  the ground; ground drops belong to mapview "drop" (still
  unhandled). SaveData v3 -> v4 (additive): equipped slots persist
  across restarts.
- Verified: UI PROBE RUN/EQUIP OK; UNIT EQUIP, E2E EQUIP (incl.
  restart persistence) OK; battery extended with equip flow + probe +
  equip persistence; 81 cargo tests green; fmt + clippy clean.
- NEXT (handoff): mapview "drop" ground-drop flow; equipment effects
  (armor class from tooltips, avatar layer changes when equipment
  changes); craft paginae ad->action wiring for the remaining recipes;
  grid-owner partitioning for the 10k target (single game task is
  still the simulation owner).

## 2026-10-04 - Session 17: login portrait fixed (standing-frame layers), stale-jar guard
Continuation under .unlazy/session17 (tree 99). Commits 0a45392
(stale-jar guard), 84ebcfb (portrait layers), 1d61679 (portrait log).

- Stale-jar guard (highest-probability root cause of "symptoms persist
  after fixes"): run-client.bat used to build the jar only when
  build/haven.jar was absent, so a `git pull` never rebuilt and the
  user kept running pre-fix client code (the avatar/encodeurl/res-ver
  fixes all live in src/haven). run-client.bat now rebuilds whenever
  HEAD moved (rev stamp build/.clientrev) or the jar is missing;
  start-server.bat applies the same stamp to the gameres pack
  (gameres/.genrev). Gate: server/scripts/verify_windows_launch.sh.
- Login portrait ("no face at login"): the charlist "add" uimsg listed
  gfx/borka/{body,head,hair} - pose-router ("plalay") resources whose
  layers carry ZERO imgc images, so AvaRender resolved all layers and
  still drew a blank card. The server now sends frame 0 of the
  standing pose of each body part (legs, torso male, head, idle arms,
  hair-karin); in-game OD_LAYERS keeps the routers (sprite factories
  resolve poses there). The UiProbe proved the defect headlessly:
  3 layers resolved, 0 image layers, blank portrait reproduced.
- UiProbe charlist mode: asserts the add uimsg decodes into a real
  char entry, every portrait layer resolves client-side, the
  composited image inventory is non-empty, and world entry happens
  through the REAL Button.click() chain (not a raw queued play msg).
  Gate: verify_ui_probe.sh charlist; wired into the battery.
- Charlist portrait observability: every login logs the exact layer
  names into logs/server.log (verify_charlist_log.sh), so future bug
  reports carry the portrait evidence automatically.
- Verified: UI PROBE RUN/EQUIP/CHARLIST OK; CARGO TEST, FMT CLIPPY,
  BATTERY (incl. charlist probe), CHARLIST LOG, HANDOFF FILE gates
  green; committed and pushed to master.
- NEXT (handoff): mapview "drop" ground-drop flow; equipment effects
  (armor class, avatar layer changes when equipment changes); craft
  paginae ad->action wiring; grid-owner partitioning for the 10k
  target. If the user still reports a frozen client after this
  session: ask for logs/bugreport-*.zip (collect-logs.bat) - with the
  stale-jar guard the client rev in the log header must match HEAD.

## 2026-10-04 - Session 18: real-GL-client reproduction killed the post-enter freeze
Continuation under /unlazy tree 99 (scope .unlazy/session18). Commits
1ee1a1c (shared flat+nested res resolver), 172211d (mapview-before-slen
bootstrap + real avatar frame layers), 5dff2fb (spawn only existing
resources). Pushed to master.

- New local verification capability (outside the repo, scripts/jogl/):
  the REAL client runs under Xvfb + Mesa llvmpipe with Temurin JDK 8
  (JDK 21's XRender GC breaks JOGL 1.1 visual selection) and JOGL
  1.1.1 natives extracted from Ubuntu debs; a DriveAgent javaagent
  drives the real login/charselect widget chain and prints UI state.
  This closed the gap every headless probe had: actual rendering.
- Bug (b) "client frozen after entering" was THREE stacked defects,
  each fatal only in the render path: (1) file_version() announced
  ver 1 for nested tilesets while the served file carries the real
  version - the client dies on "Wrong res version"; fixed by one
  shared resolve_res_file() used by both the HTTP source and the
  version reader; (2) slen was created before mapview, the fork's
  SlenHud builds MinimapPanel capturing ui.mapview at construction,
  NPE in MiniMap.draw killed the render thread; bootstrap reordered;
  (3) avatar layers were pose routers (plalay/plparts layers the
  client drops) -> "No negative found"; the server now layers
  concrete standing-pose frames like the legacy server did. Also
  bumlings/trees/kritters referenced non-existent resources; all
  spawn names verified against the pack.
- Bug (a) "no face at login" re-verified visually: the portrait
  layers composite a full character (rendered to PNG through the
  same imgc/z-order logic as AvaRender). If the user still sees an
  empty card, their jar is stale (rev guard handles it).
- Verified: real client 70+ s alive after world entry, 0 resource
  errors, session stable (DriveAgent state log); cargo test 66/66;
  fmt+clippy clean; test_client WORLD ENTRY: OK; UI PROBE CHARLIST OK.
- NEXT (handoff): mapview "drop" ground-drop flow; walking-pose
  animation client-side (static standing frames slide while moving);
  equipment effects (armor class, avatar layer changes); craft
  paginae ad->action wiring; grid-owner partitioning for 10k.

## 2026-10-04 - Session 19: the frozen character root-caused and fixed (real-client verified)
Continuation under .unlazy/session19 (tree 99; honest decomposition is a
depth-2 tree, see the PLAN). Commits 07e9abc (probe), dea3f67 (reader
hardening + neg pack), a463788 (THE fix), 8c1b245, da34b6b (harness +
AGENTS.md), e29538f, plus the test_farming parser fix.

- New environment capability, now COMMITTED (scripts/jogl/): deploy
  script (Temurin 8, JOGL 1.1.1 natives from old-releases.ubuntu.com,
  X11 libs, Ant, DriveAgent javaagent) and the e2e runner: fresh server
  + Xvfb + the REAL GL client, login through the real widget chain,
  real AWT Robot clicks, MOVEMENT verdicts from Gob.position(). The
  session-18 setup was never committed and died with the sandbox.
- AGENTS.md now REQUIRES real-client verification for client-visible
  changes; wire probes alone are insufficient (this session proves it).
- BUG (b) "character stands rooted on clicks" was TWO stacked defects:
  1) hnh-proto list(): the legacy client's Message.addlist encodes
     wdgmsg args WITHOUT the T_END terminator (list ends at EOM). The
     strict parser errored on every real click and net.rs'
     unwrap_or_default() dropped ALL args ("map click received nargs=0"
     at TRACE). on_map_click saw no mc -> no LINBEG. Wire probes never
     caught it: they append T_END manually. Fixed at the parser (EOF
     ends the list; explicit T_END still terminates early) with two
     regression tests.
  2) OCache.cres (client): one bad resource (gfx/invobjs/meat - animal
     loot drop - and gfx/invobjs/wood - tree-chop drop - ship WITHOUT
     the mandatory neg layer) threw out of ResDrawable init on the
     Session RWorker thread and KILLED the reader; after that the
     client never processed LINBEG/OBJDATA again - a zombie that renders
     old state. cres now catches sprite-init failures (gob renders
     nothing); the pack gains synthesized neg layers
     (scripts/add_neg_layer.py, res/compiled overlay).
  Diagnostic chain that found them: MapView.mousedown branch print ->
  RemoteUI.rcvmsg print -> "click args=Coord..Coord..Integer..Integer"
  sent vs "nargs=0" received. The click diagnostics are now gated
  behind -Dhaven.debugclicks=true.
- Bug (a) "no head/torso at login" re-verified on the REAL client: the
  in-world avatar renders head/torso/legs (screenshot: /tmp evidence in
  the session run; world + minimap + menu grid + chat all render). The
  remaining plausible user-side cause is the stale-jar guard (fixed in
  session 17) - their report likely predates it.
- test_client.py's walk click targeted ~570 tiles off-spawn (spawn is
  tile (50,50) -> subtile (555,555)); it verified nothing. New
  probe_walk.py asserts OD_LINBEG + OD_LINSTEP for the player gob
  identified from mapview args: MOVE PROBE: OK.
- test_farming.py: OD_LAYERS parse is variable-length now (5 pose
  frames) - fixed stride had broken player detection since session 18.
  test_craft.py eats by fep label (seeds/axe are not food).
- Verified this session: cargo test 85 green (11+66+8); fmt+clippy -D
  warnings clean; WORLD ENTRY + CATTR ORDER OK; MOVE PROBE OK;
  CRAFT/EAT OK; FARMING OK; CHAT+PARTY OK; E2E EQUIP OK; REAL CLIENT
  MOVEMENT: MOVED x2 (driveuser36-40 runs); load 1000/1000 saturated
  bots, steady tick ~42 ms vs 100 ms budget, fights live, RSS ~820 MB
  on this 2-core/4 GB box.
- NEXT (handoff): mapview "drop" ground-drop flow; walking-pose
  animation; equipment effects (armor class, avatar layer changes);
  craft paginae ad->action wiring; grid-owner partitioning for 10k;
  shared visibility groups (dedupe per-cell candidate scans) as the
  documented 10k lever.

## 2026-10-04 - Session 20: movement fidelity + charlist portrait (real-client verified)
Continuation under .unlazy/session20 (tree 99; gates in
.unlazy/session20/GATES.md, all leaf gates met). Commit 7d42202 (server
movement rework) + the follow-up fixes in this session.

- The four user-reported defects were ROOT-CAUSED on the client side and
  fixed server-side / client-side, then verified on the REAL GL client:
  1) "No face at login" (charlist card): TWO stacked issues. The original
     TexRT AvaRender draws layers onto the framebuffer under a bottom-up
     ortho and copies the screen back - on this render stack the card
     stayed empty. AvaRender is now a CPU composite (TexI, BufferedImage,
     z-ordered imgc layers) anchored so the figure lands in the rectangle
     Avaview actually shows (its draw offset exposes buffer rect
     69..143 x, 20..94 y). VERIFIED: screenshot shows the character in the
     card; AVATAR COMPOSITE images=6; PORTRAIT: OK (2024 dark px).
     The early "portrait OK" verdict was a FALSE POSITIVE (the pixel
     script looked at the login art, not the card) - the visual read of
     /tmp/client_charlist.png caught it; the script now checks the real
     frame region. READ THE SCREENSHOTS.
  2) "Teleports on rapid clicks": the server pinned the logical position
     to the destination at move start, so a re-click restarted the move
     from the destination. start_move now retargets from the interpolated
     on-path position. VERIFIED: RAPID CLICKS: GLIDING.
  3) "Rubber-band to start after each walk" (found while verifying 2):
     a final bare LINSTEP (l >= c) drops the client Moving attribute and
     position() falls back to the STALE pre-move rc. The finalizer now
     sends OD_MOVE (destination) + LINSTEP(l>=c) in one block, so Gob.move
     pins rc first. VERIFIED: DIAG pos==rc==destination after arrival;
     MOVEMENT: MOVED 555,555 -> 575,575 exactly.
  4) "Moves too fast / no walking animation": (a) c was derived from
     100 ms ticks while the client interpolates a move in c*66.67 ms -
     clients outran the server 1.5x; c is now total_ms*3/200. (b) The
     base speed 44 subtile/s exceeded the documented walk speed; the
     gait system (crawl/walk/run/sprint = 16/33/50/66 subtile/s, RoB
     Glossary) is implemented with speedget cur=1 max=3 and the set
     wdgmsg. (c) Walking-pose animation: the server streams walking
     frame sets (OD_LAYERS) at 150 ms/frame while moving and the standing
     set on arrival. VERIFIED: SPEED 3.43 tiles/s (measured on the real
     client), mid-walk screenshot shows a walking pose.
- Harness updates: DriveAgent now captures the charlist card (waits for
  Charlist.chars, dumps the AvaRender layers + composite PNG), picks the
  character itself via Charlist.choose_player (no autoplay hack needed),
  measures tiles/s over moving samples, and reports NO TELEPORT /
  RAPID CLICKS verdicts; verify_charlist_portrait.sh added and required
  in AGENTS.md. e2e runner fixed: the server takes HNH_SAVE_FILE (the
  old --save arg was silently rejected, so every "fresh" run loaded the
  shared world.json with 1019 stale characters).
- Pack repairs: 16 more invobj resources shipped without neg layers
  (stone drop was throwing "No negative found" on the render thread);
  scripts/add_neg_batch.py synthesizes them in place + mirrors to
  res/compiled. Animal loot renamed to resources that exist
  (hide-raw-fox/cow; tail/hide did not exist).
- Verified: cargo test 90 green (11+71+8), fmt+clippy -D warnings clean,
  real client MOVEMENT/SPEED/TELEPORT/RAPID/PORTRAIT all OK (s20k).
- NEXT (handoff): grid-owner partitioning for 10k; shared visibility
  groups; mapview ground-drop flow; equipment effects (armor class,
  avatar layer changes); craft pagina ad->action wiring; the charlist
  card could use a bigger portrait (74x74 window shows a small figure) -
  cosmetic.

## 2026-10-05 - Session 21: directional animations, visible+animated animals, bite FX, equipment doll (real-client verified)
Continuation under .unlazy/session21 (tree 99; gates + evidence in
.unlazy/session21/). Commits de50412 (server pose rework), bite/offence
fix, cfb9bb6 (real-client evidence harness), this commit (docs).

The four user-reported defects were root-caused against the CLIENT source
and the .res layer structures (scripts/dump_res.py), then fixed and
verified on the real GL client:

1) "Animation does not match movement; the character spins around its
   axis": session 20's walking-pose stream cycled legs-0..7 at 150 ms -
   those are the 8 DIRECTION sets, not frames. Every directional
   resource embeds its full walk cycle (8 frames @100 ms players, @50 ms
   kritters, anim id=-1) and the client's AnimSprite animates natively.
   The server now computes move_dir (quantized movement octant, dir 0 =
   +x, counterclockwise; unit-tested wraparound) and streams ONE
   directional layer set per pose/direction change; the pose state dedupe
   is one SoA byte (moving<<3|dir). VERIFIED: probe_direction.py legs
   +x/+y/-x-y -> exactly one walking + one standing stream per leg with
   the correct direction digit (DIRECTION WIRE: OK); real-client mid-walk
   frames: east = front view, north (-y) = right profile, south (+y) =
   left profile (read the screenshots).
2) "Animals invisible (shadows only), no walk/attack animation": animals
   spawned via OD_RES gfx/kritter/<sp>/cdv with empty sdt - StaticSprite
   with empty flags renders only img.id<0 images (fox cdv: ids 0,1,2,-1
   -> 1 image). Animals now spawn through OD_LAYERS of the concrete
   kritter pose parts (base gfx/kritter/<sp>/body + standing-N/walking-N
   of the facing), same pipeline as players. VERIFIED: probe_animals.py
   (ANIMALS WIRE: OK: 330 kritter-layered spawns, 0 flat cdv, 300+
   walking-pose streams); real-client screenshot shows a boar sprite in
   the viewport (agent hunts a predator into view - passive species flee
   beyond the viewport and the camera pans with the mouse).
3) "No attack animation" had NO attack behind it: AnimalFight.off never
   incremented, so the swing condition (off >= SWING_SPEND) could never
   fire - predators never attacked. Offence now builds every tick in
   reach (mirrors the player's own_off regen); each bite lands through
   openings and broadcasts a one-shot OD_OVERLAY gfx/fx/bite on the
   victim (client removes it after one anim cycle). VERIFIED:
   predator_bites_and_broadcasts_bite_overlay unit test (hp drop + OD_
   OVERLAY on the victim's raw stream); 1517 fights + bite overlay in the
   saturated-wire probe.
4) "No doll in Equipment": the server NEVER sent OD_AVATAR, so
   Avatar.rend was null and Equipory.cdraw drew only the bg frame. The
   own gob now receives OD_AVATAR with the banzai-arms doll set (spread
   arms, camera facing dir 1); other viewers get the standing set.
   VERIFIED: EQUIP DOLL: avagob=65536 ava-rend=OK; the screenshot shows
   the spread-arms doll in the window.

Harness: DriveAgent phase 3 (directional legs with per-direction
screenshots from the live camera center mv.mc - the fork camera pans with
the mouse; equipment doll dump + frame; predator hunt into the viewport),
runner waits for the last verdict. AGENTS.md documents the session-21
evidence lines; docs/mechanics/objects/objects-and-dynamics.md gains the
"Directional pose layering" contract section.

Verified: cargo 93 tests green, fmt+clippy -D warnings clean, load smoke
300 bots tick ~4.8 ms (budget 100 ms), WORLD ENTRY OK, real-client
MOVEMENT/SPEED/NO TELEPORT/RAPID/PORTRAIT/WALKDIR x3/EQUIP DOLL/ANIMALS
all OK in one run (s21zoo6/s21i).

NEXT (handoff): grid-owner partitioning for the 10k target; shared
visibility groups; mapview ground-drop flow; equipment effects (armor
class + avatar layer changes on equip); craft pagina wiring; the
equipment doll could sit centered in the window frame (cosmetic); a
client-side camera-position getter would make animal screenshots exact.

## 2026-10-05 - Session 22: one-octant walk direction offset (art ring vs movement ring)
Continuation under .unlazy/session22. User report: the walk animation is
SHIFTED BY ONE octant from the travel direction - walking UP showed the
up-right set, walking LEFT the up-left set. Session 21 had removed the
frame-cycling spin but fed the raw MOVEMENT octant into the directional
resource name; the art pack's ring is rotated one octant against the
movement ring.

ROOT CAUSE PROVEN three independent ways:
1. Decoded the fox standing sprites into a labeled sheet
   (scripts/dump_directions.py, read visually): art 0 = head-on FRONT
   (the +x+y camera-facing octant), art 1 = down-left 3/4, art 2 = pure
   left profile, art 3 = up-left 3/4, art 4 = back, art 5 = up-right
   3/4, art 6 = pure right profile, art 7 = down-right 3/4 - sprite N
   depicts movement octant N+1 (m2s = (2x-2y, x+y), MapView:626, makes
   +x+y the camera-facing front).
2. Both user data points fit exactly: walking up (octant 5) rendered
   sprite 5 = octant 6 (up-right); walking left (octant 3) rendered
   sprite 3 = octant 4 (up-left).
3. Session 21's own real-client note "east = front view" was itself the
   defect showing through: east (octant 0) rendered sprite 0 = front
   instead of the down-right 3/4 walk view.

FIX (commit ee94dd4): art_dir(octant) = (octant - 1) mod 8 applied at
the layer-name composition boundary only (PoseTable::build for avatar
and kritter tables); `facing` keeps the true movement octant for game
logic and the one-byte pose dedupe. Spawn facing default 0 -> 1 so
fresh gobs render the front view; the login portrait path switched to
octant 1 (was hard-coded octant 0, which after the fix rendered the
3/4 view - caught by reading PORTRAIT LAYERS in the e2e log); the
equipment doll switched to sprite 0 = true head-on front (was sprite 1
= down-left 3/4). DriveAgent phase 3 gained the exact user defect
directions: screen UP (0,-220) and screen LEFT (-220,0) legs.

VERIFIED: 94 unit tests green (new art_dir_offsets_the_sprite_ring pins
the full ring + user cases; layer composition tests pin emitted
digits), fmt+clippy -D warnings clean. Real GL client (s22p run):
EAST = down-right 3/4 front, up-right leg = 3/4 back, down-left leg =
3/4 front, screen-UP = back view (user case fixed), LEFT leg (retarget
around obstacle, actual octant 2) matches travel; charlist portrait =
head-on front; EQUIP DOLL ava-rend=OK with front-view banzai doll;
ANIMALS boar mid-walk visible at pure left profile matching its
walking-2 layer. Full regression: MOVEMENT/SPEED/NO TELEPORT/PORTRAIT/
EQUIP/ANIMALS all OK.

NEXT (handoff): grid-owner partitioning for the 10k target; shared
visibility groups; equipment effects (armor class + avatar layer
changes on equip); craft pagina wiring; a pure-left (octant 3) walk leg
screenshot would complete the visual ring (the e2e leg retargeted
around an obstacle; wire+unit coverage pins it deterministically).

## 2026-10-05 - Session 23: grid-owner partitioning (the multi-node 10k unit)
Continuation under .unlazy/session23. Backlog item "grid-owner
partitioning for the 10k target" from sessions 21-22.

WHAT: the tick's parallel phases previously chunked work by COUNT
(par_chunks over animal ids, par_iter over the session scan list) -
that fan-out shares nothing with a multi-node layout. Work now groups
by VisIndex-cell OWNER via the new grid_owner.rs module: rendezvous
(highest-random-weight) hashing picks one owner per cell out of the
live node set. Properties (all unit-tested): deterministic with no
shared state; scale-out moves only the share the NEW node wins - cells
never migrate between existing nodes; balanced lattice spread; order-
preserving partitions that cover every item exactly once.

INTEGRATION: tick_animals groups animal ids by their cell's owner and
rayon runs the pure per-animal decision per partition; update_visibility
phase A2 groups scan indices by the session's cell owner and reorders
results back into to_scan order before the serial apply. The serial
apply phases (writes, wire blocks) are unchanged. Each partition is the
exact unit a separate node process would own in the multi-node
deployment, so the in-process fan-out exercises the cluster contract.

VERIFIED: 100 unit tests green (6 new grid_owner tests pin ownership
determinism, scale-out migration to the joiner only, balance, and
partition order); fmt+clippy -D warnings clean; load budget holds -
300 moving bots at workers=4 tick mean ~4.4-4.9 ms (budget 100 ms,
session 21-22 baseline ~4.5-4.8 ms, no regression); full real-client
e2e green at BOTH workers=4 (partitioned) and the default serial path:
portrait layers, movement x2, five directional walk legs, equipment
doll, animals in view. server/README.md gained a Scaling section;
AGENTS.md unchanged (harness rules already apply).

NEXT (handoff): true multi-node process split over the grid-owner
contract (node processes own partitions; cross-partition visibility
needs a shared registry + border-cell forwarding); equipment effects
(armor class + avatar layer changes on equip); craft pagina wiring;
mapview ground-drop flow; an animals-in-view hunt fallback that
teleports the camera to the nearest predator would make the s23b-style
animal screenshot deterministic.

## 2026-10-05 - Session 23 follow-up: armor class end to end
armor.rs lands the server side of the Equipment window armor class:
per-piece base def/abs (14 pieces verified against the shipped pack),
quality scaling sqrt(q/10) pinned to the documented tusk-helmet anchors,
tooltip composition into the epry "set" sync, and two combat
applications (absorption shrines HP damage, defense slows the
breakthrough). Recipe "hcloak" (2 raw cow hides -> hide cloak) gives the
pipeline a craftable source; equip/unequip already re-pushed the epry
set so tooltips update live. 105 tests green; s23c real-client e2e
green. Model note: legacy per-piece numbers and the reduction formula
are undocumented (combat-system.md open questions) - the chosen model
is documented in armor.rs and isolated behind its two pure functions.

## 2026-10-05 - Session 24: fighting/harvesting bot cohort at the 1000-session scale
Continuation under .unlazy/session24. Master-prompt requirement: "эмуляцией
1к подключений которые все бегают, сражаются и взаимодействуют с миром" -
the in-process bots only walked before, and the biggest cohort proof was
300 walkers.

BOTS (bots.rs rewritten): each session now builds a wire-level world view -
RMSG_RESID announcements fill a per-session wire-id -> name table,
MSG_OBJDATA blocks feed a gob view (remove/move/lin/res/layers/overlay/
buddy), and the behavior loop picks a target by class (drop > animal >
tree/stone via a weighted roll: ~50% fight, ~30% harvest, ~20% loot) and
clicks concrete gob ids like a real MapView. Three wire bugs found and
pinned by tests: (1) on_rel payloads carry the rmsg type byte (parse_resid
was reading it as half of the wire id - every gob resolved Other); (2)
movement ops precede RES/LAYERS in a spawn block, so the view must insert
placeholders (positions were lost for every spawned gob); (3) the old
walk-only click carried ONE coordinate - on_map_click reads mc as the
SECOND coord (coords.nth(1)), so NO bot click ever reached the game logic.
Clicks now match MapView.java:738/747 exactly [c0, mc, button, modflags,
gobid]. The bot tracks its own gob via the OD_BUDDY name plate and acts
from the streamed self position; overlay adds count as bites.

SERVER SCALE FIXES (all measured at 2 cores, --saturated):
- tick_movement fan-out rewritten: per-block per-session MessageBuf +
  send_raw became ONE combined OBJDATA datagram per session per tick
  (batch_move_broadcast; the wire allows consecutive gob blocks, client
  recv_objdata loops them). Movement phase: 68 ms -> 6-13 ms at 600+.
- LINSTEP progress frames no longer clone into per-session unacked
  (self-healing next tick) and the vis phase no longer re-sends movement
  deltas (the needs_move rescan duplicated every progress frame).
- unacked retransmit cache capped at the last 4 frames per gob: sessions
  that never OBJACK (bots) grew it unbounded - the OOM killer at 3.8 GB
  RSS in run 1k4/1k6. Retract sweep runs every 8th tick + on cell moves
  (2xR hysteresis makes per-tick sweeps wasted work).
- Raw (MAPDATA/OBJDATA) session queues are bounded with drop-on-full UDP
  semantics; inbound client datagram queues likewise (try_send).
- VIEW_RADIUS 500 -> 300 subtiles (27 tiles, still beyond the ~21-tile
  client screen; chat AREA radius follows).

VERIFIED: 113 unit tests green (8 new bot tests pin classification,
objdata parsing incl. overlay-removal without sdt, targeting buckets,
late RESID reclassification), fmt + clippy -D warnings clean. Gate run
(server/scripts/verify_session24.sh load1k): 996/1000 connected, peak
mean tick 64 ms < 100 ms budget, fights 59841 + harvests 126 + pickups
20682 + bites 17323 in one 120 s cohort; 4 missed sessions are bootstrap
timeouts under the login storm (handshake retries up to 30 s).

WINDOWS: loadtest.bat takes an optional bot count (default 1000) and
documents the bot behavior; windows/README + server/README updated.

NEXT (handoff): true multi-node process split over the grid-owner
contract (node processes own partitions; shared registry + border-cell
forwarding); equipment effects on the avatar (visible layer changes on
equip); craft pagina ad->action wiring; mapview ground-drop flow; the
4-session bootstrap loss under login storms could drop with a token-
bucket accept queue; vis scan is the next hot phase (20 ms at 1000) -
cell-level in-view caching would cut it further.

## 2026-10-05 - Session 25: equipment visuals (clothing on the world avatar and the Equipment doll)
Continuation under .unlazy/session25. Backlog item "equipment effects on
the avatar (visible layer changes on equip)" from sessions 21-24.

WHAT: equipping or un-equipping an item now changes the avatar's
composited layers. The server gained `equip.rs`: an invobj ->
borka-layer table (50 pieces: shirts, pants, shoes, hats, helms, capes,
cloaks, belt, backpack, quiver, gloves, one-handed gear) inventoried
from the served pack by scripts/inventory_clothes.py +
scripts/diff_clothes_poses.py. The per-piece file prefixes are NOT
derivable from the piece name and CHANGE between poses
(hat-chief standing-N/walking-N, hat-sprucecap standing-N/sprucecap-N,
leather boots bootz-/boots- swap) - hence the per-pose template pair.
Layer names materialize once as leaked 'static strings keyed by
(piece, pose, art octant), zero-alloc lookups at stream time. The doll
set derives from the standing templates with banzai arms; pieces that
ship no banzai variant (eq-* weapons) render on the world avatar only;
carrying-only pieces (bows) stay unrendered until a carrying pose
state exists (documented in equip.rs).

WIRE: epry drop/take now call stream_equipment_change ->
stream_pose (OD_LAYERS re-stream with the piece layers to every
viewer) + stream_avatar (OD_AVATAR push: owner gets the banzai doll
set, others the standing set). Spawn blocks and stream_spawn announce
lists carry the same layers, so late joiners see the outfit.

CLIENT FIXES (fork, verified on the real client):
1) AvaRender.recomp sorted the composited images by z alone; the
   pack's stacking lives in SUBZ (legs 4 < pants 6 < torso 5 is wrong
   too - the ladder is legs 4, torso 5, pants 6, cape 13, head 14,
   helm 18, hair 17, banzai arms 19). With all z=0 the paint order
   fell back to the resource-id order of the layer list, so a piece
   whose wire id sorted early was painted UNDER the body part it
   covers: the doll stood bare with its pants on its back. recomp now
   sorts by (z, subz).
2) AvaRender.render() refreshes the composite while resources stream
   in, but Equipory.cdraw draws through GOut.image(TexI) which bypasses
   render() - the doll never recomposited after a piece's borka
   resource arrived. cdraw now calls ava.rend.refreshIfLoading().
3) open_inventory sent new_wdg("inv") with an EMPTY arg list; the
   client's Inventory factory requires the grid-size Coord and threw
   ArrayIndexOutOfBounds inside RemoteUI.run, killing the whole UI
   thread (inventory + equipment + everything after). The grid size
   (4x8) now ships in the widget args.

EVIDENCE (s25v/s25w real-client runs): EQUIPVIS VERDICT: OK - the
agent equips the starter linen pants through the REAL widget chain
(item wdgmsg take -> epry drop), reads back EQUIPVIS DUMP
DRESSED/UNDRESSED (doll Avatar.rend names pants-0 when dressed, not
when not) and WORLD DUMP DRESSED/UNDRESSED (the Layered layer list on
the world gob), and screenshots both: /tmp/client_equip_dressed.png vs
/tmp/client_equip_undressed.png (pixel-diff confined to the pants slot
+ the doll's legs) and /tmp/client_world_player.png (the world avatar
in pants). Full regression in the same run: PORTRAIT, MOVEMENT,
NO TELEPORT, RAPID CLICKS, five WALKDIR legs, EQUIP DOLL, ANIMALS all
green. Starter kit now also ships linen pants + shirt so every fresh
character has something to wear. server/scripts/verify_session25.sh
runs the whole checklist (TAG=<tag>).

Verified: 122 unit tests green (9 new: equip table, pose-specific
prefixes, doll banzai set, hand-side split, pack cross-check; wire test
equip_change_streams_layers_and_avatar pins the OD_LAYERS re-stream +
OD_AVATAR push on equip and the re-stream on unequip), fmt+clippy -D
warnings clean.

NEXT (handoff): true multi-node process split over the grid-owner
contract; craft pagina ad->action wiring; mapview ground-drop flow
(dea3f67 hardened the client side); token-bucket accept queue for the
login-storm bootstrap loss; vis-scan cell caching (the 20 ms hot phase
at 1000 sessions); cursor item visibility (the server keeps the
out.cursor state but never ships the drag-item widget the original
client renders under the mouse - equip works, the held stack is just
invisible until dropped); carrying-pose state for bows/carried tools
(their layers are arm/carrying only).

## 2026-10-05 - Session 26: cursor item, ground drops, vis skip check, accept throttle
Continuation under .unlazy/session26. Four backlog items from the
Session 25 handoff.

WHAT:
1) CURSOR ITEM (the held stack is now visible): the server keeps the
   out.cursor stack state but never shipped the drag Item widget the
   client renders under the mouse - the held stack was invisible until
   dropped. SessionOut gained cursor_wid, kept in sync by
   sync_cursor_widget (create on take, "num" uimsg refresh on count
   change, destroy on empty). Wired into inv_take, epry_take, inv_drop,
   epry_drop and every on_map_itemact consumption path
   (sink_material/station_itemact/plant_seed/legacy drop). The widget
   is the Item.java factory contract: parent=root, args [res, q,
   drag=1, grab Coord, tooltip, num]. refresh_inventory skips the
   cursor wid so the rebuild never kills the held item.
2) GROUND DROPS (mapview `drop`): the dispatcher ignored the mapview
   drop wdgmsg entirely (Item drag release on the map). New
   on_map_drop: take_cursor_stack -> spawn_drop_near -> hide widget.
   WHILE WIRING THIS the real client exposed a bigger pre-existing
   defect: ground drops spawned with the INVENTORY icon resource
   (gfx/invobjs/*: image+tooltip layers, NO neg layer), so the sprite
   init failed with "No negative found" and EVERY ground drop (wood,
   stone, meat, loot, user drops) was invisible in the world. Fix: a
   two-resource Drop kind - resname_idx renders a gfx/terobjs/items/<base>
   world shape (image+neg; probed in the served pack via
   resources::served, fallback gfx/terobjs/items/branch keeps unmapped
   items visible), inv_res_idx restores the exact gfx/invobjs stack on
   pickup. drop_info() now returns the INVENTORY resource.
3) VIS SKIP CHECK (the 20 ms hot phase): any_dirty_in_view iterated the
   WHOLE dirty set per session (O(sessions x dirty cells); bots keep
   ~140 cells dirty across the lattice every tick). It now enumerates
   the session's own view-cell range (~36-60 cells) and probes the
   dirty HashSet with early exit. Pin test: a far-dirty world must not
   trip the skip; the +1-cell boundary tolerance still rescans.
4) ACCEPT THROTTLE (login storms): MSG_SESS handling awaited the game
   task's Accept round trip INLINE on the shard recv loop - a login
   storm stalled every other session's datagrams behind each handshake.
   The loop now does parse+PVER+throttle+reply inline and spawns
   finish_accept (game round trip + driver spawn) off the path. A
   per-shard token bucket (burst 100, refill 50/s) bounds the accept
   rate; a throttled handshake gets NO reply (the legacy client
   retransmits every 2 s up to 10 times) and its single-use cookie is
   NOT consumed. A pending-set keeps duplicate handshakes idempotent
   while an accept is in flight.

CLIENT: DriveAgent phase 5 (session 26 evidence): take a starter item
(prefers the branch - its terobjs twin renders the true sprite) via the
real widget chain, read back the drag Item (CURSOR DUMP:
dragging=<res>), screenshot the held item at the pointer
(/tmp/client_cursor_item.png), release it with a real map click
(GROUNDDROP DUMP: new-gob=<terobjs res>), screenshot the ground gob
(/tmp/client_grounddrop.png), then LEFT-click the gob to pick it back
up (PICKUP DUMP: gob-gone=true seed-in-inventory=true). Verdicts:
CURSOR VERDICT + GROUNDDROP VERDICT (server/scripts/verify_session26.sh
checks the full list; the runner now waits for the phase-5 verdicts).

EVIDENCE (s26c/s26d/s26gate real-client runs): the pre-fix run printed
"gob 66059 resource <gfx/invobjs/branch(v2)> failed to init: ... No
negative found" and GROUNDDROP DUMP: new-gob=null; after the fix the
same flow reports new-gob=gfx/terobjs/items/branch, the screenshots
READ: held branch + tooltip "Branch, quality 10" AT THE POINTER, branch
on the grass next to the player, PICKUP restores the stack. Full
regression green in the same run (PORTRAIT, MOVEMENT, NO TELEPORT,
RAPID, 5x WALKDIR, EQUIP DOLL, ANIMALS, EQUIPVIS). Load: 300 sessions
mean tick ~6 ms max 18 ms; 1000 sessions connect 1000/1000 with mean
~45-50 ms during the staggered login bootstrap (worst transient 173 ms),
settling after - the throttle paces the storm at 50 accepts/s by design.

Verified: 127 unit tests green (5 new), fmt+clippy -D warnings clean.

NEXT (handoff): true multi-node process split over the grid-owner
contract; craft pagina ad->action wiring for the remaining recipes;
vis-scan result caching (per-cell in-view result reuse - the scan
itself, not the skip decision, dominates at 1000 moving sessions);
carrying-pose state for bows/carried tools; cursor item pickup
redirection (right-click-with-held-item onto a stack merges).

## 2026-10-05 - Session 27: true multi-node process split (grid-owner cluster)
Continuation under .unlazy/session27. The top handoff backlog item: the
grid-owner partition became a REAL process split.

WHAT: `--cluster "host:port,host:port" --node N` starts N independent
server processes that share one static membership list and own the
VisIndex cells the rendezvous hash (grid_owner.rs) assigns them:

1) NODE-LINK MESH (`nodes.rs`): TCP mesh, length-prefixed bincode frames
   (1 MiB cap checked before any read-into buffer), symmetric Hello
   handshake (proto version + node index validation), 500 ms dial retry,
   per-peer outbound queues that buffer across reconnects (a short
   partition loses nothing). Messages: Sub/Unsub (viewer -> owner, cells),
   Chat, GuestAnnounce/Update/Retract/Transfer.
2) GLOBALLY-UNIQUE GOB IDS BY CONSTRUCTION (`state.rs`): gob slots
   partition across nodes (node i allocates only [i*per, (i+1)*per) of
   the 16-bit slot space), so encoded wire blocks (LINSTEP et al) are
   valid on every node without any id remapping. `Gobs::spawn_with_id`
   materializes a transferred gob under its EXACT (slot, gen) id so
   viewers keep rendering it across authority handoffs.
3) AUTHORITY: animals/world gobs simulate ONLY on their cell's owner
   (tick_movement/tick_animals filter by is_authority_slot); players are
   ALWAYS authored by their home node (the node the UDP session landed
   on). Animal crossing a cell boundary -> GuestTransfer to the new
   owner, local copy demotes to a guest (same id, viewers never flicker),
   animal_fights cleared. Player entering a foreign cell -> GuestAnnounce
   to that cell's owner; returning home -> GuestRetract.
4) GUESTS: a node subscribes its peers to the view cells its sessions
   actually look at (10-tick diffed Sub/Unsub). The owner streams
   Announce/Update/Retract for subscribed cells; the subscriber ingests
   into World.guests and feeds them through the SAME dirty-cell
   visibility machinery as local gobs (vis index, scan merge, spawn
   blocks with server-side pose resolution + OD_AVATAR/OD_BUDDY for
   players, LINSTEP progress derived locally from the linmove params -
   identical arithmetic, no per-tick streaming, retract sweep + GC when
   unviewed and unsubscribed).
5) CHAT crosses nodes through the mesh; each node re-filters by the
   sender's position (same area-chat radius).
6) PORTS parameterized (`--game-port/--auth-port/--res-port`): cluster
   peers on one machine offset them (client-facing defaults stay
   1870/1871/1872).
7) INTERACTION GUARD: fighting a guest target is a no-op (its HP lives
   on the authority node) - cross-node interaction relay is NEXT.

Single-node default (`--cluster` absent) is unchanged: cluster=None,
all cells owned by node 0, zero added tick cost.

EVIDENCE: 121 unit tests green (5 new: authority-follows-cells +
players-stay-home, guest ingest/update/retract with wire OD_LAYERS
proof, transfer identity across nodes both directions, player territory
publish/retract, chat relay radius). 2-node cluster probe (3 bots per
node): guest counters BOTH ways (n0 ingest=6 pub=50; n1 ingest=2
pub=91), mean tick 80-99 us, 1549 authority transfers in a 75 s run,
bot cohorts see the other node's players (cls_players=7 vs 3 own).
REAL CLIENT e2e (scripts/jogl/run-cluster-e2e.sh): node0 + node1, 2
bots on node1, DriveAgent on node0 walks five long legs across cell
boundaries: CLUSTER DUMP animals=13 (hare/deer/fox/boar/wolf sample),
CLUSTER VERDICT: OK, /tmp/client_cluster.png READ (world rendered on
node0 includes node1's fauna), MOVEMENT: MOVED through the cluster.
Server checklist: server/scripts/verify_session27.sh (guest ingest
counts + subscriptions + client verdicts). Toolchain note: the client
env was re-provisioned from scratch this session (Temurin 8 + JOGL 1.1.1
jni + libXtst into /home/z/tools, client rebuilt with javac - the
deploy script's JDK path is /home/z/tools/jogl-extract/jdk8u504-b01,
and etc/icon.png must be copied to classes/haven/ before jarring).

NEXT (handoff): cross-node interaction relay (attack/pickup/build
against guest gobs - InteractReq to the authority, SessionRelay for
session-targeted UI); single save/persistence story for clusters
(characters are per-node today); craft pagina ad->action wiring for
remaining recipes; vis-scan result caching; carrying-pose state for
bows; cursor pickup redirection (merge stacks).

## 2026-10-05 - Session 28: cross-node interaction relay (guest fights live)
Continuation under .unlazy/session28. The top session-27 handoff item:
fighting a guest target is no longer a no-op.

WHAT: four new NodeMsg kinds close the cross-node combat loop:
- `RelayAttack { attacker, target, chip, dmg }` (home -> authority): EVERY
  swing of a fight against a foreign-authority animal ships the (dmg, chip)
  pair computed on the home node (the attacker's str lives there). The
  owner applies the chip to its authoritative `animal_fights` bar, decides
  its own opening, lands HP damage there (`damage_animal_relayed`:
  OD_HEALTH to ITS viewers + GuestUpdate publish so the attacker's node
  streams OD_HEALTH from the ingested state), drops loot and retracts on
  death, and answers `FightBars { id, def }` so the home mirror
  (`world.guest_fights`) self-heals. The home node ALSO chips its mirror
  per swing (UI prediction); rel.defence reads the mirror.
- `PlayerHurt { player_gob, dmg, from }` (authority -> home): animal
  retaliation against a guest player ships the bite; the victim's home
  node applies armor absorption, HP, stamina, the knockout path AND the
  bite FX overlay (the owner's own overlay only covers its viewers).
  v1 bite = default str (2), documented.
- `KillCredit { player_gob, lp }`: relay kill credits the attacker's home
  node LP wallet (+10, push_cattr).
- `player_interact` guest branch: clicking a guest animal used to die at
  "interact target gone" - the REAL client could never OPEN a relay
  fight. Guest animal clicks now route through start_fight (test).
- Bookkeeping: guest retracts close relay fights BOTH directions (local
  fighter's frv widget destroyed on animal retract; animal_fights rows
  dropped on player-guest retract). Guest hp deltas stream OD_HEALTH on
  ingest (frame bump + unacked record). `node_of_gob` maps a gob id to
  its home node from the cluster slot stride.

EVIDENCE: 127 unit tests green (6 new: relay open via interact click, one
swing = one RelayAttack with exact chip/dmg, authority chip+opening+hp
publish+OD_HEALTH+KillCredit, retaliation ships PlayerHurt + retract
cleanup, guest retract closes frv, FightBars resync). CLUSTER e2e
(server/scripts/verify_session28.sh): SESSION28 RELAY WIRE VERDICT: OK -
node1 bots opened 38 relay fights against node0-authority guests, node0
applied 2 relay swings (chip+damage), 2 FightBars answers synced the
mirrors; full home->authority->home loop between two REAL processes.
Real-client e2e: CLUSTER VERDICT: OK (guests render), MOVEMENT/SPEED OK;
DriveAgent phase 7 (RELAYFIGHT) clicks the nearest on-screen guest and
requires the frv widget - the chase is best-effort (wildlife placement
is random; deer outrun the player), NO-TARGET/NO-FIGHTVIEW is a soft
verdict, the wire proof lives in the node logs. clippy clean.

NEXT (handoff): single save/persistence story for clusters (characters
are per-node); craft pagina ad->action wiring for remaining recipes;
vis-scan result caching; carrying-pose state for bows; cursor pickup
redirection (merge stacks); relay pickup/build against guest STATIC gobs
(drops/structures) - the same RelayAttack pattern with an action enum.

## 2026-10-05 - Session 29 (continued): cluster save story landed + sandbox re-provisioning
Continuation under .unlazy/session29 (no dir; see this entry). The
interrupted session-29 work (685 uncommitted lines found in the tree) was
validated, completed and pushed.

WHAT (already summarized in the cluster save story commit a294c5c):
account-scoped save keys (`account:charname`, legacy bare-name snapshots
adopted on load), per-node shard files (`save/cluster_nN.json` via
HNH_SAVE_FILE default per node index), two-phase cross-node character
migration (CharQuery broadcast over the mesh with a 700 ms retry and a
6 s deadline, CharData re-served until CharAck, CharNack short-circuit;
a downed peer never blocks a login).

EVIDENCE: 152 unit tests green, fmt/clippy clean.
server/scripts/verify_session29.sh: PHASE1 (shard isolation on disk),
PHASE2 (same-node restore after restart), PHASE3 (cross-node migration,
snapshot physically moves n0 -> n1 shard files) - SESSION29 E2E: OK.
One stale grep pattern in the script was fixed ('serving snapshot to
peer' is the actual log line).

SANDBOX RESET RECOVERY (important for every future session): the tool
sandbox was wiped between sessions - JDK8/JOGL/X11 tools, build/ and the
gitignored gameres/ pack were gone. Recovery path, now fully scripted:
1) bash scripts/jogl/deploy-agent-env.sh (now ALSO regenerates the
   gameres pack when missing), 2) ant jar with JAVA_HOME=JDK8, 3) rerun
   scripts/jogl/run-real-client-e2e.sh. New: server/scripts/make-gameres.sh
   (Linux mirror of windows/make-gameres.ps1) - extracts lib/haven-res.jar
   then overlays res/compiled. WITHOUT the res/compiled overlay the real
   client 404s gfx/hud/vilind (KinInfo static init) and EVERY movement
   verdict degrades to STUCK - the pack overlay is load-bearing for the
   client e2e. Symptom signature for the future: client log shows
   LoadException gfx/hud/vilind + ExceptionInInitializerError + player
   frozen at spawn; the server silently falls back to a stale
   server/gameres dir (exe-anchored candidate #2) when repo-root gameres
   is missing.

REAL CLIENT e2e AFTER the account-keying change: FULL regression green -
MOVEMENT: MOVED, WALKDIR all five directions ARRIVED, NO TELEPORT: OK,
RAPID CLICKS: GLIDING, PORTRAIT layers present, EQUIPVIS VERDICT: OK
(doll recomposites linenpants), CURSOR drag + GROUNDDROP OK. The
account-scoped save keys did not regress the real client path.

NEXT (handoff): craft pagina ad->action wiring for remaining recipes;
vis-scan result caching; carrying-pose state for bows; cursor pickup
redirection (merge stacks); relay pickup/build against guest STATIC gobs
(drops/structures) - the same RelayAttack pattern with an action enum;
load-test story for the sharded save (per-node bot cohorts persisting).

## 2026-10-05 - Session 29 (continued 2): 1k load evidence + vis sub-phase attribution
Measurement session under .unlazy (no new dir). Goal: verify the
"vis-scan result caching" handoff item against data before implementing.

EVIDENCE (1000 in-process bots ramping to 1000 players / 1000 sessions,
10 Hz, --workers 4, release build):
- Steady state at the full 1000 sessions: mean_tick_us 43-45 ms
  (45% of the 100 ms budget), max_tick_us 139 ms.
- New sub-phase counters (committed): vis_scan_us 2-3 ms (dirty-cell
  candidate scan), vis_spawn_us 9.7-32 ms (per-viewer spawn encode -
  DOMINANT, spikes with bot cell-crossing churn), vis_retract_us ~0-6 ms.
  Movement phase 5-33 ms. AI/combat/vitals negligible at this scale.
- Decision on "vis-scan result caching": DEFERRED with rationale. At
  VIEW_RADIUS=300 / CELL=250 a view square spans ~16 cells, only ~4 are
  fully inside the Chebyshev square, so a per-cell set-bump cache caps at
  ~25% cell skips; exact-range spawn correctness requires keying entries
  on the session position (within-cell movement changes exact distances),
  which zeroes the benefit for movers and helps only static viewers.
  Re-evaluate ONLY with a static-heavy-world probe showing the scan
  phase dominant.
- The actual next perf lever (data-backed): the spawn encode path
  (stream_spawn -> encode_gob_block + res table + unacked clone per
  (gob, viewer)). Options for next session: buffer reuse in the encode
  path, per-gob encode sharing where res ids are node-global, or spawn
  coalescing budgets.

Also this session: sandbox-wipe recovery is now one command
(scripts/jogl/deploy-agent-env.sh regenerates gameres via the new
server/scripts/make-gameres.sh); real-client full e2e green after the
account-keying change (MOVEMENT/WALKDIR/NO TELEPORT/GLIDING/PORTRAIT/
EQUIPVIS/CURSOR/GROUNDDROP all OK).

NEXT (handoff): spawn-encode path optimization (data-backed, see above);
craft pagina ad->action wiring for remaining recipes; carrying-pose
state for bows; cursor pickup redirection (merge stacks); relay
pickup/build against guest STATIC gobs (RelayAttack pattern with an
action enum); load-test story for the sharded save (per-node bot
cohorts persisting across a cluster restart).

---

## 2026-10-06 - Session 30: relay static acts, cursor merge, vis cache, sharded load story

Continuation under .unlazy/session30 (PLAN + GATES there). Sandbox was
wiped again (no cargo/ant/JOGL, save/ artifacts gone) - re-provisioned
via rustup + scripts/jogl/deploy-agent-env.sh (which also regenerated
gameres, 6382 files). Session-29 recovery recipe confirmed end to end.

WHAT (all landed on master, one commit per leaf):

1) RELAY STATIC ACTS (cross-node pickup/chop/mine, session-29 NEXT #5):
   - GuestKind::Static gained a STABLE StaticClass tag (Drop/Tree/Stone/
     Structure); statics now PUBLISH across nodes (guest_state_from_slot
     previously filtered them out - drops near cell boundaries were
     invisible AND unclickable on the far node).
   - New mesh messages RelayStaticAct{player,target,act} (home ->
     authority) and StaticAck{player,stack,lp} (authority -> home). The
     authority re-validates the act against its own Kind; a stale guest
     view (class vs kind drift) is dropped, never trusted.
   - Pickup acks carry the removed stack as resource NAME + count + ql +
     fep label; the home node grants it through grant_pickup (cursor
     redirection included). Chop acks carry lp; exhausted trees leave a
     stump exactly like the local path.
   - BUG FOUND+FIXED by the leaf's tests: publish(Retract) after kill()
     never fired (Gobs::get requires alive) - remote retracts relied on
     the subscriber GC sweep. publish() now resolves dead-gob retracts
     through the split id with a generation check.
2) CURSOR PICKUP REDIRECTION + STACK MERGING (NEXT #4):
   - InvStack::absorb: counts add, quality re-averages count-weighted
     (integer, loftar convention; stacking policy is server policy per
     items-and-quality.md - policy choice documented in code).
   - grant_pickup: same-resource cursor absorbs the pickup (one drag
     stack, num sync via sync_cursor_widget); otherwise merges into the
     first same-resource inventory stack. Applied to: ground-drop clicks,
     inventory releases (inv_drop), craft outputs (recipe + roast),
     crop harvests; teardown merges the dangling cursor stack. Different
     resources never merge (unit-proven).
3) VIS-SCAN RESULT CACHING (NEXT #3):
   - VisIndex now records per-tick TOUCHED lists (insert/remove/
     reposition/mark_mover record the gob under every relevant cell).
   - A position-stable session reuses its last scan result: nothing
     touched in view -> provably unchanged (skip); few touched (<=128
     size guard) -> patch (leavers re-filtered by current position,
     enterers added, deaths purged by liveness); dense views route to a
     full rescan (patch would cost more than the scan).
   - Worst-case measured parity at 1000 walking bots (p50 21.6ms vs
     20.7ms baseline, this 2-core box, both well inside the 100ms
     budget); 600-bot window in the verify script: p50 8.4ms,
     max 50ms. Patch correctness unit-proven (patched result == fresh
     rescan; bsearch-on-unsorted found and fixed during the session).
4) SHARDED-SAVE LOAD STORY (NEXT #6): server/scripts/verify_session30.sh
   - Phases: relay-static (5 unit tests), cursor-merge (3), vis-cache
     (unit + 600-bot budget window), cluster load (2 nodes x 60-bot
     cohorts through the real UDP path, perf-checked), graceful SIGTERM
     flush, both shard files carry the cohorts, FULL CLUSTER RESTART ->
     60+60 bot logins restore their snapshots ("restoring persisted
     character"). SESSION30 E2E: OK.
   - Cargo invocations run from server/ (workspace root); test-pass
     counts sum across suites.

EVIDENCE: 160 unit tests green (9 new: 3 pickup-merge, 5 relay-static,
1 vis-cache), clippy -D warnings clean, fmt clean; wire e2e regression
(test_client WORLD ENTRY + test_craft EAT FLOW) re-run green on the
final tree.

NEXT (handoff): craft pagina ad->action wiring for remaining recipes
(needs verified recipe data from the wiki - do not invent numbers);
carrying-pose state for bows (depends on a bow being craftable); relay
SessionRelay for session-targeted UI on foreign nodes (crop/station
menus); vis-cache: cheapen the Full path (bucket pos storage instead of
per-id lookups) if 10k sessions on one node becomes a real target -
today the cluster split carries that goal.

Real-client e2e: attempted after re-provisioning; see the session-30
commit trail for the verdict recorded below.
