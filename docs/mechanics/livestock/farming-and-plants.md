# Farming and Plants (Legacy Haven & Hearth)

> **Sources:** src/haven/resutil/GrowingPlant.java, src/haven/resutil/CommonPlant.java, src/haven/resutil/GaussianPlant.java, src/haven/resutil/BollSprite.java, src/haven/Session.java, src/haven/OCache.java, src/haven/Gob.java, src/haven/MapView.java, src/haven/OptWnd.java, src/haven/MenuGrid.java, src/haven/Config.java, src/haven/Glob.java, src/haven/Astronomy.java, src/haven/MCache.java, src/union/jsbot/JSHaven.java, etc/needed/fep.conf; Ring of Brodgar wiki (legacy server): Legacy:Farming, Legacy:Wild_Windsown_Weed, Legacy:Foraging, Legacy:Finding_high_quality_water,_clay,_and_soil, Legacy:Bee_Hive, Legacy:Apple_Tree

## Summary

Farming in legacy Haven & Hearth is a server-side simulation of per-tile crop state,
observed by the client purely through "planted gob" rendering. A crop is a gob whose
resource carries a one-byte stage value inside the sprite dynamic data (sdt); the
client's `GrowingPlant.Factory.create` (`src/haven/resutil/GrowingPlant.java`) reads
that byte with `sdt.uint8()` and selects the texture `strands[m]` for stage `m`. The
server, not the client, owns all growth: it decides when a stage advances, pushes an
object delta (`OD_RES` in `src/haven/Session.java`) that re-creates the sprite with
the new stage, and computes yields at harvest time. There is no client-side growth
prediction; if the server does not send a stage update, the crop does not visibly grow.

The authoritative gameplay rules recovered from the legacy-era Ring of Brodgar wiki
are: crops are planted into plowed soil only (trees are the exception), each crop has
a fixed number of growth stages with real-time durations on the order of hours to
days, harvest yield and stage choice interact (harvesting early gives fewer or
different products), and quality propagates from seed quality through a random
[-5, +5] roll influenced by soil quality and the farmer's skill. This document
specifies the planting flow, the growth/stage model, the quality model, soil
semantics, the crop list, tree growth, foraging, and the concrete tick/delta model a
Rust server must implement to reproduce legacy behavior.

## Client-side rendering contract (what the server must send)

The client is a dumb renderer for plants. The contract a server must honor:

- A planted crop is a gob (`src/haven/Gob.java`) created by an `OD_RES` object-delta
  entry (`src/haven/Session.java`, `getobjdata`): a `uint16` resource id; if the
  0x8000 bit is set, one `uint8` length prefix follows and that many bytes of sprite
  dynamic data (sdt) are appended. For growing plants the first sdt byte is the
  growth stage.
- Re-sending `OD_RES` with a new sdt stage re-creates the drawable:
  `OCache.cres` (`src/haven/OCache.java`) installs a fresh `ResDrawable` whenever the
  resource changes or the new sdt is non-empty, which rebuilds the sprite. A stage
  update always carries a non-empty sdt, so this is the growth update path; note that
  a bare `OD_RES` with an empty sdt does not by itself re-create the sprite unless
  the resource id changed as well.
- `GrowingPlant.Factory.create` (`src/haven/resutil/GrowingPlant.java`) reads the
  stage: `int m = sdt.uint8();` then places `num` strand sprites, each chosen as
  `strands[m][rnd.nextInt(strands[m].length)]` using the deterministic per-gob random
  from `owner.mkrandoom()`. The factory constructor maps resource image layers to
  `[stages][variants]` (layout depends on the `rev` flag). Consequences:
  - Stage is a small unsigned byte (0..stages-1). Stage 0 is the just-planted look.
  - A crop gob at a given stage always looks the same per client session because the
    strand variants are seeded by gob id (`mkrandoom`), but variant choice is not
    game state; the server never needs to know which variant a client picked.
- `CommonPlant.Factory.create` (`src/haven/resutil/CommonPlant.java`) and
  `GaussianPlant.Factory.create` (`src/haven/resutil/GaussianPlant.java`) do not read
  a stage byte in this client fork: they place `num` strands chosen randomly from all
  image layers. These classes are used for static vegetation (forageable herbs,
  bushes) rather than staged crops. The name `GaussianPlant` historically refers to
  plants whose useful harvest window is spread out; in this fork its `create` is
  structurally identical to `CommonPlant`, so any "gaussian" timing semantics live
  entirely server-side and are invisible to the wire format.
