# Legacy Haven and Hearth Network Protocol (session wire format)

> **Sources:** src/haven/Session.java, src/haven/Message.java, src/haven/Utils.java, src/haven/MCache.java, src/haven/Glob.java, src/haven/Party.java, src/haven/OCache.java, src/haven/RemoteUI.java, src/haven/UI.java, src/haven/Widget.java, src/haven/Resource.java, src/haven/AuthClient.java, src/haven/HackSocket.java, src/haven/SslHelper.java, http://legacy.havenandhearth.com/portal/doc-src

## Summary

This document is the authoritative wire-level specification of the legacy
Haven and Hearth session protocol, reverse-engineered from the open-source
Java client in `src/haven/`. It covers the datagram framing, the reliable
ordered message layer, every session-level message type (`MSG_*`), the
reliable sub-message types (`RMSG_*`), the object-delta sub-messages
(`OD_*`), the widget message protocol, and the session error codes. A Rust
server that implements exactly the layouts in this document can speak to an
unmodified legacy client. Authentication (TLS TCP) and resource transport
(HTTPS) are only summarized here; see
`docs/mechanics/network/session-lifecycle.md` for the full connection
lifecycle and auth handshake.

All integers on the wire are LITTLE-ENDIAN (least significant byte first),
as implemented by `Utils.uint16d` and `Utils.uint32d` (src/haven/Utils.java
lines 222-244). Strings are UTF-8 bytes terminated by a single zero byte
(`Utils.strd`, src/haven/Utils.java lines 266-278). There is no compression
at the session layer and no checksum; the first byte of every datagram is
the message type.

## Transport: one message per UDP datagram

The game session runs over UDP. The client binds an ephemeral port with a
plain `DatagramSocket` (src/haven/Session.java line 835) and sends every
packet to UDP port **1870** on the server
(`Session.sendmsg(byte[])`, src/haven/Session.java line 893). This is the
only game-protocol transport; do not confuse it with:

- The authentication channel: TLS-wrapped TCP to port **1871**
  (src/haven/AuthClient.java line 56), trusting the built-in
  `etc/authsrv.crt` certificate.
- The resource channel: HTTPS GET of `<resname>.res` from
  `Config.resurl`, trusting `etc/ressrv.crt`, User-Agent `Haven/1.0`
  (src/haven/Resource.java lines 432-480). `src/haven/HackSocket.java` is
  NOT part of the game protocol; it is an interruptible `java.net.Socket`
  wrapper used only by `SslHelper.connect` (src/haven/SslHelper.java line
  152) for the TLS channels above.

Datagram layout (both directions):

```
offset  size  field
0       1     msgtype (MSG_* byte)
1       n     payload (may be empty)
```

The reader allocates a 65536-byte receive buffer per packet
(src/haven/Session.java lines 417-432) and drops datagrams whose source
address does not equal the server address (line 433). Note that the client
validates the source ADDRESS only, never the source port, so the server may
reply from any UDP socket as long as the IP matches what the client
connected to.

## Primitive encodings

All helpers below live in src/haven/Message.java; the byte order helpers
they call live in src/haven/Utils.java.

| Type | Encoding | Writer / reader | Notes |
|------|----------|-----------------|-------|
| uint8 | 1 byte | `Message.adduint8` / `Message.uint8` (lines 110-171) | |
| int8 | 1 byte signed | `Message.int8` (line 165) | used by `OD_FOLLOW` |
| uint16 | 2 bytes LE | `Message.adduint16` / `Message.uint16` (lines 114-176); `Utils.uint16e` / `Utils.uint16d` (src/haven/Utils.java lines 261-264, 222-224) | |
| int32 | 4 bytes LE two's complement | `Message.addint32` / `Message.int32` (lines 120-186); `Utils.int32e` / `Utils.int32d` (src/haven/Utils.java lines 254-259, 246-252) | |
| uint32 | 4 bytes LE | `Utils.uint32e` / `Utils.uint32d` (src/haven/Utils.java lines 234-244) | not used by Message itself |
| string | UTF-8 + 0x00 terminator | `Message.addstring` / `Message.string` (lines 136-193) | `addstring2` appends raw UTF-8 without terminator (lines 126-134); only AuthClient uses that variant |
| coord | int32 x then int32 y | `Message.addcoord` / `Message.coord` (lines 141-197) | |
| color | 4 x uint8 (r, g, b, a) | `Message.color` (lines 199-201) | sent inside typed lists |

Typed argument lists (used by the widget protocol, see below) prefix every
element with a one-byte type tag and end with tag 0 or end-of-message
(`Message.addlist`, lines 146-159; `Message.list`, lines 203-221). Tags
(src/haven/Message.java lines 47-51):

- `T_END = 0`
- `T_INT = 1` followed by int32
- `T_STR = 2` followed by string
- `T_COORD = 3` followed by coord
- `T_COLOR = 6` followed by color

