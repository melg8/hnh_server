# Objects and Dynamics: The Gob Model and Per-Object State Protocol

> **Sources:** src/haven/Gob.java, src/haven/OCache.java, src/haven/Session.java, src/haven/Message.java, src/haven/Moving.java, src/haven/LinMove.java, src/haven/Homing.java, src/haven/Following.java, src/haven/DrawOffset.java, src/haven/Lumin.java, src/haven/Speaking.java, src/haven/KinInfo.java, src/haven/GobHealth.java, src/haven/ResDrawable.java, src/haven/Layered.java, src/haven/Avatar.java, src/haven/Drawable.java, src/haven/GAttrib.java, src/haven/Resource.java, src/haven/resutil/GrowingPlant.java, src/haven/MapView.java, src/haven/BuddyWnd.java, src/union/jsbot/JSGob.java, upstream dolda2000/hafen-client OCache.java/Session.java (compatibility cross-check), Ring of Brodgar wiki (Legacy: namespace)

## Summary

Every world object in Haven and Hearth, legacy protocol, is a single client-side
record called a **gob** (`Gob`, src/haven/Gob.java). The server streams per-gob
update entries to the client inside `MSG_OBJDATA` datagrams (type 6, see
src/haven/Session.java:42). Each entry carries a gob id, a per-gob monotonically
increasing **frame** number, and a list of typed attribute sub-messages
(`OD_*`, src/haven/Session.java:45-61) terminated by `OD_END`. A gob's position,
appearance, movement, speech, light, status overlays, health and kinship label
are all dynamic attributes layered on top of the gob; the object's *identity*
(i.e. what it is) is a reference to a shared **resource** identified by a 16-bit
id, optionally with a server-defined state blob (`sdt`) used for things like
plant growth stage.

This document is the normative reference for a Rust server reimplementation of
the object layer: the exact wire encoding of every `OD_*` sub-message, the
client-side semantics the server can rely on (interpolation constants, replace
rules, ack behavior), and the invariants the server must maintain (frame
ordering, removal tombstones, ack-gated retransmission).

All numeric constants quoted below were verified in this repository's client
source; line numbers refer to the files as checked in here.

## The gob object model (src/haven/Gob.java)

A `Gob` is:

- `rc` (real coordinate, absolute map position in tile-fraction units) and `sc`
  (screen coordinate, render-time only) - src/haven/Gob.java:33.
- `id` (int32, server-assigned object id) and `frame` (int32, per-gob update
  sequence number assigned by the server) - src/haven/Gob.java:35.
- `attr`, a map of dynamic attributes keyed by the attribute *family* - the
  direct `GAttrib` subclass (`attrclass` walks up the class hierarchy to the
  direct subclass of `GAttrib`, src/haven/Gob.java:208-215), so there is at
  most one attribute per family: installing a `LinMove` on a gob that is
  currently `Following` replaces it, because both key to `Moving`. See
  `setattr`/`getattr`/`delattr` at src/haven/Gob.java:217-231. The attribute
  families (all verified via `extends GAttrib`/`extends Moving`/`extends
  Drawable` declarations) are: `Drawable` (concrete `ResDrawable` and
  `Layered`), `Moving` (concrete `LinMove`, `Homing`, `Following`),
  `Speaking`, `Lumin`, `DrawOffset`, `Avatar`, `GobHealth`, `KinInfo`
  (src/haven/GAttrib.java and the concrete files listed in Sources).
- `ols`, an unordered list of `Gob.Overlay` (src/haven/Gob.java:38 and 51-68);
  overlays are addressed by an integer id (`findol`, src/haven/Gob.java:146-152).
- `initdelay`, a random value in `[0, 3000)` ms consumed once by the first
  `ctick` to desynchronize animation of identical resources (src/haven/Gob.java:35
  and 108-110). Server implication: clients deliberately render identical gobs
  out of phase; do not try to synchronize animation phases from the server.

Key behaviors a server implementer must know:

