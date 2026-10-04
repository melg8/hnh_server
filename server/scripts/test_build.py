#!/usr/bin/env python3
"""End-to-end building + station verification against a running server.

Flow (crafting-and-building.md, building pipeline + production stations):
  1. Auth + session + world entry (same wire path as test_client.py).
  2. act("oven") on the menugrid -> expect the mapview `place` uimsg
     (placement ghost drive).
  3. mapview wdgmsg `place` at a tile near the player -> expect the
     construction-plan gob (gfx/terobjs/oven, stage sdt 0).
  4. itemact with the held branch stack -> stage advance (sdt re-render).
  5. itemact with the held stone stack -> plan completes: the gob becomes
     the finished station; clicking it now offers the `Light` flower menu.
  6. Station flow: itemact branch (fuel), itemact meat (input), flower
     menu Light, wait for the tick job, pick up the output drop, verify
     the Roasted Beef label and the formula quality.

Exit 0 only when the whole chain passes; prints per-step diagnostics.
Modes: `buildbot` (steps 1-5, BUILD FLOW), `stationbot` (6, STATION
FLOW), `all` (both, default: buildbot first then a fresh character).
"""
import os
import socket
import struct
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(REPO, "server", "target", "release", "hnh-server")

MSG_REL, MSG_MAPDATA, MSG_OBJDATA = 1, 5, 6
RMSG_WDGMSG, RMSG_RESID, RMSG_CATTR = 1, 6, 9
LIST_END, LIST_INT, LIST_STR, LIST_COORD = 0, 1, 2, 3
LIST_COLOR = 6
OD_MOVE, OD_RES, OD_LINBEG, OD_LINSTEP, OD_BUDDY, OD_END = 1, 2, 3, 4, 15, 255
OD_LAYERS, OD_HEALTH = 6, 14


def le16(v):
    return struct.pack("<H", v & 0xFFFF)


def le32(v):
    return struct.pack("<i", v)


def havstr(s):
    return s.encode() + b"\x00"


def auth_cookie(username, password="x"):
    """TLS auth -> session cookie (test_farming.py handshake)."""
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
        probe.connect(("127.0.0.1", 1871))
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
            probe.connect(("127.0.0.1", 1871))
            probe.close()
            return proc
        except OSError:
            time.sleep(0.4)
    raise RuntimeError("server did not come up")


class BuildClient:
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
        self.slen_id = None
        self.sm_wid = None
        self.sm_opts = []
        self.resids = {}  # wire id -> name
        self.gobs = {}  # gobid -> {"res": name, "sdt": bytes, "pos": (x,y)}
        self.item_info = {}  # item wid -> {"res": name, "ql": int, "tt": str}
        self.player_gob = None
        self.place_seen = None  # mapview `place` uimsg args

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
        if t == 0:  # NEWWDG
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
            elif name == "slen":
                self.slen_id = wid
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
        elif t == 2:  # DSTWDG
            wid = struct.unpack("<H", body[0:2])[0]
            self.widgets.pop(wid, None)
            self.item_info.pop(wid, None)
        elif t == RMSG_WDGMSG:
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            args = list(self.parse_args(body[nend + 1 :]))
            if name == "place" and wid == self.mapview_id:
                self.place_seen = args
        elif t == RMSG_RESID:
            wire = struct.unpack("<H", body[0:2])[0]
            end = body.index(0, 2)
            name = body[2:end].decode()
            self.resids[wire] = name

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
                    off += 20
                elif code == OD_LINSTEP:
                    off += 4
                elif code == OD_LAYERS:
                    base = struct.unpack("<H", body[off : off + 2])[0]
                    off += 2
                    while True:
                        layer = struct.unpack("<H", body[off : off + 2])[0]
                        off += 2
                        if layer == 0xFFFF:
                            break
                    # The layered base is the avatar body resource; record
                    # it so player-gob detection keeps working now that
                    # players spawn without a plain OD_RES.
                    g["res"] = self.resids.get(base)
                elif code == OD_HEALTH:
                    off += 1
                elif code == OD_BUDDY:
                    end = body.index(0, off)
                    if body[off:end].decode(errors="replace") == self.username:
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


def enter_world(username):
    c = BuildClient(username)
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


