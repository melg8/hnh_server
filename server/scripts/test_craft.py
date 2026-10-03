#!/usr/bin/env python3
"""End-to-end crafting + eating verification against a running server.

Flow (crafting-and-building.md making protocol):
  1. Auth + session + world entry (same wire path as test_client.py).
  2. act("craft", "axe") on the menugrid -> expect a `make` widget + `pop`.
  3. `make 0` -> expect item widgets (the crafted stone axe in inv).
  4. iact on a food item -> expect an `sm` (FlowerMenu) with "Eat".
  5. `cl 0` -> expect the `food` uimsg on the chr widget (FEP bar update).

Exit 0 only when the whole chain passes; prints per-step diagnostics.
"""
import socket
import struct
import sys
import time


def le16(v):
    return struct.pack("<H", v & 0xFFFF)


def le32(v):
    return struct.pack("<i", v)


def havstr(s):
    return s.encode() + b"\x00"


class CraftClient:
    def __init__(self, username):
        self.username = username
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.settimeout(0.3)
        self.server = ("127.0.0.1", 1870)
        self.tseq = 0
        self.rseq = 0
        self.held = {}
        self.widgets = {}
        self.charlist_id = None
        self.mapview_id = None
        self.slen_id = None
        self.scm_id = None
        self.make_wid = None
        self.pop_seen = False
        self.sm_wid = None
        self.sm_opts = []
        self.chr_wid = None
        self.food_msg = False
        self.item_labels = {}

    # ---- wire helpers ----
    def send_rel_subs(self, subs):
        out = bytes([1]) + le16(self.tseq)
        for i, p in enumerate(subs):
            if i < len(subs) - 1:
                out += bytes([p[0] | 0x80]) + le16(len(p) - 1) + p[1:]
            else:
                out += p
        self.tseq += len(subs)
        self.sock.sendto(out, self.server)

    @staticmethod
    def parse_uimsg(body):
        """[wid][name][typed args] -> (wid, name, args)."""
        wid = struct.unpack("<H", body[0:2])[0]
        off = 2
        nend = body.index(0, off)
        name = body[off:nend].decode()
        off = nend + 1
        args = []
        while off < len(body):
            t = body[off]
            off += 1
            if t == 0:
                break
            if t == 1:
                args.append(struct.unpack("<i", body[off:off + 4])[0])
                off += 4
            elif t == 2:
                e = body.index(0, off)
                args.append(body[off:e].decode())
                off = e + 1
            elif t == 3:
                args.append(struct.unpack("<i", body[off:off + 4])[0])
                args.append(struct.unpack("<i", body[off + 4:off + 8])[0])
                off += 8
            elif t == 6:
                args.append(tuple(body[off:off + 4]))
                off += 4
            else:
                break
        return wid, name, args

    @staticmethod
    def parse_newwdg(body):
        """[wid][type str][i32 x][i32 y][u16 parent][typed args]."""
        wid = struct.unpack("<H", body[0:2])[0]
        off = 2
        nend = body.index(0, off)
        name = body[off:nend].decode()
        off = nend + 1
        off += 8  # coord x,y (two int32)
        off += 2  # parent uint16
        args = []
        # widget factory args share the typed-list encoding (T_* tags)
        while off < len(body):
            t = body[off]
            off += 1
            if t == 0:
                break
            if t == 1:
                args.append(struct.unpack("<i", body[off:off + 4])[0])
                off += 4
            elif t == 2:
                e = body.index(0, off)
                args.append(body[off:e].decode())
                off = e + 1
            elif t == 3:
                args.append((struct.unpack("<i", body[off:off + 4])[0],
                             struct.unpack("<i", body[off + 4:off + 8])[0]))
                off += 8
            elif t == 6:
                args.append(tuple(body[off:off + 4]))
                off += 4
            else:
                break
        return wid, name, args

    # ---- message handling ----
    def on_rel(self, t, body):
        if t == 0:
            wid, name, args = self.parse_newwdg(body)
            self.widgets[wid] = name
            if name == "charlist":
                self.charlist_id = wid
            elif name == "mapview":
                self.mapview_id = wid
                for gy in (-1, 0, 1):
                    for gx in (-1, 0, 1):
                        self.sock.sendto(bytes([4]) + le32(gx) + le32(gy), self.server)
            elif name == "slen":
                self.slen_id = wid
            elif name == "scm":
                self.scm_id = wid
            elif name == "make":
                self.make_wid = wid
            elif name == "sm":
                self.sm_wid = wid
                self.sm_opts = [a for a in args if isinstance(a, str)]
            elif name == "item":
                # item factory args: (res, ql, drag, tooltip, num)
                self.item_labels[wid] = args[3] if len(args) >= 4 and isinstance(args[3], str) else ""
            elif name == "chr":
                self.chr_wid = wid
        elif t == 1:
            wid, name, args = self.parse_uimsg(body)
            if name == "pop":
                self.pop_seen = True
            if name == "food":
                self.food_msg = True

    def pump(self, seconds):
        """Receive and dispatch for up to `seconds` wall time."""
        deadline = time.time() + seconds
        while time.time() < deadline:
            try:
                data, _ = self.sock.recvfrom(65536)
            except socket.timeout:
                continue
            if data[0] == 2:
                continue
            if data[0] != 1:
                continue
            seq = struct.unpack("<H", data[1:3])[0]
            off = 3
            while off < len(data):
                t = data[off]
                off += 1
                if t & 0x80:
                    ln = struct.unpack("<H", data[off:off + 2])[0]
                    off += 2
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
                    self.sock.sendto(bytes([2]) + le16((self.rseq - 1) & 0xFFFF), self.server)
                elif ((seq - self.rseq) & 0xFFFF) < 0x8000:
                    self.held[seq] = (t, body)
                seq = (seq + 1) & 0xFFFF

    # ---- session phases ----
    def connect(self):
        import hashlib
        import ssl
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        raw = socket.create_connection(("127.0.0.1", 1871), timeout=5)
        s = ctx.wrap_socket(raw)

        def send_frame(ty, payload):
            s.sendall(bytes([ty, len(payload)]) + payload)

        def recv_frame():
            head = b""
            while len(head) < 2:
                chunk = s.recv(2 - len(head))
                if not chunk:
                    raise RuntimeError("eof")
                head += chunk
            ln = head[1]
            body = b""
            while len(body) < ln:
                chunk = s.recv(ln - len(body))
                if not chunk:
                    raise RuntimeError("eof")
                body += chunk
            return head[0], body

        send_frame(1, self.username.encode())
        ty, _ = recv_frame()
        if ty != 0:
            raise SystemExit("CMD_USR rejected")
        send_frame(2, hashlib.sha256(b"x").digest())
        ty, body = recv_frame()
        s.close()
        if ty != 0:
            raise SystemExit(f"CMD_PASSWD rejected type={ty}")
        cookie = body
        print(f"cookie ok ({len(cookie)} bytes)")
        sess = bytes([0]) + le16(1) + havstr("Haven") + le16(2) + havstr(self.username) + cookie
        for _ in range(10):
            self.sock.sendto(sess, self.server)
            try:
                data, _ = self.sock.recvfrom(65536)
                if data[0] == 0 and len(data) == 2 and data[1] == 0:
                    print("ACCEPTED")
                    return True
            except socket.timeout:
                pass
        print("session not accepted")
        return False

    def enter_world(self):
        deadline = time.time() + 8
        played = False
        inv_requested = False
        while time.time() < deadline:
            self.pump(0.3)
            if self.charlist_id and not played:
                played = True
                # Play the per-run character so the starter kit is fresh
                # regardless of what older sessions did to saved chars.
                self.send_rel_subs([bytes([1]) + le16(self.charlist_id) + b"play\x00"
                                    + bytes([2]) + self.username.encode() + b"\x00"
                                    + bytes([0])])
            # The client opens the inventory window with `inv` on slen
            # (SlenHud button); the starter-kit item widgets stream then.
            if played and self.slen_id and not inv_requested:
                inv_requested = True
                self.send_rel_subs([bytes([1]) + le16(self.slen_id) + b"inv\x00" + bytes([0])])
            items = [w for w, n in self.widgets.items() if n == "item"]
            if played and self.mapview_id and len(items) >= 3:
                return True
        return self.mapview_id is not None and len(
            [w for w, n in self.widgets.items() if n == "item"]) > 0

    def item_count(self):
        return len([w for w, n in self.widgets.items() if n == "item"])


