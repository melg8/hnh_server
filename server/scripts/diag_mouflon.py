#!/usr/bin/env python3
"""Session 93 diagnostic: the sheep probe's fight-open clicks are
refused with the client believing the mouflon stands at dist=0 while
the server's TEMP attack diagnostic prints it hundreds of subtiles
away. Dump the client-side gob state for every kritter in the stream
(res, cached pos, live extrapolation, follow, LinMove stream), then
trigger the server's ground-truth diagnostic by clicking the nearest
mouflon (an attack the server will refuse - which is the point).

Usage: python3 diag_mouflon.py
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import REPO, enter_world, stop_server  # noqa: E402
from test_dough import request_neighborhood  # noqa: E402
from test_sheep import (  # noqa: E402
    MOUFLON_PREFIX,
    SERVER_LOG,
    ensure_sheep_server,
)


def dump(c, label):
    print("---- %s ----" % label)
    pg = c.player_gob
    print("player gob %d live=%s cached=%s" % (
        pg, c.live_pos(pg), c.gobs[pg]["pos"]))
    rows = 0
    for gid, g in sorted(c.gobs.items()):
        res = g["res"] or ""
        if "kritter" not in res:
            continue
        rows += 1
        short = "/".join(res.split("/")[2:4]) if res.count("/") >= 3 else res
        print("gob %d %-28s pos=%-14s live=%-14s follow=%-6s "
              "linbeg=%s step=%r moves=%d rm=%s" % (
                  gid, short, g.get("pos"), c.live_pos(gid),
                  g.get("follow"), g.get("linbeg"), g.get("linstep"),
                  len(g.get("moves", [])), g.get("removed")))
    if rows == 0:
        print("(no kritters in the stream yet)")


def watch(c, gid, seconds):
    """Watch one gob's client-side state over time: does the retract
    sweep ever deliver its OD_REM (removed flag), do its movements
    stream (linbeg/moves growing)?"""
    t0 = time.time()
    while time.time() - t0 < seconds:
        c.pump(2.0)
        g = c.gobs.get(gid)
        if g is None:
            print("t=%.0f gob %d GONE from the cache" % (
                time.time() - t0, gid))
            return
        print("t=%.0f gob %d pos=%s live=%s linbeg=%s moves=%d rm=%s" % (
            time.time() - t0, gid, g.get("pos"), c.live_pos(gid),
            g.get("linbeg"), len(g.get("moves", [])), g.get("removed")))


def main():
    save = os.path.join(REPO, "server", "target", "diag-mouflon-save.json")
    proc = ensure_sheep_server(save)
    try:
        c = enter_world("diag%d" % (int(time.time()) % 100000))
        request_neighborhood(c)
        c.pump(5.0)
        # The furthest streamed mouflon: the stale-position suspect.
        found = {
            g: info for g, info in c.gobs.items()
            if (info["res"] or "").startswith(MOUFLON_PREFIX)
            and not info.get("removed")
        }
        me = c.live_pos(c.player_gob) or (0, 0)
        gid = max(
            found,
            key=lambda g: abs(
                (c.live_pos(g) or found[g]["pos"])[0] - me[0])
            + abs((c.live_pos(g) or found[g]["pos"])[1] - me[1]))
        print("WATCHING furthest mouflon %d at %s (player %s)" % (
            gid, found[gid]["pos"], me))
        watch(c, gid, 30.0)
        print("chat tail: %r" % [t for t, _ in c.chat_lines[-4:]])
    finally:
        if proc is not None:
            stop_server(proc)


if __name__ == "__main__":
    sys.exit(main())
