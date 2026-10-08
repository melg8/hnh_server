#!/usr/bin/env python3
"""Cross-node station relay probe (session 34).

Drives the FULL guest-oven lifecycle through a real 2-node cluster
(TCP mesh + real UDP), closing the session-33 verification gap:

  - builderbot homes on node 1: walks to the cell boundary and builds
    an oven on a NODE-1-OWNED tile through the REAL build flow
    (build pagina -> place -> stone x2 -> branch x1). Its plan gob,
    stage advances and completion all publish across the mesh.
  - probebot homes on node 0 (the oven's cell owner is node 1, so the
    oven is a GUEST for probebot): follows the build as guest gobs,
    then drives the station lifecycle through the guest path:
      * itemact branch on the guest oven -> RelayStationItem ->
        "Fuel added to the oven." system line
      * itemact meat -> RelayStationItem -> "Input loaded; right-click
        the oven to light it."
      * oven click -> Light flower menu opened LOCALLY from the
        snapshot, choice relays RelayStationAct
      * the guest oven re-renders with lit sdt 1 (the session-34
        kind-flip re-render)
      * the roast output drop appears next to the oven (authority
        spawn -> guest announce)

The companion shell phase greps both node logs for the relay pair
("relay station item sent" + "relay station act sent" on node 0;
"relay station fueled" + "relay station input loaded" + "relay station
lit" on node 1) proving the real path end to end.

Usage: probe_station.py <username> [home_game_port] [auth_port]
                            [builder_game_port] [builder_auth_port]
Prints "STATION RELAY: OK ..." on success.
"""
import hashlib
import os
import socket
import ssl
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import hnhlib as tb  # noqa: E402
from hnhlib import (  # noqa: E402
    LIST_COORD,
    LIST_END,
    LIST_INT,
    LIST_STR,
    havstr,
    le16,
    le32,
)
from test_build import BuildClient  # noqa: E402

M64 = (1 << 64) - 1


def _u64(v):
    return v & M64


def _score(cell, node):
    # grid_owner::node_score - the python port verified against a
    # rustc-compiled reference in probe_plow.py (session 32).
    h = _u64(_u64(cell[0]) * 0x9E3779B97F4A7C15)
    h ^= _u64(_u64(cell[1]) * 0xC2B2AE3D27D4EB4F)
    h ^= _u64(node * 0x165667B19E3779F9)
    h ^= h >> 30
    h = _u64(h * 0xBF58476D1CE4E5B9)
    h ^= h >> 27
    h = _u64(h * 0x94D049BB133111EB)
    h ^= h >> 31
    return h


def owner_of(cell, nodes):
    best, best_score = 0, -1
    for node in range(nodes):
        s = _score(cell, node)
        if node == 0 or s > best_score:
            best, best_score = node, s
    return best


