#!/usr/bin/env python3
"""Direction probe: verify the wire carries direction-correct pose layers.

Enters the world, clicks ground targets in three different directions
(east, south, north-west) and, for each leg, captures the OD_LAYERS block
streamed for the own gob right after OD_LINBEG (walking set) and on
arrival (standing set). Both sets must name directional resources whose
direction digit matches the quantized movement octant of the leg, and
they must stay on that one direction for the whole leg (no cycling).

Prints DIRECTION WIRE: OK / FAIL.

Usage: probe_direction.py <username>
"""
import math
import socket
import struct
import sys
import time
import hashlib

def le16(v): return struct.pack("<H", v)
def le32(v): return struct.pack("<i", v)

def auth_cookie(username, password="x"):
    import ssl as _ssl
    ctx = _ssl.SSLContext(_ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = _ssl.CERT_NONE
    raw = socket.create_connection(("127.0.0.1", 1871), timeout=5)
    tls = ctx.wrap_socket(raw)
    def send_frame(ty, payload): tls.sendall(bytes([ty, len(payload)]) + payload)
    def recv_frame():
        head = b""
        while len(head) < 2:
            c = tls.recv(2 - len(head))
            if not c: raise RuntimeError("eof")
            head += c
        ln = head[1]; body = b""
        while len(body) < ln:
            c = tls.recv(ln - len(body))
            if not c: raise RuntimeError("eof")
            body += c
        return head[0], body
    send_frame(1, username.encode())
    ty, _ = recv_frame()
    assert ty == 0, "USR rejected"
    send_frame(2, hashlib.sha256(password.encode()).digest())
    ty, body = recv_frame()
    assert ty == 0, "PASSWD rejected"
    return body

OD_END, OD_MOVE, OD_RES, OD_LINBEG, OD_LINSTEP = 0, 1, 2, 3, 4
OD_LAYERS, OD_AVATAR, OD_OVERLAY, OD_BUDDY = 6, 9, 12, 15


def move_dir(s, t):
    """Mirror of the server's move_dir: quantized movement octant."""
    dx, dy = t[0] - s[0], t[1] - s[1]
    if dx == 0 and dy == 0:
        return 0
    deg = math.degrees(math.atan2(dy, dx))
    return int(math.floor((deg + 22.5) / 45.0)) % 8


def decode_objdata(blob):
    """Yield (id, frame, [(type, payload)]) with layers resolved later."""
    off = 0
    while off < len(blob):
        fl = blob[off]; off += 1
        gid = struct.unpack("<i", blob[off:off+4])[0]; off += 4
        frame = struct.unpack("<i", blob[off:off+4])[0]; off += 4
        ops = []
        while off < len(blob):
            t = blob[off]; off += 1
            if t == OD_END:
                break
            elif t == OD_MOVE:
                ops.append(("MOVE", struct.unpack("<ii", blob[off:off+8])))
                off += 8
            elif t == OD_LINBEG:
                v = struct.unpack("<iiiii", blob[off:off+20]); off += 20
                ops.append(("LINBEG", v))
            elif t == OD_LINSTEP:
                ops.append(("LINSTEP", struct.unpack("<i", blob[off:off+4])[0]))
                off += 4
            elif t == OD_RES:
                resid = struct.unpack("<H", blob[off:off+2])[0]; off += 2
                if resid & 0x8000:
                    n = blob[off]; off += 1 + n
                ops.append(("RES", resid & ~0x8000))
            elif t in (OD_LAYERS, OD_AVATAR):
                base = None
                if t == OD_LAYERS:
                    base = struct.unpack("<H", blob[off:off+2])[0]; off += 2
                ids = []
                while True:
                    lid = struct.unpack("<H", blob[off:off+2])[0]; off += 2
                    if lid == 65535:
                        break
                    ids.append(lid)
                ops.append(("LAYERS" if t == OD_LAYERS else "AVATAR", (base, ids)))
            elif t == OD_OVERLAY:
                olid = struct.unpack("<i", blob[off:off+4])[0]; off += 4
                resid = struct.unpack("<H", blob[off:off+2])[0]; off += 2
                if resid & 0x8000:
                    n = blob[off]; off += 1 + n
                ops.append(("OVERLAY", (olid >> 1, resid & ~0x8000)))
            elif t == OD_BUDDY:
                off = blob.index(0, off) + 1 + 2
                ops.append(("BUDDY", None))
            elif t == 14:  # OD_HEALTH
                off += 1
            elif t == 5:  # OD_SPEECH
                off += 8
                off = blob.index(0, off) + 1
            else:
                ops.append((f"TYPE{t}", None))
                break
        yield (gid, frame, ops)


class Probe:
    def __init__(self, username):
        self.username = username
        cookie = auth_cookie(username)
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.settimeout(0.2)
        self.server = ("127.0.0.1", 1870)
        self.tseq = self.rseq = 0
        self.held = {}
        self.widgets = {}
        self.res_names = {}      # wire id -> resource name
        self.linbegs = []        # (gid, (sx, sy, tx, ty, c))
        self.layer_events = []   # (gid, base_name, [names])
        self.avatar_events = []  # (gid, [names]) from OD_AVATAR ops
        self.overlay_events = []  # (gid, olid, res_name)
        self.objacks = {}
        self.player_gob = None
        self.last_ack = 0.0
        self._handshake(cookie)

    def _handshake(self, cookie):
        sess = bytes([0]) + le16(1) + b"Haven\x00" + le16(2) + \
            self.username.encode() + b"\x00" + cookie
        for _ in range(10):
            self.sock.sendto(sess, self.server)
            try:
                data, _ = self.sock.recvfrom(65536)
                if data[0] == 0 and len(data) == 2 and data[1] == 0:
                    return
            except socket.timeout:
                pass
        raise RuntimeError("session not accepted")

    def send_rel_subs(self, subs):
        out = bytes([1]) + le16(self.tseq)
        for i, p in enumerate(subs):
            if i < len(subs) - 1:
                out += bytes([p[0] | 0x80]) + le16(len(p) - 1) + p[1:]
            else:
                out += p
        self.tseq += len(subs)
        self.sock.sendto(out, self.server)

    def on_rel(self, t, body):
        if t == 0:  # NEWWDG
            wid = struct.unpack("<H", body[0:2])[0]
            name = body[2:body.index(0, 2)].decode()
            self.widgets[wid] = wid
            self.widgets[name] = wid
            if name == "mapview":
                try:
                    off = body.index(0, 2) + 1
                    off += 8 + 2
                    ints = []
                    while off < len(body):
                        tag = body[off]; off += 1
                        if tag == 1:
                            ints.append(struct.unpack("<i", body[off:off+4])[0]); off += 4
                        elif tag == 3:
                            off += 8
                        else:
                            break
                    self.player_gob = ints[-1] if ints else None
                except (IndexError, ValueError):
                    pass
                for gy in (-1, 0, 1):
                    for gx in (-1, 0, 1):
                        self.sock.sendto(bytes([4]) + le32(gx) + le32(gy), self.server)
        elif t == 6:  # RESID: u16 wire + string name + u16 ver
            wid = struct.unpack("<H", body[0:2])[0]
            name = body[2:body.index(0, 2)].decode("latin1")
            self.res_names[wid] = name

    def pump(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            now = time.time()
            if self.objacks and now - self.last_ack > 0.2:
                msg = bytes([7])
                for gid, frame in self.objacks.items():
                    msg += le32(gid) + le32(frame)
                self.sock.sendto(msg, self.server)
                self.last_ack = now
            try:
                data, _ = self.sock.recvfrom(65536)
            except socket.timeout:
                continue
            if data[0] == 2:
                continue
            if data[0] != 1:
                if data[0] == 6:  # raw OBJDATA
                    try:
                        for gid, frame, ops in decode_objdata(data[1:]):
                            self.objacks[gid] = max(self.objacks.get(gid, frame), frame)
                            for op, arg in ops:
                                if op == "LINBEG":
                                    self.linbegs.append((gid, arg))
                                elif op == "LAYERS":
                                    base, ids = arg
                                    names = [self.res_names.get(i, f"?{i}") for i in ids]
                                    bn = self.res_names.get(base, f"?{base}") if base else None
                                    self.layer_events.append((gid, bn, names))
                                elif op == "AVATAR":
                                    _base, ids = arg
                                    self.avatar_events.append(
                                        (gid, [self.res_names.get(i, f"?{i}") for i in ids]))
                                elif op == "OVERLAY":
                                    olid, resid = arg
                                    self.overlay_events.append(
                                        (gid, olid, self.res_names.get(resid, f"?{resid}")))
                    except (struct.error, ValueError, IndexError):
                        pass
                continue
            seq = struct.unpack("<H", data[1:3])[0]
            off = 3
            while off < len(data):
                t = data[off]; off += 1
                if t & 0x80:
                    ln = struct.unpack("<H", data[off:off+2])[0]; off += 2
                    body = data[off:off+ln]; off += ln
                else:
                    body = data[off:]; off = len(data)
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

    def click(self, x, y):
        click = bytes([1]) + le16(self.widgets["mapview"]) + b"click\x00"
        click += bytes([3]) + le32(0) + le32(0)
        click += bytes([3]) + le32(x) + le32(y)
        click += bytes([1]) + le32(1)
        click += bytes([1]) + le32(0)
        click += bytes([0])
        self.send_rel_subs([click])


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "dirprobe"
    p = Probe(username)
    p.pump(2.0)
    assert "charlist" in p.widgets, "no charlist widget"
    p.send_rel_subs([bytes([1]) + le16(p.widgets["charlist"]) + b"play\x00" +
                     bytes([2]) + username.encode() + b"\x00" + bytes([0])])
    p.pump(5.0)
    assert "mapview" in p.widgets and p.player_gob, "no mapview / player gob"
    print(f"player gob {p.player_gob}")

    # The own gob's Avatar attribute (OD_AVATAR) must carry the banzai
    # doll set from the spawn block: Equipory.cdraw renders exactly this
    # (the missing Equipment paperdoll defect, wire side).
    doll = [names for g, names in p.avatar_events if g == p.player_gob and
            any("arm/banzai/" in n for n in names)]
    print(f"banzai doll sets on spawn: {len(doll)}")
    if not doll:
        print("DIRECTION WIRE: FAIL (no OD_AVATAR doll set)")
        return 1

    # Legs: east, south, north-west - three distinct octants (0, 2, 5).
    start = (555, 555)
    legs = [((start[0] + 220, start[1]), "+x"),
            ((start[0], start[1] + 220), "+y"),
            ((start[0] - 150, start[1] - 150), "-x-y")]
    fails = []
    for target, label in legs:
        # Current interpolated start = wherever the last leg ended. Wait
        # dynamically for arrival: legs can be ~450 subtile Manhattan at
        # 33 subtile/s (walk gait) ~= 14 s.
        p.linbegs.clear()
        p.layer_events.clear()
        p.click(*target)
        p.pump(0.7)
        lin = [(g, a) for g, a in p.linbegs if g == p.player_gob]
        if not lin:
            fails.append(f"{label}: no LINBEG")
            continue
        _, (sx, sy, tx, ty, _c) = lin[0]
        want = move_dir((sx, sy), (tx, ty))
        deadline = time.time() + 30.0
        while time.time() < deadline:
            p.pump(0.5)
            stand = [names for g, _b, names in p.layer_events
                     if g == p.player_gob and any("standing/legs-" in n for n in names)]
            if stand:
                break
        walk = [names for g, _b, names in p.layer_events
                if g == p.player_gob and any("walking/legs-" in n for n in names)]
        stand = [names for g, _b, names in p.layer_events
                 if g == p.player_gob and any("standing/legs-" in n for n in names)]
        if not walk:
            fails.append(f"{label}: no walking LAYERS for own gob")
            continue
        # Every walking set must use exactly the expected direction digit,
        # and only ONE walking set may appear (no per-frame streaming).
        wdirs = {n.rsplit("-", 1)[-1] for names in walk for n in names
                 if "walking/legs-" in n}
        if wdirs != {str(want)}:
            fails.append(f"{label}: walking dirs {wdirs}, want {{{want}}}")
        if len(walk) > 1:
            fails.append(f"{label}: {len(walk)} walking layer streams (cycling?)")
        if not stand:
            fails.append(f"{label}: no standing LAYERS on arrival")
            continue
        sdirs = {n.rsplit("-", 1)[-1] for names in stand for n in names
                 if "standing/legs-" in n}
        if sdirs != {str(want)}:
            fails.append(f"{label}: standing dirs {sdirs}, want {{{want}}}")
        print(f"leg {label}: lin ({sx},{sy})->({tx},{ty}) dir={want} "
              f"walk_streams={len(walk)} stand_streams={len(stand)}")

    # A re-click retarget keeps the doll off the walking path: the doll
    # rides only the spawn block, so no further banzai events are needed.
    if fails:
        print("DIRECTION WIRE: FAIL")
        for f in fails:
            print("  " + f)
        return 1
    print("DIRECTION WIRE: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