def run_buildbot():
    username = "build%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    c = enter_world(username)
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)
    site = (ptile[0] + 1, ptile[1])  # one tile east

    # --- 1. build pagina -> placement ghost drive --------------------------
    c.menu_act("oven")
    ok = c.wait_for(lambda: c.place_seen is not None, 5)
    assert ok, "mapview never received the place uimsg"
    pres = c.place_seen[0] if c.place_seen else None
    assert pres == "gfx/terobjs/oven", "place uimsg names %r" % (pres,)
    ontile = c.place_seen[2] if len(c.place_seen) > 2 else None
    assert ontile == 1, "on-tile flag missing from place uimsg: %r" % (c.place_seen,)
    print("placement ghost driven:", c.place_seen)

    # --- 2. commit the placement -> plan gob --------------------------------
    mc = (site[0] * 11 + 5, site[1] * 11 + 5)
    c.send_place(mc, 1, 0)
    ok = c.wait_for(
        lambda: any(
            info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
            for info in c.gobs.values()
        ),
        5,
    )
    assert ok, "plan gob never spawned at the committed tile"
    plan = next(
        g for g, info in c.gobs.items()
        if info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
    )
    assert c.gobs[plan]["sdt"] == b"\x00", (
        "plan spawn must carry stage-0 sdt, got %r" % (c.gobs[plan]["sdt"],)
    )
    print("plan gob placed:", plan, "stage sdt", list(c.gobs[plan]["sdt"]))

    # --- 3. sink the stone stack: stage advances ----------------------------
    # Oven demand: stone x2 + branch x1 (3 units, 2 stages). Two stones
    # cross the 2/3 boundary: stage 0 -> 1 (sdt re-render).
    stone = c.find_item_by_res("gfx/invobjs/stone")
    assert stone is not None, "starter stone missing"
    c.take_item(stone)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "stage never advanced after stone sink (sdt=%r)" % (
        c.gobs[plan]["sdt"],)
    print("stones sunk: stage sdt ->", list(c.gobs[plan]["sdt"]))

    # --- 4. sink the branch stack: completion --------------------------------
    branch = c.find_item_by_res("gfx/invobjs/branch")
    assert branch is not None, "starter branch missing (items=%s)" % (
        [(i["res"], i["tt"]) for i in c.item_info.values()],)
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    # Completion converts the plan in place; wait for the sdt to drop back
    # to the station's unlit byte (0) after the stage-1 render.
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x00", 4)
    assert ok, "plan never completed (sdt=%r)" % (c.gobs[plan]["sdt"],)
    print("branch sunk: plan completed, gob", plan)

    # --- 5. the finished gob offers the Light flower menu -------------------
    c.click_gob(plan, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Light"], 4)
    assert ok, "station flower menu never offered Light (opts=%s)" % (c.sm_opts,)
    print("BUILD FLOW: OK")
    return c, plan, mc


def run_stationbot():
    username = "stat%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    c = enter_world(username)
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)

    # Build the oven first (same choreography as run_buildbot), scanning
    # candidate tiles because an earlier flow may own the first one.
    mc = None
    plan = None
    for dx in range(-2, 3):
        cand = (ptile[0] + dx, ptile[1] + 1)
        cmc = (cand[0] * 11 + 5, cand[1] * 11 + 5)
        c.menu_act("oven")
        ok = c.wait_for(lambda: c.place_seen is not None, 5)
        assert ok, "place uimsg missing"
        c.place_seen = None
        c.send_place(cmc, 1, 0)
        if c.wait_for(
            lambda: any(
                info["res"] == "gfx/terobjs/oven" and info["pos"] == cmc
                for info in c.gobs.values()
            ),
            2.5,
        ):
            mc = cmc
            plan = next(
                g for g, info in c.gobs.items()
                if info["res"] == "gfx/terobjs/oven" and info["pos"] == cmc
            )
            break
    assert plan is not None, "no free tile accepted an oven plan"
    stone = c.find_item_by_res("gfx/invobjs/stone")
    c.take_item(stone)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    # Wait out the stage-1 re-render: refresh_inventory recreates the item
    # widgets, so the branch lookup below needs the post-refresh ids.
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "stage never advanced after stone sink (sdt=%r)" % (
        c.gobs[plan]["sdt"],)
    branch = c.find_item_by_res("gfx/invobjs/branch")
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x00" and c.gobs[plan]["sdt"] != b"\x01", 4)
    assert ok, "oven never completed"
    print("oven built:", plan)

    # --- station: fuel delivery --------------------------------------------
    # Oven demand consumed stone x2 + branch x1; the branch stack carried
    # two units, so the leftover branch is still on the cursor (take moves
    # the whole stack). The next itemact delivers it as fuel.
    c.map_itemact(mc, plan)
    c.pump(0.5)
    print("fuel delivered")

    # --- station: input delivery -------------------------------------------
    meat = c.find_item_by_res("gfx/invobjs/meat")
    assert meat is not None, "starter meat missing"
    c.take_item(meat)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    c.pump(0.5)
    print("input delivered")

    # --- station: light via the flower menu ---------------------------------
    c.click_gob(plan, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Light"], 4)
    assert ok, "Light menu missing before lighting (opts=%s)" % (c.sm_opts,)
    c.flower_choice(c.sm_wid, 0)
    # The lit station re-renders with sdt byte 1.
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "station never re-rendered as lit (sdt=%r)" % (c.gobs[plan]["sdt"],)
    print("station lit; waiting for the tick job...")

    # --- output: the roast drop appears beside the oven ---------------------
    drops_before = set(c.find_gobs("gfx/invobjs/meat").keys())
    ok = c.wait_for(
        lambda: len(set(c.find_gobs("gfx/invobjs/meat").keys()) - drops_before) > 0,
        10,
    )
    assert ok, "no output drop appeared"
    drop_id = (set(c.find_gobs("gfx/invobjs/meat").keys()) - drops_before).pop()
    drop_pos = c.gobs[drop_id]["pos"]
    print("output drop:", drop_id, "at", drop_pos)

    # --- pick up the output and verify label + quality ----------------------
    c.click_gob(drop_id, drop_pos)
    ok = c.wait_for(lambda: c.find_item_by_tooltip("Roasted Beef") is not None, 5)
    assert ok, "roasted output never reached the inventory (items=%s)" % (
        [(i["res"], i["tt"]) for i in c.item_info.values()],)
    out_wid = c.find_item_by_tooltip("Roasted Beef")
    ql = c.item_info[out_wid]["ql"]
    # Formula: (2*q_item + q_station + q_fuel)/4 = (2*10+10+10)/4 = 10.
    assert ql == 10, "output quality %r violates the station formula" % (ql,)
    print("STATION FLOW: OK")


def run_persistbot():
    """Place a plan and sink a partial delivery, then leave it half-built
    for the restart gate (verify_build.sh persist-build)."""
    username = "pers%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    c = enter_world(username)
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)
    mc = ((ptile[0] + 1) * 11 + 5, ptile[1] * 11 + 5)
    c.menu_act("oven")
    ok = c.wait_for(lambda: c.place_seen is not None, 5)
    assert ok, "place uimsg missing"
    c.send_place(mc, 1, 0)
    ok = c.wait_for(
        lambda: any(
            info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
            for info in c.gobs.values()
        ),
        5,
    )
    assert ok, "plan gob missing"
    plan = next(
        g for g, info in c.gobs.items()
        if info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
    )
    stone = c.find_item_by_res("gfx/invobjs/stone")
    c.take_item(stone)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "partial sink did not advance the stage"
    print("PERSIST: OK")


