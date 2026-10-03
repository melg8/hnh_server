# Map, Grids and Terrain

> **Sources:** src/haven/MCache.java (MCache, MCache.Grid, MCache.Overlay, randoom, mkrandoom, initrandoom, makeflavor, mapdata, mapdata2, invalblob, tilemap, sendreqs, colors, cmaps, tilesz), src/haven/MapView.java (draw, m2s, s2m, tilify, drawol, enol, disol, uimsg "place"/"flashol"/"polowner", olc, plrad), src/haven/MiniMap.java (draw, caveTex, gridsHashes, isCave), src/haven/Resource.java (Tileset, Tile, WeightList, flavobjs, flavprob, btrans, ctrans), src/haven/Session.java (MSG_MAPREQ, MSG_MAPDATA, RMSG_MAPIV, RMSG_GLOBLOB, RMSG_TILES, OD_LUMIN), src/haven/Message.java (coord, uint8, uint16, int32, string), src/haven/Utils.java (uint16d, int32d, strd - little-endian decoders), src/haven/OCache.java (move, getgob), src/haven/Gob.java (rc, position), src/haven/MapMod.java (MapMod, Grabber, enol(17)), src/haven/MinimapPanel.java (pcl "Show claims", vcl "Show village authority"), src/haven/DeclaimVerification.java, src/haven/Defrag.java, src/haven/resutil/GrowingPlant.java, "The Universe of Haven" (official H&H forum thread 35295, Xcom, Feb 2014), Ring of Brodgar wiki (Legacy:Cartography, Legacy:Village_Claim)

## Summary

The legacy Haven and Hearth world is a flat, endless-seeming isometric tile plain. Space has exactly three nested units that a server implementer must keep apart:

1. **Tile** - the gameplay unit (1 tile = one entry in a grid's tile array). Walkability, terraforming, farming, claims and resource nodes are all tile-quantized.
2. **Subtile ("map pixel")** - the position unit. Every object, player and animal coordinate is expressed in subtiles; there are 11x11 subtiles per tile (`MCache.tilesz = (11, 11)`).
3. **Grid** - the network and storage unit. A grid ("map grid") is 100x100 tiles (`MCache.cmaps = (100, 100)`), streamed to clients on demand as one zlib-compressed tile array via the `MSG_MAPREQ` / `MSG_MAPDATA` exchange.

The server is the sole authority for tile data. The client never generates terrain; it only renders what `MAPDATA` tells it, plus deterministic *flavor objects* (trees, bushes, stones) that the client itself places from a seeded per-tile RNG (`MCache.randoom` / `mkrandoom` / `Grid.makeflavor`) so that server and client agree without the server sending those objects as entities. Player claims and village plots are streamed as per-tile overlay masks inside the same `MAPDATA` payload and drawn as colored diamonds (`MapView.drawol`).

This document specifies the wire-visible map model, the coordinate conventions, tile types, overlays/plots, flavor-object replication, and the terraforming behaviors that the server must implement or emulate. Weather and time are covered in `time-weather-astronomy.md`; planting/growth details are covered in `../livestock/farming-and-plants.md`.

## Coordinate conventions (read this first)

All numbers below are verified against client code. Little-endian wire encoding is used everywhere (`Message.int32` -> `Utils.int32d`, `Message.uint16` -> `Utils.uint16d`).

- **Subtile / map-pixel coordinate ("map coord", `Coord` on the wire)**: one signed 32-bit pair (`Message.coord()` = two `int32`). This is the fundamental position unit. `Gob.rc` (src/haven/Gob.java) is a map coord; movement targets, click positions and placement coordinates are all map coords. The client divides/multiplies by 11 to move between subtiles and tiles (`mc.div(tileSize)` in src/haven/MapView.java).
- **Tile coordinate**: `mapCoord / 11` using the client's `Coord.div(Coord)` (src/haven/Coord.java lines 132-142), which is **true floor division** - it adjusts negative operands so results round toward negative infinity, not toward zero. A tile's center subtile is `(5,5)` within the tile (community-verified; also why `MapView.tilify` snaps clicks: `c.div(tileSize).mul(tileSize)`, src/haven/MapView.java lines 842-852).
- **Grid coordinate**: `tileCoord / 100` (`tc.div(cmaps)`), with in-grid tile coordinate `tileCoord.mod(cmaps)` (`MCache.gettilen`, `MCache.getground`). `cmaps = (100, 100)`. `Coord.mod` (src/haven/Coord.java lines 152-162) is likewise Euclidean: the in-grid component is always in `0..99` for any sign of the absolute coordinate, so grids tile the plane seamlessly across the origin.
- **Absolute vs per-grid**: grid coords are the "absolute" addressing scheme; tile arrays are stored per grid (`Grid.tiles[100][100]`). The client converts absolute tile coord -> (grid coord, in-grid coord) with `div`/`mod` on `cmaps` every time it reads a tile (`MCache.gettilen`). Flavor-object RNG seeding uses the *absolute* tile coordinate: `Grid.mkrandoom(c)` seeds with `c.add(gc.mul(cmaps))` (src/haven/MCache.java lines 201-207).
- **World extent**: no bounds are encoded anywhere in the protocol; coords are 32-bit. Legacy world 7 was reportedly 7x7 "super grids" of 50x50 map grids each (external, forum post) - an implementation target, not a protocol limit.
- **Isometric projection (client-only)**: `MapView.m2s(c) = (2x - 2y, x + y)` and `s2m` its inverse (src/haven/MapView.java lines 626-632). A tile (11x11 subtiles) renders as a 44x22 px diamond. The server never sees screen coordinates; it works purely in map coords and tile coords.
- **Where each unit is used**:
  - Tile coords: `MAPREQ`/`MAPDATA` grid addressing, plot rectangles, `MapMod` area selection (`mc.div(MCache.tileSize)`, src/haven/MapMod.java), overlay masks, claims.
  - Map coords: gob positions (`OCache.move`), click messages `wdgmsg("click", c0, mc, button, modflags[, id, rc])` (src/haven/MapView.java lines 729, 738), plant/terobj placement.
  - Grid coords: only inside the map protocol (`MSG_MAPREQ` payload, `MAPDATA` grid key, `invalblob` type 0 and type 1 rects).

Recommended Rust model: `MapCoord { x: i32, y: i32 }` (subtiles), `TileCoord = MapCoord.div_euclid(11)`, `GridCoord = TileCoord.div_euclid(100)`, `in_grid = TileCoord.rem_euclid(100)` - Rust's `div_euclid`/`rem_euclid` reproduce the client's `Coord.div`/`Coord.mod` exactly for all signs (src/haven/Coord.java).

## Grid system and the MAPREQ / MAPDATA flow

Client side (src/haven/MCache.java):

- `MCache.grids: Map<Coord, Grid>` holds received grids; `MCache.req` holds pending grids. `MapView.draw()` requests every grid intersecting the box `cameraCenter +/- 500` map pixels (src/haven/MapView.java lines 1953-1961); `MiniMap.draw()` additionally requests every grid visible on the minimap (src/haven/MiniMap.java lines 304-317).
- `sendreqs()` sends `Session.MSG_MAPREQ` (type 4, client->server) per pending grid at most once per second per grid (`gr.lastreq`), and gives up after 5 attempts (`gr.reqs >= 5`). Payload: one coord (the grid coordinate) - `msg.addcoord(c)`.
- The server answers with `Session.MSG_MAPDATA` (type 5, server->client) messages that are **fragmented**: header `int32 pktid, uint16 off, uint16 len` followed by a fragment of the assembled payload (`MCache.mapdata`). Fragments accumulate in `Defrag` buffers (src/haven/Defrag.java) keyed by `pktid`; buffers idle > 10 s are dropped (src/haven/MCache.java lines 536-542). A Rust server should fragment large grids (assembled payload is >= 10,010 bytes) and can pick any `pktid` per transfer.
- Assembled payload, parsed by `MCache.mapdata2` (src/haven/MCache.java lines 397-515):
  1. `coord` - grid coordinate (two little-endian `int32`).
  2. `string` (zero-terminated UTF-8, `Utils.strd`) - the grid name `mnm`. Empty string means "no name" (parsed as `null`). The fork's minimap uses `mnm` as a persistent per-grid identity to stitch an explored-world map across sessions (`MiniMap.gridsHashes` / `coordHashes`, src/haven/MiniMap.java lines 319-361); grids whose `mnm` is absent are treated as cave grids by the minimap (`MiniMap.caveTex`, `MiniMap.isCave`). Treat `mnm` as a stable per-grid hash string.
  3. A plot-flag lookup table: repeated (`uint8 pidx`, `uint8 fl`) pairs terminated by `pidx == 255` (`pfl[pidx] = fl`).
  4. A raw zlib (`java.util.zip.Inflater`) deflate stream. The client inflates from the current offset to end of message; on a data-format exception the fork retries after skipping 2 bytes (`msg.off += 2`), which hints the fork server may prefix the stream with a 2-byte header. Decide one format and stay consistent (see Open questions).
  5. Decompressed body: exactly `100 * 100` `uint8` tile ids, written **row-major over y then x**: `for y { for x { tiles[x][y] = uint8() } }`. Index as `tiles[x][y]`.
  6. Plot records: repeated until a `uint8 == 255` terminator: `uint8 pidx` (index into `pfl`), `uint8 type`, then a rectangle `uint8 c1x, uint8 c1y, uint8 c2x, uint8 c2y` (in-grid tile coords, inclusive on both ends). Overlay mask for the whole rect: `type == 0` -> `1`, or `2` when `fl & 1`; `type == 1` -> `4`, or `8` when `fl & 1` (src/haven/MCache.java lines 473-502). Masks are OR-ed into `Grid.ol[x][y]`. Client naming convention (from the minimap toggles, see below): type 0 rects are personal claims, type 1 rects are village claims.
- On success the client moves the grid from `req` to `grids`, drops any previous grid at the same coord (`Grid.remove()` detaches client-side flavor gobs), renders the 1-px-per-tile minimap texture (`Grid.render()` using `MCache.colors`), and regenerates flavor objects (`Grid.makeflavor`).
- Invalidation, server->client `RMSG_MAPIV` (relmsg type 3, src/haven/Session.java line 346) -> `MCache.invalblob`: `uint8 mode`; mode 0 = invalidate one grid (a coord follows; client re-requests it), mode 1 = trim: two coords `ul, lr` - client deletes all cached grids *outside* the inclusive grid-coord rect (`MCache.trim`), mode 2 = `trimall()` - drop everything. Mode 1/2 are used on teleport/login/zone switches; mode 0 is the per-grid "tiles changed" push.
- Tileset table, server->client `RMSG_TILES` (type 11) -> `MCache.tilemap`: repeated (`uint8 id`, `string resname`, `uint16 resver`) binding tile id -> tileset resource; up to 256 ids (`sets = new Tileset[256]`). The fork hard-pins id 11 to `gfx/tiles/wald/leaf` version 6 ("looks like a shit", src/haven/MCache.java lines 546-557). Send this once after login (and whenever the server adds tile types).

Server obligations: persist per-grid 10,000-byte tile arrays; accept `MSG_MAPREQ`, answer with the exact payload shape above; push `invalblob` mode 0 whenever a tile changes under a client that has the grid loaded; use mode 1/2 to manage client cache scope on player relocation. Community documentation of the official server ("The Universe of Haven", forum thread 35295) confirms grids are 100x100 tiles and that the server re-uploads a grid to viewers whenever any tile in it is updated.

## Tile types and terrain encoding

A tile is a single `uint8` id in the grid array; there is no per-tile metadata on the wire beyond that id plus the overlay byte. Tile *appearance* is defined by the tileset resource bound to the id via `RMSG_TILES` (`Resource.Tileset` layer, src/haven/Resource.java lines 827-936):

- `Tileset` binary layout: `uint8 fl` (flag bit 0 = "has transition tiles"), `uint16 flavobj-count` (read only when `Config.showFlavors` is set in this fork), `uint16 flavprob`, then `flavobj-count` entries of (`string resname`, `uint16 resver`, `uint8 weight`). All multi-byte values little-endian (`Utils.uint16d`).
- `Tileset.init()` partitions the resource's `Tile` layers: `t == 'g'` ground variants into the `ground` weight list; `t == 'b'`/`t == 'c'` border/corner transition tiles into `btrans[t.id - 1]` / `ctrans[t.id - 1]` (15 slots, tile ids 1..15). `hastrans = (fl & 1) != 0`.
- The client picks a ground variant per tile deterministically: `ts.ground.pick(randoom(tc))` (src/haven/MCache.java `getground`), and renders 3x3-neighborhood transitions with the same seeded RNG (`gettrans`, src/haven/MCache.java lines 272-336): transition tiles exist only for ids *strictly lower* than the center tile's id; a border (`btrans`) variant is chosen when an edge neighbor has exactly that id, a corner (`ctrans`) variant when a corner neighbor does, and a corner neighbor is ignored when its id is greater than or equal to one of its two adjacent edge-neighbor ids (the comparison chain at lines 296-311, which prevents slivers where two transition bands meet). A Rust server does not need this for authority, but needs the same tileset resources to be available to clients.

Tile id semantics. The client's minimap color table (`MCache.colors`, lines 61-82) is the most reliable in-repo evidence for what each id means:

| id | color constant | meaning |
|----|----------------|---------|
| 0 | 0x3152a2 | deep water |
| 1 | 0x4480c8 | shallow water |
| 8 | (160,160,160) | stone paving |
| 9 | (200,200,200) | plowed |
| 10 | 0x497937 | coniferous forest |
| 11 | 0x60864f | broadleaf forest |
| 12 | (220,220,200) | thicket |
| 13 | 0x468d37 | grass |
| 14 | 0xac7664 | moor |
| 15 | 0x999927 | heath |
| 16 | 0x60ad8a | swamp 1 |
| 17 | 0x3d6242 | swamp 2 |
| 18 | 0x5e6453 | swamp 3 |
| 19 | 0xa67936 | dirt |
| 20 | (212,164,81) | sand |
| 21 | (212,212,212) | house floor |
| 24 | (80,80,80) | mine |
| 25 | (112,116,112) | cave |
| 255 | black | void |

Ids without a client color render magenta in `Grid.render()`. The external source adds ids the fork's table omits (forum thread 35295, world-7 era): 3-7 brick floors (red/yellow/black/blue/white), 22 house cellar, 23 mine entry, 26 mountain. Id 2 is not listed anywhere in the repo.

Gameplay notes (external, community-verified on legacy-era servers): grass/heath/moor/forest/dirt are terraformable; water, shallow water, cave, mountain, void and brick/paved floors are fixed; plowed (9) is the intermediate state for farming (see `../livestock/farming-and-plants.md`) and decays back over time; `void` (255) is what mountain becomes when mined out. Walkability and movement speed per tile type are server-side rules not visible in this client.

## Flavor objects: deterministic per-tile placement (randoom)

This is the single most integration-critical mechanic in the map model. Trees, bushes, stones and other "flavor" decorations are *not* streamed as gobs. Both sides derive them from the tile data:

- Seeded RNG: `MCache.initrandoom(Random r, Coord c)` does `r.setSeed(c.x); r.setSeed(r.nextInt() ^ c.y);` - a two-step seeding of `java.util.Random` from the absolute tile coordinate (src/haven/MCache.java lines 222-225). `MCache.mkrandoom(c)` returns a fresh `Random` seeded that way; `MCache.randoom(c)` returns `abs(nextInt())`, `randoom(c, r)` returns `randoom(c) % r`.
- Placement: `Grid.makeflavor()` (lines 176-199) walks every in-grid tile `c` (in-grid coords), looks up the tileset of `tiles[c]`, and if `set.flavobjs` is non-empty: `rnd = mkrandoom(absolute tile coord)`; if `rnd.nextInt(set.flavprob) == 0`, pick a weighted flavor resource `set.flavobjs.pick(rnd)` and create a client-local gob at `c.add(gc.mul(cmaps)).mul(tileSize)` - i.e. **subtile coord = tile coord * 11**, the tile's corner. Gob id is `-1` (client-local).
- Consequence for the server: to know which flavor objects exist at which coordinates (and thus what a player is clicking at), the server must run the identical algorithm: identical tileset tables (`flavobjs`, `flavprob`), identical weights, and a bit-exact reimplementation of `java.util.Random` (a 48-bit LCG with the standard `setSeed` scramble; `nextInt` = next(32)). This is well-understood and easy to port to Rust.
- Consequence for interaction: a click on a flavor gob sends a click *without* object id - `wdgmsg("click", c0, mc, button, ui.modflags())` (src/haven/MapView.java lines 720-740) - because the gob's id is -1 and only `mc` (the click's map coord) is meaningful. Server-side resolution is by coordinate: derive the flavor gob from the map coord's tile (and, if needed, the exact subtile), then run the interaction. Clicks on streamed gobs instead append `hit.id` and `hit.position()`.
- Flavor objects are pure decoration in this fork's rendering (gated by `Config.showFlavors` at load time and `paginae/add/hide/flav` visibility toggle, src/haven/Glob.java lines 58-77), but on official servers the same coordinates back real, harvestable resource objects (trees you chop are flavor-positioned). Whether the server should also spawn *server-side* resource entities at the same coordinates, or treat "flavor object at coord X" as implicitly existing, is an architecture decision - both work with this client as long as click-by-coordinate is resolved.

