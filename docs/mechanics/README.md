# Haven & Hearth - Server Mechanics Reference

This folder is the golden source-of-truth blueprint of legacy Haven & Hearth
server-relevant mechanics. It was distilled from the open-source client in
`src/haven/` (the client renders, the server owns), the legacy Ring of
Brodgar wiki, the fandom wiki, official forums and patch notes, and the repo
configs `etc/needed/fep.conf` and `etc/needed/curio.conf`. It is written for
AI agents implementing the Rust server under `server/`: start here, follow
only the links your current task needs, and treat the linked domain documents
as the authority to implement against - and to update when implementation
reveals new nuance.

## How to use this folder

- Always start from this index. Follow links relevant to your task only; do
  not load whole domains into context without need. Every domain doc opens
  with a short `## Summary` sized for exactly this triage.
- Cross-references inside a domain doc ("see ...") are part of the contract;
  follow them when the topic touches your task.

Question-to-document map:

| Question | Document(s) |
| --- | --- |
| How do clients authenticate? | [session-lifecycle.md](network/session-lifecycle.md) |
| What does the server stream when? | [network-protocol.md](network/network-protocol.md) |
| Wire format of message X? | [network-protocol.md](network/network-protocol.md) |
| Which coordinate units does the world use? | [map-and-terrain.md](world/map-and-terrain.md) |
| How does in-game time pass? | [time-weather-astronomy.md](world/time-weather-astronomy.md) |
| How are objects spawned, updated, and removed? | [objects-and-dynamics.md](objects/objects-and-dynamics.md) |
| What must be persisted? | [visibility-and-lifecycles.md](objects/visibility-and-lifecycles.md) plus the domain docs it cites |
| Which character state does the server own? | [attributes-and-vitals.md](character/attributes-and-vitals.md) |
| How does eating raise attributes? | [food-and-fep.md](character/food-and-fep.md) |
| How are skills bought and curios studied? | [learning-points-and-curiosity.md](skills/learning-points-and-curiosity.md) |
| How does item quality affect damage? | [items-and-quality.md](items/items-and-quality.md) + [combat-system.md](combat/combat-system.md) |
| How do crafting and building work? | [crafting-and-building.md](crafting/crafting-and-building.md) |
| How do crops grow? | [farming-and-plants.md](livestock/farming-and-plants.md) |
| How do animals behave, breed, and produce? | [animals-and-husbandry.md](livestock/animals-and-husbandry.md) |

Conventions shared by every document in this folder:

- Fixed skeleton: H1 title, a `> Sources:` block quote, `## Summary`,
  mechanics sections, `## Server implementation notes`, and a closing
  `## Open questions`.
- Claims cite their evidence: client code as `src/haven/File.java` plus
  symbol names, external material by wiki page or forum thread.
- Exact numbers appear only where sourced (repo configs, client code, or a
  cited wiki/forum source); unknowns are explicitly quarantined in
  `## Open questions` instead of being invented.

Update policy: when implementing server code, if you discover a nuance that
contradicts or refines a doc, update the relevant domain doc in the same
change set and note what changed. This folder is a living golden reference,
not a snapshot; repo-wide agent conventions live in [AGENTS.md](../../AGENTS.md).

## Domain index

