# Visibility and Object Lifecycles: Spawning, Despawning, and What a Server Must Persist

> **Sources:** src/haven/OCache.java, src/haven/Session.java, src/haven/Gob.java, src/haven/MapView.java, src/haven/MCache.java, src/haven/Resource.java, src/haven/ResDrawable.java, src/haven/resutil/GrowingPlant.java, src/haven/resutil/CommonPlant.java, src/haven/resutil/GaussianPlant.java, src/haven/Item.java, src/haven/Inventory.java, src/haven/ISBox.java, src/haven/Equipory.java, src/haven/FlowerMenu.java, src/haven/GobHealth.java, src/haven/Fightview.java, src/haven/KinInfo.java, src/union/jsbot/JSItem.java, src/union/jsbot/JSGob.java, src/union/KerriUtils.java, upstream dolda2000/hafen-client OCache.java (virtual-gob cross-check), Ring of Brodgar wiki (Legacy: namespace)

## Summary

The legacy client has no notion of "requesting objects": apart from map grid
requests (`MSG_MAPREQ = 4`, src/haven/Session.java:40) it only *receives* world
objects. Everything a player can see, click, or fight arrives through the
`OCache` (src/haven/OCache.java), streamed and retracted at the server's sole
discretion. The server is therefore the visibility authority: it maintains a
per-observer set of visible gobs, spawns them with full state, updates them
with deltas, and retracts them when they leave the observer's view.

This document specifies the observable lifecycle contract (spawn, update,
removal, tombstones, frame ordering), what the client source reveals about the
server's view radius, resource-based object composition, plant growth stages,
item drop and lifting flows, container widgets vs world gobs, object health,
aggro/crime hints, and the categories of object state a Rust server must
persist.

Cross-references: the wire format and attribute semantics are in
objects-and-dynamics.md; animal behaviors that drive aggro and pursuit are in
../livestock/animals-and-husbandry.md.

## The visibility contract: server-authoritative streaming

Observable facts from the client:

- The only client->server request related to the world is the map grid request
  (`sendreqs` in src/haven/MCache.java:612, driven by
  src/haven/MapView.java:1953-1965). No message type exists for "send me
  objects"; object state arrives exclusively in `MSG_OBJDATA`
  (src/haven/Session.java:471-474).
- The renderer draws exactly the gobs the `OCache` iteration yields, which
  covers both the server-streamed map (`objs`, keyed by id) and client-local
  collections (`local`, added/removed via `ladd`/`lrem`,
  src/haven/OCache.java:84-90): tileset flavor gobs (id -1, generated from
  tile-position-seeded randomness when a grid arrives,
  src/haven/MCache.java:176-199) and the placement-preview "plob"
  (src/haven/MapView.java:877-901). Everything else the player sees exists
  because the server said so.
- Objects are retracted by the server via the removal entry (flag bit 0 or
  `OD_REM = 0`, src/haven/Session.java:200-206); the client also forgets
  everything on session end (state "fin"/"dead", src/haven/Session.java:475-480).

Consequently the server must implement, per observer: a visibility set (enter =
full spawn description, stay = deltas, leave = removal), and it must tolerate
unreliable delivery by retransmitting until the per-gob acknowledgment
(`MSG_OBJACK`, see objects-and-dynamics.md) confirms receipt.

## What the client source can and cannot tell us about the view radius

The client never learns or stores the server's view radius; removal is purely
server-driven. Two client-side anchors bound the problem:

- **Grid fetch window.** While rendering, the client requests every 100x100
  grid (`MCache.cmaps = new Coord(100, 100)`, src/haven/MCache.java:54; tile
  size 11 units, `MCache.tileSize = new Coord(11, 11)`,
  src/haven/MCache.java:53) intersecting a square of +/-500 map units around
  the view center: `mc.add(-500, -500).div(tileSize).div(cmaps)` to
  `mc.add(500, 500)...` (src/haven/MapView.java:1930-1931 and 1953-1954).
  That window is about +/-45 tiles (500/11 = 45.45). Objects inside grids the
  client has fetched may become visible without warning, so the legacy
  server's streaming radius is at least comparable to this window; a server
  that streams objects within roughly this distance will never show the
  client an area it has objects for but no gobs.
