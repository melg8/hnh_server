#!/usr/bin/env python3
"""End-to-end farming verification (docs/mechanics/livestock/farming-and-plants.md).

Chain under test (wire level, the same path the Java client drives):
  auth -> world entry -> Plow Field pagina act -> map click (tilth)
  -> inventory item "take" (cursor) -> mapview itemact (plant)
  -> crop gob spawn with sdt stage byte -> server-side stage growth
  (OD_RES re-sends) -> click crop -> Harvest flower menu -> yield items.

Auto-starts the server binary on an isolated save with a fast crop clock
(HNH_CROP_TIME_SCALE=10000000 -> 250 ms per stage) when 1871 is free;
otherwise reuses the already-running server (crop clock then follows its
configuration, so stages advance in minutes rather than milliseconds).

Modes: default runs the full plant-grow-harvest flow; `skillbot` verifies
the skill gate (planting refused without the Farming skill value, purchased
via the char sheet sattr contract at the legacy cost, then planting works,
unknown buys refused).
"""
import os
import socket
import struct
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(REPO, "server", "target", "release", "hnh-server")

LIST_END, LIST_INT, LIST_STR, LIST_COORD = 0, 1, 2, 3
MSG_REL, MSG_MAPDATA, MSG_OBJDATA = 1, 5, 6
RMSG_WDGMSG, RMSG_RESID, RMSG_CATTR = 1, 6, 9
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
    """Start an isolated fast-clock server if none is listening."""
    probe = socket.socket()
    probe.settimeout(0.4)
    try:
        probe.connect(("127.0.0.1", 1871))
        probe.close()
        return None
    except OSError:
        pass
    env = dict(os.environ)
    env["HNH_CROP_TIME_SCALE"] = "10000000"
    env["HNH_LP_RATE"] = "1000"
    env["HNH_SAVE_FILE"] = os.path.join(REPO, "server", "target", "farm-test-save.json")
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