def run_persistcheck():
    """After a restart: find the restored half-built plan and finish it —
    only possible if the credited materials survived."""
    username = "perc%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    c = enter_world(username)
    # The restored plan carries the stage-1 sdt byte (stone x2 credited).
    ok = c.wait_for(
        lambda: any(
            info["res"] == "gfx/terobjs/oven" and info["sdt"] == b"\x01"
            for info in c.gobs.values()
        ),
        8,
    )
    assert ok, "no half-built oven plan visible after restart (gobs=%d)" % len(c.gobs)
    plan = next(
        g for g, info in c.gobs.items()
        if info["res"] == "gfx/terobjs/oven" and info["sdt"] == b"\x01"
    )
    mc = c.gobs[plan]["pos"]
    branch = c.find_item_by_res("gfx/invobjs/branch")
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x00", 5)
    assert ok, "restored plan would not complete (sdt=%r)" % (c.gobs[plan]["sdt"],)
    print("PERSIST: OK")


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "all"
    server_proc = ensure_server()
    try:
        if mode == "buildbot":
            run_buildbot()
        elif mode == "stationbot":
            run_stationbot()
        elif mode == "persistbot":
            run_persistbot()
        elif mode == "persistcheck":
            run_persistcheck()
        else:
            run_buildbot()
            print("---")
            run_stationbot()
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


if __name__ == "__main__":
    main()
