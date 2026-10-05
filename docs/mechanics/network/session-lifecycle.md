# Legacy Haven and Hearth Session Lifecycle (server perspective)

> **Sources:** src/haven/AuthClient.java, src/haven/Bootstrap.java, src/haven/MainFrame.java, src/haven/Session.java, src/haven/RemoteUI.java, src/haven/UI.java, src/haven/Widget.java, src/haven/Charlist.java, src/haven/Listbox.java, src/haven/LoginScreen.java, src/haven/MCache.java, src/haven/OCache.java, src/haven/Glob.java, src/haven/Resource.java, src/haven/SslHelper.java, src/haven/Utils.java, src/haven/test/CharSelector.java, http://legacy.havenandhearth.com/portal/doc-src

## Summary

This document walks through the complete life of a legacy Haven and Hearth
session from the point of view of a server implementation: the out-of-band
endpoints a client needs before connecting, the TLS authentication
handshake that yields a cookie, the UDP session handshake (MSG_SESS), the
server-driven widget bootstrap, character selection, the in-world
initialization order (resources, tilesets, map grids, object sync, global
state), steady-state traffic, teardown, and reconnection. Every step states
exactly what the server must send or accept, in order. The wire framing of
each message referenced here is specified in
`docs/mechanics/network/network-protocol.md`; this file focuses on
sequence, state, and ordering.

The client's own bootstrap logic lives in `Bootstrap.run`
(src/haven/Bootstrap.java lines 147-300): it binds a client-local login
screen at widget id 1 (line 150: `ui.bind(new LoginScreen(ui.root), 1)`),
talks to the auth server, creates `Session`, waits for the MSG_SESS
accept, destroys the login widget (line 264), and then hands control to
`RemoteUI.run` (src/haven/MainFrame.java lines 215-217), which services
server widget messages until the session dies
(src/haven/RemoteUI.java lines 47-91).

## Phase 0: endpoints and assets the server must provide

Before any connection, a legacy client expects three network services and
one certificate decision:

1. **Game server**, UDP port 1870 (client sends there;
   src/haven/Session.java line 893).
2. **Auth server**, TLS TCP port 1871, whose certificate must match the
   built-in `etc/authsrv.crt` (src/haven/AuthClient.java lines 45-53,
   56; src/haven/SslHelper.java lines 95-110). The client pins the exact
   X.509 certificate, so a new server must either ship a modified
   certificate or have clients trust its own cert.
3. **Resource server**, an HTTP(S) base URL (`haven.resurl` property or
   `-r` command line option; src/haven/Config.java lines 162-166,
   314-318) from which the client fetches `<resname>.res` files with
   User-Agent `Haven/1.0` (src/haven/Resource.java lines 466-475).
   HTTPS responses are checked against `etc/ressrv.crt` unless the URL
   is plain http (lines 437-447, 469-472). Local sources are consulted
   first: the `./res` directory and a local disk cache
   (src/haven/Resource.java lines 145-149, 207-224).

A server that reuses the stock resources must also serve them over one of
those channels; the in-session RMSG_RESID mapping only tells the client
the NAME and VERSION to load, never the bytes.

## Phase 1: authentication over TLS (AuthClient)

Implemented in src/haven/AuthClient.java. The protocol is a simple
synchronous request/reply channel inside TLS:

```
frame (both directions):
  uint8  type
  uint8  len       payload length, max 255
  len bytes payload
```

(sendmsg: lines 127-136; recvmsg: lines 147-153; the 255-byte limit is
enforced client-side at line 128). Commands (lines 36-39):

| Constant | Value | Client sends | Server reply type 0 payload |
|----------|-------|--------------|-----------------------------|
| `CMD_USR` | 1 | UTF-8 username, NOT NUL-terminated (`addstring2`, line 64) | empty (accept) |
| `CMD_PASSWD` | 2 | 32-byte SHA-256 digest of the UTF-8 password (`digest`, lines 72-87) | the session cookie blob |
| `CMD_GETTOKEN` | 3 | empty | a reusable token blob |
| `CMD_USETOKEN` | 4 | previously issued token blob | the session cookie blob |