`Message.eom()` (line 161) is simply "read offset reached end of blob";
streamed sections always parse until eom or until a sentinel value.

## Session message types (MSG_*)

Defined in src/haven/Session.java lines 36-44. The first byte of every
datagram is one of these:

| Constant | Value | Direction | Meaning |
|----------|-------|-----------|---------|
| `MSG_SESS` | 0 | client to server, and server to client as the reply | session handshake / accept / error |
| `MSG_REL` | 1 | both | reliable ordered sub-messages (widget and world control stream) |
| `MSG_ACK` | 2 | both | cumulative acknowledgment of MSG_REL sequence numbers |
| `MSG_BEAT` | 3 | both | keepalive, empty payload |
| `MSG_MAPREQ` | 4 | client to server | request one map grid |
| `MSG_MAPDATA` | 5 | server to client | map grid data, fragmented |
| `MSG_OBJDATA` | 6 | server to client | object (gob) delta stream |
| `MSG_OBJACK` | 7 | client to server | per-object acknowledgment of OBJDATA frames |
| `MSG_CLOSE` | 8 | both | session close, empty payload |

Any datagram whose type byte is not understood is ignored by the client
reader (in this fork it prints a warning; src/haven/Session.java lines
481-486). A server must therefore also tolerate unknown client datagrams
without breaking the session.

### MSG_SESS (0)

Client to server, sent repeatedly until answered. Built in
`SWorker.run` (src/haven/Session.java lines 676-682):

```
uint16  flavour       constant 1
string  game name     constant "Haven"
uint16  PVER          protocol version, client constant 2 (line 34)
string  username      account name, NUL-terminated UTF-8
bytes   cookie        authentication cookie from AuthClient, opaque
```

Retry pacing: first send immediately, re-send every 2000 ms; after more
than 10 unanswered attempts the client gives up and reports
`SESSERR_CONN` to the UI (lines 667-685). The flavour uint16 and the
"Haven" string are legacy constants the server must simply accept and
ignore.

Server to client reply, handled in `RWorker.run` (lines 437-449):

```
uint8   error   0 = session accepted; otherwise one of the SESSERR_* codes
```

No other payload is sent in the accept case. Until the client sees error 0
it ignores every other datagram type (state `"conn"`, lines 451-452), so
the server MUST NOT send MSG_REL / MSG_OBJDATA / MSG_MAPDATA before the
accept reply.

### MSG_REL (1)

The reliable control stream. Carries a sequence number plus one or more
concatenated sub-messages (parse code: src/haven/Session.java lines
453-468):

```
uint16  seq        sequence number of the FIRST sub-message
then repeated:
  uint8  type      RMSG_* type
  if (type & 0x80):
    type &= 0x7f
    uint16 len     explicit payload length
    len bytes      payload
  else:
    payload extends to the end of the datagram
  seq increments by 1 per sub-message
```

So a datagram usually bundles several RMSG_* sub-messages under consecutive
sequence numbers. The explicit-length form (high bit set) is required only
when more sub-messages follow in the same datagram; the last sub-message
may be encoded without a length prefix.

### MSG_ACK (2)

```
uint16  seq   highest consecutively received MSG_REL sequence number
```

Sent by the receiving side at most every `ackthresh = 30` ms after the
first still-unacknowledged in-order sub-message arrives (constant at
src/haven/Session.java line 68; send logic lines 762-769). It is a
cumulative ACK: everything up to and including `seq` is confirmed. The
client builds this packet by hand as three bytes `{MSG_ACK, lo, hi}` using
`Utils.uint16e` (lines 764-766).

### MSG_BEAT (3)

Empty payload. The client sends one whenever it has transmitted nothing
else for 5000 ms (lines 771-776). The client never reacts to receiving a
beat (in this fork it logs "Unknown message type"; upstream ignored it).
Its purpose is NAT keepalive and liveness detection on the server side; a
Rust server should send beats on the same 5 second idle schedule and should
treat a silent peer (no REL, ACK, OBJACK, or BEAT) as timed out on its own
policy timeout - the legacy client itself imposes no UDP-level timeout.

### MSG_MAPREQ (4) - client to server

```
coord   gc    grid coordinate (the grid origin in grid units)
```

The client issues one MSG_MAPREQ per missing grid and repeats it every
1000 ms until answered, abandoning a grid after 5 attempts
(`MCache.sendreqs`, src/haven/MCache.java lines 612-632). The server must
answer each request with one or more MSG_MAPDATA datagrams carrying the
requested grid.

### MSG_MAPDATA (5) - server to client

One 100x100 grid is at least 10 kB of tile bytes plus overhead, which
exceeds typical path MTUs, so the server fragments each grid answer.
Each datagram carries (src/haven/MCache.java lines 517-544):

