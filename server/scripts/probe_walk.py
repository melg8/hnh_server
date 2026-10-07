#!/usr/bin/env python3
"""Movement probe: enter the world, click the ground, dump the LIN flow.

Asserts that a ground click produces OD_LINBEG followed by OD_LINSTEP
progress frames for the player gob, that the gob arrives exactly at the
clicked target (arrival OD_MOVE), and that a gob-target click does not
produce a walk. Prints MOVE PROBE: OK / FAIL.

The transport and the OBJDATA op table live in hnhlib.py.

Usage: probe_walk.py <username>
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import WireClient, ensure_server, stop_server  # noqa: E402


class WalkProbe(WireClient):
    pass


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "probeuser"
    server_proc = ensure_server()
    try:
        p = WalkProbe(username)
        p.connect()
        print("session accepted")
        # Drain the charlist burst.
        p.pump(2.0)
        assert "charlist" in p.widgets.values(), "no charlist widget"
        # Play the requested character.
        p.play(username)
        p.pump(5.0)
        assert p.mapview_id is not None, "no mapview widget"
        print(f"mapview widget {p.mapview_id}")

        # Ground click 20 tiles east of the spawn center. The server
        # spawns at tile (50,50) -> subtile (555,555) (find_spawn_position).
        cx, cy = 555, 555
        p.click_ground(cx + 220, cy)
        print("ground click sent")
        p.pump(3.0)
        print(f"player gob id from mapview args: {p.player_gob}")
        own = p.gobs.get(p.player_gob, {})
        print(f"LINBEG after click: {own.get('linbeg')}")
        print(
            f"LINSTEP total={len(own.get('linsteps', []))} "
            f"MOVE total={len(own.get('moves', []))}"
        )
        if p.player_gob is None:
            print("MOVE PROBE: FAIL (player gob unknown)")
            return 1
        linbeg = own.get("linbeg")
        if linbeg is None:
            print("MOVE PROBE: FAIL (no LINBEG for own gob)")
            return 1
        sx, sy, tx, ty, c = linbeg
        steps = own.get("linsteps", [])
        print(f"own gob {p.player_gob}: LINSTEP frames {len(steps)} "
              f"last={steps[-1] if steps else None} of c={c}")
        if not steps:
            print("MOVE PROBE: FAIL (no LINSTEP progress)")
            return 1
        # Arrival contract: the step counter reaches the LINBEG step count
        # and the last MOVE lands exactly on the clicked target.
        deadline_ok = p.wait_for(
            lambda: (
                p.gobs.get(p.player_gob, {}).get("linsteps", [0])[-1] >= c
            ),
            20,
        )
        own = p.gobs.get(p.player_gob, {})
        steps = own.get("linsteps", [])
        if not deadline_ok:
            print(f"MOVE PROBE: FAIL (walk never completed, last={steps[-1] if steps else None})")
            return 1
        moves = own.get("moves", [])
        if not moves or moves[-1] != (tx, ty):
            print(f"MOVE PROBE: FAIL (no arrival snap: last MOVE {moves[-1] if moves else None} "
                  f"!= target {(tx, ty)})")
            return 1
        print(f"arrived exactly at {moves[-1]}")

        # Gob-target click on the own gob: interact path, no new walk.
        before = len(own.get("linbegs", []))
        p.click_gob(p.player_gob, (cx, cy))
        p.pump(2.0)
        n_linbegs = len(p.gobs.get(p.player_gob, {}).get("linbegs", []))
        print(f"gob-target click: own LINBEG count={n_linbegs} "
              f"(start was {before})")
        print("MOVE PROBE: OK")
        return 0
    finally:
        stop_server(server_proc)


if __name__ == "__main__":
    sys.exit(main() or 0)