Any reply with a type other than 0 means failure (lines 67-69, 93-98,
104-109, 115-120). The exact sequence used by the login UI is:

1. Connect TLS to port 1871 (`new AuthClient(host, username)`, lines
   55-60).
2. Send CMD_USR with the username; require reply 0.
3a. Password path: send CMD_PASSWD with the SHA-256 password digest
   (`trypasswd`, lines 89-99). On success the reply payload is the
   `cookie`.
3b. Saved-token path: send CMD_USETOKEN with the stored 32-byte token
   (64 hex characters in the `savedtoken` preference;
   src/haven/Bootstrap.java lines 155-156, 188-195). On success the reply
   payload is the `cookie`.
4. Optionally, after a successful password login and only when the user
   asked to save the login, send CMD_GETTOKEN and persist the returned
   token (src/haven/Bootstrap.java lines 239-242).
5. Close the TLS connection (`AuthClient.close`); the cookie is the only
   thing carried forward.

Server obligations: maintain per-username password digests (SHA-256 of
the UTF-8 password, no salt in the legacy client - keep that contract or
clients cannot log in), issue single-use cookies, issue tokens if
CMD_GETTOKEN is implemented, and reject CMD_USETOKEN with a nonzero type
for unknown/expired tokens.

## Phase 2: UDP session handshake (MSG_SESS)

With cookie in hand the client opens a UDP socket and starts the session
worker threads (src/haven/Session.java lines 829-845). The handshake, in
order:

1. Client sends MSG_SESS repeatedly every 2000 ms until answered:
   `uint16 flavour = 1`, `string "Haven"`, `uint16 PVER = 2`,
   `string username`, `bytes cookie` (src/haven/Session.java lines
   676-682). After more than 10 retries without a reply the client
   declares `SESSERR_CONN` and gives up (lines 667-675).
2. Server validates the cookie and replies with a one-byte MSG_SESS:
   `uint8 error`, where 0 accepts the session and anything else is a
   `SESSERR_*` code (src/haven/Session.java lines 437-449). Error
   meanings (src/haven/Bootstrap.java lines 268-287):

   | Code | Constant | Meaning |
   |------|----------|---------|
   | 1 | `SESSERR_AUTH` | invalid cookie ("Invalid authentication token") |
   | 2 | `SESSERR_BUSY` | account already has a live session ("Already logged in") |
   | 3 | `SESSERR_CONN` | transport failure (also self-generated by the client) |
   | 4 | `SESSERR_PVER` | protocol version mismatch ("This client is too old") |
   | 5 | `SESSERR_EXPR` | expired cookie ("Authentication token expired") |

3. On error 0 the client leaves the `"conn"` state and starts processing
   every other message type (lines 451-487). Before that point the client
   discards all non-MSG_SESS datagrams, so the server must not race ahead
   of its own accept reply.
4. Duplicate MSG_SESS attempts keep arriving until the client sees the
   reply; answer each one identically (idempotent accept), because the
   client will not stop retrying until a reply datagram arrives.

The server should bind the session to the client's address and assign
session state (player account, pending character, sequence spaces) at
this point. PVER must be checked against the uint16 in the MSG_SESS
payload: reply `SESSERR_PVER` when the server does not speak version 2.

## Phase 3: widget bootstrap and character selection

Everything after the accept is server-driven through the widget protocol
(NEWWDG / WDGMSG / DSTWDG inside MSG_REL). The client has exactly one
pre-bound widget: the root at id 0 (src/haven/UI.java lines 135-141),
plus the client-local login screen at id 1 which Bootstrap destroys the
moment the session is accepted (src/haven/Bootstrap.java line 264) - the
server never sees widget 1.

