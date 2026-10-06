#!/usr/bin/env python3
"""Minimal manual session test: handshake, dump first datagrams.

Usage: test_client.py <username>
The cookie is obtained via the TLS auth channel using openssl s_client is
out of scope here; instead we rely on dev mode: the auth server accepts any
user/password, so we do the TLS handshake with openssl and pipe frames.
For raw datagram debugging we pass --debug-cookie to skip auth (server will
reject with SESSERR_AUTH=1, which is still useful to see replies).
"""
import socket, struct, sys, time, subprocess, hashlib

# Exactly the names CharWnd.baseval/skillval/Belief/Study dereference via
# glob.cattr.get() in the constructor; one missing name NPEs the client.
REQUIRED_CATTR = {
    "str", "agil", "intel", "cons", "perc", "csm", "dxt", "psy",
    "expmod",
    "unarmed", "melee", "ranged", "explore", "stealth", "sewing",
    "smithing", "carpentry", "cooking", "farming", "survive",
    "life", "night", "civil", "nature", "martial", "change",
}

def le16(v): return struct.pack("<H", v)
def le32(v): return struct.pack("<i", v)
def havstr(s): return s.encode() + b"\x00"

def auth_cookie(username: str, password: str = "x", port: int = 1871) -> bytes:
    """Do the TLS auth handshake natively (python ssl)."""
    import ssl as _ssl
    ctx = _ssl.SSLContext(_ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = _ssl.CERT_NONE
    raw = socket.create_connection(("127.0.0.1", port), timeout=5)
    tls = ctx.wrap_socket(raw)
    usr = username.encode()
    digest = hashlib.sha256(password.encode()).digest()

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

    send_frame(1, usr)
    ty, _ = recv_frame()
    if ty != 0:
        raise RuntimeError("CMD_USR rejected")
    send_frame(2, digest)
    ty, body = recv_frame()
    if ty != 0:
        raise RuntimeError(f"CMD_PASSWD rejected type={ty}")
    return body

def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "testuser"
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 1870
    auth_port = int(sys.argv[3]) if len(sys.argv) > 3 else 1871
    cookie = auth_cookie(username, port=auth_port)
    print(f"cookie ok ({len(cookie)} bytes)")
    print(f"game port: {port}")
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(0.5)
    server = ("127.0.0.1", port)
    sess = bytes([0]) + le16(1) + havstr("Haven") + le16(2) + havstr(username) + cookie
    for _ in range(5):
        sock.sendto(sess, server)
        try:
            data, _ = sock.recvfrom(65536)
            print(f"reply: type={data[0]} len={len(data)} body={data[1:10].hex()}")
            if data[0] == 0 and len(data) == 2 and data[1] == 0:
                print("ACCEPTED")
                break
        except socket.timeout:
            print("timeout")

    tseq = 0
    rseq = 0
    held = {}
    widgets = {}
    charlist_id = None
    mapview_id = None
    slen_id = None
    chr_seen = False
    cattr_names = set()
    cattr_at_chr = None
    stats = {"mapdata": 0, "objdata": 0, "resid": 0, "tiles": 0, "globlob": 0,
             "newwdg": 0, "wdgmsg": 0, "bytes": 0, "cattr": 0}

    def send_rel_subs(subs):
        """Bundle sub-message payloads into one MSG_REL datagram."""
        nonlocal tseq
        out = bytes([1]) + le16(tseq)
        for i, p in enumerate(subs):
            if i < len(subs) - 1:
                out += bytes([p[0] | 0x80]) + le16(len(p) - 1) + p[1:]
            else:
                out += p
        tseq += len(subs)
        sock.sendto(out, server)

    def on_rel(t, body):
        nonlocal charlist_id, mapview_id, slen_id, chr_seen, cattr_at_chr, rseq, held
        if t == 0:  # NEWWDG
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            widgets[wid] = name
            stats["newwdg"] += 1
            if name == "charlist":
                charlist_id = wid
            if name == "mapview":
                mapview_id = wid
                # Request surrounding grids immediately (client behavior).
                for gy in (-1, 0, 1):
                    for gx in (-1, 0, 1):
                        sock.sendto(bytes([4]) + le32(gx) + le32(gy), server)
            if name == "slen":
                slen_id = wid
                # Simulate the real client: SlenHud.binded() requests the
                # character sheet the moment the HUD binds, i.e. before any
                # map data arrives. The server must have already queued the
                # full CharWnd cattr set by the time it answers with the
                # `chr` newwidget.
                if not chr_seen:
                    send_rel_subs([bytes([1]) + le16(wid) + b"chr\x00" + bytes([0])])
            if name == "chr":
                chr_seen = True
                cattr_at_chr = set(cattr_names)
        elif t == 1:
            stats["wdgmsg"] += 1
        elif t == 4:
            stats["globlob"] += 1
        elif t == 5:  # PAGINAE: per entry '+' name\0 u16 ver
            off = 0
            while off < len(body):
                act = body[off]
                off += 1
                nend = body.index(0, off)
                nm = body[off:nend].decode()
                off = nend + 1
                ver = struct.unpack("<H", body[off:off + 2])[0]
                off += 2
                if act == 0x2B and nm.startswith("paginae/atk/"):
                    stats[f"pag:{nm}={ver}"] = 1
        elif t == 6:
            stats["resid"] += 1
        elif t == 9:  # CATTR: (name\0 int32 base int32 comp)*
            stats["cattr"] += 1
            off = 0
            while off < len(body):
                nend = body.index(0, off)
                nm = body[off:nend].decode()
                cattr_names.add(nm)
                off = nend + 9  # name\0 + two LE int32
        elif t == 11:
            stats["tiles"] += 1

    deadline = time.time() + 8
    played = False
    play_sent_at = 0
    while time.time() < deadline:
        try:
            data, _ = sock.recvfrom(65536)
        except socket.timeout:
            # Fire play once charlist seen
            if charlist_id and not played:
                played = True
                play_sent_at = time.time()
                # Play a per-run character (dev auth auto-provisions any
                # name): reusing a fixed name would eventually inherit a
                # stale inventory from previous runs.
                send_rel_subs([bytes([1]) + le16(charlist_id) + b"play\x00" + bytes([2])
                              + username.encode() + b"\x00" + bytes([0])])
            continue
        stats["bytes"] += len(data)
        if data[0] == 2:  # ACK for our play
            continue
        if data[0] != 1:
            if data[0] == 5:
                stats["mapdata"] += 1
            elif data[0] == 6:
                stats["objdata"] += 1
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
            if seq == rseq:
                on_rel(t, body)
                rseq = (rseq + 1) & 0xFFFF
                while rseq in held:
                    t2, b2 = held.pop(rseq)
                    on_rel(t2, b2)
                    rseq = (rseq + 1) & 0xFFFF
                sock.sendto(bytes([2]) + le16((rseq - 1) & 0xFFFF), server)
            elif ((seq - rseq) & 0xFFFF) < 0x8000:
                held[seq] = (t, body)
            seq = (seq + 1) & 0xFFFF

    print("widgets:", {hex(k): v for k, v in sorted(widgets.items())})
    print("stats:", stats)
    ok = mapview_id is not None and stats["mapdata"] > 0 and stats["objdata"] > 0
    print("WORLD ENTRY:", "OK" if ok else "INCOMPLETE")
    if chr_seen:
        missing = REQUIRED_CATTR - (cattr_at_chr or set())
        if missing:
            print("CATTR ORDER: FAIL (missing at chr creation:", sorted(missing), ")")
        else:
            print("CATTR ORDER: OK (%d names present before chr widget)" % len(REQUIRED_CATTR))
    else:
        print("CATTR ORDER: chr widget never created")
    if mapview_id and stats["mapdata"] > 0:
        # Walk test: click 20 tiles east
        click = bytes([1]) + le16(mapview_id) + b"click\x00"
        click += bytes([3]) + le32(0) + le32(0)          # c0 screen coord
        click += bytes([3]) + le32(555*11 + 220) + le32(555*11)  # mc
        click += bytes([1]) + le32(1)                    # button
        click += bytes([1]) + le32(0)                    # modflags
        click += bytes([0])
        send_rel_subs([click])
        print("walk click sent")
    time.sleep(1)

main()
