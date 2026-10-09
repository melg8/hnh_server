#!/usr/bin/env python3
"""Session 81 dough-chain probe on the hnhlib harness.

Drives the new forage/apple/hive/raisin legs against a live server:

  1. FORAGE: walk to the broadleaf forest (or heath/grass for the
     other kinds), pick a wild plant, verify the multi-unit handful
     (one drop gob, 3 units in the inventory after one pickup).
  2. APPLE: pick an apple tree 5 times (one Apple drop each), verify
     the 6th click yields a branch (the degraded plain tree - the
     legacy stage-6 apple tree yields both).
  3. HIVE: click a wild beehive WITHOUT a bucket (refusal line),
     craft saw + bucket from the starter kit, harvest honey (empty
     bucket -> Bucket of Honey).
  4. RAISINS: pick a grapevine, hand-craft grapes x2 -> raisins
     through the make widget (the fork pagina composes the page).
  5. EAT: eat a raw apple -> the `food` uimsg (the new fep.conf row
     Apple=CON:1; before the row the eat silently bailed).
  6. PIE: the full end-to-end bake cycle over a session-81 dough:
     farm wheat on the fast crop clock, grind flour at the quern,
     craft Blueberry Pie Dough (flour + bucket-water + the foraged
     blueberries), build the oven, bake the pie, verify the baked
     label/resource, and eat it (the Blueberry Pie=INT:4 PER:3 row).

Prints "DOUGH: OK ..." on success.

Usage: python3 test_dough.py [username]
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import (  # noqa: E402
    LIST_COORD,
    LIST_END,
    LIST_INT,
    REPO,
    RMSG_WDGMSG,
    TILE_SPAN,
    WireClient,
    ensure_server,
    enter_world,
    le32,
)

# The full-pie leg reuses the test_bake farming/station/bake helpers
# (same directory; imported for reuse, not re-implemented).
from test_bake import (  # noqa: E402
    FLOUR_INV,
    OVEN_RES,
    QUERN_RES,
    BakeClient,
    build_station,
    find_and_fill_water,
    grind_flour,
    plant_and_harvest,
)

# Tile ids (mirror hnh_world::gen).
BROADLEAF = 11
GRASS = 13
HEATH = 15

APPLETREE_RES = "gfx/terobjs/trees/appletree"
HIVE_RES = "gfx/terobjs/bhive"
GRAPEVINE_RES = "gfx/terobjs/plants/wine"

BLUEBERRY_PLANT = "gfx/terobjs/herbs/blueberry"
CHANT_PLANT = "gfx/terobjs/herbs/chantrelle"
ONION_PLANT = "gfx/terobjs/plants/onion"

# World drop shapes: blueberries render through the mulberry alias
# (DROP_WORLD_ALIASES), the others have their own terobj shapes.
BLUEBERRY_DROP = "gfx/terobjs/items/mulberry"
BLUEBERRY_INV = "gfx/invobjs/bluberry"
CHANT_DROP = "gfx/terobjs/items/shrooms-picked"
CHANT_INV = "gfx/invobjs/shrooms-picked"
ONION_DROP = "gfx/terobjs/items/onion"
ONION_INV = "gfx/invobjs/onion"

GRAPE_DROP = "gfx/terobjs/items/grapes"
GRAPE_INV = "gfx/invobjs/grapes"
APPLE_DROP = "gfx/terobjs/items/apple"
APPLE_INV = "gfx/invobjs/apple"
BRANCH_DROP = "gfx/terobjs/items/branch"
BRANCH_INV = "gfx/invobjs/branch"
BUCKETE_INV = "gfx/invobjs/buckete"
HONEY_INV = "gfx/invobjs/bucket-honey"
HONEY_LABEL = "Bucket of Honey"
RAISINS_INV = "gfx/invobjs/raisins"
RAISINS_LABEL = "Raisins"
SAW_INV = "gfx/invobjs/saw"
APPLE_LABEL = "Apple"

# The full-pie leg (BakeClient's farming helpers ride the same
# transport; the dough is the one session-81 recipe whose every
# ingredient the probe itself produced).
PIE_DOUGH_INV = "gfx/invobjs/dough-pie-blueberry"
PIE_INV = "gfx/invobjs/pie-blueberry"
PIE_WORLD = "gfx/terobjs/items/pie-blueberry"
PIE_LABEL = "Blueberry Pie"


class DoughClient(BakeClient):
    """BakeClient (farming/station helpers) + the `food` uimsg flag
    (the eat verdict).

    __init__ deliberately skips BakeClient's and builds on WireClient
    directly: BakeClient turns request_chr off (its flow never needs
    the char sheet at bind time), but the eat verdicts here need the
    chr window - the server pushes the `food` uimsg ON that window
    (items.rs push_food_msg chr_window()). The farming helpers this
    class reuses do not depend on BakeClient's __init__ state.
    """

    def __init__(self, username):
        WireClient.__init__(self, username)
        self.food_msg = False

    def on_event(self, t, body):
        if t == RMSG_WDGMSG and not self.food_msg:
            try:
                body.index(0, 2)
                name = body[2:body.index(0, 2)].decode()
                if name == "food":
                    self.food_msg = True
            except (ValueError, UnicodeDecodeError):
                pass
        return super().on_event(t, body)


def ensure_dough_server():
    """Fast-crop-clock isolated server (the pie leg farms wheat - the
    real crop clock takes hours; the test_bake scale makes it 250 ms
    per stage) on a per-run save file."""
    return ensure_server(
        env_extra={"HNH_CROP_TIME_SCALE": "10000000"},
        save_path=os.path.join(
            REPO, "server", "target",
            "dough-test-save-%d.json" % (os.getpid() % 100000),
        ),
    )


def inv_total(c, resname):
    return sum(
        info.get("count", 1)
        for info in c.item_info.values()
        if info["res"] == resname
    )


def request_neighborhood(c, ring=2):
    """Ask the 5x5 grid neighborhood (the statics live in small
    biome patches; the 3x3 burst around spawn may carry none of a
    given kind - the first probe draft walked to a broadleaf TILE
    whose patch held no grapevine and died)."""
    deadline = time.time() + 15
    while True:
        missing = [
            (gx, gy)
            for gy in range(-ring, ring + 1)
            for gx in range(-ring, ring + 1)
            if c.tiles.get((gx, gy)) is None
        ]
        if not missing:
            return
        assert time.time() < deadline, "grids never streamed: %s" % (missing,)
        for gc in missing:
            c.mapreq(*gc)
        c.wait_for(lambda: False, 1.5)


def walk_to_gob(c, resname, what):
    """Find the nearest live gob of a resource in the loaded grids,
    walk to it, return (gid, info)."""
    found = {
        g: info
        for g, info in c.gobs.items()
        if info["res"] == resname and not info.get("removed")
    }
    assert found, "no %s spawned in the 5x5 grids" % resname
    px, py = c.gobs[c.player_gob]["pos"]
    gid = min(found, key=lambda g: abs(found[g]["pos"][0] - px)
              + abs(found[g]["pos"][1] - py))
    pos = found[gid]["pos"]
    ok = c.nav_walk(pos, stop=50, max_clicks=400,
                    log=lambda m: print(m))
    assert ok, "the %s at %s is unreachable" % (what, pos)
    print("at the %s: %s" % (what, c.gobs[c.player_gob]["pos"]))
    return gid, found[gid]


def pick_and_pickup(c, plant_res, drop_res, inv_res, label, units):
    """Click the plant, verify ONE multi-unit drop, pick it up."""
    gid, info = walk_to_gob(c, plant_res, label + " plant")
    before = inv_total(c, inv_res)
    c.click_gob(gid, c.gobs[gid]["pos"])
    ok = c.wait_for(lambda: c.gobs.get(gid, {}).get("removed"), 6)
    assert ok, "the plant was not consumed by the pick"
    drops = [
        g
        for g, info in c.gobs.items()
        if info["res"] == drop_res and not info.get("removed")
    ]
    assert drops, "no %s drop spawned after the forage pick" % drop_res
    drop_id = sorted(drops)[0]
    c.click_gob(drop_id, c.gobs[drop_id]["pos"])
    ok = c.wait_for(lambda: inv_total(c, inv_res) >= before + units, 8)
    assert ok, "the handful never landed (%d units)" % inv_total(c, inv_res)
    total = inv_total(c, inv_res)
    assert total == before + units, (
        "expected %d units in one handful, got %d" % (units, total - before))
    print("FORAGE PICK: %s -> %d units (multi-unit drop OK)" % (label, units))


def apple_tree_leg(c):
    gid, info = walk_to_gob(c, APPLETREE_RES, "apple tree")
    apples = 0
    for i in range(5):
        before = inv_total(c, APPLE_INV)
        seen = [
            g for g, _ in (
                (g, info) for g, info in c.gobs.items()
                if info["res"] == APPLE_DROP and not info.get("removed"))
        ]
        c.click_gob(gid, c.gobs[gid]["pos"])
        ok = c.wait_for(
            lambda: any(
                g not in seen and not info.get("removed")
                for g, info in c.gobs.items()
                if info["res"] == APPLE_DROP), 6)
        assert ok, "pick %d: no apple drop spawned" % (i + 1)
        drop_id = sorted(
            g for g, info in c.gobs.items()
            if info["res"] == APPLE_DROP and not info.get("removed")
            and g not in seen)[0]
        c.click_gob(drop_id, c.gobs[drop_id]["pos"])
        ok = c.wait_for(
            lambda: inv_total(c, APPLE_INV) > before, 8)
        assert ok, "pick %d: the apple never landed" % (i + 1)
        apples = inv_total(c, APPLE_INV)
        print("APPLE PICK %d: %d apples" % (i + 1, apples))
    assert apples == 5, "expected 5 apples, got %d" % apples
    # The degraded tree keeps its gob (plain Kind::Tree now): the next
    # click must yield a branch, not another apple.
    before_branch = inv_total(c, BRANCH_INV)
    before_apple = inv_total(c, APPLE_INV)
    seen = [
        g for g, info in c.gobs.items()
        if info["res"] == BRANCH_DROP and not info.get("removed")
    ]
    c.click_gob(gid, c.gobs[gid]["pos"])
    ok = c.wait_for(
        lambda: any(
            g not in seen and not info.get("removed")
            for g, info in c.gobs.items()
            if info["res"] == BRANCH_DROP), 6)
    assert ok, "the exhausted tree yielded no branch (degradation broken)"
    drop_id = sorted(
        g for g, info in c.gobs.items()
        if info["res"] == BRANCH_DROP and not info.get("removed")
        and g not in seen)[0]
    c.click_gob(drop_id, c.gobs[drop_id]["pos"])
    ok = c.wait_for(lambda: inv_total(c, BRANCH_INV) > before_branch, 8)
    assert ok, "the branch never landed"
    assert inv_total(c, APPLE_INV) == before_apple, (
        "the exhausted tree still drops apples")
    print("APPLE TREE: 5 apples + degraded to a branch tree (OK)")


def hive_leg(c):
    gid, info = walk_to_gob(c, HIVE_RES, "beehive")
    # Refusal without a bucket (the starter kit ships none).
    c.chat_lines.clear()
    c.click_gob(gid, c.gobs[gid]["pos"])
    ok = c.wait_for(
        lambda: any("empty bucket" in t for t, _ in c.chat_lines), 6)
    assert ok, "no bucketless refusal line (chat=%r)" % (c.chat_lines[-4:],)
    print("HIVE REFUSAL: OK (needs an empty bucket)")
    # Craft saw + bucket from the starter kit (10 branch / 6 stone).
    craft_once(c, "saw", SAW_INV)
    craft_once(c, "bucket", BUCKETE_INV)
    assert inv_total(c, BUCKETE_INV) >= 1, "no empty bucket crafted"
    c.click_gob(gid, c.gobs[gid]["pos"])
    ok = c.wait_for(lambda: inv_total(c, HONEY_INV) == 1, 8)
    assert ok, "the hive never filled the bucket (items=%r)" % (
        [(i["res"], i["tt"]) for i in c.item_info.values()],)
    honey = c.find_item_by_tooltip(HONEY_LABEL)
    assert honey is not None and (
        c.item_info[honey]["res"] == HONEY_INV), (
        "the honey stack lost its label")
    print("HIVE HARVEST: %s (OK)" % HONEY_LABEL)


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


def raisins_leg(c):
    craft_once(c, "raisins", RAISINS_INV)
    wid = c.find_item_by_res(RAISINS_INV)
    info = c.item_info[wid]
    assert info["tt"] == RAISINS_LABEL, (
        "raisins label %r (want %r)" % (info["tt"], RAISINS_LABEL))
    assert info.get("count", 1) == 1, "raisins count %r" % (info.get("count"),)
    print("RAISINS: crafted %s x%d (fork pagina OK)"
          % (RAISINS_LABEL, info.get("count", 1)))


def eat_apple_leg(c):
    wid = c.find_item_by_tooltip(APPLE_LABEL)
    assert wid is not None, "no Apple item to eat"
    c.wdgmsg(
        wid, "iact",
        bytes([LIST_COORD]) + le32(0) + le32(0) + bytes([LIST_END]))
    ok = c.wait_for(lambda: c.sm_wid is not None, 6)
    assert ok, "the Eat flower menu never opened"
    assert "Eat" in c.sm_opts, "menu options %r" % (c.sm_opts,)
    c.flower_choice(c.sm_wid, c.sm_opts.index("Eat"))
    ok = c.wait_for(lambda: c.food_msg, 8)
    assert ok, "eating the apple pushed no `food` uimsg (fep row dead?)"
    print("EAT APPLE: food uimsg seen (Apple=CON:1 row live)")


def eat_item(c, label, what):
    """Flower-menu Eat any inventory item and re-arm the food flag
    (the first eat latched it; the second verdict needs a fresh one)."""
    wid = c.find_item_by_tooltip(label)
    assert wid is not None, "no %s item to eat" % what
    c.wdgmsg(
        wid, "iact",
        bytes([LIST_COORD]) + le32(0) + le32(0) + bytes([LIST_END]))
    ok = c.wait_for(lambda: c.sm_wid is not None, 6)
    assert ok, "the Eat flower menu never opened for the %s" % what
    assert "Eat" in c.sm_opts, "menu options %r" % (c.sm_opts,)
    c.food_msg = False
    c.flower_choice(c.sm_wid, c.sm_opts.index("Eat"))
    ok = c.wait_for(lambda: c.food_msg, 8)
    assert ok, "eating the %s pushed no `food` uimsg" % what


def find_any_tree(c):
    """The nearest live plain tree (any species; the fruit-tree
    degradation leg may already have converted the local apple tree,
    so scan every trees/ shape)."""
    found = {
        g: info
        for g, info in c.gobs.items()
        if (info["res"] or "").startswith("gfx/terobjs/trees/")
        and "appletree" not in info["res"]
        and "stump" not in (info["res"] or "")
        and not info.get("removed")
    }
    assert found, "no plain tree in the loaded grids"
    px, py = c.gobs[c.player_gob]["pos"]
    return min(
        found,
        key=lambda g: abs(found[g]["pos"][0] - px)
        + abs(found[g]["pos"][1] - py))


def gather_branches(c, want):
    """Pick `want` branches off forest trees (the full-pie leg's fuel
    budget: the starter kit's 10 branches cover saw + two buckets +
    quern + oven; the oven burn itself needs more)."""
    while inv_total(c, BRANCH_INV) < want:
        gid = find_any_tree(c)
        seen = {
            g for g, info in c.gobs.items()
            if info["res"] == BRANCH_DROP and not info.get("removed")
        }
        c.click_gob(gid, c.gobs[gid]["pos"])
        ok = c.wait_for(
            lambda: any(
                g not in seen and not info.get("removed")
                for g, info in c.gobs.items()
                if info["res"] == BRANCH_DROP), 6)
        assert ok, "the tree yielded no branch"
        drop_id = sorted(
            g for g, info in c.gobs.items()
            if info["res"] == BRANCH_DROP and not info.get("removed")
            and g not in seen)[0]
        c.click_gob(drop_id, c.gobs[drop_id]["pos"])
        before = inv_total(c, BRANCH_INV)
        ok = c.wait_for(
            lambda: inv_total(c, BRANCH_INV) > before, 8)
        assert ok, (
            "the branch never landed (stuck at %d, want %d)" % (
                inv_total(c, BRANCH_INV), want))
    print("branch fuel gathered (%d)" % inv_total(c, BRANCH_INV))


def walk_to_grass(c):
    """Nav-walk to the nearest plowable grass tile. The pie leg farms
    far from the start: the forage legs leave the player deep in the
    broadleaf forest, and the server's plow_tile refuses every tile
    but grass (farming.rs: `tile != tile::GRASS`)."""
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    px, py = ppos[0] // TILE_SPAN, ppos[1] // TILE_SPAN
    grass = None
    for r in range(0, 80, 2):
        for dy in range(-r, r + 1, 2):
            for dx in range(-r, r + 1, 2):
                if max(abs(dx), abs(dy)) != r:
                    continue
                t = c.tile_at(
                    (px + dx) * TILE_SPAN + 5,
                    (py + dy) * TILE_SPAN + 5,
                )
                if t == GRASS:
                    grass = (px + dx, py + dy)
                    break
            if grass:
                break
        if grass:
            break
    assert grass is not None, "no grass tile within ring 80"
    target = (grass[0] * TILE_SPAN + 5, grass[1] * TILE_SPAN + 5)
    ok = c.nav_walk(target, stop=20, max_clicks=400)
    assert ok, "nav to the grass field %s failed" % (grass,)
    print("at the grass field: %s" % (grass,))


def blueberry_pie_leg(c):
    """The full bake cycle over a session-81 dough: farm -> grind ->
    craft -> build -> bake -> eat. Every ingredient is produced by
    this probe run (flour from the starter seeds, water from the
    second crafted bucket, blueberries off the forage leg)."""
    # Fuel headroom: one branch more than the station demands.
    gather_branches(c, inv_total(c, BRANCH_INV) + 2)
    # The water bucket (the hive leg's bucket went into the honey).
    craft_once(c, "bucket", BUCKETE_INV)
    find_and_fill_water(c)
    assert inv_total(c, FLOUR_INV) == 0, "unexpected starter flour"
    # Farm the wheat (starter seeds, fast crop clock) and grind the
    # grist at a built quern. The plow needs grass: the forage legs
    # left the player in the forest, so nav back to open grass first.
    walk_to_grass(c)
    plant_and_harvest(c)
    quern, qmc = build_station(
        c, "quern", QUERN_RES,
        [("gfx/invobjs/stone", 2), ("gfx/invobjs/branch", 2)],
    )
    while inv_total(c, FLOUR_INV) < 2:
        grind_flour(c, quern, qmc)
    assert inv_total(c, FLOUR_INV) >= 2, "flour quota unmet"
    print("flour ground (%d in inventory)" % inv_total(c, FLOUR_INV))

    # The dough craft itself: flour x2 + bucket-water + blueberry x3
    # -> dough x2 + the empty bucket back.
    craft_once(c, "dough_blueberrypie", PIE_DOUGH_INV)
    assert inv_total(c, PIE_DOUGH_INV) >= 2, "the dough craft made nothing"
    assert c.find_item_by_res(BUCKETE_INV) is not None, (
        "the empty bucket never returned from the dough craft")
    print("PIE DOUGH: crafted x%d (bucket returned)" % inv_total(
        c, PIE_DOUGH_INV))

    # The oven and the bake (the bake_bread choreography, keyed to
    # the blueberry dough and its pie world shape).
    oven, omc = build_station(
        c, "oven", OVEN_RES,
        [("gfx/invobjs/stone", 2), ("gfx/invobjs/branch", 1)],
    )
    bake_pie(c, oven, omc)

    # The eat verdict: the baked label resolves its fep.conf row.
    eat_item(c, PIE_LABEL, "blueberry pie")
    print("EAT PIE: food uimsg seen (Blueberry Pie=INT:4 PER:3 row live)")


def bake_pie(c, oven, mc):
    """Fuel one branch, load one blueberry dough, Light, catch the
    pie drop, pick it up, and verify the baked label + resource."""
    branch = c.find_item_by_res(BRANCH_INV)
    assert branch is not None, "no branch left for fuel"
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, oven)
    c.pump(0.5)
    assert c.return_cursor(), "cursor return after fuel"
    c.pump(0.3)

    dough_wid = c.find_item_by_res(PIE_DOUGH_INV)
    assert dough_wid is not None, "dough input missing"
    c.take_item(dough_wid)
    c.pump(0.3)
    c.map_itemact(mc, oven)
    c.pump(0.5)
    assert c.return_cursor(), "cursor return after the dough load"
    c.pump(0.3)

    seen = {g for g, info in c.gobs.items()
            if info["res"] == PIE_WORLD and not info.get("removed")}
    c.click_gob(oven, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Light"], 4)
    assert ok, "oven Light menu missing (opts=%s)" % (c.sm_opts,)
    c.flower_choice(c.sm_wid, 0)
    ok = c.wait_for(lambda: c.gobs[oven]["sdt"] == b"\x01", 4)
    assert ok, "oven never re-rendered as lit (sdt=%r lines=%r)" % (
        c.gobs[oven]["sdt"], c.chat_lines[-6:],)
    print("oven lit; waiting out the bake...")

    ok = c.wait_for(
        lambda: any(
            g not in seen and not info.get("removed")
            for g, info in c.gobs.items()
            if info["res"] == PIE_WORLD), 15)
    assert ok, "no pie drop appeared beside the oven"
    drop_id = sorted(
        g for g, info in c.gobs.items()
        if info["res"] == PIE_WORLD and not info.get("removed")
        and g not in seen)[0]
    c.click_gob(drop_id, c.gobs[drop_id]["pos"])
    ok = c.wait_for(
        lambda: c.find_item_by_tooltip(PIE_LABEL) is not None, 6)
    assert ok, "the pie never reached the inventory (items=%s)" % (
        [(i["res"], i["tt"]) for i in c.item_info.values()],)
    out_wid = c.find_item_by_tooltip(PIE_LABEL)
    info = c.item_info[out_wid]
    assert info["res"] == PIE_INV, "pie picked up as %s" % info["res"]
    print("PIE BAKED: %s q%d" % (PIE_LABEL, info["ql"]))


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "dough%d" % (
        int(time.time()) % 100000,)
    proc = ensure_dough_server()
    try:
        c = enter_world(username, client_cls=DoughClient)
        request_neighborhood(c)

        # 1. FORAGE: the grapevine is the raisin chain's source and the
        # multi-unit handful contract in one leg.
        pick_and_pickup(
            c, GRAPEVINE_RES, GRAPE_DROP, GRAPE_INV, "Grapes", 3)
        # The other three kinds (one pick each, the same handful
        # contract): blueberries, chantrelles, wild onions.
        pick_and_pickup(
            c, BLUEBERRY_PLANT, BLUEBERRY_DROP, BLUEBERRY_INV,
            "Blueberries", 3)
        pick_and_pickup(
            c, CHANT_PLANT, CHANT_DROP, CHANT_INV, "Chantrelles", 3)
        pick_and_pickup(
            c, ONION_PLANT, ONION_DROP, ONION_INV, "Yellow Onion", 3)

        # 2. APPLE TREE: the five-pick budget and the degradation.
        apple_tree_leg(c)

        # 3. HIVE: refusal, then the saw+bucket craft and the honey.
        hive_leg(c)

        # 4. RAISINS: grapes x2 -> raisins through the make widget.
        raisins_leg(c)

        # 5. EAT: the raw apple resolves its fep.conf row.
        eat_apple_leg(c)

        # 6. PIE: the full farm -> grind -> craft -> bake -> eat cycle
        # over a session-81 dough (the fast-crop server this probe
        # boots makes the wheat leg minutes, not hours).
        blueberry_pie_leg(c)

        print("DOUGH: OK (%s: 3 grapes, 5 apples, honey, raisins,"
              " apple eat, blueberry pie baked + eaten)" % username)
    finally:
        from hnhlib import stop_server
        if proc is not None:
            stop_server(proc)


if __name__ == "__main__":
    sys.exit(main())