- **Removal behavior.** `OCache.remove` (src/haven/OCache.java:47-52) is only
  ever called from the protocol parser, i.e. when the server explicitly says
  an object is gone. The client keeps a `deleted` tombstone map of
  `id -> frame` (src/haven/OCache.java:37) so late/spurious spawns of
  recently-removed objects can be rejected (see below).

The exact legacy radius (and whether it is a circle, a square, or grid-based)
is **not recoverable from the client** and is listed in Open questions. A
faithful reimplementation should treat "view radius" as a tunable policy; the
client-observable constraints are (a) objects must be spawned before they
enter the renderable window and (b) removals must be timely so hidden objects
do not accumulate.

## Frame ordering, tombstones, and anti-resurrection

Per-gob `frame` numbers are the ordering and anti-desync mechanism
(src/haven/Session.java:311-313 stamps `Gob.frame` after each entry):

- `OCache.getgob(id, frame)` (src/haven/OCache.java:96-116) creates a gob only
  if no tombstone `deleted[id] >= frame` exists; a stale spawn (frame older
  than the recorded removal) returns null and every subsequent attribute
  handler no-ops for it. Tombstones are thus cheap ordering filters, not a
  memory leak prevention mechanism only.
- Server rule: when removing a gob, record the removal frame; if the same
  logical object id cannot be reused at a *higher* frame later, prefer fresh
  ids. When reusing ids (if the legacy server ever did), the new spawn must
  carry a frame strictly greater than the removal frame the observers saw.
- A removal entry with flag bit 0 carries `frame` such that the client calls
  `remove(id, frame - 1)` (src/haven/Session.java:200-202): the removal is
  effective as of the *previous* frame. An explicit `OD_REM` attribute uses
  the entry's frame as-is (src/haven/OCache.java:47-52). A server should pick
  one convention (the flag-bit form is what a removal-only entry naturally
  uses) and apply it consistently.

## Object spawn lifecycle

Observed spawn path and its required contents:

1. The server allocates a gob id and picks the current frame for the new
   observer's stream. Ids are opaque int32 to the client; `playergob` itself
   is delivered as widget argument 2 of the `mapview` widget at session start
   (src/haven/MapView.java:123-135).
2. The server sends a full description: `OD_RES` (resource + `sdt`), `OD_MOVE`
   (absolute position), plus every currently meaningful attribute
   (`OD_LINBEG` if moving, `OD_AVATAR`/`OD_LAYERS` for characters,
   `OD_LUMIN` for lights, `OD_OVERLAY` for active effects, `OD_HEALTH` if
   damaged, `OD_BUDDY` if kinship-labeled), then `OD_END`
   (see objects-and-dynamics.md for layouts).
3. The client materializes the gob asynchronously: the resource may still be
   downloading (`ResDrawable.init` waits for it, src/haven/ResDrawable.java:46-52),
   and the first render tick of the drawable is deliberately delayed by
   `initdelay` (0..3000 ms random, src/haven/Gob.java:35 and 108-110) so that
   clusters of identical objects do not animate in lockstep.
4. The client acks the entry; the server marks the frame acked and switches
   the gob to delta mode for this observer.

Observable special cases: the client's placement flow means that when the
server sends the mapview a `place` uimsg it must be prepared to receive a
`place` wdgmsg with the confirmed coordinate and then spawn the real object
itself (the preview gob is client-local and removed on `unplace`,
src/haven/MapView.java:877-901).

## Object despawn / removal lifecycle

- Removals are `OD_REM` or the flag-bit form; both are processed
  synchronously in the parser (src/haven/Session.java:200-208). There is no
  client-side "out of view" cleanup - a server that forgets to remove will
  leak ghosts on every client, which is exactly what the tombstone map
  (src/haven/OCache.java:37, 96-116) protects against on re-entry.
