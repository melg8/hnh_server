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

If the change touches `unsafe` code, additionally run `cargo miri test` for
the affected crate. If you cannot run the toolchain in your environment, state
that explicitly in your summary instead of claiming success.

## Commit Discipline

- Small, focused commits with clear English messages (imperative mood).
- Never commit generated build artifacts (`target/`, `classes/`, `build/`).
- Never commit secrets, tokens, or credentials.
