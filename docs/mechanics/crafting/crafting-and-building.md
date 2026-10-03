# Crafting and Building (legacy Haven & Hearth)

> **Sources:** src/haven/Makewindow.java, src/haven/MenuGrid.java, src/haven/MenugridPanel.java, src/haven/Resource.java, src/haven/Glob.java, src/haven/Session.java, src/haven/RemoteUI.java, src/haven/Message.java, src/haven/UI.java, src/haven/MapView.java, src/haven/Item.java, src/haven/Inventory.java, src/haven/ISBox.java, src/haven/FlowerMenu.java, src/haven/Progress.java, src/haven/GobHealth.java, src/haven/OCache.java, src/haven/MCache.java, res/compiled/paginae/craft/rustroot.res, Ring of Brodgar wiki (Legacy:Quality, Legacy:Hearth Fire, Legacy:Stone Axe, Legacy:Brickwall, Legacy:Roundpole Fence, Legacy:Oven, Legacy:Village Claim, Category:Legacy Structures), ringofbrodgar.com/wiki/Ore_Smelter (current-world page), havenandhearth.fandom.com/wiki/Rustroot_Extract, havenandhearth.com forum tips thread (shift-placement tip)

## Summary

Legacy Haven & Hearth has one generic "making" protocol that covers hand crafting, tool making, cooking prep, and the placement and construction of every structure. The client is a thin, untrusted terminal: it renders a menu of server-pushed action entries (paginae), opens a server-defined crafting window, and relays button presses. All recipe data, ingredient validation, skill gating, quality math, and construction state live on the server.

The two pillars of the domain are:

- **Making (crafting).** A craftable is represented in the UI by an action resource under `paginae/craft/*`. Clicking it sends the menu action `act("craft", <recipe-id>)` through `src/haven/MenuGrid.java`. The server replies with a `make` widget (`src/haven/Makewindow.java`) carrying the recipe display name, followed by a `pop` message that lists required inputs and predicted outputs as resource/count pairs. The player then sends back `make 0` (craft one) or `make 1` (craft all) and the server validates ingredients, consumes them, rolls the result quality, and spawns the output items.
- **Building.** Structures are normal game objects (gobs). The server drives the client into placement mode with the `MapView` `place` message (`src/haven/MapView.java`); the player clicks and the client answers `place <coord> <button> <modflags>`. What appears is a construction plan gob that sinks delivered materials (sent via `itemact` on the plan gob). As materials accumulate the server re-sends the object resource/dynamic state so the plan visually advances through construction stages, until the finished structure replaces the plan. Structure quality is a weighted average of the qualities of the materials committed to it.

Production stations (oven, kiln, smelter, finery forge, quern, churn, tanning tub, and so on) are gobs that act as crafting contexts: they are fueled and loaded through item interactions (`itemact`) and flower menus (`src/haven/FlowerMenu.java`), run their work on server ticks, and apply their own quality and fuel terms to the result.

Crafting outcomes are softcapped by skills and attributes: the result quality can never meaningfully exceed what the crafter's relevant skill/stat pair supports. Skills are learned with learning points (see [learning points and curiosity](../skills/learning-points-and-curiosity.md)); quality itself is an item attribute covered in [items and quality](../items/items-and-quality.md).

## The action menu and craft paginae

The crafting menu is not hard-coded in the client. It is a tree of *paginae*: server-defined action resources whose icons, names, hotkeys, category parenting, and tooltip text all come from resource files the server serves.

