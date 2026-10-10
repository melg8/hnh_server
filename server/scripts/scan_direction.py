#!/usr/bin/env python3
"""Session 93 recon 2: trek in a compass direction past the grid edge,
counting mouflons in the fresh grid streams (post-fix, animals only
stream inside the 600-axial window - walking is the only way to meet
them). Usage: python3 scan_direction.py <dx,dy> [hops]"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import (  # noqa: E402
    REPO,
    TILE_SPAN,
    enter_world,
    ensure_server,
    stop_server,
)
from test_dough import request_neighborhood  # noqa: E402
from test_sheep import MOUFLON_PREFIX  # noqa: E402

DIRS = {
    "E": (1, 0), "NE": (1, -1), "N": (0, -1), "NW": (-1, -1),
    "W": (-1, 0), "SW": (-1, 1), "S": (0, 1), "SE": (1, 1),
}


def count_kritters(c):
    by = {}
    for gid, g in c.gobs.items():
        res = g["res"] or ""
        if "kritter" not in res or g.get("removed"):
            continue
        short = "/".join(res.split("/")[2:4]) if res.count("/") >= 3 else res
        by.setdefault(short, set()).add(gid)
    return {k: len(v) for k, v in by.items()}


def main():
    want = sys.argv[1] if len(sys.argv) > 1 else "E"
    hops = int(sys.argv[2]) if len(sys.argv) > 2 else 5
    dx, dy = DIRS.get(want, (1, 0))
    save = os.path.join(
        REPO, "server", "target", "scan-dir-%s-save.json" % want)
    proc = ensure_server(save_path=save, fresh=True)
    try:
        c = enter_world("scan%s%d" % (want, int(time.time()) % 100000))
        request_neighborhood(c)
        c.pump(3.0)
        print("t=0  player=%s kritters=%r" % (
            c.live_pos(c.player_gob), count_kritters(c)))
        for h in range(hops):
            pos = c.live_pos(c.player_gob) or c.gobs[c.player_gob]["pos"]
            goal = (pos[0] + dx * 70 * TILE_SPAN,
                    pos[1] + dy * 70 * TILE_SPAN)
            ok = c.nav_walk(goal, stop=2 * TILE_SPAN)
            c.pump(4.0)
            mfl = [
                (g, i) for g, i in c.gobs.items()
                if (i["res"] or "").startswith(MOUFLON_PREFIX)
                and not i.get("removed")]
            print("hop%d ok=%s player=%s kritters=%r mouflons=%r" % (
                h, ok, c.live_pos(c.player_gob), count_kritters(c),
                [(g, c.live_pos(g) or i["pos"]) for g, i in mfl]))
            if len(mfl) >= 2:
                print("DIRECTION %s HAS THE HERD" % want)
                break
    finally:
        if proc is not None:
            stop_server(proc)


if __name__ == "__main__":
    sys.exit(main())
