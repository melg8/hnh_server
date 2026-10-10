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
  7. HONEYBUN (session 83): the first no-butter dough with TWO
     filled-bucket inputs - flour x2 + bucket-water + the hive leg's
     bucket-honey -> Honeybun Dough x2 + both buckets back empty;
     baked in the SAME oven -> Honey Bun -> eaten (AGI:5).
  8. PIROZHKI (session 83): the savory dough over the forage legs -
     flour x2 + bucket-water + chantrelles x2 + onions x2 ->
     Pirozhki Dough x2; baked -> Chantrelle & Onion Pirozhki ->
     eaten (CON:4 DEX:4).

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
    GRIST_INV,
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

# Session 83: the two no-butter doughs whose ingredients the probe
# already produced (honey off the hive leg, chantrelles + onions off
# the forage legs). BAKE_MAP keys their baked outputs; fep.conf keys
# the eat rows (Honey Bun=AGI:5, Chantrelle & Onion Pirozhki=CON:4
# DEX:4).
HONEY_DOUGH_INV = "gfx/invobjs/dough-bun-honey"
HONEYBUN_INV = "gfx/invobjs/honeybun"
HONEYBUN_WORLD = "gfx/terobjs/items/honeybun"
HONEYBUN_LABEL = "Honey Bun"
PIRO_DOUGH_INV = "gfx/invobjs/dough-pirozhki"
PIRO_INV = "gfx/invobjs/feast-pirozhki"
PIRO_WORLD = "gfx/terobjs/items/feast-pirozhki"
PIRO_LABEL = "Chantrelle & Onion Pirozhki"


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


def find_any_tree(c, skip=()):
    """The nearest live plain tree (any species; the fruit-tree
    degradation leg may already have converted the local apple tree,
    so scan every trees/ shape). `skip` holds tree gobs that yielded
    nothing this run (session 84: a tree caps at TREE_HARVESTS = 5
    picks, then the server kills it and leaves a Stump - the stump res
    is gfx/terobjs/trees/LOG, not .../stump, so the old substring
    filter let the fruitless stump win the nearest-tree race)."""
    found = {
        g: info
        for g, info in c.gobs.items()
        if (info["res"] or "").startswith("gfx/terobjs/trees/")
        and "appletree" not in info["res"]
        and (info["res"] or "").rsplit("/", 1)[-1] != "log"
        and not info.get("removed")
        and g not in skip
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
    quern + oven; the oven burn itself needs more). Session 84: a
    tree holds TREE_HARVESTS = 5 picks, then the pick kills it and the
    next click of the dairy fuel loop hit the fruitless stump (the
    live probe asserted "the tree yielded no branch" right after the
    stations ate the fifth pick). A miss now marks the tree spent and
    the loop takes the next nearest tree - the forest always has
    another one in view."""
    spent = set()
    misses = 0
    while inv_total(c, BRANCH_INV) < want:
        gid = find_any_tree(c, skip=spent)
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
        if not ok:
            # Exhausted (or stumped, or merely slow) tree: spend it and
            # take the next nearest; bounded by the tree population.
            spent.add(gid)
            misses += 1
            assert misses < 12, "no tree in view yielded a branch"
            continue
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


def grass_tiles_near(c, ptile, count=5, rings=12):
    """Plowable grass tiles around `ptile`, nearest first, read from
    the STREAMED map (session 84: the fixed 5x5 dx/dy sweep of the
    wheat and carrot flows only works when the walk lands inside a
    wide field - on a lone grass pocket in the woods every neighbor
    but one plow-refuses ("not grass", farming.rs), and the live
    dairy runs planted 1 crop where the flow asked for 5). The
    spiral rings mirror walk_to_grass's search shape."""
    out = []
    for r in range(rings + 1):
        for dy in range(-r, r + 1):
            for dx in range(-r, r + 1):
                if max(abs(dx), abs(dy)) != r:
                    continue
                t = (ptile[0] + dx, ptile[1] + dy)
                if c.tile_at(
                        t[0] * TILE_SPAN + 5,
                        t[1] * TILE_SPAN + 5) == GRASS:
                    out.append(t)
                    if len(out) >= count:
                        return out
    return out


