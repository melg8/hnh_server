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
import zlib
from collections import deque

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
# Impassable tile ids (mirror state::tile_speed_pct: deep water, water,
# cave, mountain). Everything else accepts a LinMove.
IMPASSABLE_TILES = frozenset((0, 1, 25, 26))
# Grid geometry (mirror Grid::idx and the mapdata wire format).
GRID_SPAN = 1100  # subtile span of one 100x100-tile grid
TILE_SPAN = 11    # subtile span of one tile
TILE_COUNT = 100  # tiles per grid axis
TILE_BYTES = TILE_COUNT * TILE_COUNT
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


def auth_cookie(username, password="x", port=AUTH_PORT, host="127.0.0.1"):
    """TLS auth handshake -> session cookie (dev policy: any password)."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection((host, port), timeout=5)
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


def _terminate_listening_server():
    """Session 90: kill any server still holding the auth port.

    A probe killed by its faulthandler watchdog (exit=True) never runs
    the `finally: stop_server(proc)` leg, so the child server survives
    and keeps serving its dirty world - the next probe's
    `ensure_server` then silently ATTACHES to it instead of booting
    fresh (live trace: the breeding run entered as player gob 69900 on
    a foreign world and found zero cows in the loaded grids). The
    match pins the exact server binary path + args so nothing else is
    touched; the port is polled until it actually frees up."""
    probe = socket.socket()
    probe.settimeout(0.4)
    try:
        probe.connect(("127.0.0.1", AUTH_PORT))
    except OSError:
        return  # nothing listening
    finally:
        probe.close()
    subprocess.run(
        ["pkill", "-f", "--", BIN + " --seed"],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    deadline = time.time() + 10
    while time.time() < deadline:
        s = socket.socket()
        s.settimeout(0.4)
        try:
            s.connect(("127.0.0.1", AUTH_PORT))
            s.close()
            time.sleep(0.4)
        except OSError:
            return
    raise RuntimeError("a stale server keeps the auth port busy")


def ensure_server(env_extra=None, save_path=None, log_path=None,
                  fresh=False, keep_save=False):
    """Start an isolated server (fresh save) if none is listening.

    env_extra: extra environment variables for the server process (for
    scenario clocks such as HNH_CROP_TIME_SCALE). save_path overrides
    the default per-scenario save file; it is removed before boot so
    every run starts from a fresh world. log_path: when set the server
    process stdout/stderr go to that file instead of /dev/null - set
    RUST_LOG in env_extra to capture the server's own debug lines
    (e.g. silent "plow refused"/"plant refused" refusals that the
    client never sees).
    fresh (session 90): terminate any server already listening on the
    auth port FIRST (a leaked watchdog-killed probe's child), then
    boot. The probes that share one server across stages keep the
    default attach behavior.
    keep_save (session 90): do NOT delete save_path before boot. The
    restart legs (the breeding probe) boot a second server on the file
    the first one flushed on its SIGTERM shutdown - deleting it here
    silently reloaded an empty world (live trace: saved_chars=0 with
    the herd parked on it, "the herd did not reload").
    """
    if fresh:
        _terminate_listening_server()
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
    if save_path is None:
        save_path = os.path.join(REPO, "server", "target", "build-test-save.json")
    # Fresh world: persistent plans from earlier runs would occupy the
    # spawn-area tiles and intercept this run's itemacts - unless the
    # caller explicitly wants the file KEPT (restart leg).
    if not keep_save:
        try:
            os.remove(save_path)
        except OSError:
            pass
    env["HNH_SAVE_FILE"] = save_path
    if env_extra:
        env.update(env_extra)
    if log_path:
        env.setdefault("RUST_LOG", "debug")
        log_fh = open(log_path, "ab")
        stdout_arg = log_fh
        stderr_arg = subprocess.STDOUT
    else:
        log_fh = None
        stdout_arg = subprocess.DEVNULL
        stderr_arg = subprocess.DEVNULL
    proc = subprocess.Popen(
        [BIN, "--seed", "42"],
        cwd=os.path.join(REPO, "server"),
        env=env,
        stdout=stdout_arg,
        stderr=stderr_arg,
    )
    if log_fh is not None:
        # The child inherited the descriptor; the parent handle can go.
        log_fh.close()
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

    def __init__(self, username, request_chr=True, send_objacks=True,
                 host="127.0.0.1", auth_port=None, game_port=None):
        """One client bound to one node. Passing the ports of a cluster
        node (e.g. auth 1873 / game 1874 for node 1 of cluster-up.sh)
        drives the whole session through THAT node - the multi-machine
        profile's entry test.

        send_objacks defaults True (session 87): the real client's
        SWorker echoes OBJACK within <=320 ms, so the server's unacked
        table drains fast and its retransmit sweep stays idle. Probes
        that never acked left ~700 bootstrap blocks pending for the
        full 10 s retirement window, and the duplicate OD_MOVE
        retransmits they caused kept wiping the first post-entry
        LINBEG (see on_objdata's MOVE branch)."""
        self.username = username
        self.request_chr = request_chr
        self.send_objacks = send_objacks
        self.auth_port = AUTH_PORT if auth_port is None else auth_port
        self.auth_host = host
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        # The world-entry burst (9 MAPDATA grids + several hundred gob
        # spawns + the reliable RMSG stream) easily overflows the
        # default ~200 KB receive buffer, and the kernel then DROPS
        # whole grids (nondeterministic 4-of-9 receptions before this
        # fix). 4 MB absorbs the burst; the drain rate is not limiting.
        self.sock.setsockopt(
            socket.SOL_SOCKET, socket.SO_RCVBUF, 4 * 1024 * 1024)
        self.sock.settimeout(0.25)
        self.server = (host, GAME_PORT if game_port is None else game_port)
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
        self.sm_args = {}  # every open flower menu: sm wid -> option labels
        self.chat_id = None  # slenchat Area Chat widget
        self.chat_lines = []  # (text, color tuple or None) from `log`
        self.chr_id = None  # char sheet window
        self.exp_seen = None  # last `exp` uimsg arg on the chr widget
        self.attrs = {}  # cattr name -> compiled value
        self.pv_id = None  # party roster widget
        self.destroyed = set()  # widget ids seen in DSTWDG
        self.buddy_names = {}  # gobid -> OD_BUDDY character name
        self.resids = {}  # wire id -> name
        self.gobs = {}  # gobid -> {"res","sdt","pos","linbeg","linsteps","moves"}
        self.item_info = {}  # item wid -> {"res","ql","tt"}
        self.player_gob = None
        self.place_seen = None  # mapview `place` uimsg args
        self.cattr_names = set()
        self.cattr_at_chr = None  # snapshot when the `chr` widget arrives
        self.paginae_atk = set()
        self.mapdata_datagrams = 0
        # Reassembled MAPDATA grids: (gx, gy) -> 10_000 tile bytes,
        # row-major y*100+x (mirror of Grid::idx on the server).
        self.tiles = {}
        self._mapfrag = {}  # pktid -> {off: chunk} until complete
        # Session 90: grids already requested around the moving avatar
        # (grid -> last request time). The spawn 3x3 + one neighborhood
        # ring used to be the whole streaming story; a chase that crossed
        # into an unrequested grid then walked BLIND (tile_at None ->
        # line_clear refuses, find_tile_path None, the probe stalled
        # while the beast fled - live trace: breeding pursuit grid (1,3)).
        # Mirrors Java MCache: request the 3x3 around the CURRENT
        # position, re-asking every 5 s while a grid stays missing
        # (UDP loss happens).
        self._mapreq_pending = {}
        self.objdata_datagrams = 0
        self.objacks = {}  # gobid -> last frame seen (SWorker mirror)
        self.last_ack = 0.0
        # SWorker idle clock (session 89): every outbound datagram
        # stamps it; pump() beats when it goes stale, mirroring the
        # Java client's 5 s idle beat (Session.java sworker).
        self._last_out = 0.0

    def _send(self, data):
        """Send one raw datagram and stamp the sworker idle clock.

        Every outbound path funnels through here so pump()'s idle beat
        sees exactly what the Java sworker would: it beats only when
        its own output has been idle for 5 s.
        """
        self._last_out = time.time()
        self.sock.sendto(data, self.server)

    # ---- session plumbing -------------------------------------------------
    def connect(self):
        cookie = auth_cookie(self.username, port=self.auth_port,
                             host=self.auth_host)
        sess = (
            bytes([MSG_SESS])
            + le16(1)
            + havstr("Haven")
            + le16(PVER)
            + havstr(self.username)
            + cookie
        )
        for _ in range(8):
            self._send(sess)
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
        self._send(out)

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
        self._send(bytes([MSG_MAPREQ]) + le32(gx) + le32(gy))

    def pump(self, seconds):
        deadline = time.time() + seconds
        while time.time() < deadline:
            now = time.time()
            if self.send_objacks and self.objacks and now - self.last_ack > 0.2:
                # Client SWorker mirror: one batched MSG_OBJACK datagram.
                msg = bytes([MSG_OBJACK])
                for gid, frame in self.objacks.items():
                    msg += le32(gid) + le32(frame)
                self._send(msg)
                self.last_ack = now
            if now - self._last_out > 5.0:
                # SWorker mirror (Session.java: beat on 5 s output
                # idle). The server's 60 s silence timeout is live
                # (session 89), so a probe must look idle-but-ALIVE,
                # never dead: an idle real client beats every 5 s.
                self._send(bytes([MSG_BEAT]))
            self._stream_player_grids(now)
            try:
                data, _ = self.sock.recvfrom(65536)
            except socket.timeout:
                continue
            self.on_datagram(data)

    def _stream_player_grids(self, now):
        """Request the 3x3 grids around the avatar's current position
        once each (re-request every 5 s while missing: a lost mapdata
        datagram would otherwise park the grid on the pending set
        forever). Called from pump - the natural cadence of every
        probe loop."""
        pos = self.gobs.get(self.player_gob, {}).get("pos")
        if not pos:
            return
        pgx, pgy = pos[0] // GRID_SPAN, pos[1] // GRID_SPAN
        for gy in (pgy - 1, pgy, pgy + 1):
            for gx in (pgx - 1, pgx, pgx + 1):
                key = (gx, gy)
                if key in self.tiles:
                    continue
                last = self._mapreq_pending.get(key)
                if last is not None and now - last < 5.0:
                    continue
                self._mapreq_pending[key] = now
                self.mapreq(gx, gy)

    def wait_for(self, predicate, timeout, step=0.25):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if predicate():
                return True
            self.pump(step)
        return False

    # ---- tile map + navigation --------------------------------------------
    # The probes used to walk BLIND (click toward the target, hope the
    # straight line is clear). The server only accepts LinMove clicks
    # whose full segment is walkable (state::path_clear), so ridge-blocked
    # clicks move nothing and the old probes stalled in place. The tile
    # grid each client already streams (MSG_MAPDATA) is the fix: parse it
    # once, path-find over it, and click only lines the server will take.

    def _on_mapdata(self, data):
        """Reassemble fragmented MSG_MAPDATA datagrams into grid payloads,
        then store each grid's 10_000 tile bytes (row-major y*100+x,
        mirroring Grid::idx server-side).

        Datagram: int32 pktid, uint16 off, uint16 total, chunk bytes.
        Payload: int32 gx, int32 gy, NUL-terminated mnm, (u8 pidx, u8 fl)*
        terminated by 255, then one zlib blob (tiles + plots)."""
        pktid, off, total = struct.unpack_from("<iHH", data, 1)
        fr = self._mapfrag.setdefault(pktid, {})
        fr[off] = data[9:]
        if sum(len(c) for c in fr.values()) < total:
            return
        payload = b"".join(fr[k] for k in sorted(fr))
        self._mapfrag.pop(pktid, None)
        gx, gy = struct.unpack_from("<ii", payload, 0)
        self._mapreq_pending.pop((gx, gy), None)
        o = payload.index(0, 8) + 1  # skip the NUL-terminated mnm string
        while payload[o] != 255:  # plot-flag table
            o += 2
        o += 1
        raw = zlib.decompress(payload[o:])
        self.tiles[(gx, gy)] = raw[:TILE_BYTES]

    def tile_at(self, x, y):
        """Tile id at subtile coords, or None outside the streamed grids.
        Python floor division matches Rust div_euclid for positive
        divisors, so negative world coords resolve identically."""
        gc = (x // GRID_SPAN, y // GRID_SPAN)
        tiles = self.tiles.get(gc)
        if tiles is None:
            return None
        ix = (x // TILE_SPAN) % TILE_COUNT
        iy = (y // TILE_SPAN) % TILE_COUNT
        return tiles[iy * TILE_COUNT + ix]

    def walkable(self, x, y):
        return self.tile_at(x, y) not in (None, *IMPASSABLE_TILES)

    def _tile_walkable(self, t):
        """Walkability of the tile CONTAINING tile-corner subtile coords,
        sampled at the tile center (deposit corners sit ON impassable
        mountain tiles; their centers decide the path)."""
        return self.walkable(t[0] * TILE_SPAN + 5, t[1] * TILE_SPAN + 5)

    def line_clear(self, x1, y1, x2, y2):
        """Client mirror of state::path_clear: sample every tile along
        the segment (one sample per tile of Manhattan distance)."""
        dist = max(abs(x2 - x1) + abs(y2 - y1), 1)
        steps = min(max(dist // TILE_SPAN, 1), 1000)
        for i in range(steps + 1):
            x = x1 + (x2 - x1) * i // steps
            y = y1 + (y2 - y1) * i // steps
            if not self.walkable(x, y):
                return False
        return True

    def find_tile_path(self, start_sub, goal_sub):
        """BFS a walkable tile path from `start_sub` to `goal_sub` (subtile
        coords). When the goal tile itself is impassable (ore deposits sit
        on mountain tiles), the path targets the nearest walkable 4-neighbor
        instead. Returns tile-center subtile waypoints, or None when no
        path exists within the streamed grids (a node budget aborts
        unreachable searches - the 5x5 streamed grids hold ~250k tiles
        and an unbounded BFS over them stalled a live probe for minutes
        while the 60 s session timeout killed its server session)."""
        st = (start_sub[0] // TILE_SPAN, start_sub[1] // TILE_SPAN)
        gl = (goal_sub[0] // TILE_SPAN, goal_sub[1] // TILE_SPAN)
        if not self._tile_walkable(gl):
            side = [
                (gl[0] + 1, gl[1]), (gl[0] - 1, gl[1]),
                (gl[0], gl[1] + 1), (gl[0], gl[1] - 1),
            ]
            gl = next((t for t in side if self._tile_walkable(t)), None)
            if gl is None:
                return None
        if st == gl:
            return [gl]
        prev = {st: None}
        q = deque([st])
        budget = 40000
        dirs = ((1, 0), (-1, 0), (0, 1), (0, -1),
                (1, 1), (1, -1), (-1, 1), (-1, -1))
        while q and budget > 0:
            cur = q.popleft()
            if cur == gl:
                break
            for d in dirs:
                nb = (cur[0] + d[0], cur[1] + d[1])
                if nb in prev or not self._tile_walkable(nb):
                    continue
                prev[nb] = cur
                q.append(nb)
                budget -= 1
        if gl not in prev:
            return None
        path = []
        cur = gl
        while cur is not None:
            path.append(cur)
            cur = prev[cur]
        path.reverse()
        return [(t[0] * TILE_SPAN + 5, t[1] * TILE_SPAN + 5) for t in path]

    def nav_walk(self, target, stop=60, max_clicks=400, log=None):
        """Walk to `target` (subtile coords) over the streamed tile map:
        BFS a tile path, then click SHORT segments along it (about five
        waypoints per click). Short clicks survive minor client/server
        sampling differences in the straight-line check; the farthest-
        line-clear variant got whole clicks rejected on long diagonal
        runs. Returns True when within `stop` subtiles of the target."""
        for n in range(max_clicks):
            pos = self.gobs[self.player_gob]["pos"]
            dx, dy = target[0] - pos[0], target[1] - pos[1]
            if abs(dx) + abs(dy) <= stop:
                return True
            path = self.find_tile_path(pos, target)
            if not path:
                if log:
                    log("nav: no tile path %s -> %s" % (pos, target))
                return False
            # Click the farthest of the next few waypoints whose whole
            # segment passes the local line check (client mirror of
            # state::path_clear); fall back to the very next waypoint.
            far = None
            for w in path[:6]:
                if self.line_clear(pos[0], pos[1], w[0], w[1]):
                    far = w
            if far is None:
                far = path[0]
            self.click_ground(far[0], far[1])
            d = abs(far[0] - pos[0]) + abs(far[1] - pos[1])
            stalled = (pos, far)
            self.pump(min(1.2 + d / 50.0, 3.0))
            if n % 25 == 0 and log:
                now = self.gobs[self.player_gob]["pos"]
                log("nav %d: %s -> %s (click %s)" % (n, pos, now, far))
            after = self.gobs[self.player_gob]["pos"]
            if after == pos:
                # Click rejected: one retry through a pure 4-neighbor
                # sidestep, then keep going (the BFS re-plan usually
                # picks a different next hop anyway).
                nxt = path[0]
                side = [
                    (nxt[0] + 11, nxt[1]), (nxt[0] - 11, nxt[1]),
                    (nxt[0], nxt[1] + 11), (nxt[0], nxt[1] - 11),
                ]
                side = [s for s in side if self.line_clear(
                    pos[0], pos[1], s[0], s[1])]
                if side:
                    self.click_ground(*side[0])
                    self.pump(1.5)
                else:
                    self.stalls = getattr(self, "stalls", 0) + 1
                    if self.stalls >= 6:
                        if log:
                            log("nav: stalled at %s (%s)" % (pos, stalled))
                        return False
                    continue
            self.stalls = 0
        pos = self.gobs[self.player_gob]["pos"]
        return abs(target[0] - pos[0]) + abs(target[1] - pos[1]) <= stop

    # ---- protocol handlers -------------------------------------------------
    def on_datagram(self, data):
        if data[0] == MSG_ACK:
            return
        if data[0] == MSG_MAPDATA:
            self.mapdata_datagrams += 1
            self._on_mapdata(data)
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
                # Bundled (non-last) sub-message: the server writes
                # type|0x80, uint16 body-length, body (rel.rs
                # poll_transmit) - the same shape Session.java reads.
                # The body starts AFTER the 2 length bytes; the old
                # data[off:off+ln] slice swallowed them and shifted
                # every mid-bundle widget parse by 2 bytes.
                ln = struct.unpack("<H", data[off:off + 2])[0]
                body = data[off + 2:off + 2 + ln]
                off += 2 + ln
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
                self._send(bytes([MSG_ACK]) + le16((self.rseq - 1) & 0xFFFF))
            elif ((seq - self.rseq) & 0xFFFF) < 0x8000:
                self.held[seq] = (t, body)
            seq = (seq + 1) & 0xFFFF

    def on_rel(self, t, body):
        if t == RMSG_NEWWDG:
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            try:
                name = body[2:nend].decode()
            except UnicodeDecodeError:
                # Diagnostic: a widget name that is not UTF-8 points at
                # a bundling/parsing drift - dump the body and skip the
                # widget so the probe can keep driving.
                print("NEWWDG PARSE: t=%d body=%r" % (t, body[:48]))
                return
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
                self.sm_args[wid] = list(self.sm_opts)
            elif name == "slenchat":
                self.chat_id = wid
            elif name == "chr":
                self.chr_id = wid
            elif name == "pv":
                self.pv_id = wid
            elif name == "item" and len(args) >= 4:
                # args: [wire res, ql, drag, tooltip, num]
                self.item_info[wid] = {
                    "res": self.resids.get(args[0]),
                    "ql": args[1] if isinstance(args[1], int) else None,
                    "tt": args[3] if isinstance(args[3], str) else "",
                    # Stack count (arg 4): probes assert merged-stack
                    # totals (the bronze charge must land count = 2).
                    # Older servers may omit the arg; default 1.
                    "count": args[4] if len(args) >= 5 and isinstance(args[4], int) else 1,
                }
            if name == "chr":
                self.cattr_at_chr = set(self.cattr_names)
        elif t == RMSG_DSTWDG:
            wid = struct.unpack("<H", body[0:2])[0]
            self.widgets.pop(wid, None)
            self.item_info.pop(wid, None)
            self.sm_args.pop(wid, None)
            self.destroyed.add(wid)
            if wid == self.sm_wid:
                self.sm_wid = None
                self.sm_opts = []
        elif t == RMSG_WDGMSG:
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            args = list(self.parse_args(body[nend + 1:]))
            if name == "place" and wid == self.mapview_id:
                self.place_seen = args
            elif name == "exp" and wid == self.chr_id:
                self.exp_seen = args[0] if args else None
            elif name == "log" and wid == self.chat_id:
                text = args[0] if args else ""
                color = next((a for a in args if isinstance(a, tuple)), None)
                self.chat_lines.append((text, color))
        elif t == RMSG_RESID:
            wire = struct.unpack("<H", body[0:2])[0]
            end = body.index(0, 2)
            name = body[2:end].decode()
            self.resids[wire] = name
        elif t == RMSG_CATTR:
            # entries (string name, i32 base, i32 compiled) until eom
            off = 0
            while off < len(body):
                end = body.index(0, off)
                nm = body[off:end].decode()
                self.cattr_names.add(nm)
                base, comp = struct.unpack("<ii", body[end + 1:end + 9])
                self.attrs[nm] = comp
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
            elif ty == LIST_COLOR:
                yield tuple(buf[off:off + 4])
                off += 4
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
                    # Java OCache.move(): updates the base rc ONLY - it
                    # never cancels a live LinMove. A retransmitted
                    # entry block (frame 0, OD_MOVE) can legally arrive
                    # AFTER a fresh LINBEG (frame 1); the old reset here
                    # wiped "linbeg" and made the first walk leg after
                    # world entry look like it never started (session
                    # 87 root cause). Arrival is the FINAL LINSTEP
                    # (l >= c) below, exactly like OCache.linstep's
                    # delattr(Moving).
                elif op == "LINBEG":
                    g["linbeg"] = arg
                    g["linbegs"].append(arg)
                    g["linstep"] = 0
                    g["lin_t"] = time.time()
                    # Anchor the streamed pos at the path START: the
                    # pos only used to move on OD_MOVE (the final
                    # landing), so a mid-flight beast looked parked at
                    # its last landing point - the taming chase closed
                    # on a ghost and every attack click died at the
                    # 33-subtile reach gate (session 83).
                    g["pos"] = (arg[0], arg[1])
                elif op == "LINSTEP":
                    g["linstep"] = arg
                    g["linsteps"].append(arg)
                    g["lin_t"] = time.time()
                    if g.get("linbeg"):
                        sx, sy, tx, ty, c = g["linbeg"]
                        f = min(max(arg / max(c, 1), 0.0), 1.0)
                        g["pos"] = (
                            int(sx + (tx - sx) * f),
                            int(sy + (ty - sy) * f))
                        if arg >= c:
                            # Java OCache.linstep(): a step at/after the
                            # step count ENDS the move (delattr Moving)
                            # - the final LINSTEP is the arrival marker,
                            # OD_MOVE is not. Snap the landing position
                            # to the target like LinMove.getc at f=1.
                            g["pos"] = (tx, ty)
                            g["linbeg"] = None
                elif op == "LAYERS":
                    base, _ids = arg
                    # The layered base is the avatar body resource; record
                    # it so player-gob detection keeps working now that
                    # players spawn without a plain OD_RES.
                    if base is not None:
                        g["res"] = self.resids.get(base)
                elif op == "BUDDY":
                    self.buddy_names[gobid] = arg
                    if arg == self.username:
                        self.player_gob = gobid
                elif op == "FOLLOW":
                    # Session 90: the leash attr. A live target renders
                    # the beast AT its tamer (Java Following.getc() =
                    # the target's position) - live_pos models that
                    # below. -1 clears the leash.
                    g["follow"] = arg if arg is not None and arg != -1 else None

    # ---- scenario actions --------------------------------------------------
    def live_pos(self, gobid):
        """The best-known position of a gob RIGHT NOW. A leashed gob
        renders AT its tamer (Java Following.getc() returns the
        TARGET's position - session 90): live_pos models the client.
        Otherwise: the streamed pos (anchored/stepped by
        LINBEG+LINSTEP) extrapolated along the live LinMove at the
        tick cadence (server steps = 100 ms each, the same clock the
        Java client's LinMove.getc runs on). A mid-flight beast
        reports its on-path position instead of its last landing -
        the taming chase needs this to close the real gap (session
        83)."""
        g = self.gobs.get(gobid)
        if not g:
            return None
        # Session 90: the leash render wins over any LinMove state
        # (the server steps followed beasts silently server-side).
        flw = g.get("follow")
        if flw is not None and flw in self.gobs and flw != gobid:
            return self.live_pos(flw)
        lb = g.get("linbeg")
        pos = g.get("pos")
        if not lb or not pos:
            return pos
        sx, sy, tx, ty, c = lb
        if c <= 0:
            return pos
        l = g.get("linstep", 0) or 0
        # Extrapolate forward from the last LINSTEP at one step per
        # 200/3 ms (the client LinMove cadence, state.rs client_steps)
        # - LINSTEP itself only arrives every two ticks.
        dt = (time.time() - g.get("lin_t", 0)) / (0.2 / 3.0)
        f = (l + dt) / c
        f = min(max(f, 0.0), 1.0)
        return (int(sx + (tx - sx) * f), int(sy + (ty - sy) * f))

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
