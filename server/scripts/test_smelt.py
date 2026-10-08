#!/usr/bin/env python3
"""Session 66/67 metal-chain probe on the hnhlib harness.

Drives the full ore -> smelter -> bar -> crucible -> bronze contract
against a live server:

  1. MINING: walk to the rocky belt on the dev seed (the world-design
     test guarantees rocky terrain within ~26 tiles of the spawn area on
     seed 42), find ore deposits (gfx/terobjs/mining/heap - the sprite
     that distinguishes deposits from plain boulders), click each one,
     and wait for ore drops to land in the inventory until BOTH a
     copper and a tin source are held (deposits carry one random kind
     each; the per-tile mix is 5 copper : 3 tin : 2 iron).
  2. BUILD: place a smelter plan (gfx/terobjs/smelter) on a free tile,
     sink stone x6 + branch x4 (both covered by the starter kit), and
     wait for the plan to complete into a station gob.
  3. SMELT: itemact branch (fuel), itemact the mined ore (input),
     flower-menu Light, wait out the 30-tick job, catch the bar drop
     (gfx/terobjs/items/bar-*), pick it up, and verify the label matches
     the craft::SMELT_MAP mapping for the ore that went in. Runs once
     per ore kind (copper AND tin): the bronze charge needs both bars.
  4. TOP-UP (session 67): the starter kit's stone/branch budget does
     not cover a SECOND station, so gather the alloyer's demand from
     the world: boulder-pick stones (BOULDER_STONES x5 per boulder) and
     tree-pick branches while walking north toward the forest.
  5. ALLOY (session 67): place an Alloying Crucible (gfx/terobjs/
     alloyer, stone x4 + branch x4), fuel it, itemact the Bar of Copper
     (input slot) and the Bar of Tin (aux slot), Light, wait out the
     30-tick job, catch the TWO Bar of Bronze drops (world shape rides
     the bar-copper alias), pick both up, and verify the inventory
     stack holds count = 2 (grant_pickup merges same-resource stacks).

Prints "SMELT: OK ..." on success.

Usage: python3 test_smelt.py [username]
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import ensure_server, enter_world  # noqa: E402

DEPOSIT_RES = "gfx/terobjs/mining/heap"
SMELTER_RES = "gfx/terobjs/smelter"
ALLOYER_RES = "gfx/terobjs/alloyer"
ORE_ITEMS = {
    "gfx/invobjs/nugget-copper": "Copper Nugget",
    "gfx/invobjs/nugget-tin": "Tin Nugget",
    "gfx/invobjs/ore-iron": "Iron Ore",
}
ORE_TO_BAR = {
    "Copper Nugget": "Bar of Copper",
    "Tin Nugget": "Bar of Tin",
    "Iron Ore": "Bar of Cast Iron",
}
# The bronze charge: one bar of each input kind.
CHARGE_INPUTS = ("Copper Nugget", "Tin Nugget")
BRONZE_LABEL = "Bar of Bronze"
BRONZE_COUNT = 2
BOULDER_PREFIX = "gfx/terobjs/bumlings/"
TREE_PREFIX = "gfx/terobjs/trees/"
STUMP_RES = "gfx/terobjs/trees/log"
BRANCH_WORLD = "gfx/terobjs/items/branch"
STONE_WORLD = "gfx/terobjs/items/stone"
# The dev-seed rocky belt centers on tile (24, 24) on seed 42 (measured
# by the hnh-world rocky_terrain_is_reachable_from_spawn test).
BELT_TILE = (24, 24)
HOP = 66








def find_deposits(c, used=()):
    return [
        (g, info)
        for g, info in c.gobs.items()
        if info["res"] == DEPOSIT_RES and not info.get("removed")
        and g not in used
    ]


def find_by_prefix(c, prefix, exclude=()):
    return [
        (g, info)
        for g, info in c.gobs.items()
        if info["res"]
        and info["res"].startswith(prefix)
        and not any(info["res"].startswith(e) for e in exclude)
        and not info.get("removed")
    ]


ORE_ITEM_PREFIXES = (
    "gfx/terobjs/items/nugget",
    "gfx/terobjs/items/ore",
)



def mine_one_ore(c, used):
    """Walk to the belt, find a FRESH deposit (not in `used`), pick it
    once, return the ore resource that landed in the inventory.

    Contract shape mirrors the gathering probe: the pick spawns a
    drop gob (the world shape, possibly through DROP_WORLD_ALIASES),
    and a SECOND click on that drop lands the ore in the inventory.

    Session 67 navigation fix: the server only accepts LinMove clicks
    whose whole segment is walkable (state::path_clear), so the old
    blind toward-the-deposit hops stalled at the first ridge. The
    probe now BFS-paths over the streamed tile grid (hnhlib's
    nav_walk) and clicks only lines the server will take; deposits
    with no tile path are skipped for good.
    """
    target = (BELT_TILE[0] * 11, BELT_TILE[1] * 11)

    def by_distance(deps):
        def dist(entry):
            pos = entry[1]["pos"]
            return abs(pos[0] - target[0]) + abs(pos[1] - target[1])

        return sorted(deps, key=dist)

    dep_id = None
    dep_info = None
    for i in range(6):
        skipped = getattr(c, "skipped_deposits", set())
        deposits = find_deposits(c, used | skipped)
        print("scan %d: pos=%s deps=%d skipped=%d grids=%d" % (
            i, c.gobs[c.player_gob]["pos"], len(deposits), len(skipped),
            len(c.tiles)))
        for cand_id, cand_info in by_distance(deposits):
            if not c.nav_walk(
                cand_info["pos"], stop=60, max_clicks=80, log=print
            ):
                c.skipped_deposits = skipped
                c.skipped_deposits.add(cand_id)
                continue
            dep_id, dep_info = cand_id, cand_info
            break
        if dep_id is not None:
            break
        if skipped and not deposits:
            # Every streamed deposit is either used or pathless: more
            # scans change nothing (the streamed grid set is fixed).
            break
    assert dep_id is not None, "no reachable ore deposit near the belt"
    print("deposit found: %s at %s" % (dep_info["res"], dep_info["pos"]))

    # Pick: click the deposit, catch the fresh ore drop, click it.
    seen = set(c.gobs.keys())
    c.click_gob(dep_id, c.gobs[dep_id]["pos"])

    def fresh_ore_drops():
        return [
            (g, info)
            for g, info in c.gobs.items()
            if info["res"].startswith(ORE_ITEM_PREFIXES)
            and not info.get("removed")
            and g not in seen
        ]

    ok = c.wait_for(lambda: bool(fresh_ore_drops()), 8)
    assert ok, "no ore drop spawned after the deposit pick"
    drop_id, drop_info = sorted(fresh_ore_drops())[0]
    print("ore drop spawned: %s at %s" % (drop_info["res"], drop_info["pos"]))
    c.click_gob(drop_id, c.gobs[drop_id]["pos"])

    ok = c.wait_for(
        lambda: any(c.find_item_by_res(res) is not None for res in ORE_ITEMS), 8
    )
    assert ok, "ore never landed in the inventory"
    ore_res = next(res for res in ORE_ITEMS if c.find_item_by_res(res) is not None)
    print("ORE PICK: OK (%s)" % ORE_ITEMS[ore_res])
    return ore_res, dep_id


def mine_until(c, wanted_labels):
    """Pick fresh deposits until the inventory holds at least one ore
    of every kind in `wanted_labels`. Deposits carry one random kind
    each (5:3:2 per-tile mix), so the kinds accumulate across picks;
    `used` keeps the probe from re-picking a deposit already sampled
    (each deposit survives ORE_PICKS = 4 picks)."""
    have = set()
    used = set()
    for _ in range(8):
        missing = [l for l in wanted_labels if l not in have]
        if not missing:
            return have
        ore_res, dep_id = mine_one_ore(c, used)
        used.add(dep_id)
        have.add(ORE_ITEMS[ore_res])
        print("kinds mined so far: %s (still need %s)" % (sorted(have), missing))
    raise AssertionError("never mined all of %s" % sorted(wanted_labels))


def build_station(c, ad, res_name, demand):
    """Place a station plan on a free tile and sink the demand stacks.
    `demand` is a list of (inv res, count) sunk in order; each sink
    asserts the plan's sdt stage byte advanced (partial stage) or the
    plan completed into the station (sdt back to 0)."""
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)
    plan = None
    mc = None
    for dx in range(-3, 4):
        cand = (ptile[0] + dx, ptile[1] + 1)
        cmc = (cand[0] * 11 + 5, cand[1] * 11 + 5)
        c.menu_act(ad)
        ok = c.wait_for(lambda: c.place_seen is not None, 5)
        assert ok, "place uimsg missing for the %s pagina" % ad
        c.place_seen = None
        c.send_place(cmc, 1, 0)
        if c.wait_for(
            lambda: any(
                info["res"] == res_name and info["pos"] == cmc
                for info in c.gobs.values()
            ),
            2.5,
        ):
            mc = cmc
            plan = next(
                g
                for g, info in c.gobs.items()
                if info["res"] == res_name and info["pos"] == cmc
            )
            break
    assert plan is not None, "no free tile accepted a %s plan" % ad
    print("%s plan placed: %s" % (ad, plan))

    # Plans spawn with stage-0 sdt; each material batch must move the
    # stage byte (partial stage for early batches, back to 0 when the
    # demand completes). Waiting for the CHANGE (not a fixed value)
    # keeps the probe independent of the station's stage count.
    last_sdt = c.gobs[plan]["sdt"]
    for resname, count in demand:
        for _ in range(count):
            stack = c.find_item_by_res(resname)
            assert stack is not None, "material missing: %s" % resname
            c.take_item(stack)
            c.pump(0.3)
            c.map_itemact(mc, plan)
            c.pump(0.5)
            assert c.return_cursor(), "inventory window missing for cursor return"
            c.pump(0.3)
        ok = c.wait_for(lambda: c.gobs[plan]["sdt"] != last_sdt, 10)
        assert ok, "%s stage never advanced after sinking %s (sdt=%r)" % (
            ad, resname, c.gobs[plan]["sdt"])
        last_sdt = c.gobs[plan]["sdt"]
        print("%s stage after %s: sdt=%r" % (ad, resname, list(last_sdt)))
    assert last_sdt == b"\x00", "%s never completed (sdt=%r)" % (ad, last_sdt)
    print("%s completed: %s" % (ad, plan))
    return plan, mc


def build_smelter(c):
    """Smelter: stone x6 + branch x4 (the starter kit's whole stone
    supply). Stage choreography: stones cross 6/10 = stage 1 (sdt 1),
    the branch sink completes the plan."""
    return build_station(
        c, "smelter", SMELTER_RES,
        [("gfx/invobjs/stone", 6), ("gfx/invobjs/branch", 4)],
    )


def build_alloyer(c):
    """Alloying Crucible: stone x4 + branch x4 (gathered, not kit)."""
    return build_station(
        c, "alloyer", ALLOYER_RES,
        [("gfx/invobjs/stone", 4), ("gfx/invobjs/branch", 4)],
    )


def bar_drops(c):
    """All gobs rendering a bar world shape (exact res, no prefix helper
    in the harness: scan gobs once)."""
    return {
        g: info
        for g, info in c.gobs.items()
        if info["res"].startswith("gfx/terobjs/items/bar-")
        and not info.get("removed")
    }


def smelt(c, plan, mc, ore_res):
    """Fuel, load the ore, light, and catch the bar drop. One call per
    ore kind; the previous call's drop is picked up (removed), so the
    fresh-drop scan is unambiguous."""
    # Fuel: one branch unit per itemact.
    branch = c.find_item_by_res("gfx/invobjs/branch")
    assert branch is not None, "no branch left for fuel"
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    c.pump(0.5)
    assert c.return_cursor(), "cursor return after fuel"
    c.pump(0.3)

    # Input: the mined ore stack.
    ore_wid = c.find_item_by_res(ore_res)
    assert ore_wid is not None, "ore stack missing: %s" % ore_res
    c.take_item(ore_wid)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    c.pump(0.5)

    # Light through the flower menu.
    c.click_gob(plan, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Light"], 4)
    assert ok, "smelter Light menu missing (opts=%s)" % (c.sm_opts,)
    c.flower_choice(c.sm_wid, 0)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "smelter never re-rendered as lit (sdt=%r)" % (c.gobs[plan]["sdt"],)
    print("smelter lit; waiting out the 30-tick job...")

    # Output: a bar drop appears (bar world shapes render through the
    # own-terobjs-shape or the sibling-metal alias).
    seen = set(bar_drops(c).keys())
    ok = c.wait_for(lambda: any(g not in seen for g in bar_drops(c)), 15)
    assert ok, "no bar drop appeared beside the smelter"
    fresh = [g for g in bar_drops(c) if g not in seen]
    drop_id = sorted(fresh)[0]
    bar_res = c.gobs[drop_id]["res"]
    print("bar drop:", bar_res, "at", c.gobs[drop_id]["pos"])

    c.click_gob(drop_id, c.gobs[drop_id]["pos"])
    want_label = ORE_TO_BAR[ORE_ITEMS[ore_res]]
    ok = c.wait_for(lambda: c.find_item_by_tooltip(want_label) is not None, 6)
    assert ok, "%s never reached the inventory (items=%s)" % (
        want_label,
        [(i["res"], i["tt"]) for i in c.item_info.values()],
    )
    out_wid = c.find_item_by_tooltip(want_label)
    print("bar quality:", c.item_info[out_wid]["ql"])
    return want_label


def inv_total(c, resname):
    """Total count across stacks for one inventory resource."""
    return sum(
        info.get("count", 1)
        for info in c.item_info.values()
        if info["res"] == resname
    )


def inv_label_total(c, label):
    """Total count across stacks whose tooltip matches `label`."""
    return sum(
        info.get("count", 1)
        for info in c.item_info.values()
        if info["tt"] == label
    )


def pick_drop_into_inv(c, gid, inv_res):
    """Click one drop gob and assert its resource reached the
    inventory (count grows)."""
    before = inv_total(c, inv_res)
    c.click_gob(gid, c.gobs[gid]["pos"])
    ok = c.wait_for(lambda: inv_total(c, inv_res) > before, 8)
    assert ok, "%s never landed in the inventory" % inv_res


def gather_topup(c, need_stone, need_branch):
    """Session 67: gather the crucible's build demand from the world.

    The starter kit's stone (6) went into the smelter and the branch
    budget (10) covers the smelter build + two fuel units, leaving
    exactly 4 branch - one short of the crucible's stone 4 + branch 4
    + fuel 1 demand. Boulder-pick the stones (BOULDER_STONES = 5 per
    boulder) and tree-pick the branches while walking north toward the
    seed-42 forest (grid (0, -1), ~90 tiles north of the spawn area).
    """
    seen = set(c.gobs.keys())

    def stones():
        return inv_total(c, "gfx/invobjs/stone")

    def branches():
        return inv_total(c, "gfx/invobjs/branch")

    for i in range(40):
        if stones() >= need_stone and branches() >= need_branch:
            print("TOP-UP: OK (stone=%d branch=%d)" % (stones(), branches()))
            return
        # Prefer whatever the viewport already offers.
        if stones() < need_stone:
            boulders = find_by_prefix(c, BOULDER_PREFIX)
            if boulders:
                bid, binfo = sorted(boulders)[0]
                print("boulder found: %s at %s" % (binfo["res"], binfo["pos"]))
                assert c.nav_walk(binfo["pos"], stop=80, max_clicks=60), (
                    "could not reach the boulder")
                for _ in range(need_stone - stones()):
                    assert not c.gobs[bid].get("removed"), (
                        "boulder retracted before the stone quota")
                    seen_before = set(c.gobs.keys())
                    c.click_gob(bid, c.gobs[bid]["pos"])

                    def fresh_stones():
                        return [
                            (g, info)
                            for g, info in c.gobs.items()
                            if info["res"] == STONE_WORLD
                            and not info.get("removed")
                            and g not in seen_before
                        ]

                    ok = c.wait_for(lambda: bool(fresh_stones()), 8)
                    assert ok, "no stone drop spawned after the pick"
                    gid, _ = sorted(fresh_stones())[0]
                    pick_drop_into_inv(c, gid, "gfx/invobjs/stone")
                    print("STONE PICK: %d" % stones())
                continue
        if branches() < need_branch:
            trees = find_by_prefix(c, TREE_PREFIX, exclude=(STUMP_RES,))
            if trees:
                tid, tinfo = sorted(trees)[0]
                print("tree found: %s at %s" % (tinfo["res"], tinfo["pos"]))
                assert c.nav_walk(tinfo["pos"], stop=80, max_clicks=60), (
                    "could not reach the tree")
                for _ in range(need_branch - branches()):
                    seen_before = set(c.gobs.keys())
                    c.click_gob(tid, c.gobs[tid]["pos"])

                    def fresh_branches():
                        return [
                            (g, info)
                            for g, info in c.gobs.items()
                            if info["res"] == BRANCH_WORLD
                            and not info.get("removed")
                            and g not in seen_before
                        ]

                    ok = c.wait_for(lambda: bool(fresh_branches()), 8)
                    assert ok, "no branch drop spawned after the pick"
                    gid, _ = sorted(fresh_branches())[0]
                    pick_drop_into_inv(c, gid, "gfx/invobjs/branch")
                    print("BRANCH PICK: %d" % branches())
                continue
        # Nothing usable in view: walk north (the forest sits north on
        # seed 42; boulders stream in across the grass too).
        pos = c.gobs[c.player_gob]["pos"]
        c.nav_walk((pos[0], pos[1] - 500), stop=150, max_clicks=10)
    raise AssertionError(
        "top-up failed (stone=%d/%d branch=%d/%d)" % (
            stones(), need_stone, branches(), need_branch))


def alloy(c, plan, mc):
    """Session 67: charge the crucible, light it, catch both bronze
    drops. Charge order: fuel first (fuel is accepted while unlit and
    is the first gate on Light), then the copper bar (input slot),
    then the tin bar (aux slot)."""
    # Fuel: one branch unit.
    branch = c.find_item_by_res("gfx/invobjs/branch")
    assert branch is not None, "no branch left for the crucible fuel"
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    c.pump(0.5)
    assert c.return_cursor(), "cursor return after fuel"
    c.pump(0.3)

    # Input slot: the copper bar.
    cu_wid = c.find_item_by_tooltip("Bar of Copper")
    assert cu_wid is not None, "Bar of Copper missing for the charge"
    c.take_item(cu_wid)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    c.pump(0.5)

    # Aux slot: the tin bar.
    tin_wid = c.find_item_by_tooltip("Bar of Tin")
    assert tin_wid is not None, "Bar of Tin missing for the charge"
    c.take_item(tin_wid)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    c.pump(0.5)

    # Light through the flower menu.
    c.click_gob(plan, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Light"], 4)
    assert ok, "crucible Light menu missing (opts=%s)" % (c.sm_opts,)
    c.flower_choice(c.sm_wid, 0)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "crucible never rendered as lit (sdt=%r)" % (
        c.gobs[plan]["sdt"],)
    print("crucible lit; waiting out the 30-tick job...")

    # Output: TWO bronze drops (ALLOY_OUT_COUNT). Both render through
    # the bar-copper world alias; the smelted copper/tin drops are long
    # picked up (removed), so a seen-set keyed before the light works.
    seen = set(bar_drops(c).keys())
    ok = c.wait_for(
        lambda: len([g for g in bar_drops(c) if g not in seen]) >= BRONZE_COUNT,
        15,
    )
    assert ok, "expected %d bronze drops, saw %d" % (
        BRONZE_COUNT, len([g for g in bar_drops(c) if g not in seen]))
    fresh = sorted(g for g in bar_drops(c) if g not in seen)
    for gid in fresh:
        print("bronze drop:", c.gobs[gid]["res"], "at", c.gobs[gid]["pos"])
        pick_drop_into_inv(c, gid, "gfx/invobjs/bar-bronze")

    ok = c.wait_for(
        lambda: inv_label_total(c, BRONZE_LABEL) >= BRONZE_COUNT, 6
    )
    assert ok, "bronze stack never reached count %d (items=%s)" % (
        BRONZE_COUNT,
        [(i["res"], i["tt"], i.get("count")) for i in c.item_info.values()],
    )
    print("BRONZE: OK (%d x %s in the inventory)" % (
        inv_label_total(c, BRONZE_LABEL), BRONZE_LABEL))


def refine_to_wrought_iron(c, bar_label):
    """Session 66 refinement leg: a cast-iron bar refines into wrought
    iron through the shipped bloom2wrought pagina (real make widget).
    Only runs when the smelter produced cast iron (the iron-ore leg);
    copper/tin bars stop at the smelting leg."""
    if bar_label != "Bar of Cast Iron":
        print("refinement skipped (%s is not iron)" % bar_label)
        return
    c.menu_act("craft", "wroughtiron")
    ok = c.wait_for(lambda: any(n == "make" for n in c.widgets.values()), 8)
    assert ok, "no make widget for wroughtiron"
    make_wid = max(w for w, n in c.widgets.items() if n == "make")
    c.pump(0.3)
    c.wdgmsg(make_wid, "make", bytes([1]) + (0).to_bytes(4, "little") + bytes([0]))
    ok = c.wait_for(
        lambda: c.find_item_by_res("gfx/invobjs/bar-wroughtiron") is not None, 8
    )
    assert ok, "wrought-iron bar never reached the inventory (items=%s)" % (
        [(i["res"], i["tt"]) for i in c.item_info.values()],
    )
    print("WROUGHT IRON: OK")


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "smelt"
    server_proc = ensure_server()
    try:
        c = enter_world(username)
        print("in world; driving the bronze-chain contract")
        mine_until(c, CHARGE_INPUTS)
        plan, mc = build_smelter(c)
        # Smelt each charge input: copper first (input slot leg), then
        # tin (the aux leg's bar). The wrought-iron refinement only
        # runs when the belt yielded iron instead.
        for label in CHARGE_INPUTS:
            res = next(r for r, l in ORE_ITEMS.items() if l == label)
            bar = smelt(c, plan, mc, res)
            print("SMELT %s: OK" % bar)
        gather_topup(c, need_stone=4, need_branch=5)
        aplan, amc = build_alloyer(c)
        alloy(c, aplan, amc)
        c.sock.close()
        print("SMELT: OK (ore picks, smelter build, copper + tin bars,"
              " crucible build, %d bronze)" % BRONZE_COUNT)
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


if __name__ == "__main__":
    main()