def ensure_flour(c, quern, qmc, want=2):
    """Grind flour at the standing quern until `want` units sit in the
    inventory; re-farm wheat when the leftover grist runs dry (the
    mature harvest rolls 1-2 grist per crop, so five crops can leave
    the third dough leg one unit short - the fast crop clock makes the
    re-farm seconds, not hours)."""
    while inv_total(c, FLOUR_INV) < want:
        if inv_total(c, GRIST_INV) == 0:
            walk_to_grass(c)
            plant_and_harvest(c)
        grind_flour(c, quern, qmc)
    assert inv_total(c, FLOUR_INV) >= want, "flour quota unmet"
    print("flour ground (%d in inventory)" % inv_total(c, FLOUR_INV))


def blueberry_pie_leg(c):
    """The full bake cycle over a session-81 dough: farm -> grind ->
    craft -> build -> bake -> eat. Every ingredient is produced by
    this probe run (flour from the starter seeds, water from the
    second crafted bucket, blueberries off the forage leg).
    Returns the built stations (quern, oven) so the session-83 dough
    legs reuse them instead of building their own."""
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
    bake_dough(
        c, oven, omc, PIE_DOUGH_INV, PIE_WORLD, PIE_LABEL, "blueberry pie")

    # The eat verdict: the baked label resolves its fep.conf row.
    eat_item(c, PIE_LABEL, "blueberry pie")
    print("EAT PIE: food uimsg seen (Blueberry Pie=INT:4 PER:3 row live)")
    return (quern, qmc), (oven, omc)


def honeybun_leg(c, quern, oven):
    """Session 83, leg 7: the honeybun dough (the one recipe with TWO
    filled-bucket inputs). The hive leg's Bucket of Honey + one
    water fill + ground flour -> dough x2 + both buckets back."""
    qmc, omc = quern[1], oven[1]
    quern, oven = quern[0], oven[0]
    # Fuel for this leg's bake.
    gather_branches(c, inv_total(c, BRANCH_INV) + 2)
    # The water bucket: the blueberry dough craft returned one empty.
    assert inv_total(c, BUCKETE_INV) >= 1, (
        "no empty bucket returned from the pie dough craft")
    find_and_fill_water(c)
    assert inv_total(c, HONEY_INV) >= 1, (
        "the hive leg's honey is gone (items=%s)" % (
            [(i["res"], i["tt"]) for i in c.item_info.values()],))
    ensure_flour(c, quern, qmc)

    craft_once(c, "hbdough", HONEY_DOUGH_INV)
    assert inv_total(c, HONEY_DOUGH_INV) >= 2, "the honeybun dough made < 2"
    assert inv_total(c, BUCKETE_INV) >= 2, (
        "both buckets must return empty (got %d)" % inv_total(c, BUCKETE_INV))
    print("HONEYBUN DOUGH: crafted x%d (both buckets returned)" % (
        inv_total(c, HONEY_DOUGH_INV)))

    bake_dough(
        c, oven, omc, HONEY_DOUGH_INV, HONEYBUN_WORLD, HONEYBUN_LABEL,
        "honeybun")
    eat_item(c, HONEYBUN_LABEL, "honey bun")
    print("EAT HONEYBUN: food uimsg seen (Honey Bun=AGI:5 row live)")


def pirozhki_leg(c, quern, oven):
    """Session 83, leg 8: the savory pirozhki dough over the forage
    legs' chantrelles and onions (flour x2 + water + shrooms x2 +
    onion x2 -> dough x2)."""
    qmc, omc = quern[1], oven[1]
    quern, oven = quern[0], oven[0]
    gather_branches(c, inv_total(c, BRANCH_INV) + 2)
    assert inv_total(c, BUCKETE_INV) >= 1, "no empty bucket for the water fill"
    find_and_fill_water(c)
    assert inv_total(c, CHANT_INV) >= 2, "chantrelles short (forage leg)"
    assert inv_total(c, ONION_INV) >= 2, "onions short (forage leg)"
    ensure_flour(c, quern, qmc)

    craft_once(c, "dough_pirozhki", PIRO_DOUGH_INV)
    assert inv_total(c, PIRO_DOUGH_INV) >= 2, "the pirozhki dough made < 2"
    # The savory inputs are consumed by the craft (2 of each).
    assert inv_total(c, CHANT_INV) == 1, (
        "the craft left %d chantrelles (want 1: 3 foraged - 2 consumed)"
        % inv_total(c, CHANT_INV))
    assert inv_total(c, ONION_INV) == 1, (
        "the craft left %d onions (want 1: 3 foraged - 2 consumed)"
        % inv_total(c, ONION_INV))
    print("PIROZHKI DOUGH: crafted x%d (2 shrooms + 2 onions consumed)"
          % inv_total(c, PIRO_DOUGH_INV))

    bake_dough(
        c, oven, omc, PIRO_DOUGH_INV, PIRO_WORLD, PIRO_LABEL, "pirozhki")
    eat_item(c, PIRO_LABEL, "pirozhki")
    print("EAT PIROZHKI: food uimsg seen"
          " (Chantrelle & Onion Pirozhki=CON:4 DEX:4 row live)")


