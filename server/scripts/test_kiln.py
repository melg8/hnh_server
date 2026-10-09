#!/usr/bin/env python3
"""Session 69 kiln-chain probe on the hnhlib harness.

Drives the full clay -> kiln -> brick contract against a live server:

  1. CLAY: walk to the sandy shore on the dev seed (the hnh-world
     sandy_shores_are_reachable_from_spawn test pins the nearest shore
     at ~51 tiles from the spawn area on seed 42), find clay deposits
     (gfx/terobjs/mining/heap piles standing ON a sand tile - the ore
     heaps sit on mountain/cave ground, the tile under the pile tells
     them apart), click each one, and catch the Clay drops until the
     inventory holds CLAY_NEED = 46 units (45 for the kiln build
     demand + 1 for the firing input; pickups merge into one stack).
  2. BUILD: place a kiln plan (gfx/terobjs/kiln) on a free tile near
     the shore, sink the Clay x45 demand (the merged stack closes it
     in one delivery), and wait for the plan to complete into a
     station gob.
  3. FIRE: itemact a branch (fuel), itemact the remaining Clay (input
     slot), flower-menu Light, wait out the 30-tick job, catch the
     Brick drop (gfx/terobjs/items/brick - the pack ships the real
     world shape, no alias), pick it up, and verify the "Brick"
     tooltip landed in the inventory with the station-formula quality.

Prints "KILN: OK ..." on success.

Usage: python3 test_kiln.py [username]
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import ensure_server, enter_world  # noqa: E402

DEPOSIT_RES = "gfx/terobjs/mining/heap"
CLAY_WORLD = "gfx/terobjs/items/clay"
CLAY_INV = "gfx/invobjs/clay"
BRICK_WORLD = "gfx/terobjs/items/brick"
BRICK_INV = "gfx/invobjs/brick"
BRICK_LABEL = "Brick"
KILN_RES = "gfx/terobjs/kiln"
SAND_TILE = 20  # mirror hnh_world::gen tile::SAND
# 45 build + 1 firing input (the legacy Kiln demand is Clay x45).
CLAY_NEED = 46
# The dev-seed shore centers on tile (101, 0) on seed 42 (measured by
# the hnh-world sandy_shores_are_reachable_from_spawn test).
SHORE_TILE = (101, 0)


def find_clay_deposits(c):
    """Heap gobs standing on a SAND tile: the shore belt is the clay
    leg's ground; the rocky belt's ore heaps sit on mountain/cave
    tiles, so the tile under the pile separates the two families."""
    found = []
    for g, info in c.gobs.items():
        if info.get("removed") or info["res"] != DEPOSIT_RES:
            continue
        pos = info["pos"]
        if c.tile_at(pos[0], pos[1]) == SAND_TILE:
            found.append((g, info))
    return found


def find_brick_drops(c):
    return [
        (g, info)
        for g, info in c.gobs.items()
        if info["res"] == BRICK_WORLD and not info.get("removed")
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
    """BFS-navigate to the pinned shore tile; the streamed grids and
    the path rebuild as the player moves (the test_smelt nav pattern).
    The shore grid (gc 1,0) arrives in the post-entry MAPDATA burst, so
    WAIT for it before planning: BFS only walks loaded tiles."""
    target = (SHORE_TILE[0] * 11 + 5, SHORE_TILE[1] * 11 + 5)
    shore_gc = (SHORE_TILE[0] // 100, SHORE_TILE[1] // 100)
    assert c.wait_for(lambda: c.tiles.get(shore_gc) is not None, 15), (
        "the shore grid %s never streamed in" % (shore_gc,))
    assert c.nav_walk(target, stop=80, max_clicks=220), (
        "could not reach the shore at %s" % (target,)
    )
    print("at the shore: %s" % (c.gobs[c.player_gob]["pos"],))


def gather_clay(c):
    """Pick shore deposits until the inventory holds CLAY_NEED clay.
    Each deposit survives CLAY_PICKS = 8 picks; `used` keeps the probe
    from re-picking an exhausted pile."""
    used = set()
    while inv_total(c, CLAY_INV) < CLAY_NEED:
        deposits = [d for d in find_clay_deposits(c) if d[0] not in used]
        if not deposits:
            # Scan a wider ring: stream new grids by walking along the
            # shore line (the belt runs along the coast).
            pos = c.gobs[c.player_gob]["pos"]
            assert c.nav_walk((pos[0] + 150, pos[1]), stop=60, max_clicks=40), (
                "shore ran out of reachable deposits"
            )
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
    """Sink `units` of `resname` into the plan. Pickups merge into one
    stack, so the whole demand usually closes in a single delivery;
    the loop tolerates partial stacks anyway (each round delivers what
    the cursor holds). Stop on the completion system line: the plan
    converts into the station UNDER THE SAME GOB ID (complete_plan),
    and a second delivery would feed the station's input slot."""
    c.chat_lines.clear()
    for _ in range(units):
        if c.gobs.get(plan) is None:
            return  # plan completed and converted under us
        if any("is finished" in t for t, _ in c.chat_lines):
            return  # completion announcement (sink_material)
        stack = c.find_item_by_res(resname)
        assert stack is not None, "material missing: %s" % resname
        c.take_item(stack)
        c.pump(0.3)
        c.map_itemact(mc, plan)
        c.pump(0.5)
        assert c.return_cursor(), "inventory window missing for cursor return"
        c.pump(0.3)