```
int32   pktid   fragment group id (server chosen, unique per grid transfer)
uint16  off     byte offset of this fragment inside the reassembly buffer
uint16  len     total reassembly length
bytes   chunk   fragment payload (datagram length minus 8 byte header)
```

The client reassembles fragments with `Defrag`, discards incomplete groups
after 10000 ms, and when complete parses the assembled buffer as a fresh
Message (`MCache.mapdata2`, src/haven/MCache.java lines 397-515):

```
coord   gc                  grid being delivered
string  mmname              minimap resource name, "" = none
repeated until uint8 == 255:          plot flavor table
  uint8  pidx                       plot index referenced in the blob below
  uint8  pfl                        plot flavor flags
zlib (RFC 1950, java.util.zip.Inflater) compressed remainder:
  100*100 bytes        tile ids, row major: outer loop y, inner loop x
  repeated until uint8 == 255:         plot (claim) list
    uint8  pidx                       index into the flavor table above
    uint8  type                       0 or 1
    uint8  c1x, c1y                   upper left tile in-grid
    uint8  c2x, c2y                   lower right tile in-grid (inclusive)
```

Plot type 0 yields overlay mask 1, or 2 when `pfl & 1`; plot type 1 yields
mask 4, or 8 when `pfl & 1` (lines 481-495). Mask bit 1/2 are the two
personal-claim render styles and 4/8 the two village-claim styles
(`olc[0..3]` colors, src/haven/MapView.java lines 136-139). The inflated
tile block is exactly `MCache.cmaps.x * MCache.cmaps.y = 100*100` bytes
(constants at src/haven/MCache.java lines 53-54; one grid tile covers
`MCache.tileSize = 11x11` pixels).

Fork quirk: this client retries the zlib inflate starting 2 bytes later on
failure (`msg.off += 2`, lines 431-435), a compatibility shim for
slightly different server encodings. A new server should emit the format
above exactly and not rely on that shim.

### MSG_OBJDATA (6) - server to client

See the dedicated "Object delta stream (OD_*)" section below.

### MSG_OBJACK (7) - client to server

```
repeated:
  int32  id      gob id being acknowledged
  int32  frame   latest frame number of that gob the client has applied
```

The client buffers one acknowledgment per object and flushes them in
bundles: a given object's ack is (re)sent when more than 200 ms have
passed since its last transmission, and the entry is deleted - after
sending one final ack - when no new OBJDATA for that object has arrived
for more than 120 ms (`SWorker`, src/haven/Session.java lines 737-761;
`ObjAck` class lines 140-152). Semantics for the server: an OBJACK for
frame N proves the client applied every delta up to frame N for that gob;
the server may then retire buffered deltas older than N.

### MSG_CLOSE (8)

Empty payload. On local shutdown the client sends MSG_CLOSE three times in
a row (interrupt handler in `SWorker.run`, src/haven/Session.java lines
779-782) and then tears down. When the client RECEIVES MSG_CLOSE it moves
to state `"fin"` and closes (lines 475-480). A server should send
MSG_CLOSE once and then stop sending; sending it three times like the
client does is harmless but not required. There is no close reason code -
any error reporting must happen through SESSERR_* at handshake time or
through widget-level UI messages.

## Reliability layer semantics

Sequence numbers are 16-bit and wrap modulo 65536, independently per
direction (`tseq` for outgoing, `rseq` for incoming;
src/haven/Session.java lines 75, 388-407, 865-874).

Sender behavior (the server must mirror what the client does in
`queuemsg` plus `SWorker`):

1. Assign the next `tseq` to each outgoing RMSG sub-message and keep it in
   the `pending` list until an MSG_ACK with `seq >= msg.seq` arrives
   (`gotack`, lines 183-192).
2. Retransmit unacknowledged messages with an escalating backoff, per
   message, based on the retransmission counter `retx` (lines 713-723):

   | retx | delay before next send |
   |------|------------------------|
   | 0 (first send) | immediately |
   | 1 | 80 ms |
   | 2-3 | 200 ms |
   | 4-9 | 620 ms |
   | 10+ | 2000 ms |

   All still-pending messages are re-bundled into fresh MSG_REL datagrams
   starting at the oldest pending sequence number.

Receiver behavior (`getrel`, lines 388-407; `RWorker.run`, lines 453-468):

1. Sub-messages with `seq == rseq` are delivered immediately; `rseq` then
   increments by one (mod 65536) and any consecutively buffered messages
   are drained and delivered in order.
2. Sub-messages with `seq > rseq` are stored in the `waiting` map for
   later in-order delivery (no reply is generated for them yet).
3. Sub-messages with `seq < rseq` (duplicates, usually from
   retransmission) are dropped.
4. After each delivery batch the receiver schedules a cumulative
   `MSG_ACK` for `rseq - 1` (the last in-order sequence), delayed by the
   30 ms `ackthresh`.