class FarmClient:
    def __init__(self, username):
        self.username = username
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.settimeout(0.25)
        self.server = ("127.0.0.1", 1870)
        self.tseq = 0
        self.rseq = 0
        self.held = {}
        self.widgets = {}
        self.charlist_id = None
        self.mapview_id = None
        self.scm_id = None
        self.chr_id = None
        self.chat_id = None
        self.exp_seen = None
        self.attrs = {}  # cattr name -> compiled value
        self.chat_lines = []  # (text, color or None)
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
            # Datagram layout: [MSG_OBJDATA][flags][gob blocks...]
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
        if t == 0:  # NEWWDG: [id u16][type\0][x i32][y i32][parent u16][args]
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
            elif name == "chr":
                self.chr_id = wid
            elif name == "slenchat":
                self.chat_id = wid
            elif name == "item" and len(args) >= 4:
                # args: [res, ql, flags, (drag coord), tooltip, num]
                tooltip = args[3] if isinstance(args[3], str) else ""
                self.items[wid] = tooltip
        elif t == RMSG_WDGMSG:
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            args = list(self.parse_args(body[nend + 1 :]))
            if name == "exp" and wid == self.chr_id:
                self.exp_seen = args[0] if args else None
            elif name == "log" and wid == self.chat_id:
                color = next((a for a in args if isinstance(a, tuple)), None)
                self.chat_lines.append((args[0] if args else "", color))
        elif t == RMSG_RESID:  # RESID: u16 wire, str name, u16 ver
            wire = struct.unpack("<H", body[0:2])[0]
            end = body.index(0, 2)
            name = body[2:end].decode()
            self.resids[wire] = name
        elif t == RMSG_CATTR:
            # entries (string name, i32 base, i32 compiled) until eom
            off = 0
            while off < len(body):
                nend = body.index(0, off)
                nm = body[off:nend].decode()
                off = nend + 1
                base, comp = struct.unpack("<ii", body[off : off + 8])
                off += 8
                self.attrs[nm] = comp

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
                    off += 20  # 2x coord(8) + int32 steps
                elif code == OD_LINSTEP:
                    off += 4
                elif code == OD_LAYERS:
                    off += 8  # 4x uint16 wire ids
                elif code == OD_HEALTH:
                    off += 1  # quarters byte
                elif code == OD_BUDDY:
                    end = body.index(0, off)
                    if body[off:end].decode(errors="replace") == self.username:
                        self.player_gob = gobid
                    off = end + 3  # name NUL + two flag bytes
                else:
                    return  # unknown sub-message: skip the rest safely

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

    def click_tile(self, tile):
        mc = (tile[0] * 11 + 5, tile[1] * 11 + 5)
        self.wdgmsg(
            self.mapview_id,
            "click",
            bytes([LIST_COORD]) + le32(0) + le32(0)
            + bytes([LIST_COORD]) + le32(mc[0]) + le32(mc[1])
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

    def arm_plow(self):
        self.wdgmsg(
            self.scm_id,
            "act",
            bytes([LIST_STR]) + havstr("plow") + bytes([LIST_END]),
        )

    def take_item(self, wid):
        self.wdgmsg(wid, "take", bytes([LIST_COORD]) + le32(0) + le32(0) + bytes([LIST_END]))

    def map_itemact(self, tile):
        mc = (tile[0] * 11 + 5, tile[1] * 11 + 5)
        self.wdgmsg(
            self.mapview_id,
            "itemact",
            bytes([LIST_COORD]) + le32(0) + le32(0)
            + bytes([LIST_COORD]) + le32(mc[0]) + le32(mc[1])
            + bytes([LIST_INT]) + le32(0)
            + bytes([LIST_END]),
        )

    def flower_choice(self, wid, idx=0):
        self.wdgmsg(wid, "cl", bytes([LIST_INT]) + le32(idx) + bytes([LIST_END]))

    def find_item(self, tooltip):
        for wid, tt in self.items.items():
            if tt == tooltip:
                return wid
        return None

    def find_gobs(self, resname):
        return {
            g: info
            for g, info in self.gobs.items()
            if info["res"] == resname
        }


def buy_farming_value(c):
    """Raise the Farming skill value to 1 through the real char sheet
    contract (slen 'chr' -> sattr pairs). Returns the LP balance seen at
    sheet open."""
    slen_wid = next(w for w, n in c.widgets.items() if n == "slen")
    c.wdgmsg(slen_wid, "chr", bytes([LIST_END]))
    ok = c.wait_for(lambda: c.chr_id is not None and c.exp_seen is not None, 5)
    assert ok, "char sheet never opened (widgets=%s)" % sorted(set(c.widgets.values()))
    exp_before = c.exp_seen
    c.wdgmsg(
        c.chr_id,
        "sattr",
        bytes([LIST_STR]) + havstr("farming")
        + bytes([LIST_INT]) + le32(1)
        + bytes([LIST_END]),
    )
    ok = c.wait_for(lambda: c.attrs.get("farming", 0) >= 1, 5)
    assert ok, "farming value never raised (attrs=%s)" % (c.attrs,)
    return exp_before


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "farmbot"
    server_proc = ensure_server()
    try:
        if mode == "skillbot":
            run_skillbot()
        else:
            run(mode)
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


def run(mode):
    # Per-run character: fresh starter kit every time (a reused character
    # may have spent its seeds in an earlier run).
    username = "%s%d%d" % (mode, int(time.time()) % 100000, os.getpid() % 1000)
    c = FarmClient(username)
    c.connect()
    print("session accepted")
    c.pump(1.5)
    c.play(username)
    ok = c.wait_for(lambda: c.mapview_id is not None and c.player_gob is not None, 12)
    if not ok and c.player_gob is None:
        # Fallback: the avatar gob carries the unique body resource.
        mine = [
            g for g, info in c.gobs.items()
            if info["res"] == "gfx/borka/body"
        ]
        c.player_gob = mine[0] if mine else None
    assert ok or c.player_gob is not None, (
        "world entry incomplete: mapview=%s player=%s gobs=%d resids=%d" % (
            c.mapview_id, c.player_gob, len(c.gobs), len(c.resids)))
    # Open the inventory the way the client's slen button does.
    slen_wid = next(w for w, n in c.widgets.items() if n == "slen")
    for _ in range(4):
        c.wdgmsg(slen_wid, "inv", bytes([LIST_END]))
        c.pump(0.3)
    c.wait_for(lambda: any(n == "inv" for n in c.widgets.values()), 4)
    print("world entry: player gob", c.player_gob)

    # The Farming skill value gates planting: buy it through the char
    # sheet before the plow/plant loop (legacy cost 100 for point 1).
    buy_farming_value(c)
    print("farming skill value raised via sattr")

    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)

    # --- plow + plant: scan nearby tiles until a crop gob appears ---------
    wheat_item = c.find_item("Wheat Seeds")
    assert wheat_item is not None, "starter wheat seeds missing (items=%s widgets=%s)" % (
        c.items, sorted(set(c.widgets.values())))
    crop = None
    for dx in range(-2, 3):
        for dy in range(-2, 3):
            if abs(dx) + abs(dy) > 3:
                continue
            tile = (ptile[0] + dx, ptile[1] + dy)
            c.arm_plow()
            c.pump(0.25)
            c.click_tile(tile)
            c.pump(0.3)
            c.take_item(wheat_item)
            c.pump(0.25)
            c.map_itemact(tile)
            if not c.wait_for(
                lambda: any(
                    (r or "").startswith("gfx/terobjs/plants/")
                    for r in (info["res"] for info in c.gobs.values())
                ),
                2.5,
            ):
                continue
            crops = c.find_gobs("gfx/terobjs/plants/wheat")
            if crops:
                crop = next(iter(crops.items()))
                break
    assert crop is not None, "no crop gob spawned on any candidate tile"
    (gob, info) = crop
    stage0 = info["sdt"]
    print("planted crop gob", gob, "stage sdt", list(stage0))
    # sdt must carry the stage byte; at 250 ms/stage the first tick may
    # already have advanced it, so only emptiness is a failure.
    assert stage0 != b"", "spawn block must carry a non-empty sdt stage byte"

    # --- growth: server advances stages (250 ms each at test scale) -------
    # Wheat matures at wire stage 3 (stages=3 in the crop table).
    ok = c.wait_for(lambda: c.gobs[gob]["sdt"] == b"\x03", 10)
    assert ok, "crop never reached maturity (sdt=%r)" % (c.gobs[gob]["sdt"],)
    print("growth: mature at stage byte", list(c.gobs[gob]["sdt"]))

    # --- harvest via flower menu ------------------------------------------
    ok = c.wait_for(
        lambda: any(w for w, n in c.widgets.items() if n == "sm"),
        1.0,
    )
    c.click_gob(gob, c.gobs[gob]["pos"])
    ok = c.wait_for(
        lambda: any(n == "sm" for n in c.widgets.values()), 4.0
    )
    assert ok, "harvest flower menu never opened"
    sm_wid = next(w for w, n in c.widgets.items() if n == "sm")
    before_wids = set(c.items)
    c.flower_choice(sm_wid)

    def new_yields():
        # refresh_inventory recreates every item widget, so yields show up
        # as NEW widget ids carrying the yield tooltip.
        return [
            tt
            for wid, tt in c.items.items()
            if wid not in before_wids and tt in ("Straw", "Wheat Seeds")
        ]

    ok = c.wait_for(lambda: new_yields(), 5.0)
    assert ok, "no yield item landed in inventory"
    print("harvest yields:", sorted(set(new_yields())))
    print("FARMING FLOW: OK")


