# AGENTS.md — Guidelines for AI Coding Agents

This repository contains two code areas:

1. **Legacy Java client** (`src/`, built with Apache Ant) — the Haven & Hearth
   client. Do not modify it while working on Rust unless explicitly asked.
2. **Rust game server** (`server/`) — a new server implementation written in
   Rust. The quality bar for this code is **maximum**: it must be idiomatic,
   safe, well-tested, and performant. There is no such thing as "temporary"
   quality here.

The repository also contains `docs/mechanics/`, the verified game mechanics
reference for legacy Haven & Hearth; consulting and updating it is mandatory
(see the section below).

All code, comments, commit messages, and documentation you produce in this
repository MUST be in English.

---

## Session Protocol (read HANDOFF.md FIRST)

This repo is developed in 2-hour sessions with rotating goals; your
context may be cut at any moment.

- **Read `HANDOFF.md` before doing anything else** - it carries the
  session-type rotation log (one goal per session, never repeat the
  previous session's type), the last session's DONE/NOT-DONE, and the
  carried gaps. Keep its entries to the last two sessions (archive
  older records into `HANDOFF_ARCHIVE.md`).
- Commit small and push often - never leave work uncommitted when the
  2-hour budget is close.
- The shared agent worklog lives outside the repo at
  `/home/z/my-project/worklog.md` (session-level, not committed).

---

## Windows quick start (the user-facing contract)

The user plays from Windows; `windows/README.md` is the entry point and
`windows/run-client.bat` is THE one command - it rebuilds the client jar
when stale, regenerates `gameres/` when older than the tree, auto-starts
the seed-42 server when it is not answering, waits for the auth and
resource ports, then launches the Java client into the persistent world.
Any change to boot, ports, resources, or client startup MUST keep the
`windows/*.bat` flow working (see also `windows/start-cluster.bat`,
`windows/loadtest.bat`, `windows/collect-logs.bat`).

---

## MANDATORY: Use and Maintain docs/mechanics/ (Game Mechanics Reference)

`docs/mechanics/` is the golden source of truth for game mechanics. Its index
is `docs/mechanics/README.md`.

1. **Search first.** BEFORE designing, writing, or reviewing any server-side
   behavior, consult `docs/mechanics/README.md` and the domain docs it links
   (network, world, objects, character, skills, items, crafting, livestock,
   combat). Do not re-derive mechanics from client code or memory when a
   documented answer exists; every documented claim cites its client files
   and external sources.
2. **What the folder is.** A verified behavioral blueprint of legacy Haven &
   Hearth, distilled from the `src/haven/` client code, the legacy Ring of
   Brodgar wiki, the fandom wiki, official forums/patch notes, and the repo
   configs (`etc/needed/fep.conf`, `etc/needed/curio.conf`). The index maps
   questions to documents and flags load-bearing constants shared across
   domains.
3. **Update rule (living document).** When implementation reveals a nuance
   that contradicts, refines, or extends a doc, update that domain doc in
   the same change set. Record uncertainty in the doc's `## Open questions`
   section instead of inventing numbers. If the server deliberately changes
   a documented behavior, update the doc to state both the legacy behavior
   and the new decision. Keep docs English and ASCII, each under ~60KB;
   prefer editing the specific domain doc over growing
   `docs/mechanics/README.md`.
4. **New coverage.** To document a domain not yet covered, create a new
   subfolder under `docs/mechanics/` with the shared skeleton (H1,
   `> **Sources:**` quote, `## Summary`, mechanics sections,
   `## Server implementation notes`, `## Open questions`) and link it from
   `docs/mechanics/README.md`.

---

## MANDATORY STEP: Load Rust Skills Before Any Rust Work

**If you are about to write, modify, refactor, or review any Rust code in this
repository (including everything under `server/`), you MUST first download and
use the [leonardomso/rust-skills](https://github.com/leonardomso/rust-skills)
repository.** Do not skip this step, do not rely on your built-in Rust
knowledge alone, and do not write Rust code from memory of the rules — read
the actual rule files.

### Step 1 — Download (or refresh) the skills repository

Clone it into a temporary directory **outside this repository** (never commit
the skills copy into `hnh_server`):

```bash
git clone --depth 1 https://github.com/leonardomso/rust-skills.git /tmp/rust-skills
```

If a clone already exists, refresh it instead of re-cloning:

```bash
git -C /tmp/rust-skills pull --ff-only
```

On Windows, use `%TEMP%\rust-skills` (or any path outside the repository) in
place of `/tmp/rust-skills`.

Alternative installation methods that are also acceptable:

```bash
# If your agent supports skills and npx is available:
npx add-skill leonardomso/rust-skills

# Or clone directly into your agent's skills directory, e.g.:
git clone --depth 1 https://github.com/leonardomso/rust-skills.git .agents/skills/rust-skills
```

After installing via any of these alternatives, locate the installed
`SKILL.md` and continue with Step 2.

### Step 2 — Read the index first

Open `SKILL.md` from the downloaded copy. It is a lightweight index of **265
rules in 26 categories** (current for Rust 1.96, 2024 edition), grouped by
priority (CRITICAL / HIGH / MEDIUM / LOW / REFERENCE) with a one-line summary
per rule. Read it fully before deciding what applies to your task.

### Step 3 — Select and read the relevant rule files

Each category has a filename prefix. Load only the categories relevant to the
code you are touching, then open the full `rules/<prefix>-<name>.md` file for
every rule that applies. For game-server work these categories are almost
always relevant:

| Task area | Categories to load |
|-----------|--------------------|
| Any Rust code | `own-` (ownership), `err-` (error handling) |
| Networking / async I/O | `async-`, `conc-`, `anti-lock-across-await` |
| Hot paths / entity updates | `mem-`, `perf-`, `coll-` |
| Public APIs, data models | `api-`, `type-`, `serde-`, `conv-` |
| `unsafe` (last resort) | `unsafe-`, plus Miri requirements below |
| Tests and CI | `test-`, `lint-` |
| Project layout, Cargo | `proj-`, `name-` |
| Before finishing a task | `anti-` (anti-pattern checklist) |

### Step 4 — Apply the rules and make them traceable

Write the code according to the loaded rules. When a non-obvious decision is
dictated by a rule, reference the rule id in a short English comment, e.g.:

```rust
// own-slice-over-vec + own-borrow-over-clone: accept a borrowed slice,
// avoid allocating on every tick.
fn apply_damage(targets: &[EntityId], amount: i32) { /* ... */ }
```

When reviewing code, name the violated rules explicitly instead of vague
stylistic complaints.

---

## Quality Bar for `server/` (Rust)

The rules above define the details; these are the non-negotiables enforced in
this repository:

- **No `unwrap()` / bare `expect()` in production paths.** Return
  `Result`/`Option`, propagate with `?`, add context. `expect()` is allowed
  only for genuine invariants with a `BUG:`-style message
  (`err-no-unwrap-prod`, `err-expect-bugs-only`).
- **Structured errors.** `thiserror` for library/domain error types, `anyhow`
  (or a top-level error enum) at the application boundary, with context chains
  (`err-thiserror-lib`, `err-anyhow-app`, `err-context-chain`).
- **Async discipline.** Tokio-based; never hold a std `Mutex`/`RwLock` guard
  across `.await`; prefer channels and structured spawning; every task must
  handle cancellation (`async-no-lock-await`, `anti-lock-across-await`,
  `async-joinset-structured`, `async-cancel-safety`).
- **`unsafe` is a last resort.** If unavoidable: minimal block scope, a
  `// SAFETY:` comment justifying every invariant, and `cargo miri test` must
  pass (`unsafe-*` rules).
- **Numeric safety.** No silent `as` casts that can truncate; use `TryFrom`,
  `checked_*`/`saturating_*` where overflow is possible (`num-*`).
- **Observability.** Use `tracing` with spans and structured fields, never
  `println!`, and never log secrets or credentials (`obs-*`).
- **Zero-warning policy.** `cargo fmt` clean; `cargo clippy --all-targets --
  -D warnings` clean; new dependencies justified and minimal.

## Verification Before Committing Rust Changes

Run all of these; every command must pass:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo test` includes the black-box wire integration layer
(`server/crates/hnh-server/tests/wire.rs`): it boots the real binary on
ephemeral ports and asserts the auth/session/bootstrap/movement
contracts over the real protocol. It needs no gameres pack and no
python - a fresh clone runs it as-is. The richer scenario probes
(build, farming, party, stations) live in `server/scripts/` on the
shared `hnhlib.py` harness (see `server/scripts/README.md`); run the
relevant one when your change touches its domain.

CI: the intended contract is fmt + clippy + `cargo test --workspace`
on every push/PR to master via `.github/workflows/rust.yml`. As of
session 91 the workflow file still has not landed on the remote (the
PAT lacks the `workflow` scope; the file content is preserved in
HANDOFF_ARCHIVE.md, session-50 addendum) - retry that push once per
session. Until it lands, the green local run of the three commands
above is the ONLY gate: do not rely on CI to catch what you can
verify yourself.

If the change touches `unsafe` code, additionally run `cargo miri test` for
the affected crate. If you cannot run the toolchain in your environment, state
that explicitly in your summary instead of claiming success.

## Multi-Node Cluster Surfaces (run + verify)

Any change to the cluster path (`grid_owner.rs`, `nodes.rs`,
`game/cluster.rs`, node message encoding in `hnh-proto`) or to the
session/stream layer it feeds MUST be verified on a real multi-node
boot, not only by the in-process unit battery:

```bash
cd server
./scripts/test_cluster.sh          # local 2-node profile  -> CLUSTER E2E: OK
./scripts/test_remote_cluster.sh   # per-machine profile   -> REMOTE CLUSTER: OK
```

The REAL-client version of the remote gate (the actual GL render path,
through machine A then machine B's own address) is STAGED because sandboxed
CI hosts may evict long-lived idle wrapper processes - each stage is one
short foreground call, the Xvfb+client pair lives exactly as long as the
JVM renders through it:

```bash
scripts/jogl/run-remote-e2e.sh up      # boot both machines -> REMOTE GL MESH: OK
scripts/jogl/run-remote-e2e.sh legA    # full agent corpus via machine A
                                       #   -> REMOTE GL WALK + RENDER: OK
scripts/jogl/run-remote-e2e.sh legB    # re-enter via 127.0.0.2:1873/1874
                                       #   -> REMOTE GL MIGRATION + CLIENT: OK
scripts/jogl/run-remote-e2e.sh down    # stop both machines
# ("all" chains the stages for hosts where one long call is fine)
```

- `cluster-up.sh up` boots a local 2..4-node cluster with the port
  formula node i = auth 1871+2i, game 1870+4i, res 1872+4i, mesh
  18790+i (node 0 keeps the client-facing defaults, so the Java
  client, `run-client.bat` and `test_client.py` work unchanged).
- A clustered node binds its game UDP socket to its OWN CLUSTER_SPEC
  address (not wildcard): the real Java client (`Session.java`
  RWorker) drops every datagram whose source address != the server it
  dialed, so machine B answering from a wildcard bind would be
  silently discarded by any client entering through B. Single-node
  keeps the wildcard contract (local clients, the bot fleet). The
  Java client gains `-Dhaven.authport`/`-Dhaven.gameport` overrides
  for entering through any node's own address.
- `cluster-up.sh remote` boots ONLY this machine's node from
  `CLUSTER_SPEC=hostA:18790,hostB:18791,... SELF=i` - one copy per
  machine, same `--seed`; the rendezvous-hash grid ownership splits
  the world across machines. `stop`/`status` manage both modes.
- The e2e gates prove the mesh (dial link up), world entry through
  node 0, the guest walk across peer-owned cells (peer-subscribed +
  guest-ingested evidence), and cross-node character migration
  (snapshot served by the owner, migration received by the peer).
  The remote gate runs the operator command per machine on two
  loopback addresses (127.0.0.1 + 127.0.0.2) and re-enters the
  migrated character through machine B's own ports.
- Probe clients (`hnhlib.WireClient`) bind to any node's ports and
  echo OBJACK like the real client's SWorker; their movement
  semantics mirror `src/haven/OCache.java` exactly (OD_MOVE never
  cancels a live LinMove; the final LINSTEP `l >= c` is the arrival
  marker). Keep that parity when touching either side.

## MANDATORY: Verify Client-Visible Changes on the REAL Client

Every change that can affect what the user sees or does in the game
(bootstrap, widgets, movement, combat, resources, rendering) MUST be
verified end to end in YOUR environment on the REAL GL client before you
claim the fix works. Headless probes (UiProbe, wire-level python
scripts) are necessary but NOT sufficient: they cannot catch render-path
deaths, resource-load failures on real sprites, widget-chain freezes,
or input dispatch problems - each of which has already shipped a
"fixed" bug to the user that was not fixed (frozen character, blank
portrait, black screen).

The committed harness (`scripts/jogl/`) boots the real client in an
agent sandbox:

```bash
# One-time per sandbox (downloads Temurin 8, JOGL 1.1.1 natives, X11
# libs, Ant; builds DriveAgent). Idempotent.
scripts/jogl/deploy-agent-env.sh

# Full loop: fresh server + Xvfb + real client, login through the REAL
# widget chain, a real AWT Robot map click, and MOVEMENT verdicts read
# from the client's own gob position:
scripts/jogl/run-real-client-e2e.sh <username> <tag>

# Character-selection portrait: captures the charlist screen (the agent
# picks the character itself via Charlist.choose_player after the shot)
# and pixel-checks the avatar frame:
scripts/jogl/verify_charlist_portrait.sh <tag>
```

`MOVEMENT: MOVED` plus `SPEED VERDICT: OK` (measured tiles/s inside the
documented 3.0 walk gait window), `NO TELEPORT: OK`, and
`RAPID CLICKS: GLIDING` are the movement pass lines; `PORTRAIT: OK` is the
pass line for the login card. `EQUIP DOLL: avagob=... ava-rend=OK` plus the
session-25 `EQUIPVIS VERDICT: OK` / `WORLD DUMP DRESSED|UNDRESSED` lines are
the equipment-visuals pass marks: the equip-visuals phase equips the starter
linen pants through the real widget chain and reads back both the doll
(Avatar.rend) and the world drawable (Layered.layers) - a missing or
stuck-on piece means the equip pipeline broke. The client log is at
`/tmp/client_<tag>.log`, the server trace at `/tmp/server_<tag>.log`. For
visual verification (portrait, avatar pose, rendering) the agent saves
full-window screenshots to `/tmp/client_charlist.png`,
`/tmp/client_walking.png`, `/tmp/client_equip_dressed.png`,
`/tmp/client_equip_undressed.png`, `/tmp/client_world_player.png`,
`/tmp/client_world_*.png` - READ them, do not assume. The client debug flag
`-Dhaven.debugclicks=true` traces which branch consumes a map click when
you need to debug input handling; `-Dhaven.avadebug=1` traces the doll
composite (layer list + per-image offsets) into the client log.

Session-21 evidence lines (part of every full e2e run): the agent orders
three directional legs (`WALKDIR EAST|NORTH|SOUTH: ARRIVED` with
`WALKDIR SCREENSHOT: saved /tmp/client_walk_{east,north,south}.png` - read
the frames: the walk pose must FACE the travel direction, one direction
per leg, no spinning), dumps the equipment doll state
(`EQUIP DOLL: avagob=<id> ava-rend=OK` plus the `/tmp/client_equip.png`
frame - the spread-arms banzai doll must be visible), and hunts a predator
until a kritter lands inside the viewport (`ANIMALS SCREENSHOT: saved
kritter at <x>,<y> res=gfx/kritter/...` plus `/tmp/client_animals.png` -
the animal SPRITE must be visible, not a shadow-only gob). The wire probes
`server/scripts/probe_direction.py` (DIRECTION WIRE: OK) and
`server/scripts/probe_animals.py` (ANIMALS WIRE: OK, needs `--saturated`)
cover the same contracts at the protocol level.

Rule of thumb: a fix is done when the wire test passes AND the real
client demonstrates the behavior. Record both evidences in HANDOFF.md.

## Commit Discipline

- Small, focused commits with clear English messages (imperative mood).
- Never commit generated build artifacts (`target/`, `classes/`, `build/`).
- Never commit secrets, tokens, or credentials.

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **hnh_server** (10963 symbols, 39272 relationships, 624 execution flows).

> Index stale? Run `node .gitnexus/run.cjs analyze --index-only` from the project root — it auto-selects an available runner. No `.gitnexus/run.cjs` yet? Bootstrap with `npx`, `bunx`, or `pnpm dlx` — e.g. `bunx gitnexus@latest analyze` (npm 11 npx crash; #1939).
>
> No `.gitnexus/` index in your environment at all (fresh sandbox, env restart)? Rebuild it with the bootstrap above, or - for that one session only - fall back to text search (`grep`/`rg`) plus reading the affected files for impact analysis, and say so explicitly in the handoff record. Never silently skip the impact step, and never treat a grep hit count as equivalent to the graph verdict.

## Always Do

- **MUST run impact before editing.** Use `impact({target: "symbolName", direction: "upstream"})` or `node .gitnexus/run.cjs impact "symbolName" --direction upstream --repo .`; report callers, processes, and risk. Never substitute grep for graph analysis.
- **MUST analyze graph changes before committing.** Use `detect_changes({scope: "all"})` (MCP) or `node .gitnexus/run.cjs detect-changes --scope all --repo .` (CLI fallback). `partial: true` or `truncated: true` is not a clean check — a zero means unseen, not unaffected; re-run it. For regression review: `detect_changes({scope: "compare", base_ref: "master"})` or `node .gitnexus/run.cjs detect-changes --scope compare --base-ref "master" --repo .`.
- MUST warn on HIGH/CRITICAL `risk` pre-edit; never use `riskSharedAxes` to waive a HIGH/CRITICAL `risk` warning. Compare File/symbol: MCP File omits axes; Graph-RAG expands File.
- **MUST treat `risk: UNKNOWN` as unresolved, not as low.** An empty caller set is not evidence the symbol is unused — it can also mean the callers are not resolvable by the index (plain-object property access, dynamic dispatch, cross-language calls). `impact` pairs `UNKNOWN` with a `riskNote` saying so. Confirm with a text search before treating the symbol as safe to change or delete; do not proceed on the strength of a zero.
- **MUST use `query({search_query: "concept"})` for concepts/flows, `context({name: "symbolName"})` for a named symbol, or `impact` for blast radius, on read-only callers, dependencies, imports, or execution flow.** Graph first; text search only for empty/`UNKNOWN`/literals.
- For security review, `explain({target: "fileOrSymbol"})` lists taint findings (source→sink flows; needs `analyze --pdg`).

## Never Do

- NEVER edit a function, class, or method before MCP/CLI impact analysis.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis, and never read `UNKNOWN` as an all-clear — it means the walk could not answer, which is the one verdict that requires confirming by other means.
- NEVER rename symbols with find-and-replace — use `rename` which understands the call graph.
- NEVER commit before MCP/CLI graph change analysis.

## Resources

| Resource | Use for |
| --- | --- |
| `gitnexus://repo/hnh_server/context` | Codebase overview, check index freshness |
| `gitnexus://repo/hnh_server/clusters` | All functional areas |
| `gitnexus://repo/hnh_server/processes` | All execution flows |
| `gitnexus://repo/hnh_server/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
| --- | --- |
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->