### 3.1 Character selection screen

For the selection phase the legacy server creates a minimal UI. This
fork's client expects:

1. `NEWWDG` of type `img` for the character-selection backdrop with arg
   `gfx/ccscr` (the client re-centers it; src/haven/RemoteUI.java lines
   61-63) and another `img` for the logo `gfx/logo2` (lines 64-65).
2. `NEWWDG` of type `charlist` with one int arg: the number of visible
   rows (src/haven/Charlist.java lines 53-57). The client re-centers the
   widget (src/haven/RemoteUI.java lines 66-67).
3. One `WDGMSG` `add` to the charlist per character:
   `string name`, then any number of `int` avatar layer resource ids for
   the row's portrait (src/haven/Charlist.java lines 147-163). Every
   resid referenced here must have been announced with RMSG_RESID first.
4. Optionally an `ibtn` new-character button using resources
   `gfx/hud/buttons/ncu` / `gfx/hud/buttons/ncd` (the client repositions
   it; src/haven/RemoteUI.java lines 68-73).

The client answers with `WDGMSG` `play` carrying one T_STR argument: the
chosen character name (src/haven/Charlist.java lines 124 and 137). That
is the character-selection completion signal.

Historical variant: the upstream-era test robot
(src/haven/test/CharSelector.java lines 43-75) drives an older selection
UI - a `lb` (Listbox) widget answered with `chose` (option name;
src/haven/Listbox.java lines 92-93) plus a Button labeled
"I choose you!" answered with `activate`. A server targeting this fork
only needs `charlist`/`play`, but accepting both is trivial and covers
older clients.

### 3.2 Entering the world

After `play`, the canonical sequence a server must perform (each numbered
step is directly evidenced by the consumer code cited; the relative
order of steps 4-8 is the recommended safe order, see Open questions):

1. **Destroy the selection widgets** (`DSTWDG` for the charlist and img
   widgets). The old upstream robot treats the charlist's destruction as
   the "in world" signal (src/haven/test/CharSelector.java lines 77-82).
2. **Announce resources**: a burst of `RMSG_RESID` messages
   (uint16 resid, string resname, uint16 resver) for every resource the
   initial scene needs (player avatar layers, starting gear, terrain
   objects). Loading happens asynchronously out of band; the client
   stalls rendering of any gob whose resources are not yet resolved
   (src/haven/Session.java lines 352-358; src/haven/Resource.java lines
   239-278).
3. **Announce tilesets**: `RMSG_TILES` (uint8 tileset id, string
   resname, uint16 resver) covering every tile id that will appear in
   map grids (src/haven/MCache.java lines 546-557). Must precede the
   first MSG_MAPDATA.
4. **Create the HUD**: `NEWWDG` type `slen` (no args; status bar with
   equipment, meters, menu button; src/haven/SlenHud.java lines 71-75),
   `NEWWDG` type `scm` (action menu; src/haven/MenuGrid.java lines
   70-76), `NEWWDG` type `speedget` (args: int current speed, int max
   speed; src/haven/Speedget.java lines 51-57), `NEWWDG` type `buffs`
   (buff tray; src/haven/Bufflist.java line 42).
5. **Create the map view**: `NEWWDG` type `mapview` with args
   `args[1]` = center Coord (world position the camera starts on) and
   `args[2]` = the player's gob id (src/haven/MapView.java lines
   123-135). This is how the client learns which gob it controls.
6. **Seed global state**: `RMSG_GLOBLOB` records - at minimum
   `GMSG_TIME` (int32 epoch seconds) and `GMSG_ASTRO` (day fraction,
   moon phase, year fraction, each int32 scaled by 1e9), optionally
   `GMSG_LIGHT` (ambient RGBA) (src/haven/Glob.java lines 108-128).
7. **Publish the action menu**: `RMSG_PAGINAE` entries
   (`+`, string resname, uint16 resver) for the starting paginae
   (src/haven/Glob.java lines 130-145).
