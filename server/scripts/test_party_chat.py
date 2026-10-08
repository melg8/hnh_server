#!/usr/bin/env python3
"""End-to-end chat + party verification (docs/mechanics/network/communication.md).

Chain under test (wire level, the same path the Java client drives):
  auth -> world entry -> slenchat "Area Chat" widget appears
  -> client "msg" line relays to sessions inside the area radius
  -> far-away session receives nothing
  -> player-gob click -> invite flower menu -> system line to the invitee
  -> accept -> RMSG_PARTY member list + leader + markers + pv roster widget
  -> leave -> disband broadcast clears client state.

Modes:
  chatbot   chat relay + radius isolation + one-sided system line
  partybot  full invite/accept/leave/disband party choreography

Auto-starts the server binary on an isolated save when 1871 is free;
otherwise reuses the already-running server.

The transport, the session driver, the chat/flower-menu/widget tracking
and the DSTWDG bookkeeping live in hnhlib.py.
"""
import os
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import (  # noqa: E402
    LIST_END,
    LIST_STR,
    REPO,
    WireClient,
    ensure_server,
    havstr,
    parse_objdata,
)

PD_LIST, PD_LEADER, PD_MEMBER = 0, 1, 2


def ensure_party_server():
    """Isolated server on the party scenario save."""
    return ensure_server(
        save_path=os.path.join(REPO, "server", "target", "party-test-save.json")
    )