- Persistent overlays on a removed gob simply disappear with it; but for
  fade-out polish the server should send overlay removals (resid 65535)
  before the removal entry (see objects-and-dynamics.md "Overlays").
- Decorative death states are still server data: a dead player is a normal gob
  whose avatar layers include the death pose (`gfx/borka/body/dead/...`
  detection in src/union/jsbot/JSGob.java:240-246). Removal of the corpse gob
  is a server policy (decay), not a client behavior.

## Resource-based composition of a gob

A gob is "resource + dynamic state" (the resource provides *what it is*, the
sdt and OD attributes provide *how it currently is*):

- Resource identity: 16-bit id resolved via `RMSG_RESID = 6` announcements
  (`uint16 resid, string resname, uint16 resver`, src/haven/Session.java:352-358).
  Names are dot-separated paths (e.g. `gfx/terobjs/bhive`,
  src/haven/MapView.java:611); the version distinguishes content revisions.
- Resource content relevant to objects (src/haven/Resource.java:52-60):
  `imgc` images, `negc` footprint (`Neg.cc/bc/bs/sz/ep`,
  src/haven/Resource.java:755-783 - the bounding box is stored in map space
  after an s2m conversion at parse time), `animc` animations, `tooltip`,
  `action` (`AButton` - the action-menu button descriptor used by the menu
  grid, src/haven/MenuGrid.java:100), `pagina`, `audio`, `tile`,
  `tileset`.
- The sprite is built by `Sprite.create(gob, res, sdt)` from the resource and
  the `sdt` blob (src/haven/ResDrawable.java:51); the same (resource, sdt)
  pair must therefore be treated by the server as the object's "visual kind".
- Interaction options are server logic, not resource data: the server creates
  the flower menu (widget type `sm`, src/haven/FlowerMenu.java:44-54) with
  plain string options, and the client replies with `cl` and the chosen option
  number (petal selection at src/haven/FlowerMenu.java:90, menu reply
  `wdgmsg("cl", ...)` at src/haven/FlowerMenu.java:203-208 and 262-276,
  server-driven act/cancel uimsgs at src/haven/FlowerMenu.java:235-245).

## Growth stages: plants and stage-carrying objects

`GrowingPlant.Factory.create` (src/haven/resutil/GrowingPlant.java:61-78)
reads the first `sdt` byte as the growth stage `m` and selects
`strands[m][variant]`; the number of stages and variants comes from the
resource's sprite factory configuration (constructor
`Factory(int stages, int variants, int num, boolean rev)`,
src/haven/resutil/GrowingPlant.java:39-59). The strand scatter within the
footprint is client-side deterministic randomness keyed by gob id
(`Gob.mkrandoom`, src/haven/Gob.java:317-322 - `new Random(id)` for real gobs,
position-seeded for local ones). Decorative (non-growing) plant resources use
`CommonPlant` / `GaussianPlant`, which ignore `sdt` and scatter fixed strands
(src/haven/resutil/CommonPlant.java:51-61,
src/haven/resutil/GaussianPlant.java:52-62).

Server consequences:

- The *entire* growth state of a planted crop is one byte in the gob's `sdt`.
  Advancing a crop = resend `OD_RES` with the new byte (the replace rule in
  `OCache.cres`, src/haven/OCache.java:125-133, rebuilds the sprite because
  the new sdt is non-empty). The stage byte indexes `strands[m]` directly
  (src/haven/resutil/GrowingPlant.java:74), so an out-of-range stage would
  crash the client sprite build; the server must only emit stage values in
  `[0, stages)` for the resource.
- The server, not the resource, owns growth timing; the resource only says how
  many stages exist. Persist (gob id, resource, position, stage byte, planted-
  at/growth-due timestamps) for every crop.