8. **Push character attributes**: `RMSG_CATTR` records (string name,
   int32 base, int32 comp) for the attributes the UI meters read
   (src/haven/Glob.java lines 147-162).

### 3.3 Map delivery

As soon as the mapview exists the client computes which 100x100-tile
grids it needs and starts sending `MSG_MAPREQ` (coord grid coord) for
each missing one, repeating every 1000 ms and abandoning after 5 tries
(src/haven/MCache.java lines 612-632). The server must answer each
request with the grid's data as one or more fragmented MSG_MAPDATA
datagrams (int32 pktid, uint16 off, uint16 total, chunk) which reassemble
into: grid coord, minimap resource name, plot flavor table, and a zlib
blob holding 100x100 tile bytes plus the claim plot list
(src/haven/MCache.java lines 397-544; full layout in the protocol
document). Grid coordinates are in grid units: grid `(x, y)` covers
world tiles `[x*100, x*100+99] x [y*100, y*100+99]`, one tile being
11x11 pixels (src/haven/MCache.java lines 53-54).

When terrain changes later, the server invalidates cached grids with
`RMSG_MAPIV` (type 0 single grid, type 1 area trim, type 2 trim all),
which makes the client re-request them (src/haven/MCache.java lines
259-270).

### 3.4 Initial object sync

The client only knows about objects (gobs) the server tells it about.
The initial state of the player's surroundings is delivered as
`MSG_OBJDATA` blocks: for each visible gob, one block with
`OD_MOVE` (position), `OD_RES` (appearance resource, with optional
dynamic data), plus any attribute deltas (OD_LINBEG if it is moving,
OD_LAYERS/OD_AVATAR for layered characters, OD_SPEECH, OD_HEALTH,
OD_BUDDY kin labels, OD_OVERLAY effects), terminated by `OD_END`
(src/haven/Session.java lines 194-331). Every block must carry the gob's
monotonic frame number, and the client will acknowledge each block with
`MSG_OBJACK` (id, frame) - the server uses those acks to know when it can
stop re-sending state (src/haven/Session.java lines 737-761; object
lifecycle in src/haven/OCache.java lines 47-52, 96-116).

Removal is asynchronous and frame-guarded: `OD_REM` deletes a gob; a
newer-frame delta resurrects it only if the deletion frame is older
(src/haven/OCache.java lines 96-116). The flags bit 0x01 on an object
block removes the gob at frame-1, covering the "deleted then
immediately recreated" race (src/haven/Session.java lines 200-202).

## Phase 4: steady state

Once the player is in world the traffic settles into the following
responsibilities, all specified wire-level in the protocol document:

- Reliability: MSG_REL sequences, cumulative MSG_ACK (30 ms delay),
  retransmission backoff, MSG_BEAT every 5 s idle
  (src/haven/Session.java lines 687-776).
- Object streaming: MSG_OBJDATA deltas for movement (OD_LINBEG /
  OD_LINSTEP), actions, overlays, speech, health, kin labels; object
  acknowledgments from the client pace the server's retransmit buffer.
- Global clock: periodic `RMSG_GLOBLOB` TIME/ASTRO updates keep the
  client clock and astronomy display correct (src/haven/Glob.java lines
  108-128).
- UI flow: server pushes widgets and WDGMSGs (windows, inventories as
  `inv`/`item` widget trees, crafting via `make`, flower menus via `sm`
  with client replies `cl <n>`); clients send gameplay commands as
  WDGMSGs (`act` from the menu grid, `click`/`itemact`/`drop` from the
  map view, `play` from charlists, `chose` from listboxes). The action
  strings sent by `scm` originate from the pagina resources the server
  published, so the server effectively defines its own command strings
  (src/haven/MenuGrid.java lines 366-405).