Note also `Gob`-level RNG reuse: `GrowingPlant.Factory.create` uses `owner.mkrandoom()` to jitter plant strand placement (src/haven/resutil/GrowingPlant.java) - the same seeded-RNG-from-coord pattern, worth keeping in the Rust port for visual parity.

## Plots, claims and map overlays

Per-tile overlay bits travel inside `MAPDATA` (see plot records above) and are rendered by `MapView.drawol` (src/haven/MapView.java lines 1095-1138): for each enabled bit the tile diamond is filled with the color at 32/255 alpha, and edges are outlined where the neighbor tile lacks the bit (crisp claim borders).

- Overlay colors, indexed by bit: `olc[0]` pink (255,0,128), `olc[1]` blue (0,0,255), `olc[2]` red (255,0,0), `olc[3]` purple (128,0,255), `olc[16]` green (0,255,0), `olc[17]` yellow (255,255,0) (src/haven/MapView.java lines 136-141).
- Visibility toggles: `MinimapPanel` button `pcl` ("Show claims") calls `mv.enol(0, 1)` / `disol(0, 1)`; button `vcl` ("Show village authority") toggles bits 2 and 3 (src/haven/MinimapPanel.java lines 20-66). So bits 0/1 belong to personal claims and bits 2/3 to village claims; the `fl & 1` variant of each type (bits 1 and 3) is a second visual state of the same plot kind (candidate meanings: own vs. foreign plot, or paid/abandoned state - see Open questions).
- `MapView.uimsg("flashol", mask, ms)` briefly flashes any overlay mask - used by the server-driven UI to highlight an area (src/haven/MapView.java lines 869-876).
- Local (client-only) overlays use the same machinery: `MCache.Overlay(c1, c2, mask)` rectangles held in `MCache.ols` are OR-ed into `getol()` results (src/haven/MCache.java lines 390-394).
- Area selection widget: `MapMod` ("Area Selector", widget type `mapmod`, instantiated by the server through `RMSG_NEWWDG`, src/haven/MapMod.java lines 42-48) enables overlay bit 17 (`ui.mapview.enol(17)`), grabs map mouse events (`MapView.Grabber`: `mmousedown/mmouseup/mmousemove`), and tracks an inclusive tile rectangle `c1..c2` with a live "Selected: WxH" label. What the Accept button sends back to the server is fork-incomplete (the accept/cancel buttons carry no click handlers in this tree); the upstream pattern is a `wdgmsg` back to the server with the rectangle. Use this widget for server-side bulk operations (declaring land, placing fence runs, admin work).
- Declaiming: `DeclaimVerification` (src/haven/DeclaimVerification.java) is a confirmation dialog ("Are you sure you want to declaim your plot?") whose Declaim button re-sends the original menu action array (`ui.menugrid.wdgmsg("act", ad)`) - i.e. declaiming is just a normal paginae/menu action, invoked with confirmation. The current fork has the automatic confirmation path commented out in `MenuGrid` (lines 371-372).
- Region naming: `MapView.uimsg("polowner", name)` - the server pushes the owning polity's name when the player crosses a boundary; empty string means leaving; the client renders "Entering <name>" / "Leaving <name>" (src/haven/MapView.java lines 902-915). This is how claim/village/region names reach the HUD.

