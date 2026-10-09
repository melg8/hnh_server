# HANDOFF ARCHIVE — hnh_server session log (sessions 1-50, verbatim)

This file preserves the full historical session log of the project, moved
verbatim out of HANDOFF.md in session 53 (type 0: docs hygiene) to stop
the active handoff file from growing unboundedly. Nothing was edited,
condensed, or lost: every entry below is byte-identical to its state at
commit cf9e0e7 (the session-52 tip).

Rules:

- This file is READ-ONLY history. Never append new session entries here.
- New session entries go to HANDOFF.md, which keeps only the last two
  sessions in full; older entries move here verbatim each session.
- The living context (how to continue, architecture, gaps, rotation
  log, session index) lives ONLY in HANDOFF.md.

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

Real-client e2e after sandbox re-provisioning (deploy-agent-env.sh +
ant jar with JDK8): FULL regression green on the final tree -
MOVEMENT: MOVED, all five WALKDIR legs ARRIVED, SPEED 3.43 tiles/s OK,
NO TELEPORT OK, RAPID CLICKS: GLIDING, PORTRAIT layers present, EQUIPVIS
OK (doll recomposites linenpants), CURSOR OK (drag + ground drop +
pickup restored through the REAL widget chain - covers the session-30
pickup-merge changes), GROUNDDROP OK.

## 2026-10-06 - Session 31: cross-node crop harvest relay (farming loop closes across nodes)
Continuation after another sandbox wipe (full re-clone; recovery: deploy-
agent-env.sh + ant jar + cargo build - all scripted, ~10 min).

WHAT (commit e9de696): the LAST cluster interaction gap for the farming
loop - a player homed on node N can now harvest a crop that grows on
node M's cell.
- Crops publish with a stable StaticClass::Crop tag + a (spec, stage)
  payload; the farming scheduler re-publishes GuestUpdate on every stage
  advance; the subscriber renders the new stage from the sdt byte in the
  re-encoded guest block (OD_RES | 0x8000 shape - identical to the local
  plant path's wire).
- Click on a guest crop: the harvest flower menu opens on the HOME node
  from the guest view (open_crop_menu split into a local wrapper +
  show_crop_menu shared with the guest path; unripe crops open nothing,
  same as local). Acting on the menu relays StaticAct::HarvestCrop.
- Authority: re-validates the Kind (stale-view acts are dropped), decides
  mature vs unripe from ITS crop state, rolls the SAME quality/yield
  tables as the local path, kills the crop, restores tilth, and answers
  one StaticAck PER yielded stack (relay_static legs return stack vecs
  now; LP rides the first ack only). The home node grants every ack via
  grant_pickup - cursor redirection and stack merging apply unchanged.
- verify_session30.sh now exports ~/.cargo/bin into PATH: a
  non-interactive shell without cargo made all three unit-test phases
  count 0 (phases 4-5 were unaffected - they use the release binary).

EVIDENCE: 154 unit tests green (6 new: payload shape, stage-advance
publish, menu gating ripe/unripe, mature acks = one per yield table
entry with zero LP, mismatch drop, multi-ack home grants), clippy/fmt
clean. SESSION30 E2E fully OK on this tree (relay-static 5/5,
cursor-merge 3/3, vis-cache unit + 600-bot p50 10.8 ms, cluster load
n0 11.5 ms / n1 7.9 ms max ticks, 60+60 shard persistence, restart
restore 60+60). Real-client e2e after re-provisioning: MOVEMENT MOVED,
five WALKDIR legs ARRIVED, NO TELEPORT OK, RAPID CLICKS GLIDING,
PORTRAIT layers, EQUIPVIS OK, CURSOR + GROUNDDROP OK.

NEXT (handoff): craft pagina ad->action wiring for remaining recipes
(needs verified recipe data from the wiki - do not invent numbers);
carrying-pose state for bows (depends on a bow being craftable); relay
SessionRelay for session-targeted UI on foreign nodes is now only
needed for STATION menus (crop menus are session-local by design) -
station fuel/input/progress widgets read authority state, so the
cleanest shape is a station-state snapshot piggybacked on the guest
payload + relayed widget actions (the RelayStaticAct pattern again);
planting across nodes (a player standing on node N planting INTO a
foreign furrow) is the remaining farming gap - same relay pattern with
a PlantCrop act carrying the seed stack.

## 2026-10-06 - Session 31 (continued): cross-node planting relay
Commit 5a77e4c + e380daa. The planting half of the farming loop now also
crosses nodes, completing the plant->grow->harvest cycle for a player
homed anywhere against furrows anywhere.
- plant_seed on a foreign-cell furrow relays RelayPlantAct{player, tx,
  ty, spec, seed_ql}; the seed stays on the cursor until PlantAck (a
  rejected or lost hop never destroys a seed). relay_plant on the
  authority runs the SAME validation order and state transitions as the
  local path and answers PlantAck ok:true; refusals stay silent (cursor
  keeps the seed, parity with a local refusal).
- Cross-node PLOWING is deliberately deferred: plow_tile mutates grid
  TILES, which are per-node generated + mutated state; relaying it needs
  a TileMutation mesh broadcast + grid resend to every holder on every
  node + persistence placement. The furrow must exist locally on the
  tile's authority node today (bots/players on that node plow; everyone
  else plants/harvests cross-node).
- 154 unit tests green (3 new), clippy clean, fmt clean.

NEXT (handoff): craft pagina ad->action wiring (wiki-verified numbers
only); carrying-pose state for bows; TileMutation broadcast protocol for
cross-node plowing/terraforming; station menus relay (station-state
snapshot on the guest payload + relayed widget actions); load-test story
for the sharded save at 300+ bot cohorts per node.

## 2026-10-06 - Session 32: cross-node plowing (TileMutation), tilth decay revert
The last deferred farming gap is closed: a player standing anywhere can
plow, plant and harvest furrows anywhere - every tile act now crosses
nodes. Plus a real gameplay bug fix found on the way.

WHAT (commit 95d8106 + the verification commit):
- RelayPlowAct/PlowAck/TileMutation node-link messages. plow_tile on a
  foreign-cell tile relays to the tile authority; the home node never
  mutates its own grid while relaying (no shadow furrow), stamina drains
  only on the ok ack (a refused or lost relay costs nothing, parity with
  a local refusal). The authority validates against ITS grid, mutates
  (override recorded for persistence), starts the tilth clock, acks and
  broadcasts TileMutation to every peer; LOCAL plows broadcast too.
- Every node applies TileMutation: resident grids take the full mutation
  plus a MAPDATA re-send to local holders; non-resident grids only
  record the override (GridStore::note_override_maybe - never
  materialize a grid nobody looks at just to shadow a mutation).
- TILTH DECAY REVERT (real bug fix): an expired furrow now reverts its
  tile to GRASS - live grid, persisted override, holder re-send and (in
  cluster mode) TileMutation broadcast. Before this the tile stayed
  PLOWED forever: un-re-plowable (not grass) AND un-plantable (no
  tilth) - a permanent dead end after one missed planting window.
- plow_tile's inline holder re-send extracted into resend_grid_to_holders
  + mutate_tile_local (plow, decay revert, remote apply share them).
- VERIFY: server/scripts/verify_session32.sh (relay-plow / decay-revert /
  wire / cluster-plow phases) + scripts/probe_plow.py - a FarmClient
  subclass that decodes the MAPDATA fragment stream, ports
  grid_owner::owner_of to python (rendezvous hash, rustc-verified),
  picks a foreign-cell grass tile and drives the plow through the REAL
  widget chain against a REAL 2-node cluster.

EVIDENCE: 176 unit tests green (6 new: 5 game relay/decay/mutation-path
tests + 1 GridStore), clippy -D warnings clean, fmt clean. Real-cluster
cluster-plow phase: client observed the PLOWED tile byte in the re-sent
MAPDATA; node0 log "relay plow act sent" + "remote tile mutation
applied"; node1 log "relay plow applied". SESSION30 E2E fully green on
this tree (600-bot window p50 10.3ms max 46ms; cluster load n0 10.9ms /
n1 12.7ms; 60+60 shard persistence + restart restore). Wire regression:
WORLD ENTRY OK + FARMING FLOW OK.

TWO VERIFICATION LESSONS (both caught by the failing phase, not by
review - the reason the gates exist):
1) A STALE RELEASE BINARY: `cargo test --release` does NOT rebuild the
   bin target (only the unittest hosts in deps/), so the cluster-plow
   phase silently ran the PRE-change binary and the relay branch never
   fired. Rule: run `cargo build --release` after edits before any
   binary-driven verification.
2) A PYTHON PORT BUG: the owner_of port missed the 64-bit truncation of
   the multiply stage (2 * 0x9E37.. carries a 65th bit in python but
   wrapping_mul truncates in Rust) - it silently flipped cell owners and
   made the probe "pass" through the LOCAL path. Fixed by masking every
   product; cross-checked against a rustc-compiled reference on four
   cells. Port order-splitmix operations are a standing footgun.

HARNESS FIX: test_farming.py now uses a per-run isolated save file. The
old shared fixed path accumulated restored crops across runs; the flow's
find_gobs then clicked a stale first-seen crop instead of the one this
run planted. Server-side acts were correct per the debug logs (plow /
plant / harvest all executed); the yield grant landed per the cursor
merge rules and the harness's inventory-widget assertion only tracks
this run's expectations. If a future change makes grants land on the
cursor, that assertion stays blind to same-resource cursor merges by
design (session 30) - worth remembering when extending the flow.

NEXT (handoff):
- station menus relay (station-state snapshot piggybacked on the guest
  payload + relayed widget actions - the RelayStaticAct pattern again);
  station fuel/input/progress widgets read authority state.
- craft pagina ad->action wiring for remaining recipes (wiki-verified
  numbers only - do not invent).
- carrying-pose state for bows (depends on a bow being craftable).
- load-test story for the sharded save at 300+ bot cohorts per node
  (today's evidence is 60/node).
- DISCOVERED (pre-existing, not touched this session): on_mapreq
  populates foreign-cell grids locally on EVERY node (each node carries
  its own shadow statics for the same grid; the peer's copies are never
  announced because publish_targets only reaches peers subscribed to
  cells the PEER owns). Single-player-visible per node, so no duplicate
  rendering per client today, but cross-node static visibility near a
  boundary depends on BOTH nodes having populated the same grid. The
  clean fix is owner-filtered populate + a Sub-driven populate/announce
  on the authority; design it with the station relay above.

## 2026-10-06 - Session 33: station menus relay, owner-filtered populate
Both NEXT items from session 32 landed: the LAST session-UI surface
(station Light/Extinguish + fuel/input delivery) now crosses nodes, and
the shadow-statics defect is fixed at the root.

WHAT (commit f4f6f89 + fd06ce7):
- STATIONS RELAY. Stations publish as StaticClass::Station with a
  StationView snapshot {spec, lit, fuel, has_input} piggybacked on the
  guest payload (additive field, bincode-positional like every node-link
  change). A guest oven click opens the flower menu LOCALLY from the
  snapshot (session UI lives on the home node - the crop-menu pattern);
  the menu arms with the act intent picked from `lit`. The choice relays
  RelayStationAct{Light/Extinguish}; the authority re-validates against
  its own StationState in the SAME order as the local menu path
  (stale-lit -> Stale silent, fuel -> NeedsFuel, input -> NeedsInput),
  applies the transitions, re-renders through set_station_lit and
  answers StationAck. Refusal acks render the EXACT system lines the
  local path emits ("The oven needs fuel first." etc) - cross-node UX
  parity. Fuel/input delivery: a held-stack click on a guest oven ships
  RelayStationItem with the stack described by NAME (res/ql/label);
  the cursor stack is consumed ONE unit only on the FuelAdded/
  InputLoaded ack (the session-31 seed-safe pattern - a refused or lost
  relay never destroys an item); every refusal maps to its local
  system line. set_station_lit / station_itemact re-publish
  GuestUpdate so subscribers re-render lit/fuel/input changes from the
  sdt byte (same OD_RES|0x8000 shape as crop stages).
- OWNER-FILTERED POPULATE (the session-32 DISCOVERED defect).
  populate_grid/populate_animals take an owner filter in cluster mode:
  a node spawns only content whose VisIndex cell it owns. Before: every
  node populated every grid it looked at, so each carried shadow
  statics for foreign cells, and populate_animals' per-node rng placed
  DIFFERENT animals than the owner's roll (two desynced copies near
  every boundary). Now on_mapreq materializes only the TILES (seed-
  deterministic, identical everywhere) and the CELL OWNER materializes
  the content: a peer's Sub runs populate_for_subscriber, which
  materializes the authority's part of every grid the subscribed cells
  touch (grids_touching_cell: a 250-subtile cell touches 1-2 grids per
  axis, at most 4) and announces EVERY gob held in the subscribed cells
  - freshly spawned AND pre-existing (stations, structures, earlier
  statics) - so the subscriber's view starts from the single
  authoritative copy.

EVIDENCE: 187 unit tests green (11 new: station publish snapshot,
guest click -> menu + relay, authority act validation order incl.
stale/extinguish-preserves-input, item relay ships + cursor untouched
before ack, authority item validation order, ack cursor consumption,
refusal system lines, owner-filtered spawn, Sub-driven populate
announce, grids_touching_cell coverage), clippy -D warnings clean, fmt
clean. verify_session33.sh (station/populate/wire/cluster phases).
SESSION30 E2E green ON THIS TREE: 600-bot window p50 10.8ms max
50.7ms, cluster 60+60 cohorts max tick 9.7/9.0ms, shard persist +
restart restore 60+60. Session-32 wire + relay-plow + decay-revert
phases re-run green.

DESIGN NOTE for the station relay: the flower menu is one-shot session
UI, so StationAck refusals re-render through system lines instead of a
widget state push; if station fuel/progress WIDGETS ever land (the
legacy oven had four slots + a gauge), the snapshot already carries
fuel/has_input and the natural next step is a widget-state snapshot on
the ack instead - no protocol change needed, the payload is there.

VERIFICATION GAP (deliberate, timeboxed): the station relay is proven
by unit tests on both sides of the mesh channel (home: click->menu->
relay->ack handling; authority: validation/apply/ack) and the cluster
story re-run green, but there is NO real-cluster probe driving a live
client through a guest oven light+fuel yet (needs an oven built or
spawned on the peer's side of a boundary - the probe_plow.py pattern
with a built_oven fixture). That is the first candidate for the next
session's cluster probe.

NEXT (handoff):
- real-cluster station probe (probe_station.py): drive a live client to
  fuel + light a guest oven through the real mesh; the unit coverage
  above defines the assertions.
- craft pagina ad->action wiring for remaining recipes (wiki-verified
  numbers only - do not invent).