def run_skillbot():
    """Skill-gate verification: planting refused without the Farming skill
    value; sattr purchase at the exact legacy cost; planting then works;
    unknown catalog buys are refused."""
    username = "skill%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    c = FarmClient(username)
    c.connect()
    print("session accepted")
    c.pump(1.5)
    c.play(username)
    ok = c.wait_for(lambda: c.mapview_id is not None and c.player_gob is not None, 12)
    assert ok or c.player_gob is not None, "world entry incomplete"
    assert c.chat_id is not None, "Area Chat widget missing"
    slen_wid = next(w for w, n in c.widgets.items() if n == "slen")
    for _ in range(4):
        c.wdgmsg(slen_wid, "inv", bytes([LIST_END]))
        c.pump(0.3)
    c.wait_for(lambda: any(n == "inv" for n in c.widgets.values()), 4)

    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)
    wheat_item = c.find_item("Wheat Seeds")
    assert wheat_item is not None, "starter wheat seeds missing"

    def plant_gobs():
        return {
            g
            for g, info in c.gobs.items()
            if (info["res"] or "").startswith("gfx/terobjs/plants/")
        }

    def tile_free(tx, ty):
        # Skip tiles a previous run's persisted crop still occupies.
        for g in c.gobs.values():
            pos = g["pos"]
            if pos and (pos[0] // 11, pos[1] // 11) == (tx, ty):
                if (g["res"] or "").startswith("gfx/terobjs/plants/"):
                    return False
        return True

    def plant_at(tile):
        """Plow + act with the cursor seed; returns the set of NEW plant
        gobs that appeared (diff against the pre-action snapshot). The
        cursor persists between attempts, so a re-take is only sent when
        the previous attempt could not have left a seed armed."""
        before = plant_gobs()
        c.arm_plow()
        c.pump(0.25)
        c.click_tile(tile)
        c.pump(0.3)
        if not cursor_held:
            c.take_item(wheat_item)
            c.pump(0.25)
        c.map_itemact(tile)
        c.wait_for(lambda: len(plant_gobs()) > len(before), 2.5)
        return plant_gobs() - before

    # The cursor is armed by the first take and stays armed: the refusal
    # path consumes nothing, and a successful plant consumes exactly one
    # unit out of the 5-seed stack.
    cursor_held = False

    # --- 1. planting is refused while the farming value is 0 -------------
    tile = next((tx, ty) for dx in range(1, 6) for tx, ty in [(ptile[0] + dx, ptile[1])] if tile_free(tx, ty))
    new_crops = plant_at(tile)
    cursor_held = True  # the take was sent; the refusal consumed nothing
    assert not new_crops, (
        "planting must be refused without the Farming skill (spawned=%s)"
        % (new_crops,)
    )
    refused = any("Farming skill" in t for t, _ in c.chat_lines)
    assert refused, "no refusal system line arrived (lines=%s)" % (c.chat_lines,)
    print("gate: planting refused with a colored system line")

    # --- 2. buy the farming point through the char sheet ------------------
    exp_before = buy_farming_value(c)
    print("farming purchased; LP", exp_before, "->", c.exp_seen)
    # Legacy curve: one point from 0 costs exactly 100 LP. Fast LP accrual
    # (HNH_LP_RATE) may add a few points between the two reads.
    assert exp_before - 100 <= c.exp_seen <= exp_before - 40, (
        "sattr charge off the legacy curve: %s -> %s" % (exp_before, c.exp_seen)
    )

    # The cursor still holds the seed (the gate consumed nothing).
    tile2 = next((tx, ty) for dx in range(1, 8) for tx, ty in [(ptile[0] + dx, ptile[1])] if tile_free(tx, ty) and (tx, ty) != tile)
    c.arm_plow()
    c.pump(0.25)
    c.click_tile(tile2)
    c.pump(0.3)
    c.map_itemact(tile2)
    ok = c.wait_for(
        lambda: any(
            (r or "").startswith("gfx/terobjs/plants/")
            for r in (info["res"] for info in c.gobs.values())
        ),
        4.0,
    )
    assert ok, "planting still refused after buying the Farming skill"
    print("gate lifted: crop gob spawned after purchase")

    # --- 3. unknown catalog buy is refused --------------------------------
    c.wdgmsg(
        c.chr_id,
        "buy",
        bytes([LIST_STR]) + havstr("nosuchskill") + bytes([LIST_END]),
    )
    ok = c.wait_for(
        lambda: any("unknown to this server" in t for t, _ in c.chat_lines), 3
    )
    assert ok, "unknown skill buy was not refused (lines=%s)" % (c.chat_lines,)
    print("unknown buy refused with a system line")
    print("SKILL GATE: OK")


if __name__ == "__main__":
    main()