class PartyClient(WireClient):
    """Party/chat scenario client.

    Movement fidelity (session 20 semantics): the server's logical
    position is the ON-PATH interpolated point, never the destination
    ahead of time - the mv phase set_pos()es lm.pos_at(now) every tick
    and only pins the destination on arrival. The walk-away assertions
    must judge distance by the same measure, so this subclass tracks
    the client-side interpolation: LINBEG records the path, LINSTEP
    advances the position along it (arrival = l >= c pins the target).
    Party records and the scenario helpers keep their old shapes.
    """

    def __init__(self, username):
        super().__init__(username)
        self.party_msgs = []  # parsed RMSG_PARTY record lists
        self.linpaths = {}  # gobid -> (sx, sy, tx, ty, c) active path

    def _gob(self, gobid):
        return self.gobs.setdefault(gobid, {
            "res": None, "sdt": b"", "pos": None,
            "linbeg": None, "linbegs": [], "linsteps": [], "moves": [],
        })

    def on_objdata(self, body):
        for gobid, _frame, ops in parse_objdata(body):
            for op, arg in ops:
                if op == "LINBEG":
                    sx, sy, tx, ty, c = arg
                    self._gob(gobid)["pos"] = (sx, sy)
                    self.linpaths[gobid] = (sx, sy, tx, ty, max(c, 1))
                elif op == "LINSTEP":
                    p = self.linpaths.get(gobid)
                    if p is None:
                        continue
                    sx, sy, tx, ty, c = p
                    l = arg
                    if l >= c:
                        self._gob(gobid)["pos"] = (tx, ty)
                        self.linpaths.pop(gobid, None)
                    else:
                        self._gob(gobid)["pos"] = (
                            sx + (tx - sx) * l // c, sy + (ty - sy) * l // c)
                elif op == "MOVE":
                    # A plain MOVE (spawn/teleport/arrival pin) replaces
                    # any active path.
                    self.linpaths.pop(gobid, None)
        super().on_objdata(body)

    def on_event(self, t, body):
        # RMSG_PARTY is a party-domain record stream: keep the raw
        # records for the choreography assertions.
        if t == 7:  # RMSG_PARTY
            self.party_msgs.append(self.parse_party(body))

    @staticmethod
    def parse_party(body):
        """Parse one RMSG_PARTY stream into a record list."""
        off = 0
        records = []
        while off < len(body):
            tag = body[off]
            off += 1
            if tag == PD_LIST:
                ids = []
                while off + 4 <= len(body):
                    gid = struct.unpack("<i", body[off:off + 4])[0]
                    off += 4
                    if gid == -1:
                        break
                    ids.append(gid)
                records.append((PD_LIST, ids))
            elif tag == PD_LEADER:
                records.append((PD_LEADER, struct.unpack("<i", body[off:off + 4])[0]))
                off += 4
            elif tag == PD_MEMBER:
                gob = struct.unpack("<i", body[off:off + 4])[0]
                off += 4
                visible = body[off]
                off += 1
                pos = None
                if visible == 1:
                    pos = struct.unpack("<ii", body[off:off + 8])
                    off += 8
                color = tuple(body[off:off + 4])
                off += 4
                records.append((PD_MEMBER, gob, pos, color))
            else:
                break
        return records

    # ---- scenario actions --------------------------------------------------
    def walk_to(self, x, y):
        self.click_ground(x, y)

    def chat(self, line):
        self.wdgmsg(
            self.chat_id,
            "msg",
            bytes([LIST_STR]) + havstr(line) + bytes([LIST_END]),
        )

    def open_sm(self):
        """Return the currently open sm widget id (or None)."""
        return next((w for w, n in self.widgets.items() if n == "sm"), None)

    def my_pos(self):
        info = self.gobs.get(self.player_gob)
        return info["pos"] if info else None

    def find_other_player(self, name=None):
        """First visible gob that is another player's avatar, optionally
        matched by the OD_BUDDY character name (robust when stale
        sessions from earlier battery flows still stream their avatars)."""
        for gob, info in sorted(self.gobs.items()):
            if gob == self.player_gob or info["res"] != "gfx/borka/body":
                continue
            if name is None or self.buddy_names.get(gob) == name:
                return gob
        return None


def enter_world(username):
    c = PartyClient(username)
    c.connect()
    c.pump(1.5)
    c.play(username)
    ok = c.wait_for(lambda: c.mapview_id is not None and c.player_gob is not None, 12)
    assert ok or c.player_gob is not None, "world entry incomplete for %s" % username
    assert c.chat_id is not None, "Area Chat widget missing at world entry"
    c.pump(0.5)
    return c


def run_chatbot():
    a_name = "chatA%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    b_name = "chatB%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    c_name = "chatC%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    a = enter_world(a_name)
    b = enter_world(b_name)
    c = enter_world(c_name)
    print("three sessions entered world; area chat widgets present")

    # Walk C out of the area radius. Terrain may refuse some directions
    # (water/mountain), so try several until the client's interpolated
    # position - the same on-path measure the server's chat filter uses
    # - crosses beyond the radius plus a safety margin.
    origin = c.my_pos() or (0, 0)
    candidates = [
        (600, 0),
        (0, 600),
        (-600, 0),
        (0, -600),
        (425, 425),
        (-425, -425),
        (425, -425),
        (-425, 425),
    ]

    def beyond_radius(p):
        return (p is not None
                and (p[0] - origin[0]) ** 2 + (p[1] - origin[1]) ** 2 > 550 ** 2)

    moved = False
    for dx, dy in candidates:
        c.walk_to(origin[0] + dx, origin[1] + dy)
        if c.wait_for(lambda: beyond_radius(c.my_pos()), 24):
            moved = True
            break
    assert moved, "could not walk the out-of-range client away (pos=%s)" % (c.my_pos(),)
    c.pump(1.0)
    print("C walked beyond the radius to", c.my_pos())

    # A chats: A hears the echo, B hears it, C must not.
    marker_a = "greeting-from-a-%d" % (os.getpid(),)
    a.chat(marker_a)
    got_a = a.wait_for(lambda: any(marker_a in t for t, _ in a.chat_lines), 4)
    assert got_a, "sender never heard their own echo (lines=%s)" % (a.chat_lines,)
    got_b = b.wait_for(lambda: any(marker_a in t and a_name in t for t, _ in b.chat_lines), 4)
    assert got_b, "in-range peer never received the relay (lines=%s)" % (b.chat_lines,)
    c.pump(1.5)
    assert not any(marker_a in t for t, _ in c.chat_lines), (
        "out-of-range client received the chat relay (lines=%s)" % (c.chat_lines,)
    )
    print("relay: sender echo + in-range delivery + out-of-range silence")

    # B answers; A receives it back.
    marker_b = "reply-from-b-%d" % (os.getpid(),)
    b.chat(marker_b)
    got_a2 = a.wait_for(lambda: any(marker_b in t and b_name in t for t, _ in a.chat_lines), 4)
    assert got_a2, "chat back-channel broken (lines=%s)" % (a.chat_lines,)
    print("back-channel: B -> A relayed")

    # System line: A invites B (menu on A), B gets the one-sided prompt.
    ok = a.wait_for(lambda: a.find_other_player(b_name) is not None, 6)
    assert ok, "A never saw B's avatar"
    b_gob = a.find_other_player(b_name)
    assert b_gob is not None, "A never saw B's avatar"
    b_pos = a.gobs[b_gob]["pos"]
    a.click_gob(b_gob, b_pos)
    ok = a.wait_for(lambda: a.open_sm() is not None, 4)
    assert ok, "invite menu never opened for the clicker"
    sm = a.open_sm()
    assert "Invite to party" in a.sm_args.get(sm, []), (
        "invite menu options wrong: %s" % (a.sm_args.get(sm),)
    )
    a.flower_choice(sm, 0)
    got_sys = b.wait_for(
        lambda: any(
            a_name in t and "invites you" in t and col is not None
            for t, col in b.chat_lines
        ),
        4,
    )
    assert got_sys, "invitee never got the colored system line (lines=%s)" % (
        b.chat_lines,
    )
    assert not any(
        "invites you" in t for t, _ in a.chat_lines
    ), "the clicker must not receive the invitee's system prompt"
    # Decline to close the invitee menu cleanly.
    bsm = b.open_sm()
    assert bsm is not None, "invitee join menu missing"
    b.flower_choice(bsm, 1)
    print("system line reached exactly the invitee (colored, one-sided)")
    print("CHAT FLOW: OK")


def run_partybot():
    a_name = "ptA%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    b_name = "ptB%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    a = enter_world(a_name)
    b = enter_world(b_name)
    print("two sessions entered world")

    # A invites B.
    ok = a.wait_for(lambda: a.find_other_player(b_name) is not None, 6)
    assert ok, "A never saw B's avatar"
    b_gob = a.find_other_player(b_name)
    assert b_gob is not None, "A never saw B's avatar"
    a.click_gob(b_gob, a.gobs[b_gob]["pos"])
    ok = a.wait_for(lambda: a.open_sm() is not None, 4)
    assert ok, "invite menu never opened"
    a.flower_choice(a.open_sm(), 0)
    ok = b.wait_for(lambda: b.open_sm() is not None, 4)
    assert ok, "join menu never reached the invitee"

    # B accepts -> RMSG_PARTY on both with the full member list.
    b.flower_choice(b.open_sm(), 0)

    def party_formed(c):
        for recs in c.party_msgs:
            lists = [r[1] for r in recs if r[0] == PD_LIST]
            leaders = [r[1] for r in recs if r[0] == PD_LEADER]
            members = [r for r in recs if r[0] == PD_MEMBER]
            if any(a.player_gob in ids and b.player_gob in ids for ids in lists):
                if leaders and leaders[0] == a.player_gob:
                    if len(members) == 2:
                        return True
        return False

    ok = a.wait_for(lambda: party_formed(a), 5)
    assert ok, "leader never saw the formed party state: %s" % (a.party_msgs,)
    ok = b.wait_for(lambda: party_formed(b), 5)
    assert ok, "member never saw the formed party state: %s" % (b.party_msgs,)
    assert a.pv_id is not None and b.pv_id is not None, "pv roster widgets missing"
    print("party formed: list+leader+2 markers broadcast, pv widgets created")

    # Marker colors differ per member.
    colors = {}
    for recs in a.party_msgs:
        for r in recs:
            if r[0] == PD_MEMBER:
                colors[r[1]] = r[3]
    assert len({colors[a.player_gob], colors[b.player_gob]}) == 2, (
        "member marker colors must differ: %s" % (colors,)
    )

    # B leaves -> party of two disbands: empty list broadcast, pv destroyed.
    assert b.pv_id in b.widgets, "B's pv widget gone before leave"
    b.wdgmsg(b.pv_id, "leave")

    def disbanded(c):
        # The last received state for this client must be an empty list.
        recs = c.party_msgs[-1] if c.party_msgs else []
        lists = [r[1] for r in recs if r[0] == PD_LIST]
        return bool(lists and lists[-1] == [])

    ok = a.wait_for(lambda: disbanded(a), 5)
    assert ok, "leader never saw the disband broadcast: %s" % (a.party_msgs[-2:],)
    ok = b.wait_for(lambda: disbanded(b), 5)
    assert ok, "leaver never saw the disband broadcast: %s" % (b.party_msgs[-2:],)
    a.wait_for(lambda: a.pv_id in a.destroyed, 3)
    b.wait_for(lambda: b.pv_id in b.destroyed, 3)
    assert a.pv_id in a.destroyed, "leader's pv widget was not destroyed"
    assert b.pv_id in b.destroyed, "leaver's pv widget was not destroyed"
    print("leave -> disband: empty PD_LIST broadcast, pv widgets destroyed")

    # Bookkeeping really cleared: A can re-invite B immediately.
    a.click_gob(b_gob, a.gobs.get(b_gob, {}).get("pos") or (0, 0))
    ok = a.wait_for(lambda: a.open_sm() is not None, 4)
    assert ok, "re-invite blocked after disband (bookkeeping leak)"
    print("re-invite after disband works (state cleared)")
    print("PARTY FLOW: OK")


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "chatbot"
    server_proc = ensure_party_server()
    try:
        if mode == "chatbot":
            run_chatbot()
        elif mode == "partybot":
            run_partybot()
        else:
            raise SystemExit("unknown mode: %s" % mode)
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


if __name__ == "__main__":
    main()