- `src/haven/resutil/BollSprite.java` is the base class for sprites that draw
  individual round "bolls" (fruit bodies) as screen-space parts: `add(Boll)`,
  `remove(Boll)`, per-frame `tick(int dt)`/`tick2(int dt)`, and a `setup` that offsets
  each boll by its (x, y, z). Plant resources that show discrete fruits (e.g.
  pumpkins on a vine, bunches of berries) would be built on this; the boll positions
  come from the sprite factory, not from server state.
- The `Config.simple_plants` option (`src/haven/Config.java`) makes
  `GrowingPlant.Factory.create` render a single strand at a fixed offset instead of
  `num` randomized strands. It is a pure client performance toggle; it must not
  change server behavior.

Resource-name conventions the client exposes (used by hide-object and highlight
features in `src/haven/OptWnd.java` and `src/haven/MapView.java`):

- Planted crops live under `gfx/terobjs/plants/...`.
- Trees live under `gfx/terobjs/trees/...` (and logs under `gfx/terobjs/trees/log`,
  referenced by `src/union/APXUtils.java`).
- Forageable herbs live under `gfx/terobjs/herbs/...`; `MapView.drawcurioses` derives
  the inventory icon by string-replacing `terobjs` with `invobjs` (with a special case
  for `mussel` under `terobjs/herbs`), proving herbs are gobs with matching item
  resources.
- Wild bushes are tileset-layer objects under `gfx/tiles/wald` and thickets under
  `gfx/tiles/dwald`; stones under `gfx/terobjs/bumlings`; ridges under
  `gfx/terobjs/ridges/grass/` and `gfx/terobjs/ridges/mountain/`; blood decals under
  `gfx/terobjs/blood`.
- The paginae (action menu) includes `paginae/add/plants` and `paginae/add/animal`
  (see `res/raw/res/paginae/add/`), which the server pushes via `Glob.paginae`
  to give players the farming/animal action pages.

## Wild plants vs planted crops

Two distinct plant systems exist and must be modeled separately:

1. Forageables (wild herbs, mushrooms, roots, flowers, mussels) are static gobs
   spawned by the world generator on appropriate terrain. They are visible to a
   player only after a Perception x Exploration check (see the Foraging section). The
   herb gob carries a quality derived from the soil tile it stands on. Harvesting is
   a flower-menu pick (`src/haven/FlowerMenu.java`; options are server-defined
   strings, the client answers with the `cl` widget message carrying the chosen petal
   number).
2. Planted crops are player-created gobs on plowed soil. They are always fully
   visible (no Per*Exp check), they have staged growth, and they yield seeds plus
   byproducts whose quality follows the planted-seed quality.

The legacy world had no wild wheat/flax fields: the "wild crop patches" of world 2
were replaced by Wild Windsown Weed (WWW), a forageable that dries into a random
seed. A server that wants the legacy progression must therefore gate early crop
seeds behind foraging (WWW), not behind wild crop fields.

## Planting flow (seed item -> tile -> planted gob)

1. Prerequisites on the legacy server: the Farming skill (400 LP, requires Plant
   Lore per Legacy:Farming). Planting itself is not affected by skill level, but the
   skill gate is absolute; grape seeds additionally require Winemaking.