- The client keeps the set of visible paginae in `Glob.paginae` (`src/haven/Glob.java`). The server adds/removes entries with the paginae session message handled in `Session.handlerel` as `RMSG_PAGINAE` (`src/haven/Session.java`): a sequence of records, each an action byte `'+'` or `'-'` followed by a resource name string and a uint16 resource version (`Glob.paginae(Message)`). The wire constants are named in `src/haven/Message.java` (`RMSG_PAGINAE = 5`).
- The menu itself is `MenuGrid` (`src/haven/MenuGrid.java`), wrapped by `MenugridPanel` (`src/haven/MenugridPanel.java`). It lays entries into a resizable grid (default 4x4, `MenuGrid.gsz`), resolving the current page's children by walking the `AButton.parent` links of every pagina (`MenuGrid.getSubResources`). Entries with no action arguments sort as categories and open a submenu; leaf entries execute.
- Each pagina resource carries an `action` layer parsed by `Resource.AButton` (`src/haven/Resource.java`): parent resource name plus uint16 parent version, display name, a *prerequisite skill* string that the client parses and discards, a hotkey char, and a string array `ad` - the action command. Optionally a `pagina` layer (`Resource.Pagina`) supplies the tooltip text.
- Clicking a leaf sends `wdgmsg("act", <ad strings>)` up the widget tree, which the client serializes as a `RMSG_WDGMSG` message addressed to the menugrid widget id (`RemoteUI.rcvmsg` in `src/haven/RemoteUI.java`). So "craft a Rustroot Extract" is literally `act("craft", "rustroot")` reaching the server.
- The server may also force the menu to a page with the `goto` uimsg (`MenuGrid.uimsg`): an empty string returns to the root page, otherwise the named resource becomes the current category.

A concrete decoded example shipped with this client (`res/compiled/paginae/craft/rustroot.res`, version 3): action layer parent = `paginae/craft/potions` v1, name = "Rustroot Extract", prerequisite skill = "geo", hotkey = 'R', `ad = ["craft", "rustroot"]`. This demonstrates the whole model in one file: the craft verb, the server recipe id, the category tree, and the skill prerequisite travel in the resource; the server still owns the actual gating decision.

Client-side specials in the same menu: `crime`, `tracking`, and `swim` are toggle actions the client handles locally by adding/removing buffs with fixed negative ids (-1, -2, -3 in `MenuGrid.use`); they still round-trip as `act` commands so the server knows the toggle state. The `paginae/add/hide/*` resources (`paginae/add/hide/plan`, `paginae/add/hide/wall`, `paginae/add/hide/gate`, and friends, added locally in the `Glob` constructor) are client-side display filters for map objects - among them *building plans*, *walls*, and *gates* - and are not server mechanics; do not model them as such.

The wiki menu paths ("Craft > Tools > Stone axe", "Build > Walls and Fences > Brickwall", quoted from Ring of Brodgar object pages) are exactly this pagina parent tree rendered by `MenuGrid`.

## The making window (Makewindow semantics)

The crafting dialog is the `make` widget implemented by `src/haven/Makewindow.java`.

- **Creation.** The server creates it as a new widget of type `make` whose only factory argument is the recipe display name (`Makewindow` factory, `Widget.addtype("make", ...)`). The client stores the window in `UI.make_window` (`src/haven/UI.java`) and mirrors the name into `Makewindow.craft_name`. The window caption is "Crafting" and it hosts two buttons: "Craft" (`obtn`) and "Craft All" (`cbtn`).
- **Recipe contents ("pop").** The server pushes a `pop` uimsg on the window: a flat list of `(resource-id, count)` int pairs - required ingredients first, terminated by a `-1`, then result pairs. The client builds one 1x1 `Inventory` box per slot and drops an `Item` into each (`Makewindow.uimsg`). On the result items the count overlay (`Item.num`, drawn by `Item.draw` via `Item.getqtex`) shows how many units one craft produces; on the inputs it shows the required quantity. The ingredient items are displayed with quality unset (the window passes q = -1), so the window is a *requirement preview*, not a snapshot of the player's goods.
- **Required versus provided.** The makewindow never shows what the player currently holds. The player must have the ingredients in the relevant scope (inventory, or a container the recipe accepts - for example, Rustroot Extract needs an empty jar and a water-filled container in inventory, per the Fandom recipe page) and the server re-validates on every craft attempt. Bots in this repo (`src/union/JSBotUtils.java`, `checkCraft`/`craftItem`) simply wait for `is_ready` and recipe-name equality, then press craft - the client performs no ingredient checking of its own.
- **Craft commands.** Pressing the buttons (or Enter, which sends `make 0`, or Ctrl+Enter which sends `make 1` - `Makewindow.globtype`) sends the widget message `make` with a single int argument: `0` = craft one, `1` = craft all. That message goes to the server as `RMSG_WDGMSG` on the makewindow widget id.
- **Lifecycle.** `is_ready` flips true when `pop` arrives. The window is closable like any `HWindow`; `Makewindow.unlink` clears `UI.make_window`, and the client tracks a single crafting dialog at a time (`UI.make_window` is one slot) - a server should assume one open make window per player. The server destroys windows with `RMSG_DSTWDG` (`src/haven/Session.java`, `RemoteUI.run`).

