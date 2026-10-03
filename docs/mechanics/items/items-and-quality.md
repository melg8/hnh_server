# Items, Inventory, Equipment, and Quality (Legacy Haven & Hearth)

> **Sources:** src/haven/Item.java, src/haven/Inventory.java, src/haven/Equipory.java, src/haven/ISBox.java, src/haven/DTarget.java, src/haven/DTarget2.java, src/haven/DropTarget.java, src/haven/GiveButton.java, src/haven/Fightview.java, src/haven/MapView.java, src/haven/Window.java, src/haven/Makewindow.java, src/haven/Glob.java, src/haven/CharWnd.java, src/haven/Avatar.java, src/haven/UI.java, src/haven/Widget.java, src/union/jsbot/JSItem.java, src/union/jsbot/JSEquip.java, src/union/JSBot.java, etc/needed/fep.conf, etc/needed/curio.conf, Ring of Brodgar wiki pages "Quality" and "Legacy:Combat Actions", legacy.havenandhearth.com portal doc-src (rules live server-side), Haven & Hearth official forums

## Summary

This document is the authoritative server-side blueprint for legacy Haven & Hearth
items: what an item is, how it lives in inventories and equipment, how it moves
between places, and how the quality (QL) number that dominates the game is
produced and consumed. The legacy client (src/haven/**) is UI-only: every item a
player sees is a widget the server created and every item action is a widget
message (wdgmsg) the server must interpret and validate. There is no client-side
item simulation to fall back on - the server owns all item state.

Key facts established from the client code:

- An item widget is created from a small tuple: resource id, packed quality
  int, optional "dragging on cursor" offset, optional server-composed tooltip
  string, optional number (src/haven/Item.java, widget type "item").
- Item quality is a single integer packed with flag bits (src/haven/Item.java,
  `decq`); a second, "inner" quality is parsed out of the tooltip text and takes
  precedence when present (contents of containers, e.g. water in a bucket).
- Inventories are square grids of 1x1-cell slots drawn from a 32x32 px cell
  texture at a 31 px pitch; items occupy rectangular footprints in whole cells
  (src/haven/Inventory.java, `invsq`/`invSqSize`; src/haven/Item.java,
  `coord()`/`size()`).
- Equipment is a fixed array of 16 single-slot inventories (src/haven/Equipory.java,
  `ecoords`, 16 entries; slots are addressed by index 0..15 in wire messages).
- Stockpiles and construction-site material boxes are not inventories at all:
  they are a single-widget resource counter (src/haven/ISBox.java, widget type
  "isbox", three ints rendered as "rem/av/bi").
- The quality multiplier the client uses to display food FEPs and curiosity LP
  is `sqrt(q/10)` (src/haven/Item.java, `qmult`), which matches the published
  quality system (Ring of Brodgar wiki, "Quality": `QM = sqrt(Q/10)`, default
  Q10, softcapping by skills via geometric means, tool weight 1/4, station and
  fuel averaging for ovens/kilns/smelters/finery forges).
- There is no money, no item durability/wear field, and no client-side weight
  readout anywhere in the legacy client (verified by exhaustive grep); the
  legacy economy is barter, and weight is purely server-side.

Sections below give the mechanism, the exact client symbols and message names
that define the wire behavior, and (at the end) a concrete data-model proposal
for a Rust server plus a list of everything that could not be determined from
available sources.

## Widget-level model: how the legacy client sees items

The client is a remote-UI: the server creates widgets by type name and the
client routes user input back as wdgmsg("name", args...) tagged with the widget
id (src/haven/UI.java, `wdgmsg`/`uimsg`; src/haven/Widget.java registers
widget types via `addtype`, including "item", "inv", "isbox", "epry", "make",
"give"). Every fact below about "what the server sends/receives" is expressed in
those terms; exact numeric opcodes of the session transport are out of scope
here (see the protocol documentation).

Item-related widget types the server must implement:

| Type string | Client class | Meaning |
| --- | --- | --- |
| `item` | src/haven/Item.java | One item, in an inventory, on a cursor, or as a craft preview |
| `inv` | src/haven/Inventory.java | A square grid of slots (player inventory, chest window, equip slot) |
| `isbox` | src/haven/ISBox.java | Single-resource counter: stockpile or build-site material box |
| `epry` | src/haven/Equipory.java | The 16-slot equipment window of a player |
| `make` | src/haven/Makewindow.java | Crafting window with input/output preview slots |
| `give` | src/haven/GiveButton.java | Button used to hand items to another character |

## Item identity and dynamic state

### Identity: resource name plus dynamic attributes

An item's identity is its client resource ("gfx/invobjs/..." and friends),
exposed as `Item.GetResName()` (src/haven/Item.java). The resource supplies the
sprite, the default tooltip name (`Resource.tooltip` layer, see `Item.name()`),
and the cell footprint: `Item.size()` divides the sprite size by 30 px, so a
30x30 sprite is 1x1 cells and larger sprites span multiple cells. Everything
else about an item is dynamic state the server layers on top:

- `quality` - the packed quality int (see below).
- `num` - an optional integer rendered as a corner number (`Item.num`, drawn via
  `getqtex(num)` in `Item.draw()`; updated by the "num" uimsg). It is a stack
  count for stackable goods, a required/produced quantity in crafting previews,
  and a growth stage for things like silkworms (see the `stage()` comment in
  src/union/jsbot/JSItem.java). The server decides per item type what `num`
  means.
- `meter` - an optional 0..100 progress value (uimsg "meter") drawn as a vertical
  colored bar (`Item.draw()`); used for curiosity study progress and generic
  per-item progress.
- tooltip - a server-composed string (uimsg "tt", `Item.settip()`). The tooltip
  is not decoration: it carries machine-parsed payload (see next section).
- outline color - uimsg "color" sets `Item.olcol`, a server-driven outline
  (e.g. highlights); the client masks the sprite with it.

### Packed quality int and the "high quality" flag

`Item.decq(int q)` (src/haven/Item.java) defines the wire encoding the client
expects:

- `q < 0` means "no quality" (the item displays no quality number).
- Otherwise the low 24 bits are the quality value and the high byte is a flag
  byte: bit 0 is the `hq` ("high quality") flag, rendered as a "+" after the
  quality in tooltips (`Item.shorttip()`).

So a server item instance needs at minimum `(resource, Option<quality>,
hq_flag)`. Quality values are unbounded positive integers by design (see
"Quality system" below). When the player enables "Show item quality"
(src/haven/OptWnd.java, the `Config.showq` checkbox), the effective quality
number is drawn at the item's bottom-right corner (`Item.draw()`, via
`getqtex(tq)` where `tq` is the inner quality if present, else the outer).

### Inner quality (contents) and tooltip counters

`Item.settip()` parses two patterns out of every tooltip the server sends:

- `patt = "quality (\\d+)"` - the *last* match becomes `q2`, the "inner"
  quality. `Item.get_quality()` returns `q2` when `q2 > 0`, else the outer
  quality. Per the field comments in src/union/jsbot/JSItem.java
  (`innerQuality()`), this is the quality of the *contents* of a container item,
  e.g. the water in a bucket, not the bucket itself.
- `pattVal = "\\(([0-9.]+)/([0-9.]+)"` - the first "a/b" pair becomes
  `count_of_value` / `count_of_maximum` (`Item.count_of_value`,
  `Item.count_of_maximum`), the fill state of a container item, e.g. liters of
  water in a bucket (`currentAmount()`/`maxAmount()` in
  src/union/jsbot/JSItem.java).

Consequence for the server: container items need a tooltip that embeds both a
"(current/max)" counter and a "quality N" phrase for their contents, because
that is the only channel the legacy client reads them from. Sorting commands
also depend on this: `transfer_such_all_ql` ordering uses
`Item.get_quality()`, so inner quality participates in "transfer all, ordered
by quality" (src/haven/Inventory.java, `Item.ItemQualityComparator`).

### FEP and curiosity metadata (client-side tables)

The client augments tooltips with data from two config tables keyed by item
name:

- etc/needed/fep.conf: per-food FEP deltas keyed by attribute names (STR, AGI,
  INT, CON, PER, CHA, DEX, PSY, HHP), displayed scaled by `qmult`
  (`Item.calcFEP()`).
- etc/needed/curio.conf: per-curiosity LP, attention cost (AT), and study time
  (TIME); the client displays `LP = round(baseLP * qmult * expmode)` and
  remaining study time derived from `meter` (`Item.shrtTip()`).

These tables exist client-side in this fork, but the values they display must
match server behavior (the server multiplies FEP/LP by the quality multiplier
when the item is actually consumed/studied). A faithful server should treat
these tables as data, not logic.

## Inventory model (square grid slots)

src/haven/Inventory.java implements the grid. Facts the server must reproduce:

- Creation: widget type "inv" with one argument, the grid size as a Coord
  (`isz`). The server can resize a live inventory with the "sz" uimsg
  (`Inventory.uimsg`).
- Geometry: one cell texture `gfx/hud/invsq` (`invsq`, 32x32 px, `invSqSize`);
  cells are drawn at a 31 px pitch (`invSqSizeSubOne = 31x31`). Item widgets are
  positioned at `cell * 31` px (`Item.coord_x()/coord_y()/coord()` all divide by
  31).
- Footprints: an item occupies `Item.size()` cells (minimum 1x1). The server is
  responsible for occupancy tracking; the client does no collision checking -
  it draws whatever the server places, which is why placement validation
  (does the item fit, are the cells free) is entirely server-side.
- The player's main inventory, chest windows, and each equipment slot are all
  the same "inv" widget with different sizes.
- The trash button (src/haven/Inventory.java, `trash`, textures
  gfx/hud/trashu/trashd/trashh) is client-side confirmation followed by a loop
  of individual item "drop" wdgmsgs (`empty()`); the server never sees a
  "drop all" command, only N single-item drops.

Note: the task term "InventoryWidget" corresponds to this class in the legacy
fork; there is no separate `InventoryWidget` type in this codebase.

## Equipment (Equipory)

src/haven/Equipory.java (widget type "epry") is the player's equipment window
and the authoritative definition of the equipment model:

- Exactly 16 slots (`ecoords.length == 16`), each implemented as a 1x1
  `Inventory` (`epoints`), with the currently equipped item mirrored in
  `equed` (a `List<Item>` of the same length).
- Slot geometry (`ecoords`): two columns, left column at x=0 (slots 0, 2, 4,
  6, 8, 10, 12, 14) and right column at x=244 (slots 1, 3, 5, 7, 9, 11, 13,
  15), rows at y = 0, 31, 62, 93, 124, 155, 186, 217. The slot index used on
  the wire is the array index; even indices are the left column, odd indices
  the right column, interleaved row by row.
- The client does not know what body part each slot means. Slot semantics are
  server-side; the client addresses slots purely by index 0..15 (see also
  src/union/jsbot/JSEquip.java, `quality(int slot)`/`resName(int slot)` with
  the 0..15 clamp).
- Full-state sync: the "set" uimsg carries, for each of the 16 slots in order,
  either `-1` (empty) or `(resid, q)` optionally followed by a tooltip string
  (`Equipory.uimsg`, case "set"). Incremental updates: "setres" (slot, resid,
  q) changes the equipped resource (and quality), "settt" (slot, tooltip)
  replaces the tooltip, "ava" (gob id) sets the avatar preview gob.
- Equipping flows (wdgmsg routing, `Equipory.wdgmsg`):
  - Dropping a held item onto a slot's `Inventory` sends the slot's "drop"
    forwarded as epry wdgmsg ("drop", ep) where ep is the slot index.
  - Clicking an equipped item with take/transfer semantics sends ("take", ep,
    coord), ("transfer", ep, coord), ("iact", ep, coord), ("itemact", ep).
  - Wheel-transfer onto an equip slot is explicitly swallowed client-side
    ("xfer" from an epoint is ignored), so equipping by wheel does not happen.
  - Dropping a held item on the equipment window background sends
    ("drop", -1) - the -1 marks "no specific slot" and the server decides what
    it means (in practice: nothing or unequip-into-hand policies).
- Armor class readout: `Equipory.calcAC()` parses every equipped item's tooltip
  against `patt = "Armor class: (\\d+)/(\\d+)"` and displays "def/abs
  (def+abs)". This proves the legacy server ships armor values inside equipment
  tooltips in exactly that format: first group = defense, second group =
  absorption (the community calls the pair armor class). The server must
  compose this tooltip text when gear is equipped, and must recompute the real
  defensive effect itself (the label is informational only).

Because the slot-to-body-part mapping is not in the client, a reimplementation
must either observe a legacy server (equip each gear type, watch which index
changes in the "set" payload) or pick a canonical mapping and keep it constant.
Both paths are listed under Open questions.

## Containers

Containers are ordinary gobs in the world whose interaction (FlowerMenu
option, e.g. "Open", src/haven/FlowerMenu.java) makes the server open a
`Window` (src/haven/Window.java) containing an "inv" widget. The client has no
container concept of its own: chest vs. cupboard vs. cart is just different
windows/grids the server creates, plus the gob itself. Everything in the
inventory section applies. Additional container facts:

- A Window is itself a drop target: dropping a held item on a window's empty
  area sends the window's wdgmsg ("drop", cc) when the window was created with
  drop-target behavior (src/haven/Window.java, `drop()`; the `dt` flag, which
  is one of the window's server-sent creation arguments).
- Small container items carried *inside* the inventory (buckets, waterskins)
  are not "inv" widgets at all: their contents are modeled with the tooltip
  counters and inner quality described above (src/haven/Item.java `pattVal`,
  `patt`; src/union/jsbot/JSItem.java `currentAmount()`/`maxAmount()` -
  "10 liters in a bucket").
- Water and "wet" item states have no client representation in this fork
  (no attribute, flag, or uimsg). Wetness, if it existed server-side in legacy
  (e.g. doused fires, soggy items), is invisible to this client.

## Stockpiles and construction material boxes (ISBox)

src/haven/ISBox.java (widget type "isbox") is the counter widget for piles of a
single resource and for the material boxes of a construction project:

- Creation args: `(resource name string, rem int, av int, bi int)`; the label
  is rendered "%d/%d/%d" (`setlabel(rem, av, bi)`) and the resource sprite is
  drawn from the named resource (`resName`, `toolTip`, `boxValues` fields).
- Live update: "chnum" uimsg with three new ints (`ISBox.uimsg`).
- Player actions (wdgmsgs the server receives):
  - "click" - left click: take one unit out (into the cursor).
  - "xfer" - shift+left click: move one unit into the player's inventory
    (`takeOne()`/`transferOne()` helpers).
  - "xfer2" (dir, modflags) - mouse wheel: move one unit in either direction
    with modifier-dependent behavior (`ISBox.mousewheel`).
  - "drop" - put the held item into the pile (`ISBox.drop`).
  - "iact" - right-click item interaction (`ISBox.iteminteract`).
- The bots in src/union/JSBot.java (`getBuildValues`, `getBuildToolTip`,
  `takeBuildItem`, `transferBuildItem`) use ISBox widgets as the material
  requirement boxes of build windows and parse `boxValues` ("a/b/c") to check
  delivery state. This confirms the widget doubles as the construction-site
  material tracker.

The *meaning* of the three numbers is not encoded in the client (the field
names rem/av/bi are the bot author's guess). In build windows they clearly
track required-vs-delivered material; for free-standing stockpiles the pair
(count, and possibly total capacity or quality) is unconfirmed. The server must
define them consistently; see Open questions.

## Item transfers and drag semantics

All item movement is initiated by the client as wdgmsgs and completed by the
server pushing new widget state. There is no client-side optimistic movement:
the item only appears to move when the server updates the widgets.

### Taking an item to the cursor

`Item.mousedown` (src/haven/Item.java) sends:

- left click: "take" (c) - item goes on the cursor. A created-on-cursor item
  widget is sent with the drag flag and a drag offset Coord; the client then
  `ui.grabmouse()`s it and it follows the pointer (`Item` constructor,
  `isDragging`, `doff`). The player's avatar switches to the carrying pose -
  the server controls this via the avatar layer "arm/carrying"
  (src/haven/Avatar.java, `isCarrying()`).
- shift+left: "transfer" (c) - quick-move into the "other" open inventory.
- ctrl+left: "drop" (c) - drop under the cursor target.
- right click: "iact" (c) - item context menu (FlowerMenu sent by server), or,
  with modifiers, the mass operations below.

While an item is on the cursor, releasing left click over a widget walks the
widget tree and offers the drop to `DTarget` implementations
(`Item.dropon()`, src/haven/DTarget.java: `drop(cc, ul)` /
`iteminteract(cc, ul)`; the `DTarget2` variant in src/haven/DTarget2.java also
passes the item). `DropTarget.dropthing(cc, thing)` (src/haven/DropTarget.java,
dispatched in src/haven/UI.java `dropthing`) is a *different* mechanism: it
carries a client Resource (an action-menu button being dragged, see
src/haven/MenuGrid.java) and is not part of item movement.

### The standard target wdgmsgs

- Inventory: drop - ("drop", cell) where cell = (ul + (15,15)) / 32, i.e. the
  intended grid cell (src/haven/Inventory.java `drop`). The server should
  honor the requested cell when free, otherwise fall back to a nearest-free
  policy or reject.
- Inventory wheel: ("xfer", +-1, modflags) (src/haven/Inventory.java
  `mousewheel`). The client does not say which item to move - the selection
  policy is server-side (see Open questions).
- Ground (MapView): ("drop", modflags) (src/haven/MapView.java `drop`) - the
  held item becomes a gob at the player's position; ("itemact", cc0, mc,
  modflags [, gob id, gob pos]) (src/haven/MapView.java `iteminteract`) -
  right-click interaction of the held item with the world or a gob (use item
  on thing).
- Item-to-item: ("itemact", modflags) (src/haven/Item.java `iteminteract`) -
  using the held item on the clicked item (combining, pouring, etc.).
- ISBox and Equipory messages are listed in their sections above.
- Mass ops (client expands them, the server receives plain per-item messages):
  "transfer_such_all" (resource name) transfers every matching item;
  "drop_such_all" drops every matching item; "transfer_such_all_ql" /
  "transfer_such_all_qldesc" sort matching items by `get_quality()`
  in quality order first (src/haven/Inventory.java `wdgmsg`,
  src/haven/Item.java `ItemQualityComparator`). Note the naming inversion,
  visible in the comparator: `_ql` (default comparator, `desc = -1`) sorts
  best-quality first; `_qldesc` (comparator built with `true`) sorts
  worst-quality first. Each expands to individual "transfer"/"drop" wdgmsgs,
  so the server sees a burst of ordinary messages and must apply its own rate
  limits and atomicity rules.

### Giving items to other players

The give flow runs through the combat view (src/haven/Fightview.java) and
src/haven/GiveButton.java:

- Every combat `Relation` carries a `GiveButton` ("give" widget type). The
  server sets its 2-bit state via the Fightview uimsgs "new"/"upd" (argument
  3, `rel.give(state)`); the client renders state bit 0 and bit 1 as
  left/right outline overlays (`GiveButton.draw()`, state & 1 / state & 2),
  i.e. one bit per side of the exchange.
- Clicking the button sends Fightview wdgmsg ("give", gob_id, mouse button)
  (`GiveButton.mousedown` sends ("click", button); Fightview translates it).
- The server then performs the handover and updates the state bits.

The precise handshake (whether the held item is attached by dragging it onto
the button or by clicking with an item on the cursor, and what each state bit
certifies) is not fully determined by the client code; it is listed in Open
questions. What is certain: the only give primitive this client exposes is the
("give", gob_id, button) wdgmsg on the Fightview widget.

### Crafting window transfers

src/haven/Makewindow.java ("make") shows predicted inputs and outputs as 1x1
inventories containing `Item` previews with q = -1 and `num` = quantity. The
player's only outputs are the "make" (0) and "make" (1) wdgmsgs (Craft / Craft
All buttons and the Enter key). Ingredient insertion happens by dropping real
items from the inventory onto the makewindow's input slots (the "inv" drop
path above). See ../crafting/crafting-and-building.md for the crafting rules.

## Carrying weight

There is no weight value, weight meter, or carry-capacity readout anywhere in
the legacy client (no such symbol exists; verified by search over src/haven).
Conclusions for the server:

- Item weights, total carried weight, and the capacity limit are pure
  server-side state. The legacy client cannot display them, so a faithful
  server enforces them silently (or via error messages the server chooses to
  send).
- Character attributes arrive as server-pushed pairs (name, base, computed)
  via `Glob.cattr` (src/haven/Glob.java `cattr(Message)`, `CAttr` with base and
  comp values, displayed in src/haven/CharWnd.java). Strength (STR) exists in
  this attribute stream (see also etc/needed/fep.conf attribute keys).
- Per the Ring of Brodgar wiki, the relevant attribute for carrying capacity
  is Strength (more Strength, more carry). The wiki documents no exact legacy
  capacity formula, and this document will not invent one: treat capacity as
  `f(STR)` with unknown coefficients and put the exact rule in Open questions.
- What the wiki does state qualitatively: Strength also multiplies melee
  damage (see the damage formula in "Quality system" below), so the two uses
  of STR must be kept apart in the server model.

Practical recommendation: store a per-item-type weight (grams) in server data,
sum it across inventory, equipment, cursor, and nested containers, and compare
against a capacity derived from STR. The numbers themselves are an Open
question.

## The quality system

Quality (QL) is the central scalar of legacy H&H. The Ring of Brodgar wiki
"Quality" page (which quotes the game author, loftar) is the best published
source; the client corroborates the parts it can see.

### The quality multiplier

- Every item and many world objects have a quality Q in 1..infinity; natural
  objects default to Q10 (e.g. all trees).
- Where quality matters it usually acts through the quality multiplier
  `QM = sqrt(Q / 10)`: Q10 = x1, Q40 = x2, Q90 = x3, Q160 = x4, Q250 = x5,
  Q640 = x8, Q1000 = x10 (rule of thumb from the wiki).
- The legacy client independently computes exactly this for display:
  `qmult = Math.sqrt((double) q / 10)` in the `Item` constructor
  (src/haven/Item.java). The server should use the same multiplier for food
  FEPs and curiosity LP so that displayed and applied values agree.

### Determination at craft time

Published crafting-quality rules (RoB wiki "Quality", "Notes from loftar"):

1. Inputs: for each ingredient type, average the qualities of the individual
   items arithmetically; then combine the per-type averages into one input
   quality with an arithmetic weighted average, the weights expressing each
   ingredient's importance (e.g. the iron dominates an iron plow).
2. Tools: if a tool is involved, blend the tool quality at weight 1/4:
   `q = (3 * q_input + q_tool) / 4`.
3. Skills/attributes (softcap): if the recipe involves n skill or attribute
   values s1..sn, their "skill quality" is the geometric mean
   `q_skill = (s1 * s2 * ... * sn) ^ (1/n)`. If `q_skill < q_input` then
   `q_result = (q_skill + q_input) / 2`, otherwise `q_result = q_input`.
   Fractional results round down to the nearest integer.
4. Multiple attributes combine geometrically (e.g. gathering Rock Crystal
   caps at the geometric mean of the involved stats).

Worked example from the wiki: a Q40 bone and a Q10 bough average to Q25 for a
bone saw, but with Survival 15 the softcap yields (25 + 15) / 2 = Q20.

Known skill/stat pairings: sewing and leatherworking recipes use Sewing and
Dexterity; smithing recipes commonly combine Smithing and Strength.

### Stations and fuel

For ovens, kilns, smelters and finery forges the result quality is
`(2 * q_item + q_station + q_fuel) / 4`, where `q_fuel` is the arithmetic
average of the fuel items loaded. For buildable objects (houses, fences), the
per-type averages are combined arithmetically (usually weighted toward the
"obviously" important materials) with no tool or skill term documented.

The client exposes one hook for station influence: the crafting window and its
previews are server-driven (src/haven/Makewindow.java), so the server can apply
any station rule and simply tell the client the result.

### Effects of quality

- Food: FEPs are multiplied by QM; the restored energy is not. The client's
  FEP tooltip display scales by `qmult` (src/haven/Item.java `calcFEP`),
  matching this.
- Curiosities: LP gain is multiplied by QM (client shows
  `round(baseLP * qmult * expmode)`, src/haven/Item.java `shrtTip`); attention
  cost and base study time are not quality-dependent.
- Weapons: two published formulas disagree, and this is a genuine legacy
  question:
  - RoB "Legacy:Combat Actions" gives the legacy melee damage formula as
    `damage = basedamage * ql * str / 10` - quality enters *linearly*. Caution:
    the page's own worked example (Q65 Soldier's Sword, base damage 400,
    STR 319, result 1517) does not reproduce arithmetically from the stated
    formula, so the exact scaling constant is unverified.
  - RoB "Quality" (current world) gives damage scaling with
    `sqrt(q/10)` (a Q10 Bronze Sword at 90 damage deals 720 at Q640).
  A legacy-faithful server should prefer the Legacy page formula but flag it
  for verification (Open questions). See ../combat/combat-system.md for how
  this feeds the fight model.
- Armor: defense and absorption scale with QM (Q10 Boar Tusk Helmet 1/7, Q160
  4/28 - both components scale). On the wire this appears in the equipment
  tooltips the server composes ("Armor class: X/Y", parsed by
  src/haven/Equipory.java `calcAC`).
- Tools and containers: quality affects whatever the server applies it to
  (crafting caps, harvest yields); the wiki notes some items (e.g. buckets)
  have no quality effect at all.

### Quality of gathered resources

Gathered resource quality (wood, stone, hides, crops) is decided at harvest
time by the relevant skill/attribute of the harvester (the same softcap
mechanism caps it). The exact stat-to-resource mapping is not published in the
sources available here and is listed in Open questions. Natural growth defaults
to Q10 absent player influence.

## Stacking and grouping

- The only stack mechanism on the wire is the `num` field ("num" uimsg and the
  optional trailing int of the item creation tuple). `num >= 0` renders the
  corner counter; `num = -1` renders nothing.
- Which resource types stack, and their stack limits, are server policy. The
  client supports any value the server sends; bots read it as both a count and
  a growth stage depending on item (src/union/jsbot/JSItem.java `stage()`
  comment vs. Makewindow quantities).
- Grouping by quality is a client-side convenience only
  (`transfer_such_all_ql` sorts by `get_quality()` before issuing transfers);
  the server never sees the sort, only the resulting message order.
- Weight-based "grips"/bundles, if the legacy server had them, have no client
  support in this fork beyond the generic tooltip.

## Money

There is no currency in the legacy client: no coin resources, no money meter,
no trade-cash widget (exhaustive grep over src/haven for coin/money/silver
returns nothing). Legacy H&H is a barter economy; player-to-player exchange
runs through the give flow ("give" wdgmsg, see above) and, for commerce, via
mutual container access (chest permissions are a claim/law concern owned by
other documentation). A server implementation needs no wallet data model for
legacy fidelity.

## Decay, wear, and repair

No durability, wear, condition, decay, or repair concept exists in the legacy
client: items have no condition field, no condition uimsg, and tooltips carry
quality/armor-class/content but no condition phrase (verified by search over
src/haven for decay/wear/condition/repair). The generic `meter` field is a
progress indicator (e.g. curiosity study), not damage.

Conclusions: a legacy-faithful server should implement items as
non-degrading. Perishability that legacy did have (food spoilage or similar)
would have to ride the tooltip/meter channels; there is no positive evidence
of it, so it is an Open question rather than a feature.

## Server implementation notes

### Data model

Recommended Rust model (immutable base + mutable state), mirroring what the
wire actually needs:

```text
ItemBase (per resource, static data)
  resource_name: String          // "gfx/invobjs/..." as in GetResName()
  footprint: (u8, u8)            // cells, from sprite size / 30
  weight_g: u32                  // server data, client-invisible
  stackable: Option<u32>         // max num if the type stacks
  is_container: Option<ContainerSpec>  // capacity liters etc.
  quality_effect: QualityClass   // weapon / armor / food / curio / none

ItemState (per instance, mutable)
  quality: Option<NonZeroU32>    // the 24-bit value; None = no quality (-1)
  hq: bool                       // high-quality flag byte bit 0
  num: i32                       // -1 = unset
  meter: u8                      // 0..=100
  tooltip: String                // server-composed, payload-bearing
  contents: Option<(f64, f64)>   // count_of_value / count_of_maximum
  inner_quality: Option<u32>     // q2, echoed as "quality N" in tooltip
```

Quality should be stored as the plain value plus the hq flag; the packed wire
int is `(if hq { 0x01000000 } else { 0 } | value)` and `-1` for no quality
(src/haven/Item.java `decq`). Keep tooltips a derived, serialized artifact:
because the client parses payloads out of them, any state change that affects
quality/contents/armor class must re-send "tt" (and "settt" for equipped
items).

### Slot and container registry

- Player inventory: `Vec<Option<ItemId>>` plus `width`/`height`, or an
  occupancy map keyed by cell; enforce footprint placement on drop requests
  (the client requests a specific cell).
- Equipment: `[Option<ItemId>; 16]`, index order identical to `ecoords`
  (row-major, left/right interleaved). Broadcast the full "set" tuple on any
  change (the legacy client rebuilds all 16 slots from one "set" message) and
  "setres"/"settt" for incremental changes.
- Containers: each container gob owns an inventory exactly like the player's;
  opening/closing is a widget lifetime concern (create/destroy the window
  widget, not the item state).
- Stockpiles: `struct Stockpile { resource: String, rem: i32, av: i32, bi:
  i32 }` with "chnum" pushes; define the three counters once and document them
  in code (see Open questions for the legacy meaning).
- Cursor: at most one item per player can be on the cursor; model it as an
  optional item slot on the player entity, because take/drop/give all move
  through it.

### Transfer command handling

Every client message from the tables above must be validated server-side:
ownership (does this widget id belong to the sender's UI), range (is the
player near the container gob), capacity (grid occupancy, container liters,
carry weight), and slot compatibility (equipment slot accepts the resource).
Client-initiated bursts from mass operations ("transfer_such_all" expansion)
should be handled idempotently and rate-limited. Never trust the client's
notion of the item: the item id in the wdgmsg refers to a widget id the server
created; resolve it back to authoritative state.

### Weight computation

Sum `ItemBase::weight_g * max(1, num)` over inventory + equipment + cursor +
open/nested container contents as the server requires; compare with the
capacity function of STR. Since the client shows nothing, the server chooses
where to surface rejections (system messages or silent refusal of the transfer
wdgmsg).

### Quality propagation

- Crafting: implement the three-stage pipeline (inputs average -> tool blend
  at 1/4 -> skill softcap via geometric mean) as a pure function so both
  actual crafting and result prediction (Makewindow "pop" previews) use it.
  Details in ../crafting/crafting-and-building.md.
- Combat: weapon QL multiplies damage (legacy formula
  `basedamage * ql * str / 10` pending verification) and armor tooltips carry
  QM-scaled defense/absorption. Details in ../combat/combat-system.md.
- Food and curiosities: multiply FEPs and LP by `sqrt(q/10)`; keep energy and
  attention quality-independent, matching the client display math
  (src/haven/Item.java `qmult`, `calcFEP`, `shrtTip`).
- Harvesting: assign gathered-resource quality at the moment of harvest, then
  never change it (qualities are immutable once created except where the
  server explicitly transforms an item via "chres").

### Resource changes on live items

The "chres" uimsg (resid, q) lets the server morph an existing item widget
into another resource with a new quality (src/haven/Item.java `chres`) - this
is how the same slot can change from, e.g., full to empty variants without a
destroy/create cycle. Use it for item transformations; do not silently mutate
resource identity server-side without pushing it.

## Open questions

1. Equipment slot semantics: which of the 16 indices is head, torso, hands,
   etc. The client (src/haven/Equipory.java) treats slots as opaque 0..15.
   How to determine: run the legacy client against a legacy server (or packet
   logs from one), equip each gear type, and record which index flips in the
   "set" payload.
2. Legacy melee damage: linear `basedamage * ql * str / 10` (RoB
   "Legacy:Combat Actions") vs. `sqrt(q/10)` scaling (RoB "Quality", current
   world). Note additionally that the Legacy page's worked example does not
   reproduce arithmetically from its own formula, so even the constant `10`
   is in doubt. How to determine: legacy server packet capture plus controlled
   hits at known QL/STR, or a dev statement in the H&H forum archive.
3. ISBox counter semantics: what rem/av/bi mean for stockpiles and build
   boxes (the client only formats them; the names come from bot comments).
   How to determine: observe a legacy server filling/draining a stockpile and
   a construction site while recording "chnum"/creation values.
4. Main inventory grid dimensions: the size is server-sent ("inv" creation
   arg); the bot comment in src/haven/ISBox.java mentions "56 - maximum
   inventory that u can have", suggesting a 56-slot legacy main inventory.
   How to determine: read the "inv" creation args of a legacy session.
5. Carry capacity: the exact legacy capacity formula and per-item weights.
   The client has no weight display at all. How to determine: legacy server
   observation (find the load at which "take" starts failing) or leaked
   server data; until then keep capacity as a configurable STR function.
6. Wheel-transfer ("xfer" on "inv") selection policy: the client sends only a
   direction and modifier flags, not which item to move. How to determine:
   legacy server behavior tests (hover which cell, watch what moves).
7. Give handshake details: whether the item is attached by dragging onto the
   GiveButton or by clicking with an item on the cursor, and the meaning of
   the two state bits. How to determine: legacy session with two clients,
   capture Fightview "give"/"new"/"upd" traffic.
8. Stacking policy: which legacy resource types stack and their caps.
   How to determine: legacy server observation of `num` values per resource.
9. Item decay/food spoilage: absent from the client; confirm whether legacy
   had any (and if so, which channel - meter vs. tooltip - carried it).
10. Harvest quality mapping: which skill/attribute caps the quality of each
    gathered resource class. How to determine: RoB per-resource pages and
    legacy testing; the softcap mechanism itself is documented (see "Quality
    system").
11. Inner-quality scope: the full list of container items that emit "quality
    N" tooltips (water containers are confirmed by bot comments) and whether
    inner quality affects derived effects (e.g. watering plants).
12. Exact legacy per-item weights and armor-class numbers per gear type at
    Q10: wiki tables are current-world and qualitative for legacy; the
    server's "Armor class: X/Y" tooltip composition needs a legacy source
    table before it can be data-driven.
