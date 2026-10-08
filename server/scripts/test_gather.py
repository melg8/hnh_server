#!/usr/bin/env python3
"""Session 60 world-gathering probe on the hnhlib harness.

Drives the wire contract for bough/stone picking against a live server:

  1. Find a tree among the streamed statics, click it (the pick act),
     and wait for a branch drop (gfx/terobjs/items/branch) to spawn
     next to it. Click the drop -> the branch lands in the inventory as
     gfx/invobjs/branch. The tree SURVIVES the pick.
  2. Find a boulder (bumling), pick it BOULDER_STONES (= 5) times:
     every pick spawns ONE stone drop; after the last pick the boulder
     is retracted. All five stones land in the inventory.

Spawn layout note: fresh characters start on the open grass around tile
(50, 50) where boulders stream in view immediately, but the nearest
forest sits in grid (0, -1) - roughly 90 tiles north on seed 42 (the
seed ensure_server() always boots). The tree phase therefore walks
north until the forest streams into the 300-subtile view radius.

The same behaviors are pinned by the cargo unit battery
(relay_mine_boulder_yields_one_stone_per_pick, stump_pick_yields_nothing,
relay_chop_exhaustion_leaves_a_structure_class_stump); this probe is the
wire-contract half of the AGENTS.md pyramid.

Usage: python3 test_gather.py [username]
Prints "GATHER: OK ..." on success.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import enter_world  # noqa: E402

TREE_PREFIX = "gfx/terobjs/trees/"
STUMP_RES = "gfx/terobjs/trees/log"
BOULDER_PREFIX = "gfx/terobjs/bumlings/"
BRANCH_WORLD = "gfx/terobjs/items/branch"
STONE_WORLD = "gfx/terobjs/items/stone"
BOULDER_STONES = 5
# Per ground-click hop: 6 tiles, inside the path-check budget.
HOP = 66
NORTH_HOPS = 40


def find_by_prefix(c, prefix, exclude=()):
    return [
        (g, info)
        for g, info in c.gobs.items()
        if info["res"]
        and info["res"].startswith(prefix)
        and not any(info["res"].startswith(e) for e in exclude)
        and not info.get("removed")
    ]


def wait_for_drop(c, world_res, seen_before):
    """Wait for a NEW drop gob rendering world_res to appear."""

    def got():
        drops = [
            (g, info)
            for g, info in c.gobs.items()
            if info["res"] == world_res and not info.get("removed")
        ]
        return [d for d in drops if d[0] not in seen_before]

    ok = c.wait_for(lambda: bool(got()), 8)
    return got()[0] if ok else None


def pick_drop(c, seen, world_res, inv_res, label):
    """Click a fresh drop and assert it lands in the inventory."""
    found = wait_for_drop(c, world_res, seen)
    assert found, "no %s drop spawned" % world_res
    gid, info = found
    seen.add(gid)
    c.click_gob(gid, info["pos"])
    assert c.wait_for(lambda: c.find_item_by_res(inv_res) is not None, 8), (
        "%s did not land in the inventory" % inv_res
    )
    print("%s: OK" % label)


def hop(c, dx, dy, seconds=2.2):
    pos = c.gobs[c.player_gob]["pos"]
    c.click_ground(pos[0] + dx, pos[1] + dy)
    c.pump(seconds)


def approach(c, target, stop=220, tries=25):
    """Walk within `stop` subtiles of `target` (Manhattan). Drops spawn
    next to the object and only stream inside VIEW_RADIUS (= 300), so a
    pick click must happen from next to the object - the real client
    walks the character there before acting, and so does this probe."""

    def step(v):
        return max(-HOP, min(HOP, v))

    for _ in range(tries):
        pos = c.gobs[c.player_gob]["pos"]
        dx, dy = target[0] - pos[0], target[1] - pos[1]
        if abs(dx) + abs(dy) <= stop:
            return True
        c.click_ground(pos[0] + step(dx), pos[1] + step(dy))
        c.pump(2.2)
    return False


def find_tree(c):
    return find_by_prefix(c, TREE_PREFIX, exclude=(STUMP_RES,))


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "gather"
    c = enter_world(username)
    print("in world; driving the world-gathering contract")

    # 1) Branch picking: reach the forest, pick a tree, catch the drop.
    trees = find_tree(c)
    if not trees:
        print("no trees in view; walking north toward grid (0, -1)")
        for i in range(NORTH_HOPS):
            hop(c, 0, -HOP)
            trees = find_tree(c)
            if trees:
                break
    assert trees, "no tree streamed within walking range"
    tree_id, tree_info = sorted(trees)[0]
    print("tree found: %s at %s" % (tree_info["res"], tree_info["pos"]))
    assert approach(c, tree_info["pos"]), "could not reach the tree"

    seen = set()
    c.click_gob(tree_id, tree_info["pos"])
    pick_drop(c, seen, BRANCH_WORLD, "gfx/invobjs/branch", "BRANCH PICK")
    assert not c.gobs[tree_id].get("removed"), "tree must survive a pick"
    print("TREE SURVIVES: OK")

    # 2) Stone picking: BOULDER_STONES picks, then the boulder is gone.
    boulders = find_by_prefix(c, BOULDER_PREFIX)
    if not boulders:
        for _ in range(10):
            hop(c, HOP, 0)
            boulders = find_by_prefix(c, BOULDER_PREFIX)
            if boulders:
                break
    assert boulders, "no boulder streamed within walking range"
    bid, binfo = sorted(boulders)[0]
    print("boulder found: %s at %s" % (binfo["res"], binfo["pos"]))
    assert approach(c, binfo["pos"]), "could not reach the boulder"

    for i in range(BOULDER_STONES):
        assert not c.gobs[bid].get("removed"), "boulder retracted too early"
        c.click_gob(bid, c.gobs[bid]["pos"])
        pick_drop(c, seen, STONE_WORLD, "gfx/invobjs/stone", "STONE PICK %d" % (i + 1))
    c.pump(1.0)
    assert c.gobs[bid].get("removed"), "depleted boulder must be retracted"
    print("BOULDER DEPLETED: OK")

    c.sock.close()
    print("GATHER: OK (branch pick, tree survives, 5 stone picks, boulder gone)")


if __name__ == "__main__":
    main()
