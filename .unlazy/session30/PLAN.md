# PLAN — session 30

Original request: the standing full-implementation prompt (see AGENTS.md /
HANDOFF.md top). Continuation of the session-29 handoff NEXT list, which is
the agreed roadmap for this session:

1. Relay pickup/build against guest STATIC gobs (drops/structures) - the
   same RelayAttack pattern with an action enum.
2. Cursor pickup redirection (merge stacks).
3. Vis-scan result caching.
4. Load-test story for the sharded save (per-node bot cohorts persisting).
5. Craft pagina ad->action wiring for remaining recipes (stretch; needs
   verified recipe data - do not invent numbers per AGENTS.md).
6. Carrying-pose state for bows (stretch; depends on 5 landing a bow).

Depth note: `tree 99` requested; honest decomposition is depth-2
(root -> integration -> 5 leaves). Filler leaves to reach 99 would violate
the skill contract; stated here per method.md.

## Contract inventory (independently omittable outcomes)

| Id | Outcome | Tier | Owns | Needs |
| --- | --- | --- | --- | --- |
| leaf-1 | Cross-node static interaction: pickup/build clicks against foreign-authority STATIC gobs (drops, structures) route through the node mesh like RelayAttack: home node computes the interaction, authority node applies state (remove drop / place structure), guests see consistent state, no desync (loot duplication impossible: single authority owns the drop's existence). | judgment | nodes.rs, game.rs (player_interact guest branch, build placement), state.rs | session-28/29 mesh (master) |
| leaf-2 | Cursor pickup redirection: picking up a ground drop while the cursor already holds a matching stack merges onto the cursor (no failed pickup); same-type inventory/cursor transfers merge stacks instead of refusing. | judgment | game.rs (pickup/take/xfer flows) | master |
| leaf-3 | Vis-scan result caching: unchanged visibility queries skip the full rescan; measured tick-time improvement at 1000 bots with zero behavior change (same 121+ tests green, same e2e verdicts). | judgment | visidx.rs, game.rs (update_visibility) | master |
| leaf-4 | Sharded-save load-test story: a committed verify script boots a 2-node cluster, drives bot cohorts on BOTH nodes through the real UDP path, restarts the nodes under load, and proves the per-node character snapshots persisted (positions/inventories restored). | judgment | server/scripts/verify_session30.sh, bots.rs (cluster cohort hooks if needed) | leaf-1 (guest pickup covers bot flows) |
| leaf-5 | Craft wiring for remaining recipes: add verified recipes (wiki-sourced, documented in docs/mechanics) so their paginae render in MenuGrid and act("craft", id) resolves; update crafting doc. STRETCH - only after leaf-1..4. | judgment | craft.rs, game.rs (menu paginae), docs/mechanics/crafting/ | master |
| root | Full battery green: cargo test, clippy -D warnings, fmt --check, test_client.py WORLD ENTRY: OK, test_craft.py OK, real-client e2e (if the JOGL env re-provisions in time), cluster e2e; HANDOFF.md session entry; commits pushed to master. | judgment | HANDOFF.md, .unlazy/session30 | leaf-1..N settled |

## Interfaces and shared assumptions

- NodeMsg stays the only cross-node wire; new kinds follow the existing
  bincode framing and the 500 ms dial retry. Ids: next variant numbers after
  session-29's (see nodes.rs NodeMsg enum).
- Static interaction relay reuses the session-28 authority rule: the
  authority node owns the drop/structure lifecycle; the home node only
  computes player-side facts (distance, inventory).
- No new wire protocol changes toward the client: all client-visible effects
  flow through existing OD_* / wdgmsg builders.
- English-only code/comments/commits; Russian in user-facing chat replies.
- Never commit the GitHub token; push via a one-shot URL, never in config.

## Verification strategy per leaf

- leaf-1: unit test (relay pickup removes drop on authority + guest retract
  consistency + no duplicate loot), cluster probe verdict line.
- leaf-2: unit test (pickup with occupied cursor merges counts+quality rules
  per items doc), test_craft-style wire probe still green.
- leaf-3: A/B timing at 1000 bots (--perf tick_us before/after), all
  existing vis tests green.
- leaf-4: verify_session30.sh prints SESSION30 E2E: OK with explicit
  restart-and-restore phase lines.
- leaf-5: recipe data cited from the wiki in the doc; wire probe crafting
  the new recipe once.

## Dispatch waves

- Wave 1: leaf-1, leaf-2, leaf-3 (independent files/flows; leaf-1 and leaf-3
  both touch game.rs - sequenced leaf-1 first, then leaf-3 rebases on it).
  leaf-2 is independent of both (session flow files).
- Wave 2: leaf-4 (needs leaf-1's guest pickup for bot flows).
- Wave 3: leaf-5 (stretch).
- Root: after all settled waves.