2. Soil preparation: the action Adventure > Landscaping > Plow Field plows a tile
   into furrowed soil. The action is invoked from the action menu
   (`src/haven/MenuGrid.java`: `use(Resource)` sends the widget message `act` with the
   button's `ad` arguments to the server). Plowing by hand costs more stamina than
   using the Plow object. Furrowed soil is not required for planting trees.
3. Planting interaction: the player picks up a seed item (it becomes the cursor
   object) and right-clicks a plowed tile. The client sends the `itemact` widget
   message on MapView with the tile coordinate (`src/haven/MapView.java`,
   `wdgmsg("itemact", ..., mc, mod)`). Holding Shift repeats the action with the next
   seed in inventory. If the seeds are inside a seedbag, the bag itself can be used
   on the tile, planting from the bag (seedbags are ordinary container windows; see
   the bot API comment "convenient for Seedbag" in `src/union/jsbot/JSHaven.java`,
   `jGetWindows`).
4. Server state change: on success the server consumes one seed from the cursor
   stack, marks the tile as planted, and creates a crop gob at the tile center with
   resource `gfx/terobjs/plants/<crop>` and sdt stage 0. One tile holds one crop gob.
5. Growth then proceeds server-side (see the tick model below); the client only
   receives stage updates.

Legacy quirk to reproduce: plowed fields revert to their original tile type over
time if left unplanted (Legacy:Farming). A server needs a decay timer on plowed-tile
state, and planting must clear it.

## Growth model: stages, timings, and the harvest window

The wiki-documented legacy crop table (times are real-time assuming beehive
coverage; each crop's "additional product" column shows the alternative harvest
targets):

| Crop | Planted with | Byproducts / alternatives | Stage-harvest timings |
| --- | --- | --- | --- |
| Carrot | Carrot, Carrot Seeds | Carrots (yield depends on stage) | 4 h / 8 h / 12 h |
| Beetroot | Beetroot | Beetroot Leaves | 18 h (leaf harvest) |
| Flax | Flax Seeds | Plant Fibres | 1 d / 1.5 d / 2 d |
| Hemp | Hemp Seeds | Plant Fibres, Fresh Hemp Bud (third stage) | 33 h / 34 h |
| Hops | Hops Cones | - | 12 h |
| Peas | Peapod | - | 24-28 h |
| Peppercorn | Peppercorn | - | 1 d |
| Poppies | Poppy Seeds | Poppy Flower | - |
| Pumpkin | Pumpkin Seeds | Giant Pumpkin -> Pumpkin Seeds, Pumpkin Flesh | 5-7 days |
| Tea | Tea Seeds | Fresh Tea Leaves | - |
| Tobacco | Tobacco Seeds | Fresh Tobacco Leaf | 22 h |
| Wheat | Wheat Seeds | Straw | 1 d / 1.5 d / 2 d |
| Yellow Onion | Yellow Onion | - | - |

Structural rules:

- The wiki's stage numbers are 1-based for humans; the wire byte (`sdt.uint8()` in
  `GrowingPlant.Factory.create`) is the zero-based index into the stage texture
  array. Stage 1 (wire byte 0) is the initial planted stage; nothing can be done to
  the crop at that stage.
- Several crops expose two or three harvest points: harvesting early yields the
  byproduct (e.g. wheat straw, flax plant fibres), harvesting late yields the main
  food/seed product. This is the "optimal harvest time window" concept: the server
  must expose the same crop resource at multiple stages and offer different harvest
  outcomes per stage. The client cannot tell the player which outcome a click will
  produce; the flower menu option set for the gob (server-defined) is the only
  affordance.
- Carrots are the documented 5-stage case: stage 3 always yields 1 carrot; stage 4
  yields 1-3 carrot seeds; stage 5 yields 1-3 carrots.
- Pumpkin is a multi-day crop whose final form is a Giant Pumpkin trellis-style
  object; when harvested it converts into Pumpkin Seeds and Pumpkin Flesh
  (`etc/needed/fep.conf` lists "Pumpkin Flesh" with FEPs STR:1 CON:1).

The legacy time base: one in-game day is 8 real hours (Legacy:Wild_Windsown_Weed:
drying on a drying frame takes "2 in game days, or 16 hours of real time"). The
client-side year is 365 in-game days (`src/haven/Astronomy.java`:
`day = (int)(365*yt)`). All crop durations above are therefore anchored to a real
clock; the server should implement stage advance on wall-clock timers, not frame
ticks.

Gaussian growth windows: the class name `GaussianPlant`
(`src/haven/resutil/GaussianPlant.java`) and community lore indicate that some plants
become harvestable within a window whose start is random per plant, rather than at
an exact tick. A faithful server can model this by giving each planted crop a
per-plant random offset (drawn from a zero-mean gaussian with a per-crop standard
deviation in game hours) added to its nominal stage durations. Nothing on the wire
depends on this choice; keep it server-internal and record the per-crop parameters
as tunable data (see Open questions).

