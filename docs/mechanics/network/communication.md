# Communication: Area Chat and Parties

> **Sources:** src/haven/ChatHW.java, src/haven/Chatwindow.java, src/haven/Party.java, src/haven/Partyview.java, src/haven/Message.java, src/haven/RemoteUI.java, docs/mechanics/network/network-protocol.md (widget protocol, RMSG_PARTY)

## Summary

This document covers the two player-to-player communication systems the
server implements: the Area Chat text channel and the party system. Both
are pure server-driven widget flows — the client ships no chat or party
login logic beyond the generic widget protocol, so every behavior below is
a server decision verified against the client sources.

## Area chat

### Widget wiring

- The server creates one `slenchat` widget per session at world entry with
  args `(String title, int closable)` — ChatHW.java registers the
  `slenchat` factory and takes args[0] as the window title and optional
  args[1] as the closable flag. The server uses title `"Area Chat"` and
  `closable = 0`; the client hard-codes `Area Chat` to hide its close
  button (`ChatHW` constructor: `if (title.equals("Area Chat")) cbtn.hide()`),
  so the window is permanent.
- Typed lines travel client-to-server as a widget message from the chat
  widget: `msg` with one string argument (`ChatHW.wdgmsg` forwards the
  TextEntry `activate` payload verbatim).
- Delivered lines travel server-to-client as a widget message to the chat
  widget: `log` with args `(String text[, Color color[, int urgent]])`
  (`ChatHW.uimsg`, case `"log"`). The color arg is optional; a color
  present in the "darken" set is darkened client-side, so the server
  avoids pure green/cyan/yellow for system lines.

### Relay rules (server policy)

- **Radius**: a line is relayed to sessions whose player gob sits within
  `AREA_CHAT_RADIUS` of the sender (Euclidean, subtiles). The radius
  equals `VIEW_RADIUS` (500 subtiles, ~45 tiles): *you hear what you can
  see*. This is a deliberate server policy — the legacy radius is not
  recoverable from this client.
- **Format**: `"<Name>: <text>"` with no color arg (client default color).
- **Echo**: the sender receives their own line (distance 0 always passes
  the radius filter).
- **Sanitization** (`chat::sanitize`): lines are trimmed; empty lines are
  dropped; lines longer than 240 characters are refused (the client
  `Textlog` renders one row per entry and long lines would overflow it).
- **System lines**: server-to-one-player notifications (skill gate
  refusals, party notices, invite prompts) ride the same `log` message
  with the soft-red color `(255, 128, 128)`. They are private to the
  target session.

## Parties

### State

- `World.parties` holds formed parties (`PartyState`: ordered member gob
  ids + leader gob id). Membership order is join order and doubles as the
  marker-color index. One party per player; joining another party requires
  leaving the current one first (the invite flow refuses partyed players).
- **Cap**: `party::MAX_MEMBERS = 10`. The cap is server policy recorded
  here (the legacy cap is not visible in this client; 10 matches the
  commonly cited legacy party size).
- **Colors**: `party::MEMBER_COLORS` assigns each member index a distinct
  marker color used in `PD_MEMBER` records; the client draws minimap
  markers and map lines with it. Legacy colors are not recoverable from
  the client, so the palette is server-defined.

### Invite choreography (wire-only consent, no client changes)

1. Player A clicks player B's gob (mapview `click` carrying a gob id).
2. The server opens A a flower menu (`sm`) with options
   `["Invite to party", "Cancel"]`. Refusals are delivered as chat system
   lines before any menu opens: self-invite, target already in a party,
   the clicker is a member but not the leader (`Only the party leader can
   invite.`), or the clicker's party is full.
3. A confirms (petal 0). The server opens B a flower menu with
   `["Join <A>'s party", "Decline"]` and sends B the system line
   `"<A> invites you to join their party."`.
4. B confirms (petal 0): the party is created (A becomes leader) or
   extended; the state broadcast below fires; B gets `You joined the
   party.` and A gets `"<B> joined your party."`.
5. Decline/cancel (any other petal or a closed menu) just closes the
   client menu; no state changes.

A and B must both still be partyless when the accept lands; stale offers
re-check both sides and refuse with a system line instead of corrupting
state.

### State broadcast

Every membership change re-broadcasts the full party state to each member
session (RMSG_PARTY stream, Party.java tags):

- `PD_LIST` — the full member gob id list (empty list on disband clears
  the client roster state; `Party.msg` rebuilds `memb` from the list).
- `PD_LEADER` — the leader's gob id.
- `PD_MEMBER` — per member: gob id, visible flag (1 with the last known
  position), and the member's marker color.

Joining also creates each member a `pv` roster widget (Partyview.java,
args = the player's own gob id; the widget shows the *other* members'
avatar views and the Leave button).

### Leave and disband

- The roster's Leave button sends the widget message `leave`.
- Removing a non-leader keeps the party; removing the leader transfers
  leadership to the earliest remaining joiner (`You are now the party
  leader.`); dropping to one member disbands the party entirely.
- Disband destroys every member's `pv` widget and broadcasts an empty
  `PD_LIST` so the client's `Party.memb` clears.
- Logging out removes the player from their party through the same path
  (session teardown calls the leave routine before the gob is killed).

## Server implementation notes (implemented flow)

- Modules: `chat.rs` (sanitize, radius, system color), `party.rs`
  (membership bookkeeping, palette), `resources.rs` (`wdg::party` RMSG
  encoder), `game.rs` (widget handlers: `slenchat` `msg`, `pv` `leave`,
  player-click invite menus; `sync_party` broadcasts; teardown cleanup).
- The area chat relay is an O(sessions) scan per line (chat lines are
  rare events; the 10 Hz tick hot path is untouched).
- No periodic `PD_MEMBER` position refresh is sent: the client resolves
  member positions from the object cache when a member gob is in view,
  and the broadcast-time position is the fallback for out-of-sight
  members. Markers for far members update on the next membership change
  or re-entry; this is an accepted simplification documented here.

## Open questions

- Legacy area-chat radius: not recoverable from this client; the server
  currently reuses the view radius. If a period source fixes the radius
  (wiki/forum), move `AREA_CHAT_RADIUS` to its own constant.
- Legacy party member colors and the exact leadership-transfer rule on
  leader logout are server folklore; the implemented rules (palette,
  earliest-joiner transfer) are recorded here as server policy.
- Party invites in legacy used the right-click flower menu the same way,
  but the exact legacy prompt strings are unknown; the current strings
  are server-defined.
