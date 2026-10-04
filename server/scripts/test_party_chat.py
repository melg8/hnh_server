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
"""
import os
import socket
import struct
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(REPO, "server", "target", "release", "hnh-server")

LIST_END, LIST_INT, LIST_STR, LIST_COORD, LIST_COL = 0, 1, 2, 3, 6
MSG_REL, MSG_MAPDATA, MSG_OBJDATA = 1, 5, 6
RMSG_NEWWDG, RMSG_WDGMSG, RMSG_DSTWDG, RMSG_PARTY, RMSG_RESID = 0, 1, 2, 7, 6
PD_LIST, PD_LEADER, PD_MEMBER = 0, 1, 2
OD_MOVE, OD_RES, OD_LINBEG, OD_LINSTEP, OD_BUDDY, OD_END = 1, 2, 3, 4, 15, 255
OD_LAYERS, OD_HEALTH = 6, 14


def le16(v):
    return struct.pack("<H", v & 0xFFFF)


def le32(v):
    return struct.pack("<i", v)


def havstr(s):
    return s.encode() + b"\x00"


def auth_cookie(username, password="x"):
    import hashlib
    import ssl
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection(("127.0.0.1", 1871), timeout=5)
    tls = ctx.wrap_socket(raw)

    def send_frame(ty, payload):
        tls.sendall(bytes([ty, len(payload)]) + payload)

    def recv_frame():
        head = b""
        while len(head) < 2:
            c = tls.recv(2 - len(head))
            if not c:
                raise RuntimeError("eof")
            head += c
        ln = head[1]
        body = b""
        while len(body) < ln:
            c = tls.recv(ln - len(body))
            if not c:
                raise RuntimeError("eof")
            body += c
        return head[0], body

    send_frame(1, username.encode())
    ty, _ = recv_frame()
    assert ty == 0, "CMD_USR rejected"
    send_frame(2, hashlib.sha256(password.encode()).digest())
    ty, body = recv_frame()
    assert ty == 0, "CMD_PASSWD rejected"
    return body


def ensure_server():
    """Start an isolated server if none is listening."""
    probe = socket.socket()
    probe.settimeout(0.4)
    try:
        probe.connect(("127.0.0.1", 1871))
        probe.close()
        return None
    except OSError:
        pass
    env = dict(os.environ)
    env["HNH_SAVE_FILE"] = os.path.join(REPO, "server", "target", "party-test-save.json")
    proc = subprocess.Popen(
        [BIN, "--seed", "42"],
        cwd=os.path.join(REPO, "server"),
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    deadline = time.time() + 30
    while time.time() < deadline:
        try:
            probe = socket.socket()
            probe.settimeout(0.4)
            probe.connect(("127.0.0.1", 1871))
            probe.close()
            return proc
        except OSError:
            time.sleep(0.4)
    raise RuntimeError("server did not come up")


class PartyClient:
    def __init__(self, username):
        self.username = username
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.settimeout(0.25)
        self.server = ("127.0.0.1", 1870)
        self.tseq = 0
        self.rseq = 0
        self.held = {}
        self.widgets = {}  # wid -> type name
        self.sm_args = {}  # sm wid -> option labels
        self.destroyed = set()
        self.charlist_id = None
        self.mapview_id = None
        self.scm_id = None
        self.chat_id = None
        self.pv_id = None
        self.chat_lines = []  # (text, color or None)
        self.party_msgs = []  # parsed RMSG_PARTY record lists
        self.items = {}  # wid -> tooltip
        self.resids = {}  # wire id -> name
        self.gobs = {}  # gobid -> {"res": name, "sdt": bytes, "pos": (x,y)}
        self.player_gob = None
        self.played = False

    # ---- session plumbing -------------------------------------------------
    def connect(self):
        cookie = auth_cookie(self.username)
        sess = (
            bytes([0])
            + le16(1)
            + havstr("Haven")
            + le16(2)
            + havstr(self.username)
            + cookie
        )
        for _ in range(8):
            self.sock.sendto(sess, self.server)
            try:
                data, _ = self.sock.recvfrom(65536)
                if data[0] == 0 and len(data) == 2 and data[1] == 0:
                    return
            except socket.timeout:
                continue
        raise RuntimeError("session not accepted")

    def send_rel(self, subs):
        out = bytes([1]) + le16(self.tseq)
        for i, p in enumerate(subs):
            if i < len(subs) - 1:
                out += bytes([p[0] | 0x80]) + le16(len(p) - 1) + p[1:]
            else:
                out += p
        self.tseq += len(subs)
        self.sock.sendto(out, self.server)

    def wdgmsg(self, wid, name, args=b""):
        self.send_rel([bytes([1]) + le16(wid) + havstr(name) + args])

    def pump(self, seconds):
        deadline = time.time() + seconds
        while time.time() < deadline:
            try:
                data, _ = self.sock.recvfrom(65536)
            except socket.timeout:
                continue
            self.on_datagram(data)

    def wait_for(self, predicate, timeout, step=0.25):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if predicate():
                return True
            self.pump(step)
        return False

    # ---- protocol handlers -------------------------------------------------
    def on_datagram(self, data):
        if data[0] == 2:
            return  # ack
        if data[0] == MSG_OBJDATA:
            self.on_objdata(data[2:])
            return
        if data[0] != MSG_REL:
            return
        seq = struct.unpack("<H", data[1:3])[0]
        off = 3
        while off < len(data):
            t = data[off]
            off += 1
            if t & 0x80:
                ln = struct.unpack("<H", data[off : off + 2])[0]
                body = data[off : off + ln]
                off += ln
            else:
                body = data[off:]
                off = len(data)
            t &= 0x7F
            if seq == self.rseq:
                self.on_rel(t, body)
                self.rseq = (self.rseq + 1) & 0xFFFF
                while self.rseq in self.held:
                    t2, b2 = self.held.pop(self.rseq)
                    self.on_rel(t2, b2)
                    self.rseq = (self.rseq + 1) & 0xFFFF
                self.sock.sendto(bytes([2]) + le16((self.rseq - 1) & 0xFFFF), self.server)
            elif ((seq - self.rseq) & 0xFFFF) < 0x8000:
                self.held[seq] = (t, body)
            seq = (seq + 1) & 0xFFFF

    def on_rel(self, t, body):
        if t == RMSG_NEWWDG:
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            self.widgets[wid] = name
            aoff = nend + 1 + 10  # skip x, y, parent
            args = list(self.parse_args(body[aoff:]))
            if name == "charlist":
                self.charlist_id = wid
            elif name == "mapview":
                self.mapview_id = wid
                for gy in (-1, 0, 1):
                    for gx in (-1, 0, 1):
                        self.sock.sendto(bytes([4]) + le32(gx) + le32(gy), self.server)
            elif name == "scm":
                self.scm_id = wid
            elif name == "slenchat":
                self.chat_id = wid
            elif name == "pv":
                self.pv_id = wid
            elif name == "sm":
                self.sm_args[wid] = [a for a in args if isinstance(a, str)]
            elif name == "item" and len(args) >= 4:
                tooltip = args[3] if isinstance(args[3], str) else ""
                self.items[wid] = tooltip
        elif t == RMSG_RESID:
            # u16 wire id, str name, u16 version
            wire = struct.unpack("<H", body[0:2])[0]
            end = body.index(0, 2)
            name = body[2:end].decode()
            self.resids[wire] = name
        elif t == RMSG_WDGMSG:
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            args = list(self.parse_args(body[nend + 1 :]))
            if name == "log" and wid == self.chat_id:
                text = args[0] if args else ""
                color = next((a for a in args if isinstance(a, tuple)), None)
                self.chat_lines.append((text, color))
        elif t == RMSG_DSTWDG:
            wid = struct.unpack("<H", body[0:2])[0]
            self.destroyed.add(wid)
            self.widgets.pop(wid, None)
            self.sm_args.pop(wid, None)
        elif t == RMSG_PARTY:
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
                    gid = struct.unpack("<i", body[off : off + 4])[0]
                    off += 4
                    if gid == -1:
                        break
                    ids.append(gid)
                records.append((PD_LIST, ids))
            elif tag == PD_LEADER:
                records.append((PD_LEADER, struct.unpack("<i", body[off : off + 4])[0]))
                off += 4
            elif tag == PD_MEMBER:
                gob = struct.unpack("<i", body[off : off + 4])[0]
                off += 4
                visible = body[off]
                off += 1
                pos = None
                if visible == 1:
                    pos = struct.unpack("<ii", body[off : off + 8])
                    off += 8
                color = tuple(body[off : off + 4])
                off += 4
                records.append((PD_MEMBER, gob, pos, color))
            else:
                break
        return records

    def parse_args(self, buf):
        off = 0
        while off < len(buf) and buf[off] != LIST_END:
            ty = buf[off]
            off += 1
            if ty == LIST_INT:
                yield struct.unpack("<i", buf[off : off + 4])[0]
                off += 4
            elif ty == LIST_STR:
                end = buf.index(0, off)
                yield buf[off:end].decode()
                off = end + 1
            elif ty == LIST_COORD:
                x, y = struct.unpack("<ii", buf[off : off + 8])
                off += 8
                yield (x, y)
            elif ty == LIST_COL:
                yield tuple(buf[off : off + 4])
                off += 4
            else:
                return

    def on_objdata(self, body):
        off = 0
        while off + 8 <= len(body):
            gobid = struct.unpack("<i", body[off : off + 4])[0]
            off += 4
            off += 4  # frame
            g = self.gobs.setdefault(gobid, {"res": None, "sdt": b"", "pos": None})
            while off < len(body):
                code = body[off]
                off += 1
                if code == OD_END:
                    break
                if code == OD_RES:
                    wire = struct.unpack("<H", body[off : off + 2])[0]
                    off += 2
                    if wire & 0x8000:
                        ln = body[off]
                        off += 1
                        g["sdt"] = body[off : off + ln]
                        off += ln
                        wire &= 0x7FFF
                    g["res"] = self.resids.get(wire)
                elif code == OD_MOVE:
                    x, y = struct.unpack("<ii", body[off : off + 8])
                    off += 8
                    g["pos"] = (x, y)
                elif code == OD_LINBEG:
                    _sx, _sy, tx, ty = struct.unpack("<iiii", body[off : off + 16])
                    off += 20  # 2x coord(8) + int32 steps
                    # The server's logical position is the destination;
                    # record it so walk-away assertions see real moves.
                    g["pos"] = (tx, ty)
                elif code == OD_LINSTEP:
                    off += 4
                elif code == OD_LAYERS:
                    off += 8
                elif code == OD_HEALTH:
                    off += 1
                elif code == OD_BUDDY:
                    end = body.index(0, off)
                    nm = body[off:end].decode(errors="replace")
                    if not hasattr(self, "buddy_names"):
                        self.buddy_names = {}
                    self.buddy_names[gobid] = nm
                    if nm == self.username:
                        self.player_gob = gobid
                    off = end + 3
                else:
                    return

    # ---- scenario actions --------------------------------------------------
    def play(self, name):
        assert self.charlist_id is not None
        self.send_rel(
            [
                bytes([1])
                + le16(self.charlist_id)
                + b"play\x00"
                + bytes([2])
                + name.encode()
                + b"\x00"
                + bytes([0])
            ]
        )

    def click_gob(self, gobid, pos):
        self.wdgmsg(
            self.mapview_id,
            "click",
            bytes([LIST_COORD]) + le32(0) + le32(0)
            + bytes([LIST_COORD]) + le32(pos[0]) + le32(pos[1])
            + bytes([LIST_INT]) + le32(1)
            + bytes([LIST_INT]) + le32(0)
            + bytes([LIST_INT]) + le32(gobid)
            + bytes([LIST_COORD]) + le32(pos[0]) + le32(pos[1])
            + bytes([LIST_END]),
        )

    def walk_to(self, x, y):
        self.wdgmsg(
            self.mapview_id,
            "click",
            bytes([LIST_COORD]) + le32(0) + le32(0)
            + bytes([LIST_COORD]) + le32(x) + le32(y)
            + bytes([LIST_INT]) + le32(1)
            + bytes([LIST_INT]) + le32(0)
            + bytes([LIST_END]),
        )

    def chat(self, line):
        self.wdgmsg(
            self.chat_id,
            "msg",
            bytes([LIST_STR]) + havstr(line) + bytes([LIST_END]),
        )

    def flower_choice(self, wid, idx=0):
        self.wdgmsg(wid, "cl", bytes([LIST_INT]) + le32(idx) + bytes([LIST_END]))

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
            if name is None or (getattr(self, "buddy_names", {}).get(gob) == name):
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
    # (water/mountain), so try several until the server-confirmed
    # destination (LINBEG endpoint) lands beyond the radius.
    origin = c.my_pos() or (0, 0)
    candidates = [
        (6000, 0),
        (0, 6000),
        (-6000, 0),
        (0, -6000),
        (4000, 4000),
        (-4000, -4000),
        (4000, -4000),
        (-4000, 4000),
    ]
    moved = False
    for dx, dy in candidates:
        c.walk_to(origin[0] + dx, origin[1] + dy)
        dest = c.my_pos()
        if dest is None or dest == origin:
            c.pump(0.4)
            dest = c.my_pos()
        if dest is not None and (dest[0] - origin[0]) ** 2 + (dest[1] - origin[1]) ** 2 > 500 ** 2:
            moved = True
            break
    assert moved, "could not walk the out-of-range client away (pos=%s)" % (c.my_pos(),)
    c.pump(1.0)
    print("C walked away to", c.my_pos())

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
    server_proc = ensure_server()
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