- `Gob.position()` returns `Moving.getc()` if the gob currently has a `Moving`
  attribute, else `rc` (src/haven/Gob.java:200-206). The authoritative position
  of a moving gob is therefore *derived by the client*, not stored; the server
  only sends movement descriptors.
- `Gob.move(Coord)` delegates to the current `Moving` attribute and then
  overwrites `rc` (src/haven/Gob.java:189-194); `LinMove.move()` is a no-op, but
  `Homing.move()` resets its traveled distance to 0 (src/haven/Homing.java:52-54),
  so an `OD_MOVE` while homing re-anchors the homing progress.
- `Gob.drawoff()` combines `DrawOffset.off` and `Following.doff` (src/haven/Gob.java:233-242).
- `Gob.GetBlob(index)` / `getBlob()` expose the `sdt` blob of the `ResDrawable`
  (src/haven/Gob.java:162-182); bots and UI code read object state (e.g. growth
  stage, content counters) from these bytes.
- `Gob.getHealth()` returns `GobHealth.asfloat() * 100` or -1 when the gob has
  no health attribute (src/haven/Gob.java:154-160).

## Protocol layer: MSG_OBJDATA entries (src/haven/Session.java)

Transport constants (src/haven/Session.java:34-44): `PVER = 2`,
`MSG_SESS = 0`, `MSG_REL = 1`, `MSG_ACK = 2`, `MSG_BEAT = 3`,
`MSG_MAPREQ = 4`, `MSG_MAPDATA = 5`, `MSG_OBJDATA = 6`, `MSG_OBJACK = 7`,
`MSG_CLOSE = 8`.

Object updates are *not* wrapped in the sequenced reliable stream
(`MSG_REL`); they arrive as independent `MSG_OBJDATA` datagrams
(src/haven/Session.java:471-474). Reliability for objects is therefore
end-point supplied: the client acknowledges every gob entry and the server is
expected to retransmit unacked state (see "Object acknowledgement" below).

A `MSG_OBJDATA` datagram contains zero or more gob entries back to back
(`getobjdata`, src/haven/Session.java:194-331). Each entry:

```
uint8   fl        flag byte
int32   id        gob id
int32   frame     per-gob update frame
<OD_* sub-messages> ...
uint8   OD_END    terminator (255)
```

Flag semantics observed in this client (src/haven/Session.java:197-202):

- `fl & 1` - the object was removed at `frame - 1`. The client calls
  `OCache.remove(id, frame - 1)` immediately, *before* parsing the remaining
  attribute list. The client parses an attribute list after the flag regardless
  of whether one is present, so a bare `fl OD_END` removal and a removal with
  trailing attributes are both tolerated; which one the legacy server emits is
  convention (see Open questions in visibility-and-lifecycles.md).
- The upstream (modern) client additionally defines `fl & 2` = "virtual gob"
  and `fl & 4` = "old attributes" (upstream OCache.java `ObjDelta.fl`); the
  legacy client ignores these bits. A legacy-protocol server must send 0 or 1
  only.

After the attribute list, the client stamps `Gob.frame = frame`
(src/haven/Session.java:311-313) and records an acknowledgement
(src/haven/Session.java:315-326). Any `OD_*` type outside the table below
raises `MessageException("Unknown objdelta type")` (src/haven/Session.java:306-308);
the client cannot skip unknown sub-messages, so the server must not invent
sub-message types for this protocol version.

## OD_* attribute sub-message reference

Constants (src/haven/Session.java:45-61): `OD_REM = 0`, `OD_MOVE = 1`,
`OD_RES = 2`, `OD_LINBEG = 3`, `OD_LINSTEP = 4`, `OD_SPEECH = 5`,
`OD_LAYERS = 6`, `OD_DRAWOFF = 7`, `OD_LUMIN = 8`, `OD_AVATAR = 9`,
`OD_FOLLOW = 10`, `OD_HOMING = 11`, `OD_OVERLAY = 12`,
(`OD_AUTH = 13` is commented out - removed), `OD_HEALTH = 14`,
`OD_BUDDY = 15`, `OD_END = 255`.

Payload layouts as parsed at src/haven/Session.java:204-310:

| Type | Name | Payload | Client handler (src/haven/OCache.java) |
|------|------|---------|----------------------------------------|
| 0 | OD_REM | none | `remove(id, frame)` (line 47) deletes the gob |
| 1 | OD_MOVE | `coord c` | `move(id, frame, c)` (line 118) |
| 2 | OD_RES | `uint16 resid`, if `resid & 0x8000`: `uint8 n` + `n` bytes sdt | `cres(id, frame, res, sdt)` (line 125) |
| 3 | OD_LINBEG | `coord s`, `coord t`, `int32 c` | `linbeg(...)` (line 160) |
| 4 | OD_LINSTEP | `int32 l` | `linstep(...)` (line 170) |
| 5 | OD_SPEECH | `coord off`, `string text` | `speak(...)` (line 193) |
| 6 | OD_LAYERS | `uint16 base`, layer list (below) | `layers(...)` (line 210) |
| 7 | OD_DRAWOFF | `coord off` | `drawoff(...)` (line 234) |
| 8 | OD_LUMIN | `coord off`, `uint16 sz`, `uint8 str` | `lumin(...)` (line 251) |
| 9 | OD_AVATAR | layer list (below) | `avatar(...)` (line 222) |
| 10 | OD_FOLLOW | `int32 oid`; if `oid != -1`: `int8 szo`, `coord off` | `follow(...)` (line 258) |
| 11 | OD_HOMING | `int32 oid`; `oid == -1`: stop; `oid == -2`: `coord tc`, `uint16 v` (coord update); else: `coord tc`, `uint16 v` | `homostop/homing/homocoord` (lines 277-300) |
| 12 | OD_OVERLAY | `int32 olid`, `uint16 resid`; `prs = olid & 1`, `olid >>= 1`; `resid == 65535` removes; else if `resid & 0x8000`: `uint8 n` + `n` bytes sdt | `overlay(...)` (line 302) |
| 14 | OD_HEALTH | `uint8 hp` (0..4) | `health(...)` (line 323) |
| 15 | OD_BUDDY | `string name`, `uint8 group`, `uint8 btype` | `buddy(...)` (line 330) |
| 255 | OD_END | none | terminates the sub-message loop |

The `OD_LAYERS` / `OD_AVATAR` layer list (src/haven/Session.java:233-247) is a
sequence of `uint16` resource ids terminated by the sentinel `65535`.

There is no separate "OAttrs" message type in this legacy fork. Dynamic
per-object state arrives in exactly three places: (a) the `sdt` blob of
`OD_RES` (growth stage, content counters, per-resource state), (b) the typed
`OD_*` attributes above, and (c) gob overlays with `sdt`. The modern upstream
client added `OD_ICON`, `OD_RESATTR`, `OD_CMPPOSE`, `OD_CMPMOD`, `OD_CMPEQU`
(upstream OCache.java lines 52-57) and replaced `OD_LAYERS`/`OD_DRAWOFF` with
`OD_COMPOSE`/`OD_ZOFF` (same numeric slots 6/7) - a Rust server targeting this
legacy client must use the *legacy* meanings of slots 6 and 7.

Per-attribute client semantics the server must respect:

- **Replace-or-clear.** Every handler except overlays either creates/updates
  the attribute or deletes it when a sentinel value arrives: empty speech text
  removes `Speaking` (src/haven/OCache.java:197-199), `OD_DRAWOFF` with
  `Coord(0,0)` removes `DrawOffset` (lines 238-240), `oid == -1` in
  `OD_FOLLOW` removes `Following` (lines 262-263), `oid == -1` in `OD_HOMING`
  removes `Homing` (lines 277-282), an all-zero `OD_BUDDY` tuple
  (empty name, group 0, type 0) removes `KinInfo` (lines 334-336). `OD_LUMIN`,
  `OD_RES`, `OD_MOVE`, `OD_LINBEG` and `OD_HEALTH` always overwrite.