def cell_of(x, y):
    return (x // 250, y // 250)


def auth_cookie_port(username, port, password="x"):
    """TLS auth against a specific node's auth port (test_build pattern)."""
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


class StationProbeClient(BuildClient):
    """BuildClient wired to one cluster node + Area Chat capture.

    The chat lines are the ack-visible side of the station relay: the
    home node renders the authority's FuelAdded / InputLoaded outcomes
    as the EXACT system lines the local path emits.
    """

    def __init__(self, username, game_port, auth_port):
        super().__init__(username)
        self.server = ("127.0.0.1", game_port)
        self.auth_port = auth_port
        self.chat_id = None
        self.chat_lines = []  # [(text, color or None)]
        self.rel_log = []  # (seq, rseq_at_recv, [sub rtypes]) - loss forensics

    def connect(self):
        cookie = auth_cookie_port(self.username, self.auth_port)
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
                        self.sock.sendto(
                            bytes([4]) + le32(gx) + le32(gy), self.server
                        )
            elif name == "scm":
                self.scm_id = wid
            elif name == "slen":
                self.slen_id = wid
            elif name == "slenchat":
                self.chat_id = wid
            elif name == "sm":
                self.sm_wid = wid
                self.sm_opts = [a for a in args if isinstance(a, str)]
            elif name == "item" and len(args) >= 4:
                self.item_info[wid] = {
                    "res": self.resids.get(args[0]),
                    "ql": args[1] if isinstance(args[1], int) else None,
                    "tt": args[3] if isinstance(args[3], str) else "",
                }
        elif t == 2:  # DSTWDG
            wid = struct.unpack("<H", body[0:2])[0]
            self.widgets.pop(wid, None)
            self.item_info.pop(wid, None)
            if wid == self.sm_wid:
                self.sm_wid = None
                self.sm_opts = []
        elif t == tb.RMSG_WDGMSG:
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            args = list(self.parse_args(body[nend + 1 :]))
            if name == "place" and wid == self.mapview_id:
                self.place_seen = args
            elif name == "log" and wid == self.chat_id:
                text = args[0] if args and isinstance(args[0], str) else ""
                color = args[1] if len(args) > 1 else None
                self.chat_lines.append((text, color))
        elif t == tb.RMSG_RESID:
            wire = struct.unpack("<H", body[0:2])[0]
            end = body.index(0, 2)
            name = body[2:end].decode()
            self.resids[wire] = name
            # Late resid: a raw OBJDATA block can beat its RESID
            # datagram across the wire (separate send paths); resolve
            # every pending gob that referenced this wire id.
            for g in self.gobs.values():
                if g.get("wire") == wire:
                    g["res"] = name
                    del g["wire"]

    def on_datagram(self, data):
        # test_build's datagram loop + REL forensics (rel_log): every
        # MSG_REL datagram records (seq, rseq_at_recv, sub summary) so a
        # lost/stuck reliable stream is diagnosable from the dump.
        if data[0] == 2:
            return  # ack
        if data[0] == tb.MSG_OBJDATA:
            self.on_objdata(data[2:])
            return
        if data[0] != tb.MSG_REL:
            return
        seq = struct.unpack("<H", data[1:3])[0]
        # Pass 1: split the datagram into subs (types + bodies).
        entries = []
        off = 3
        while off < len(data):
            t = data[off]
            off += 1
            if t & 0x80:
                ln = struct.unpack("<H", data[off : off + 2])[0]
                body = data[off + 2 : off + 2 + ln]
                off += 2 + ln
            else:
                body = data[off:]
                off = len(data)
            entries.append((t & 0x7F, body))
        # Forensics: one log row per datagram.
        summary = tuple(
            ("resid", struct.unpack("<H", b[0:2])[0])
            if t == tb.RMSG_RESID and len(b) >= 2
            else (t, len(b))
            for t, b in entries
        )
        self.rel_log.append((seq, self.rseq, summary))
        # Pass 2: the test_build delivery loop.
        for t, body in entries:
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

    def on_objdata(self, body):
        # test_build's parser + lazy resid resolution: OD_RES wire ids
        # that arrive before their RESID announcement stay pending on
        # the gob row and resolve in on_rel when the resid lands.
        off = 0
        while off + 8 <= len(body):
            gobid = struct.unpack("<i", body[off : off + 4])[0]
            off += 4
            off += 4  # frame
            g = self.gobs.setdefault(gobid, {"res": None, "sdt": b"", "pos": None})
            while off < len(body):
                code = body[off]
                off += 1
                if code == tb.OD_END:
                    break
                if code == tb.OD_RES:
                    wire = struct.unpack("<H", body[off : off + 2])[0]
                    off += 2
                    if wire & 0x8000:
                        ln = body[off]
                        off += 1
                        g["sdt"] = body[off : off + ln]
                        off += ln
                        wire &= 0x7FFF
                    name = self.resids.get(wire)
                    if name is not None:
                        g["res"] = name
                        g.pop("wire", None)
                    else:
                        g["wire"] = wire
                elif code == tb.OD_MOVE:
                    x, y = struct.unpack("<ii", body[off : off + 8])
                    off += 8
                    g["pos"] = (x, y)
                elif code == tb.OD_LINBEG:
                    off += 20
                elif code == tb.OD_LINSTEP:
                    off += 4
                elif code == tb.OD_LAYERS:
                    base = struct.unpack("<H", body[off : off + 2])[0]
                    off += 2
                    while True:
                        layer = struct.unpack("<H", body[off : off + 2])[0]
                        off += 2
                        if layer == 0xFFFF:
                            break
                    g["res"] = self.resids.get(base)
                elif code == tb.OD_HEALTH:
                    off += 1
                elif code == tb.OD_BUDDY:
                    end = body.index(0, off)
                    if body[off:end].decode(errors="replace") == self.username:
                        self.player_gob = gobid
                    off = end + 3
                else:
                    return

    # ---- scenario helpers --------------------------------------------------
    def walk_to(self, coord):
        self.wdgmsg(
            self.mapview_id,
            "click",
            bytes([LIST_COORD]) + le32(0) + le32(0)
            + bytes([LIST_COORD]) + le32(coord[0]) + le32(coord[1])
            + bytes([LIST_INT]) + le32(1)
            + bytes([LIST_INT]) + le32(0)
            + bytes([LIST_END]),
        )

    def player_pos(self):
        info = self.gobs.get(self.player_gob)
        return info["pos"] if info else None

    def wait_arrived(self, coord, tol, timeout):
        def there():
            pos = self.player_pos()
            return pos is not None and abs(pos[0] - coord[0]) <= tol and abs(
                pos[1] - coord[1]
            ) <= tol

        return self.wait_for(there, timeout, step=0.25)

    def wait_chat(self, substr, timeout):
        return self.wait_for(
            lambda: any(substr in text for text, _ in self.chat_lines),
            timeout,
            step=0.25,
        )


def enter_world(c):
    """Shared world-entry (test_build.enter_world for a pre-wired client)."""
    c.connect()
    print("[%s] session accepted" % c.username)
    c.pump(1.5)
    c.play(c.username)
    ok = c.wait_for(
        lambda: c.mapview_id is not None and c.player_gob is not None, 12
    )
    if not ok and c.player_gob is None:
        mine = [g for g, info in c.gobs.items() if info["res"] == "gfx/borka/body"]
        c.player_gob = mine[0] if mine else None
    assert ok or c.player_gob is not None, (
        "[%s] world entry incomplete: mapview=%s player=%s gobs=%d"
        % (c.username, c.mapview_id, c.player_gob, len(c.gobs))
    )
    assert c.slen_id is not None, "[%s] slen widget missing" % c.username
    for _ in range(4):
        c.wdgmsg(c.slen_id, "inv", bytes([LIST_END]))
        c.pump(0.3)
    c.wait_for(lambda: any(n == "inv" for n in c.widgets.values()), 4)
    print("[%s] world entry: player gob %s" % (c.username, c.player_gob))
    return c


def co_pump(clients, seconds):
    """Pump every client cooperatively: a UDP session that stops reading
    for tens of seconds overflows its socket buffer and loses raw OBJDATA
    blocks forever (no reliability layer on those), so the two characters
    must be drained together."""
    end = time.time() + seconds
    while time.time() < end:
        for c in clients:
            c.pump(max(0.02, (end - time.time()) / len(clients)))


def co_wait(clients, pred, timeout, what=""):
    """wait_for over multiple clients, pumping them all."""
    end = time.time() + timeout
    while time.time() < end:
        co_pump(clients, 0.1)
        if pred():
            return True
    return False


def build_oven_near(c, observer, nodes, me):
    """Walk to the cell boundary and build an oven on a cell THIS node
    owns (the observer's foreign cell) through the REAL build flow. The
    oven must live on the builder's own node: a gob spawned on a
    foreign cell is never published to that cell's owner (the peer is
    not subscribed to us for its own cells). `observer` (the probe
    character on the other node) is pumped cooperatively throughout -
    see co_pump. Returns (gob_id, mc) of the finished station."""
    pair = [c, observer]
    ppos = c.gobs[c.player_gob]["pos"] or (555, 555)
    ptile = (ppos[0] // 11, ppos[1] // 11)
    # Scan for site tiles whose cell THIS node owns AND which sit deep
    # inside the cell (>= 60 subtiles from every cell edge). The roast
    # output drop spawns with a +/-30 subtile jitter around the oven, so
    # a site on the cell rim can land the drop in the PEER's cell - a
    # gob spawned there is never published to the cell's owner and the
    # probe would never see the output. Deep sites keep the drop (and
    # the oven) safely inside the authority's own cell. Collect a few
    # candidates: terrain (water/cliff) refuses some sites.
    candidates = []
    for r in range(1, 31):
        for dy in range(-r, r + 1):
            for dx in range(-r, r + 1):
                if max(abs(dx), abs(dy)) != r:
                    continue
                cand_site = (ptile[0] + dx, ptile[1] + dy)
                smc = (cand_site[0] * 11 + 5, cand_site[1] * 11 + 5)
                cell = cell_of(smc[0], smc[1])
                if owner_of(cell, nodes) != me:
                    continue  # the peer's cell: a gob there is invisible
                ox, oy = smc[0] - cell[0] * 250, smc[1] - cell[1] * 250
                if not (60 <= ox <= 190 and 60 <= oy <= 190):
                    continue  # cell rim: the output jitter crosses out
                candidates.append(cand_site)
        if len(candidates) >= 6:
            break
    assert candidates, "no deep own-cell tile found near spawn"

    plan = None
    mc = None
    for site in candidates:
        staging = (
            site[0] - 2 if site[0] > ptile[0] else site[0] + 2,
            site[1] - 2 if site[1] > ptile[1] else site[1] + 2,
        )
        mc = (site[0] * 11 + 5, site[1] * 11 + 5)
        stag_mc = (staging[0] * 11 + 5, staging[1] * 11 + 5)
        c.walk_to(stag_mc)

        def there():
            pos = c.player_pos()
            return pos is not None and abs(pos[0] - stag_mc[0]) <= 30 and abs(
                pos[1] - stag_mc[1]
            ) <= 30

        ok = co_wait(pair, there, 30, "builder walk")
        assert ok, "[%s] never reached the staging tile" % c.username

        # Build flow (run_buildbot choreography): pagina -> place.
        c.menu_act("oven")
        ok = co_wait(pair, lambda: c.place_seen is not None, 5, "place uimsg")
        assert ok, "[%s] mapview never received the place uimsg" % c.username
        c.place_seen = None
        c.send_place(mc, 1, 0)

        def plan_up():
            return any(
                info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
                for info in c.gobs.values()
            )

        if co_wait(pair, plan_up, 3, "plan spawn"):
            plan = next(
                g
                for g, info in c.gobs.items()
                if info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
            )
            break
        print(
            "[%s] site %s refused (terrain/occupied), trying the next" % (c.username, site)
        )
    assert plan is not None, "no candidate site accepted an oven plan"
    print(
        "[%s] staging %s -> oven site %s (cell %s owner node%d) plan gob %s"
        % (
            c.username,
            staging,
            site,
            cell_of(mc[0], mc[1]),
            owner_of(cell_of(mc[0], mc[1]), nodes),
            plan,
        )
    )

    stone = c.find_item_by_res("gfx/invobjs/stone")
    assert stone is not None, "[%s] starter stone missing" % c.username
    c.take_item(stone)
    co_pump(pair, 0.3)
    c.map_itemact(mc, plan)
    ok = co_wait(pair, lambda: c.gobs[plan]["sdt"] == b"\x01", 4, "stage 1")
    assert ok, "[%s] stage never advanced after the stone sink" % c.username
    print("[%s] stones sunk: stage 1" % c.username)
    # The kit's stone stack (6) exceeds the plan's stone demand (2): the
    # remainder stays on the drag cursor and blocks the branch take (one
    # cursor item at a time, game/items.rs inv_take; session-51 contract).
    assert c.return_cursor(), "[%s] inventory window missing for cursor return" % c.username
    co_pump(pair, 0.3)

    branch = c.find_item_by_res("gfx/invobjs/branch")
    assert branch is not None, "[%s] starter branch missing" % c.username
    c.take_item(branch)
    co_pump(pair, 0.3)
    c.map_itemact(mc, plan)
    ok = co_wait(pair, lambda: c.gobs[plan]["sdt"] == b"\x00", 6, "completion")
    assert ok, "[%s] oven never completed (sdt=%r)" % (
        c.username,
        c.gobs[plan]["sdt"],
    )
    print("[%s] oven completed: gob %s" % (c.username, plan))
    # Same contract for the branch stack (6 vs the 1-unit demand line).
    assert c.return_cursor(), "[%s] inventory window missing for cursor return" % c.username
    co_pump(pair, 0.3)
    return plan, mc


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "stationbot"
    game_port = int(sys.argv[2]) if len(sys.argv) > 2 else 1870
    auth_port = int(sys.argv[3]) if len(sys.argv) > 3 else 1871
    b_game_port = int(sys.argv[4]) if len(sys.argv) > 4 else 1882
    b_auth_port = int(sys.argv[5]) if len(sys.argv) > 5 else 1883
    nodes = int(sys.argv[6]) if len(sys.argv) > 6 else 2

    stamp = "%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    probe = StationProbeClient("probe" + stamp, game_port, auth_port)
    builder = StationProbeClient("build" + stamp, b_game_port, b_auth_port)
    enter_world(probe)
    enter_world(builder)

    # The builder walks to the cell boundary and raises the oven on a
    # node-1-owned tile; the deep-cell site sits ~30 tiles from the
    # spawn, so the probe walks part of the way too (VIEW_RADIUS is 300
    # subtiles - the probe must come within ~200 of the oven to see it).
    plan, mc = build_oven_near(builder, probe, nodes, 1)
    probe_target = (mc[0] - 140, mc[1] - 140)
    probe.walk_to(probe_target)
    ok = co_wait(
        [probe, builder],
        lambda: (
            lambda p: p is not None
            and abs(p[0] - probe_target[0]) <= 40
            and abs(p[1] - probe_target[1]) <= 40
        )(probe.player_pos()),
        40,
        "probe approach",
    )
    assert ok, "probe never approached the oven area"
    print("[probe] in view range of the oven")
    # Let the completion GuestUpdate reach the probe's node and the vis
    # scan deliver the re-render.
    co_pump([probe, builder], 1.5)
    ovens = probe.find_gobs("gfx/terobjs/oven")
    if not ovens:
        wids = sorted(probe.resids.keys())
        resids_seen = sorted(
            w for (s, r, subs) in probe.rel_log for x in subs
            if x[0] == "resid" for w in [x[1]]
        )
        print(
            "[probe] DEBUG gobs=%d resids=%d wire range=%s..%s tail=%s\n"
            "  rseq=%s held=%s rel_datagrams=%d resids_on_wire=%d\n"
            "  resid wires on wire: %s\n"
            "  resid wires resolved: %s\n"
            "  oven wire=%s station wire in resids=%s"
            % (
                len(probe.gobs),
                len(probe.resids),
                wids[0] if wids else None,
                wids[-1] if wids else None,
                wids[-12:],
                probe.rseq,
                sorted(probe.held.keys()),
                len(probe.rel_log),
                len(resids_seen),
                resids_seen[-15:],
                wids[-15:],
                probe.gobs.get(98587, {}).get("wire"),
                (probe.gobs.get(98587, {}).get("wire") in probe.resids)
                if probe.gobs.get(98587, {}).get("wire") is not None
                else None,
            )
        )
    assert ovens, "probe never saw the guest oven"
    assert plan in ovens, (
        "probe's oven gob ids %s do not include the builder's %s"
        % (sorted(ovens.keys()), plan)
    )
    print(
        "[probe] guest oven visible: gob %s sdt=%r (class flip delivered)"
        % (plan, ovens[plan]["sdt"])
    )

    # --- input first: itemact meat on the GUEST oven ---------------------
    # Order matters: the branch stack carries two units and one unit
    # survives the fuel delivery on the cursor (take is refused while a
    # cursor is held), so the MEAT must load before the branch fuels.
    pair = [probe, builder]
    meat = probe.find_item_by_res("gfx/invobjs/meat")
    assert meat is not None, "probe starter meat missing"
    probe.take_item(meat)
    co_pump(pair, 0.3)
    probe.map_itemact(mc, plan)
    ok = co_wait(
        pair,
        lambda: any("Input loaded" in t for t, _ in probe.chat_lines),
        6,
        "input ack",
    )
    assert ok, "input relay ack never rendered (chat=%s)" % (probe.chat_lines,)
    print("[probe] input loaded through the relay")

    # --- fuel: itemact branch on the GUEST oven --------------------------
    branch = probe.find_item_by_res("gfx/invobjs/branch")
    assert branch is not None, "probe starter branch missing"
    probe.take_item(branch)
    co_pump(pair, 0.3)
    probe.map_itemact(mc, plan)
    ok = co_wait(
        pair,
        lambda: any("Fuel added" in t for t, _ in probe.chat_lines),
        6,
        "fuel ack",
    )
    assert ok, "fuel relay ack never rendered (chat=%s)" % (probe.chat_lines,)
    print("[probe] fuel delivered through the relay")

    # --- light: flower menu from the snapshot, choice relays -----------
    probe.click_gob(plan, mc)
    ok = co_wait(
        pair,
        lambda: probe.sm_wid is not None and probe.sm_opts == ["Light"],
        4,
        "light menu",
    )
    assert ok, "guest oven never offered the Light menu (opts=%s)" % (
        probe.sm_opts,
    )
    print("[probe] Light menu opened from the snapshot")
    probe.flower_choice(probe.sm_wid, 0)
    # The authority lights it; the kind-flip re-render delivers sdt 1.
    ok = co_wait(pair, lambda: probe.gobs[plan]["sdt"] == b"\x01", 6, "lit re-render")
    assert ok, "guest oven never re-rendered as lit (sdt=%r)" % (
        probe.gobs[plan]["sdt"],
    )
    print("[probe] oven lit through the relay: sdt=1")

    # --- output: the roast drop appears beside the oven -----------------
    # The drop GOB renders with the gfx/terobjs/items world shape
    # (drop_world_res: inventory resources have no `neg` layer), so the
    # world-gob search keys on the terobjs resource, not the invobjs
    # icon resource the restored stack will carry.
    drop_res = "gfx/terobjs/items/meat"
    drops_before = set(probe.find_gobs(drop_res).keys())
    ok = co_wait(
        pair,
        lambda: len(set(probe.find_gobs(drop_res).keys()) - drops_before) > 0,
        12,
        "roast output",
    )
    assert ok, (
        "no roast output drop appeared next to the guest oven "
        "(drops_before=%s all_meat_gobs=%s near_oven=%s chat=%s)"
        % (
            drops_before,
            {
                g: (i["res"], i.get("wire"), i["sdt"], i["pos"])
                for g, i in probe.find_gobs(drop_res).items()
            },
            {
                g: (i["res"], i.get("wire"), i["sdt"], i["pos"])
                for g, i in probe.gobs.items()
                if i["pos"]
                and abs(i["pos"][0] - mc[0]) < 120
                and abs(i["pos"][1] - mc[1]) < 120
            },
            probe.chat_lines[-4:],
        )
    )
    drop = (set(probe.find_gobs(drop_res).keys()) - drops_before).pop()
    drop_pos = probe.gobs[drop]["pos"]
    print(
        "[probe] roast output drop %s at %s (authority spawn -> guest announce)"
        % (drop, drop_pos)
    )
    near = (
        drop_pos is not None
        and abs(drop_pos[0] - mc[0]) < 120
        and abs(drop_pos[1] - mc[1]) < 120
    )
    assert near, "output drop %s too far from the oven %s" % (drop_pos, mc)
    print(
        "STATION RELAY: OK oven=%s cell=%s owner=node%d (fuel+input+light+output)"
        % (plan, cell_of(mc[0], mc[1]), owner_of(cell_of(mc[0], mc[1]), nodes))
    )


if __name__ == "__main__":
    main()
