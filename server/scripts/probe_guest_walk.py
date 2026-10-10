#!/usr/bin/env python3
"""Cluster guest-walk probe (session 54; ports refreshed session 86).

Expects a 2-node cluster already listening (node 0: auth 1871 / game
1870 UDP; node 1: auth 1873 / game 1874 - the cluster-up.sh /
windows start-cluster.bat profile; probe_guest_walk only ever talks
to node 0). Drives one wire client through node 0:
session -> charlist -> play -> bootstrap, then walks EAST in validated
220-subtile legs (modeled on probe_walk: each leg waits for the LINSTEP
counter to reach the LINBEG step count). Four legs cross 3+ VisIndex
cells; cells owned by the peer node exercise the guest path (subscribe
-> publish -> ingest -> scan spawn).

Verdict lines:
  WORLD ENTRY: OK   - bootstrap through node 0
  GUEST WALK: OK    - all legs arrived (LINSTEP counters completed)
"""

import sys

sys.path.insert(0, "/home/z/my-project/hnh_server/server/scripts")

from hnhlib import WireClient  # noqa: E402


def walk_leg(c: WireClient, tx: int, ty: int) -> bool:
    """One 220-subtile leg; returns True when the client saw arrival."""
    c.click_ground(tx, ty)
    if not c.wait_for(lambda: c.gobs.get(c.player_gob, {}).get("linbeg") is not None, 8):
        return False
    linbeg = c.gobs[c.player_gob]["linbeg"]
    steps_target = linbeg[4]
    return c.wait_for(
        lambda: c.gobs.get(c.player_gob, {}).get("linsteps", [0])[-1] >= steps_target,
        30,
    )


def main() -> int:
    user = sys.argv[1] if len(sys.argv) > 1 else "guestwalk"
    c = WireClient(user)
    c.connect()
    c.pump(2.0)
    if "charlist" not in c.widgets.values():
        print("WORLD ENTRY: FAIL (no charlist)")
        return 1
    c.play(user)
    c.pump(5.0)
    if c.mapview_id is None:
        print("WORLD ENTRY: FAIL (no mapview)")
        return 1
    print("WORLD ENTRY: OK")

    # The server spawns at tile (50,50) -> subtile (555,555).
    x, y = 555, 555
    ok = True
    for leg in range(1, 5):
        x += 220
        arrived = walk_leg(c, x, y)
        print(f"leg {leg}: target=({x},{y}) arrived={arrived}")
        ok = ok and arrived
    print(f"GUEST WALK: {'OK' if ok else 'FAIL'} (final target {x},{y})")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