There is no negative acknowledgment and no per-message NAK path: a lost
MSG_REL datagram is only recovered by sender retransmission, and
everything behind the gap stays buffered on the receiver. A Rust server
should reproduce exactly this window-less "hold everything until the gap
fills" discipline, because widget ids and gob frames assume ordered
delivery.

## Reliable sub-message types (RMSG_*)

Defined in src/haven/Message.java lines 33-45; dispatched in
`Session.handlerel` (src/haven/Session.java lines 333-386). These are the
payloads of MSG_REL sub-messages.

| Constant | Value | Handled by | Payload summary |
|----------|-------|------------|-----------------|
| `RMSG_NEWWDG` | 0 | `RemoteUI.run` | create widget (see widget protocol) |
| `RMSG_WDGMSG` | 1 | `RemoteUI.run` | widget message (see widget protocol) |
| `RMSG_DSTWDG` | 2 | `RemoteUI.run` | destroy widget |
| `RMSG_MAPIV` | 3 | `MCache.invalblob` | map invalidation |
| `RMSG_GLOBLOB` | 4 | `Glob.blob` | global state blob (time, astronomy, light) |
| `RMSG_PAGINAE` | 5 | `Glob.paginae` | action menu (paginae) add/remove |
| `RMSG_RESID` | 6 | `Session.handlerel` | resource id to name/version mapping |
| `RMSG_PARTY` | 7 | `Party.msg` | party state |
| `RMSG_SFX` | 8 | `Session.handlerel` | play a sound resource |
| `RMSG_CATTR` | 9 | `Glob.cattr` | character attributes |
| `RMSG_MUSIC` | 10 | `Session.handlerel` | play or stop music |
| `RMSG_TILES` | 11 | `MCache.tilemap` | tileset id to resource mapping |
| `RMSG_BUFF` | 12 | `Glob.buffmsg` | buff list commands |

An RMSG type outside 0..12 aborts the client session reader with a
`MessageException` (lines 382-385), so the server must never send an
unknown RMSG type to a legacy client.

### RMSG_MAPIV (3) - map invalidation

```
uint8  type
type 0: coord gc        invalidate (refetch) one grid
type 1: coord ul, coord lr   trim all cached grids outside [ul, lr]
type 2: (nothing)       trim all cached grids everywhere
```

(src/haven/MCache.java lines 259-270.)

### RMSG_GLOBLOB (4) - global state

A stream of records until end of message (src/haven/Glob.java lines
108-128). Sub-record tags (lines 36-38):

```
uint8 tag
tag GMSG_TIME = 0: int32   server epoch time in seconds; client stores it
                           as Timer.server and pairs it with its own clock
tag GMSG_ASTRO = 1: int32 dt, int32 mp, int32 yt
                           three fixed point values scaled by 1e9
                           (Glob.defix, line 104): dt = fraction of the
                           24 h day (0..1), mp = moon phase (0..1,
                           eight named phases), yt = fraction of the
                           365 day year (0..1). The client derives
                           night = (dt < 0.25) || (dt > 0.75).
tag GMSG_LIGHT = 2: color  ambient light color (RGBA)
```

The server should push TIME and ASTRO periodically (the client renders the
in-game clock from them; src/haven/Astronomy.java lines 58-66).

### RMSG_PAGINAE (5) - action menu entries

Repeated until eom (src/haven/Glob.java lines 130-145):

```
uint8  op     '+' (0x2B) add, or '-' (0x2D) remove
string resname
uint16 resver
```

Each entry names a resource that carries an action layer (pagina). The
client shows them in the action menu; clicking sends the strings stored in
the resource's action layer back to the server (see MenuGrid example
below). The server controls the whole action menu through this message.

### RMSG_RESID (6) - resource mapping

```
uint16 resid      numeric resource id used by OD_RES / OD_OVERLAY / SFX etc.
string resname    resource name, e.g. "gfx/tiles/wald/leaf"
uint16 resver     resource version
```

(src/haven/Session.java lines 352-358.) The client caches the mapping and
loads the actual resource out of band (local directory, cache, then HTTPS
`resurl`; src/haven/Resource.java lines 239-278, 432-480, and
`MainFrame.setupres` lines 227-251). Every resid referenced anywhere else
on the wire MUST have been announced with this message first.

### RMSG_PARTY (7) - party state

Stream of records until eom (src/haven/Party.java lines 61-101). Tags
(lines 35-37):

```
uint8 tag
PD_LIST = 0:   int32 gobid, repeated, terminated by int32 -1:
               the full member list
PD_LEADER = 1: int32 gobid of the party leader
PD_MEMBER = 2: int32 gobid, uint8 visible (1 = position follows),
               coord c (only when visible), color col
               per-member marker color and last known position
```

### RMSG_SFX (8) - sound effect

```
uint16 resid    resource id of a sound resource, previously announced by RMSG_RESID
```

(src/haven/Session.java lines 361-365.)