def build_kiln(c):
    """Place a kiln plan on a free tile and sink the Clay x45 demand.
    Mirrors test_smelt.build_station's stage choreography (stage byte
    must CHANGE per material batch and return to 0 on completion)."""
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
    # The Clay x45 demand fits ONE merged pickup stack, so the first
    # delivery completes the plan outright: the observable completion
    # signal is the system line (a full-demand delivery never shows an
    # intermediate stage byte). Wait it out, then assert the gob re-
    # rendered as a station (sdt 0 = unlit).
    ok = c.wait_for(
        lambda: any("The kiln is finished." in t for t, _ in c.chat_lines), 10
    )
    assert ok, "kiln completion never announced (lines=%r)" % (c.chat_lines[-4:],)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x00", 10)
    assert ok, "kiln never re-rendered as a station (sdt=%r)" % (
        c.gobs[plan]["sdt"],)
    print("kiln completed: %s" % plan)
    return plan, mc


def fire_brick(c, plan, mc):
    """Fuel, load the clay, light, and catch the brick drop."""
    # Fuel: one branch unit per itemact.
    branch = c.find_item_by_res("gfx/invobjs/branch")
    assert branch is not None, "no branch left for fuel"
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    c.pump(0.5)
    assert c.return_cursor(), "cursor return after fuel"
    c.pump(0.3)

    # Input: the leftover clay (count 1 after the 45-unit build sink).
    clay_wid = c.find_item_by_res(CLAY_INV)
    assert clay_wid is not None, "clay input missing"
    c.take_item(clay_wid)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    c.pump(0.5)

    # Light through the flower menu.
    c.click_gob(plan, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Light"], 4)
    assert ok, "kiln Light menu missing (opts=%s)" % (c.sm_opts,)
    c.flower_choice(c.sm_wid, 0)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "kiln never re-rendered as lit (sdt=%r)" % (c.gobs[plan]["sdt"],)
    print("kiln lit; waiting out the 30-tick job...")

    # Output: a brick drop appears beside the kiln.
    seen = set(g for g, _ in find_brick_drops(c))
    ok = c.wait_for(lambda: any(g not in seen for g, _ in find_brick_drops(c)), 15)
    assert ok, "no brick drop appeared beside the kiln"
    drop_id = sorted(g for g, _ in find_brick_drops(c) if g not in seen)[0]
    print("brick drop:", c.gobs[drop_id]["res"], "at", c.gobs[drop_id]["pos"])

    c.click_gob(drop_id, c.gobs[drop_id]["pos"])
    ok = c.wait_for(lambda: c.find_item_by_tooltip(BRICK_LABEL) is not None, 6)
    assert ok, "Brick never reached the inventory (items=%s)" % (
        [(i["res"], i["tt"]) for i in c.item_info.values()],
    )
    out_wid = c.find_item_by_tooltip(BRICK_LABEL)
    info = c.item_info[out_wid]
    assert info["res"] == BRICK_INV, "brick picked up as %s" % info["res"]
    print("brick quality:", info["ql"])
    print("BRICK: OK (%s q%s in the inventory)" % (BRICK_LABEL, info["ql"]))


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "kiln"
    server_proc = ensure_server()
    try:
        c = enter_world(username)
        print("in world; driving the kiln-chain contract")
        walk_to_shore(c)
        gather_clay(c)
        plan, mc = build_kiln(c)
        fire_brick(c, plan, mc)
        c.sock.close()
        print("KILN: OK (shore clay picks, kiln build, one brick fired)")
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


if __name__ == "__main__":
    main()