## Quality model

Documented legacy rules (Legacy:Farming):

- Final product quality = planted seed quality + random roll in [-5, +5].
- If the soil quality of the tile is lower than the seed quality, the roll is capped
  to [-5, +2]. Soil quality can only hurt, never help beyond the seed's own value.
- If the farmer's Farming skill is lower than the harvested crop's quality, the crop
  quality is softcapped by the farming skill.
- Byproducts (straw, plant fibres) take the quality of the planted seed exactly.
- Selective breeding of seeds: because the roll can exceed the seed quality, replanting
  only the highest-quality seeds each generation raises average quality over time,
  approaching the farmer's skill. A high Nature personal belief accelerates this.
- Planting itself ignores skill; the skill matters only at harvest.

Softcap semantics: a softcap q_c on a value q means the value approaches q_c with
diminishing returns rather than clamping hard; the exact legacy softcap curve is not
recovered (see Open questions). The client only displays quality numbers on items;
the formula is entirely server-side.

## Soil types and soil quality

- Furrowed (plowed) soil: a per-tile state created by Plow Field; required for crop
  planting; reverts over time. It is a tile overlay/state, not a gob; the client
  renders it from the tileset stream (the server sends tile types via the map
  grid machinery in `src/haven/MCache.java`, grids of `cmaps = 100 x 100` tiles,
  `tilesz = 11 x 11` map units per tile).
- Soil quality: each world region has soil quality "sources" with a peak quality at
  a center point, falling off with distance (Legacy:Finding_high_quality_water,_clay,_and_soil).
  Repeated harvesting at a source can decrement its quality by one; the source
  regenerates toward its maximum after a few days of no harvesting. Forageable
  quality directly reflects the soil quality beneath them, which is how players
  prospect soil. All gathered resources are hardcapped by the Survival skill
  (world-7 rule quoted by the wiki).
- Soil quality is tile-state data the server must persist; it gates the crop quality
  roll (see above) and forageable quality.
- Manure: no evidence of a manure or fertilizer item exists in the legacy-era
  sources reviewed for this document (no manure page on the legacy wiki, no manure
  reference in the client resources). The legacy soil system is plowed-tilth plus
  natural soil quality gradients. Treat any manure-based fertilization as a
  current-world feature; do not implement it for legacy parity (see Open questions).

## Beehives accelerate crops

Beehives (`gfx/terobjs/bhive`, built via Bee Keeping) speed up crop growth in a
radius of 13 tiles (Legacy:Bee_Hive). The client corroborates the geometry:
`src/haven/MapView.java` draws a highlight circle of radius 150 map units around
`gfx/terobjs/bhive` gobs (`radiuses.put("gfx/terobjs/bhive", 150)`), and with
`src/haven/MCache.java` defining a tile as 11x11 map units, 150 units is
approximately 13.6 tiles - matching the wiki's 13-tile radius. A server should mark
tiles within that radius as "pollinated" and scale crop stage timers by the speedup
factor (exact factor unknown; see Open questions). A hive holds at most 1.0 L honey
and 5 wax.

## Trees and fruit-bearing trees

- Trees are planted by players with the Farming skill; furrowed soil is not needed
  (Legacy:Farming). Tree gobs live under `gfx/terobjs/trees/...`.
- The documented apple tree life cycle (Legacy:Apple_Tree) uses seven stages:
  1 (Sapling, no actions), 2 (choppable), 3-5 (undocumented intermediate growth),
  6 (fully grown: yields Apples, Bark, Branches), 7 (Stump; "Remove" yields
  4 Blocks of Wood). The seed is the Apple Core left over from eating an apple.
  This is the same stage-attribute pattern as crops: one gob resource, stage byte
  advancing 0..6, with per-stage flower-menu actions (Harvest, Chop, Remove).
- Tree stage rendering reuses the plant machinery (stage-indexed textures;
  `GrowingPlant`/`GaussianPlant`-style factories, or resource-specific sprite code
  built on `src/haven/resutil/BollSprite.java` for fruit bodies). The exact sprite
  factory per tree resource is resource data, not wire data.