- **Resource change detection.** `cres` (src/haven/OCache.java:125-133) only
  rebuilds the `ResDrawable` when the resource differs or when either the old
  or the new `sdt` is non-empty. Server rule: to mutate `sdt` state (plant
  stage, contents), resend `OD_RES` with the new blob; the client will
  recreate the sprite.
- **Frame guard.** `getgob(id, frame)` (src/haven/OCache.java:96-116) refuses
  to resurrect a gob deleted at a frame >= the incoming frame (see
  visibility-and-lifecycles.md). Updates must therefore always carry
  increasing frames per gob.

## Full sync vs delta updates

The legacy wire format has a single entry type; the distinction between a
"full object description" (the classic `objdata` full sync) and a delta
(`objdelta`) is structural:

- A **full sync** entry contains `OD_RES` (what the object is, plus `sdt`)
  and normally `OD_MOVE` (where it is), plus any currently active attributes
  (`OD_LINBEG`/`OD_FOLLOW`/`OD_OVERLAY`/`OD_LUMIN`/`OD_AVATAR`/`OD_HEALTH`/
  `OD_BUDDY`), then `OD_END`. This is what the server sends when a gob enters
  the client's visibility set, and after any client session event that could
  have lost state.
- A **delta** entry contains only the attributes that changed. The client
  applies them on top of the existing `Gob`. Nothing in the entry marks it as
  "full" or "delta"; the client simply applies whatever is present, which is
  why partial deltas work and why the server must resend a full description
  whenever it cannot trust that the client holds prior state (unacked frames,
  new client, gob re-entering view).

A remove entry (flag bit 0, or an `OD_REM` attribute) carries no object state
that the client keeps; the gob is deleted and the frame recorded as a
tombstone (src/haven/Session.java:200-208, src/haven/OCache.java:47-52).

## Object acknowledgement (MSG_OBJACK)

For every gob entry received, the client records an `ObjAck(id, frame, recv)`
(src/haven/Session.java:140-152, 315-326). The writer thread flushes them:

- An ack is (re)sent if more than 200 ms have elapsed since it was last sent
  (src/haven/Session.java:743-744).
- An ack entry is dropped once more than 120 ms have elapsed since it was
  received (src/haven/Session.java:745-746).
- Acks are batched into one `MSG_OBJACK = 7` datagram of repeated
  `int32 id`, `int32 frame` pairs (src/haven/Session.java:747-753); the
  scheduler wakes the writer within 200 ms whenever unacked gob entries exist
  (src/haven/Session.java:692-695).

Server design consequence: the server must keep a per-client, per-gob
"last acked frame" and retransmit the newest full or delta state of any gob
whose latest frame has not been acked, at a similar cadence. The reliable
widget stream uses a retransmit ladder of 0/80/200/620/2000 ms by retry count
(src/haven/Session.java:712-733); a comparable backoff is a sane model for
object retransmission. When no ack arrives for a gob for a long time the
server should treat the object as unobserved (cache it, stop streaming) rather
than queueing updates indefinitely. Keepalive: when the client has nothing to
send it emits `MSG_BEAT = 3` at most every 5 s (src/haven/Session.java:771-776);
the server should use per-client silence timeouts consistent with this.

## Resource binding and the sdt state blob

`OD_RES` binds a gob to a resource. The 16-bit `resid` is resolved through the
server-pushed resource table: the server announces `RMSG_RESID = 6`
(src/haven/Message.java:39) with `uint16 resid, string resname, uint16 resver`
(src/haven/Session.java:352-358), after which the client loads the resource by
name and version. Resources are content packages containing typed layers:
`imgc` (images), `negc` (hitbox/negative space), `animc`, `tile`,
`tileset`, `pagina`, `action` (`AButton` - the action-menu button descriptor
consumed by the menu grid, src/haven/MenuGrid.java:100), `audio`,
`tooltip` (src/haven/Resource.java:52-60 and the layer classes at
src/haven/Resource.java:621-830).