There is no per-recipe client logic anywhere: the same window serves stone axes and pies. (Terminology note: current-world clients later replaced these input boxes with a dedicated `MakeBox` widget; in this legacy client the boxes are plain `Inventory` widgets, and the `pop`/`make` choreography is the same thing older community material calls MakeBox semantics.) A Rust server must reproduce the *message choreography*, then attach its own recipe semantics.

## Crafting flow, end to end

1. Player clicks a craft leaf in `MenuGrid`; client sends `act("craft", "<recipe-id>")` (the exact `ad` strings come from the pagina resource).
2. Server resolves the recipe, checks the prereq skill is learned, checks tool/station context, and decides whether the window may open. (The client cannot enforce any of this; the `AButton` prerequisite field is advisory only.)
3. Server creates the `make` widget with the recipe name, then sends `pop` with input and output `(resource-id, count)` pairs. Resource ids are resolved by `Session.getres` (`src/haven/Session.java`): the server binds an id to a resource name/version with `RMSG_RESID` when the id is first used, and must serve the resource bytes themselves.
4. Player inspects requirements, presses Craft (or Craft All / Ctrl+Enter).
5. Server re-validates: all required ingredients present and consumable, tool condition, station state, stamina cost affordable. On success it removes the inputs, computes the result, and creates the output item widgets inside the player's inventory widget (`inv`, `src/haven/Inventory.java`); item widgets are `item` widgets whose creation args are resource id, quality, drag flag, tooltip string, and stack count (`Item` factory in `src/haven/Item.java`).
6. For `make 1` the server repeats step 5 until an ingredient, tool, or inventory-space precondition breaks, then stops (silently stopping at the last successful iteration is acceptable client behavior; the window simply stops producing).
7. The window stays open for further crafts; the server closes or leaves it as it sees fit. The client is also perfectly happy for the server to answer `act` without opening any window (for instance when gating fails).

The generic widget transport under all of this: `Message.addlist`/`Message.list` (`src/haven/Message.java`) tag every argument as `T_INT`/`T_STR`/`T_COORD`; coordinates are two int32s; strings are null-terminated. Reliability, sequencing, and the `MSG_OBJDATA`/`OD_*` object channel are documented in the network and objects domain files of this documentation set.

## Server-side recipe model

A recipe definition in the server should carry at least:

- **id** - the string used as the `ad` argument (for example `rustroot`).
- **display name** - pushed as the makewindow title.
- **inputs** - list of (resource, count, optional constraint) where a constraint can express things like "container with at least 0.4 L water" rather than a plain item.
- **outputs** - list of (resource, count).
- **prerequisite skill** - must match the string embedded in the pagina action layer; the server gates on it (skills and learning points are covered in [learning points and curiosity](../skills/learning-points-and-curiosity.md)).
- **tool requirements** - optional equipped/held tool resources that participate in the quality formula.
- **station requirements** - optional proximity target (type of gob, radius) such as an oven or anvil.
- **quality weights and softcap** - the ingredient weighting, tool weight, and the attribute pair used for the softcap (next section).
- **timing model** - instant (single validation + creation) versus station-job (fuel, ticks).

Paginae must be pushed to the client at login (and whenever skill changes add or remove craftable pages) using `RMSG_PAGINAE`, and the corresponding resources must be downloadable from the server. Note that `Glob.paginae` is a sorted set keyed by resource identity; name/version collisions resolve through `Resource.load`.

## Ingredient requirements, tools, and stations