- The same "stage in sdt" pattern is reused by other staged objects (e.g.
  silkworm growth is surfaced through the item widget's `num` attribute,
  src/union/jsbot/JSItem.java:120-126, src/haven/Item.java:475-477 - items use
  the widget `num` uimsg where world gobs use sdt).

## Item drops and liftable objects

Client-side item handling distinguishes two worlds: **inventory widgets**
(server-created `Item` widgets inside `Inventory`/`Equipory` windows) and
**world gobs**. The flows, all verifiable in the client:

- **Take to cursor** (lift): left-click on an inventory item sends
  `wdgmsg("take", c)` (src/haven/Item.java:496-505); the item then follows the
  mouse cursor client-side (`isDragging`).
- **Drop to world**: while dragging, clicking the map view reaches
  `MapView.drop` -> `wdgmsg("drop", modflags)` (src/haven/MapView.java:2091-2094);
  a `drop` on an inventory cell sends `wdgmsg("drop", cell)`
  (src/haven/Inventory.java:100-102). The server must then spawn (or move) a
  world gob representing the dropped item; the dropped-item gob is rendered
  like any terobj. (The mapping of an item resource to its dropped-gob
  resource is server data - see Open questions.)
- **Transfer between inventories**: `wdgmsg("transfer", c)`
  (src/haven/Item.java:499-500) moves an item between two server-backed
  widget containers (inventory windows, stockpile boxes, equipory). Mass
  variants: `transfer_such_all`, `drop_such_all`, `transfer_such_all_ql`,
  `transfer_such_all_qldesc` (src/haven/Item.java:507-515; aggregation logic
  in src/haven/Inventory.java:116-176).
- **Item-on-world interaction**: dragging an item and right-clicking a gob
  sends mapview `itemact` with (screen coord, map coord, modflags, clicked gob
  id, gob position) (src/haven/MapView.java:2096-2107; bot wrapper
  src/union/jsbot/JSGob.java:184-190); the server resolves "use item on
  object". Right-clicking with an empty cursor is the plain interaction
  `click` flow (src/haven/MapView.java:720-740).
- **Visual of carried items on players**: the avatar layer set includes
  carrying poses; `Avatar.isCarrying()` checks the `arm/carrying` image
  (src/haven/Avatar.java:51-55) and bots detect carry-over-head via
  `gfx/borka/.../arm/banzai/` layers (src/union/jsbot/JSGob.java:216-223). The
  server should add the appropriate layer(s) when a character is carrying a
  visible object (e.g. a bucket) and remove them on put.

The practical bot scripts wrap exactly these flows (`take`, `drop`,
`transfer`, `itemact`, `iact` in src/union/jsbot/JSItem.java:149-209), which
makes them a reliable checklist of the lift/put command surface a server must
accept.

## Inventory widgets vs world gobs (what exists where)

- Inventory/equipment items are widgets created by the server through the
  widget protocol (`RMSG_NEWWDG = 0`, `RMSG_WDGMSG = 1`, `RMSG_DSTWDG = 2`,
  src/haven/Message.java:33-35; widget creation plumbing in
  src/haven/UI.java:169 and replies in src/haven/UI.java:257-265). Their state
  (resource, quality, count, meter, tooltip) is widget uimsg state (`num`,
  `chres`, `color`, `tt`, `meter`, src/haven/Item.java:475-494).
- World objects are gobs in `OCache` with the wire state described in
  objects-and-dynamics.md. An object can exist in *both* presentations at
  once only in the sense that a container gob (say a chest) has a widget
  window opened on top; the container's contents live in the widget tree and
  the container's world presence in the gob.
- Inventory cells are 31-32 px; item coordinates inside an inventory are
  `c.div(31)` (src/haven/Item.java:129-139, and the invisible-cell texture
  `invsq` sized 32x32 at src/haven/Inventory.java:48-49). Slot occupancy is
  server-authoritative; the client only reflects it.

## Containers and stockpiles