### RMSG_CATTR (9) - character attributes

Repeated until eom (src/haven/Glob.java lines 147-162):

```
string nm      attribute name, e.g. "pts" learning points, "hp", "uw" ...
int32  base    base value
int32  comp    computed (buffed) value
```

### RMSG_MUSIC (10) - music control

```
string resname   resource name; empty string stops playback
uint16 resver    resource version
uint8  loop      optional (present when bytes remain): nonzero = loop
```

(src/haven/Session.java lines 368-377.)

### RMSG_TILES (11) - tileset mapping

Repeated until eom (src/haven/MCache.java lines 546-557):

```
uint8  id       tileset index (0..255) as referenced by map grid tile bytes
string resname  tileset resource
uint16 resver   resource version
```

Must be sent before (or with) the first MSG_MAPDATA so the client can
resolve tile ids. This fork hard-remaps tileset id 11 to
`gfx/tiles/wald/leaf` version 6 client-side (lines 551-554).

### RMSG_BUFF (12) - buffs

One command per message (src/haven/Glob.java lines 164-201):

```
string cmd
cmd "clear": (nothing)             remove all buffs
cmd "set":   int32 id, uint16 resid, string tt (tooltip, "" = none),
             int32 ameter, int32 nmeter, int32 cmeter, int32 cticks,
             uint8 major
cmd "rm":    int32 id              remove one buff
```

`ameter`/`nmeter`/`cmeter` are meter values (-1 = not shown); `cmeter` is
interpreted by the client as a remaining fraction of 100
(`Buff.GetTimeLeft`, src/haven/Buff.java lines 66-79) and `cticks` as a
number of 0.06 s ticks over which the meter drains. `major` marks the buff
as a large/major buff icon. Special buff ids -1 (crime), -2 (tracking) and
-3 (swim) are toggled client-side by the action menu
(src/haven/MenuGrid.java lines 374-394), so the server should never reuse
those ids for real buffs.

## Object delta stream (OD_*)

`MSG_OBJDATA` payloads are parsed by `Session.getobjdata`
(src/haven/Session.java lines 194-331). The payload is a concatenation of
per-object blocks until end of message:

```
repeated until eom:
  uint8  flags    bit 0 (0x01) = also remove this gob at frame - 1
  int32  id       gob id
  int32  frame    monotonically increasing per-gob frame/version number
  repeated attribute sub-messages until type == OD_END:
    uint8  type   one of the OD_* constants
    ...           type specific payload
```

The flag bit 0x01 schedules an additional removal one frame earlier
(`oc.remove(id, frame - 1)`, line 200-202) - used to retroactively delete
a gob that turns out to have been stale. Every processed block, even an
empty one, causes the client to queue an OBJACK for `(id, frame)`, so the
server can always track client progress (lines 315-326).

Frame numbers are the consistency mechanism: each attribute update carries
the frame it belongs to, the client refuses to resurrect a gob deleted at
a newer frame (`OCache.getgob`, src/haven/OCache.java lines 96-116), and
attribute setters ignore updates for gobs that do not exist yet or were
deleted. A server must therefore assign each gob a strictly increasing
frame counter and attach it to every delta.

OD_* constants (src/haven/Session.java lines 45-61):

| Constant | Value | Payload (after the type byte) |
|----------|-------|-------------------------------|
| `OD_REM` | 0 | (none) remove the gob at this frame |
| `OD_MOVE` | 1 | coord c - teleport/set position |
| `OD_RES` | 2 | uint16 resid, optional dynamic data (see below) |
| `OD_LINBEG` | 3 | coord s, coord t, int32 c - start linear move s to t with c steps |
| `OD_LINSTEP` | 4 | int32 l - progress the linear move to step l |
| `OD_SPEECH` | 5 | coord off, string text - speech bubble; empty text removes it |
| `OD_LAYERS` | 6 | uint16 base resid, then uint16 layer resids until 65535 |
| `OD_DRAWOFF` | 7 | coord off - draw offset; (0,0) removes the attribute |
| `OD_LUMIN` | 8 | coord off, uint16 sz, uint8 str - light source |
| `OD_AVATAR` | 9 | uint16 layer resids until 65535 (avatar layers, no base) |
| `OD_FOLLOW` | 10 | int32 oid; if oid != -1: int8 szo, coord off |
| `OD_HOMING` | 11 | int32 oid; -1 = stop, -2 = homocoord (coord tgtc, uint16 v), else oid, coord tgtc, uint16 v |
| `OD_OVERLAY` | 12 | int32 olid_and_flags, uint16 resid, optional dynamic data |
| `OD_HEALTH` | 14 | uint8 hp - health level in quarters, 0..4 (4 = full; rendered as a red tint by GobHealth, `asfloat() = hp / 4`, src/haven/GobHealth.java lines 31-61) |
| `OD_BUDDY` | 15 | string name, uint8 group, uint8 btype - kin info; all empty/zero removes it |
| `OD_END` | 255 | (none) end of this gob's attribute list |

