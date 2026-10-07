#!/usr/bin/env python3
"""hnhlib - shared black-box wire harness for the server probes.

Every script in this directory drives the same protocol path the Java
client uses: TLS auth -> cookie -> MSG_SESS -> reliable RMSG stream +
raw MAPDATA/OBJDATA datagrams. Before this module existed each probe
re-implemented the plumbing (auth_cookie, the reliability walk, the
OBJDATA op table), so a wire change had to be patched in eight places.

This module is the single source of that plumbing:

  - wire constants (byte-exact with server/crates/hnh-proto/src/consts.rs)
  - le16 / le32 / havstr encoders
  - auth_cookie(): TLS auth handshake -> session cookie
  - ensure_server(): boots an isolated release server when none listens
  - parse_objdata(): the OBJDATA op decoder, semantics matched to the
    server's own bots.rs parse_objdata (OD_REM is a no-payload op, an
    OD_OVERLAY resid of 65535 is a removal without sdt, a flag-1 block
    carries no ops)
  - WireClient: the session driver (reliable stream + cumulative ACK +
    hold-back, widget/res/gob tracking, the real client's bootstrap
    behaviors: 3x3 MAPREQ on mapview bind, `chr` request on slen bind,
    batched MSG_OBJACK) with an on_event() hook for script-specific
    widgets.

The op table lives here ONCE. When the server encoder grows a new op,
update parse_objdata and every probe sees it.
"""
import hashlib
import os
import socket
import ssl
import struct
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(REPO, "server", "target", "release", "hnh-server")
GAME_PORT = 1870
AUTH_PORT = 1871

# Wire constants (mirror crates/hnh-proto/src/consts.rs).
MSG_SESS, MSG_REL, MSG_ACK, MSG_BEAT, MSG_MAPREQ = 0, 1, 2, 3, 4
MSG_MAPDATA, MSG_OBJDATA, MSG_OBJACK, MSG_CLOSE = 5, 6, 7, 8
RMSG_NEWWDG, RMSG_WDGMSG, RMSG_DSTWDG, RMSG_MAPIV = 0, 1, 2, 3
RMSG_GLOBLOB, RMSG_PAGINAE, RMSG_RESID, RMSG_PARTY = 4, 5, 6, 7
RMSG_SFX, RMSG_CATTR, RMSG_MUSIC, RMSG_TILES, RMSG_BUFF = 8, 9, 10, 11, 12
OD_REM, OD_MOVE, OD_RES, OD_LINBEG, OD_LINSTEP = 0, 1, 2, 3, 4
OD_SPEECH, OD_LAYERS, OD_DRAWOFF, OD_LUMIN, OD_AVATAR = 5, 6, 7, 8, 9
OD_FOLLOW, OD_HOMING, OD_OVERLAY, OD_HEALTH, OD_BUDDY, OD_END = 10, 11, 12, 14, 15, 255
SESSERR_AUTH, SESSERR_BUSY, SESSERR_CONN, SESSERR_PVER, SESSERR_EXPR = 1, 2, 3, 4, 5
PVER = 2
LIST_END, LIST_INT, LIST_STR, LIST_COORD, LIST_COLOR = 0, 1, 2, 3, 6

# The character attributes CharWnd.baseval/skillval/Belief/Study
# dereference via glob.cattr.get(); one missing name NPEs the client.
REQUIRED_CATTR = {
    "str", "agil", "intel", "cons", "perc", "csm", "dxt", "psy",
    "expmod",
    "unarmed", "melee", "ranged", "explore", "stealth", "sewing",
    "smithing", "carpentry", "cooking", "farming", "survive",
    "life", "night", "civil", "nature", "martial", "change",
}


def le16(v):
    return struct.pack("<H", v & 0xFFFF)


def le32(v):
    return struct.pack("<i", v)


def havstr(s):
    return s.encode() + b"\x00"