The `Resource.Neg` layer defines the object's footprint in map coordinates:
`cc` (center), `bc`/`bs` (bounding box origin and size, converted from screen
to map space with `MapView.s2m` at parse time), `sz` and `ep` (attachment
points) - src/haven/Resource.java:755-783. Bots use `Gob.getneg().bs` as the
hitbox size (src/union/jsbot/JSGob.java:64-68). A server implementation needs
the same data to place objects and validate placement.

The `sdt` blob is opaque to the protocol; its meaning is defined per resource
by the sprite factory that consumes it. The canonical example is the growth
stage of plants (see visibility-and-lifecycles.md). Client code also reads the
blob generically via `Gob.GetBlob` (src/haven/Gob.java:162-170).

## Movement system

The client has exactly three concrete `Moving` attributes. All of them compute
position from descriptors sent by the server; the server never streams
per-tick positions.

### Absolute teleport: OD_MOVE

`OD_MOVE` carries one absolute `coord` and sets `rc` (src/haven/OCache.java:118-123,
src/haven/Gob.java:189-194). For a gob with a `LinMove` the move only changes
`rc` (the `LinMove` stays and keeps interpolating from its own `s`/`t`), while
for a `Homing` it zeroes the accumulated distance, effectively restarting the
pursuit from the new position (src/haven/Homing.java:52-54). `OD_MOVE` on a
stationary gob is the standard "put object at x".

### Linear movement: LinMove via OD_LINBEG / OD_LINSTEP

`linbeg` installs `new LinMove(g, s, t, c)` (src/haven/OCache.java:160-168),
replacing any previous `Moving` attribute (attributes are one-per-class).

`LinMove` state (src/haven/LinMove.java:29-48): start point `s`, target `t`,
total step count `c`, progress fraction `a` in [0, 1].
`getc() = s + (int)((t.x - s.x) * a), (int)((t.y - s.y) * a)`.

Time model (src/haven/LinMove.java:57-62):

```
da = (dt / 1000) / (c * 0.06)
a += da * 0.9
a  = min(a, 1)
```

Interpretation: the move is defined to take `c * 0.06` seconds total, i.e. one
step lasts 60 ms and `c` is the number of steps. The client advances `a` at
90% of that rate (the `0.9` damping keeps the client slightly behind the
server so that authoritative `LINSTEP` markers never move it backwards) and
clamps at 1 so the gob always arrives at `t` even if no further packets come.

`OD_LINSTEP` carries `l`, the authoritative step index completed by the
server. Client behavior (src/haven/OCache.java:170-191 and
src/haven/LinMove.java:64-68):

- `l < 0` or `l >= c`: the movement ends, the `Moving` attribute is deleted,
  and the gob rests at its current interpolated position (the server should
  follow with an `OD_MOVE` to snap the final position).
- Otherwise `setl(l)` sets `a = l / c` but *only forward* (`if (a > this.a)`),
  so stale or duplicate `LINSTEP` packets cannot rewind the client.

Server model implied by this code: the server simulates movement in discrete
60 ms steps; it announces intent once with `OD_LINBEG` and then confirms
progress with `OD_LINSTEP(l)` per step (or in batches; the client interpolates
between markers). Because the client's own clock drifts, periodic `LINSTEP`
markers are the error-correction mechanism. Since `MSG_OBJDATA` is unreliable,
every `LINBEG` and `LINSTEP` must be retransmitted under the object-ack
discipline until acked.

### Pursuit: Homing via OD_HOMING

`homing(id, frame, oid, tc, v)` installs `Homing(tgt = oid, tc, v)`
(src/haven/OCache.java:284-289). Three encodings share one type byte
(src/haven/Session.java:263-275):

- `oid == -1`: stop homing (`homostop`, src/haven/OCache.java:277-282).
- `oid == -2`: coordinate-retarget only (`homocoord`, lines 291-300): update
  `tc` and `v` on an existing `Homing`; ignored if the gob is not homing.
- otherwise: home onto gob `oid`; `tc` is the fallback/final target coordinate
  and `v` the speed.

`Homing` (src/haven/Homing.java:29-59) steps the gob toward the *current*
position of the target gob if it is visible (`tgt.rc`), else toward `tc`:

```
per ctick: dist += ((dt / 1000) / 0.06) * 0.9 * (v / 100)
getc() = rc + unit(target - rc) * dist
```

So `v` is speed in map units per 60 ms tick, expressed in hundredths
(`v/100`). `OD_MOVE` resets `dist` to 0 (re-anchoring). The client never stops
a `Homing` by itself; the server must send the `oid == -1` encoding on arrival
and typically follows with an `OD_MOVE`. Homing is the mechanism behind animal
chase and NPC pursuit; see ../livestock/animals-and-husbandry.md for the
behavioral side.

### Attachment: Following via OD_FOLLOW

`follow(id, frame, oid, off, szo)` glues the gob to target gob `oid`
(src/haven/OCache.java:258-275); `oid == -1` removes the attribute.
`Following.getc()` returns the target gob's current position
(src/haven/Following.java:41-47), and `doff` is added as a draw offset
(src/haven/Gob.java:238-240). `szo` is an int8 payload read by the parser
(src/haven/Session.java:254-262) but unused by this client's renderer beyond
storage (src/haven/Following.java:29-39). Use cases: riders and boat
passengers (the passenger gob follows the vehicle gob), items strapped to
animals, and similar "physically attached" states. A following gob's *logical*
position for click purposes is the target's position; the server should treat
the pair as one collision/placement unit.

### Draw offsets: OD_DRAWOFF

`drawoff(id, frame, off)` sets or clears (off == 0,0) a `DrawOffset`
(src/haven/OCache.java:234-249). The offset shifts where the gob's sprites are
drawn relative to its logical position (src/haven/Gob.java:233-242). A
plausible legacy use is pose correction on player gobs (sitting/lying offsets
relative to the seat/bed gob); the exact original uses are unobserved and
listed in Open questions.

## Speech (OD_SPEECH)

`speak(id, frame, off, text)` shows a speech bubble anchored at `off` relative
to the gob (src/haven/OCache.java:193-208). An empty string removes the
attribute; a new text on an existing `Speaking` updates in place
(src/haven/Speaking.java:47-49). The bubble is pure UI (rendered from
`gfx/hud/emote` boxes, src/haven/Speaking.java:40-42) and expires only when
the server sends empty text or the gob is removed - the server must schedule
bubble expiry itself.

## Overlays (status effects) via OD_OVERLAY

Overlays are sprite effects drawn over/around the gob: status effects, combat
flashes, carry indicators, crime clues. Wire format
(src/haven/Session.java:276-295):

- `olid` is an int32 whose bit 0 is the "persistent" flag; the id is
  `olid >> 1`. Ids are chosen by the server and must be unique per gob
  (`Gob.findol`, src/haven/Gob.java:146-152).
- `resid == 65535` removes the overlay with the given id.
- Otherwise the overlay is resource `resid` (bit 15 = has `sdt`, same
  convention as `OD_RES`), and the existing overlay is replaced when its
  `sdt` differs (src/haven/OCache.java:302-321).

Persistence semantics (src/haven/Gob.java:117-128): a one-shot overlay's
sprite reports completion and is dropped automatically; an overlay created
with the persistent bit sets `delign = true` and is *kept* until the server
sends an explicit removal, unless its sprite implements `Gob.Overlay.CDel`
(custom deletion animation, src/haven/Gob.java:65-67). Server rule: use the
persistent bit for ongoing states (e.g. riding, sleeping, highlighted-by-game
marks) and one-shot (no bit) for transient animations. Removing an overlay
whose sprite implements `Gob.Overlay.CDel` triggers the sprite's own
`delete()` fade-out instead of instant removal
(src/haven/OCache.java:316-319).

Overlay `sdt` carries per-instance parameters; for example the crime "clue"
gobs (`gfx/terobjs/clue`) encode the crime type in the first overlay's sdt
(see visibility-and-lifecycles.md). The debug command `plol` also shows that
an overlay is identified by (resource, sdt) pairs (src/haven/MapView.java:2133-2146).

## Light sources via OD_LUMIN