(There is no OD value 13; the removed `OD_AUTH` constant is commented out
at line 58.)

Details that are easy to get wrong:

- OD_RES dynamic data (`sdt`): if bit 0x8000 is set in resid, the low 15
  bits are the resource id and the payload continues with
  `uint8 len` followed by `len` raw bytes which the client exposes to the
  resource's drawing code (`msg.derive(0, msg.uint8())`, lines 212-219).
  The sdt blob is stored by `ResDrawable` and handed to the resource's
  sprite factory, where it selects resource-defined variants (growth
  stages, object states); src/haven/ResDrawable.java lines 31-51.
  The same 0x8000 scheme applies to OD_OVERLAY (lines 283-294). A resid
  value of 65535 in OD_OVERLAY means "remove overlay" (lines 283-286).
- OD_LAYERS / OD_AVATAR: the layer list is terminated by the sentinel
  uint16 65535 (lines 238-243).
- OD_FOLLOW with oid == -1 removes the following attribute; otherwise the
  gob is glued to gob `oid` at `off` with sprite z-order `szo`
  (src/haven/OCache.java lines 258-275).
- OD_HOMING: oid == -1 stops homing (`homostop`), oid == -2 retargets an
  existing Homing to a coordinate (target gob id is cleared to a pure
  coordinate chase), any other oid homes toward gob `oid` passing through
  `tgtc` at speed `v` (src/haven/OCache.java lines 277-300;
  src/haven/Homing.java).
- OD_LINBEG + OD_LINSTEP implement the client movement model: LINBEG sets
  a LinMove from `s` to `t` split into `c` steps; LINSTEP advances to
  step `l` and, when `l` is outside `[0, c)`, removes the Moving
  attribute, ending the move (src/haven/OCache.java lines 160-191;
  src/haven/LinMove.java). The client interpolates the move locally from
  the LINBEG timing model (`c * 66.67 ms`), so LINSTEP frames are a
  counter re-sync, not a position push: the server ships them on a 5 Hz
  cadence (every 2nd tick, `LINSTEP_EVERY_TICKS = 2`) instead of every
  tick, halving the progress fan-out; finalizers always ship.
- OD_MOVE and LINBEG/LINSTEP coordinates are in absolute world pixel
  coordinates (tile * 11 pixels per axis; src/haven/MCache.java line 53).

## Widget protocol (RMSG_NEWWDG / RMSG_WDGMSG / RMSG_DSTWDG)

The entire UI is server-driven; the client is a "dumb terminal" by
design (philosophy statement at
http://legacy.havenandhearth.com/portal/doc-src). Widget ids are uint16
and the root widget always exists with id 0
(src/haven/UI.java lines 135-141). Decoding is in `RemoteUI.run`
(src/haven/RemoteUI.java lines 47-91).

### RMSG_NEWWDG (0) - server creates a widget

```
uint16  id       new widget id (server chosen, must be unique)
string  type     widget type name, or a resource path (see below)
coord   c        position inside the parent
uint16  parent   parent widget id
typed-list args  type specific arguments
```

If `type` contains a `/` it is treated as a resource name with an
optional `:version` suffix; the client loads the resource and instantiates
the widget from its Code entry (src/haven/UI.java lines 169-199). Built-in
type names are registered by each widget class via `Widget.addtype`; the
registry is forced to load through `Widget.initbardas` / the `barda`
class list (src/haven/Widget.java lines 52-86) and the direct `addtype`
call sites. Names used by this client: `cnt` (plain container, args[0] =
size Coord), `mapview` (args[1] = center Coord, args[2] = player gob id;
src/haven/MapView.java lines 123-135), `slen` (status HUD, no args;
src/haven/SlenHud.java lines 71-75), `scm` (action menu;
src/haven/MenuGrid.java lines 70-76), `speedget` (args[0] = current
speed, args[1] = max speed; src/haven/Speedget.java lines 51-57),
`charlist` (args[0] = visible row count; src/haven/Charlist.java lines
53-57), `img`, `lbl`, `btn`, `ibtn`, `wnd`, `inv`, `epry`, `item`, `isbox`,
`make`, `text`, `chk`, `vm`, `im`, `prog`, `av`, `av2`, `pv`, `frv`,
`sm`, `npc`, `cal`, `buddy`, `give`, `chr`, `log`, `hwnd`, `mapmod`,
`buffs`, `chat`, `slenchat`, `slenlog`, `lb`, `ltbtn`.

Client-side layout fixups in this fork reposition some server widgets
("cnt" gets the real window size, "charlist" is centered, "wnd" at
(400,200) is centered, certain "img" screens are centered;
src/haven/RemoteUI.java lines 59-76). These do not change the wire
format; a new server can ignore them but should be aware legacy clients
apply them.

### RMSG_WDGMSG (1) - widget messages

Server to client:

```
uint16  id     target widget id
string  name   message name
typed-list args
```

The message is delivered to `Widget.uimsg` of that widget
(src/haven/UI.java lines 267-279); sending to a nonexistent id raises a
client exception, so DSTWDG/NEWWDG ordering matters. Messages understood
by the base Widget class for all widgets (src/haven/Widget.java lines
256-279): `tabfocus`, `act` (enable activate), `cancel` (enable cancel),
`autofocus`, `focus` (focus child by id), `curs` (set cursor resource).
Each concrete widget adds its own; for example `mapview` understands
`move` (coord), `flashol` (int mask, int ms), `place` (string resname,
int resver, int plontile, optional int radius), `unplace`, `polowner`
(string) - src/haven/MapView.java lines 864-919 - and `charlist`
understands `add` (string name, repeated uint16 avatar layer resids) -
src/haven/Charlist.java lines 147-163.

Client to server (the replies that drive all gameplay commands):

```
uint16  id     sending widget id
string  name   message name
typed-list args
```

Encoded by `RemoteUI.rcvmsg` (src/haven/RemoteUI.java lines 39-45) and
delivered through `UI.wdgmsg` (src/haven/UI.java lines 257-265). These
travel as RMSG_WDGMSG sub-messages inside the client's MSG_REL stream.
Canonical examples every server must support:

- `charlist` widget: client sends `play` with one T_STR argument, the
  chosen character name (src/haven/Charlist.java lines 124, 137).
- `scm` action menu: client sends `act` with the action strings taken
  from the pagina resource's action layer (src/haven/MenuGrid.java line
  404) - the server defines what the strings mean when it publishes the
  paginae resources.