| Domain | Documents | What it covers |
| --- | --- | --- |
| Network | [network-protocol.md](network/network-protocol.md) | Wire-level session protocol: UDP framing, little-endian encoding, reliable ordered layer, MSG_*/RMSG_*/OD_* layouts, widget messages, session error codes. |
| Network | [session-lifecycle.md](network/session-lifecycle.md) | Full session sequence: pre-connect endpoints, TLS auth handshake and cookie, MSG_SESS, widget bootstrap, character selection, in-world init order, teardown, reconnection. |
| Network | [communication.md](network/communication.md) | Area chat (slenchat widget, radius relay, system lines) and parties (invite/accept flower menus, RMSG_PARTY broadcast, pv roster, leave/disband), with the server-policy decisions the client cannot reveal. |
| World | [map-and-terrain.md](world/map-and-terrain.md) | Tile/subtile/grid coordinate model, MAPDATA grid streaming, tilesets, claim and plot overlays, flavor-object replication, terraforming. |
| World | [time-weather-astronomy.md](world/time-weather-astronomy.md) | GLOBLOB world-clock blob (absolute time, day/moon/year fractions, ambient light), 3x time ratio, 365-day year, and the absence of any weather model. |
| Objects | [objects-and-dynamics.md](objects/objects-and-dynamics.md) | The gob model and per-object state protocol: OD_* sub-message encodings, movement and interpolation semantics, frame ordering, removal tombstones, ack-gated retransmission. |
| Objects | [visibility-and-lifecycles.md](objects/visibility-and-lifecycles.md) | Server-authoritative visibility: spawn/update/retract lifecycle, view radius, plant stages, item drop and lift flows, and what object state must be persisted. |
| Character | [attributes-and-vitals.md](character/attributes-and-vitals.md) | Permanent and volatile character state: base attributes and CAttr wire format, vitals meters (health, stamina, hunger/energy, happiness, authority), speed state, belief sliders. |
| Character | [food-and-fep.md](character/food-and-fep.md) | Food-driven growth: FEP accumulation and attribute-gain algorithm, fep.conf format, quality scaling, eat interaction flow, hunger/energy economy, starvation, drinking, buffs. |
| Skills | [learning-points-and-curiosity.md](skills/learning-points-and-curiosity.md) | Learning-point economy: earning LP via curios and first-time discoveries, spending on skills, the attention budget, and time-based study driven by curio.conf. |
| Items | [items-and-quality.md](items/items-and-quality.md) | Items as server-created widgets: inventory grids, 16-slot equipment, stockpile counters, quality packing and inner quality, item state and movement. |
| Crafting | [crafting-and-building.md](crafting/crafting-and-building.md) | The generic making protocol (paginae, Makewindow, craft flow), building placement and construction plans, production stations, softcapping by skills. |
| Livestock | [farming-and-plants.md](livestock/farming-and-plants.md) | Server-side crop simulation: sdt stage-byte rendering contract, planting flow, growth stages and durations, quality propagation, soil, trees, foraging. |
| Livestock | [animals-and-husbandry.md](livestock/animals-and-husbandry.md) | Animals as server-owned gobs: creature roster and aggression classes, taming protocol, breeding and production rates, feeding, movement/health wire contract. |
| Combat | [combat-system.md](combat/combat-system.md) | Server-authoritative openings-based combat: Fightview relation records, offence/defence bars and initiative points, damage formula, disengagement, knockdown, death. |

## Cross-cutting facts

Load-bearing constants, each traceable to its domain doc:

- `PVER = 2`: the client's protocol version constant, carried in the MSG_SESS
  handshake (`network/network-protocol.md`).
- Message taxonomy: session-level `MSG_*`, reliable sub-messages `RMSG_*`,
  object-delta sub-messages `OD_*` (`network/network-protocol.md`).
- A map grid is 100x100 tiles (`world/map-and-terrain.md`).
- A tile is 11x11 subtiles; all object positions are subtiles
  (`world/map-and-terrain.md`).
- One in-game day = 8 real hours (`SERVER_RATIO = 3`); the protocol carries
  absolute day/moon/year fractions, not deltas
  (`world/time-weather-astronomy.md`).
- One in-game year = 365 in-game days (`world/time-weather-astronomy.md`).
- Item quality multiplier: `sqrt(q / 10)` scales FEP gain, study gain, and
  damage (`items/items-and-quality.md`, `combat/combat-system.md`).
- Food (FEP) and curiosity (LP study) data tables live in
  `etc/needed/fep.conf` and `etc/needed/curio.conf`
  (`character/food-and-fep.md`, `skills/learning-points-and-curiosity.md`).

## Maintenance

Keep every doc factual: a claim without a citation is a bug. Keep each domain
doc under roughly 60KB; split a domain only if it genuinely outgrows one
file. Prefer updating the relevant domain doc over growing this README.md -
this file is an index, not a domain doc. Record uncertainty in a doc's
`## Open questions` section rather than inventing numbers.