- `ISBox` is the stockpile box widget: the server creates it with
  (resource name, remaining, available, bi) and the client displays
  `rem/av/bi` (src/haven/ISBox.java:44-63). Taking one item is `wdgmsg("click")`,
  putting one back is `wdgmsg("xfer")` (src/haven/ISBox.java:84-103). The
  numbers live server-side; the widget is a view.
- `Equipory` routes per-slot item commands with the slot index `ep`:
  `drop`/`take`/`itemact`/`transfer`/`iact`
  (src/haven/Equipory.java:144-170). Equipment state doubles as the avatar
  layer source (see objects-and-dynamics.md).
- Container *openness* is a session-level widget, not gob state: the server
  creates the window widget when the player opens the container gob and
  destroys it on close; contents never go through the gob wire state.

## Object health and destruction

- Gob health arrives as `OD_HEALTH` (uint8 0..4) into `GobHealth`
  (src/haven/OCache.java:323-328). Undamaged is 4; the client red-tints
  anything below 4 (`HpFx`, src/haven/GobHealth.java:40-56) and
  `Gob.getHealth()` reports `hp/4*100` (src/haven/Gob.java:154-160).
- Trees, animals, players and structures all use the same attribute; the
  distinction is behavioral and lives in resources/server logic. Destruction
  = removal entry (plus, where the game wants it, a one-shot overlay or a
  resource change to a "wreck" resource before removal).
- Player death is observable as the dead-pose avatar layer
  (src/union/jsbot/JSGob.java:240-246); combat state otherwise lives in the
  fight widget, not the gob.

## Aggro hints and crime clues

- **Crime clue gobs**: server-spawned gobs with resource name containing
  `gfx/terobjs/clue`; their first overlay's `sdt` encodes the crime kind, and
  the fork client filters them by it (src/haven/MapView.java:1659-1670). The
  fork's `KerriUtils` maps specific sdt values to crimes (trespass 52465,
  theft 53185, vandalism 62977, assault 53057, battery 62529, murder 32961,
  src/union/KerriUtils.java:178-212). These magic numbers are fork-specific
  observations, not protocol constants; treat the mechanism (clue gob +
  overlay sdt) as authoritative and the values as approximate.
- **Combat relations**: the fight widget tracks per-opponent relations
  (`Relation` with `gob_id`, offence/defence meters, src/haven/Fightview.java:56-90,
  updates at src/haven/Fightview.java:238-285). Aggro from animals and NPCs is
  expressed through movement (`OD_HOMING` onto the victim) plus, in legacy,
  the combat initiation flow; the behavioral rules (which animals attack,
  trespass reactions, domestication calming) belong to
  ../livestock/animals-and-husbandry.md.
- **Kinship labeling** (`OD_BUDDY`, src/haven/OCache.java:330-344) is what
  lets clients color enemies vs village mates; `type & 2` marks same-village
  (src/haven/Gob.java:351-356, src/haven/KinInfo.java:54-90). A server must
  re-evaluate these labels per observer (law/claim context).

## Persistence categories for a server implementation

Everything the client shows must survive a server restart; the client gives a
complete inventory of what that is:

1. **Terrain** - map grids (100x100 tiles of 11x11 units, tile ids + overlay
   bits; see src/haven/MCache.java:53-54 and the grid decoder
   src/haven/MCache.java:397-515) plus tileset bindings (`RMSG_TILES = 11`,
   src/haven/Message.java:44). Claim/plot overlays arrive *with* grid data
   (src/haven/MCache.java:480-501).
2. **Player structures** (terobjs): any gob with a resource, position, `sdt`
   content (e.g. stage, fill level), health, and optionally lumin/overlays.
3. **Containers and their contents**: stockpile counts (ISBox state) and full
   inventories (per-slot item resource, quality, count, meter); container
   gobs themselves persist as category 2.
4. **Planted crops**: gob + stage byte + growth timers (see "Growth stages").
5. **Animals**: gob + resource + movement state + health + domestication
   state; position may be rebuilt from spawn data but a server should persist
   last position.