- **Inventory-scope validation.** The canonical check is "can the server remove every input from the player's possessions?" The client-side verbs that move items (`take`, `drop`, `transfer`, `transfer_such_all`, `iact`, `itemact` on `Item` and `Inventory`, `src/haven/Item.java`, `src/haven/Inventory.java`) show the model: items are individual widgets with resource, quality, and count; the server owns the authoritative inventory contents.
- **Tool requirements.** Recipes that name a tool (churn, anvil, smithy's hammer) consume nothing but require the tool to exist in scope, and its quality enters the result formula with an explicit weight (Legacy:Quality documents loftar's rule: the tool is averaged in, "often with a weight of 1/4", i.e. `q' = (3*q + q_tool)/4` for the common case).
- **Station/place requirements.** Station recipes require the player to stand near a specific gob type (near a table or anvil for the relevant crafts, in front of an oven, and so on). The client has no notion of this; the server performs a radius check around the player. For *placement* of station objects the server can supply a display radius (the fourth `place` argument, drawn by `MapView` as a circle - see `plrad`, with the beehive `gfx/terobjs/bhive` as the cited example in the client).
- **Tool wear.** Items have wear/state attributes driven by resources (`src/haven/Item.java` tooltip fields; attribute framework in the items domain, [items and quality](../items/items-and-quality.md)). The server should decrement tool condition on use; the legacy client exposes no tool-wear UI beyond item tooltips and state overlays, so the exact per-craft wear values are server policy (see Open questions).

## Quality of crafted results

Quality (Q) is the central production stat: roughly every effect multiplies by the quality factor `QM = Q/10` (1x at Q10, 2x at Q40, 3x at Q90 - Legacy:Quality). Default quality for natural objects is 10.

The authoritative crafting formula, quoted from loftar (Legacy:Quality, "Crafting formula"):