`lumin(id, frame, off, sz, str)` installs `Lumin(off, sz, str)` with `sz`
(uint16) the light radius and `str` (uint8) the strength
(src/haven/OCache.java:251-256, src/haven/Lumin.java:29-39). Every
`OD_LUMIN` replaces any previous light on the gob. Lights are composited with
the global ambient light pushed via the glob blob (`RMSG_GLOBLOB = 4`
sub-message `GMSG_LIGHT = 2`, src/haven/Glob.java:36-38 and 108-128), which is
also how day/night interacts with lamp objects. A flickering light (e.g. a
torch) is produced by the server sending periodic `OD_LUMIN` updates; there is
no client-side flicker simulation.

## Avatar and layered composition via OD_AVATAR / OD_LAYERS

- `OD_AVATAR` sets the `Avatar` attribute: an unordered list of equipment
  layer resources terminated by `65535` (src/haven/OCache.java:222-232,
  src/haven/Avatar.java:63-68). The client detects special states by layer
  name: `isPlayer()` is "no `kritter` image" (i.e. not an animal-shaped
  body), `isCarrying()` looks for `arm/carrying`, `isSitting()` for
  `body/sitting` (src/haven/Avatar.java:38-61); the bot API additionally
  detects carrying-over-head and death by `gfx/borka/body/...` layer names
  (src/union/jsbot/JSGob.java:216-246). Server rule: avatar layer sets are the
  authoritative rendering of equipment and pose; changing equipment or pose
  means re-sending the whole layer list.
- `OD_LAYERS` is the generic composited drawable (`Layered`): a base resource
  plus layers (src/haven/OCache.java:210-220, src/haven/Layered.java:141-159).
  It is the legacy encoding for player characters (base body + equipment
  sprites). `Layered.setlayers` sorts the list and rebuilds sprites when the
  list changes.

The `mapview` widget creation carries the player's own gob id as widget
argument 2 (src/haven/MapView.java:123-135); the server must emit it once at
session start so the client can bind `playergob` (used for camera and
`OCache.isplayerid`, src/haven/OCache.java:151-158).

## Kinship labels via OD_BUDDY

`buddy(id, frame, name, group, type)` shows a name label above the gob
(src/haven/OCache.java:330-344). `group` selects one of 8 name colors
(`BuddyWnd.gc`, src/haven/BuddyWnd.java:48-52); `type` is a bitfield in which
bit 2 (`type & 2`) marks a member of the viewer's village - the client shows
the `gfx/hud/vilind` icon and `Gob.isInYourVillage()` tests it
(src/haven/KinInfo.java:54-90, src/haven/Gob.java:351-356). An all-zero tuple
removes the label. The server decides, per viewer, which kinship tier to
reveal; this is also the channel through which combat-relevant kinship (party,
village) is communicated to the client.

## Object health via OD_HEALTH

`health(id, frame, hp)` sets `GobHealth` with `hp` in 0..4
(src/haven/OCache.java:323-328, src/haven/GobHealth.java:31-61). `hp == 4` is
undamaged (no red tint); `hp < 4` tints the sprite progressively red
(`HpFx`, alpha `128 - (hp * 128) / 4`, src/haven/GobHealth.java:40-56);
`asfloat() = hp / 4`. The same attribute covers animals, players and damageable
structures; the client shows a health bar for the player gob and a tint for
everything else. Gob health is distinct from the player HP meters (widget
protocol) and from item wear (item widget attributes).

## Client-local gobs (must not be streamed as objects)

Two gob populations exist purely client-side; a server must not simulate or
re-send them:

- **Flavor objects**: decorative per-tile objects (`id == -1`) generated from
  tileset `flavobjs` with position-seeded randomness when a map grid is
  decoded (src/haven/MCache.java:176-199, position-seeded randomness via
  `MCache.mkrandoom`, src/haven/MCache.java:241).