- `sm` flower menu: client sends `cl` with the chosen option number, or
  -1 for cancel (src/haven/FlowerMenu.java lines 205-276); the menu
  options themselves came from a NEWWDG whose args are the option label
  strings (lines 45-54).
- `mapview`: `click` (with screen coord, world coord, button, modifier
  flags, and optional gob id and gob position), `place`, `itemact`,
  `drop` (src/haven/MapView.java lines 699, 729-738, 2092-2105).

### RMSG_DSTWDG (2) - destroy a widget

```
uint16  id    widget id to destroy
```

Destroying a widget recursively destroys and unbinds its children client
side (src/haven/UI.java lines 209-227). Destroying widget 0 (root) is not
meaningful.

## Session error codes (SESSERR_*)

Returned in the uint8 error field of the MSG_SESS reply
(src/haven/Session.java lines 62-66). The client maps them to UI strings
in src/haven/Bootstrap.java lines 268-287:

| Constant | Value | Client message |
|----------|-------|----------------|
| `SESSERR_AUTH` | 1 | "Invalid authentication token" (the cookie was rejected) |
| `SESSERR_BUSY` | 2 | "Already logged in" (same account already has a session) |
| `SESSERR_CONN` | 3 | "Could not connect to server" (also self-generated by the client after 10 unanswered MSG_SESS attempts; src/haven/Session.java lines 667-675) |
| `SESSERR_PVER` | 4 | "This client is too old" (protocol version mismatch against the MSG_SESS PVER field) |
| `SESSERR_EXPR` | 5 | "Authentication token expired" (cookie lifetime exceeded) |

Values above 5 hit the default branch and show a generic "Connection
failed". The client treats any nonzero reply as terminal for that
connection attempt and closes the socket (src/haven/Session.java lines
440-448).

## Server implementation notes

- Reproduce byte order exactly: everything is little-endian, strings are
  NUL-terminated UTF-8, coords are two int32. A single endianness bug
  will pass the handshake and corrupt everything after it.
- Implement the MSG_REL layer as a faithful state machine: 16-bit
  sequence spaces per direction, cumulative ACK with the 30 ms delay,
  the exact retransmission backoff table, and in-order-only delivery
  with a hold-back buffer. Do not "improve" it with selective ACKs; the
  legacy client only understands the cumulative MSG_ACK.
- The server side of OBJDATA needs the mirror of the client's objack
  pacing (200 ms bundle cadence, ~120 ms linger) if it wants to keep
  UDP traffic low, but it must ACCEPT objacks arriving at any time and
  bundled in any quantity.
