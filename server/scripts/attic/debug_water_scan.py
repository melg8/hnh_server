#!/usr/bin/env python3
"""Find the nearest water tile to the spawn point on the live server.

Session 71 planning probe: the bucket-fill mechanic (itemact an empty
bucket onto a water tile) needs a reachable water tile for the bake
probe to fill at. Walks the streamed map data around the spawn and
scans tile ids 0 (deep water) / 1 (water).
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import TILE_SPAN, WireClient, auth_cookie  # noqa: E402


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "waternull"
    cookie = auth_cookie(username)
    c = WireClient(username)
    c.connect()
    deadline = time.time() + 8
    while time.time() < deadline and c.charlist_id is None:
        c.pump(0.2)
    assert c.charlist_id is not None, "no charlist widget"
    c.play(username)
    c.pump(6.0)
    pos = c.gobs[c.player_gob]["pos"]
    px, py = pos[0] // TILE_SPAN, pos[1] // TILE_SPAN
    print(f"player tile: ({px},{py})")
    # Scan an expanding square ring for water tiles (ids 0/1).
    found = None
    for r in range(0, 60, 2):
        for dy in range(-r, r + 1, 2):
            for dx in range(-r, r + 1, 2):
                if max(abs(dx), abs(dy)) != r:
                    continue
                t = c.tile_at(px + dx, py + dy)
                if t in (0, 1):
                    found = (px + dx, py + dy, t, r)
                    break
            if found:
                break
        if found:
            break
    if found:
        print(f"WATER: tile ({found[0]},{found[1]}) id={found[2]} ring={found[3]}")
    else:
        print("WATER: none within ring 60")


if __name__ == "__main__":
    main()