- Fruit trees produce limited seasonal amounts of fruit; the legacy wiki does not
  document whether harvest windows are tied to the season system. The client has no
  season logic at all: it only knows the year fraction `yt` pushed through
  `GMSG_ASTRO` in `src/haven/Glob.java` and derives a day number from it
  (`src/haven/Astronomy.java`: `day = (int)(365*yt)`), so season boundaries and any
  season-gated fruiting are purely server policy. Season-gated fruiting is plausible
  but unsourced; see Open questions.

## Foraging: herbs, flowers, and Wild Windsown Weed

- Visibility gating: a forageable of base level B becomes visible to a character
  with Per*Exp = Perception x Exploration starting at B/2 ("First Seen") and always
  visible at 2B ("All Seen"), with 100 documented gradations between. Quoting the
  wiki's Loftar quote: "Rustroot's base level is 1000. Which means you'll start
  seeing them at 500 and see all of them at 2000." Visibility is re-rolled per
  character each time the gob enters sight range; the item has no per-spawn
  difficulty.
- Terrain binding: each forageable spawns on specific terrains (Blueberries:
  forest/heath; Dandelion: forest/heath; Stinging Nettle: forest; Chantrelles:
  forest; WWW: swamp/forest/heath/beach/thicket; Thorny Thistle: mudflat;
  Four-Leaf Clover and Uncommon Snapdragon: heath; Candleberry, Lady's Mantle,
  Royal Toadstool, Chiming Bluebell: swamp; Rustroot, Bloated Bolete: forest;
  Bladderwrack, River Pearl Mussel, Gray Clay: beach/water; Frog's Crown,
  Glimmermoss, Feldspar, Edelweiss: mountain/cave/moor; Stalagoom, Cavebulb,
  Cave Clay: cave).
- Quality: herb quality equals the soil quality of its tile, softcapped by
  Survival per the WWW page and hardcapped by Survival per the Foraging page (the
  two legacy pages disagree; treat as "Survival-capped, exact curve unknown").
- Wild Windsown Weed: first seen at Per*Exp 80. When dried on an Herbalist Table
  (8 in-game hours, about 2.6 real hours) or a Drying Frame (2 in-game days,
  16 real hours), it converts into one random seed type (Wheat, Flax, Carrot,
  Beetroot, Yellow Onion, Peapod, Grape, Hops, Poppy, Peppercorn, Tea, Tobacco).
  The seed type is random but spatially correlated: WWWs found in the same area
  tend to give the same seed. Resulting seed quality is at most the WWW's quality
  (exact relation unknown per the wiki talk notes).
- Ant hills are raided via foraging (ants, larvae, pupae, queens as curios/food;
  see `etc/needed/fep.conf` entries Ant Larvae, Ant Pupae, Ant Queen).

## Harvest flow and yields

- The generic harvest action is Adventure > Landscaping > Harvest Crop (Legacy:
  Farming); it applies to whatever crop gob is clicked. The client sends the
  selected flower-menu option (`src/haven/FlowerMenu.java`, petal number via the
  `cl` widget message) and the server resolves the gob, its stage, and the outcome.
- The server must implement per-stage yield tables (byproduct vs main product vs
  seed return), with seed counts in the documented ranges (carrots: 1-3 seeds at
  stage 4, 1-3 carrots at stage 5) and with quality rolls per the quality model.
- Crop byproducts used downstream: Straw (wheat) feeds the straw-crafting chain
  (Straw Basket, Straw Hat, Straw Doll) and animal fodder; Plant Fibres (flax,
  hemp) feed cloth making; Flour is milled from grain with the Quern.

## Representative legacy crop/food list

Names below are cross-checked against `etc/needed/fep.conf` (the client's FEP
table), which proves the item names as they exist in the shipped resources:
Beetroot (PER:0.5 CHA:0.2), Carrot (PER:1), Grapes (CHA:1), Peapod
(STR:0.1 PER:0.9), Pumpkin Flesh (STR:1 CON:1), Yellow Onion (HHP:1),
Blueberries (INT:1), Chantrelles (DEX:1), Candleberry (CHA:3), plus prepared
crop dishes: Apple Pie, Carrot Cake, Pumpkin Bread, Pumpkin Pie, Pea Pie,
Onion Rings, Blueberry Pie, Bark Bread. Animal-sourced foods (meats, eggs, milk
products) are cataloged in the companion document
`docs/mechanics/livestock/animals-and-husbandry.md`.