def auth_cookie(username, password="x", port=AUTH_PORT):
    """TLS auth handshake -> session cookie (dev policy: any password)."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection(("127.0.0.1", port), timeout=5)
    tls = ctx.wrap_socket(raw)

    def send_frame(ty, payload):
        tls.sendall(bytes([ty, len(payload)]) + payload)

    def recv_frame():
        head = b""
        while len(head) < 2:
            ch = tls.recv(2 - len(head))
            if not ch:
                raise RuntimeError("eof")
            head += ch
        ln = head[1]
        body = b""
        while len(body) < ln:
            ch = tls.recv(ln - len(body))
            if not ch:
                raise RuntimeError("eof")
            body += ch
        return head[0], body

    send_frame(1, username.encode())
    ty, _ = recv_frame()
    assert ty == 0, "CMD_USR rejected"
    send_frame(2, hashlib.sha256(password.encode()).digest())
    ty, body = recv_frame()
    assert ty == 0, "CMD_PASSWD rejected"
    return body


def ensure_server():
    """Start an isolated server (fresh save) if none is listening."""
    probe = socket.socket()
    probe.settimeout(0.4)
    try:
        probe.connect(("127.0.0.1", AUTH_PORT))
        probe.close()
        return None
    except OSError:
        pass
    env = dict(os.environ)
    env["HNH_LP_RATE"] = "1000"
    save_path = os.path.join(REPO, "server", "target", "build-test-save.json")
    # Fresh world: persistent plans from earlier runs would occupy the
    # spawn-area tiles and intercept this run's itemacts.
    try:
        os.remove(save_path)
    except OSError:
        pass
    env["HNH_SAVE_FILE"] = save_path
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
            probe.connect(("127.0.0.1", AUTH_PORT))
            probe.close()
            return proc
        except OSError:
            time.sleep(0.4)
    raise RuntimeError("server did not come up")


def parse_objdata(body):
    """Decode one OBJDATA datagram payload into per-gob blocks.

    Yields (gobid, frame, ops) with ops a list of (name, value) pairs.
    The op table matches bots.rs parse_objdata exactly:
      - a flag-1 block IS the removal; it carries no ops,
      - OD_REM (0) is a no-payload op followed by OD_END,
      - OD_OVERLAY with raw resid 65535 is an overlay removal (no sdt),
      - OD_RES with the 0x8000 flag carries a u8 length + sdt bytes,
      - OD_LAYERS is base u16 + layer u16s to 65535; OD_AVATAR drops
        the base u16.
    """
    off = 0
    while off + 8 <= len(body):
        fl = body[off]
        off += 1
        gobid = struct.unpack("<i", body[off:off + 4])[0]
        off += 4
        frame = struct.unpack("<i", body[off:off + 4])[0]
        off += 4
        ops = []
        if fl & 1:
            ops.append(("REMOVE", frame - 1))
        else:
            while off < len(body):
                code = body[off]
                off += 1
                if code == OD_END:
                    break
                if code == OD_REM:
                    ops.append(("REMOVE", frame - 1))
                elif code == OD_MOVE:
                    x, y = struct.unpack("<ii", body[off:off + 8])
                    off += 8
                    ops.append(("MOVE", (x, y)))
                elif code == OD_RES:
                    wire = struct.unpack("<H", body[off:off + 2])[0]
                    off += 2
                    sdt = b""
                    if wire & 0x8000:
                        ln = body[off]
                        off += 1
                        sdt = body[off:off + ln]
                        off += ln
                        wire &= 0x7FFF
                    ops.append(("RES", (wire, sdt)))
                elif code == OD_LINBEG:
                    sx, sy, tx, ty, c = struct.unpack("<iiiii", body[off:off + 20])
                    off += 20
                    ops.append(("LINBEG", (sx, sy, tx, ty, c)))
                elif code == OD_LINSTEP:
                    l = struct.unpack("<i", body[off:off + 4])[0]
                    off += 4
                    ops.append(("LINSTEP", l))
                elif code == OD_SPEECH:
                    off += 8
                    end = body.index(0, off)
                    text = body[off:end].decode(errors="replace")
                    off = end + 1
                    ops.append(("SPEECH", text))
                elif code in (OD_LAYERS, OD_AVATAR):
                    base = None
                    if code == OD_LAYERS:
                        base = struct.unpack("<H", body[off:off + 2])[0]
                        off += 2
                    ids = []
                    while True:
                        layer = struct.unpack("<H", body[off:off + 2])[0]
                        off += 2
                        if layer == 0xFFFF:
                            break
                        ids.append(layer)
                    ops.append(
                        ("LAYERS", (base, ids)) if base is not None
                        else ("AVATAR", ids)
                    )
                elif code == OD_DRAWOFF:
                    off += 8
                    ops.append(("DRAWOFF", None))
                elif code == OD_LUMIN:
                    off += 11
                    ops.append(("LUMIN", None))
                elif code == OD_FOLLOW:
                    oid = struct.unpack("<i", body[off:off + 4])[0]
                    off += 4
                    if oid != -1:
                        off += 1 + 8
                    ops.append(("FOLLOW", oid))
                elif code == OD_HOMING:
                    oid = struct.unpack("<i", body[off:off + 4])[0]
                    off += 4
                    if oid != -1:
                        off += 8 + 2
                    ops.append(("HOMING", oid))
                elif code == OD_OVERLAY:
                    olid = struct.unpack("<i", body[off:off + 4])[0]
                    off += 4
                    raw = struct.unpack("<H", body[off:off + 2])[0]
                    off += 2
                    if raw != 0xFFFF and raw & 0x8000:
                        ln = body[off]
                        off += 1 + ln
                    ops.append(("OVERLAY", (olid, raw & 0x7FFF)))
                elif code == OD_HEALTH:
                    q = body[off]
                    off += 1
                    ops.append(("HEALTH", q))
                elif code == OD_BUDDY:
                    end = body.index(0, off)
                    name = body[off:end].decode(errors="replace")
                    off = end + 1 + 2
                    ops.append(("BUDDY", name))
                else:
                    # Unknown op: stop the block the way the client's
                    # reader would fail rather than desync.
                    break
        yield gobid, frame, ops


class WireClient:
    """One black-box client session over the real UDP path.

    Tracks what the real client's reader tracks: widgets by id, RESID
    wire-name map, gob state (res/sdt/pos + movement streams), item
    widgets, the flower menu, cattr and paginae pushes. Scripts subclass
    it (or use it directly) and override on_event() for their specific
    widgets instead of re-implementing the transport.
    """

    def __init__(self, username, request_chr=True, send_objacks=False):
        self.username = username
        self.request_chr = request_chr
        self.send_objacks = send_objacks
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.settimeout(0.25)
        self.server = ("127.0.0.1", GAME_PORT)
        self.tseq = 0
        self.rseq = 0
        self.held = {}
        self.widgets = {}
        self.charlist_id = None
        self.mapview_id = None
        self.scm_id = None
        self.slen_id = None
        self.sm_wid = None
        self.sm_opts = []
        self.resids = {}  # wire id -> name
        self.gobs = {}  # gobid -> {"res","sdt","pos","linbeg","linsteps","moves"}
        self.item_info = {}  # item wid -> {"res","ql","tt"}
        self.player_gob = None
        self.place_seen = None  # mapview `place` uimsg args
        self.cattr_names = set()
        self.cattr_at_chr = None  # snapshot when the `chr` widget arrives
        self.paginae_atk = set()
        self.mapdata_datagrams = 0
        self.objdata_datagrams = 0
        self.objacks = {}  # gobid -> last frame seen (SWorker mirror)
        self.last_ack = 0.0

    # ---- session plumbing -------------------------------------------------
    def connect(self):
        cookie = auth_cookie(self.username)
        sess = (
            bytes([MSG_SESS])
            + le16(1)
            + havstr("Haven")
            + le16(PVER)
            + havstr(self.username)
            + cookie
        )
        for _ in range(8):
            self.sock.sendto(sess, self.server)
            try:
                data, _ = self.sock.recvfrom(65536)
                if data[0] == MSG_SESS and len(data) == 2 and data[1] == 0:
                    return
                if data[0] == MSG_SESS:
                    raise RuntimeError("session rejected err=%d" % data[1])
            except socket.timeout:
                continue
        raise RuntimeError("session not accepted")

    def send_rel(self, subs):
        out = bytes([MSG_REL]) + le16(self.tseq)
        for i, p in enumerate(subs):
            if i < len(subs) - 1:
                out += bytes([p[0] | 0x80]) + le16(len(p) - 1) + p[1:]
            else:
                out += p
        self.tseq += len(subs)
        self.sock.sendto(out, self.server)

    def wdgmsg(self, wid, name, args=b""):
        self.send_rel([bytes([RMSG_WDGMSG]) + le16(wid) + havstr(name) + args])

    def widget_by_name(self, name):
        for wid, n in self.widgets.items():
            if n == name:
                return wid
        return None

    def return_cursor(self):
        """Release the held (cursor) stack back into the inventory.

        Mirrors the legacy client's drag release inside the inventory
        grid: the inv window `drop` wdgmsg. The plan sink leaves the
        undelivered remainder of a stack on the drag cursor (stone x4
        starter stack against the oven's stone x2 demand, session 36);
        the remainder must be stowed before another stack can be taken
        (one cursor item at a time, game/items.rs inv_take).
        """
        inv = self.widget_by_name("inv")
        if inv is None:
            return False
        self.wdgmsg(inv, "drop", bytes([LIST_END]))
        return True

    def mapreq(self, gx, gy):
        self.sock.sendto(bytes([MSG_MAPREQ]) + le32(gx) + le32(gy), self.server)

    def pump(self, seconds):
        deadline = time.time() + seconds
        while time.time() < deadline:
            now = time.time()
            if self.send_objacks and self.objacks and now - self.last_ack > 0.2:
                # Client SWorker mirror: one batched MSG_OBJACK datagram.
                msg = bytes([MSG_OBJACK])
                for gid, frame in self.objacks.items():
                    msg += le32(gid) + le32(frame)
                self.sock.sendto(msg, self.server)
                self.last_ack = now
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
        if data[0] == MSG_ACK:
            return
        if data[0] == MSG_MAPDATA:
            self.mapdata_datagrams += 1
            return
        if data[0] == MSG_OBJDATA:
            self.objdata_datagrams += 1
            self.on_objdata(data[1:])
            return
        if data[0] != MSG_REL:
            return
        seq = struct.unpack("<H", data[1:3])[0]
        off = 3
        while off < len(data):
            t = data[off]
            off += 1
            if t & 0x80:
                ln = struct.unpack("<H", data[off:off + 2])[0]
                body = data[off:off + ln]
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
                self.sock.sendto(bytes([MSG_ACK]) + le16((self.rseq - 1) & 0xFFFF), self.server)
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
                # Real-client behavior: request the 3x3 neighborhood.
                for gy in (-1, 0, 1):
                    for gx in (-1, 0, 1):
                        self.mapreq(gx, gy)
                # Args: [I(0), C(spawn), I(player gob)] - the last int is
                # the player's own gob id.
                if args:
                    self.player_gob = args[-1]
            elif name == "scm":
                self.scm_id = wid
            elif name == "slen":
                self.slen_id = wid
                # Real-client behavior: SlenHud.binded() requests the
                # character sheet the moment the HUD binds. Opt-in: the
                # legacy probes predate the flow and keep their exact
                # old wire shape (test_client.py opts in).
                if self.request_chr:
                    self.wdgmsg(wid, "chr", bytes([LIST_END]))
            elif name == "sm":
                self.sm_wid = wid
                self.sm_opts = [a for a in args if isinstance(a, str)]
            elif name == "item" and len(args) >= 4:
                # args: [wire res, ql, drag, tooltip, num]
                self.item_info[wid] = {
                    "res": self.resids.get(args[0]),
                    "ql": args[1] if isinstance(args[1], int) else None,
                    "tt": args[3] if isinstance(args[3], str) else "",
                }
            if name == "chr":
                self.cattr_at_chr = set(self.cattr_names)
        elif t == RMSG_DSTWDG:
            wid = struct.unpack("<H", body[0:2])[0]
            self.widgets.pop(wid, None)
            self.item_info.pop(wid, None)
        elif t == RMSG_WDGMSG:
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            args = list(self.parse_args(body[nend + 1:]))
            if name == "place" and wid == self.mapview_id:
                self.place_seen = args
        elif t == RMSG_RESID:
            wire = struct.unpack("<H", body[0:2])[0]
            end = body.index(0, 2)
            name = body[2:end].decode()
            self.resids[wire] = name
        elif t == RMSG_CATTR:
            off = 0
            while off < len(body):
                end = body.index(0, off)
                self.cattr_names.add(body[off:end].decode())
                off = end + 9  # name\0 + two LE int32
        elif t == RMSG_PAGINAE:
            off = 0
            while off < len(body):
                act = body[off]
                off += 1
                nend = body.index(0, off)
                nm = body[off:nend].decode()
                off = nend + 1 + 2  # NUL + u16 ver
                if act == 0x2B and nm.startswith("paginae/atk/"):
                    self.paginae_atk.add(nm)
        self.on_event(t, body)

    def on_event(self, t, body):
        """Hook for script-specific RMSG handling (chat, menus, ...)."""
        return None

    def parse_args(self, buf):
        off = 0
        while off < len(buf) and buf[off] != LIST_END:
            ty = buf[off]
            off += 1
            if ty == LIST_INT:
                yield struct.unpack("<i", buf[off:off + 4])[0]
                off += 4
            elif ty == LIST_STR:
                end = buf.index(0, off)
                yield buf[off:end].decode()
                off = end + 1
            elif ty == LIST_COORD:
                x, y = struct.unpack("<ii", buf[off:off + 8])
                off += 8
                yield (x, y)
            else:
                return

    def on_objdata(self, body):
        for gobid, frame, ops in parse_objdata(body):
            g = self.gobs.setdefault(gobid, {
                "res": None, "sdt": b"", "pos": None,
                "linbeg": None, "linbegs": [], "linsteps": [], "moves": [],
            })
            if frame >= self.objacks.get(gobid, -1):
                self.objacks[gobid] = frame
            for op, arg in ops:
                if op == "REMOVE":
                    g["removed"] = True
                elif op == "RES":
                    wire, sdt = arg
                    g["sdt"] = sdt
                    g["res"] = self.resids.get(wire)
                elif op == "MOVE":
                    g["pos"] = arg
                    g["moves"].append(arg)
                elif op == "LINBEG":
                    g["linbeg"] = arg
                    g["linbegs"].append(arg)
                elif op == "LINSTEP":
                    g["linsteps"].append(arg)
                elif op == "LAYERS":
                    base, _ids = arg
                    # The layered base is the avatar body resource; record
                    # it so player-gob detection keeps working now that
                    # players spawn without a plain OD_RES.
                    if base is not None:
                        g["res"] = self.resids.get(base)
                elif op == "BUDDY" and arg == self.username:
                    self.player_gob = gobid

    # ---- scenario actions --------------------------------------------------
    def play(self, name):
        assert self.charlist_id is not None
        self.send_rel(
            [
                bytes([RMSG_WDGMSG])
                + le16(self.charlist_id)
                + b"play\x00"
                + bytes([LIST_STR])
                + name.encode()
                + b"\x00"
                + bytes([LIST_END])
            ]
        )

    def click_ground(self, mcx, mcy):
        self.wdgmsg(
            self.mapview_id,
            "click",
            bytes([LIST_COORD]) + le32(0) + le32(0)
            + bytes([LIST_COORD]) + le32(mcx) + le32(mcy)
            + bytes([LIST_INT]) + le32(1)
            + bytes([LIST_INT]) + le32(0)
            + bytes([LIST_END]),
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

    def menu_act(self, *words):
        args = b"".join(bytes([LIST_STR]) + havstr(w) for w in words) + bytes([LIST_END])
        self.wdgmsg(self.scm_id, "act", args)

    def send_place(self, coord, button=1, modflags=0):
        self.wdgmsg(
            self.mapview_id,
            "place",
            bytes([LIST_COORD]) + le32(coord[0]) + le32(coord[1])
            + bytes([LIST_INT]) + le32(button)
            + bytes([LIST_INT]) + le32(modflags)
            + bytes([LIST_END]),
        )

    def take_item(self, wid):
        self.wdgmsg(wid, "take", bytes([LIST_COORD]) + le32(0) + le32(0) + bytes([LIST_END]))

    def map_itemact(self, coord, gobid=None):
        mc = coord
        args = (
            bytes([LIST_COORD]) + le32(0) + le32(0)
            + bytes([LIST_COORD]) + le32(mc[0]) + le32(mc[1])
            + bytes([LIST_INT]) + le32(0)
        )
        if gobid is not None:
            args += (
                bytes([LIST_INT]) + le32(gobid)
                + bytes([LIST_COORD]) + le32(mc[0]) + le32(mc[1])
            )
        args += bytes([LIST_END])
        self.wdgmsg(self.mapview_id, "itemact", args)

    def flower_choice(self, wid, idx=0):
        self.wdgmsg(wid, "cl", bytes([LIST_INT]) + le32(idx) + bytes([LIST_END]))

    def find_item_by_res(self, resname):
        # refresh_inventory recreates item widgets with fresh ids; the
        # newest wid (max) is the live one once DSTWDG pruning is applied.
        best = None
        for wid, info in self.item_info.items():
            if info["res"] == resname and (best is None or wid > best):
                best = wid
        return best

    def find_item_by_tooltip(self, tooltip):
        for wid, info in self.item_info.items():
            if info["tt"] == tooltip:
                return wid
        return None

    def find_gobs(self, resname):
        return {
            g: info
            for g, info in self.gobs.items()
            if info["res"] == resname
        }


def enter_world(username, client_cls=WireClient):
    c = client_cls(username)
    c.connect()
    print("session accepted")
    c.pump(1.5)
    c.play(username)
    ok = c.wait_for(lambda: c.mapview_id is not None and c.player_gob is not None, 12)
    if not ok and c.player_gob is None:
        mine = [
            g for g, info in c.gobs.items()
            if info["res"] == "gfx/borka/body"
        ]
        c.player_gob = mine[0] if mine else None
    assert ok or c.player_gob is not None, (
        "world entry incomplete: mapview=%s player=%s gobs=%d" % (
            c.mapview_id, c.player_gob, len(c.gobs)))
    # Open the inventory the way the client's slen button does.
    assert c.slen_id is not None, "slen widget missing"
    for _ in range(4):
        c.wdgmsg(c.slen_id, "inv", bytes([LIST_END]))
        c.pump(0.3)
    c.wait_for(lambda: any(n == "inv" for n in c.widgets.values()), 4)
    print("world entry: player gob", c.player_gob)
    return c


def stop_server(proc):
    if proc is not None:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