1. `q_item = sum(q_i * w_i) / sum(w_i)` - a weighted average over the consumed ingredients; each ingredient type has a design-time weight, and types that matter more weigh more (the wiki's arrow example: heavier weight on branch than bone - the actual per-recipe weights are game data).
2. If a tool is used: `q_item = (3*q_item + q_tool) / 4`.
3. If attributes/skills are involved, compute `q_skill = (s_1 * s_2 * ... * s_n)^(1/n)` (geometric mean of the involved skill/stat values). If `q_skill < q_item`, then `q_item = (q_skill + q_item) / 2` - the softcap. Otherwise the value stands.

Worked examples documented on Legacy:Quality (verify against your own data pack): stone axe `Q = (q_branch + q_stone)/2`, softcapped by Survival; boards `Q = sqrt(q_log * q_tool)`, softcapped by Carpentry; leather `Q = (3*q_hide + q_bark + q_water + q_tub)/6`; full metal products `Q = (4*q_metal + 1.5*q_anvil + 1.5*q_hammer)/7`; smithy's hammer `Q = (3*q_metal + q_anvil)/4`. Skill-dependent softcap pairings are enumerated on the same page (Sewing+Dexterity, Smithing+Strength or +Psyche, Cooking+Perception, Survival caps arrows/bone saws, Farming caps seeds/flour/trees, Marksmanship caps bows).

Station-produced items (oven, kiln, smelter, finery forge) use the station formula instead:

- `Q = (2*q_item + q_oven + q_fuel) / 4`, where `q_fuel` is the average quality of the fuel items burned for that job.

Quality is stored per item and displayed by the client from the item's quality field plus the "quality N" text the server writes into the item tooltip (`Item.patt` regex, `Item.settip`, `Item.get_quality` resolving the inner-quality `q2` override). How quality interacts with item transfer, stacking, and wear is the items domain's topic: see [items and quality](../items/items-and-quality.md).

## Skills as crafting gates

Two distinct mechanisms, both server-side:

1. **Unlock gating.** A recipe's pagina action layer names a prerequisite skill, and the server must refuse `act`/`make` flows for unlearned skills. Learned skills are bought with learning points; the LP economy and curiosity/study loop are specified in [learning points and curiosity](../skills/learning-points-and-curiosity.md).
2. **Quality softcapping.** As above: the skill level (often paired geometrically with an attribute) caps achievable output quality without blocking the craft. A Q40 bone plus a Q10 branch with Survival 15 yields Q25, not Q35, because `(35 + 15)/2 = 25` (Legacy:Quality worked example).

The client evidences both: it renders the prerequisite string (then discards it), and it renders attribute meters and the skill-relevant stat windows (`src/haven/CharWnd.java`) purely as display state pushed by the server (`RMSG_CATTR`, `Glob.cattr`).

## Batch crafting ("Craft All")

`make 1` requests repeated crafting. Design decisions a server must make (legacy behaviors to replicate, marked where unverified):

- Loop the single-craft procedure while every precondition holds: inputs available, output space, tool/station still valid, stamina affordable.
- Each iteration should re-roll quality from the *actually consumed* items (auto-picked inputs can differ in quality between iterations); the legacy client's `transfer_such_all_ql` / `transfer_such_all_qldesc` item verbs (sort by quality ascending/descending, `Item.mousedown`) exist precisely because players care which stack gets consumed first.
- Stop condition and partial-failure reporting to the player (error text widget vs silent stop) are not pinned down by this client; see Open questions.

## Building system: plans, materials, stages

Every structure - houses, walls, fences, furniture, ovens - is built through the same gob-based pipeline.

1. **Selection.** The player activates a build pagina (menu category "Build > ..." on the wiki pages). As with crafting, this arrives as `act(<verb>, <object-id>)` on the menugrid; the verb for build entries is not visible in this client (see Open questions), but everything downstream is.
2. **Placement mode.** The server answers on the map view widget: `MapView.uimsg` case `place` takes `(resource-name, version, on-tile flag[, radius])` and creates a *ghost* gob (`plob`) that follows the mouse, rendered from the named resource (`ResDrawable`). `on-tile` snaps the ghost to the tile grid: `plontile XOR ui.modshift` decides snapping per frame, and `MapView.tilify` centers on the tile (`MCache.tileSize` is 11x11 client units). A radius argument draws a placement circle. Shift-dragging constructions off the grid is a known player technique (forum tips thread), which is exactly the `plontile ^ modshift` path.
3. **Commit.** Clicking sends `wdgmsg("place", <coord>, <button>, <modflags>)` - the map coordinate, or the ghost position if the click lands on a gob (`MapView.mousedown`, `map_place`). The server validates terrain (Roundpole Fence: "cannot be built on dirt"), tile occupancy, claim permissions, and spacing, then either spawns the *construction plan* gob (an object-data update with the plan's resource and initial dynamic state) or cancels with the `unplace` uimsg.
4. **Material sinking.** The plan gob lists its total material demand. The player takes a material item "in hand" (dragging it on the cursor) and clicks the plan: the client sends `itemact` with the target gob id and position (`MapView.iteminteract`: `wdgmsg("itemact", cc, mc, modflags, hit.id, hit.position())`). The server consumes the held item and credits it to the plan. Repeat until the demand is met - this is the "repeated material deliveries" flow. RoB object infoboxes record these demands directly (Object(s) Required: e.g. Hearth Fire = Branch x5 plus the quest item "A Beautiful Dream!"; Oven = Brick x45; Ore Smelter = Brick x35, Stone x10, Bar of Hard Metal x3).
5. **Stages.** As materials arrive, the server advances the plan's visual stage by re-sending the gob's resource/state: `OCache.cres` (`src/haven/OCache.java`) replaces the gob's `ResDrawable` whenever the resource or the dynamic-state blob (`sdt`) changes. The concrete stage-to-sprite mapping is defined by each structure resource (sprite layers consume the `sdt`), so a server needs the legacy resource pack to reproduce stages exactly. A `prog` widget (`src/haven/Progress.java` - server-pushed "N%" text updated with the `p` uimsg) is the generic way to expose percentage progress for timed construction work.
6. **Completion.** When the last material is credited, the plan converts to the finished structure: replace the gob resource (again via `cres`) or remove and respawn the gob. Structure quality is computed per loftar's buildable rule (Legacy:Quality): average the qualities of the delivered items *per material type*, then weighted-average the per-type averages, weighting types by obvious importance. Per the same page, "the quality of structures is not affected by who builds them or what they are repaired with."
7. **Damage/health.** Structures have hit points and a soak value (RoB infoboxes: Brickwall HP 5000, soak 70; Roundpole Fence HP 20, soak 0). Damaged objects take the `OD_HEALTH` attribute (`Session.OD_HEALTH = 14`), rendered by `GobHealth` (`src/haven/GobHealth.java`) as a red tint scaling with `hp/4`.

Walls, fences, and gates extend step 4/5 with an *expansion* model rather than one-shot placement (next section).

## Walls, fences, gates, and cornerposts

Legacy walls and fences are cornerpost-expandable systems (Legacy:Brickwall, Legacy:Roundpole Fence):

- You place an **initial cornerpost** as a plan and sink its (large) material demand: Brickwall initial cornerpost = Brick x300 + Bar of Wrought Iron x10; Roundpole Fence initial cornerpost = Branch x10.
- The initial Brickwall cornerpost then has a **setting period** (12 hours per the wiki) before it can be interacted with; subsequent cornerposts need fewer materials (Brick x50 + iron x5, or Branch x5) and no wait.
- Right-clicking the finished cornerpost opens a flower menu (`sm` widget, `src/haven/FlowerMenu.java`) with expansion options (expand north/south/east/west, build gate). Choosing one sends `cl <option-index>` back (petal index, or `-1` for cancel; the server confirms with the `act`/`cancel` uimsgs). Each expansion is effectively a new 1-tile plan along the wall line with its own material demand (Brickwall section = Brick x10; Roundpole section = Branch x1).
- **Gates** require a 2-tile gap flanked by cornerposts; the gate itself is a plan with its own demand (Brickwall gate = Brick x75 + Bar of Steel x5; Roundpole gate = Branch x5). A Brickwall gate issues a **Key** to the character who laid the final material; Roundpole gates are keyless. Day-to-day gate use (open/close, lock with a key) is again flower-menu interaction on the gate gob, so the server must bind per-gate key ownership and door state to the gob.
- Placement constraints: fences cannot be built on dirt; all objects decay more slowly on paved tiles (RoB notes).

## Furniture and container placement

- **Free placement versus grid.** Furniture and containers are placed through the same `place` flow; the `on-tile` flag selects tile snapping, and the player's shift key toggles it interactively. Which objects snap and which place freely is per-object server data (the oven's 3x3 paved footprint is a documented tile-bound case), and the client defers entirely to the flag.
- **Containers.** A container gob opens an inventory-style window when interacted with; the server creates the window and `inv` widgets and thereafter mirrors item movement messages. Stockpiles are the special-cased surface widget `ISBox` (`src/haven/ISBox.java`): created with `(resource, rem, av, bi)` and rendering "rem/av/bi"; it supports `click` (take one), `xfer` (transfer one), `xfer2` (wheel transfer with modflags), `drop`, `iact`, and the `chnum` uimsg to update the three counters. Stockpiles are the ground-side material buffer for building logistics.
- **Liftable objects.** RoB infoboxes mark objects "Can be Lifted" yes/no; lifting is a flower-menu interaction that turns the gob into a carried item. Whether an object can be lifted is thus recipe/object metadata on the server.

## Hearth fire and claim anchors

The hearth fire is the player's anchor object: login/spawn point and fast-travel target, and the visual anchor of a personal claim.

- Built (not crafted) via "Adventure > Light Hearth Fire" with Branch x5 plus "A Beautiful Dream!" (a quest item), skill requirement Wilderness Survival; built hearth fires have 250 HP (Legacy:Hearth Fire).
- Its flame color encodes owner presence (bright green online, pale green/white offline) - server pushes the appropriate state or resource variant.
- A new hearth fire turns the old one into a plain wood pile; hearth fires are destructible, and destroying one off your own claim leaves a "Vile Vapor of Vandalism" scent - vandalism forensics are server-side scent bookkeeping.
- The hearth fire is also the anchor of the personal claim, and the Village Claim (Legacy:Village Claim) is the village-scale equivalent (101x101 claimed area, authority pool, Lawspeaking skill, 30,000 LP cost shared among up to five Yeomanry holders). Claim sizes, authority, and the social rules that govern building permissions inside claims belong to the social/claims coverage of this documentation set rather than this file; the crafting-relevant contract is: **placement and material crediting on a plan gob must consult claim/permission state at commit time and at every itemact.**

## Production stations: oven, kiln, smelter, finery forge

Stations combine the building system (they are built from plans with material demands) with a tick-based production loop:

- **Oven** (Legacy:Oven): Baking skill, Brick x45, 3x3 paved footprint. Fueled with branches (blocks of wood, boards, and charcoal count as 2 branches each); up to four dough slots; lit via the right-click flower menu ("Light"). Result quality: `(DoughQ*2 + OvenQ + FuelQ)/4`; the oven's own quality is the average quality of its bricks. Over-fueling burns the contents to ash - fuel amount per item is game data.
- **Kiln** (Legacy:Kiln; Category:Legacy Structures): fires clay into pottery/ceramics and bricks. Legacy:Quality states pottery follows the oven formula with the ceramic as the "dough" and the crafter's Dexterity as the softcap on the unfired piece.
- **Smelter / Ore Smelter** (Legacy:Ore Smelter exists in the legacy category; the numbers below are from the current-world page and must be re-verified for legacy): Metal Working skill, Brick x35 + Stone x10 + Bar of Hard Metal x3, 5x5 internal storage, up to 25 ore per load, 12 charcoal to fuel, roughly 55 minutes per load. Smelting quality: `(2*q_ore + q_smelter + q_avg_fuel)/4`; slag keeps the ore's quality. The modern implementation tracks, per item, the average fuel quality it has seen while burning (patch note quoted on the Ore Smelter page) - a sound model for a new server.
- **Finery Forge** (Legacy:Finery Forge): refines ore into higher-tier metal products; same fuel-and-tick pattern; legacy specifics are thin on the wiki (see Open questions).

All of them share the same server shape: an inventory (per-gob item store), a fuel store with fuel-quality averaging, a lit/unlit state machine with burn-down per tick, a job queue or per-item progress values, and the station quality term in the output formula. Fueling and loading are driven by `itemact` on the station gob and by flower-menu options (Light, and so on); internal inventories are mirrored with `inv` widgets; `Item.meter` (the 0-100 red-to-green bar, `Item.uimsg` case `meter`) is the per-item progress overlay the client can display inside such stores. When a station job finishes, the server morphs the stored item widget in place rather than recreating it: the `chres` uimsg swaps resource and quality (`Item.chres`), `tt` replaces the tooltip, and `num` updates stack counts - all three are handled in `Item.uimsg` (`src/haven/Item.java`).

## Repair and decay

- Structures and placeables have HP and soak; repair materials are per-object ("Repaired With": Brick for the oven and smelter, Branch for the roundpole fence). Repair is an `itemact` with the material on the damaged gob, restoring HP; it never alters the structure's quality (Legacy:Quality).
- Objects decay over time, more slowly on paved tiles (RoB notes). Decay is a server tick concern; the client observes it only through health updates, stage/sprite changes, and eventual object removal (`OD_REM` in the object channel).
- The smelter patch note quoted above ("no longer reset their progress when burning out") documents the modern fix for a legacy failure mode: legacy ovens could burn dough when lit with stale fuel states (the "dough storage bug" note on Legacy:Oven). A new server should track per-item fuel quality and progress from the start.

## Server implementation notes

- **Recipe data model.** Static, server-side registry keyed by the `ad` argument string. Include: display name, inputs/outputs with weights and optional item constraints, prerequisite skill, tool and station requirements (gob type + radius), quality weights/softcap attributes, timing model, and the pagina tree position. The pagina resource files themselves (action layer with parent/name/prereq/hotkey/ad, optional pagina tooltip, image) must be generated and served so the client can render the menu; `res/compiled/paginae/craft/rustroot.res` is a byte-level template.
- **Station proximity checks.** Resolve the player's gob position against candidate station gobs of the required type within a small radius; treat the radius as data per recipe. The same check gates "near a table/anvil" recipes. Placement radii reuse the `place` radius field for display.
- **Progress persistence.** Plans and station jobs are world state: store credited materials per plan gob (item resource, quality, count), stage index, and per-job progress/fuel averages in the same durable store as gob state so server restarts do not lose half-built walls or lit ovens.
- **Material sinking.** On `itemact` against a plan: validate the held item against the plan's remaining demand, consume it, credit it (with its quality - needed for the buildable quality formula), advance stage, and update health/progress visuals. Reject and leave the item in hand on mismatch. The client cannot tell the difference between a real plan and a decoy; the server answers or does not.
- **Tick-based versus instant.** Hand crafting is instant and atomic (validate-consume-produce in one step). Station work is ticked: fuel burn-down, per-item progress, quality snapshotting at completion. Keep one canonical tick (for example 100 ms) and derive all durations from data.
- **Failure modes to handle explicitly.** Missing input mid-batch (stop the loop after the last success); inventory full on output; player moved out of station radius; station gob destroyed or decayed mid-job; claim permission revoked between placement and completion; logout mid-flow (the client's `make_window` dies with the session, so all authoritative state must already be server-side); duplicate `itemact` storms from held-item spam (idempotency per item instance).
- **Client trust boundary.** Everything here is replayable by bots (this repo ships a scripting layer that presses Craft automatically). Treat every incoming `act`, `make`, `place`, `click`, and `itemact` as hostile until validated.
- **Quality bookkeeping.** Snapshot ingredient qualities at consumption time (crafting) or delivery time (building); do not re-derive from stacks. The items domain document ([items and quality](../items/items-and-quality.md)) defines the quality attribute itself, the inner-quality tooltip override (`Item.q2`), and the quality multiplier conventions.

## Open questions

- **Build-menu action verb.** The exact `ad` strings for build paginae (presumably a `build`/`place` verb plus object id) are not recoverable from this client; determine by capturing a live legacy session's menugrid `act` messages, or by diffing a fuller legacy resource pack's `paginae/act/*`/`paginae/build*` resources.
- **Per-recipe data.** Ingredient weights `w_i`, tool weights, and softcap attribute pairs per recipe are game data; the wiki documents examples (stone axe, boards, leather, metal products) but not the full legacy table. Extract from legacy resources/wiki object pages as they are digitized.
- **Legacy smelter/kiln/finery numbers.** Fuel amounts, load sizes, and durations quoted above for the smelter are current-world values; the legacy pages exist (Category:Legacy Structures) but were empty or not yet fetched. Fetch Legacy:Ore Smelter, Legacy:Kiln, Legacy:Finery Forge and reconcile.
- **Craft All stop reporting.** Whether legacy servers sent an error widget/message when batch crafting stopped early, or stopped silently. Determine from a live capture (watch for `RMSG_NEWWDG` text/error widgets after `make 1`).
- **Pop refresh semantics.** The makewindow `pop` appears to be sent once per window open; whether ingredient counts ever update dynamically (a second `pop` rebuilding the lists) needs a capture of a long-lived window.
- **Tool wear per craft.** Legacy per-craft wear values for tools are unknown; decide whether to adopt modern values or reconstruct from patch notes.
- **Skill prerequisite names.** The prerequisite field in action resources is a short code (seen: "geo" for a potion page). Build the full code-to-skill mapping from the legacy resource set.
- **Structure stage encodings.** The mapping from `sdt` bytes to construction-stage sprites is defined by each structure resource's sprite code; the full legacy resource pack is required (this repo's `res/compiled` only carries a client-side subset such as the hide-filter paginae and one craft pagina).
- **Instant-craft timing.** Whether legacy used the `prog` widget for any hand-crafting (versus only for station/construction work) is unresolved; if timed crafts exist, their durations are data.