def bake_dough(c, oven, mc, dough_inv, world_res, label, what):
    """Fuel one branch, load one dough, Light, catch the baked drop,
    pick it up, and verify the baked label + resource (the bake_bread
    choreography, keyed to any BAKE_MAP dough)."""
    branch = c.find_item_by_res(BRANCH_INV)
    assert branch is not None, "no branch left for fuel"
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, oven)
    c.pump(0.5)
    assert c.return_cursor(), "cursor return after fuel"
    c.pump(0.3)

    dough_wid = c.find_item_by_res(dough_inv)
    assert dough_wid is not None, "dough input missing"
    c.take_item(dough_wid)
    c.pump(0.3)
    c.map_itemact(mc, oven)
    c.pump(0.5)
    assert c.return_cursor(), "cursor return after the dough load"
    c.pump(0.3)

    seen = {g for g, info in c.gobs.items()
            if info["res"] == world_res and not info.get("removed")}
    c.click_gob(oven, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Light"], 4)
    assert ok, "oven Light menu missing (opts=%s)" % (c.sm_opts,)
    c.flower_choice(c.sm_wid, 0)
    ok = c.wait_for(lambda: c.gobs[oven]["sdt"] == b"\x01", 4)
    assert ok, "oven never re-rendered as lit (sdt=%r lines=%r)" % (
        c.gobs[oven]["sdt"], c.chat_lines[-6:],)
    print("oven lit; waiting out the %s bake..." % what)

    ok = c.wait_for(
        lambda: any(
            g not in seen and not info.get("removed")
            for g, info in c.gobs.items()
            if info["res"] == world_res), 15)
    assert ok, "no %s drop appeared beside the oven" % what
    drop_id = sorted(
        g for g, info in c.gobs.items()
        if info["res"] == world_res and not info.get("removed")
        and g not in seen)[0]
    c.click_gob(drop_id, c.gobs[drop_id]["pos"])
    ok = c.wait_for(
        lambda: c.find_item_by_tooltip(label) is not None, 6)
    assert ok, "the %s never reached the inventory (items=%s)" % (
        what, [(i["res"], i["tt"]) for i in c.item_info.values()],)
    out_wid = c.find_item_by_tooltip(label)
    info = c.item_info[out_wid]
    assert info["tt"] == label, "%s label %r (want %r)" % (
        what, info["tt"], label)
    print("%s BAKED: %s q%d" % (what.upper(), label, info["ql"]))
    return out_wid


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
        # boots makes the wheat leg minutes, not hours). The leg hands
        # its quern + oven back for the session-83 doughs.
        quern, oven = blueberry_pie_leg(c)

        # 7. HONEYBUN: the no-butter dough over the hive chain (water +
        # honey buckets), baked in the same oven, eaten (AGI:5).
        honeybun_leg(c, quern, oven)

        # 8. PIROZHKI: the savory dough over the forage legs (shrooms +
        # onions), baked in the same oven, eaten (CON:4 DEX:4).
        pirozhki_leg(c, quern, oven)

        print("DOUGH: OK (%s: 3 grapes, 5 apples, honey, raisins,"
              " apple eat, blueberry pie + honeybun + pirozhki baked"
              " + eaten)" % username)
    finally:
        from hnhlib import stop_server
        if proc is not None:
            stop_server(proc)


if __name__ == "__main__":
    sys.exit(main())
