#!/usr/bin/env python3
"""Session 70 kiln-nav diagnostic: why does nav_walk fail to reach the
shore tile on seed 42? Walk with logging, dump the tile belt around the
farthest reached point."""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import ensure_server, enter_world  # noqa: E402
from test_kiln import CLAY_NEED, SHORE_TILE, gather_clay, inv_total, walk_to_shore  # noqa: E402


def main():
    server_proc = ensure_server()
    try:
        c = enter_world("kilnnav")
        print("in world; walking to the shore with a verbose log")
        pos = c.gobs[c.player_gob]["pos"]
        print("spawn pos:", pos)
        target = (SHORE_TILE[0] * 11 + 5, SHORE_TILE[1] * 11 + 5)

        def log(msg):
            print(msg, flush=True)

        ok = c.nav_walk(target, stop=80, max_clicks=220, log=log)
        print("nav ok:", ok, "final pos:", c.gobs[c.player_gob]["pos"])
        if ok:
            walk_to_shore(c)
            print("walking gather...")
            gather_clay(c)
            print("clay total:", inv_total(c, CLAY_INV), "need", CLAY_NEED)
        c.sock.close()
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


if __name__ == "__main__":
    main()
