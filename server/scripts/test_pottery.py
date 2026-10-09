#!/usr/bin/env python3
"""Session 79 pottery probe on the hnhlib harness.

Drives the full clay -> mold -> kiln -> fired-ware contract against a
live server (the session-79 pottery legs):

  1. CLAY: walk to the dev-seed shore (the test_kiln shore pattern),
     pick the sand-tile heap deposits until CLAY_NEED = 50 units
     (45 kiln build + 3 jar molding + 2 mug molding).
  2. BUILD: place the kiln and sink the Clay x45 demand.
  3. MOLD: hand-craft the Unburnt Jar (clay x3, jardough) and the
     Unburnt Clay Mug (clay x2, mugdough) through the make widget -
     the crafted stacks must carry the unburnt labels the kiln
     dispatch keys on.
  4. FIRE: fuel a branch, load the Unburnt Jar, Light, wait out the
     30-tick job, catch the Clay Jar drop (gfx/terobjs/items/jar),
     pick it up and verify the tooltip + inventory resource. Repeat
     for the mug (gfx/terobjs/items/mug -> "Clay Mug").

Prints "POTTERY: OK ..." on success.

Usage: python3 test_pottery.py [username]
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import (  # noqa: E402
    LIST_INT,
    LIST_END,
    REPO,
    ensure_server,
    enter_world,
    le32,
)

DEPOSIT_RES = "gfx/terobjs/mining/heap"
CLAY_WORLD = "gfx/terobjs/items/clay"
CLAY_INV = "gfx/invobjs/clay"
KILN_RES = "gfx/terobjs/kiln"
SAND_TILE = 20
# 45 build + 3 jar mold + 2 mug mold (the RoB Legacy clay ladder).
CLAY_NEED = 50
SHORE_TILE = (101, 0)
BRANCH_INV = "gfx/invobjs/branch"

MUG_WORLD = "gfx/terobjs/items/mug"
MUG_INV = "gfx/invobjs/mug"
MUG_LABEL = "Clay Mug"
JAR_WORLD = "gfx/terobjs/items/jar"
JAR_INV = "gfx/invobjs/jar"
JAR_LABEL = "Clay Jar"


def find_deposits(c):
    found = []
    for g, info in c.gobs.items():
        if info.get("removed") or info["res"] != DEPOSIT_RES:
            continue
        pos = info["pos"]
        if c.tile_at(pos[0], pos[1]) == SAND_TILE:
            found.append((g, info))
    return found


def find_drops(c, resname):
    return [
        (g, info)
        for g, info in c.gobs.items()
        if info["res"] == resname and not info.get("removed")
    ]


def inv_total(c, resname):
    return sum(
        info.get("count", 1)
        for info in c.item_info.values()
        if info["res"] == resname
    )


def pick_drop_into_inv(c, gid, inv_res):
    before = inv_total(c, inv_res)
    c.click_gob(gid, c.gobs[gid]["pos"])
    ok = c.wait_for(lambda: inv_total(c, inv_res) > before, 8)
    assert ok, "%s never landed in the inventory" % inv_res


def walk_to_shore(c):
    target = (SHORE_TILE[0] * 11 + 5, SHORE_TILE[1] * 11 + 5)
    shore_gc = (SHORE_TILE[0] // 100, SHORE_TILE[1] // 100)
    # The one-shot 3x3 MAPREQ at mapview-bind can lose a fragment under
    # the entry burst (kernel receive overflow on loopback); the real
    # client re-issues MAPREQ for every grid it lacks, so mirror that
    # here for the WHOLE neighborhood - the BFS needs the neighbor
    # grids the shore path detours through, not just the shore one.
    deadline = time.time() + 15
    while True:
        missing = [(gx, gy) for gy in (-1, 0, 1) for gx in (-1, 0, 1)
                   if c.tiles.get((gx, gy)) is None]
        if not missing:
            break
        assert time.time() < deadline, (
            "grids never streamed in: %s" % (missing,))
        for gc in missing:
            c.mapreq(*gc)
        c.wait_for(lambda: False, 1.5)
    ok = c.nav_walk(target, stop=80, max_clicks=220,
                    log=lambda m: print(m))
    if not ok:
        print("NAV DIAG: grids=%s player=%s"
              % (sorted(c.tiles.keys()),
                 c.gobs[c.player_gob]["pos"]))
        raise AssertionError("could not reach the shore at %s" % (target,))
    print("at the shore: %s" % (c.gobs[c.player_gob]["pos"],))


def gather_clay(c):
    used = set()
    while inv_total(c, CLAY_INV) < CLAY_NEED:
        deposits = [d for d in find_deposits(c) if d[0] not in used]
        if not deposits:
            pos = c.gobs[c.player_gob]["pos"]
            assert c.nav_walk((pos[0] + 150, pos[1]), stop=60, max_clicks=40), (
                "shore ran out of reachable deposits")
            continue
        deposits.sort(key=lambda d: abs(d[1]["pos"][0] - c.gobs[c.player_gob]["pos"][0])
                      + abs(d[1]["pos"][1] - c.gobs[c.player_gob]["pos"][1]))
        dep_id, dep_info = deposits[0]
        assert c.nav_walk(dep_info["pos"], stop=60, max_clicks=80), (
            "could not reach a clay deposit")
        used.add(dep_id)
        while (not c.gobs[dep_id].get("removed")
               and inv_total(c, CLAY_INV) < CLAY_NEED):
            seen = set(c.gobs.keys())
            c.click_gob(dep_id, c.gobs[dep_id]["pos"])

            def fresh_clay():
                return [
                    (g, info)
                    for g, info in c.gobs.items()
                    if info["res"] == CLAY_WORLD and not info.get("removed")
                    and g not in seen
                ]

            ok = c.wait_for(lambda: bool(fresh_clay()), 8)
            assert ok, "no clay drop spawned after the deposit pick"
            gid, _ = sorted(fresh_clay())[0]
            pick_drop_into_inv(c, gid, CLAY_INV)
        print("CLAY PICK: %d/%d" % (inv_total(c, CLAY_INV), CLAY_NEED))
    assert inv_total(c, CLAY_INV) >= CLAY_NEED, "clay quota unmet"


def sink_demand(c, plan, mc, resname, units):
    c.chat_lines.clear()
    for _ in range(units):
        if c.gobs.get(plan) is None:
            return
        if any("is finished" in t for t, _ in c.chat_lines):
            return
        stack = c.find_item_by_res(resname)
        assert stack is not None, "material missing: %s" % resname
        c.take_item(stack)
        c.pump(0.3)
        c.map_itemact(mc, plan)
        c.pump(0.5)
        assert c.return_cursor(), "inventory window missing for cursor return"
        c.pump(0.3)


def build_kiln(c):
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)
    plan = None
    mc = None
    for dx in range(-3, 4):
        cand = (ptile[0] + dx, ptile[1] + 1)
        cmc = (cand[0] * 11 + 5, cand[1] * 11 + 5)
        c.menu_act("kiln")
        ok = c.wait_for(lambda: c.place_seen is not None, 5)
        assert ok, "place uimsg missing for the kiln pagina"
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
                g
                for g, info in c.gobs.items()
                if info["res"] == KILN_RES and info["pos"] == cmc
            )
            break
    assert plan is not None, "no free tile accepted a kiln plan"
    print("kiln plan placed: %s" % plan)

    sink_demand(c, plan, mc, CLAY_INV, 45)
    ok = c.wait_for(
        lambda: any("The kiln is finished." in t for t, _ in c.chat_lines), 10
    )
    assert ok, "kiln completion never announced (lines=%r)" % (c.chat_lines[-4:],)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x00", 10)
    assert ok, "kiln never re-rendered as a station (sdt=%r)" % (
        c.gobs[plan]["sdt"],)
    print("kiln completed: %s" % plan)
    return plan, mc


def craft_once(c, recipe_id, expect_res):
    """Open the make window, press craft, wait for the output item."""
    c.menu_act("craft", recipe_id)
    ok = c.wait_for(lambda: any(n == "make" for n in c.widgets.values()), 8)
    assert ok, "no make widget for %s" % recipe_id
    make_wid = max(w for w, n in c.widgets.items() if n == "make")
    c.pump(0.3)
    c.wdgmsg(make_wid, "make", bytes([LIST_INT]) + le32(0) + bytes([LIST_END]))
    ok = c.wait_for(lambda: c.find_item_by_res(expect_res) is not None, 8)
    assert ok, "make %s never produced %s" % (recipe_id, expect_res)


def fire_ware(c, plan, mc, input_res, world_res, inv_res, label):
    """Fuel one branch, load the unburnt ware, Light, catch the drop."""
    branch = c.find_item_by_res(BRANCH_INV)
    assert branch is not None, "no branch left for fuel"
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    c.pump(0.5)
    assert c.return_cursor(), "cursor return after fuel"
    c.pump(0.3)

    ware = c.find_item_by_res(input_res)
    assert ware is not None, "unburnt input missing: %s" % input_res
    c.take_item(ware)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    c.pump(0.5)

    seen = set(g for g, _ in find_drops(c, world_res))
    c.click_gob(plan, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Light"], 4)
    assert ok, "kiln Light menu missing (opts=%s)" % (c.sm_opts,)
    c.flower_choice(c.sm_wid, 0)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "kiln never re-rendered as lit (sdt=%r)" % (c.gobs[plan]["sdt"],)
    print("kiln lit; waiting out the 30-tick firing...")

    ok = c.wait_for(lambda: any(g not in seen for g, _ in find_drops(c, world_res)), 15)
    assert ok, "no %s drop appeared beside the kiln" % label
    drop_id = sorted(g for g, _ in find_drops(c, world_res) if g not in seen)[0]
    c.click_gob(drop_id, c.gobs[drop_id]["pos"])
    ok = c.wait_for(lambda: c.find_item_by_tooltip(label) is not None, 6)
    assert ok, "%s never reached the inventory (items=%s)" % (
        label, [(i["res"], i["tt"]) for i in c.item_info.values()],)
    out_wid = c.find_item_by_tooltip(label)
    info = c.item_info[out_wid]
    assert info["res"] == inv_res, "%s picked up as %s" % (label, info["res"])
    print("fired %s quality: %s" % (label, info["ql"]))


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "pot%d" % (
        int(time.time()) % 100000,)
    server_proc = ensure_server(
        save_path=os.path.join(
            REPO, "server", "target",
            "pottery-test-save-%d.json" % (os.getpid() % 100000),
        ),
    )
    try:
        c = enter_world(username)
        print("in world; driving the pottery-chain contract")

        walk_to_shore(c)
        gather_clay(c)

        plan, mc = build_kiln(c)

        pre = inv_total(c, CLAY_INV)
        craft_once(c, "jardough", "gfx/invobjs/dough-jar")
        jar_wid = c.find_item_by_res("gfx/invobjs/dough-jar")
        assert c.item_info[jar_wid]["tt"] == "Unburnt Jar", (
            "the jar dough label must key the kiln dispatch (tt=%r)"
            % c.item_info[jar_wid]["tt"])
        assert inv_total(c, "gfx/invobjs/dough-jar") >= 1, "jar molding empty"
        assert inv_total(c, CLAY_INV) == pre - 3, (
            "jar molding consumed the wrong clay count")
        print("JAR MOLD: OK (clay x3 -> Unburnt Jar)")

        pre = inv_total(c, CLAY_INV)
        craft_once(c, "mugdough", "gfx/invobjs/dough-mug")
        mug_wid = c.find_item_by_res("gfx/invobjs/dough-mug")
        assert c.item_info[mug_wid]["tt"] == "Unburnt Clay Mug", (
            "the mug dough label must key the kiln dispatch (tt=%r)"
            % c.item_info[mug_wid]["tt"])
        assert inv_total(c, "gfx/invobjs/dough-mug") >= 1, "mug molding empty"
        assert inv_total(c, CLAY_INV) == pre - 2, (
            "mug molding consumed the wrong clay count")
        print("MUG MOLD: OK (clay x2 -> Unburnt Clay Mug)")

        fire_ware(c, plan, mc, "gfx/invobjs/dough-jar",
                  JAR_WORLD, JAR_INV, JAR_LABEL)
        print("JAR FIRE: OK")
        fire_ware(c, plan, mc, "gfx/invobjs/dough-mug",
                  MUG_WORLD, MUG_INV, MUG_LABEL)
        print("MUG FIRE: OK")

        c.sock.close()
        print("POTTERY: OK (shore clay, kiln build, jar + mug molded and fired)")
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


if __name__ == "__main__":
    main()