## Server implementation notes (farming)

- State: per-tile records `{tilth: bool, tilth_planted_at, tilth_decay_at, soil_q}`,
  per-crop-gob records `{res: crop_id, stage: u8, planted_at, stage_durations: [hours],
  seed_q, random_offset}`. Persist tilth and soil quality; crop gobs persist with
  their gob id and sdt.
- Tick structure: one scheduler pass per few seconds scans due crop timers. On stage
  advance, update the gob's sdt stage byte and emit `OD_RES` to watchers. Do not
  emit full object recreation; the `OD_RES` delta with a fresh sdt is sufficient
  (the client's `OCache.cres` re-creates the sprite because the arriving sdt is
  non-empty, even when the resource id is unchanged).
- Deterministic per-client cosmetic randomness: nothing server-side; the client
  seeds strand variants from gob id via `mkrandoom`.
- Planting validation: require Farming skill (except where a crop demands another
  skill, e.g. grapes/Winemaking), require furrowed tile, require empty tile, consume
  the seed, emit the new gob with stage 0, and clear the tilth decay timer.
- Tilth decay: schedule reversion of unplanted plowed tiles to their base tile type;
  emit the tile change to affected map-grid watchers.
- Beehive pollination: mark tiles within 13 tiles (150 map units, matching
  `MapView`'s bhive radius constant) of a beehive; multiply stage durations by the
  configured speedup; recompute on hive placement/removal.
- Harvest: on the flower-menu choice, validate stage against the yield table, roll
  quality ([-5,+5], soil cap [-5,+2] when soil_q < seed_q, skill softcap), spawn
  item gobs or push items to the actor, then either delete the crop gob (`OD_REM`)
  or rewind its stage (replant-by-harvest behavior for seed harvests is a design
  decision; legacy appears to end the crop gob and require replanting - the wiki
  describes harvesting seeds as a harvest outcome, then replanting).
- Forageables: spawn on generation with per-terrain tables and soil-linked quality;
  gate visibility per character by Per*Exp vs base level (B/2 .. 2B); the server
  must therefore track what a character has "seen" only transiently - visibility is
  recomputed on sight entry, so no persistent per-character reveal state is needed
  beyond the skill values.
- Time: drive everything off the same clock as `GMSG_ASTRO` (`dt` day fraction, `yt`
  year fraction in `src/haven/Glob.java`); one in-game day = 8 real hours; 1 year =
  365 in-game days. Crop timers are real-time regardless of night/day.

## Open questions (farming)

- Exact per-crop stage counts and stage durations without beehives; the wiki table
  assumes beehive coverage. Determine by packet-capturing a legacy client session or
  from emulator prior art (search GitHub for H&H server emulators).
- The beehive growth-speedup factor (a single multiplier? per-stage?) - unsourced;
  determine by timing crops with and without hive coverage on a legacy server.
- The softcap curve for skill-capped crop quality, and whether Survival caps
  forageable quality softly or hard (legacy wiki pages disagree). The current-world
  client code contains no cap curves; they are server-side.
- Whether pumpkin/trellis-style plants occupy multiple tiles and whether the Giant
  Pumpkin gob blocks movement (resource neg data needed from a full resource pack;
  the local `res/` tree contains only a partial resource set without
  `gfx/terobjs/plants`).
- Season effects on crops and fruit trees (season-gated fruiting, winter kill) - no
  legacy documentation found. Determine from astrolabe observations on a live legacy
  server (year fraction `yt` vs harvest availability).
- The exact wire value of the planted-gob resource names per crop (the local
  resource pack lacks `gfx/terobjs/plants/*`); recover from a full legacy resource
  pack or a resource dump of a legacy session.
- Manure/fertilizer: confirm definitively absent in legacy (absent from all reviewed
  legacy wiki pages and client resources) before implementing anything beyond tilth
  reversion and soil-quality gradients.
- Whether harvesting seeds leaves the crop gob alive at a lower stage (multi-harvest)
  or destroys it; wiki wording implies replanting, but a capture would settle it.