- OBJDATA retransmission regime (session 64): raw OBJDATA rides lossy
  UDP and the client NEVER re-requests gob state, so the server must
  recover losses itself. Keep the last 4 unconfirmed (gob, frame)
  blocks per session, ordered by frame, and on a ~300 ms sweep resend
  every block whose delay elapsed, IN FRAME ORDER (the raw socket is
  FIFO: a resent stale frame must never overtake a newer one - an
  out-of-order OD_REM phantom-deletes a fresh spawn). Gate each block
  on the client's acked high-water mark (an OBJACK for frame N retires
  everything <= N) and schedule attempts as 5x250 ms fast then 4x1 s
  slow for CRITICAL blocks - spawn, OD_REM, full re-renders - versus
  3x250 ms + 2x1 s for self-healing ones (movement finalizers, hp
  ticks). Retire exhausted blocks instead of keeping them (sessions
  that never ack - load bots, dead peers - would otherwise accumulate
  retransmit state; the measured OOM shape from the 1000-session
  scale). A removal op must carry frame = max(every frame the client
  saw for that gob, highest pending frame) + 1: a removal at a frame
  the ack already covers would be silently skipped by the gate and
  the client would render a phantom gob forever. Wire probe:
  `lost_static_spawn_wave_is_retransmitted` in wire.rs drops a 1.2 s
  OBJDATA window at world entry and demands recovery through the
  sweep.
- Keep every datagram under the path MTU anyway; the legacy server kept
  map fragments and objdata bundles small enough for typical MTUs. The
  client's 65536-byte read buffer is not an invitation to send huge
  datagrams.
- Send RMSG_RESID mappings before first use of any resid in OD_RES,
  OD_OVERLAY, OD_LAYERS, OD_AVATAR, or RMSG_SFX; missing mappings stall
  the client renderer (the Indir placeholders never resolve).
- Remember that the client filters inbound datagrams by source address
  only (src/haven/Session.java line 433); bind one UDP socket per
  session and reply from it, or make sure the game socket replies from
  the same IP the client targeted.
- Unknown MSG_* datagram types from the client should be logged and
  dropped, never fatal; unknown RMSG_* types the server sends will kill
  the client reader thread, so they must be avoided.
- MSG_SESS attempts arrive as duplicates (client retries every 2 s up to
  10 times); the accept reply is idempotent - answer each duplicate with
  the same error-0 datagram until the client stops.
- Movement timing (verified against src/haven/LinMove.java, session 20):
  the client interpolates a whole LINBEG move on its own render clock -
  `ctick: a += (dt/1000)/(c*0.06) * 0.9` - so it covers the path in
  `c * 66.67 ms` no matter when LINSTEP frames arrive, and `setl` only
  ever ADVANCES the client progress. Therefore: derive the step count as
  `c = round(total_ms / 66.67)`, send LINSTEP `l = floor(progress * c)`
  only when the index advances, and finish each move with ONE block
  carrying `OD_MOVE` (destination) followed by `OD_LINSTEP(l >= c)`:
  `Gob.move` pins `rc` to the goal, then linstep drops the Moving
  attribute. A final bare LINSTEP without OD_MOVE lets `position()` fall
  back to the STALE pre-move `rc` - the avatar visibly rubber-bands to
  its start point (measured on the real client).
- Retargeting while moving must start the new LINBEG from the mover's
  interpolated position, not from the old destination: the client snaps
  the gob to the new `s` the moment LINBEG arrives, so a
  destination-anchored restart teleports on rapid clicks.

## Open questions

- The exact acceptable length and internal structure of the
  authentication `cookie` passed in MSG_SESS is not constrained anywhere
  in the client (it is an opaque `byte[]` from AuthClient, sent with
  `addbytes`, src/haven/Session.java lines 676-682). The original server
  issued it over the TLS auth channel; its length can be chosen freely
  by a new server as long as AuthClient's 255-byte auth-frame limit
  (src/haven/AuthClient.java line 128) is respected.
- The meaning of the first MSG_SESS uint16 (constant 1) is not
  documented beyond its value; treat it as a handshake flavour constant
  and ignore it server-side.
- The maximum MSG_REL sub-messages per datagram and any server-side
  send coalescing schedule of the original server are unknown; only the
  client-side receive rules are fully determined. Choose the bundle size
  from MTU limits.
- Whether the original server ever used the `ackthresh`-based ACK
  piggybacking on its own REL stream symmetrically (it must, per
  protocol symmetry) and whether it sent MSG_BEAT on the same 5 s
  schedule cannot be proven from client code alone; the client accepts
  both silently.
- The semantics of the MSG_MAPDATA "plot flavor table" second byte
  (`pfl`) are only partially recoverable from the client (bit 0 switches
  the rendered claim style, src/haven/MCache.java lines 481-495). What
  the original server encoded in the remaining bits is unknown; send 0
  for plain claims.
- The fork-specific 2-byte inflate retry in `MCache.mapdata2`
  (src/haven/MCache.java lines 431-435) implies some historical server
  emitted 2 extra bytes before the zlib stream; the exact variant is
  undocumented. Emit the canonical layout instead and ignore the shim.
- Buff meters: the precise server-side meaning of `ameter` versus
  `nmeter` (which one carries the numeric label versus the bar) can only
  be pinned down against live server traffic; the client just renders
  both (src/haven/Buff.java lines 36-79).