6. **Item drops**: gobs whose resources are item-derived; persist position,
   resource, quality/count.
7. **Claims, kinship and law state** (village membership, buddy groups,
   claim rectangles) - they parameterize what the server streams (`OD_BUDDY`
   labels, plot overlay bits).
8. **Session-global state** pushed at login and on change: `GMSG_TIME`,
   `GMSG_ASTRO`, `GMSG_LIGHT` (src/haven/Glob.java:36-38, 108-128),
   character attributes `RMSG_CATTR` (src/haven/Glob.java:147-162), buffs
   (src/haven/Glob.java:164-174).

Things that must *not* be persisted as world state: widget layout, the
client-local flavor gobs (regenerable from tilesets), placement previews, and
anything the client invents (`plob`, id 0/-1 gobs).

## Server implementation notes

- Maintain a per-observer visibility index keyed by gob id; on enter send the
  full description, on change send deltas, on exit send the removal entry;
  retransmit until acked (see objects-and-dynamics.md "Object
  acknowledgement").
- Enforce frame monotonicity per gob; on removal write a tombstone so a
  reused id cannot resurrect an object at an older frame
  (src/haven/OCache.java:96-116 semantics).
- Choose one removal encoding and stay consistent (flag-bit form implies the
  removal is effective at `frame - 1`, src/haven/Session.java:200-202).
- Stream objects for a radius at least matching the client's grid fetch
  window (about 45 tiles around the view center, derived from
  src/haven/MapView.java:1953-1954) and never leave fetched grids empty of
  objects that should be there; pick a concrete radius policy and document it.
- Spawn crops, drops and structures as ordinary gobs whose `sdt` carries
  their small state; resend `OD_RES` on any sdt change (replace rule,
  src/haven/OCache.java:125-133).
- Accept and implement the full lift/put command surface: `take`, `drop`,
  `transfer`, `drop_such_all`, `transfer_such_all*`, `iact`, `itemact` on
  item widgets; mapview `click`, `place`, `drop`, `itemact` for world
  interaction (file:line references throughout this document).
- Keep flower-menu options server-driven per object state (they are plain
  strings sent with the `sm` widget, src/haven/FlowerMenu.java:44-54); the
  client renders whatever options the server sends
  (src/haven/FlowerMenu.java:190-208).
- Persist by the categories above; treat (resource, sdt) as the minimal
  visual identity and attrs as rebuildable runtime state.

## Open questions

- **Exact server view radius and shape** for the legacy server: not encoded
  in the client. Determine empirically by logging `OD_REM` timing while
  flying across the map on the official legacy server, or from a community
  emulator's config; the client-side bound is the +/-500 unit grid window
  (about +/-45 tiles, src/haven/MapView.java:1953-1954).
- **Dropped-item gob resources**: the client never maps item resources to
  their ground-gob equivalents; the server data must define it (likely a
  naming convention in the resource tree). Determine by dropping each item on
  a legacy capture and logging the spawned gob resource.
- **Removal-entry convention actually used** by the legacy server (flag-bit
  tombstone vs explicit `OD_REM`, and whether removal entries carry trailing
  attributes): both are handled; capture traffic to learn which to emit for
  byte-compatibility.
- **Crime clue sdt encoding**: the exact bit layout behind the six magic
  values in src/union/KerriUtils.java:178-212 (they appear to be packed
  field+flag words, possibly including the actor's relation id) is unknown;
  decode from captures of actual crimes.
- **Whether the legacy server streams objects in grids it considers loaded**
  (i.e. grid-following visibility) or by pure distance: not observable from
  the client; decide as a design choice or recover from captures.
- **Flavor object parity**: flavor gobs are generated client-side from
  tilesets (src/haven/MCache.java:176-199); confirm that the legacy server
  truly has no server-side flavor objects of the same resources (they would
  be indistinguishable in captures only by their id being -1 absence).
