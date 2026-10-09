#!/usr/bin/env python3
"""Session 70 kiln-nav diagnostic 2: dump the tile composition along the
spawn -> shore line and the walkable frontier, straight from the streamed
grids (no walking)."""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import ensure_server, enter_world  # noqa: E402

TILE_SPAN = 11


def main():
    server_proc = ensure_server()
    try:
        c = enter_world("kilnnav2")
        print("grids loaded:", sorted(c.tiles.keys())[:12], "...")
        print("spawn tile:", (555 // TILE_SPAN, 555 // TILE_SPAN))
        print("shore tile:", (101, 0), "tile value:", c.tile_at(101 * 11 + 5, 0 * 11 + 5))
        # Column scan x=55..101 at y=0..50: find where walkability breaks.
        import collections
        start = (50, 50)
        goal = (101, 0)
        # BFS purely on the probe's tiles to find the frontier.
        prev = {start: None}
        q = collections.deque([start])
        while q:
            cur = q.popleft()
            if cur == goal:
                break
            for d in ((1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)):
                nb = (cur[0] + d[0], cur[1] + d[1])
                if nb in prev or not c.walkable(nb[0] * TILE_SPAN + 5, nb[1] * TILE_SPAN + 5):
                    continue
                prev[nb] = cur
                q.append(nb)
        print("BFS reached:", sorted(prev.keys())[-1] if prev else None)
        print("goal in prev:", goal in prev)
        # Distance reached toward the goal: max x+y progression.
        if goal not in prev:
            best = max(prev.keys(), key=lambda t: t[0] - t[1])
            print("farthest tile toward goal:", best,
                  "tile value:", c.tile_at(best[0] * TILE_SPAN + 5, best[1] * TILE_SPAN + 5))
            # Sample the row between best and the goal along the line.
            t = best
            samples = []
            for step in range(1, 8):
                sx = best[0] + (goal[0] - best[0]) * step // 7
                sy = best[1] + (goal[1] - best[1]) * step // 7
                samples.append(((sx, sy), c.tile_at(sx * TILE_SPAN + 5, sy * TILE_SPAN + 5)))
            print("line samples:", samples)
        # Tile histogram of the loaded area.
        hist = collections.Counter()
        for (gx, gy), tiles in c.tiles.items():
            for i, tv in enumerate(tiles):
                if tv:
                    hist[tv] += 1
        print("tile histogram (loaded):", dict(hist))
        c.sock.close()
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


if __name__ == "__main__":
    main()