## Terraforming and tile modification by players

The client has no "edit tile" message. All player terraforming is: player invokes an action (adventure menu / paginae, e.g. plow, pave, dig), the server validates and changes the tile server-side, then invalidates the grid (`invalblob` mode 0), and the client re-fetches the whole grid via `MAPREQ`. Evidence:

- Plowing produces tile id 9, paving id 8 (color table above; also `paginae` menu actions arrive as server-defined resources - the fork's `paginae/add/*` entries in src/haven/Glob.java are UI toggles, not actions).
- The fork's `paginae/add/hide/*` resources (tree, wall, bush, gate, flav, ston, thik, cabi, mans, plan; src/haven/Glob.java lines 67-77) are client-side *visibility filters* for object categories, not map actions - do not mistake them for terraforming verbs.
- `MapMod` area selection exists precisely because terraforming and construction operate on tile rectangles.

Server-side tile lifecycle (community-verified, legacy era, forum thread 35295 - implement as tunable rules):

- Plowed tiles decay back to their origin tile (grass/heath/moor) after a random period; a decaying plow can also flip to moor/heath, which is how players convert grassland.
- Heath spreads onto grass and moor; moor spreads onto grass; dirt decays similarly to plowed. Spread is a slow random neighbor (including diagonal) walk.
- Trees can convert nearby tiles toward their forest type (coniferous/broadleaf); reportedly only within their 9.09-tile "server grid" (see below), and 8 surrounding grids seeded with a forest type can activate auto-spread of the center grid.
- Only *player* terraforming forced a grid update on the official server; server-side spread/decay did not push updates (a long-standing bug, per the forum post). A Rust server should do better: push `invalblob` mode 0 to all viewers whenever tiles change for any reason, but rate-limit to avoid floods.
- "Server grids": the official server reportedly uses an internal 100x100-*subtile* lattice (11x11 per map grid, ~9.09 tiles) for view distance, animal food checks, tree terraforming ranges and beehive coverage. None of this is protocol-visible; replicate it only if you want bug-compatible behavior (see Open questions).

## Object placement: tiles vs. subtile coordinates

- Gobs (objects, players, animals) live at map coords (`Gob.rc`). The server streams them via `OCache` messages (`Session.MSG_OBJDATA`/`MSG_OBJACK` path; `OCache.move`, `OD_MOVE` = 1, src/haven/Session.java lines 45-61); movement is client-interpolated from `LinMove`/`Homing` attributes. This doc only fixes the map-side rule: **object placement is not tile-quantized**; a gob can stand on any subtile.
- Placement UI: `MapView.uimsg("place", resname, resver, plontile[, radius])` creates a ghost gob that follows the mouse, either free (`plontile == 0`, at exact mouse map coord) or tile-snapped (`tilify(mousepos)`), plus an optional green radius ellipse (`plrad`, subtile units; `drawradius` renders `fellipse(c, (r*4*sqrt(0.5), r*2*sqrt(0.5)))`, src/haven/MapView.java lines 1140-1143, 877-901). Beehives carry a radius (`radiuses` map, "gfx/terobjs/bhived" special case). The ghost is confirmed by a normal click; the server places the object at the reported coord and is responsible for enforcing snap/radius rules.
- Pathfinding and movement happen on subtiles; the server validates each step (community-documented: the mover "teleports" step by step, with step length derived from speed, which is why high-speed characters can skip over thin hitboxes). Tile type affects traversal (water blocks, etc.) - rules are server-side.
- Buildings: house floor tile (21) / cellar (22) are *tiles* set by the server when construction completes; walls, gates and furniture are gobs. Cave/mine "walls" likewise are gobs over cave (25) / mine (24) tiles, not tiles themselves.

## Water, rivers, elevation and resource nodes

- Water exists only as tile ids 0 (deep) and 1 (shallow). There is no elevation, no water level and no flow data anywhere in the client protocol; rivers and lakes are simply painted tiles, and boats/ferries are gobs. Any river-bank erosion or flooding mechanic would be purely server-side tile edits.
- Resource nodes (soil/clay/water/fish quality) are server-side scalars; community measurement shows their quality peaks exactly at map-grid crosshair centers (the four-grid corner points) and decays radially, with quality regenerating about 1 unit per 8 real hours (forum thread 35295). This strongly suggests node fields are stored per-grid-corner with radial interpolation - a convenient, bug-compatible choice for the Rust server. Node depletion on use is server-side; the client only sees resulting item quality.

## Caves and mines

- Cave interiors use tile ids 24 (mine), 25 (cave), 26 (mountain), with 23 as mine entry and 22 as house cellar (external list; client colors confirm 24/25). Caves are ordinary grids of these tile ids.
- The fork minimap detects caves via `MiniMap.isCave()` / `caveTex`: grids delivered without an `mnm` name string are rendered with their per-grid colored texture and collected in `caveTex` (src/haven/MiniMap.java lines 302-361); `trimall()` clears the cave texture cache. Practical implication: name your overworld grids and leave cave grids unnamed (or handle the distinction server-side and ignore this fork quirk).
- Mining targets `void`/mountain tiles server-side (digging converts mountain toward void/mine tiles); the client displays the result after a grid invalidation. Walls/stones inside caves are gobs.

## Resource growth and respawning on the map

- Plant crops: a planted crop is a gob whose drawable resource is a `GrowingPlant`/`CommonPlant`/`GaussianPlant` sprite; the growth stage arrives as the first byte of the gob's dynamic sprite data (`sdt.uint8()` in src/haven/resutil/GrowingPlant.java). The server advances stages on its own clock (season/weather multipliers belong there) and re-sends the object data on change. Full farming rules: `../livestock/farming-and-plants.md`.
- Wild resource respawning (bushes regrowing, stumps decaying, tree stages) follows the same gob-attribute model - server-owned timers, client-invisible until an object update or grid invalidation.
- Animal feeding and beehive coverage are computed against grass tiles in a radius (150 subtiles for beehives per community measurement); see `../livestock/` for the livestock-side contract.

## Server implementation notes

Minimum viable map stack for the Rust server, in dependency order:

1. Storage: `HashMap<(i32, i32), Grid>` with `Grid { tiles: [u8; 10000], overlays: Vec<Plot>, name: Option<String> }`; plots stored as (type, flag, inclusive rect) plus the OR-ed mask cache. Persist dirty grids.
2. PRNG: port `java.util.Random` exactly (48-bit LCG, `setSeed`, `nextInt`); implement `initrandoom` = `set_seed(x); set_seed(next_int() ^ y)`. Unit-test against known Java outputs.
3. Tilesets: ship the same `gfx/tiles/**` resources the client loads; parse the tileset layer only if you need server-side flavor placement (`flavprob`, `flavobjs` weights).
4. Flavor generation: run `makeflavor` logic server-side at grid creation/modification; index produced coordinates for click resolution. Keep it in sync on every tile change (re-derive affected tiles).
5. Protocol: answer `MSG_MAPREQ` (fragmented `MSG_MAPDATA`), push `RMSG_MAPIV` invalidations (mode 0 per changed grid for all viewers; mode 1/2 on relocation/login), send `RMSG_TILES` once at login.
6. Claims: maintain claim/plot rects per owner; send them in every `MAPDATA` of an affected grid; push `polowner` transitions on player movement; expose the `mapmod` widget for rectangle selection flows.
7. Placement validation: enforce tile-snapping when the client sends `place` with `plontile`, and radius checks (radius arrives in subtile units).
8. Terraforming verbs: implement plow/pave/dig/stump-to-dirt as tile edits + invalidations; implement decay/spread as a slow background tick; decide explicitly whether to replicate the official "no invalidation on natural spread" bug (recommendation: do not).
9. Node fields: store quality scalars at grid corners (or per 100x100-subtile cell), interpolate radially, regenerate ~1 Q / 8 real hours.
10. Client compatibility quirks to honor: the `pfl` flag table and 255 terminators; zlib body without a declared length (the client inflates to end-of-message); `mnm` presence/absence to steer the fork minimap; id 11 tileset pin if you serve this exact fork.

## Open questions

- Exact semantics of the plot `fl & 1` flag (overlay bits 1 and 3, blue/purple): own-vs-foreign coloring, abandoned/disputed state, or something else. Determine by decoding `MAPDATA` from a live legacy server around claim creation/declaim events.
- Whether the zlib body in `MSG_MAPDATA` carries a 2-byte prefix on the official server (the fork client's `msg.off += 2` retry suggests it). Determine by capturing a session against the official server or the fork's server companion.
- The intended content/format of the `mnm` grid name string (random hash? coordinate-derived?). Observe multiple sessions to see if it is stable across relogs and world states; the fork minimap depends on its stability.
- Whether flavor objects correspond 1:1 to server-side resource entities on the official server (and whether flavor coordinates shift when tiles change). Determine by clicking trees at computed coordinates and observing which server objects resolve.
- Server-side terrain generation: the client has no generator; supergrid-scale generation (50x50 grids, water not crossing supergrid borders) is community-documented only. The Rust server is free to generate or hand-author grids; document your seed format.
- Flavor-object click resolution: flavor gobs are placed at tile *corners* (`tileCoord * 11`), and a click delivers a raw map coord. Confirm on a live server whether interactions resolve the flavor object by nearest object origin, by the clicked tile, or by a hitbox test, and match that in the Rust server's click handler.
- Tile id 2 and the exact brick-floor ids 3-7 on this fork: absent from the client color table; confirm by rendering a grid containing them (they will draw magenta) or from the official tileset table sent in `RMSG_TILES`.
- Movement-speed-dependent step rounding on subtiles (community-documented "teleport steps" and corner-jump exploits): exact server algorithm unknown; decide whether to replicate (exploit-compatible) or fix.
