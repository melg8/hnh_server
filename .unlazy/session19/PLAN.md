# PLAN — session 19

Original request: the standing full-implementation prompt (see AGENTS.md /
HANDOFF.md), resent with two user-visible defects and one new process
requirement:

1. "При логине не показывается голова и туловище выбранного персонажа до
   сих пор." - the login portrait still shows no head/torso.
2. "В игру уйти удалось, но перемещение по миру при кликах не происходит,
   персонаж стоит как вкопаный." - the character does not move on map
   clicks after entering the world.
3. "Добавь скрипт того как развернуть клиент в твоей среде и добавь что
   его нужно использовать и проверять все в твоей среде на реальном
   клиенте в AGENTS.md" - commit a script that deploys the REAL client in
   the agent environment and make real-client verification mandatory.

Session-19 evidence gathered so far (probes against a live server):

- Wire probe (server/scripts/probe_walk.py): a ground click with a CORRECT
  spawn-relative target produces OD_LINBEG + OD_LINSTEP for the player
  gob. The server movement path works; the old test's walk click targeted
  ~570 tiles off-spawn (assumed spawn 555*11, actual spawn tile (50,50)),
  which is why no gate ever caught the real-client regression.
- Real-client run under Xvfb + Temurin 8 + JOGL 1.1.1 (recreated the
  session-18 environment; script now committed): the world renders, but
  `gfx/invobjs/meat` (no neg layer in the shipped pack) throws
  "No negative found" inside OCache.cres on the Session RWorker thread and
  KILLS the reader. Every later server message (LINBEG included) is then
  never processed - the client is a zombie: renders whatever arrived
  before death, clicks produce outgoing wdgmsgs the server answers to a
  dead reader. This is the exact "character stands rooted" symptom.
- Dead animal loot drops (gfx/invobjs/meat) and tree-chop drops
  (gfx/invobjs/wood) are ground gobs rendered through StaticSprite which
  hard-requires a neg layer. Inventory icons do not (Item widget draws
  the image layer directly), so the bug only fires when a drop lies
  within view radius - which the saturated wildlife world guarantees.

Depth note: `tree 99` was requested; the honest decomposition is a
depth-2 tree (root -> node-1 -> four leaves). Stated per the skill
contract.

## Contract inventory (independently omittable outcomes)

| Id | Outcome | Tier | Owns | Needs |
| --- | --- | --- | --- | --- |
| leaf-1 | Session-reader hardening: a resource-level sprite failure inside OCache.cres (and cres-like gob setup paths) can no longer kill the RWorker thread; the client logs and continues. Pack repair: gfx/invobjs/meat.res and gfx/invobjs/wood.res receive neg layers via scripts/add_neg_layer.py into res/compiled (so ground drops render) and the gameres regeneration covers them. | judgment | src/haven/OCache.java, res/compiled/gfx/invobjs/{meat,wood}.res, scripts/add_neg_layer.py | master (07e9abc) |
| leaf-2 | Real-client movement verification: a committed runner script (scripts/jogl/run-real-client.sh + DriveAgent source) boots Xvfb + Temurin 8 + JOGL natives, logs in through the REAL widget chain, clicks the map with a real AWT Robot click, and prints MOVEMENT: MOVED|STUCK verdicts read from the client's own gob position. | judgment | scripts/jogl/, tools/agent/DriveAgent.java (committed copy) | leaf-1 verified |
| leaf-3 | Login portrait re-verification on the real client: screenshot the charselect card and the in-world avatar through the real GL path; if head/torso are missing in OUR environment, root-cause and fix; if they render, record the evidence (screenshot + log) in HANDOFF (the user's report likely predates the session-17/18 fixes now guarded by the rev-stamp). | judgment | scripts/jogl/, HANDOFF.md evidence | leaf-2 verified |
| node-1 | Integration: cargo test + fmt + clippy green; e2e battery (world entry + walk probe + UI probe charlist) in one server generation; AGENTS.md gains the mandatory real-client verification section; HANDOFF.md session entry; committed and pushed to master. | judgment | AGENTS.md, HANDOFF.md | leaf-1..3 settled |

## Interfaces and shared assumptions

- The client deploy script lives OUTSIDE the toolchain assumptions of
  windows/ (it is Linux/Xvfb specific) and is committed under scripts/jogl/
  with its Java agent sources; tool downloads stay in the script (URLs
  pinned to the archives used this session).
- DriveAgent prints only ASCII verdict lines (AGENT:/MOVEMENT:) that the
  runner greps; it never writes into the repo.
- The pack repair keeps the neg blob minimal (cc/bc/bs/sz + zero
  endpoints) and derives cc from the target's first image layer.

## Waves

Wave 1: leaf-1 (fix + pack repair).
Wave 2: leaf-2 (real-client runner + movement verdict).
Wave 3: leaf-3 (portrait evidence), then node-1 integration + push.