- Party: `RMSG_PARTY` records keep the member list, leader flag, marker
  colors, and last-known positions current (src/haven/Party.java lines
  61-101).
- Buffs: `RMSG_BUFF` `set`/`rm`/`clear` commands (src/haven/Glob.java
  lines 164-201).
- Audio: `RMSG_SFX` (uint16 resid) for effects and `RMSG_MUSIC` (string
  name, uint16 ver, optional uint8 loop; empty name stops) for music
  (src/haven/Session.java lines 361-377).

## Phase 5: session close

There is exactly one close primitive: `MSG_CLOSE` (empty payload,
type 8). Two closure paths exist:

1. **Server-initiated**: the server sends MSG_CLOSE. The client sets its
   state to `"fin"`, closes the socket, and the UI session ends
   (src/haven/Session.java lines 475-480). The client displays no
   reason - any user-facing explanation must have been pushed earlier as
   a widget message (for example a textlog or window widget).
2. **Client-initiated**: on shutdown the client interrupts its writer,
   which sends MSG_CLOSE three times in quick succession and then stops
   (src/haven/Session.java lines 779-782). A server should treat the
   first MSG_CLOSE as authoritative and stop sending after it; the
   duplicates are just lost-packet insurance.

There is no close reason code and no graceful "drain" state: MSG_CLOSE
is immediate. The `SESSERR_*` codes are only defined in the MSG_SESS
handshake reply and cannot be delivered mid-session; a legacy server
rejecting an action mid-session did so through widget-level messages.

Idle timeout is entirely the server's decision. The client imposes no
UDP-level timeout of its own: its reader loops forever on a 1 s socket
timeout (src/haven/Session.java lines 409-432) and only user action or
server close ends the session. The client does keep sending MSG_BEAT
every 5 s while idle, which a server can use as a liveness signal.

## Reconnection semantics

There is no session resume in the legacy client. Losing the session
(handshake failure, MSG_CLOSE, reader thread death) leaves the UI dead and
the user must log in again from the login screen
(src/haven/Bootstrap.java retry loop, lines 159-296: on any connfailed
code the client returns to authentication and reconnects from scratch).

Consequently a reconnection is: Phase 1 (or CMD_USETOKEN with the saved
token) -> Phase 2 -> Phase 3 again, with fresh widget ids, fresh gob
frames, fresh grid requests, and a fresh cookie. Server-side state that
must be rebuilt per session includes: the RESID mapping (resid numbers
are session-local, resolved through `Session.getres` against a per-
session cache, src/haven/Session.java lines 84, 97-138), the
RMSG_TILES tileset mapping (also session-local, src/haven/MCache.java
lines 546-557), widget id space, gob id space, and MSG_REL sequence
spaces (both start at 0 each session; src/haven/Session.java line 75).

If the same account is still marked logged in when a new MSG_SESS
arrives, the server must reply `SESSERR_BUSY` (2) unless it has decided
to drop the older session; the client surfaces that as "Already logged
in" (src/haven/Bootstrap.java lines 272-274).

## Server implementation notes

- Order matters more than anything else in this document: RESID before
  anything that references a resid; TILES before MAPDATA; the mapview
  NEWWDG before any gob the player should see rendered in it; MSG_SESS
  accept before any other datagram.
- Treat every client message as potentially duplicated (MSG_SESS every
  2 s, MSG_MAPREQ every 1 s, MSG_OBJACK bundles): make handlers
  idempotent.
- Keep a per-session state machine with at least these states:
  `awaiting-sess` (before accept), `charselect`, `in-world`, `closed`.
  Widget traffic before accept is a protocol violation; charlist
  widgets during `in-world` are harmless but pointless.
- The character name sent in `play` WDGMSG is the exact string the
  server passed to `charlist add` - match it case-sensitively.
- Give every gob a per-session unique int32 id and a monotonic int32
  frame counter; the objack mechanism is the only delivery
  confirmation you get for object state.