- **Placement preview ("plob")**: while the server has sent the mapview a
  "place" uimsg (resource, version, snap-to-tile flag, optional placement
  radius), the client shows a ghost gob (id 0, hence the id-0 render filter at
  src/haven/MapView.java:1655) and confirms with a `place` wdgmsg
  carrying the chosen position (src/haven/MapView.java:877-901 and 695-699).
  Known radii used by the client's radius renderer: minesupport 100, beehive
  150 (src/haven/MapView.java:609-611).

## Server implementation notes

- **Object store.** Keep one authoritative record per gob: `id`, current
  `frame` (uint32, strictly increasing per gob), resource id, `sdt`, position,
  movement descriptor, and the set of active attributes (speaking, lumin,
  overlays, health, buddy, avatar/layers, following, draw offset).
- **Delta vs full.** Send a full description on spawn and whenever the
  observer may lack state (unacked frames, observer re-entry, after teleport);
  otherwise send minimal deltas. An entry with no attributes plus `OD_END`
  is legal but pointless; prefer only sending entries on change or for ack
  purposes.
- **Frame discipline.** Every entry for a gob must carry its current frame;
  increment the frame whenever any attribute changes so the ack gate works.
  Removal must use the tombstone conventions described in
  visibility-and-lifecycles.md.
- **Ack-gated retransmission.** Track per-observer acked frames from
  `MSG_OBJACK` batches; retransmit unacked gob state with backoff (the
  reliable-stream ladder 0/80/200/620/2000 ms in src/haven/Session.java:712-733
  is a good template) and give up/deprioritize after a bounded time.
- **Movement.** Implement movement as 60 ms steps; announce `OD_LINBEG` once,
  emit `OD_LINSTEP` markers as steps complete (never rewinding `l`), send
  `OD_MOVE` on arrival correction and use `OD_HOMING` for pursuit with `v` in
  map-units-per-tick * 100. Stop homing explicitly with the `oid == -1`
  encoding.
- **One attribute per class.** Because `setattr` replaces by class, the wire
  semantics for attributes are "last write wins". Overlays live in a separate
  list keyed by overlay id; never reuse an id for a different effect on the
  same gob.
- **Speech expiry, light flicker** are server-scheduled; nothing on the client
  times them out for you.
- **Resource table.** Push `RMSG_RESID` entries before first use in
  `OD_RES`/`OD_OVERLAY`/layer lists; resource identity is (name, version).
- **Numbers are protocol-critical.** Step length 0.06 s, damping 0.9, speed
  scaling `v/100`, sdt sentinel `0x8000`, layer sentinel `65535`, hp scale 0..4
  - all verified above; changing any of them desynchronizes clients.

## Open questions

- Exact legacy server retransmission schedule for unacked `MSG_OBJDATA`
  entries is not recoverable from the client (only the ack cadence
  200 ms / 120 ms is). Determine by instrumenting the official legacy server
  or a community emulator capture; until then use the reliable-stream ladder
  as a stand-in.
- Whether the legacy server batches multiple gob entries per datagram and how
  it fragments large `OD_LAYERS` lists near the ~64 KB datagram limit
  (receive buffer is 65536 bytes, src/haven/Session.java:418) is unobserved.
  A safe server caps entries per datagram by total size.
- The meaning of `Following.szo` (int8 in the wire format,
  src/haven/Session.java:259) in the original server: the legacy client stores
  but never renders it. Candidates: z-level draw priority or mount tier.
  Determine from a protocol capture of a mounted/riding player.
- Actual legacy uses of `OD_DRAWOFF` (which poses/objects carry a draw
  offset): recover from captures by logging `OD_DRAWOFF` alongside gob
  resources during sitting, sleeping and combat knockdown.
- Precise resource-id allocation policy of the legacy server (which ids map to
  which resource names per build) is data, not code; recover from the official
  resource pack index or by logging `RMSG_RESID` on a live session.
- The `OD_AUTH = 13` slot (commented "Removed" at src/haven/Session.java:58)
  implies an earlier authentication-per-object use; its historical payload is
  unknown and irrelevant to a reimplementation, but do not reuse 13.
- Overlay id allocation conventions of the legacy server (persistent ranges vs
  one-shot ranges) are unobserved; the client imposes only uniqueness per gob.
