#!/usr/bin/env python3
"""Direction probe: verify the wire carries direction-correct pose layers.

Enters the world, clicks ground targets in three different directions
(east, south, north-west) and, for each leg, captures the OD_LAYERS block
streamed for the own gob right after OD_LINBEG (walking set) and on
arrival (standing set). Both sets must name directional resources whose
direction digit matches the ART octant of the leg, and they must stay on
that one direction for the whole leg (no cycling).

Octant contract (session 22): the art ring is offset one step
counterclockwise from the movement ring (server art_dir = (octant + 7)
& 7, unit-pinned in art_dir_offsets_the_sprite_ring), so the expected
digit is the quantized movement octant shifted by -1.

Prints DIRECTION WIRE: OK / FAIL.

Usage: probe_direction.py <username>

The transport and the OBJDATA op table live in hnhlib.py.
"""
import math
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import WireClient, parse_objdata  # noqa: E402


def move_dir(s, t):
    """Mirror of the server's move_dir: quantized movement octant."""
    dx, dy = t[0] - s[0], t[1] - s[1]
    if dx == 0 and dy == 0:
        return 0
    deg = math.degrees(math.atan2(dy, dx))
    return int(math.floor((deg + 22.5) / 45.0)) % 8


class DirectionProbe(WireClient):
    """Adds the event-log tracking the verdicts need.

    The verdicts must see EVERY LAYERS/AVATAR event in arrival order
    (per leg: one walking set, then the standing set) and the LINBEG
    stream of the own gob. WireClient keeps the latest state per gob;
    this probe additionally logs the raw event sequence. The extra pass
    runs over the same parsed ops the base class already consumes.
    """

    def __init__(self, username):
        super().__init__(username, send_objacks=True)
        self.linbegs = []        # (gid, (sx, sy, tx, ty, c))
        self.layer_events = []   # (gid, base name, [names])
        self.avatar_events = []  # (gid, [names])

    def on_objdata(self, body):
        for gobid, _frame, ops in parse_objdata(body):
            for op, arg in ops:
                if op == "LINBEG":
                    self.linbegs.append((gobid, arg))
                elif op == "LAYERS":
                    base, ids = arg
                    bn = self.resids.get(base, "?%s" % base) if base else None
                    names = [self.resids.get(i, "?%d" % i) for i in ids]
                    self.layer_events.append((gobid, bn, names))
                elif op == "AVATAR":
                    ids = arg
                    self.avatar_events.append(
                        (gobid, [self.resids.get(i, "?%d" % i) for i in ids]))
        super().on_objdata(body)


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "dirprobe"
    p = DirectionProbe(username)
    p.connect()
    p.pump(2.0)
    assert p.charlist_id is not None, "no charlist widget"
    p.play(username)
    p.pump(5.0)
    assert p.mapview_id is not None and p.player_gob, "no mapview / player gob"
    print(f"player gob {p.player_gob}")

    # The own gob's Avatar attribute (OD_AVATAR) must carry the banzai
    # doll set from the spawn block: Equipory.cdraw renders exactly this
    # (the missing Equipment paperdoll defect, wire side).
    doll = [names for g, names in p.avatar_events if g == p.player_gob and
            any("arm/banzai/" in n for n in names)]
    print(f"banzai doll sets on spawn: {len(doll)}")
    if not doll:
        print("DIRECTION WIRE: FAIL (no OD_AVATAR doll set)")
        return 1

    # Legs: east, south, north-west - three distinct octants (0, 2, 5).
    start = (555, 555)
    legs = [((start[0] + 220, start[1]), "+x"),
            ((start[0], start[1] + 220), "+y"),
            ((start[0] - 150, start[1] - 150), "-x-y")]
    fails = []
    for target, label in legs:
        # Current interpolated start = wherever the last leg ended. Wait
        # dynamically for arrival: legs can be ~450 subtile Manhattan at
        # 33 subtile/s (walk gait) ~= 14 s.
        p.linbegs.clear()
        p.layer_events.clear()
        p.click_ground(*target)
        p.pump(0.7)
        lin = [(g, a) for g, a in p.linbegs if g == p.player_gob]
        if not lin:
            fails.append(f"{label}: no LINBEG")
            continue
        _, (sx, sy, tx, ty, _c) = lin[0]
        octant = move_dir((sx, sy), (tx, ty))
        # Session-22 art ring: emitted digits are (movement octant + 7) & 7.
        want = (octant + 7) & 7
        deadline = time.time() + 30.0
        while time.time() < deadline:
            p.pump(0.5)
            stand = [names for g, _b, names in p.layer_events
                     if g == p.player_gob and any("standing/legs-" in n for n in names)]
            if stand:
                break
        walk = [names for g, _b, names in p.layer_events
                if g == p.player_gob and any("walking/legs-" in n for n in names)]
        stand = [names for g, _b, names in p.layer_events
                 if g == p.player_gob and any("standing/legs-" in n for n in names)]
        if not walk:
            fails.append(f"{label}: no walking LAYERS for own gob")
            continue
        # Every walking set must use exactly the expected direction digit,
        # and only ONE walking set may appear (no per-frame streaming).
        wdirs = {n.rsplit("-", 1)[-1] for names in walk for n in names
                 if "walking/legs-" in n}
        if wdirs != {str(want)}:
            fails.append(f"{label}: walking dirs {wdirs}, want {{{want}}}")
        if len(walk) > 1:
            fails.append(f"{label}: {len(walk)} walking layer streams (cycling?)")
        if not stand:
            fails.append(f"{label}: no standing LAYERS on arrival")
            continue
        sdirs = {n.rsplit("-", 1)[-1] for names in stand for n in names
                 if "standing/legs-" in n}
        if sdirs != {str(want)}:
            fails.append(f"{label}: standing dirs {sdirs}, want {{{want}}}")
        print(f"leg {label}: lin ({sx},{sy})->({tx},{ty}) octant={octant} "
              f"art={want} walk_streams={len(walk)} stand_streams={len(stand)}")

    # A re-click retarget keeps the doll off the walking path: the doll
    # rides only the spawn block, so no further banzai events are needed.
    if fails:
        print("DIRECTION WIRE: FAIL")
        for f in fails:
            print("  " + f)
        return 1
    print("DIRECTION WIRE: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
