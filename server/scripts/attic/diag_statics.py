#!/usr/bin/env python3
"""Diagnostic: what statics actually spawn around the spawn area.

Connects, requests the 5x5 grid neighborhood, walks nowhere, prints:
  - the tile-id histogram of loaded grids
  - every terobjs gob resource with a count, by grid
"""
import os
import sys
import time
from collections import Counter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import REPO, WireClient, ensure_server  # noqa: E402


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "diag%d" % (
        int(time.time()) % 100000,)
    proc = ensure_server()
    try:
        c = WireClient(username)
        c.connect()
        c.pump(1.5)
        c.play(username)
        ok = c.wait_for(lambda: c.player_gob is not None, 12)
        assert ok, "no player gob"
        # Ask the 5x5 neighborhood.
        for gy in (-2, -1, 0, 1, 2):
            for gx in (-2, -1, 0, 1, 2):
                c.mapreq(gx, gy)
        c.wait_for(lambda: False, 6)
        tiles = Counter()
        for tiles_ in c.tiles.values():
            for t in tiles_:
                tiles[t] += 1
        print("tile histogram:", dict(sorted(tiles.items())))
        res = Counter()
        for g, info in c.gobs.items():
            if info.get("removed") or info["res"] is None:
                continue
            if info["res"].startswith("gfx/terobjs"):
                res[info["res"]] += 1
        print("terobjs gobs seen from spawn (%d total):" % len(c.gobs))
        for r, n in sorted(res.items()):
            print("  %4d  %s" % (n, r))
        px, py = c.gobs[c.player_gob]["pos"]
        print("player at", (px, py))
    finally:
        if proc is not None:
            from hnhlib import stop_server
            stop_server(proc)


if __name__ == "__main__":
    main()