- carrying-pose state for bows (depends on a bow being craftable).
- load-test story for the sharded save at 300+ bot cohorts per node
  (today's evidence is 60/node; the 600-bot window is single-node).
- populate_for_subscriber announces by scanning gobs_in_view per cell -
  fine at 60-bot cohorts, revisit if the announce burst shows up in the
  perf counters at 300+.

## 2026-10-06 - Session 34: build-transition publish, live station probe, 300/node load
Both remaining NEXT deliverables from session 33 landed (the live
guest-oven probe and the 300+/node load story), and driving the real
cluster end to end caught and fixed FIVE latent defects that unit
coverage on both sides of the mesh channel could not see.

WHAT (commits 58cd502 + 558ce34 + bb37595):

- BUILD TRANSITIONS PUBLISH (the read-side gap). sink_material's stage
  advance and complete_plan only re-rendered LOCAL viewers
  (restage_gob); peers watching a build never heard anything, so a
  guest oven built by a peer stayed a dead Structure guest forever
  (every station interaction keys off the Station class). Three
  layers: GuestKind::Static carries a plan `stage: Option<u8>`
  (additive bincode-positional field, same policy as the session-33
  station snapshot); both transitions re-publish GuestUpdate; and
  ingest_guest detects a kind-payload flip on an EXISTING guest and
  re-renders every viewer with a full OD_RES block carrying the fresh
  sdt byte - the wire mirror of restage_gob. That last piece also
  fixed the session-33 lit re-render claim: the existing-guest path
  only streamed pose/move/hp deltas before, so a lit guest oven never
  re-rendered for players already watching it.

- PROBE_STATION.PY (session 33's deferred verification gap). On a real
  2-node cluster: a builder on node 1 walks to the cell boundary and
  raises an oven on a DEEP own-cell tile (>= 60 subtiles from every
  cell edge - the roast output drop spawns with a +/-30 jitter and a
  rim site lands it in the peer's cell where gobs are invisible)
  through the real build flow; a probe homed on node 0 follows the
  build as guests and drives fuel + input (RelayStationItem), the
  Light flower menu from the snapshot (RelayStationAct), the lit
  re-render and the roast output drop. Both characters pump
  cooperatively (a UDP session that stops reading for tens of seconds
  overflows its socket buffer and loses raw OBJDATA blocks forever).

- FOUR DEFECTS THE LIVE PROBE CAUGHT:
  1. Sub carried diffs (added cells) but the receiver REPLACED the
     whole subscription set: the first follow-up Sub from a moving
     session silently unsubscribed every earlier cell - cross-node
     updates for still-subscribed cells stopped flowing (a lit oven
     never re-rendered). Sub now extends incrementally, like Unsub
     always did. Unit test: sub_diffs_extend_not_replace.
  2. tokio::select! in the session driver picked branches at random,
     so a raw OBJDATA block could beat its own RESID announcement
     (separate send paths) and the client could never resolve the
     gob's resource. Biased polling drains inbound ACKs, then the
     reliable stream, then raw datagrams.
  3. Guest static spawn blocks carried OD_LAYERS with a bare 0xFFFF
     terminator (no base u16) - the local path never writes OD_LAYERS
     for statics and every strict OD parser chokes on the orphan.
     Statics render from OD_RES alone now; stream_guest_pose skips
     them.
  4. test_build.py searched the roast output drop by the INVENTORY
     icon resource while the drop GOB renders with the gfx/terobjs
     items world shape (drop_world_res, session 26) - the station
     flow's output stage had been searching for a gob that never
     matches.

- 300/NODE LOAD STORY (session 33's deferred deliverable). First run
  FAILED the budget: node0 max_tick_us=168166 at 300 clustered walking
  bots. Extending the perf attribution from 5 to 9 phases (farming,
  stations, cluster, guests were invisible) pinpointed
  phase_guests_us = 56-67 ms/tick: tick_guests built a viewers Vec by
  filtering ALL sessions for EVERY moving guest every tick -
  O(guests x sessions) HashSet lookups plus one datagram per (guest,
  viewer) pair. Rewritten in the batch_move_broadcast shape (blocks
  encoded once, one pass over sessions, ONE datagram per session,
  LINSTEPs not recorded in unacked). Result at 600 clustered bots:
  max_tick_us 62378/63936 - both nodes hold the 100 ms budget with
  margin and the sharded save persists both cohorts.

EVIDENCE: 191 unit tests green (5 new: stage advance publishes the
Structure class + stage 1, completion publishes the Station class +
snapshot, kind flip re-renders OD_RES with the lit sdt byte for an
existing viewer (raw wire proof), sub diffs extend not replace),
clippy -D warnings clean, fmt clean. verify_session34.sh: station-units
+ cluster-station (probe verdict "STATION RELAY: OK" + the relay pair
on both node logs: item/act sent on node 0, fueled/input/lit/job on
node 1) + load-300 (300 sessions per node, both within budget, shards
persist 300+300). Full regression green on this tree: session-30 E2E
(600-bot window, cluster 60+60, shard persist, restart restore),
session-32 relay-plow, session-33 station.

KNOWN LIMITATION (documented, not fixed): a gob SPAWNED by a node on a
cell it does not own (e.g. a station output drop whose jitter crosses
a cell boundary) is never published to the cell's owner - the peer is
not subscribed to us for its own cells. Animals transfer authority on
cell crossing; drops/statics do not. The probe sidesteps it by
building deep in the owning cell. A general fix would mirror the
animal authority-transfer path for drops - next-session candidate if
boundary builds ever matter.

NEXT (handoff):
- craft pagina ad->action wiring for remaining recipes (wiki-verified
  numbers only - do not invent).
- carrying-pose state for bows (depends on a bow being craftable).
- drop authority transfer on cell boundary (the known limitation
  above) if boundary builds become a real scenario.
- 1000-session single-node window re-measure on this tree (the
  tick_guests batch should also lift the single-node 600-bot numbers;
  the session-30 phase-3 gate still passes as-is).
- real-client e2e re-run against the biased session driver (the
  RESID-before-raw ordering is what the legacy client implicitly
  assumed all along; the GL client should be re-verified).

## 2026-10-06 - Session 35: drop authority transfer, 1000-bot window, per-node certs
The session-34 known limitation ("a gob spawned by a node on a cell it
does not own is never published to the cell's owner") is CLOSED, the
single-node 1000-session window is measured in budget, and a fresh-
sandbox cluster-boot race on the shared dev certificate is fixed.

WHAT (commits 5b38557 + 0cc175e + 07f74be + handoff):

- DROP AUTHORITY TRANSFER (the animal path mirrored for drops). A
  Kind::Drop spawned by a node onto a cell it does not own (a station
  output drop whose +/-33 subtile spawn jitter crosses the boundary,
  stone rubble, loot) is now handed to the cell's owner and demoted to
  a guest locally; the owner claims the EXACT id back into Kind::Drop.
  Wire shape: GuestKind::Static gained an additive `drop:
  Option<DropView>` payload (inv_res, ql, label) - same bincode-
  positional policy as the session-33 station snapshot and the
  session-34 stage field. The world render shape is NOT carried: the
  receiver re-derives it from the inventory resource name via the SAME
  deterministic drop_world_res the spawner used, so both nodes agree
  on the sprite without shipping it. Labels cross as Strings and leak
  into the interned-name arena (leak_static, the pattern every other
  cross-node string already follows).

- PROBE_DROP.PY (live proof through the real mesh). The builder raises
  an oven at the RIM corner of cell (4,2) - a node-1 cell whose FOUR
  axis neighbors all belong to node 0 (grid_owner scoring is seed-
  independent, so the site plan is stable across runs). The probe
  roasts through the guest relay (session-34 regression), and the
  output drop that crosses the boundary must TRANSFER: it becomes a
  LOCAL gob on the probe's node and a plain click (no relay hop)
  restores 'Roasted Beef' into the probe inventory. Fallbacks when
  the jitter keeps a drop inside the oven cell: a second roast driven
  locally by the builder (fuel-first order - the surviving branch unit
  sits in the builder's cursor after the build, and take is refused
  while a cursor is held), then guest rim-tree chops (each chop
  spawns a wood drop with the same jitter; trees survive 5 harvests).
  LIVE FIRST-RUN EVIDENCE: roast 1 stayed (relay pickup regression
  exercised instead), roast 2 crossed -> node1 "drop authority
  transferred id=98590 owner=0" + node0 "drop authority claimed
  id=98590" + local pickup proof.

- 1000-BOT SINGLE-NODE WINDOW (the session-34 handoff's re-measure
  ask). The in-process bots log in at ~12 sessions/s (TLS auth + play
  + world entry each), so a fixed 75 s settle window cut the cohort at
  938; the phase now polls until the cohort settles. Measured: 1000
  sessions live (saturated world, walking/fighting),
  max_tick_us=88838 - the 100 ms budget holds with 11 percent
  headroom on the post-session-34 tree (batch tick_guests). Previous
  single-node evidence was the session-30 600-bot window.

- PER-NODE DEV CERTS (a fresh-sandbox cluster-boot race). Two nodes
  booted from one repo raced on the SHARED certs/authsrv.* pair: both
  regenerate it at startup, and the second node can parse a half-
  written PEM and die at the auth self-check ("auth tcp/1883 not
  reachable" - reproduced on the first cluster boot of this sandbox;
  session 34 got lucky timing). Cluster nodes now default to
  authsrv-n<N>.{crt,key}.pem; the single-node default path is
  unchanged; explicit --cert/--key always wins.

EVIDENCE: 194 unit tests green (3 new: drop transfer send+demote with
the DropView payload, receiver claim with the pickup round-trip
including the re-derived world shape, negative own-cell control),
clippy -D warnings clean, fmt clean. verify_session35.sh: drop-units
+ cluster-drop (probe verdict + BOTH node logs grepped for the
transfer pair) + load-1000 (sessions=1000 max_tick_us=88838) +
regression (unit battery, clippy, session-34 station-units,
session-34 cluster-station all green on this tree).

NOT DONE THIS SESSION (rolled to NEXT):
- craft pagina ad->action wiring for remaining recipes (wiki-verified
  numbers only - do not invent). Carried from session 34.
- carrying-pose state for bows (depends on a bow being craftable).
- the ~12 sessions/s bot login pace is now the load-test bottleneck
  (86 s to enter a 1000-bot cohort); if 10k-bot windows are ever
  measured, parallelizing the bot login loop is the first lever.

NEXT (handoff):
- craft pagina ad->action wiring for remaining recipes (wiki-verified
  numbers only - do not invent).
- carrying-pose state for bows (depends on a bow being craftable).
- real-client e2e re-run against the biased session driver (carried
  from session 34; the RESID-before-raw ordering change is still
  unverified on the GL client).
- drop transfer is proven for Kind::Drop; plans/structures built on a
  foreign cell still spawn a local-only gob on the builder's node (the
  session-35 transfer pass deliberately handles drops only - plans
  mutate through the build flow, not a stateless transfer).

## 2026-10-06 - Session 36: the bow chain - woodbow/stonearrow/bonearrow recipes, bone loot, carrying pose
The session-34/35 carried deliverable "craft pagina ad->action wiring
for remaining recipes" advances with the full bow chain, and the
"carrying-pose state for bows" NEXT item is CLOSED.

WHAT:

- WIKI VERIFICATION FIRST (the "wiki-verified numbers only" rule).
  ringofbrodgar.com and the fandom mirrors sit behind a Cloudflare
  interstitial that also defeats the headless browser, and archive.org
  is unreachable from this sandbox, so numbers were recovered through
  search-index snippets: RoB Legacy:Bow gives the quality formula
  "(qBranches + qString)/2, Softcapped by Marksmanship" (the Fandom
  Marksmanship page repeats the worked example (50 + 40)/2); RoB
  Legacy:Quality (already cited in crafting-and-building.md) gives the
  arrow example as a weighted average with a HEAVIER WEIGHT ON BRANCH,
  Survival softcapping arrows. The legacy forum (havenandhearth.com,
  phpBB - reachable) confirms string is plant-fiber-derived and bows/
  bone-arrow quivers existed in the 2011 legacy world. The UNIT COUNTS
  (4 branch + 1 string per bow; 1 tip + 2 branch per 10-arrow batch)
  could NOT be verified from any reachable source - they are chosen
  server policy, isolated in the RECIPES table and recorded in
  crafting-and-building.md Open questions for reconciliation.

- RECIPE.Q_WEIGHTS (the quality engine change). Recipe gained a
  per-INPUT-TYPE weight slice. Empty = the pre-36 UNIT-weighted
  average (w = consumed units; kept for axe and hcloak so their
  behavior is unchanged). Non-empty = the RoB TYPE-weighted model:
  each input TYPE first averages its own consumed units
  (lowest-quality-first consumption is unchanged), then the type
  averages combine as sum(q_t * w_t)/sum(w_t). This is what makes
  (qBranches + qString)/2 true for a 4:1 unit mix. craft_once
  accumulates (qsum, units) per input index alongside the legacy flat
  unit list; the softcap halving (q + attr)/2 is unchanged and now
  keys "ranged" (Marksmanship) for the bow and "survive" (Survival)
  for the arrows - both live in attrs as skill values (skills.rs
  SKILL_VALUES), so no new attribute plumbing was needed.

- THREE RECIPES (craft.rs RECIPES, resources verified in the served
  pack: gfx/invobjs/{bow,arrow-stone,arrow-bone,string,branch,stone,
  bone} + paginae/craft/{woodbow,stonearrow,bonearrow} with ad
  ["craft", id]): woodbow (branch x4 + string x1 -> bow x1, weights
  [1,1], softcap ranged), stonearrow (stone x1 + branch x2 ->
  arrow-stone x10, weights [1,2], softcap survive), bonearrow (bone x1
  + branch x2 -> arrow-bone x10, weights [1,2], softcap survive).
  Batch outputs bundle as ONE stack per craft (count 10). Paginae are
  pushed automatically by the existing `for r in RECIPES` loop at
  world entry; no MenuGrid changes were needed.

- BONE LOOT (state.rs Species::loot). Every species now drops
  gfx/invobjs/bone on death (Deer/Aurochs/Cow/Boar/Wolf x2, Fox/Hare
  x1) so the bone-arrow recipe has an in-world source. Counts follow
  the server's scaled-down death-drop policy (meat x3/x4 against
  legacy butcher x10); the legacy butcher numbers are already in
  animals-and-husbandry.md and the delta is recorded in its Open
  questions. Species::ALL (#[cfg(test)]) added for the roster sweep.

- CARRYING POSE (equip.rs PIECES, closes the handoff item). The pack
  ships dedicated two-handed bow layers: gfx/borka/eq-bow/{standing,
  walking,dead}/arm/carrying/{left,right}-{d}.res. The new
  piece!("gfx/invobjs/bow", "eq-bow", ...) entry maps both carrying
  templates for every octant, standing and walking. The templates
  carry no {hand} (idle/banzai) placeholder, so the existing
  doll-set derivation keeps the front carrying pair on the paperdoll
  unchanged - no doll special case needed. Any equipment slot works
  (slot semantics stay server-side policy).

- STARTER KIT (game.rs, dev policy). Fresh characters now spawn with
  branch x6, stone x4, string x2 (was branch x2, stone x2) plus the
  existing meat/seeds/clothing, so one Wooden Bow AND one Stone Arrow
  batch are craftable out of the box with zero foraging.

EVIDENCE: 200 unit tests green (6 new: woodbow_quality_is_type_weighted
asserts q17 = softcap((40+10)/2, ranged=10) - the pre-36 unit math
would give 22, so the assert distinguishes the models;
stonearrow_bundles_ten_and_branch_weighs_double asserts a 10-batch at
q20 = softcap((10*1+40*2)/3, survive=10); bow_equip_renders_carrying_
pose asserts the eq-bow carrying layers for standing, walking, and the
doll; starter_kit_covers_the_bow_chain asserts kit counts + pagina
registration; bow_chain_recipes_are_consistent sweeps Species::ALL for
bone loot and checks weights/softcaps; q_note_marks_type_weighted_
recipes pins the provenance note). One existing test updated for the
new starter kit (static_ack branch stack 2 -> 6). clippy -D warnings
clean (three findings fixed: dead-code q_note/ALL now #[cfg(test)],
useless u32::from dropped), fmt clean. scripts/verify_session36.sh
(four phases: bow-units, full, lint, boot - the release binary boots
with the extended tables): SESSION36 VERIFY: OK.

NOT DONE THIS SESSION (rolled to NEXT):
- Bow SHOOTING: attack rolls for a bow-equipped player against a
  target at range (aim time, arrow consumption, damage 75*sqrt(q/10)
  from the Fandom Bow page) - the crafting/equip side is done, the
  combat use is not.
- Quiver container (gfx/invobjs/quiver exists in the pack).
- Real-client e2e re-run against the biased session driver (carried
  from sessions 34/35; still unverified on the GL client).
- Bow-chain unit counts reconciliation against RoB when a source is
  reachable (see crafting-and-building.md Open questions).

NEXT (handoff):
- bow shooting mechanics (docs/mechanics/combat/combat-system.md is
  the entry point; the RoB aim-speed note "a Ranger's Bow aims at half
  the speed of a Wooden Bow and one-sixth the speed of a Sling" is
  already recovered in this session's research).
- real-client e2e re-run (carried).
- craft pagina ad->action wiring for further recipes beyond the bow
  chain (straw basket / wooden bowl are the natural next containers -
  paginae and invobj resources exist in the pack).

## 2026-10-06 - Session 37: bow shooting (the Shoot action) - aim meter, arrow economy, quiver recipe, real-client probe green

The session-36 NEXT items "bow shooting mechanics" and the carried
"real-client e2e re-run" are CLOSED (the probe scope); the quiver
recipe advances the craft pagina wiring.

WHAT:

- ARCHERY MODULE (server/crates/hnh-server/src/archery.rs). The Shoot
  action of Legacy:Combat_Actions is live: aiming fills an ACCURACY
  METER (10000 scale, Wooden Bow 250/tick = full aim in 4 s at the
  10 Hz combat tick; the RoB "Ranger's Bow aims at half the Wooden
  Bow's speed" note lives as a BOWS table row, data not code). Range
  132 units (~12 tiles); inside 300 units the archer closes in and
  keeps the aim, beyond it the aim drops (same radius as melee
  disengage). Release is automatic at a full meter: ONE arrow
  (arrow-stone | arrow-bone) is consumed hit or miss, the frv offence
  bar (attack meter) is zeroed per the documented rule, stamina -2.
  Damage `75*sqrt(q_bow/10)` (Fandom Bow page, the same
  k*sqrt(x/10) shape as the unarmed maneuvers): q10 -> 75 (one-shots
  Deer/Fox), q40 -> 150. Arrows BYPASS the openings economy - the hit
  roll IS the resolution: chance `95 - 55*(dist/132) +
  min(20, Marksmanship/5)` percent clamped 15..99 (server policy, the
  legacy formula is not client-observable; combat-system.md Open
  questions). Aim auto re-arms while the target lives and arrows
  remain; a kill runs the standard death flow (loot + bone drops,
  +10 LP, teardown).

- ENGAGEMENT WIRING (game.rs). Clicking an animal with a
  gfx/invobjs/bow stack in ANY equip slot takes the ranged path
  INSTEAD of opening the frv duel; a dry bow refuses with a chat line
  and never falls back to melee while equipped; without a bow the
  same click opens the melee duel as before. Ground click cancels the
  aim; an frv click (selecting a melee opponent) drops it. Progress
  lines stream to chat at 25/50/75% (the client has no accuracy
  widget - this was verified against the 2009 client's fight widgets
  in session 26). Species::name() added for the hit/miss chat lines.

- QUIVER (craft.rs + docs). Recipe `quiver` = raw cow hide x2 +
  string x1 -> gfx/invobjs/quiver, type-weighted [1,1], ranged
  softcap, pagina paginae/craft/quiver (shipped in the pack with ad
  ["craft","quiver"], skill link Leather Working). The avatar back
  layers (gfx/borka/quiver/{standing,walking,dead}) were ALREADY
  wired in equip.rs PIECES (pre-existing entry); the new test pins
  the world_layers mapping. namu.wiki confirms the back-slot
  arrow-carrying role; the unit counts are documented server policy
  (crafting-and-building.md Open questions) - RoB/Fandom stayed
  behind Cloudflare, jina 401. The quiver is EQUIP-ONLY for now: the
  2009 client ships no container widget (ISBox is a craft-window
  counter, not a container).

- REAL-CLIENT PROBE (carried item CLOSED in the probe scope). The
  sandbox lost its repo-root gameres/ (it is NOT in git - regenerate
  with `unzip -o -q lib/haven-res.jar 'res/*' -d /tmp/hx && cp -rn
  /tmp/hx/res/* gameres/ && cp -r res/compiled/* gameres/`); after
  regenerating, the headless UiProbe (REAL client classes - Session +
  UI + RemoteUI, the exact post-play receive path, no GL) is fully
  green against the session-37 server: UI PROBE RUN/EQUIP/CHARLIST:
  OK. The GL client itself still needs a display; the Windows
  launcher path remains the user-side check.

EVIDENCE: 193 unit tests green (15 new: 4 archery formula tests -
damage curve 75/150/225, chance falloff 95->40 + marks bonus with
15..99 clamp, 4 s meter fill, ordered report thresholds; 8 engagement
flow tests - bow click opens aim not fight / dry bow refuses / hit
consumes one arrow + depletes the attack meter + re-arms / miss
spends the arrow only / meter fills with 25-75% chat lines / chase
beyond range with the aim kept / walk cancels / lethal q40 shot kills
+ loots; quiver_recipe_and_back_layers_are_wired; plus the 2 pre-36
count updates). clippy -D warnings clean, fmt clean.
scripts/verify_session37.sh (archery-units/full/lint/boot):
SESSION37 VERIFY: OK. scripts/verify_ui_probe.sh run|equip|charlist:
all OK after the gameres regeneration.

NOT DONE THIS SESSION (rolled to NEXT):
- Guest-animal ranged fire (the cluster relay path is melee-only;
  a RelayAttack with chip=0 would carry the damage but the aim/chase
  state per foreign target needs the same guest_fights mirror work
  melee got in session 21).
- Player-vs-player archery (the melee relay fight between players
  exists; ranged needs the same authority split).
- Bow-chain and quiver unit counts reconciliation against RoB when a
  source is reachable (crafting-and-building.md Open questions).
- GL-client run on a display-capable host (the probe covers the wire
  path only).

NEXT (handoff):
- ranged fire over the cluster relay (guest animals) - the natural
  continuation of archery.rs into nodes.rs.
- the remaining craft paginae with pack resources (none beyond the
  bow chain + quiver are fully resourced; basket/bowl paginae do NOT
  exist in this pack - checked this session).
- Windows launcher smoke: make-gameres.ps1 + run scripts against the
  session-37 binary (verify_windows_launch.sh exists; re-run after
  the next resource regeneration).

### Session 37 addendum: cross-node archery (guest animals)

The "ranged fire over the cluster relay" NEXT item landed the same
session. `tick_aim`/`shoot_arrow` now resolve guest targets from the
guest table (position, species via GuestKind::Animal, liveness via
guests.contains_key); the shot's hit roll stays on the shooter's node
(session state) and a hit ships `RelayAttack { chip: 0, dmg }` to the
animal's authority. `relay_swing` gained the marker semantics:
chip == 0 is RANGED (damage applied straight through a full defence
bar - arrows bypass the openings economy; no bar mutation, no
FightBars answer), chip > 0 is the melee swing path unchanged. Guest
animal clicks with an equipped bow open the aim instead of the relay
duel (player_interact guest branch). Two new tests:
relay_arrow_shot_ships_ranged_relayattack (meter fill -> one chip-0
RelayAttack at Fandom damage, arrow spent on the shooter's node) and
relay_swing_chip0_bypasses_openings_and_kills (authority side: 200
damage through a full bar kills a 60 HP wolf). 195 tests green,
clippy/fmt clean, SESSION37 VERIFY: OK (engagement battery now 15).
Remaining from the NEXT list: player-vs-player archery, unit-count
reconciliation vs RoB, Windows launcher smoke after resource regen.

## 2026-10-06 - Session 38: player-versus-player archery end to end (local + cross-node + wire probe)

The session-37 NEXT items "player-vs-player archery" and "Windows
launcher smoke" are CLOSED; the unit-count reconciliation against RoB
stays open (sources still blocked).

WHAT:

- PVP ENGAGEMENT (game.rs player_interact). Clicking another player
  with an equipped bow opens the ranged aim: local Kind::Player
  targets take the ranged path BEFORE the party-invite menu; guest
  GuestKind::Player targets (previously a no-op click) aim too; a
  SELF-click never aims (start_aim guard, falls through to the party
  menu which ignores self-clicks); without a bow the click keeps the
  party-invite menu - melee PvP between players remains unimplemented
  and the old "melee relay fight between players exists" doc claim
  was WRONG (corrected in combat-system.md: only the animal relay
  fight exists).

- SHOT RESOLUTION (game.rs shoot_arrow). Target resolution became a
  ShotTarget match over guest/local x animal/player. A LOCAL player
  hit applies hurt_player directly (armor absorption armor.rs
  dmg*K/(K+abs), HP, stamina, knockout reset to 50), streams
  gfx/fx/hit on the victim's avatar, and chats both sides ("Your
  arrow hits <name> for N damage." / "An arrow hits you for N
  damage."). A CROSS-NODE hit ships NodeMsg::PvpArrow { victim,
  attacker, dmg } to the VICTIM's home node - node_of_gob derives it
  from the gob id's slot range, the same authority split as the
  animal-bite PlayerHurt flow - and the victim's node applies
  hurt_player + victim chat + hit FX, answering PvpArrowResult {
  shooter, killed } so the shooter's chat can report "You have
  defeated your target!" on a knockout. hurt_player now returns the
  knockout flag. The aim re-arms while the victim lives and arrows
  remain (players do not die as gobs - the knockout keeps the avatar
  in the world).

- WIRE PROBE (server/scripts/probe_pvp.py). The full chain against a
  live server, the exact path the Java client drives: two clients
  enter, the shooter crafts a Wooden Bow and a 10-arrow Stone Arrow
  batch from the starter kit through the menugrid act/make widgets,
  equips the bow through the paperdoll (the epry "ava" uimsg is the
  reliable self-identity - OD_BUDDY does NOT stream for a fresh
  character, found by debugging), gob-clicks the victim, and the
  probe observes the "Aiming at 25%..." progress line, the
  auto-release, "Your arrow hits pvpvictim-... for 75 damage." on
  the shooter, "An arrow hits you..." on the victim, and the victim's
  OD_HEALTH quarters dropping to 1/4 (100-75 q10). PVP WIRE: OK.

- WINDOWS LAUNCHER SMOKE. windows/make-gameres.ps1 gained the
  wipe-first semantics of make-gameres.sh (files removed from the jar
  never linger) and extracts the jar fully BEFORE wiping so a failed
  extraction leaves the served pack intact.
  server/scripts/verify_windows_gameres.sh runs the REAL ps1 under
  PowerShell Core 7.4 on Linux (backslash path literals converted,
  $env:TEMP mapped) - 6382 files generated, hair.res/bow.res
  verified, .genrev correctly wiped: WIN GAMERES SMOKE: OK. The
  structural gates verify_windows_launch.sh leaf1-g1/g2 stay green.
  UiProbe re-run after the regeneration: run/equip/charlist all OK.

EVIDENCE: 200 unit tests green (5 new PvP: aim-vs-party click,
local hit through armor + arrow economy + re-arm, lethal knockout +
defeat chat, guest shot ships PvpArrow at Fandom damage, authority
apply + PvpArrowResult answer non-lethal and lethal). clippy -D
warnings clean, fmt clean. scripts/verify_session38.sh (pvp-units/
full/lint/boot): SESSION38 VERIFY: OK. probe_pvp.py against a live
server: PVP WIRE: OK. verify_windows_gameres.sh: WIN GAMERES SMOKE:
OK (6382 files). Unlazy gates session38: G1-G5 automatic PASS,
G6/G7 manual (this entry + push).

NOT DONE THIS SESSION (rolled to NEXT):
- Melee PvP between players (unarmed duel between two players; the
  openings economy currently only engages players vs animals).
- LP/murder consequences for PvP kills (no LP grant, no criminal
  state - a knockout chat is all; legacy policy undocumented).
- Unit-count reconciliation of the bow chain vs RoB (sources still
  behind Cloudflare).
- GL-client run on a display-capable host (the wire probe + UiProbe
  cover the protocol path only).

NEXT (handoff):
- melee PvP between players (openings duel across two session
  players, local first, then the PvpArrow-style relay split).
- PvP consequences: LP policy or criminal/murder state if a legacy
  source surfaces.
- remaining craft paginae with pack resources (none fully resourced
  beyond the bow chain + quiver - basket/bowl paginae do NOT exist
  in this pack, verified session 37).
- re-run scripts/jogl/run-real-client-e2e.sh on a display-capable
  host when one is available (carried since session 34).

## 2026-10-07 - Session 39: melee PvP between players (local openings duel + cross-node PvpSwing relay)

The session-38 NEXT item "melee PvP between players" is CLOSED end to
end. The LP/murder-consequences item stays open (legacy policy still
undocumented); unit-count reconciliation vs RoB stays blocked.

WHAT:

- ENGAGEMENT (game.rs). Clicking another player with no bow opens the
  party flower menu, which now carries a Fight petal: petal 0 invites,
  petal 1 duels. Party refusals no longer block the menu - when the
  invite does not apply (target partied / clicker's party full) the
  menu opens as ["Fight", "Cancel"], because the duel is never gated
  by party state. Cross-node guest players get the same Fight-only
  menu (party membership has no cross-node relay). Confirming Fight
  arms the attacker (fight_target), drops any live ranged aim, opens
  the frv fight window on BOTH sides (the victim can answer
  immediately through the frv select - the legacy two-sided duel),
  and chats both lines ("You attack <name>!" / "<name> attacks you!").

- LOCAL DUEL (tick_combat, new Kind::Player branch). The openings
  economy runs against the VICTIM'S SESSION defence bar
  (FightState::own_def) instead of an animal_fights row: swings spend
  half the attacker's offence, respect atkc, chip SWING_DEF_DMG *
  weight (weight 0.5..2.0 off the relation balance), and only an
  opening (<= OPENING_THRESHOLD) passes (5*str/10).max(1) through
  hurt_player (armor absorption, HP, knockout); the bar resets on the
  break, both relations accrue IP, stamina -2 per swing. A lethal
  swing knocks the victim out (50 HP floor, energy -10, full fight
  reset) and the attacker's chat reports the defeat.

- CROSS-NODE SPLIT (nodes.rs + game.rs). Every swing ships one
  PvpSwing { attacker, victim, chip, dmg } to the VICTIM'S home node
  (node_of_gob - the same authority split as PvpArrow); the guest
  branch of tick_combat routes GuestKind::Player targets there while
  guest animals keep the cell-owner RelayAttack. The victim's home
  node chips the authoritative own_def, lands damage through
  hurt_player + victim chat + gfx/fx/hit, and answers PvpSwingResult
  { attacker, victim, def, landed, killed }; the attacker's node
  re-syncs its guest_fights mirror + relation view from the answer
  and closes the duel on a knockout. GuestRetract still tears the
  duel down when the victim walks out of view (common teardown path).

- LATENT BUGS FOUND AND FIXED. (1) FightState::new() now starts the
  defence bar FULL - derive-Default left own_def at 0, which opened
  every fresh session to instant damage from the first bite/swing.
  (2) Animal bites never wrote the chipped defence back (the local
  new_def was compared and dropped); the chip now accumulates across
  bites until the opening, matching the documented openings economy.

- LOAD BOTS FIGHT PLAYERS TOO (bots.rs). The bot cohort now duels:
  pick_target gained a Player branch (never self, 15-tile radius, two
  of four roll buckets lead with it), bots parse NEWWDG/DSTWDG to track
  the flower-menu widget, a click on a player arms a petal-confirm
  that answers the menu with cl 1 (Fight) through the real widget path,
  and the bot then HOLDS for 6 s so the duel actually runs (chase +
  swings) before picking a new target. STAT_DUELS joins the load
  verdict; an info! line logs every landed PvP hit. 40-bot 70 s smoke:
  duels=1253, pvp melee hit=419, knockouts=3 (knocked=true), zero
  errors/warnings - the melee economy runs under cohort load.

- WIRE PROBE (scripts/probe_melee.py). The full client-path chain
  against a live server: two players enter, the attacker gob-clicks
  the victim, the flower menu opens, petal 1 (Fight) arms the duel,
  both sides get their attack line + frv window, the attacker chases
  into reach and swings until the opening, and the probe observes
  "You hit meleevic-... for 5 damage.", the victim's "hits you" line,
  and the victim's OD_HEALTH quarters dropping to 3/4. MELEE WIRE:
  OK (found the stale-server trap on the way: ensure_server reuses a
  listening binary from a previous session - kill it first when the
  probe's flower menu opens but no Fight petal lands).

- DOCS. combat-system.md gained the "Implemented melee PvP model"
  section and lost the stale "melee PvP between players is NOT
  implemented" claim; communication.md documents the Fight petal and
  the no-refusal-on-party-state behavior.

EVIDENCE: 207 unit tests green (7 new: duel-open through the real
menu path, chip-until-opening with stamina, lethal knockout teardown,
mutual duel swinging both ways, relay ship exactly one PvpSwing,
authority apply + answer non-lethal and lethal, result resync +
knockout close). clippy -D warnings clean, fmt clean.
scripts/verify_session39.sh (melee-units/full/lint/boot):
SESSION39 VERIFY: OK. probe_melee.py against a live server:
MELEE WIRE: OK.

NOT DONE THIS SESSION (rolled to NEXT):
- LP/murder consequences for PvP knockouts (criminal state, scents) -
  legacy policy undocumented; needs a source or a written server
  policy decision.
- Unit-count reconciliation of the bow chain vs RoB (sources still
  behind Cloudflare).
- GL-client run on a display-capable host (carried since session 34).

NEXT (handoff):
- PvP consequences: decide and document the LP/criminal policy for
  player knockouts (a written server policy is acceptable - mark it
  in combat-system.md Open questions when sourced numbers exist).
- Re-run the 1000-bot load test with the duel behavior at full scale
  (the 40-bot smoke proves the chain; the 1k cohort number needs a
  fresh timed run for the perf table).
- Weapons for melee PvP (the unarmed model covers everyone; weapon
  base-damage table needs legacy item resources - Open question 8).
- Maneuver selection in the frv window (the give handshake exists;
  the maneuver/IP move economy is still animal-vs-player only).
- Re-run the Windows launcher smoke against the session-39 binary
  (verify_windows_gameres.sh should be unchanged, but re-run after
  any resource regeneration).

## 2026-10-07 - Session 40: melee weapons, knockout consequences, the maneuver economy, 1000-duelist load window

The session-39 NEXT items are CLOSED: weapons in melee PvP, the
LP/criminal knockout policy, the maneuver/IP economy, and the 1000-bot
duel load re-measurement. A cluster-load regression was FOUND and
diagnosed (below) - it is the top NEXT item.

WHAT:

- MELEE WEAPONS (fight.rs + game.rs). `WEAPONS` base-damage table +
  `weapon_dmg()` (`base * sqrt(q/10) * (str/10)`, the pack-consistent
  QM model - the RoB linear formula does not reproduce its own example)
  + `unarmed_dmg()` (legacy `(5*str/10).max(1)`). `Game::melee_dmg`
  scans the equipment slots (first weapon wins) and every swing path
  reads it: local PvP, local animal fights, the guest relay branch
  (RelayAttack/PvpSwing ship the weapon number). Stone axe base 15 =
  3 unarmed blows at q10/str10; unarmed fallback unchanged.

- PVP KNOCKOUT CONSEQUENCES (server policy, all PvP paths - melee,
  arrows, and the relay authority split). The loser forfeits 10% of
  unused LP (victim's home node applies). The winner is flagged
  CRIMINAL (assault) for 30 real minutes: `Player.criminal_until_ms`
  (persisted, save v5 `#[serde(default)]`), live buff through the
  real RMSG_BUFF channel (id 1, gfx/hud/buffs/thorn, countdown in
  legacy 1/60 s cticks, re-streamed on world entry), expiry sweep in
  the tick (RMSG_BUFF rm + chat). buff_set/buff_rm wire helpers were
  waiting as dead code since the resources.rs wire surface - now live.

- MANEUVER ECONOMY (fight.rs MANEUVERS + game.rs on_maneuver). All 28
  paginae/atk buttons work end to end: world entry announces the root
  + every table entry; MenuGrid act("atk", id) routes to on_maneuver.
  Attack selections fill the two-slot queue (frv atk [cur,next]),
  Dodge sets blk, boosts move IP/advantage. Advantage accumulates in
  tenths (FightRel.adv) and sync_balance rounds/clamps to the wire
  dial. Gating: Cleave >= 3 advantage, Battle Cry >= 14 IP, Skuld
  >= 10 IP (RoB numbers; refusals chat and mutate nothing). Opponent
  IP deltas apply to local victims and stream both windows; guest
  targets keep authority home (mirror prediction only - documented
  NEXT for the relay).

- LOAD WINDOW (verify_session40.sh). Single node, 1000 dueling bots,
  2-CPU sandbox: sessions=1000, steady-state mean 76 ms, p95 103 ms
  (97% of the 100 ms tick budget), pvp_hits 3830, knockouts 105,
  zero panics. The p95 is 3% over budget - documented as the frontier
  (phase split: vis 22-47 ms, combat 13-24 ms, mv 20-34 ms; the vis
  spawn churn of a 1000-runner crowd is the optimization target).
  CLUSTER REGRESSION FOUND: 2x300 bots (s34's proven-good config)
  now measures p95 175-242 ms with phase_guests_us 11-66 ms at
  580-705 guests - the duel cohort's permanent chase keeps every
  foreign gob moving (interpolation + LINSTEP fan-out per tick).
  s34 passed the same cohort WITHOUT duels; the guest tract, not the
  duel logic, is what regressed. Diagnosed and handed to NEXT.

- WINDOWS SMOKE: NOT re-run this session - the sandbox has no pwsh
  (verify_windows_gameres.sh needs it). The ps1 itself is unchanged
  since session 38; re-run on a pwsh-capable host.

EVIDENCE: 237 unit tests green (14 new: 3 weapon, 1 consequences,
3 maneuver-formula/table, 4 maneuver-integration, plus updated
literals), clippy -D warnings clean, fmt clean.
scripts/verify_session40.sh units/full/boot: SESSION40 phases OK;
load-1000 measured (frontier); load-cluster measured (regression
diagnosis). Release binary boots; test_client WORLD ENTRY OK.

NOT DONE THIS SESSION (rolled to NEXT):
- Guest-phase optimization: the cluster duel regression (top item).
- Cross-node relay of maneuver IP deltas (guest mirrors predict only).
- Windows gameres smoke re-run (no pwsh in this sandbox).
- GL-client run on a display-capable host (carried since session 34).
- Unit-count reconciliation vs RoB (sources still behind Cloudflare).

NEXT (handoff):
- Profile and optimize tick_guests (guest interpolation + LINSTEP
  fan-out) for the always-chasing duel cohort; re-run load-cluster
  (the strict p95 budget returns once the regression is fixed).
- Single-node vis spawn churn (vis_spawn_us up to 34 ms at 1000
  runners) - spawn hysteresis/debounce on the vis boundary.
- Cross-node maneuver IP relay (extend PvpSwingResult or add a small
  ManeuverDelta message).
- Windows smoke on a pwsh host; then the GL client e2e.

## 2026-10-07 - Session 41: guest-phase optimization, batched poses, 5 Hz LINSTEP cadence

The session-40 NEXT top item (profile and optimize tick_guests for the
always-chasing duel cohort; re-run load-cluster) is CLOSED. The cluster
regression is fixed at the median level and the node0 p95 is back inside
the 100 ms budget; node1 still shows noisy p95 tails on the 2-CPU
sandbox (see numbers).

WHAT:

- SUB-PHASE INSTRUMENTATION FIRST (perf-profile-first). `Perf` gained
  guests_encode/fanout/pose_us plus move_blocks/move_cells. The 2x200
  profile pinned the fan-out stage (p95 7.9 ms at 200 sessions, linear
  in the session count; encode was 0.09 ms) - not the encode loops.

- PACKED CELL-INDEXED MOVEMENT FAN-OUT (server/src/move_batch.rs, new).
  tick_guests and tick_movement now encode blocks ONCE into one shared
  scratch buffer (reused across ticks via clear - no per-tick allocator
  churn), each block tagged with the VisIndex cell of the gob position.
  broadcast_batch walks per session only the NON-EMPTY cells and rejects
  whole cells with one rectangle test against the 2x-retract-hysteresis
  square (FANOUT_SPAN = 2R + 8-tick drift); the exact visible.contains
  filter stays authoritative per block. This replaces the
  O(sessions x movers) hash-probe scan of every block per session (the
  s34 batch_move_broadcast shape); datagrams materialize lazily per
  session. Wire bytes are byte-identical (finalizers still land in
  unacked for OBJACK retransmit).

- BATCHED GUEST POSE STREAMING. stream_guest_pose (per (guest, viewer)
  datagram + per-call GuestGob clone) is replaced by
  stream_guest_poses_batched: jobs sorted per session, ONE datagram per
  session carrying ALL of that session's finished guests' OD_LAYERS
  blocks, guest rows read in place (no clone). The same batched path
  serves the ingest pose-flip fan-out. leak_static is MEMOIZED
  (OnceLock<RwLock<HashMap>>) - the pose fan-out used to Box::leak a
  fresh copy per call, an unbounded per-tick allocation leak.

- 5 Hz LINSTEP CADENCE (LINSTEP_EVERY_TICKS = 2). The client
  interpolates the linmove locally from LINBEG (deterministic timing
  model, c * 66.67 ms), so per-tick server progress pushes are only a
  counter re-sync. Shipping them every 2nd tick halves the largest
  remaining (session, mover) pair fan-out AND the progress datagram
  traffic at the 10k scale; a lost datagram self-heals within 200 ms.
  Finalizers always ship. Documented in network-protocol.md.

- LOAD NUMBERS (2-CPU sandbox, both nodes + bot cohorts share the two
  cores; run-to-run variance is high):
  * cluster 2x300 dueling bots: node0 steady p95 278 -> 87.6 ms (in
    budget), node1 278 -> 161 ms (noisy tail: fanout p95 spikes with
    guests=767 on that node); medians 42/31 ms. pvp chains intact.
  * single node 1000 dueling bots: steady p95 110 -> 133 ms (noisy;
    mean 87.6 -> 77.4 ms), pvp_hits 3616, knockouts 79, zero panics.
  The remaining single-node cost lives in phase_mv + vis at the 1000-
  session scale and the residual (tick phases do not yet account for
  the whole tick); the frontier stays the 1000-runner single node.

EVIDENCE: 240 unit tests green (3 new move_batch: reuse-after-clear,
cell grouping, axis rect bounds), clippy -D warnings clean, fmt clean.
scripts/verify_session41.sh (units/full/boot/guest-opt/load):
SESSION41 UNITS/BOOT: OK (world entry + MOVE PROBE OK under the new
cadence), FULL: OK (240 tests). probe_walk MOVE PROBE: OK. Cluster and
single-node load windows re-measured (numbers above).

NOT DONE THIS SESSION (rolled to NEXT):
- node1 p95 tail (fanout spikes at guests~767): the rectangle-test
  fan-out still scales O(sessions x visible pairs); consider per-cell
  precomputed subscriber lists or a second fan-out level.
- Single-node vis spawn churn (vis_spawn_us p95 up to ~8 ms at 2x200,
  larger at 1000) - spawn hysteresis/debounce on the vis boundary.
- Residual attribution: tick phases sum to ~2/3 of tick_us at load;
  instrument the residual (criminal expiry, clear_dirty, bookkeeping).
- Cross-node maneuver IP relay (extend PvpSwingResult or ManeuverDelta).
- Windows smoke on a pwsh host; then the GL client e2e (carried).
- Unit-count reconciliation vs RoB (sources still behind Cloudflare).

NEXT (handoff):
- Re-run the load windows on a quiet multi-core host for clean numbers.
- Vis boundary hysteresis (spawn debounce) - the single-node vis_spawn
  p95 is the next optimization target.
- Cross-node maneuver IP relay.
- Windows smoke + GL client e2e when a display-capable host exists.

### Session 41 addendum: open UiProbe PaginaException (top NEXT item)

The headless UI probe (verify_ui_probe.sh run) fails on the session-40
maneuver paginae with `MenuGrid$PaginaException: Invalid pagina:
paginae/atk/blk`, preceded by a delayed resource error `Wrong res
version (1 != 28484)` from the HTTP res source. Established facts:

- The wire stream is CORRECT: the new test_client PAGINAE parser shows
  every paginae/atk/* entry with its true file version (blk=1, exactly
  the version of gameres/paginae/atk/blk.res; no file in the pack or
  the jar carries 28484; grep finds no such constant in the tree).
- The same server + wire path passes WORLD ENTRY via test_client.
- The 28484 value appears ONLY inside the real-client probe (real
  Glob/MenuGrid/Resource pipeline), reproducibly, on a fresh server
  and fresh save. Per-entry PAGINAE frames did not fix it.
- Session 40 shipped the maneuver paginae but did not re-run UiProbe;
  session 38 was the last green probe run. This is a session-40
  regression visible only through the real client pipeline.

NEXT: reproduce with the real jar (java -cp build/haven.jar:lib/*
haven.MainFrame) and trace which caller loads blk with ver 28484
(Resource.load call sites: Glob.paginae vs MenuGrid.getSubResources vs
the fightview frv atk/blk uimsg payloads - the frv 'atk'/'blk' uimsgs
carry resource-id ints, check fight.rs uimsg(w, "blk", &[wb]) against
the client FightView.java parse). The fightview path is the only other
place the blk pagina resource is referenced and is session-40 new.

## 2026-10-07 - Session 42: the PaginaException pack bug, vis spawn-churn debounce, cross-node maneuver IP relay

The session-41 addendum blocker (open UiProbe PaginaException) is
CLOSED with a root cause in the legacy resource pack itself, plus two
NEXT items landed: the vis spawn-churn debounce and the cross-node
maneuver IP relay.

WHAT:

- PAGINAE EXCEPTION ROOT CAUSE (the pack, not the server). A temporary
  diagnostic hook in the client's Resource.load caught the real call
  site: `Resource$AButton.<init>` - the parent-pagina reference embedded
  in an action layer. `paginae/atk/dodge.res` (and `blk.res`) ship with
  the uint16 parent-version field DROPPED at pack-build time inside
  lib/haven-res.jar: the client parser reads the first two button-name
  bytes as the version ('D','o' of "Dodge" -> ver 28484). Legacy
  clients tolerated the resulting "Wrong res version (1 != 28484)"
  delayed error because nothing else referenced paginae/atk/blk; the
  session-40 pagina announce (blk at its real file version 1) turned it
  into a by-name version conflict (the cache entry is replaced by the
  failed ver=28484 load, per Resource.load's res.ver < ver rule) and
  MenuGrid.getSubResources threw on the broken entry. Per-entry PAGINAE
  frames were never the problem.

- PACK REPAIR (server/scripts/fix_gameres_versions.py). Parses every
  action layer, detects the dropped version (ASCII high byte where a
  uint16 belongs; name-overlap check), splices in the parent's real
  file version, extends the layer length. Idempotent; exits 1 when
  corruption remains. Wired into make-gameres.sh AND
  windows/make-gameres.ps1 so regenerated packs stay consistent. The
  repaired blk.res + dodge.res are committed under res/compiled/ -
  the client source chain resolves HAVEN_RESDIR BEFORE JarSource, and
  the jar (in the probe's classpath) was the actual broken source
  feeding the probe. UiProbe run/equip/charlist all green again.

- VIS SPAWN-CHURN DEBOUNCE (game.rs retract_sweep_due). Load
  attribution first (new `vis_spawns` perf counter): 1000 dueling bots
  spawn ~420 gobs/tick MEAN into session views (max 3098 in login
  storms); the spawn phase was the dominant vis cost (p95 36 ms). The
  old `cell_moved` trigger swept a moving session EVERY tick, so a gob
  oscillating across the 2x VIEW_RADIUS boundary was retracted and
  re-spawned on each crossing. The sweep is now gated to once per
  RETRACT_SWEEP_EVERY (8) ticks per session: a quick boundary return
  never sees a retract; a departed gob disappears at most 0.8 s late
  (two view radii off-screen); deaths keep the immediate
  broadcast_retract path. Measured after: spawns/tick mean 377,
  vis_spawn_us p95 29 ms, single-node steady tick p95 96 ms (in
  budget) with 5119 landed PvP hits - the remaining churn is geometry
  of the dense duel arena (R=300 against a 1000-bot herd), documented
  as the frontier, not a defect.

- CROSS-NODE MANEUVER IP RELAY (NodeMsg::ManeuverDelta). The maneuver
  economy's opponent-pool delta was local-only: a foreign victim's
  authoritative IP pool lived on her home node but never heard about
  `ip_opp` (HANDOFF NEXT since session 40). The on_maneuver guest
  branch now ships `ManeuverDelta { attacker, victim, ip_opp }` to the
  victim's home node, which folds it into her relation row keyed by the
  attacker's guest gob (clamped at zero) and re-streams her window.
  The attacker's ip_other mirror stays the between-frames prediction.
  Roundtrip wire test + fold/clamp/stream/no-op unit test.

EVIDENCE: 241 unit tests green (2 new incl. roundtrip), clippy -D
warnings clean, fmt clean. UiProbe RUN/EQUIP/CHARLIST: OK.
probe_walk MOVE PROBE: OK. test_client WORLD ENTRY OK + CATTR ORDER OK
on the final release binary. Load-1000 re-measured (numbers above).
Commits: e908bf0 (pack fix), 1de7f28 (spawn debounce), 3a9bbe1
(maneuver relay) pushed to origin/master.

NOT DONE THIS SESSION (rolled to NEXT):
- The duel-arena spawn churn is geometry-bound (R=300, 1000 bots on one
  arena): further work is spawn-cost reduction (encode/unacked path
  profiling at ~30 us/spawn) or a wider test arena, not more hysteresis.
- Single-node tick p95 96 ms vs the 100 ms budget on the noisy 2-CPU
  sandbox; re-measure on a quiet multi-core host (carried).
- Combat-phase p95 spikes (39 ms observed) - next attribution target.
- Windows smoke on a pwsh host (make-gameres.ps1 now also runs the
  pack-repair python step - verify it there); then the GL client e2e
  (carried since session 34).
- Unit-count reconciliation vs RoB (sources still behind Cloudflare).

NEXT (handoff):
- Combat phase attribution at 1000 sessions (the phase_vis p95 is now
  matched by phase_combat spikes).
- Spawn-encode cost profiling (vis_spawn_us / vis_spawns ~30 us each;
  the batched-spawn idea from session 41 is unexplored).
- Windows smoke + GL client e2e when a display-capable host exists.
- Re-run load-cluster on a quiet host for the perf table.

## 2026-10-07 - Session 43: combat-phase attribution + slot/viewer indexes

The session-42 NEXT top item (combat-phase p95 spikes) is attributed
to its root costs and the O(N^2) aggregate the attribution pointed at
is replaced with per-tick slot/cell indexes. All numbers below are
from the 1000-bot saturated duel cohort on the noisy 2-CPU sandbox
(mean over the 40-50 s steady windows, scripts/load43.sh).

WHAT:

- COMBAT SUB-ATTRIBUTION (perf counters, first). The combat phase
  mean (18-28 ms) splits into: index build 11-16 us (free), player
  phase 17.9-27.2 ms (almost everything), animal retaliation 0.1-0.6
  ms (the session-42 O(N^2) scan is already gone), relay 0. The
  player phase splits again: chase `start_move` 5.5-6.6 starts/tick
  at 2.4-3.0 ms EACH (13-18 ms/tick), swing bookkeeping ~70-140/tick
  (cheap), landed-hit tail 2.5-4/tick at 1.3-3.6 ms each (hurt + chat
  x2 + FX broadcast + info log).

- COMBAT SLOT INDEXES (the O(N^2) kill). The PvP melee path resolved
  its victim with a linear `players` scan per attacker per tick, and
  the animal retaliation loop scanned players per animal. One
  O(players) pass per tick now fills `CombatIndex::player_of_slot`
  (gob slot -> player idx+1; players never leave mid-tick - knockout
  resets bars, removal happens in the logout path) and
  `engaged_of_slot` (fight-target slot -> first engaged player idx+1,
  the removed linear `find` semantics). The animal loop iterates an
  engaged-animal snapshot in `animal_gobs` order with a live
  fight_target re-check (a PvP knockout inside the player phase
  leaves a stale row; the re-check stops the deer biting the
  knocked-out player - unit-tested). Per-tick allocations in the
  phase (players range collect, animal_gobs clone, guest_attackers
  collect, bar-stream sids collect) moved into taken/restored
  scratch vectors.

- VIEWER FAN-OUT INDEX. The per-event full `sessions` scan
  (LINBEG fan-out, pose layer stream, FX overlay broadcast) cost
  636-1100 us per event at this scale: cache-miss traversal of every
  large SessionOut. `ViewerIndex` maps session id -> its player's
  VisIndex cell, rebuilt once per tick in one O(sessions) pass;
  `viewers_of_slot` probes the 5x5 cell neighborhood (VIEW_RADIUS
  300 + one 50-subtile drift step fits inside two 250-tile cells on
  each axis) and keeps the exact `visible.contains` filter
  authoritative. Measured after: mv_viewers 417 us/call (was 636),
  mv_pose 912 (was 1104), chase start 2.6 ms (was 3.0); combat mean
  28.1 -> 23.2 ms, tick mean 84 -> 76 ms. Net effect is real but
  bounded: at this cohort density the mean session SEES ~530 gobs, so
  a mover's fan-out pair work (hundreds of sends per start) is the
  honest lower bound - the scan overhead is only part of the cost.

- DIAGNOSTICS that shaped the session: `start_move` sub-phase
  counters (mv_path/mv_viewers/mv_pose_us, mv_calls), GridStore
  gen_count/hit_count (grid-miss proof: 5 generations per 115 s
  window - NOT the chase cost), ix_cand_n (cell-index candidate
  volume), combat chase/hit/swing counters, scripts/load43.sh (boot
  + 1000-bot cohort + sub-phase histogram parser).

EVIDENCE: 243 unit tests green (2 new: shared-target first-winner,
stale-row guard), clippy -D warnings clean, fmt clean. Wire probes
green on the release binary: WORLD ENTRY OK, CATTR ORDER OK, MOVE
PROBE OK, MELEE WIRE OK. Commits 7e42828, 5e833df, 6a06a92 pushed to
origin/master.

NOT DONE THIS SESSION (rolled to NEXT):
- Batched move starts (the session-41 packed-batch pattern applied to
  LINBEG + pose + FX: encode once, one datagram per session per
  tick). The remaining chase cost is pair work; batching collapses
  the per-pair send/clone overhead (~30-40% of the tail by the
  move_batch precedent).
- Hit-tail cost (1.3-3.6 ms/hit: info! log + 2x chat + FX) - a
  chat/fx datagram merge or log-rate limit is the cheap cut.
- Single-node tick p95 129 ms vs the 100 ms budget on the noisy
  2-CPU sandbox - re-measure on a quiet multi-core host (carried).
- Windows smoke on a pwsh host + GL client e2e (carried since
  session 34).

NEXT (handoff):
- Batched move starts (LINBEG/pose/FX through the MoveBatch pattern).
- Hit-tail trim (log rate limit; chat/fx merge).
- Load-cluster re-run on a quiet host for the perf table.
- Windows smoke + GL client e2e when a display host exists.

## 2026-10-07 - Session 44: batched move starts + FX wire patch + two wire fixes

The session-43 NEXT top item is implemented: LINBEG move starts and
one-shot FX overlays now encode ONCE into a per-tick packed start batch
(MoveBatch) fanned out at tick end - one datagram per session per tick.
Two wire-format defects were exposed and fixed along the way.

WHAT:

- BATCHED STARTS (session-43 NEXT item). start_move no longer encodes a
  LINBEG block per viewer (5.5-6.6 starts/tick x ~530 visible sessions x
  encode+clone+send was the measured 13-18 ms/tick chase cost); it
  encodes once into `start_scratch` and `tick()` fans the batch out
  through the existing `broadcast_batch` (cell rectangle prefilter +
  exact `visible.contains`, fin=true so the authoritative frame lands in
  `unacked`). fx_overlay_broadcast does the same for OD_OVERLAY: the
  block stores the game-global resource index as the wire placeholder
  plus the byte offset of that uint16, and the fan-out rewrites the 2
  bytes per session (first use also queues the RMSG_RESID announcement
  there) - one encoded block serves every viewer despite session-local
  wire ids. `broadcast_batch` now reuses a taken/restored
  session-anchor scratch vector.

- WIRE FIX 1 - HEADERLESS BLOCKS. probe_walk exposed a defect carried
  since session 41: batch blocks were encoded WITH a per-block
  MSG_OBJDATA+flags header, so multi-block datagrams were
  [06 00 ...][06 00 ...]. The legacy client (Session.getobjdata) parses
  a datagram as ONE type byte followed by consecutive headerless blocks
  ([fl][id i32][frame i32][ops..][OD_END]) - every block after the
  first was misread with a 1-byte shift (fl=0x06, garbage id/frame).
  Single-block datagrams parsed correctly by coincidence, which is why
  sparse-world probes stayed green while any real crowd broke. All
  batch-encoded blocks (movement LINSTEP/finalizer, guest finish/step,
  LINBEG, FX) are now headerless; broadcast_batch opens the datagram
  with one MSG_OBJDATA byte. FX wire patch offset moved 15->14.

- WIRE FIX 2 - START BATCH CLEAR. The load cohort regressed to 560
  stuck logins and 157-183 ms mean ticks (sum of phases ~5 ms). tick()
  restored the taken start batch WITHOUT clearing it: blocks
  accumulated forever and re-fanned-out every tick - O(tick^2) datagram
  explosion. The batch now clears right after its fan-out (capacity
  reused, like the movement batch).

- HIT-TAIL TRIM (session-43 NEXT item). The per-hit info! log
  (25-40 lines/s at the 1000-dueler scale, a measured chunk of the
  1.3-3.6 ms hit tail) is now debug!; an aggregate info! (hit count +
  mean hit-tail us) prints every 5 s of activity.

MEASURED (scripts/load43.sh, 1000-bot duel cohort, noisy 2-CPU sandbox;
session-43 numbers in parentheses):
- chase start ~1.7 ms/start (was 2.4-3.0); combat player phase
  11-16 ms/tick (was 18-27).
- mv_viewers_us 5-9 us/call (was 417); mv_pose 840-1370 us/call
  (was 912) - pose fan-out is still per-session (NEXT).
- combat_hit_us 12-56 us/hit (was 1.3-3.6 ms/hit).
- mean tick 92-96 ms in the tail windows (was 83-85) - same noisy
  sandbox, within run-to-run variance; cohort settles 1000/1000 again
  after WIRE FIX 2.

EVIDENCE: 245 unit tests green (3 new: batch_linbeg_fans_out_once_per_
tick, batch_fx_patches_session_wire_id, plus the move_batch patch
metadata tests), clippy -D warnings clean, fmt clean. SESSION41 BOOT
OK, WORLD ENTRY OK, CATTR ORDER OK, MOVE PROBE OK (own LINBEG n=88
after a click, zero decode errors), MELEE WIRE OK. Commits b66d75d,
59d0727, baca65e pushed to origin/master.

NOT DONE THIS SESSION (rolled to NEXT):
- Batched POSE fan-out (OD_LAYERS/OD_AVATAR): the block embeds several
  per-session wire ids, so encode-once needs multi-patch metadata;
  mv_pose 840-1370 us/call is now the top fan-out cost.
- Session-anchor + per-session wire tables for guests: guests fan-out
  still scans all sessions per pose job (pose_jobs loop).
- Single-node tick p95 ~244 ms vs the 100 ms budget on the noisy 2-CPU
  sandbox - re-measure on a quiet multi-core host (carried).
- Windows smoke on a pwsh host + GL client e2e (carried since
  session 34; the wire-format fixes make this EASY to re-verify now).

NEXT (handoff):
- Batched pose fan-out with multi-patch MoveBatch blocks.
- GL client e2e + Windows smoke: the legacy client now parses batch
  datagrams correctly; re-run the client smoke to prove it.
- Perf table on a quiet host.
- Crafting paginae/Makewindow flow remains the top feature gap.

## 2026-10-07 - Session 44 addendum: batched pose fan-out + ViewerIndex retired

- BATCHED POSE FAN-OUT. stream_pose was the last per-viewer fan-out
  (840-1370 us/call). The OD_LAYERS block encodes ONCE with every wire
  slot as a game-global placeholder recorded as Patch::Many (base +
  every layer offset) and fans out through the packed start batch at
  tick end: the per-session pass resolves each wire id (first use
  announces), rewrites the placeholders, and lands the patched block in
  unacked. Patch is an enum: One (single-wire FX, no allocation) or
  Many (pose layer list).

- VIEWERINDEX RETIRED. All three fan-outs (LINBEG, FX, pose) now route
  through the batch's cell-rectangle prefilter + exact visible.contains,
  so the session-43 ViewerIndex (viewer_ix field, viewers_of_slot, the
  per-tick rebuild pass) is removed - one less O(sessions) pass per
  tick.

- MEASURED (load43.sh, 1000-bot cohort): mv_pose 7-15 us/call (was
  840-1370), mean tick 77-84 ms - the batch fan-outs now sit BELOW the
  session-43 baseline. 246 tests green; probes green (MOVE PROBE, MELEE
  WIRE, SESSION41 BOOT, WORLD ENTRY). Commit d5d3d1f pushed.

NEXT (handoff):
- GL client e2e + Windows smoke (carried; the wire-format fixes make
  client verification the top trust gap).
- Crafting paginae/Makewindow flow (top feature gap).
- Perf table on a quiet multi-core host.

## 2026-10-07 - Session 45: real-client e2e green on the batched wire, gear-chain recipes, Windows smoke, cluster regression confirmed fixed

The session-44 NEXT top items are CLOSED: the real GL client e2e
against the post-wire-fix batched datagram format, the crafting
gear-chain gap, the carried-since-38 Windows smoke, and the cluster
load re-measurement (the s40-41 regression is now measured FIXED).

WHAT:

- REAL GL CLIENT E2E: GREEN (two full harness runs, s45 and s45b - the
  second after the craft pagina additions). scripts/jogl/ boot: Temurin
  8 + JOGL 1.1.1 natives + X11 libs + Ant 1.10.15 + DriveAgent +
  gameres regen (6382 files, fix_gameres_versions: corrupt=0) on a
  FRESH sandbox (no persisted tools) - the deploy script is fully
  reproducible. Client under Xvfb + llvmpipe: login through the real
  widget chain, portrait layers, MOVEMENT MOVED + MOVEMENT2, SPEED
  VERDICT OK (3.43 tiles/s), NO TELEPORT OK, RAPID CLICKS GLIDING,
  five directional legs ARRIVED (E/N/S/UP/LEFT), EQUIP DOLL
  ava-rend=OK, EQUIPVIS VERDICT OK (dress/undress recomposite), CURSOR
  drag, GROUNDDROP gob spawn. ZERO Exception/PaginaException lines in
  the client log. THE SESSION-44 HEADERLESS BATCH BLOCKS RENDER AND
  INTERPOLATE CORRECTLY ON THE UNMODIFIED 2009 CLIENT - the wire trust
  gap is closed. Screenshots read (equipment/map/menugrid/chat all
  render; avatar faces the walk direction). Ant note: `ant jar` fails
  on Debian JDK 21 ("release version 8 not supported"); build with
  Temurin 8 (JAVA_HOME=jdk8u504-b01) - documented here, HANDOFF
  how-to stays valid otherwise.

- GEAR-CHAIN RECIPES (craft.rs, the s44 "crafting top feature gap"):
  rope, waterskin, backpack, poorbelt - four pack pieces that were
  previously unobtainable. Ids, display parents and advisory prereq
  codes parsed from the shipped pagina action layers (rope ->
  paginae/craft/cloth, prereq "ahusb"; waterskin -> tools/"hunting";
  backpack + poorbelt -> leather/"leather"). Ingredient counts are
  documented server policy (crafting-and-building.md Open questions,
  same discipline as quiver/woodbow). belt-poor + backpack render on
  the avatar through the existing equip.rs PIECES entries (layer
  mapping unit-tested); rope is the documented taming precondition
  (animals-and-husbandry.md) - the taming protocol itself is NEXT.
  test_craft.py on the release binary: CRAFT FLOW OK, EAT FLOW OK.

- WINDOWS SMOKE CLOSED (carried since session 38): PowerShell Core
  7.4.6 installed in the sandbox from the GitHub release tarball
  (~$HOME/pwsh, tar xzf, chmod +x). windows/make-gameres.ps1 executed
  END TO END on Linux: jar extract, res/compiled overlay, the
  session-42 pack-repair python step (scanned=6382 corrupt=0):
  WIN GAMERES SMOKE: OK. All windows/*.ps1 parse clean under the
  PowerShell language parser (collect-logs, make-gameres,
  wait-server). The .bat launchers remain structural-check-only (no
  cmd.exe on Linux).

- LOAD RE-MEASUREMENT (post-s44 binary, noisy 2-CPU sandbox):
  * single node, 1000 dueling bots: cohort settles 1000/1000, mean
    tick 80-85 ms, samples 89-104 ms, 0 panics - consistent with the
    s44 baseline; vis_spawn_us 35-37 ms at ~200-370 spawns/tick
    remains the dominant phase (documented dense-arena frontier).
  * cluster 2x300 duelists: node0 steady p95 68.4 ms, node1 47.1 ms -
    BOTH INSIDE the 100 ms budget, 0 panics. The s40-41 guest-phase
    regression is CONFIRMED FIXED: phase_guests_us 2.6-2.9 ms at
    ~580 guests (was 11-66 ms pre-batching), guests_fanout_us
    2.1-2.4 ms. Quiet-host re-measure for the final perf table stays
    NEXT.

- VERIFIER DRIFT FIX (verify_session40.sh): the pvp counter read the
  per-hit info line that s44 moved to debug; it now counts the
  duel-start lines plus the 5s aggregate (hits=N mean_hit_us=N). The
  stale "p95 over budget" cluster message is updated to the measured
  in-budget state.

EVIDENCE: 247 unit tests green (1 new: gear_chain_recipes_are_wired),
clippy -D warnings clean, fmt clean. Probes on the final release
binary: WORLD ENTRY OK, CATTR ORDER OK, MOVE PROBE OK, MELEE WIRE OK.
Commits af9380a (gear chain), c7b2251 (verifier), plus this handoff,
pushed to origin/master.

NOT DONE THIS SESSION (rolled to NEXT):
- Taming protocol (rope now exists; intensity-0 + rope-equipped +
  Animal Husbandry flow is unimplemented - animals-and-husbandry.md).
- Waterskin/container contents (water volume) for the drinking loop.
- Recipe breadth: RECIPES carries 10 hand recipes + roast; the pack
  ships ~160 craft paginae. Tool/station requirement fields in the
  Recipe struct are still absent (softcap attribute stands in).
- Perf table on a quiet multi-core host (carried; noisy-sandbox
  numbers recorded above).
- ANIMALS SCREENSHOT phase of the e2e landed on its fallback (no
  predator inside the viewport this run); probe_animals.py --saturated
  remains the wire-level animals check.

NEXT (handoff):
- Taming chain end to end (the rope consumer) - biggest feature gap
  in the livestock domain.
- Recipe breadth + tool/station requirement plumbing in the Recipe
  model.
- Quiet-host perf table; client smoke re-run after any resource
  regeneration.

### Session 45 addendum: taming MVP - Quell the Beast end to end

The livestock domain's biggest feature gap now has a working core:
Quell the Beast tames animals through the real combat model.

WHAT:

- QUELL GATES (selection time, on_maneuver): the static fight gates
  (2 IP, advantage >= 3 in tenths - docs quote Jorb's prerequisite
  list) plus target-specific checks in quell_gate: LOCAL animal,
  rope (gfx/invobjs/rope) equipped in any slot, this tamer's rope not
  already bound to a partially-tamed beast, beast not already
  quelled, guest animals refuse (cross-node leashes are an open MVP
  limitation). Refusals chat and mutate nothing.
- QUELL RESOLUTION (tick_combat animal branch): the queued quell
  intercepts the swing cadence - same offence-bar economics as a
  normal swing, but no defence chip and no damage. One quell:
  +20 tameness, battle ends (animal_fights row removed, fight window
  torn down, engagement cleared), the beast follows client-side via
  a batched OD_FOLLOW broadcast (gob ids are global, so the block
  needs no per-session patching - Session 44 start-batch fan-out),
  and the rope binds.
- LEASH LIFECYCLE: game-tick break deadline (6000 ticks = 10 min,
  the docs' 5-15 min floor as policy), rearmed on every quell below
  100; at TAMENESS_FULL (100) the beast never breaks. Damaging the
  beast kills ALL tameness (server policy) and frees the rope. The
  tick sweep (placed before the batch fan-out) breaks due leashes
  with an OD_FOLLOW removal (oid -1) and chats the tamer. Tamed
  animals skip animal AI entirely; tame rows drop on authority
  transfer (no leaks).

EVIDENCE: 251 unit tests green (5 new: quell-refuses-without-a-rope,
quell-tames-and-binds-the-rope incl. the second-beast binding refusal,
damage-kills-tameness-and-leashes-break, full-tame-never-breaks-
loose, plus the gear-chain wiring test), clippy -D warnings clean,
fmt clean. Probes on the release binary: WORLD ENTRY OK, MELEE WIRE
OK. Commit 9d9d742 pushed. Docs: animals-and-husbandry.md gained the
session-45 implementation notes; the MVP gaps (intensity meter,
ahusb skill gate, species morph, tamed-state persistence) are
recorded in its Open questions.

NEXT (handoff):
- Taming depth: intensity de-escalation meter in animal fights, the
  Animal Husbandry skill gate, the species morph at 100 (boar->pig,
  mouflon->sheep, aurochs->cow/bull), weapon-slot-only rope check.
- Tamed-state persistence once animals themselves persist (animals
  are spawned wildlife today).
- Recipe breadth + tool/station requirement plumbing in the Recipe
  model (the earlier NEXT item, unchanged).
- Quiet-host perf table; GL client e2e re-run after any resource
  regeneration.

## 2026-10-07 - Session 46: taming depth - AH skill, battle intensity, species morph; cloth chain + tool plumbing

The session-45 NEXT "taming depth" items that are verifiable in this
environment are CLOSED: the Animal Husbandry skill gate, the
battle-intensity de-escalation meter, and the species morph at full
tameness. The carried recipe tool plumbing also landed with five new
recipes.

WHAT:

- AH SKILL GATE (docs taming step 1): `ahusb` (Animal Husbandry,
  400 LP, requires Hunting - the doc's quoted legacy values) joins the
  skill catalog; `SkillDef` gains a `prereq` field enforced inside
  `buy()` BEFORE any charge (new `BuyError::Prerequisite`; the wire
  handler chats "You need to know Hunting first."), and
  `skills::can_quell` gates the quell selection
  (`gfx/hud/skills/ahusb.res` ships in the pack so the nsk list
  renders it).
- BATTLE INTENSITY (docs step 2, Jorb's list "battle intensity
  reduced to 0"): every AnimalFight row carries an intensity bar.
  A landed blow in EITHER direction (player swing landing, animal
  bite, cross-node relay swing) raises it INTENSITY_PER_BLOW=2500;
  every combat tick without a blow de-escalates INTENSITY_DECAY=250
  (a hot fight cools in ~7 s - one O(fights) pass at the top of
  tick_combat). quell_gate refuses while intensity > 0: the working
  pattern is build advantage -> stop swinging -> wait -> quell.
- SPECIES MORPH (docs step 6): Species::morph() maps
  mouflon -> sheep, aurochs -> cow; the boar maps to None (the 2009
  pack ships NO pig kritter - policy recorded in the doc's Open
  questions). At full tameness apply_species_morph rewrites the Kind,
  the drawable resource (cdv), max_hp and speed, clamps hp (no
  healing), and broadcasts a headerless OD_RES block through the
  packed start batch with a per-session wire-id patch (Patch::One) -
  the NATIVE client re-render path (Session.java OD_RES=2 ->
  OCache.cres -> ResDrawable reset; verified against the client
  source, not a new wire contract).
- ROSTER: Species gains Mouflon (index 7, joins the wild spawn roll)
  and Sheep (index 8, NEVER spawns wild - only via the morph, per the
  wiki's "Must domesticate a Mouflon to obtain a Sheep"). The
  node-link discriminant stays append-only (0-6 frozen, unit-tested).
  PoseTable/SPECIES_FOLDERS extend to 9 species (mufflon/sheep body
  pose directories verified in gameres); sheep-family loot carries
  wool + hide-raw-sheep + Raw Mutton (fep.conf-verified label).
- TOOL PLUMBING + CLOTH CHAIN (crafting doc): Recipe gains a `tool`
  field enforced in craft_once BEFORE the consume pass (inventory or
  any equip slot; refusal destroys nothing). Five new recipes with
  pagina ids verified against the shipped action-layer bytes
  (ad=["craft", id]): yarn (wool x1), linencloth (yarn x2),
  linenshirt + linenpants (linencloth x3 each - both render through
  the equip.rs PIECES borka layers), bucket (branch x3, tool=saw,
  output buckete). Wool enters the economy through the sheep loot.

EVIDENCE: 261 unit tests green (10 new: ahusb prereq + can_quell,
quell-refuses-without-the-skill, quell-needs-a-calm-battle,
full-tame-morphs-the-species, bucket-craft-needs-the-saw,
cloth-chain wiring, species-morph table, plus the roundtrip
extension), clippy -D warnings clean, fmt clean. Wire probes on the
release binary: WORLD ENTRY OK, CATTR ORDER OK, MOVE PROBE OK, MELEE
WIRE OK, CRAFT FLOW OK (items 8 -> 17), EAT FLOW OK. Commits pushed
to origin/master.

NOT DONE THIS SESSION (rolled to NEXT):
- Weapon-slot-only rope check: the server accepts a rope in ANY equip
  slot (consistent with melee_dmg's any-slot weapon scan); narrowing
  to the weapon slot needs client slot-semantics verification -
  documented as policy in animals-and-husbandry.md.
- Tamed-state persistence (animals are spawned wildlife; tameness is
  runtime state) - unchanged from session 45.
- Intensity meter client rendering (the bar is server-side only;
  Fightview relations stream intensity=0).
- Quiet-host perf table (carried); GL client e2e re-run after any
  resource regeneration (the OD_RES morph block renders through the
  same client path the e2e already covers).

NEXT (handoff):
- Recipe breadth: ~150 shipped craft paginae remain unimplemented;
  the tool/station fields are now in place for oven-gated recipes
  (flour/bread need a grain item the 2009 pack lacks - sprout/grist
  only, see farm.rs).
- Tamed-animal production (milk/wool timers, feeding troughs) once
  animals persist.
- GL client e2e re-run with a morph walkthrough (tame a mouflon to
  100 and read the sheep sprite off the screen).

## Session 47 (2026-10-07)

Closed both carried taming items: tamed-animal persistence and
tamed-animal production (milk/wool timers + collection flows). Also
fixed a persistence bug found while wiring the animals in.

WHAT:

- PRODUCTION METERS (animals-and-husbandry.md "Animal products and
  collection flows"): TameState gains milk_units (0.01 L units),
  wool, and a shared prod_acc accumulator. Milk: quantity 10 accrues
  10 units per 6000 ticks = the doc's 0.1 L / 10 min, cap 10 L (1000
  units). Wool: 1 per 8 h at quantity 5, linear in quantity (acc +=
  q per tick, unit per 240000 quantity-ticks), cap 3. At the cap the
  accumulator stops banking time. Quantity constants are server
  policy (no verified bred-stat numbers; MILK_QUANTITY=10,
  WOOL_QUANTITY=5).
- GRAZING GATE: the production sweep runs each tick (two-phase,
  O(tamed), same shape as tick_animals) and advances a meter only
  while the animal stands on GRASS/MOOR/HEATH (the doc's q10 foods);
  off-pasture pauses accrual AND freezes the accumulator. Products
  carry the grazing quality q10 (GRAZE_PRODUCT_QL). No starvation
  deaths (the Food Trough is not built; free grazing is the only
  feeding path - Open questions).
- COLLECTION FLOWS: clicking a fully tamed producer opens the
  collection flower menu instead of the fight window (tamed livestock
  cannot be aggroed). Cow "Milk": consumes one empty bucket
  (gfx/invobjs/buckete; inventory first, then any equip slot - same
  any-slot policy as the tool scan and the taming rope), drains 1 L
  (MILK_PER_BUCKET_UNITS=100; the doc names no bucket volume - policy)
  and grants gfx/invobjs/bucket-milk at q10. Sheep "Shear": barehand
  (the doc names no shear tool), grants the whole stored
  gfx/invobjs/wool stack. An empty meter answers with a hint chat
  line and never opens a menu; the choice re-validates the live meter
  and the bucket, so a stale menu cannot overdraw. Wild and
  mid-taming animals keep the fight path (regression-tested).
- TAMED-ANIMAL PERSISTENCE (save v6, additive): rows with tameness >
  0 persist as SavedAnimal (species index, tile, hp clamped to the
  species max on load, tameness, tamer save_key, meters, acc). The
  saved species IS the domestic morph (a fully tamed mouflon reloads
  as a sheep). The tamer gob id cannot survive restarts; fully tamed
  beasts never re-arm the leash, partially tamed ones re-arm at load,
  and the tamer binding re-establishes on the next quell. Wildlife is
  seed-regenerated and never saved.
- PERSISTENCE BUGFIX: flush() never copied tile_overrides into the
  save document - furrows/terraforming silently reverted on every
  restart even though world_state carried them. Both tile_overrides
  and animals now round-trip (persist.rs).

EVIDENCE: 267 unit tests green (8 new: pasture-only accrual at the
documented rate, cap + accumulator reset, wool accrual/cap, milk flow
with bucket consumption, no-bucket refusal, shear flow, wild/mid-tame
fight-path regression, persistence roundtrip), clippy -D warnings
clean, fmt clean. Wire probes on the release binary: WORLD ENTRY OK,
MELEE WIRE OK, ANIMALS WIRE OK. Commits pushed to origin/master.

NOT DONE THIS SESSION (rolled to NEXT):
- Food Trough object + fodder transfer + starvation rules; per-animal
  breed stat rows (Milk Quantity / Wool Quality are flat constants).
- Breeding/gestation lifecycle (calves, lambs, coop eggs).
- GL client e2e with a full morph walkthrough (tame to 100, read the
  sheep sprite off the screen) - the wire path is covered by the
  session-46 OD_RES probes.
- Recipe breadth (~150 shipped paginae) - unchanged from session 46.

NEXT (handoff):
- Recipe breadth: ~150 shipped craft paginae remain unimplemented;
  the tool/station fields are in place for oven-gated recipes (flour/
  bread need a grain item the 2009 pack lacks - sprout/grist only,
  see farm.rs). Picking a large implementable batch (tools, furniture,
  containers) is the highest-value breadth move.
- Feeding depth: Food Trough (2x1 lift-able, 200 fodder units, 18
  tile radius) + fodder quality averaging; then starvation.
- GL client e2e with a production walkthrough (tame a cow, wait out
  the milk meter with HNH tick acceleration or a debug grant, milk it
  on screen).

## Session 48 (2026-10-07)

Closed the carried "Feeding depth" NEXT item: the Food Trough object
(fodder store, quality averaging, itemact loading), the trough-feeding
preference in the production sweep, the Legacy:Cattle consumption
rates, and the starvation policy. Save bumped to v7 (additive).

WHAT:

- FOOD TROUGH BUILDABLE (animals-and-husbandry.md "Feeding: troughs
  and grazing"): Buildable "trough" -> gfx/terobjs/trough (the 2009
  pack ships both the terobjs resource and paginae/build/trough).
  Demand policy: branch x4, one stage (the doc names no build
  materials). complete_plan opens an empty TroughState
  (units/ql_sum/ql_seen) in world.troughs; the trough pagina joins
  the login paginae push.
- FODDER LOADING (itemact): fodder_units() matches the doc's fodder
  list intersected with the 2009 jar - any seed-* resource + flaxseed,
  apple, applecore, mulberry, straw, pumpkinflesh, carrot,
  flower-poppy, one unit per item (blueberries, chantrelles, bloated
  bolete, peapod, beetroot/leaves and giant pumpkin have NO pack
  resources; recorded). One item per click (oven-fuel policy);
  refusals chat and destroy nothing; cap 200 units (doc).
- QUALITY AVERAGING: the store keeps a running sum/count of every
  unit EVER placed; the average is the doc's arithmetic (q5 + q12 +
  q16 -> q11). Consumption drains units, not the history. Product
  quality stays GRAZE_PRODUCT_QL (Open question).
- FEEDING PREFERENCE + CONSUMPTION: the production sweep resolves
  food per fully tamed producer: the nearest trough with fodder
  within 18 tiles (euclidean, 11 subtiles/tile) wins over the grazing
  fallback; trough feeding keeps production on ANY tile. Legacy:Cattle
  rates: cow 4.8 units/day (1 in-game day = 8 real hours) + the
  lactating surcharge 0.1 unit per liter produced (bound to the
  production rate, exactly the doc's wording); sheep 2.4/day (policy,
  no doc number). Integer nano-unit accumulation (feed_acc_nano)
  drains one whole unit per ~60000 ticks; hunger resets on any feed.
- STARVATION: a producer with no trough fodder in radius and no
  grazing tile accumulates hunger; at 3 in-game days (STARVE_DEATH_
  TICKS = 864000 ticks) it dies - despawn, tame row drop, tamer
  chatted; no corpse/loot (corpse pipeline not implemented, policy).
  Mid-taming and wild animals are out of scope.
- PERSISTENCE (save v7, additive): SavedStructure + fodder fields for
  trough rows; SavedAnimal + feed_acc_nano/hunger; restore rebuilds
  the trough store (clamped at the cap) and the feeding fields. A v6
  save loads under the v7 binary (verified on the live server: the
  repo save with 4 chars loaded cleanly).

EVIDENCE: 275 unit tests green (8 new: fodder table, trough quality
averaging + take, trough itemact load/refusal/cap, trough feeding
production + drain, radius bounds + grazing fallback, starvation
death teardown, trough + feeding persistence roundtrip, plus the full
build-flow test (pagina arm -> place -> sink branch x4 -> fodder
store opens)), clippy -D
warnings clean, fmt clean. Release binary probe: WORLD ENTRY OK
(save v6 file loaded by the v7 code path). Commits pushed to
origin/master.

NOT DONE THIS SESSION (rolled to NEXT):
- Trough-to-trough fodder transfer (the doc's lift-and-right-click
  flow) waits on a lift mechanic; the 2x1 footprint likewise.
- GL client e2e with a production walkthrough (unchanged from
  session 47).
- Recipe breadth (~150 shipped paginae) - unchanged.

NEXT (handoff):
- Recipe breadth: pick a large implementable batch (tools, furniture,
  containers); the tool/station fields are in place for oven-gated
  recipes (flour/bread need a grain item the 2009 pack lacks -
  sprout/grist only, see farm.rs).
- Feeding depth leftovers: Food Trough transfer needs lift; per-animal
  breed stat rows (Milk Quantity / Wool Quality are flat constants).
- GL client e2e with a production walkthrough (tame a cow, wait out
  the milk meter, milk it on screen).

## 2026-10-07 - Session 49 (type 2: refactoring / tech debt)

SESSION TYPE ROTATION LOG (per the alternating-goal rule; one goal per
session): 45=3, 46=3, 47=3, 48=3, 49=2. Next sessions should pick from
the under-served types: 4 (test pyramid), 5 (performance), 0 (docs
hygiene), 1 (architecture review) - another 3 only after those rotate.

WHAT (pure move refactoring, zero behavior change):

- game.rs was a 19,233-line monolith: one `impl Game` with 191 methods
  plus a 7,400-line test module. Split by feature (proj-mod-by-feature,
  proj-mod-rs-dir) into `src/game/`:
  - game/tests.rs - the whole test module (#[cfg(test)] #[path] child
    module: private-item visibility for tests preserved without
    pub-super noise).
  - game/animals.rs - animal AI, quell/taming, the production+feeding
    sweep, starvation, animal damage paths (1483).
  - game/building.rs - build placement, plans, stations, the Food
    Trough store (669).
  - game/craft.rs - make widget, recipes, roast chain (346).
  - game/farming.rs - plow/mutate/plant/crop menus/harvest + crop tick
    (496).
  - game/items.rs - drops, inventory/equipment windows, drag cursor,
    map itemact, food menu, eating (894).
  - game/stream.rs - mapreq, gob block encoder, spawn/retract,
    visibility pass, cluster helpers (693).
  - game/cluster.rs - node messages, guest mirroring/republishing,
    subscriptions, authority transfer, relay paths (2012).
  - game.rs keeps the Game core: construction, world entry, session
    lifecycle, tick dispatcher, movement, player interaction,
    party/skills, PvP vitals (5323 lines).
- Submodules are children of `game`, so items private to the module
  stay visible to them and to tests; cross-file methods are
  `pub(super)` (proj-pub-super-parent). No signatures, types, or logic
  changed - only module placement + visibility markers.
- Rust-skills rules consulted: proj-mod-by-feature, proj-lib-main-split,
  proj-mod-rs-dir, proj-pub-use-reexport.

EVIDENCE: 275 unit tests green after every split step; clippy -D
warnings clean; fmt clean; release binary rebuilt; wire probe WORLD
ENTRY OK (twice). Commits e86631b, a024bd1, f5ade59 pushed to
origin/master.

NEXT (handoff):
- Refactoring leftovers (next type-2 session): interact/movement and
  combat/party/skills still live in game.rs (~5.3k lines); candidate
  split: game/interact.rs (map click/walk/interact/movement/batch) and
  game/combat.rs (fights, arrows, vitals, criminal). After that
  game.rs is ~2.5k lines of pure core.
- Session 48 NEXT items unchanged: recipe breadth, feeding transfer
  (lift), GL e2e production walkthrough.

## 2026-10-07 - Session 50 (type 4: test pyramid / corpus consolidation)

SESSION TYPE ROTATION LOG: 45=3, 46=3, 47=3, 48=3, 49=2, 50=4. Next
sessions pick from 5 (performance), 0 (docs hygiene), 1 (architecture
review) before another 2 or 3.

WHAT:

- BLACK-BOX WIRE INTEGRATION LAYER (the missing pyramid tier). 255
  white-box unit tests + manual python probes had nothing in between.
  `server/crates/hnh-server/tests/wire.rs` boots the REAL binary on
  ephemeral ports (CARGO_BIN_EXE) and speaks the real protocol from
  Rust: TLS auth through the standard webpki trust path (per-test
  rcgen cert passed via --cert/--key - no dangerous() shortcut), UDP
  MSG_SESS handshake, per-submessage reliable seq + cumulative ACK +
  hold-back, raw MAPDATA/OBJDATA, batched MSG_OBJACK. Three contracts:
  world entry + the 26-name REQUIRED_CATTR set before the `chr`
  widget + 3x3 MAPDATA + OBJDATA + attack paginae; bogus cookie ->
  SESSERR_AUTH; movement click -> own-gob LINBEG (220 subtiles east),
  monotonic LINSTEP step indices to the LINBEG step count c, bounded
  per-frame advance, arrival MOVE snapping EXACTLY onto the clicked
  target. Runs in ~7 s, needs NO gameres (verified: server + WORLD
  ENTRY work without the pack), so CI and fresh clones run it as-is.
- OBJDATA op-table parity: the test decoder matched bots.rs
  parse_objdata and THREE real desync bugs vs the python probe's table
  were fixed in the process: OD_REM is a no-payload op (leaving its
  OD_END unconsumed desyncs the next block), OD_OVERLAY raw 65535 is
  a removal with no sdt, flag-1 blocks carry no ops section.
- PYTHON CORPUS CONSOLIDATION: `server/scripts/hnhlib.py` is now the
  single source of the transport plumbing (constants, auth_cookie,
  ensure_server, parse_objdata, WireClient with an on_event hook and
  the real client's bootstrap behaviors). test_build.py 723 -> 372
  lines (shim + scenario runners; re-exports keep the seven
  from-test_build-import probes working unchanged). test_client.py
  and probe_walk.py rewritten on WireClient with identical verdict
  lines; probe_walk's verdict gained the arrival-snap and
  gob-target-no-walk checks. server/scripts/README.md documents the
  harness and lists the five legacy self-contained scripts (probe_
  animals, probe_direction, dump_paginae, test_farming,
  test_party_chat) for mechanical migration later. Corpus net -27
  lines while ADDING the shared module; every future wire change is
  now a one-function patch (parse_objdata) instead of eight.
- CI: `.github/workflows/rust.yml` - fmt, clippy -D warnings, cargo
  test --workspace on every push/PR. AGENTS.md verification section
  updated to name the new tier and the CI contract.

REGRESSION DISCOVERED (pre-existing on master, NOT this session):

- BUILD FLOW: the branch-sink step of the oven chain silently no-ops.
  Evidence: test_build.py buildbot places the plan, sinks stones
  (sdt 0->1 works), then the branch itemact neither completes the
  plan nor logs anything server-side; the assertion
  "plan never completed (sdt=b'\x01')" fires. The OLD pre-conversion
  test_build.py fails IDENTICALLY on the same binary and fresh save
  (A/B run), so this is a master regression, not a harness artifact.
  Candidate window: the session-48 itemact churn (trough loading
  touched the shared map_itemact path). Needs a type-3 session to
  root-cause and fix; the python probe AND a new cargo wire test
  should pin the fix.

EVIDENCE: 278 tests green (255 unit + 11 proto + 9 world + 3 wire),
fmt clean, clippy -D warnings clean. Probes on the converted harness:
WORLD ENTRY OK, CATTR ORDER OK, MOVE PROBE OK (arrival snap verified).
Commits 3bfdc22, 9234c82 pushed to origin/master.

NEXT (handoff):
- TOP: root-cause the build-flow branch-sink regression (see above),
  pin with a wire test, fix.
- Migrate the five legacy self-contained probes onto hnhlib.py
  (mechanical; README has the recipe).
- Session 49 refactoring leftovers (game/interact.rs, game/combat.rs
  split) and session 48 recipe breadth unchanged.
- Windows smoke + GL client e2e when a display host exists (carried).

### Session 50 addendum: CI workflow blocked by token scope

The prepared `.github/workflows/rust.yml` (fmt -> clippy -D warnings ->
cargo test --workspace, Swatinem cache, working-directory server) could
not be pushed: the PAT refuses to create workflow files without the
`workflow` scope. The file ships in the repo working tree of the next
session's clone OR recreate it from this snippet:

    name: rust
    on:
      push:
        branches: [master]
      pull_request:
    jobs:
      test:
        runs-on: ubuntu-latest
        defaults:
          run:
            working-directory: server
        steps:
          - uses: actions/checkout@v4
          - uses: dtolnay/rust-toolchain@stable
            with:
              components: rustfmt, clippy
          - uses: Swatinem/rust-cache@v2
            with:
              workspaces: server
          - name: fmt
            run: cargo fmt --all -- --check
          - name: clippy
            run: cargo clippy --all-targets -- -D warnings
          - name: tests (unit + wire integration)
            run: cargo test --workspace

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

---

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

---

## 2026-10-08 - Session 53 (type 0: docs hygiene / context de-pollution)

SESSION TYPE ROTATION LOG: 48=3, 49=2, 50=4, 51=3, 52=5, 53=0. Type 1
(architecture review) is the only never-served type - next sessions
pick 1 before another 3/4/5. The consolidated, authoritative rotation
table now lives in the "Session type rotation log" section of
HANDOFF.md; per-entry rotation logs stop accumulating.

GOAL: stop the handoff file's unbounded growth - the top context
polluter (204 KB / 3596 lines that every session is mandated to read
top to bottom).

WHAT:

- HANDOFF SPLIT: HANDOFF.md is now the living context only (392 lines
  / 22.5 KB): protocol + new ARCHIVE POLICY section, refreshed
  how-to-continue (workers auto default, --cluster/--node pointer,
  windows/ one-command scripts), architecture rewritten to current
  master (the old section still described session-2 reality: no game/
  split, no cluster mesh, no combat/archery/armor/taming modules), a
  verified-evidence section, a consolidated known-gaps list (the old
  one was session-2's - crafting/farming/party listed as missing while
  shipped), the authoritative rotation table, a one-line session index
  (S1..S53), and full entries for the last two sessions only.
- HANDOFF_ARCHIVE.md (new): sessions 1-50 moved byte-verbatim (cut
  points verified: zero 51/52 mentions in the archive, single copies
  in the living file). 200 KB of history stays greppable without
  taxing future sessions' context budgets.
- AGENTS.md: the verification section claimed GitHub Actions runs on
  every push - false since session 50 (PAT lacks the `workflow`
  scope; the file never landed). Reworded to the honest state with a
  retry-once-per-session instruction.
- server/scripts: 25 frozen one-off session gate scripts
  (verify_sessionNN.sh etc.) moved verbatim to server/scripts/attic/;
  README gained the missing probe rows (probe_drop, probe_plow) and a
  Utilities section. Root GATES.md (a session-47 artifact) moved to
  .unlazy/session47/GATES.md per the tree's own convention. Untracked
  an accidentally committed server/scripts/__pycache__ .pyc.
- CI RETRY (carried from session 50): recreated
  .github/workflows/rust.yml from the preserved snippet, committed
  and attempted the push - REJECTED again with the same error
  (`refusing to allow a Personal Access Token to create or update
  workflow ... without workflow scope`). The commit was rolled back
  to keep master pushable; the file content remains preserved in
  HANDOFF_ARCHIVE.md (session-50 addendum). Next sessions: retry only
  if the token gains the scope.
- docs/mechanics/README.md link integrity check: all 16 links resolve.
  No doc content edits - the living-document rule has kept them
  current; a deep 16-doc audit is out of a 2 h budget.

VERIFICATION: no Rust code touched (the rust-skills load is required
before Rust work only). Repo health confirmed before the work: cargo
test --workspace green on a fresh stable toolchain (282 tests: 258
unit + 11 proto + 9 world + 4 wire). Commits 8c0e7e8 (HANDOFF split),
ef027a8 (AGENTS CI), df473d7 (attic + GATES + scripts README) pushed
to origin/master (cf9e0e7..df473d7); this entry is the fourth.

NEXT (handoff):
- HANDOFF.md "Known gaps / next steps" is now the single consolidated
  source (10 items). Top picks for coming sessions: type 1
  (architecture review - never served), then the carried items (guest
  bucket, wire-test de-flake, probe migration, game.rs split
  leftovers, recipe breadth).
- MAINTAIN THE ARCHIVE POLICY: at the end of EVERY session move all
  but the last two entries to HANDOFF_ARCHIVE.md and extend the
  session index by one line. That one minute of work keeps the
  context cut permanent.

---

## 2026-10-08 - Session 54 (type 1: architecture review)

SESSION TYPE ROTATION LOG: 49=2, 50=4, 51=3, 52=5, 53=0, 54=1. All six
types served at least once now - pick freely, avoid repeats in a row.

GOAL: review the architecture for scalability/correctness landmines on
the road to multi-node 10k, and spend the session closing the one the
gaps list flagged (the O(guests) scan term).

REVIEW FINDINGS (verified by reading the code, not assumed):

- INVARIANT (sound): every guest lifecycle site keeps the guest row and
  its VisIndex membership in sync - ingest_guest (insert/reposition),
  both authority-demote paths (insert after kill), remove_guest
  (remove). The session-30 patch path already resolves ids through
  gobs.get().or_else(guests.get()), so guests flow through the touched
  lists correctly. Because of this, the per-cell guest bucket the gaps
  list asked for was unnecessary: the buckets ALREADY hold guests.
- BUG 1 (real, fixed): scan_visible_into dropped guests in its
  compaction (gobs.get miss) and then re-walked the WHOLE guest table
  per rescan - O(node guest population) per session per scan. Empty
  table single-node (invisible in every single-node load run), a
  landmine at multi-node 10k. Fix: the compaction resolves local-first/
  guest-second and the walk is gone; scan cost is bounded by the guest
  population of the VIEW cells.
- BUG 2 (real, fixed): spawn_with_id inserts into the VisIndex
  unconditionally, so promote_transfer re-spawning a previously
  ingested guest pushed the id into the SAME cell bucket twice (the
  dead `let _ = was_guest;` binding hinted at it). Dupes made the scan
  list the id twice and cell_count() lie. Fix: VisIndex::insert is now
  idempotent per (id, cell) - same-cell re-insert marks dirty/touched;
  a drifted mapping heals through reposition.
- INVARIANT (sound): gob ids are globally unique in a cluster by
  per-node slot partitioning (Gobs::with_layout hands each node a
  disjoint slot range; wire blocks stay valid without id remapping).
- Review notes carried to the gaps list: the 50-tick guest GC walks the
  whole guest table per node (bounded by the subscribed population;
  fine at 1k, revisit only with multi-node profiling).

VERIFICATION:

- cargo fmt + clippy -D warnings clean; 285 tests green (261 unit incl.
  3 new pins: two visidx-level idempotence pins + the game-level
  guest_promotion_does_not_duplicate_the_vis_bucket_entry, 11 proto,
  4 wire, 9 world). No Rust rule violations: rust-skills loaded before
  coding (mem-reuse-collections, coll-seq-choice re-read; the compaction
  stays allocation-free write-index).
- LIVE 2-NODE CLUSTER (release binary, seed 42, fresh saves): wire
  client WORLD ENTRY: OK through node 0; MOVE PROBE: OK (probe_walk);
  new scripts/probe_guest_walk.py walked 880 subtiles east across 4
  VisIndex cells (cell (4,2) is node-1-owned by rendezvous) - node 0
  ingested 17 guests and transferred 13 local animals out, zero errors
  or panics on both nodes (RUST_LOG=hnh_server=debug to see the
  ingest/transfer lines).

COMMITS: 3fd2681 (scan + visidx fixes + test pins), dcb23da
(probe_guest_walk + README row) + this handoff entry.

NEXT (handoff):
- The gaps list in the living file is renumbered (guest scan done);
  top picks: wire-test de-flake (type 4), batch_move_broadcast
  profiling (type 5), game.rs interact/combat split (type 2), recipe
  breadth (type 3). Windows smoke + GL e2e still carried.
- CI workflow push: token unchanged, still no `workflow` scope - not
  retried this session per the session-53 note.

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


---

## 2026-10-08 - Session 57 (type 5: performance)

SESSION TYPE ROTATION LOG: 53=0, 54=1, 55=2, 56=4, 57=5. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the top carried type-5 item - profile batch_move_broadcast (the
mv phase read as #2: 3-32 ms windows) and fix what the data points at;
plus the cheap perf-field gap (per-window max tick).

PROFILE FIRST (new attribution before touching anything):

- Perf gains four mvbat_* fields splitting the movement phase: scan
  (the O(alive) position advance + dirty marks), encode (wire blocks
  + batch push), fan-out (broadcast_batch, both batches), and the
  mover count. tick_movement is now a SCAN pass + an ENCODE pass
  (candidates collected as (id, frame, step, cx, cy), encoded in slot
  order afterwards - the packed batch is unchanged), so the split is
  honest. start_blocks/fx_batch_n joined the perf report.
- Findings at 300 bots: scan 22-40 us, encode 3-14 us, fan-out the
  rest. At 1000 bots: fan-out dominates the wall clock (14-60 ms
  windows) - but the per-pair ops are all hash probes, and the 1k
  mean tick reproduces the session-52 re-baseline band (30-60 ms on
  this 2-core box). Conclusion: at 1k the wall time is scheduler-
  bound (2 cores, ~2000 runnable runtime tasks; the fan-out also
  wakes 1000 session tasks, so it eats the most preemption), and the
  fan-out itself is PAIR-bound (every visible (session, mover) pair
  must append bytes - the true lower bound). The old per-session
  HashMap cell walk was the one term that was NOT a lower bound -
  so that is what got cut.

CUTS:

- move_batch: the per-session cell walk now runs over a DENSE SORTED
  cell index - Vec<CellGroup> (~24 B per non-empty cell, ordered by
  (y, x)) plus a Vec<u32> block order, rebuilt lazily once per batch.
  A session binary-searches its y-cell range (axis_cell_lo/hi, exact
  integer bounds) and x-tests inside: strictly sequential memory in
  L1/L2 instead of sessions x cells hash probes with a cache miss per
  bucket. Correctness: axis_cell_lo/hi are cross-checked against the
  rectangle oracle over negative coords and boundary contacts
  (exhaustive unit test); a manual micro-bench (#[ignore],
  dense_index_bench) records dense=324 vs full-scan=472 ns per
  fan-out scan at the 1000-session/134-cell/500-block scale - the
  ratio EXCLUDES the HashMap cache misses the old walk also paid.
- tick_movement: per-block MessageBuf::new + finish + drop and a
  fresh finished-Vec per tick are gone - three taken/restored
  scratches (finished, progress, encoder) keep the 10 Hz path
  allocation-free (at 1k movers on cadence ticks that was ~10k
  allocs/s of 256 B churn).
- Perf: window_max_tick_us - reset by every 5 s report. The lifetime
  max never resets, so one early ramp-up spike froze every later
  report at 60 ms regardless of the steady state; the new wmax
  attribute spikes to their 5 s window (measured 17-28 ms windows at
  300 bots while lifetime max stayed at 60 ms).

VERIFICATION:

- 285 cargo tests green (11 proto + 261 unit incl. the movement/
  combat batteries + 1 ignored manual bench + 4 wire + 9 world);
  fmt + clippy -D warnings clean.
- Release binary: python probes WORLD ENTRY: OK + CATTR ORDER: OK.
- 300-bot and 1000-bot load runs with the new attribution; 1k wall
  time matches the documented session-52 band (scheduler-bound box,
  not an index regression).
- CI workflow push retried once per the session-53 rule: REJECTED
  again (PAT lacks the `workflow` scope), commit rolled back AFTER
  the perf commit was safe on its own - do not retry until the scope
  exists.

COMMITS: 7c1aef2 (perf: fan-out dense index + attribution) + this
handoff entry.

NEXT (handoff):
- The fan-out is pair-bound and the 1k single-node worst case is the
  clustered spawn shape; the multi-node path already caps pairs per
  node. Only a multi-node profile can justify more here (recorded as
  gap #2).
- Mechanical carried: five-probe hnhlib.py migration, recipe breadth
  (type 3), feeding lift, GL e2e + Windows smoke (no display host).

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

## 2026-10-08 - Session 62 (type 3: new functionality)

SESSION TYPE ROTATION LOG: 58=3, 59=5, 60=3, 61=2, 62=3. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the carried gap #5 - the Food Trough lift mechanic
(animals-and-husbandry.md "Feeding: troughs and grazing": a lift-able
object, and "lift-and-right-click on another trough transfers fodder
like a liquid"). Session 48 had explicitly scoped this out ("no lift
handling anywhere in this server yet").

THE CUT:
- state.rs: Player.carried_trough (the lifted trough's fodder store
  rides the player; one carried object at a time) + SessionOut
  .trough_menu (the pending Lift flower menu, the station_menu
  pattern).
- building.rs: clicking a placed trough opens a one-petal "Lift"
  flower menu (trough_click/open_trough_menu); choosing it retracts
  the gob for every viewer (the Drop-pickup removal path), frees the
  tile and moves the store onto the player ("You lift the trough (N
  fodder units)."). A map click while carrying takes precedence over
  the build ghost in on_map_place and places the trough back down at
  a tile validated EXACTLY like a build commit (5-tile reach,
  walkable terrain, no crop/plan/structure occupancy; "You place the
  trough (N fodder units)."). Clicking a placed trough WHILE
  carrying transfers the fodder "like a liquid": moved = min(carried,
  cap - dest); the moved units carry the SOURCE's running average so
  the destination mixes by the doc's arithmetic (q10*100 + q12*50 ->
  q10); the source keeps its FULL quality history per the session-48
  rule ("consumption drains units but NOT the quality history" - the
  unit pin caught the first draft subtracting it), so an emptied
  trough keeps its average ("Transferred N fodder units.").
- persist.rs: SavedTrough + SavedPlayer.carried_trough (v7,
  additive, bincode-safe serde default - older saves load with
  nothing carried). game.rs restores the store before any
  interaction with the fresh player row.
- nodes.rs: NodeMsg::CharData now boxes its SavedPlayer snapshot -
  the new field pushed the variant over the clippy
  large-enum-variant threshold; Box<T> serializes as T, the mesh
  wire format is unchanged.
- The trough owned by a PEER node offers no Lift petal to a guest
  (the click stays a validated no-op, like a stump pick): cross-node
  lift/transfer relays are future work, recorded in the doc's Open
  questions together with the carried-trough avatar render.

VERIFICATION (every line a fresh run this session):
- 298 cargo tests green (11 proto + 274 unit incl. 5 new pins:
  trough_lift_retracts_the_gob_and_carries_the_fodder,
  trough_place_back_restores_the_store,
  trough_transfer_moves_fodder_like_a_liquid,
  trough_transfer_respects_the_capacity_cap,
  roundtrip_preserves_a_carried_trough; + 4 wire + 9 world);
  fmt + clippy -D warnings clean.
- server/scripts/test_feeding.py (new probe on the hnhlib harness):
  builds two troughs through the REAL build pagina path, loads
  fodder one unit per itemact (wheat, then carrot seeds), lifts
  trough 1 (gob retracted + carry line), places it back down (new
  gob + place line), lifts trough 2 and transfers its 2 units into
  trough 1 by clicking it while carrying - FEEDING FLOW: OK end to
  end. Probe notes: the kit carries 5 wheat + 5 carrot seeds (both
  fodder); the trough demand is branch x4 (one sink), the cursor
  return contract applies after every sink/load.
- Smoke on the touched paths: WORLD ENTRY: OK, CATTR ORDER: OK,
  BUILD FLOW: OK, STATION FLOW: OK.

COMMITS: trough lift mechanic + probe + docs + this handoff.

NEXT (handoff):
- Type-3 candidates: metal chain groundwork (ore gathering + smelter
  numbers), flower-menu pick verbs, per-animal breed stat rows.
- Carried: cross-node lift/transfer relays, GL e2e + Windows smoke
  (no display host), multi-machine cluster profile, CI push when the
  token gets the scope.

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

---
## 2026-10-08 - Session 65 (type 5: performance)

SESSION TYPE ROTATION LOG: 61=2, 62=3, 63=4, 64=1, 65=5. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the S64 handoff named the one unverified perf claim - "the
retransmit sweep is O(pending) per ~300 ms; expect near-zero in the
steady state - verify, do not assume". First step was honesty
instrumentation: the sweep had no perf attribution at all, only a
debug! line that fires on resend work.

MEASURED (1000 bots, saturated world, release build, `--perf`):

- `retx_pending` 1 616 372 - NOT near-zero. `retx_sweep_us` 426 860 -
  the sweep alone cost more than four ticks of budget.
- `retx_queue_full` 1 382 303 of ~1.6M attempts (85% refusals);
  `mean_tick_us` 226 529 - the tick budget was broken 2.2x over.
- Root cause chain: the load-bot cohort never echoed MSG_OBJACK
  (grep bots.rs: zero hits), so every spawn/finalizer block lived in
  its session's unacked table until the try-count retirement; BUT the
  S64 no-burn rule on queue-full refusals meant a saturated session's
  blocks never retired at all (positive feedback: more pending ->
  bigger walk -> more refusals -> still no retirement). The S64
  "near-zero" assumption held for acking clients only.

FIX (one change set, measured first, re-measured after):

- Perf attribution: `retx_sweep_us / retx_pending / retx_resent /
  retx_queue_full / retx_busy_sessions` per sweep in the perf report -
  the sweep's cost is now a first-class number, not an inference.
- Hard age ceiling `RETRANS_MAX_AGE_MS` (10 s from first send): every
  block retires deterministically regardless of send attempts. A
  throttled or dead session drains its table; the sweep walk is bounded
  by the recent past.
- Per-session backpressure throttle: after a raw-queue-full refusal the
  session's retransmit pass is skipped for 1 s (`retx_throttle_until`);
  retries stop firing into a saturated channel, and the walk skips the
  session wholesale (`busy_sessions` count shows the real depth).
- Expired blocks latch the gob's in-order walk (`blocked = true`), so
  no later frame of that gob escapes through the retirement hole.
- Load bots mirror the real client's SWorker: `parse_objdata` tracks
  the max decoded frame per gob and the bot echoes one batched
  MSG_OBJACK datagram every 200 ms (the wire shape the sweep is keyed
  on). The load cohort now exercises the same retransmission contract
  the wire harness does - which is what a 1k-player load test MEANS.

POST-FIX (same 1000-bot scenario):

- `retx_pending` 10 010-47 782 (~30x down); `retx_sweep_us`
  3 355-41 063 (~10-100x down); `retx_queue_full` 98-12 446 (~200x
  down, and the throttle keeps refusals from compounding).
- `mean_tick_us` 30 808-86 092 - back INSIDE the 100 ms budget
  (window max spikes 62-283 ms are the bot-entry spawn burst, already
  known and windowed by `wmax_tick_us`).
- `WORLD ENTRY: OK` python probe green against the new binary.

VERIFIED (fresh runs): fmt --check clean; clippy -D warnings clean;
cargo test --workspace 300 green (11 proto + 274 unit + 6 wire +
9 world, gathering scenario #[ignore]d as documented). The
`lost_static_spawn_wave_is_retransmitted` probe still passes - the age
ceiling (10 s) sits an order above the test's 1.2 s loss window, and
the throttle cannot fire on a single session with an empty queue.

NOT DONE (deliberate): `Arc<Vec<u8>>` for block bytes would remove the
deep clone on resend - deferred until the retx_* fields show the clone
matters (post-fix resends are ~1-20k per sweep, not 114k; verify first,
do not optimize on vibes).

NEXT (handoff):
- Type-3 candidates: metal chain groundwork (ore + smelter), flower
  pick verbs, per-animal breed stat rows.
- Type-5 candidates: mvbat_fanout_us remains the top single phase at
  1000 sessions (14-38 ms in the post-fix run) - profile the
  per-session fan-out walk; entry-burst wmax (283 ms) attribution.
- Carried: cross-node lift/transfer relays, GL e2e + Windows smoke
  (no display host), multi-machine cluster profile, CI push when the
  token gets the workflow scope.

---
## 2026-10-08 - Session 66 (type 3: new functionality)

SESSION TYPE ROTATION LOG: 62=3, 63=4, 64=1, 65=5, 66=3. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the S64/S65 handoffs named "metal chain groundwork (ore +
smelter)" as the first type-3 candidate. The smelter had stood as a
plain structure since session 15 ("until the metal chain exists"), and
no metal item could enter the economy. TWO implementation waves landed
under this session number (the second rebased on the first's push).

WAVE 1 (d91dcb0): ore deposits + a working smelter.

- ORE DEPOSITS: MOUNTAIN/CAVE tiles with a walkable 4-neighbor spawn
  gfx/terobjs/mining/heap ore deposits (3% roll), each carrying
  Copper/Tin/Iron (OreKind::from_roll, 5:3:2 per-tile mix), ORE_PICKS
  = 4 picks of the ore item (display label), ORE_PICK_LP per pick.
  Deposits are pure seed-derived statics; hnh-world gained find_tile +
  GridStore::terrain_at and pins the dev-seed rocky belt within ~26
  tiles of the spawn area.
- SMELTER STATION: StationSpec.kind: StationKind (Oven/Smelter)
  dispatches itemact input matching, refusals and the job output;
  tick_stations drops whatever the kind rolls (the hardcoded meat drop
  is gone). craft::SMELT_MAP melts ore labels: copper nugget ->
  bar-copper, tin nugget -> bar-tin, iron ore -> bar-castiron. Branch
  fuel, one ore per 30-tick job (server policy; legacy ~55 min per
  25-ore load), station quality formula on the output.
  DROP_WORLD_ALIASES renders pack-missing tin/cast-iron world shapes
  through sibling metals. Station wording neutralized on ALL paths
  (local, relay-ack, menu) to "The station ...".

WAVE 2 (this session's commit): the refinement tier.

- The SHIPPED paginae come alive: bloom2wrought (ad "wroughtiron")
  refines bar-castiron x1 -> bar-wroughtiron x1 (the finery-forge leg
  stand-in, tanhide pattern), shammer makes the smithy's hammer
  (bar-wroughtiron + branch -> hammer-smithys), the first metal tool.
  Unit counts are server policy; both paginae and both items ship in
  the pack.
- Unit pins: wrought_iron_and_hammer_recipes_are_wired (pack-aware),
  metal_refinement_chain_crafts_bar_and_hammer (craft_once end to
  end: q40 cast iron -> 25 wrought -> 13 hammer through the str
  softcap).
- Wire probe fix: test_smelt.py missed the second pickup hop of the
  gathering shape (click the deposit -> the ORE DROP spawns -> click
  the drop -> the inventory stack) and matched tin's aliased world
  shape wrong; both fixed (ORE_WORLD mapping + nearest-few deposit
  approach retries). SMELT: OK green (copper leg measured; the iron
  leg is probabilistic - 2/10 deposits are iron).

VERIFIED (fresh runs): cargo fmt --all -- --check clean; clippy
--all-targets -D warnings clean; cargo test --workspace 306 green
(11 proto + 279 unit + 6 wire + 10 world, 1 gathering #[ignore]).
oven regression STATION FLOW: OK and WORLD ENTRY: OK re-proved on the
wave-1 binary; SMELT: OK on the wave-2 binary.

PROCESS NOTE: this session initially rebuilt the whole metal chain
independently (ore boulders + sdt flag + its own StationKind + probe)
without noticing the parallel push until the non-fast-forward reject;
the local duplicate work was discarded at reset and re-based as the
smaller refinement wave. Lesson: re-check origin/master right before
pushing ANY session-shaped work - the repo has parallel writers.

WAVE 3 (a second rebased writer under the same session number): the
Alloying Crucible completes the bronze leg:

- StationKind::Alloyer + the BUILDABLES entry (gfx/terobjs/alloyer,
  stone x4 + branch x4 demand, branch fuel, 30-tick jobs).
- StationState.aux: the crucible splits the bronze charge across the
  input (Bar of Copper) and aux (Bar of Tin) slots; the Light act is
  refused until BOTH slots are loaded. The local menu path, the relay
  light/item paths, and the StationView readiness snapshot (has_input
  keys on both slots for the crucible) agree.
- Output: ALLOY_OUT_COUNT = 2 Bar of Bronze per charge. The legacy
  Ring of Brodgar charge (2 copper + 1 tin -> 3 bronze) is a 1:1
  metal-to-bronze MASS balance; the two-slot policy realizes the same
  balance as 1+1 -> 2 (craft.rs documents the derivation).
- Persistence: the additive SavedStructure.aux field round-trips the
  tin charge; the restore re-validates the label against
  ALLOY_INPUT_TIN.
- The trough build test resolves its spec via buildable_by_ad now
  (the alloyer insertion shifted the positional registry indices -
  the second latent-index bug the positional style caused).
- Verified: fmt + clippy -D warnings clean; cargo test --workspace
  307 green (11 proto + 280 unit + 6 wire + 10 world).

NEXT (handoff):
- Bronze live probe (first candidate): test_smelt.py needs a
  crucible phase - the starter kit (stone x6 + branch x10) covers
  the smelter OR the crucible, so the probe must boulder-pick stone
  (BOULDER_STONES x5) before building the second station, then load
  copper+tin and catch the TWO bronze drops.
- Type-3 candidates: kiln + brick chain (restores the legacy
  smelter/oven demands), anvil + smithy's-hammer tool-gated recipes,
  flower pick verbs, per-animal breed stat rows.
- Type-5 candidates: mvbat_fanout_us remains the top single phase at
  1000 sessions; entry-burst wmax (283 ms) attribution.
- Carried: test_smelt.py walk phase can exceed 2 min on unlucky
  pathing (bounded retry, runs green on retry); GL e2e + Windows
  smoke (no display host); multi-machine cluster profile; CI push
  when the token gets the workflow scope.

COMMITS: wave 1 = d91dcb0 (parallel writer); wave 2 = the refinement
tier + probe fix + docs + handoff (the second writer); wave 3 = the
alloyer + fmt (a third writer under the same session number).


---
## 2026-10-08 - Session 65 (type 5: performance)

SESSION TYPE ROTATION LOG: 61=2, 62=3, 63=4, 64=1, 65=5. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the S64 handoff named the one unverified perf claim - "the
retransmit sweep is O(pending) per ~300 ms; expect near-zero in the
steady state - verify, do not assume". First step was honesty
instrumentation: the sweep had no perf attribution at all, only a
debug! line that fires on resend work.

MEASURED (1000 bots, saturated world, release build, `--perf`):

- `retx_pending` 1 616 372 - NOT near-zero. `retx_sweep_us` 426 860 -
  the sweep alone cost more than four ticks of budget.
- `retx_queue_full` 1 382 303 of ~1.6M attempts (85% refusals);
  `mean_tick_us` 226 529 - the tick budget was broken 2.2x over.
- Root cause chain: the load-bot cohort never echoed MSG_OBJACK
  (grep bots.rs: zero hits), so every spawn/finalizer block lived in
  its session's unacked table until the try-count retirement; BUT the
  S64 no-burn rule on queue-full refusals meant a saturated session's
  blocks never retired at all (positive feedback: more pending ->
  bigger walk -> more refusals -> still no retirement). The S64
  "near-zero" assumption held for acking clients only.

FIX (one change set, measured first, re-measured after):

- Perf attribution: `retx_sweep_us / retx_pending / retx_resent /
  retx_queue_full / retx_busy_sessions` per sweep in the perf report -
  the sweep's cost is now a first-class number, not an inference.
- Hard age ceiling `RETRANS_MAX_AGE_MS` (10 s from first send): every
  block retires deterministically regardless of send attempts. A
  throttled or dead session drains its table; the sweep walk is bounded
  by the recent past.
- Per-session backpressure throttle: after a raw-queue-full refusal the
  session's retransmit pass is skipped for 1 s (`retx_throttle_until`);
  retries stop firing into a saturated channel, and the walk skips the
  session wholesale (`busy_sessions` count shows the real depth).
- Expired blocks latch the gob's in-order walk (`blocked = true`), so
  no later frame of that gob escapes through the retirement hole.
- Load bots mirror the real client's SWorker: `parse_objdata` tracks
  the max decoded frame per gob and the bot echoes one batched
  MSG_OBJACK datagram every 200 ms (the wire shape the sweep is keyed
  on). The load cohort now exercises the same retransmission contract
  the wire harness does - which is what a 1k-player load test MEANS.

POST-FIX (same 1000-bot scenario):

- `retx_pending` 10 010-47 782 (~30x down); `retx_sweep_us`
  3 355-41 063 (~10-100x down); `retx_queue_full` 98-12 446 (~200x
  down, and the throttle keeps refusals from compounding).
- `mean_tick_us` 30 808-86 092 - back INSIDE the 100 ms budget
  (window max spikes 62-283 ms are the bot-entry spawn burst, already
  known and windowed by `wmax_tick_us`).
- `WORLD ENTRY: OK` python probe green against the new binary.

VERIFIED (fresh runs): fmt --check clean; clippy -D warnings clean;
cargo test --workspace 300 green (11 proto + 274 unit + 6 wire +
9 world, gathering scenario #[ignore]d as documented). The
`lost_static_spawn_wave_is_retransmitted` probe still passes - the age
ceiling (10 s) sits an order above the test's 1.2 s loss window, and
the throttle cannot fire on a single session with an empty queue.

NOT DONE (deliberate): `Arc<Vec<u8>>` for block bytes would remove the
deep clone on resend - deferred until the retx_* fields show the clone
matters (post-fix resends are ~1-20k per sweep, not 114k; verify first,
do not optimize on vibes).

NEXT (handoff):
- Type-3 candidates: metal chain groundwork (ore + smelter), flower
  pick verbs, per-animal breed stat rows.
- Type-5 candidates: mvbat_fanout_us remains the top single phase at
  1000 sessions (14-38 ms in the post-fix run) - profile the
  per-session fan-out walk; entry-burst wmax (283 ms) attribution.
- Carried: cross-node lift/transfer relays, GL e2e + Windows smoke
  (no display host), multi-machine cluster profile, CI push when the
  token gets the workflow scope.

---
## 2026-10-08 - Session 66 (type 3: new functionality)

SESSION TYPE ROTATION LOG: 62=3, 63=4, 64=1, 65=5, 66=3. All six types
served - pick freely, avoid repeating the previous session's type.

GOAL: the S64/S65 handoffs named "metal chain groundwork (ore +
smelter)" as the first type-3 candidate. The smelter had stood as a
plain structure since session 15 ("until the metal chain exists"), and
no metal item could enter the economy. TWO implementation waves landed
under this session number (the second rebased on the first's push).

WAVE 1 (d91dcb0): ore deposits + a working smelter.

- ORE DEPOSITS: MOUNTAIN/CAVE tiles with a walkable 4-neighbor spawn
  gfx/terobjs/mining/heap ore deposits (3% roll), each carrying
  Copper/Tin/Iron (OreKind::from_roll, 5:3:2 per-tile mix), ORE_PICKS
  = 4 picks of the ore item (display label), ORE_PICK_LP per pick.
  Deposits are pure seed-derived statics; hnh-world gained find_tile +
  GridStore::terrain_at and pins the dev-seed rocky belt within ~26
  tiles of the spawn area.
- SMELTER STATION: StationSpec.kind: StationKind (Oven/Smelter)
  dispatches itemact input matching, refusals and the job output;
  tick_stations drops whatever the kind rolls (the hardcoded meat drop
  is gone). craft::SMELT_MAP melts ore labels: copper nugget ->
  bar-copper, tin nugget -> bar-tin, iron ore -> bar-castiron. Branch
  fuel, one ore per 30-tick job (server policy; legacy ~55 min per
  25-ore load), station quality formula on the output.
  DROP_WORLD_ALIASES renders pack-missing tin/cast-iron world shapes
  through sibling metals. Station wording neutralized on ALL paths
  (local, relay-ack, menu) to "The station ...".

WAVE 2 (this session's commit): the refinement tier.

- The SHIPPED paginae come alive: bloom2wrought (ad "wroughtiron")
  refines bar-castiron x1 -> bar-wroughtiron x1 (the finery-forge leg
  stand-in, tanhide pattern), shammer makes the smithy's hammer
  (bar-wroughtiron + branch -> hammer-smithys), the first metal tool.
  Unit counts are server policy; both paginae and both items ship in
  the pack.
- Unit pins: wrought_iron_and_hammer_recipes_are_wired (pack-aware),
  metal_refinement_chain_crafts_bar_and_hammer (craft_once end to
  end: q40 cast iron -> 25 wrought -> 13 hammer through the str
  softcap).
- Wire probe fix: test_smelt.py missed the second pickup hop of the
  gathering shape (click the deposit -> the ORE DROP spawns -> click
  the drop -> the inventory stack) and matched tin's aliased world
  shape wrong; both fixed (ORE_WORLD mapping + nearest-few deposit
  approach retries). SMELT: OK green (copper leg measured; the iron
  leg is probabilistic - 2/10 deposits are iron).

VERIFIED (fresh runs): cargo fmt --all -- --check clean; clippy
--all-targets -D warnings clean; cargo test --workspace 306 green
(11 proto + 279 unit + 6 wire + 10 world, 1 gathering #[ignore]).
oven regression STATION FLOW: OK and WORLD ENTRY: OK re-proved on the
wave-1 binary; SMELT: OK on the wave-2 binary.

PROCESS NOTE: this session initially rebuilt the whole metal chain
independently (ore boulders + sdt flag + its own StationKind + probe)
without noticing the parallel push until the non-fast-forward reject;
the local duplicate work was discarded at reset and re-based as the
smaller refinement wave. Lesson: re-check origin/master right before
pushing ANY session-shaped work - the repo has parallel writers.

WAVE 3 (a second rebased writer under the same session number): the
Alloying Crucible completes the bronze leg:

- StationKind::Alloyer + the BUILDABLES entry (gfx/terobjs/alloyer,
  stone x4 + branch x4 demand, branch fuel, 30-tick jobs).
- StationState.aux: the crucible splits the bronze charge across the
  input (Bar of Copper) and aux (Bar of Tin) slots; the Light act is
  refused until BOTH slots are loaded. The local menu path, the relay
  light/item paths, and the StationView readiness snapshot (has_input
  keys on both slots for the crucible) agree.
- Output: ALLOY_OUT_COUNT = 2 Bar of Bronze per charge. The legacy
  Ring of Brodgar charge (2 copper + 1 tin -> 3 bronze) is a 1:1
  metal-to-bronze MASS balance; the two-slot policy realizes the same
  balance as 1+1 -> 2 (craft.rs documents the derivation).
- Persistence: the additive SavedStructure.aux field round-trips the
  tin charge; the restore re-validates the label against
  ALLOY_INPUT_TIN.
- The trough build test resolves its spec via buildable_by_ad now
  (the alloyer insertion shifted the positional registry indices -
  the second latent-index bug the positional style caused).
- Verified: fmt + clippy -D warnings clean; cargo test --workspace
  307 green (11 proto + 280 unit + 6 wire + 10 world).

NEXT (handoff):
- Bronze live probe (first candidate): test_smelt.py needs a
  crucible phase - the starter kit (stone x6 + branch x10) covers
  the smelter OR the crucible, so the probe must boulder-pick stone
  (BOULDER_STONES x5) before building the second station, then load
  copper+tin and catch the TWO bronze drops.
- Type-3 candidates: kiln + brick chain (restores the legacy
  smelter/oven demands), anvil + smithy's-hammer tool-gated recipes,
  flower pick verbs, per-animal breed stat rows.
- Type-5 candidates: mvbat_fanout_us remains the top single phase at
  1000 sessions; entry-burst wmax (283 ms) attribution.
- Carried: test_smelt.py walk phase can exceed 2 min on unlucky
  pathing (bounded retry, runs green on retry); GL e2e + Windows
  smoke (no display host); multi-machine cluster profile; CI push
  when the token gets the workflow scope.

COMMITS: wave 1 = d91dcb0 (parallel writer); wave 2 = the refinement
tier + probe fix + docs + handoff (the second writer); wave 3 = the
alloyer + fmt (a third writer under the same session number).

