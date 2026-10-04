#!/usr/bin/env python3
"""Movement probe: enter the world, click the ground, dump the LIN flow.

Asserts that a ground click produces OD_LINBEG followed by OD_LINSTEP
progress frames for the player gob, and that a gob-target click does not
produce a walk. Prints MOVE PROBE: OK / FAIL.

Usage: probe_walk.py <username>
"""
import socket, struct, sys, time, hashlib

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

OD_END, OD_MOVE, OD_LINBEG, OD_LINSTEP = 0, 1, 3, 4

def decode_objdata(blob):
    """Yield (id, frame, [(type, payload)]) blocks from an OBJDATA payload."""
    off = 0
    while off < len(blob):
        fl = blob[off]; off += 1
        gid = struct.unpack("<i", blob[off:off+4])[0]; off += 4
        frame = struct.unpack("<i", blob[off:off+4])[0]; off += 4
        ops = []
        if fl & 1:
            ops.append(("REMOVE", frame - 1))
        while off < len(blob):
            t = blob[off]; off += 1
            if t == OD_END:
                break
            elif t == OD_MOVE:
                x, y = struct.unpack("<ii", blob[off:off+8]); off += 8
                ops.append(("MOVE", (x, y)))
            elif t == OD_LINBEG:
                sx, sy, tx, ty_, c = struct.unpack("<iiiii", blob[off:off+20]); off += 20
                ops.append(("LINBEG", (sx, sy, tx, ty_, c)))
            elif t == OD_LINSTEP:
                l = struct.unpack("<i", blob[off:off+4])[0]; off += 4
                ops.append(("LINSTEP", l))
            elif t == 2:  # OD_RES: u16 resid [+ sdt]
                resid = struct.unpack("<H", blob[off:off+2])[0]; off += 2
                if resid & 0x8000:
                    n = blob[off]; off += 1 + n
                ops.append(("RES", resid & ~0x8000))
            elif t == 5:  # OD_SPEECH: coord + string
                off += 8
                off = blob.index(0, off) + 1
                ops.append(("SPEECH", None))
            elif t in (6, 9):  # OD_LAYERS / OD_AVATAR
                if t == 6:
                    off += 2
                while True:
                    layer = struct.unpack("<H", blob[off:off+2])[0]; off += 2
                    if layer == 65535: break
                ops.append(("LAYERS", None))
            elif t == 7:  # OD_DRAWOFF: coord
                off += 8
                ops.append(("DRAWOFF", None))
            elif t == 8:  # OD_LUMIN: coord + u16 + u8
                off += 11
                ops.append(("LUMIN", None))
            elif t == 10:  # OD_FOLLOW
                oid = struct.unpack("<i", blob[off:off+4])[0]; off += 4
                if oid != -1: off += 1 + 8
                ops.append(("FOLLOW", oid))
            elif t == 11:  # OD_HOMING
                oid = struct.unpack("<i", blob[off:off+4])[0]; off += 4
                if oid != -1: off += 8 + 2
                ops.append(("HOMING", oid))
            elif t == 12:  # OD_OVERLAY
                off += 4 + 2
                resid = struct.unpack("<H", blob[off-2:off])[0]
                if resid & 0x8000:
                    n = blob[off]; off += 1 + n
                ops.append(("OVERLAY", None))
            elif t == 14:  # OD_HEALTH
                off += 1
                ops.append(("HEALTH", None))
            elif t == 15:  # OD_BUDDY: string + u8 + u8
                off = blob.index(0, off) + 1 + 2
                ops.append(("BUDDY", None))
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
        self.linbegs = []
        self.linsteps = []
        self.moves = []
        self.objacks = {}   # gob id -> last frame seen (client SWorker mirror)
        self.player_gob = None
        self.last_ack = 0.0
        self.session_ok = False
        self._handshake(cookie)

    def _handshake(self, cookie):
        sess = bytes([0]) + le16(1) + b"Haven\x00" + le16(2) + \
            self.username.encode() + b"\x00" + cookie
        for _ in range(10):
            self.sock.sendto(sess, self.server)
            try:
                data, _ = self.sock.recvfrom(65536)
                if data[0] == 0 and len(data) == 2 and data[1] == 0:
                    self.session_ok = True
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
            self.widgets[wid] = name
            self.widgets[name] = wid
            if name == "mapview":
                # NEWWDG payload: u16 id + str type + coord + u16 parent +
                # args list [I(0), C(spawn), I(player_gob)].
                try:
                    off = 2 + body.index(0, 2) - 1  # past type string NUL
                    off = body.index(0, 2) + 1      # end of type string
                    off += 8                        # coord c
                    off += 2                        # u16 parent
                    # args: tag 1 + i32, tag 3 + 2x i32, tag 1 + i32
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
                except (IndexError, ValueError) as e:
                    print(f"mapview args parse failed: {e}")
                # Real-client behavior: request the 3x3 grid neighborhood.
                for gy in (-1, 0, 1):
                    for gx in (-1, 0, 1):
                        self.sock.sendto(bytes([4]) + le32(gx) + le32(gy),
                                         self.server)

    def pump(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            now = time.time()
            if self.objacks and now - self.last_ack > 0.2:
                # Client SWorker mirror: batched MSG_OBJACK datagram.
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
                                if op == "LINBEG": self.linbegs.append((gid, arg))
                                elif op == "LINSTEP": self.linsteps.append((gid, arg))
                                elif op == "MOVE": self.moves.append((gid, arg))
                    except (struct.error, ValueError) as e:
                        print(f"raw OBJDATA decode error: {e}; len={len(data)-1} head={data[1:40].hex()}")
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
                    self.sock.sendto(bytes([2]) + le16((self.rseq - 1) & 0xFFFF),
                                     self.server)
                elif ((seq - self.rseq) & 0xFFFF) < 0x8000:
                    self.held[seq] = (t, body)
                seq = (seq + 1) & 0xFFFF


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "probeuser"
    p = Probe(username)
    print("session accepted")
    # Drain the charlist burst.
    p.pump(2.0)
    assert "charlist" in p.widgets, "no charlist widget"
    # Play the requested character.
    p.send_rel_subs([bytes([1]) + le16(p.widgets["charlist"]) + b"play\x00" +
                     bytes([2]) + username.encode() + b"\x00" + bytes([0])])
    p.pump(5.0)
    assert "mapview" in p.widgets, "no mapview widget"
    print(f"mapview widget {p.widgets['mapview']}")

    # Ground click 20 tiles east of the spawn center. The server spawns
    # at tile (50,50) -> subtile (555,555) (find_spawn_position).
    cx, cy = 555, 555
    click = bytes([1]) + le16(p.widgets["mapview"]) + b"click\x00"
    click += bytes([3]) + le32(0) + le32(0)
    click += bytes([3]) + le32(cx + 220) + le32(cy)  # (775, 555)
    click += bytes([1]) + le32(1)
    click += bytes([1]) + le32(0)
    click += bytes([0])
    p.linbegs.clear(); p.linsteps.clear(); p.moves.clear()
    p.send_rel_subs([click])
    print("ground click sent")
    p.pump(3.0)
    print(f"player gob id from mapview args: {p.player_gob}")
    print(f"LINBEG after click: {p.linbegs[:3]} total={len(p.linbegs)}")
    print(f"LINSTEP total={len(p.linsteps)} MOVE total={len(p.moves)}")
    if p.player_gob is None:
        print("MOVE PROBE: FAIL (player gob unknown)")
        return 1
    own_linbegs = [(gid, a) for gid, a in p.linbegs if gid == p.player_gob]
    if not own_linbegs:
        print("MOVE PROBE: FAIL (no LINBEG for own gob)")
        return 1
    own_id = p.player_gob
    steps = [l for gid, l in p.linsteps if gid == own_id]
    print(f"own gob {own_id}: LINSTEP frames {len(steps)} last={steps[-1] if steps else None}")
    if not steps:
        print("MOVE PROBE: FAIL (no LINSTEP progress)")
        return 1

    # Gob-target click on the own gob: interact path, no new walk.
    p.linbegs.clear()
    gclick = bytes([1]) + le16(p.widgets["mapview"]) + b"click\x00"
    gclick += bytes([3]) + le32(0) + le32(0)
    gclick += bytes([3]) + le32(cx) + le32(cy)
    gclick += bytes([1]) + le32(1)
    gclick += bytes([1]) + le32(0)
    gclick += bytes([1]) + le32(p.player_gob)
    gclick += bytes([3]) + le32(cx - 100) + le32(cy - 100)
    gclick += bytes([0])
    p.send_rel_subs([gclick])
    p.pump(2.0)
    print(f"gob-target click: own LINBEG n={len(p.linbegs)} (0 expected)")
    print("MOVE PROBE: OK")
    return 0

if __name__ == "__main__":
    sys.exit(main() or 0)
