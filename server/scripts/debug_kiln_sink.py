#!/usr/bin/env python3
"""Session 69 debug: why does the second sink_demand round lose the clay?

Replays the gather (fast: 6 picks is enough - 8 clay per pile covers the
46 quota quickly), builds the kiln plan, then prints the full item_info +
sdt state after EVERY step of the first sink round.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import ensure_server, enter_world  # noqa: E402
from test_kiln import (  # noqa: E402
    CLAY_INV, CLAY_NEED, KILN_RES, find_clay_deposits, gather_clay,
    inv_total, walk_to_shore,
)


def dump(c, tag):
    items = sorted(
        (wid, info["res"], info.get("count", 1))
        for wid, info in c.item_info.items()
    )
    plan_sdt = None
    for g, info in c.gobs.items():
        if info["res"] == KILN_RES:
            plan_sdt = (g, info.get("sdt"), info.get("removed"))
    print("[%s] items=%s plan=%s" % (tag, items, plan_sdt))


def main():
    server_proc = ensure_server()
    try:
        c = enter_world("kilndbg")
        walk_to_shore(c)
        gather_clay(c)
        dump(c, "after gather")

        # Place a kiln plan (the test_kiln.build_kiln body, instrumented).
        ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
        ptile = (ppos[0] // 11, ppos[1] // 11)
        plan = mc = None
        for dx in range(-3, 4):
            cand = (ptile[0] + dx, ptile[1] + 1)
            cmc = (cand[0] * 11 + 5, cand[1] * 11 + 5)
            c.menu_act("kiln")
            ok = c.wait_for(lambda: c.place_seen is not None, 5)
            assert ok, "place uimsg missing"
            c.place_seen = None
            c.send_place(cmc, 1, 0)
            if c.wait_for(
                lambda: any(
                    info["res"] == KILN_RES and info["pos"] == cmc
                    for info in c.gobs.values()
                ),
                2.5,
            ):
                mc = cmc
                plan = next(
                    g for g, info in c.gobs.items()
                    if info["res"] == KILN_RES and info["pos"] == cmc
                )
                break
        assert plan is not None
        dump(c, "after plan")

        # ONE sink round, fully instrumented.
        stack = c.find_item_by_res(CLAY_INV)
        print("stack wid:", stack)
        assert stack is not None
        c.take_item(stack)
        c.pump(0.5)
        dump(c, "after take")
        c.map_itemact(mc, plan)
        c.pump(0.8)
        dump(c, "after itemact")
        ok = c.return_cursor()
        print("return_cursor:", ok)
        c.pump(0.8)
        dump(c, "after return")
        print("clay total:", inv_total(c, CLAY_INV), "need", CLAY_NEED)
        c.sock.close()
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


if __name__ == "__main__":
    main()
