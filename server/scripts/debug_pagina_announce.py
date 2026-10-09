#!/usr/bin/env python3
"""Capture the server's RMSG_PAGINAE announcements and compare each
announced (name, ver) pair against the real gameres file version.

Root cause probe for the MenuGrid PaginaException: the client loads
every announced pagina via HTTP and rejects a version mismatch with
"Wrong res version". A mismatch here is a server-side bug (file_version
vs the served file); a match means the stale parent_ver lives inside
some resource file itself.
"""
import os
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import RMSG_PAGINAE, WireClient, auth_cookie  # noqa: E402

RES = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "..", "..", "gameres"
)
SIG = b"Haven Resource 1"


def file_ver(name):
    path = os.path.join(RES, name + ".res")
    if not os.path.isfile(path):
        return None
    with open(path, "rb") as f:
        d = f.read(18)
    if len(d) < 18 or d[:16] != SIG:
        return None
    return struct.unpack("<H", d[16:18])[0]


class PaginaProbe(WireClient):
    def __init__(self, username):
        super().__init__(username)
        self.paginae = {}
        self.collect_secs = 12

    def on_event(self, t, body):
        if t == RMSG_PAGINAE:
            off = 0
            while off < len(body):
                act = body[off]
                off += 1
                nend = body.index(0, off)
                nm = body[off:nend].decode()
                (ver,) = struct.unpack("<H", body[nend + 1 : nend + 3])
                off = nend + 3
                if act == 0x2B:  # add
                    self.paginae[nm] = ver


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "pagenull"
    cookie = auth_cookie(username)
    probe = PaginaProbe(username)
    probe.connect()
    print("ACCEPTED, collecting paginae for", probe.collect_secs, "s")
    deadline = time.time() + 8
    while time.time() < deadline and probe.charlist_id is None:
        probe.pump(0.2)
    assert probe.charlist_id is not None, "no charlist widget within 8s"
    probe.play(username)
    end = time.time() + probe.collect_secs
    while time.time() < end:
        probe.pump(0.2)
    bad = []
    for nm, ver in sorted(probe.paginae.items()):
        real = file_ver(nm)
        if real is None:
            state = "MISSING-IN-GAMERES"
            bad.append((nm, ver, real))
        elif real != ver:
            state = f"MISMATCH (file={real})"
            bad.append((nm, ver, real))
        else:
            state = "ok"
        if state != "ok":
            print(f"  {nm}: announced={ver} {state}")
    print(f"announced paginae total={len(probe.paginae)} bad={len(bad)}")
    if not bad:
        print("PAGINAE ANNOUNCE: OK")


if __name__ == "__main__":
    main()