- Expect the client to request map grids aggressively at startup (one
  request per second per missing grid); pre-queue the grids around the
  spawn point so the 5-attempt limit is never hit.
- Log unknown client datagram types instead of dropping the session;
  this client likewise only logs (never acts on) inbound datagram types
  it does not know (src/haven/Session.java lines 481-486).
- The client never probes the resource server during the session unless
  a RESID announced a (name, version) it does not have cached - make
  sure the resource URL published to clients actually serves every
  resource name your RESIDs reference, or the client will hang waiting
  on an `Indir` that never resolves (src/haven/Session.java lines
  106-114).

## Open questions

- Cookie lifetime and single-use semantics are server policy; the
  client gives no hint beyond `SESSERR_EXPR` (5). Decide and document
  your own TTL, then answer CMD_USETOKEN with nonzero reply types for
  expired tokens.
- Whether the original server required the cookie to be bound to the
  source IP of the UDP session is unknowable from the client; the
  client sends the cookie verbatim. Recommend binding it for security.
- The exact widget id numbering scheme of the original server (which
  ids it allocated to slen/mapview/scm, and in what order the startup
  NEWWDGs actually arrived) cannot be recovered from client code, since
  ids are opaque uint16 to the client. Any allocation works; a fixed
  scheme (1 = slen, 2 = mapview, ...) is easiest to debug.
- The relative ordering of HUD creation, GLOBLOB seeding, PAGINAE
  publication, and the first MSG_OBJDATA burst in the original server is
  not derivable from the client (it tolerates any order subject to the
  RESID/TILES/mapview dependencies). The order in Phase 3.2 is the
  conservative one; capture real legacy server traffic if exact
  fidelity matters.
- Whether the original server ever sent widgets defined by resource
  Code entries (NEWWDG with a resource-path type, see src/haven/UI.java
  lines 172-180) during normal play is unknown; the client supports it,
  but all obviously server-driven widgets have built-in type names.
- `SESSERR_EXPR` (5) versus `SESSERR_AUTH` (1) for a syntactically valid
  but unknown token is a server policy choice; the client strings
  ("Authentication token expired" vs "Invalid authentication token") are
  the only distinction the user sees.
- The `savedtoken` length check (exactly 64 hex characters,
  src/haven/Bootstrap.java lines 155-156) implies 32-byte tokens, but
  nothing prevents a server from issuing other lengths; the client will
  only auto-use 32-byte ones.
- Protocol writeups of other H&H server builds sometimes mention extra
  MSG_SESS fields (for example a "bnames" list). This client build
  (src/haven/Session.java lines 676-682) sends exactly the five fields
  documented in Phase 2 - flavour, game name, PVER, username, cookie -
  and any additional trailing bytes would land inside the opaque cookie
  blob. Do not add fields after the cookie unless you control the
  client build as well.

## Server implementation notes: cluster mode (session 27)

The Rust server can run as a CLUSTER of independent processes:
`hnh-server --cluster "host:port,host:port" --node N`. Every node owns
the VisIndex cells the rendezvous hash (grid_owner.rs) assigns from the
shared membership list and simulates only the gobs standing in its own
cells. Players are always simulated by their HOME node (the node whose
UDP port the client connected to); a player standing in another node's
cell is published to that cell's owner and rendered there as a guest.
Sessions see foreign-authority gobs as guests through the same
visibility machinery as local gobs; gob ids are globally unique by
per-node slot stride, so no id remapping happens anywhere.

This is a DEVIATION from legacy behavior (legacy = one monolithic
server process). The client cannot tell the difference: the wire it
speaksis unchanged, ports are per-node configurable
(`--game-port/--auth-port/--res-port`), and spawn/move/pose/retract
blocks are byte-identical whether the gob is local or a guest.

Open questions: cross-node interaction relay (attacking/picking up a
guest gob must route to its authority); persistence is per-node today
(a character lives on the node that first accepted it).
