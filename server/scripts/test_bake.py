#!/usr/bin/env python3
"""Session 71 baking-chain probe on the hnhlib harness.

Drives the FULL grain -> bread contract against a live server:

  1. TOOLS: craft the Saw (Branch x2 + Stone x1) and the Bucket
     (Branch x3, saw-gated) from the starter kit.
  2. GRAIN: buy the Farming skill value (sattr), plow + plant the five
     starter Wheat Seeds, wait out the fast-crop maturity (250 ms per
     stage at HNH_CROP_TIME_SCALE=10000000), harvest every crop through
     the flower menu, and collect Grist of Wheat (the mature wheat
     product, farm.rs) until GRIST_NEED units are in the inventory.
  3. WATER: ring-scan the streamed tiles for the nearest water tile
     (the hnh-world water_is_reachable_from_spawn test pins one within
     250 tiles of spawn on seed 42; measured at 57), nav_walk to it
     (impassable goals retarget a walkable neighbor), and itemact the
     empty bucket onto the water tile - the cursor stack becomes a
     Bucket of Water (game/items.rs bucket fill).
  4. QUERN: build the hand mill (Stone x2 + Branch x2, no fuel gate),
     deliver one grist unit per job, start it through the Grind flower
     verb (the quern is hand-cranked - building.rs menu policy), wait
     out the 15-tick job, catch the Flour drop (world shape rides the
     DROP_WORLD_ALIASES bag-seed fallback - the pack ships no flour
     terobj), and pick it up. Repeat until FLOUR_NEED.
  5. DOUGH: hand-craft Bread Dough (Flour x2 + Bucket of Water -> Dough
     x2 + the empty bucket back) through the make widget.
  6. OVEN: build it (Stone x2 + Branch x1), fuel one branch, load one
     dough unit, Light, wait out the 8-tick job, catch the Bread drop
     (gfx/terobjs/items/bread ships), pick it up, and verify the
     "Bread" tooltip landed in the inventory.

Prints "BAKE: OK ..." on success.

Usage: python3 test_bake.py [username]
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import (  # noqa: E402
    LIST_END,
    LIST_INT,
    LIST_STR,
    REPO,
    TILE_SPAN,
    WireClient,
    enter_world,
    ensure_server,
    havstr,
    le32,
)

GRIST_INV = "gfx/invobjs/grist-wheat"
FLOUR_INV = "gfx/invobjs/flour"
FLOUR_WORLD = "gfx/terobjs/items/bag-seed"  # DROP_WORLD_ALIASES fallback
DOUGH_INV = "gfx/invobjs/dough"
BUCKETE_INV = "gfx/invobjs/buckete"
BUCKET_WATER_INV = "gfx/invobjs/bucket-water"
BREAD_WORLD = "gfx/terobjs/items/bread"
BREAD_INV = "gfx/invobjs/bread"
BREAD_LABEL = "Bread"
QUERN_RES = "gfx/terobjs/quern"
OVEN_RES = "gfx/terobjs/oven"
# 2 flour for one dough craft (1 grind job per grist unit; the mature
# wheat harvest of five planted seeds yields 5-10 grist - plenty).
GRIST_NEED = 2
FLOUR_NEED = 2
DEEP_WATER_TILE, WATER_TILE = 0, 1  # mirror hnh_world::gen tile ids


def ensure_bake_server():
    """Fast-crop-clock isolated server on a per-run save file."""
    return ensure_server(
        env_extra={"HNH_CROP_TIME_SCALE": "10000000"},
        save_path=os.path.join(
            REPO, "server", "target",
            "bake-test-save-%d.json" % (os.getpid() % 100000),
        ),
    )


class BakeClient(WireClient):
    """Tile-based farming helpers on the shared transport (the
    test_farming FarmClient shape, request_chr kept off - the flow never
    opens the char sheet beyond the sattr buy)."""

    def __init__(self, username):
        super().__init__(username, request_chr=False)

    def click_tile(self, tile):
        mc = (tile[0] * TILE_SPAN + 5, tile[1] * TILE_SPAN + 5)
        self.click_ground(*mc)

    def arm_plow(self):
        self.menu_act("plow")

    def map_itemact_tile(self, tile):
        mc = (tile[0] * TILE_SPAN + 5, tile[1] * TILE_SPAN + 5)
        self.map_itemact(mc)

    def find_item(self, tooltip):
        return self.find_item_by_tooltip(tooltip)


def buy_farming_value(c):
    """Raise the Farming skill value through the real char sheet
    contract (slen 'chr' -> sattr pairs; legacy cost 100 for point 1)."""
    slen_wid = next(w for w, n in c.widgets.items() if n == "slen")
    c.wdgmsg(slen_wid, "chr", bytes([LIST_END]))
    ok = c.wait_for(lambda: c.chr_id is not None and c.exp_seen is not None, 5)
    assert ok, "char sheet never opened (widgets=%s)" % sorted(set(c.widgets.values()))
    c.wdgmsg(
        c.chr_id,
        "sattr",
        bytes([LIST_STR]) + havstr("farming")
        + bytes([LIST_INT]) + le32(1)
        + bytes([LIST_END]),
    )
    ok = c.wait_for(lambda: c.attrs.get("farming", 0) >= 1, 5)
    assert ok, "farming value never raised (attrs=%s)" % (c.attrs,)


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


def inv_total(c, resname):
    return sum(
        info.get("count", 1)
        for info in c.item_info.values()
        if info["res"] == resname
    )


def plant_and_harvest(c):
    """Plow + plant the five starter Wheat Seeds on nearby tiles, wait
    for maturity (wire stage byte 3), and harvest every crop through
    the flower menu."""
    buy_farming_value(c)
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // TILE_SPAN, ppos[1] // TILE_SPAN)
    wheat_item = c.find_item("Wheat Seeds")
    assert wheat_item is not None, "starter wheat seeds missing"

    crops = []
    planted = 0
    for dx in range(-2, 3):
        for dy in range(-2, 3):
            if planted >= 5 or abs(dx) + abs(dy) > 3:
                continue
            tile = (ptile[0] + dx, ptile[1] + dy)
            c.arm_plow()
            c.pump(0.25)
            c.click_tile(tile)
            c.pump(0.3)
            c.take_item(wheat_item)
            c.pump(0.25)
            c.map_itemact_tile(tile)
            if c.wait_for(
                lambda: any(
                    (info["res"] or "") == "gfx/terobjs/plants/wheat"
                    for info in c.gobs.values()
                ),
                2.5,
            ):
                planted += 1
                c.pump(0.2)
    crops = c.find_gobs("gfx/terobjs/plants/wheat")
    assert len(crops) >= 2, "too few wheat crops planted (%d)" % len(crops)
    print("planted %d wheat crops" % len(crops))

    # Growth: 250 ms per stage at the test scale; wheat matures at
    # wire stage 3.
    deadline = time.time() + 15
    for gob in crops:
        while c.gobs[gob]["sdt"] != b"\x03":
            assert time.time() < deadline, "crop %s never matured" % gob
            c.pump(0.25)
    print("all crops mature")

    for gob in crops:
        c.click_gob(gob, c.gobs[gob]["pos"])
        ok = c.wait_for(lambda: any(n == "sm" for n in c.widgets.values()), 4)
        assert ok, "harvest flower menu never opened for %s" % gob
        sm_wid = next(w for w, n in c.widgets.items() if n == "sm")
        c.flower_choice(sm_wid)
        c.pump(0.5)
    total = inv_total(c, GRIST_INV)
    print("harvested: grist=%d seeds=%d straw=%d" % (
        total,
        inv_total(c, "gfx/invobjs/seed-wheat"),
        inv_total(c, "gfx/invobjs/straw"),
    ))
    assert total >= GRIST_NEED, "grist quota unmet (%d < %d)" % (total, GRIST_NEED)


def find_and_fill_water(c):
    """Ring-scan the streamed tiles for the nearest water tile, walk to
    it, and scoop a bucket of water (the cursor stack transforms in
    place)."""
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    px, py = ppos[0] // TILE_SPAN, ppos[1] // TILE_SPAN
    water = None
    for r in range(0, 80, 2):
        for dy in range(-r, r + 1, 2):
            for dx in range(-r, r + 1, 2):
                if max(abs(dx), abs(dy)) != r:
                    continue
                # tile_at takes SUBTILE coords; sample at the tile center.
                t = c.tile_at(
                    (px + dx) * TILE_SPAN + 5, (py + dy) * TILE_SPAN + 5
                )
                if t in (DEEP_WATER_TILE, WATER_TILE):
                    water = (px + dx, py + dy)
                    break
            if water:
                break
        if water:
            break
    assert water is not None, "no water tile within ring 80"
    print("water tile found: %s" % (water,))
    # Walk to a WALKABLE launch tile beside the water (the hnh-world
    # water_is_reachable_from_spawn contract pins at least one 4-neighbor
    # to exist). Aiming at the water itself makes nav_walk depend on the
    # impassable-goal retarget, which oscillates on the shore rim (the
    # S67 finding); a land goal is deterministic. Retry: the player
    # moves between attempts, so re-scan the launch each time.
    launch = None
    for attempt in range(3):
        for tx, ty in (
            (water[0] + 1, water[1]), (water[0] - 1, water[1]),
            (water[0], water[1] + 1), (water[0], water[1] - 1),
        ):
            if c._tile_walkable((tx, ty)):
                launch = (tx, ty)
                break
        assert launch is not None, "the water tile %s has no walkable neighbor" % (water,)
        mc = (launch[0] * TILE_SPAN + 5, launch[1] * TILE_SPAN + 5)
        if c.nav_walk(mc, stop=30, max_clicks=350, log=lambda m: print("  " + m)):
            break
        print("water walk attempt %d stalled; retrying" % (attempt + 1,))
        # The stall may have moved us closer: re-scan from the new spot.
        ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
        px, py = ppos[0] // TILE_SPAN, ppos[1] // TILE_SPAN
        water = None
        for r in range(0, 80, 2):
            for dy in range(-r, r + 1, 2):
                for dx in range(-r, r + 1, 2):
                    if max(abs(dx), abs(dy)) != r:
                        continue
                    t = c.tile_at(
                        (px + dx) * TILE_SPAN + 5, (py + dy) * TILE_SPAN + 5
                    )
                    if t in (DEEP_WATER_TILE, WATER_TILE):
                        water = (px + dx, py + dy)
                        break
                if water:
                    break
            if water:
                break
        assert water is not None, "no water tile within ring 80 on retry"
    mc = (water[0] * TILE_SPAN + 5, water[1] * TILE_SPAN + 5)

    bucket_wid = c.find_item_by_res(BUCKETE_INV)
    assert bucket_wid is not None, "empty bucket missing before the scoop"
    c.take_item(bucket_wid)
    c.pump(0.3)
    c.map_itemact(mc)
    ok = c.wait_for(
        lambda: any("scoop" in t for t, _ in c.chat_lines), 5
    )
    assert ok, "bucket fill refused (lines=%r)" % (c.chat_lines[-4:],)
    assert c.return_cursor(), "cursor return after the scoop"
    c.pump(0.3)
    assert c.find_item_by_res(BUCKET_WATER_INV) is not None, (
        "no Bucket of Water in the inventory after the scoop"
    )
    print("bucket filled")


def sink_demand(c, plan, mc, resname, units):
    """Sink `units` of `resname` into the plan; stop on the completion
    system line (the plan converts into the station under the same gob
    id - the S70 sink contract)."""
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


def build_station(c, pagina_id, station_res, demand):
    """Place a plan on a free tile near the player and sink the demand
    (the test_kiln choreography)."""
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // TILE_SPAN, ppos[1] // TILE_SPAN)
    plan = None
    mc = None
    for dx in range(-3, 4):
        cand = (ptile[0] + dx, ptile[1] + 1)
        cmc = (cand[0] * TILE_SPAN + 5, cand[1] * TILE_SPAN + 5)
        c.menu_act(pagina_id)
        ok = c.wait_for(lambda: c.place_seen is not None, 5)
        assert ok, "place uimsg missing for the %s pagina" % pagina_id
        c.place_seen = None
        c.send_place(cmc, 1, 0)
        if c.wait_for(
            lambda: any(
                info["res"] == station_res and info["pos"] == cmc
                for info in c.gobs.values()
            ),
            2.5,
        ):
            mc = cmc
            plan = next(
                g
                for g, info in c.gobs.items()
                if info["res"] == station_res and info["pos"] == cmc
            )
            break
    assert plan is not None, "no free tile accepted a %s plan" % pagina_id
    for resname, units in demand:
        sink_demand(c, plan, mc, resname, units)
    ok = c.wait_for(
        lambda: any("is finished" in t for t, _ in c.chat_lines), 10
    )
    assert ok, "%s completion never announced (lines=%r)" % (
        pagina_id, c.chat_lines[-4:],)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x00", 10)
    assert ok, "%s never re-rendered as a station (sdt=%r)" % (
        pagina_id, c.gobs[plan]["sdt"],)
    print("%s completed: %s" % (pagina_id, plan))
    return plan, mc


def find_drops(c, resname):
    return [
        (g, info)
        for g, info in c.gobs.items()
        if info["res"] == resname and not info.get("removed")
    ]


def pick_drop_into_inv(c, gid, inv_res):
    before = inv_total(c, inv_res)
    c.click_gob(gid, c.gobs[gid]["pos"])
    ok = c.wait_for(lambda: inv_total(c, inv_res) > before, 8)
    assert ok, "%s never landed in the inventory" % inv_res


def grind_flour(c, quern, mc):
    """One grind job: load ONE grist unit (the input takes a single
    item per delivery; the stack remainder returns with the cursor),
    start through the Grind verb, wait out the 15-tick job, and catch
    the Flour drop (the bag-seed aliased world shape)."""
    grist_wid = c.find_item_by_res(GRIST_INV)
    assert grist_wid is not None, "no grist left for the grind"
    c.take_item(grist_wid)
    c.pump(0.3)
    c.map_itemact(mc, quern)
    c.pump(0.5)
    assert c.return_cursor(), "cursor return after the grist load"
    c.pump(0.3)

    c.click_gob(quern, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Grind"], 4)
    assert ok, "quern Grind menu missing (opts=%s)" % (c.sm_opts,)
    c.flower_choice(c.sm_wid, 0)
    ok = c.wait_for(lambda: c.gobs[quern]["sdt"] == b"\x01", 4)
    assert ok, "quern never started (sdt=%r)" % (c.gobs[quern]["sdt"],)

    seen = set(g for g, _ in find_drops(c, FLOUR_WORLD))
    ok = c.wait_for(
        lambda: any(g not in seen for g, _ in find_drops(c, FLOUR_WORLD)), 15
    )
    assert ok, "no flour drop appeared beside the quern"
    drop_id = sorted(g for g, _ in find_drops(c, FLOUR_WORLD) if g not in seen)[0]
    pick_drop_into_inv(c, drop_id, FLOUR_INV)
    print("flour ground (%d in inventory)" % inv_total(c, FLOUR_INV))


def bake_bread(c, oven, mc):
    """Fuel one branch, load one dough unit, Light, and catch the Bread
    drop (the pack ships the real world shape)."""
    branch = c.find_item_by_res("gfx/invobjs/branch")
    assert branch is not None, "no branch left for fuel"
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, oven)
    c.pump(0.5)
    assert c.return_cursor(), "cursor return after fuel"
    c.pump(0.3)

    dough_wid = c.find_item_by_res(DOUGH_INV)
    assert dough_wid is not None, "dough input missing"
    c.take_item(dough_wid)
    c.pump(0.3)
    c.map_itemact(mc, oven)
    c.pump(0.5)

    # Baseline BEFORE lighting: any bread-shaped drop that already
    # exists must not satisfy the appearance wait.
    seen = set(g for g, _ in find_drops(c, BREAD_WORLD))

    c.click_gob(oven, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Light"], 4)
    assert ok, "oven Light menu missing (opts=%s)" % (c.sm_opts,)
    c.flower_choice(c.sm_wid, 0)
    ok = c.wait_for(lambda: c.gobs[oven]["sdt"] == b"\x01", 4)
    assert ok, "oven never re-rendered as lit (sdt=%r)" % (c.gobs[oven]["sdt"],)
    print("oven lit; waiting out the 8-tick bake...")

    ok = c.wait_for(
        lambda: any(g not in seen for g, _ in find_drops(c, BREAD_WORLD)), 15
    )
    if not ok:
        # Diagnostic dump: the oven's lit byte, every gob within three
        # tiles, and the tail of the system lines.
        c.pump(1.0)
        pos = c.gobs[oven]["pos"]
        near = [
            (g, info["res"], info["pos"], list(info["sdt"]))
            for g, info in c.gobs.items()
            if not info.get("removed")
            and abs(info["pos"][0] - pos[0]) < 40
            and abs(info["pos"][1] - pos[1]) < 40
        ]
        print("OVEN DEBUG: sdt=%r pos=%s" % (c.gobs[oven]["sdt"], pos))
        for row in sorted(near):
            print("  gob %s res=%s pos=%s sdt=%s" % row)
        print("  lines=%r" % (c.chat_lines[-6:],))
    assert ok, "no bread drop appeared beside the oven"
    drop_id = sorted(g for g, _ in find_drops(c, BREAD_WORLD) if g not in seen)[0]
    c.click_gob(drop_id, c.gobs[drop_id]["pos"])
    ok = c.wait_for(lambda: c.find_item_by_tooltip(BREAD_LABEL) is not None, 6)
    assert ok, "Bread never reached the inventory (items=%s)" % (
        [(i["res"], i["tt"]) for i in c.item_info.values()],
    )
    out_wid = c.find_item_by_tooltip(BREAD_LABEL)
    info = c.item_info[out_wid]
    assert info["res"] == BREAD_INV, "bread picked up as %s" % info["res"]
    print("bread quality:", info["ql"])


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "bake%d" % (
        int(time.time()) % 100000,)
    server_proc = ensure_bake_server()
    try:
        c = enter_world(username, client_cls=BakeClient)
        print("in world; driving the baking-chain contract")

        craft_once(c, "saw", "gfx/invobjs/saw")
        print("SAW CRAFT: OK")
        craft_once(c, "bucket", BUCKETE_INV)
        print("BUCKET CRAFT: OK")

        plant_and_harvest(c)
        find_and_fill_water(c)

        quern, qmc = build_station(
            c, "quern", QUERN_RES,
            [("gfx/invobjs/stone", 2), ("gfx/invobjs/branch", 2)],
        )
        while inv_total(c, FLOUR_INV) < FLOUR_NEED:
            grind_flour(c, quern, qmc)
        assert inv_total(c, FLOUR_INV) >= FLOUR_NEED, "flour quota unmet"

        craft_once(c, "dough", DOUGH_INV)
        assert inv_total(c, DOUGH_INV) >= 1, "dough craft produced nothing"
        assert c.find_item_by_res(BUCKETE_INV) is not None, (
            "the empty bucket never returned from the dough craft"
        )
        print("DOUGH CRAFT: OK (dough x%d + empty bucket back)" % inv_total(c, DOUGH_INV))

        oven, omc = build_station(
            c, "oven", OVEN_RES,
            [("gfx/invobjs/stone", 2), ("gfx/invobjs/branch", 1)],
        )
        bake_bread(c, oven, omc)
        c.sock.close()
        print("BAKE: OK (grain farmed, grist ground, dough kneaded, bread baked)")
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


if __name__ == "__main__":
    main()
