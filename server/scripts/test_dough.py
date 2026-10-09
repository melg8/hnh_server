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
    LIST_STR,
    REPO,
    RMSG_WDGMSG,
    WireClient,
    ensure_server,
    enter_world,
    havstr,
    le16,
    le32,
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


class DoughClient(WireClient):
    """WireClient + the `food` uimsg flag (the eat verdict)."""

    def __init__(self, username, **kw):
        super().__init__(username, **kw)
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
        return None


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


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "dough%d" % (
        int(time.time()) % 100000,)
    proc = ensure_server()
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

        print("DOUGH: OK (%s: 3 grapes, 5 apples, honey, raisins, eat)"
              % username)
    finally:
        from hnhlib import stop_server
        if proc is not None:
            stop_server(proc)


if __name__ == "__main__":
    sys.exit(main())
