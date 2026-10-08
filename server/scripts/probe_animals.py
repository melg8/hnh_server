#!/usr/bin/env python3
"""Animal probe: kritter pose layering + walk streams + bite FX overlay.

Enters the world of a --saturated server and asserts, purely from the
wire stream:
1. Animal gobs spawn through OD_LAYERS of concrete kritter pose parts
   (base gfx/kritter/<sp>/body + one standing-N/walking-N part) and NOT
   through a flat OD_RES sprite (the old path rendered shadow-only).
2. At least one animal streams the walking pose (movement animation on
   the wire; the client animates the 8-frame cycle itself).
3. A predator engagement produces the one-shot bite overlay
   (gfx/fx/bite) on the victim gob (attack animation).

Prints ANIMALS WIRE: OK / FAIL.

Usage: probe_animals.py <username>

The transport and the OBJDATA op table live in hnhlib.py.
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import WireClient, parse_objdata  # noqa: E402


class AnimalProbe(WireClient):
    """Adds the pose-level tracking the verdicts need.

    WireClient keeps the LAST layers base per gob; this probe must keep
    the FIRST spawn block per gob (the standing pose set) and every
    later LAYERS event (the walking streams) plus the OVERLAY resources
    (the bite FX). The extra pass runs over the same parsed ops the
    base class already consumes - parse_objdata is a pure function.
    """

    def __init__(self, username):
        super().__init__(username, send_objacks=True)
        self.spawn_ops = {}      # gob id -> (base name, [layer names]) first block
        self.gob_res = {}        # gob id -> res name from the first plain RES op
        self.kritter_walks = []  # (gid, layer name) walking streams
        self.bites = []          # (gid, res name) overlays

    def on_objdata(self, body):
        for gobid, _frame, ops in self._ops(body):
            for op, arg in ops:
                if op == "RES" and gobid not in self.gob_res:
                    wire, _sdt = arg
                    self.gob_res[gobid] = self.resids.get(wire, "?%d" % wire)
                elif op in ("LAYERS", "AVATAR") and gobid not in self.spawn_ops:
                    if op == "LAYERS":
                        base, ids = arg
                        bn = self.resids.get(base, "?%s" % base) if base else None
                    else:
                        _bn, ids = arg
                        bn = None
                    names = [self.resids.get(i, "?%d" % i) for i in ids]
                    self.spawn_ops[gobid] = (bn, names)
                elif op == "LAYERS":
                    base, ids = arg
                    for i in ids:
                        n = self.resids.get(i, "?%d" % i)
                        if "/body/walking/walking-" in n:
                            self.kritter_walks.append((gobid, n))
                elif op == "OVERLAY":
                    _olid, resid = arg
                    self.bites.append((gobid, self.resids.get(resid, "?%d" % resid)))
        super().on_objdata(body)

    @staticmethod
    def _ops(body):
        return parse_objdata(body)


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "aniprobe"
    p = AnimalProbe(username)
    p.connect()
    p.pump(2.0)
    assert p.charlist_id is not None, "no charlist widget"
    p.play(username)
    p.pump(5.0)
    assert p.mapview_id is not None and p.player_gob, "no mapview / player gob"
    print(f"player gob {p.player_gob}; waiting for animals (saturated world)")

    def critters():
        return {g: v for g, v in p.spawn_ops.items()
                if v[0] and "kritter" in (v[0] or "") and (v[0] or "").endswith("/body")}

    # Phase 1: watch 20 s, then actively hunt a predator: click it (the
    # Kind::Animal click handler opens the fight) and let the combat chase
    # close the distance - the animal fights back and bites.
    p.pump(20.0)
    predators = {g: p.gobs[g]["pos"] for g in list(critters())
                 if g in p.gobs and p.gobs[g]["pos"]
                 and ("/wolf/" in (p.spawn_ops[g][0] or "")
                      or "/boar/" in (p.spawn_ops[g][0] or ""))}
    clicked = False
    if predators:
        gid = min(predators, key=lambda g: sum(predators[g]))
        x, y = predators[gid]
        p.click_gob(gid, (x, y))
        print(f"predator click: gob {gid} at ({x},{y})")
        clicked = True
    else:
        print("no predator with a known position after 20 s; waiting passively")

    deadline = time.time() + (180.0 if clicked else 75.0)
    while time.time() < deadline:
        p.pump(1.0)
        if p.kritter_walks and p.bites and len(critters()) >= 2:
            break
    animals = critters()
    print(f"animal spawns (kritter OD_LAYERS): {len(animals)}")
    for g, (base, names) in list(animals.items())[:4]:
        print(f"  gob {g}: base={base} layers={names}")
    flat_res = [g for g, r in p.gob_res.items() if r and "kritter" in r and "/cdv" in r]
    print(f"flat cdv RES animals (must be 0): {len(flat_res)}")
    print(f"walking-pose streams: {len(p.kritter_walks)}")
    print(f"bite overlays: {[(g, r) for g, r in p.bites[:4]]}")

    fails = []
    if len(animals) < 2:
        fails.append(f"only {len(animals)} kritter-layered animals (need >= 2)")
    if flat_res:
        fails.append(f"{len(flat_res)} animals still spawn via flat cdv RES")
    if not p.kritter_walks:
        fails.append("no walking-pose stream for any animal")
    if not p.bites:
        fails.append("no bite overlay (no predator engagement in 75 s)")
    if fails:
        print("ANIMALS WIRE: FAIL")
        for f in fails:
            print("  " + f)
        return 1
    print("ANIMALS WIRE: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
