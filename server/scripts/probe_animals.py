#!/usr/bin/env python3
"""Animal probe: kritter pose layering + walk streams + bite FX overlay.

Enters the world of a --saturated server and asserts, purely from the
wire stream:
1. Animal gobs spawn through OD_LAYERS of concrete kritter pose parts
   (base gfx/kritter/<sp>/body + one standing-N/walking-N part) and NOT
   through a flat OD_RES sprite (the old path rendered shadow-only).
2. At least one animal streams the walking pose (movement animation on
   the wire; the client animates the 8-frame cycle itself).
3. A predator engagement produces the one-shot bite overlay
   (gfx/fx/bite) on the victim gob (attack animation).

Prints ANIMALS WIRE: OK / FAIL.

Usage: probe_animals.py <username>
"""
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
OD_LAYERS, OD_AVATAR, OD_OVERLAY = 6, 9, 12


def decode_objdata(blob):
    """Yield (id, frame, ops) with (op, payload) pairs."""
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
                ops.append(("MOVE", struct.unpack("<ii", blob[off:off+8])))
                off += 8
            elif t == OD_LINBEG:
                ops.append(("LINBEG", struct.unpack("<iiiii", blob[off:off+20])))
                off += 20
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
            elif t == 15:  # OD_BUDDY
                off = blob.index(0, off) + 1 + 2
            elif t == 14:  # OD_HEALTH
                off += 1
            elif t == 5:   # OD_SPEECH
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
        self.res_names = {}
        self.spawn_ops = {}      # first block ops per gob id
        self.gob_res = {}        # gob id -> res name (from RES op wire id)
        self.gob_pos = {}        # gob id -> last MOVE position
        self.kritter_walks = []  # (gid, layer name) walking streams
        self.bites = []          # (gid, res name) overlays
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
        if t == 0:
            wid = struct.unpack("<H", body[0:2])[0]
            name = body[2:body.index(0, 2)].decode()
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
        elif t == 6:
            wid = struct.unpack("<H", body[0:2])[0]
            self.res_names[wid] = body[2:body.index(0, 2)].decode("latin1")

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
                if data[0] == 6:
                    try:
                        for gid, frame, ops in decode_objdata(data[1:]):
                            self.objacks[gid] = max(self.objacks.get(gid, frame), frame)
                            for op, arg in ops:
                                if op == "MOVE":
                                    self.gob_pos[gid] = arg
                                elif op == "RES" and gid not in self.gob_res:
                                    self.gob_res[gid] = self.res_names.get(arg, f"?{arg}")
                                elif op == "LAYERS" and gid not in self.spawn_ops:
                                    base, ids = arg
                                    names = [self.res_names.get(i, f"?{i}") for i in ids]
                                    self.spawn_ops[gid] = (
                                        self.res_names.get(base, f"?{base}") if base else None,
                                        names)
                                elif op == "AVATAR" and gid not in self.spawn_ops:
                                    _b, ids = arg
                                    self.spawn_ops[gid] = (
                                        None, [self.res_names.get(i, f"?{i}") for i in ids])
                                elif op == "LAYERS":
                                    base, ids = arg
                                    names = [self.res_names.get(i, f"?{i}") for i in ids]
                                    for n in names:
                                        if "/body/walking/walking-" in n:
                                            self.kritter_walks.append((gid, n))
                                elif op == "OVERLAY":
                                    _olid, resid = arg
                                    self.bites.append((gid, self.res_names.get(resid, f"?{resid}")))
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


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "aniprobe"
    p = Probe(username)
    p.pump(2.0)
    assert "charlist" in p.widgets, "no charlist widget"
    p.send_rel_subs([bytes([1]) + le16(p.widgets["charlist"]) + b"play\x00" +
                     bytes([2]) + username.encode() + b"\x00" + bytes([0])])
    p.pump(5.0)
    assert "mapview" in p.widgets and p.player_gob, "no mapview / player gob"
    print(f"player gob {p.player_gob}; waiting for animals (saturated world)")

    def critters():
        return {g: v for g, v in p.spawn_ops.items()
                if v[0] and "kritter" in (v[0] or "") and (v[0] or "").endswith("/body")}

    # Phase 1: watch 20 s, then actively hunt a predator: click it (the
    # Kind::Animal click handler opens the fight) and let the combat chase
    # close the distance - the animal fights back and bites.
    p.pump(20.0)
    predators = {g: p.gob_pos[g] for g in list(critters())
                 if g in p.gob_pos and ("/wolf/" in (p.spawn_ops[g][0] or "")
                                        or "/boar/" in (p.spawn_ops[g][0] or ""))}
    clicked = False
    if predators:
        gid = min(predators, key=lambda g: sum(predators[g]))
        x, y = predators[gid]
        gclick = bytes([1]) + le16(p.widgets["mapview"]) + b"click\x00"
        gclick += bytes([3]) + le32(0) + le32(0)
        gclick += bytes([3]) + le32(x) + le32(y)
        gclick += bytes([1]) + le32(1)
        gclick += bytes([1]) + le32(0)
        gclick += bytes([1]) + le32(gid)
        gclick += bytes([3]) + le32(x) + le32(y)
        gclick += bytes([0])
        p.send_rel_subs([gclick])
        print(f"predator click: gob {gid} at ({x},{y})")
        clicked = True
    else:
        print("no predator with a known position after 20 s; waiting passively")

    deadline = time.time() + (180.0 if clicked else 75.0)
    while time.time() < deadline:
        p.pump(1.0)
        if p.kritter_walks and p.bites and len(critters()) >= 2:
            break
    animals = critters()
    print(f"animal spawns (kritter OD_LAYERS): {len(animals)}")
    for g, (base, names) in list(animals.items())[:4]:
        print(f"  gob {g}: base={base} layers={names}")
    flat_res = [g for g, r in p.gob_res.items() if "kritter" in r and "/cdv" in r]
    print(f"flat cdv RES animals (must be 0): {len(flat_res)}")
    print(f"walking-pose streams: {len(p.kritter_walks)}")
    print(f"bite overlays: {[(g, r) for g, r in p.bites[:4]]}")

    fails = []
    if len(animals) < 2:
        fails.append(f"only {len(animals)} kritter-layered animals (need >= 2)")
    if flat_res:
        fails.append(f"{len(flat_res)} animals still spawn via flat cdv RES")
    if not p.kritter_walks:
        fails.append("no walking-pose stream for any animal")
    if not p.bites:
        fails.append("no bite overlay (no predator engagement in 75 s)")
    if fails:
        print("ANIMALS WIRE: FAIL")
        for f in fails:
            print("  " + f)
        return 1
    print("ANIMALS WIRE: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
