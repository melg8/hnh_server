#!/usr/bin/env python3
"""Session 66 metal-chain probe on the hnhlib harness.

Drives the full ore -> smelter -> bar contract against a live server:

  1. MINING: walk to the rocky belt on the dev seed (the world-design
     test guarantees rocky terrain within ~26 tiles of the spawn area on
     seed 42), find an ore deposit (gfx/terobjs/mining/heap - the sprite
     that distinguishes deposits from plain boulders), click it, and
     wait for an ore drop (nugget-copper / nugget-tin / ore-iron) to
     land in the inventory.
  2. BUILD: place a smelter plan (gfx/terobjs/smelter) on a free tile,
     sink stone x6 + branch x4 (both covered by the starter kit), and
     wait for the plan to complete into a station gob.
  3. SMELT: itemact branch (fuel), itemact the mined ore (input),
     flower-menu Light, wait out the 30-tick job, catch the bar drop
     (gfx/terobjs/items/bar-*), pick it up, and verify the label matches
     the craft::SMELT_MAP mapping for the ore that went in.

Prints "SMELT: OK ..." on success.

Usage: python3 test_smelt.py [username]
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import ensure_server, enter_world  # noqa: E402

DEPOSIT_RES = "gfx/terobjs/mining/heap"
SMELTER_RES = "gfx/terobjs/smelter"
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
# The dev-seed rocky belt centers on tile (24, 24) on seed 42 (measured
# by the hnh-world rocky_terrain_is_reachable_from_spawn test).
BELT_TILE = (24, 24)
HOP = 66


def hop(c, dx, dy, seconds=2.0):
    pos = c.gobs[c.player_gob]["pos"]
    c.click_ground(pos[0] + dx, pos[1] + dy)
    c.pump(seconds)


def approach(c, target, stop=200, tries=30):
    """Walk within `stop` subtiles of `target` (Manhattan)."""

    def step(v):
        return max(-HOP, min(HOP, v))

    for _ in range(tries):
        pos = c.gobs[c.player_gob]["pos"]
        dx, dy = target[0] - pos[0], target[1] - pos[1]
        if abs(dx) + abs(dy) <= stop:
            return True
        c.click_ground(pos[0] + step(dx), pos[1] + step(dy))
        c.pump(2.0)
    return False


def find_deposits(c):
    return [
        (g, info)
        for g, info in c.gobs.items()
        if info["res"] == DEPOSIT_RES and not info.get("removed")
    ]


ORE_ITEM_PREFIXES = (
    "gfx/terobjs/items/nugget",
    "gfx/terobjs/items/ore",
)


def mine_one_ore(c):
    """Walk to the belt, find a deposit, pick it once, return the ore
    resource that landed in the inventory.

    Contract shape mirrors the gathering probe: the pick spawns a
    drop gob (the world shape, possibly through DROP_WORLD_ALIASES),
    and a SECOND click on that drop lands the ore in the inventory.
    """
    target = (BELT_TILE[0] * 11, BELT_TILE[1] * 11)
    deposits = find_deposits(c)

    def nearest(deps):
        """The deposit closest to the belt target (gob id order is
        grid-generation order, NOT distance - the first run proved a
        far-corner deposit can carry the smallest id)."""

        def dist(entry):
            pos = entry[1]["pos"]
            return abs(pos[0] - target[0]) + abs(pos[1] - target[1])

        return min(deps, key=dist)

    for i in range(30):
        if deposits:
            break
        pos = c.gobs[c.player_gob]["pos"]
        dx, dy = target[0] - pos[0], target[1] - pos[1]
        if abs(dx) + abs(dy) > HOP:
            hop(c, max(-HOP, min(HOP, dx)), max(-HOP, min(HOP, dy)))
        else:
            # On the belt: scan sideways until a deposit streams in.
            hop(c, HOP if i % 2 == 0 else -HOP, 0)
        deposits = find_deposits(c)
    assert deposits, "no ore deposit streamed into view near the belt"
    dep_id, dep_info = nearest(deposits)
    print("deposit found: %s at %s" % (dep_info["res"], dep_info["pos"]))
    assert approach(c, dep_info["pos"]), "could not reach the deposit"

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
    return ore_res


def build_smelter(c):
    """Place the smelter plan on a free tile and sink the starter-kit
    materials (stone x6 + branch x4)."""
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)
    plan = None
    mc = None
    for dx in range(-3, 4):
        cand = (ptile[0] + dx, ptile[1] + 1)
        cmc = (cand[0] * 11 + 5, cand[1] * 11 + 5)
        c.menu_act("smelter")
        ok = c.wait_for(lambda: c.place_seen is not None, 5)
        assert ok, "place uimsg missing for the smelter pagina"
        c.place_seen = None
        c.send_place(cmc, 1, 0)
        if c.wait_for(
            lambda: any(
                info["res"] == SMELTER_RES and info["pos"] == cmc
                for info in c.gobs.values()
            ),
            2.5,
        ):
            mc = cmc
            plan = next(
                g
                for g, info in c.gobs.items()
                if info["res"] == SMELTER_RES and info["pos"] == cmc
            )
            break
    assert plan is not None, "no free tile accepted a smelter plan"
    print("smelter plan placed:", plan)

    def sink(resname):
        stack = c.find_item_by_res(resname)
        assert stack is not None, "material missing: %s" % resname
        c.take_item(stack)
        c.pump(0.3)
        c.map_itemact(mc, plan)
        c.pump(0.5)
        assert c.return_cursor(), "inventory window missing for cursor return"
        c.pump(0.3)

    # Demand: stone x6 (starter kit carries exactly 6) + branch x4.
    # Stage choreography: stones cross 6/10 = stage 1 (sdt 1), the branch
    # sink completes the plan (the station's unlit sdt drops back to 0).
    sink("gfx/invobjs/stone")
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 6)
    assert ok, "stage never advanced after stone sink (sdt=%r)" % (
        c.gobs[plan]["sdt"],)
    sink("gfx/invobjs/branch")
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x00", 6)
    assert ok, "smelter plan never completed (sdt=%r)" % (c.gobs[plan]["sdt"],)
    print("smelter completed:", plan)
    return plan, mc


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
    """Fuel, load the ore, light, and catch the bar drop."""
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
    ok = c.wait_for(lambda: bool(bar_drops(c)), 15)
    assert ok, "no bar drop appeared beside the smelter"
    drop_id = sorted(bar_drops(c).keys())[0]
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
        print("in world; driving the metal-chain contract")
        ore_res = mine_one_ore(c)
        plan, mc = build_smelter(c)
        label = smelt(c, plan, mc, ore_res)
        refine_to_wrought_iron(c, label)
        c.sock.close()
        print("SMELT: OK (ore pick, smelter build, %s smelted)" % label)
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


if __name__ == "__main__":
    main()