def main():
    # Default to a one-shot character: the starter kit is granted only on
    # fresh creation, so a reused name would eventually run out of materials
    # and fail the flow through no fault of the server. Pass a name to reuse.
    username = sys.argv[1] if len(sys.argv) > 1 else "crafttest-%d" % int(time.time())
    c = CraftClient(username)
    if not c.connect():
        return 1
    if not c.enter_world():
        print("WORLD ENTRY: INCOMPLETE")
        return 1
    print("WORLD ENTRY: OK (starter kit items:", c.item_count(), ")")

    # Step 1-2: craft pagina -> makewindow + pop (act goes to the menugrid).
    if not c.scm_id:
        print("no menugrid widget")
        return 1
    act = (bytes([1]) + le16(c.scm_id) + b"act\x00" + bytes([2]) + b"craft\x00"
           + bytes([2]) + b"axe\x00" + bytes([0]))
    c.send_rel_subs([act])
    before = c.item_count()
    for _ in range(10):
        c.pump(0.4)
        if c.make_wid and c.pop_seen:
            break
    print("makewindow:", bool(c.make_wid), "pop:", c.pop_seen)
    if not (c.make_wid and c.pop_seen):
        print("CRAFT FLOW: FAIL (no make window or pop)")
        return 1

    # Step 3: press Craft once; expect a new item widget (stone axe).
    mk = bytes([1]) + le16(c.make_wid) + b"make\x00" + bytes([1]) + le32(0) + bytes([0])
    c.send_rel_subs([mk])
    for _ in range(10):
        c.pump(0.4)
        if c.item_count() > before:
            break
    crafted = c.item_count() > before
    print("CRAFT FLOW:", "OK" if crafted else "FAIL",
          f"(items {before} -> {c.item_count()})")
    if not crafted:
        return 1

    # Open the character sheet (chr) so the FEP bar exists to receive the
    # food update — mirrors a player with the char window open.
    if not c.slen_id:
        print("no slen widget")
        return 1
    chr_req = bytes([1]) + le16(c.slen_id) + b"chr\x00" + bytes([0])
    c.send_rel_subs([chr_req])
    for _ in range(10):
        c.pump(0.4)
        if c.chr_wid:
            break
    print("chr window open:", bool(c.chr_wid))

    # Step 4-5: eat flow - right-click a labeled (food) item widget.
    food_wids = [w for w, lab in c.item_labels.items() if lab]
    if not food_wids:
        print("EAT FLOW: FAIL (no food item widget found)")
        return 1
    iact = bytes([1]) + le16(food_wids[-1]) + b"iact\x00" + bytes([3]) + le32(0) + le32(0) + bytes([0])
    c.send_rel_subs([iact])
    for _ in range(10):
        c.pump(0.4)
        if c.sm_wid:
            break
    print("flower menu options:", c.sm_opts)
    if not c.sm_wid or "Eat" not in c.sm_opts:
        print("EAT FLOW: FAIL (no Eat option)")
        return 1
    cl = bytes([1]) + le16(c.sm_wid) + b"cl\x00" + bytes([1]) + le32(0) + bytes([0])
    c.send_rel_subs([cl])
    for _ in range(10):
        c.pump(0.4)
        if c.food_msg:
            break
    print("food uimsg on chr:", c.food_msg, "(chr wdg:", c.chr_wid, ")")
    print("EAT FLOW:", "OK" if c.food_msg else "FAIL")
    return 0 if c.food_msg else 1


if __name__ == "__main__":
    sys.exit(main())
