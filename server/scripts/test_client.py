#!/usr/bin/env python3
"""Minimal manual session test: handshake, bootstrap verdicts.

Usage: test_client.py <username> [game_port] [auth_port]

The transport lives in hnhlib.py (single source shared by every probe);
this script only declares the bootstrap contract it asserts:
  - ACCEPTED on the MSG_SESS handshake,
  - the full bootstrap: charlist -> play -> mapview -> 3x3 MAPREQ ->
    MAPDATA + OBJDATA,
  - CATTR ORDER: the complete REQUIRED_CATTR set must be present BEFORE
    the `chr` widget is created (CharWnd constructor NPEs otherwise),
  - one walk click after entry.
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import (  # noqa: E402
    RMSG_GLOBLOB,
    RMSG_RESID,
    RMSG_TILES,
    REQUIRED_CATTR,
    WireClient,
    auth_cookie,
)


class BootstrapProbe(WireClient):
    """WireClient + the bootstrap message counters this verdict needs."""

    def __init__(self, username):
        super().__init__(username)
        self.stats = {
            "mapdata": 0, "objdata": 0, "resid": 0, "tiles": 0,
            "globlob": 0, "newwdg": 0, "wdgmsg": 0, "cattr": 0,
        }

    def on_datagram(self, data):
        if data[0] == 5:
            self.stats["mapdata"] += 1
        elif data[0] == 6:
            self.stats["objdata"] += 1
        super().on_datagram(data)

    def on_event(self, t, body):
        if t == 0:
            self.stats["newwdg"] += 1
        elif t == 1:
            self.stats["wdgmsg"] += 1
        elif t == RMSG_GLOBLOB:
            self.stats["globlob"] += 1
        elif t == RMSG_RESID:
            self.stats["resid"] += 1
        elif t == RMSG_TILES:
            self.stats["tiles"] += 1
        elif t == 9:  # RMSG_CATTR
            self.stats["cattr"] += 1


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "testuser"
    game_port = int(sys.argv[2]) if len(sys.argv) > 2 else 1870
    auth_port = int(sys.argv[3]) if len(sys.argv) > 3 else 1871

    cookie = auth_cookie(username, port=auth_port)
    print(f"cookie ok ({len(cookie)} bytes)")
    print(f"game port: {game_port}")

    c = BootstrapProbe(username)
    if game_port != c.server[1]:
        c.server = ("127.0.0.1", game_port)
    c.connect()
    print("ACCEPTED")

    # Bootstrap: play as soon as the charlist binds; mapview triggers the
    # 3x3 MAPREQ, slen triggers the chr request (both in WireClient).
    deadline = time.time() + 8
    while time.time() < deadline and c.charlist_id is None:
        c.pump(0.2)
    assert c.charlist_id is not None, "no charlist widget within 8s"
    c.play(username)
    c.pump(6.0)

    print("widgets:", {hex(k): v for k, v in sorted(c.widgets.items())})
    print("stats:", c.stats)
    ok = c.mapview_id is not None and c.stats["mapdata"] > 0 and c.stats["objdata"] > 0
    print("WORLD ENTRY:", "OK" if ok else "INCOMPLETE")
    if c.cattr_at_chr is not None:
        missing = REQUIRED_CATTR - c.cattr_at_chr
        if missing:
            print("CATTR ORDER: FAIL (missing at chr creation:", sorted(missing), ")")
        else:
            print("CATTR ORDER: OK (%d names present before chr widget)" % len(REQUIRED_CATTR))
    else:
        print("CATTR ORDER: chr widget never created")
    if c.mapview_id is not None and c.stats["mapdata"] > 0:
        # Walk test: click 20 tiles east (probe coordinates).
        c.click_ground(555 * 11 + 220, 555 * 11)
        print("walk click sent")
    time.sleep(1)


if __name__ == "__main__":
    main()
