#!/usr/bin/env python3
"""Cross-node drop authority transfer probe (session 35).

Closes the session-34 verification gap ("a gob spawned by a node on a
cell it does not own is never published to the cell's owner") with the
session-35 transfer path, driven end to end through a real 2-node
cluster (TCP mesh + real UDP):

  - builderbot homes on node 1: walks to the cell (4,2) corner - a
    node-1-owned cell whose FOUR axis neighbors all belong to node 0
    (grid_owner scoring is seed-independent) - and builds an oven
    through the REAL build flow at a RIM site (5 subtiles from the
    cell corner). The station output drop spawns with a +/-33 subtile
    jitter, so any negative-axis jitter lands it in a node-0 cell.
  - probebot homes on node 0 and drives the roast through the guest
    station relay (the session-34 flow: input meat, fuel branch, Light
    from the snapshot). The output drop that crosses the boundary must
    TRANSFER to node 0: the probe sees it as a LOCAL gob and picks it
    up with a plain click (no relay) - the exact interaction that was
    impossible before (the drop was invisible to every node-0 player).
  - fallback paths when the jitter keeps the drop inside the oven
    cell: a second roast cycle driven locally by the builder, then
    guest-tree chops near the cell rim (each chop spawns a wood drop
    with the same jitter; trees survive many chops).

Companion shell phase greps both node logs for the transfer pair
("drop authority transferred" on the spawner, "drop authority claimed"
on the owner).

Usage: probe_drop.py <username> [home_game_port] [auth_port]
                        [builder_game_port] [builder_auth_port]
Prints "DROP TRANSFER: OK ..." on success.
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from probe_station import (  # noqa: E402
    StationProbeClient,
    cell_of,
    co_pump,
    co_wait,
    enter_world,
    owner_of,
)

# The roast output drop renders with the gfx/terobjs/items world shape
# (drop_world_res); the restored stack carries gfx/invobjs/meat.
MEAT_DROP_RES = "gfx/terobjs/items/meat"
WOOD_DROP_RES = "gfx/terobjs/items/wood"


def build_oven_rim(c, observer, nodes, me):
    """Build an oven at the RIM of a node-1-owned cell whose axis
    neighbors all belong to node 0 (any outward jitter lands the output
    drop in a peer cell). Returns (gob_id, mc) of the finished station."""
    pair = [c, observer]
    # Cell (4, 2) is node-1-owned with all four axis neighbors node 0
    # (computed once with the grid_owner port; seed-independent). Its
    # corner tile puts the oven 5-10 subtiles from both cell edges.
    cell = (4, 2)
    assert owner_of(cell, nodes) == me, "planned cell must belong to the builder node"
    for (nx, ny) in ((3, 2), (5, 2), (4, 1), (4, 3)):
        assert owner_of((nx, ny), nodes) != me, "all axis neighbors must be peer cells"

    # Corner tile of the cell: first tile inside the cell on both axes
    # (subtile offset ~6 from each edge - inside the +/-33 jitter cone).
    corner_tile = (cell[0] * 250 // 11 + 1, cell[1] * 250 // 11 + 1)

    candidates = []
    for dy in range(0, 3):
        for dx in range(0, 3):
            site = (corner_tile[0] + dx, corner_tile[1] + dy)
            smc = (site[0] * 11 + 5, site[1] * 11 + 5)
            cc = cell_of(smc[0], smc[1])
            ox, oy = smc[0] - cc[0] * 250, smc[1] - cc[1] * 250
            if cc != cell or not (0 <= ox <= 33 and 0 <= oy <= 33):
                continue  # drifted off the rim corner
            candidates.append((ox + oy, site, smc))
    candidates.sort()
    assert candidates, "no rim-corner tile found in the planned cell"

    plan = None
    mc = None
    site = None
    for _, site, mc in candidates:
        staging = (site[0] - 2, site[1] - 2)
        stag_mc = (staging[0] * 11 + 5, staging[1] * 11 + 5)
        c.walk_to(stag_mc)

        def there():
            pos = c.player_pos()
            return pos is not None and abs(pos[0] - stag_mc[0]) <= 30 and abs(
                pos[1] - stag_mc[1]
            ) <= 30

        ok = co_wait(pair, there, 45, "builder walk to the rim corner")
        assert ok, "[%s] never reached the rim staging tile" % c.username

        c.menu_act("oven")
        ok = co_wait(pair, lambda: c.place_seen is not None, 5, "place uimsg")
        assert ok, "[%s] mapview never received the place uimsg" % c.username
        c.place_seen = None
        c.send_place(mc, 1, 0)

        def plan_up():
            return any(
                info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
                for info in c.gobs.values()
            )

        if co_wait(pair, plan_up, 3, "plan spawn"):
            plan = next(
                g
                for g, info in c.gobs.items()
                if info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
            )
            break
        print(
            "[%s] rim site %s refused (terrain/occupied), trying the next"
            % (c.username, site)
        )
    assert plan is not None, "no rim candidate site accepted an oven plan"
    print(
        "[%s] rim oven at %s (cell %s owner node%d, all axis neighbors peer) gob %s"
        % (c.username, mc, cell, me, plan)
    )

    stone = c.find_item_by_res("gfx/invobjs/stone")
    assert stone is not None, "[%s] starter stone missing" % c.username
    c.take_item(stone)
    co_pump(pair, 0.3)
    c.map_itemact(mc, plan)
    ok = co_wait(pair, lambda: c.gobs[plan]["sdt"] == b"\x01", 4, "stage 1")
    assert ok, "[%s] stage never advanced after the stone sink" % c.username
    # The kit's stone stack (6) exceeds the plan's stone demand (2): the
    # remainder stays on the drag cursor and blocks the branch take (one
    # cursor item at a time, game/items.rs inv_take; session-51 contract).
    assert c.return_cursor(), "[%s] inventory window missing for cursor return" % c.username
    co_pump(pair, 0.3)

    branch = c.find_item_by_res("gfx/invobjs/branch")
    assert branch is not None, "[%s] starter branch missing" % c.username
    c.take_item(branch)
    co_pump(pair, 0.3)
    c.map_itemact(mc, plan)
    ok = co_wait(pair, lambda: c.gobs[plan]["sdt"] == b"\x00", 6, "completion")
    assert ok, "[%s] oven never completed (sdt=%r)" % (c.username, c.gobs[plan]["sdt"])
    print("[%s] rim oven completed: gob %s" % (c.username, plan))
    # Same contract for the branch stack (6 vs the 1-unit demand line).
    assert c.return_cursor(), "[%s] inventory window missing for cursor return" % c.username
    co_pump(pair, 0.3)
    return plan, mc


def drive_roast(driver, pair, gob, mc, via_relay):
    """Fuel + input + light a station. via_relay=True drives the guest
    flow (itemact relays + Light flower menu from the snapshot); False
    is the builder's LOCAL flow. The builder's branch unit survives the
    oven build IN THE CURSOR (take is refused while a cursor is held),
    so its order is fuel-first (drain the cursor), then input; the
    probe starts with an empty cursor: input first, then fuel."""
    if via_relay:
        order = ("input", "fuel")
    else:
        order = ("fuel", "input")
    for step in order:
        if step == "fuel" and not via_relay:
            # The cursor already holds the surviving branch unit: the
            # bare itemact sinks it (no take needed).
            pass
        elif step == "fuel":
            branch = driver.find_item_by_res("gfx/invobjs/branch")
            assert branch is not None, "[%s] no branch left for fuel" % driver.username
            driver.take_item(branch)
            co_pump(pair, 0.3)
        else:
            meat = driver.find_item_by_res("gfx/invobjs/meat")
            assert meat is not None, "[%s] no meat left for a roast" % driver.username
            driver.take_item(meat)
            co_pump(pair, 0.3)
        driver.map_itemact(mc, gob)
        what = "Fuel added" if step == "fuel" else "Input loaded"
        ok = co_wait(
            pair,
            lambda w=what: any(w in t for t, _ in driver.chat_lines),
            6,
            "%s ack" % step,
        )
        assert ok, "[%s] %s never happened (chat=%s)" % (
            driver.username,
            step,
            driver.chat_lines,
        )

    driver.click_gob(gob, mc)
    ok = co_wait(
        pair,
        lambda: driver.sm_wid is not None and "Light" in driver.sm_opts,
        4,
        "light menu",
    )
    assert ok, "[%s] oven never offered the Light menu (opts=%s)" % (
        driver.username,
        driver.sm_opts,
    )
    driver.flower_choice(driver.sm_wid, driver.sm_opts.index("Light"))
    ok = co_wait(pair, lambda: driver.gobs[gob]["sdt"] == b"\x01", 6, "lit re-render")
    assert ok, "[%s] oven never lit (sdt=%r)" % (driver.username, driver.gobs[gob]["sdt"])
    print("[%s] roast lit (%s)" % (driver.username, "relay" if via_relay else "local"))


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "dropbot"
    game_port = int(sys.argv[2]) if len(sys.argv) > 2 else 1870
    auth_port = int(sys.argv[3]) if len(sys.argv) > 3 else 1871
    b_game_port = int(sys.argv[4]) if len(sys.argv) > 4 else 1882
    b_auth_port = int(sys.argv[5]) if len(sys.argv) > 5 else 1883
    nodes = int(sys.argv[6]) if len(sys.argv) > 6 else 2

    stamp = "%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    probe = StationProbeClient("probe" + stamp, game_port, auth_port)
    builder = StationProbeClient("build" + stamp, b_game_port, b_auth_port)
    enter_world(probe)
    enter_world(builder)
    pair = [probe, builder]

    # --- rim oven on the builder's node, all axis neighbors peer ------
    plan, mc = build_oven_rim(builder, probe, nodes, 1)
    probe_target = (mc[0] - 140, mc[1] - 140)
    probe.walk_to(probe_target)
    ok = co_wait(
        pair,
        lambda: (
            lambda p: p is not None
            and abs(p[0] - probe_target[0]) <= 40
            and abs(p[1] - probe_target[1]) <= 40
        )(probe.player_pos()),
        45,
        "probe approach",
    )
    assert ok, "probe never approached the rim oven"
    co_pump(pair, 1.5)
    ovens = probe.find_gobs("gfx/terobjs/oven")
    assert plan in ovens, (
        "probe never saw the guest rim oven (ids=%s want=%s)"
        % (sorted(ovens.keys()), plan)
    )
    print("[probe] guest rim oven visible: gob %s" % plan)

    # --- roast 1 through the relay (session-34 flow, regression) ------
    drive_roast(probe, pair, plan, mc, via_relay=True)
    drops_before = set(probe.find_gobs(MEAT_DROP_RES).keys())
    ok = co_wait(
        pair,
        lambda: len(set(probe.find_gobs(MEAT_DROP_RES).keys()) - drops_before) > 0,
        12,
        "roast output",
    )
    assert ok, "roast 1 produced no output drop (chat=%s)" % (probe.chat_lines[-4:],)
    drop = (set(probe.find_gobs(MEAT_DROP_RES).keys()) - drops_before).pop()
    drop_pos = probe.gobs[drop]["pos"]
    transferred = owner_of(cell_of(*drop_pos), nodes) == 0
    print(
        "[probe] roast 1 drop %s at %s cell %s owner node%d%s"
        % (
            drop,
            drop_pos,
            cell_of(*drop_pos),
            owner_of(cell_of(*drop_pos), nodes),
            " -> TRANSFERRED" if transferred else " (stayed on the authority)",
        )
    )
    if not transferred:
        # Not wasted: pick it up through the guest relay (session-30
        # regression) so the inventory keeps room for the real check.
        probe.click_gob(drop, drop_pos)
        co_pump(pair, 1.0)

    # --- roast 2 locally by the builder (second jitter roll) ----------
    if not transferred:
        drive_roast(builder, pair, plan, mc, via_relay=False)
        drops_before = set(probe.find_gobs(MEAT_DROP_RES).keys())
        ok = co_wait(
            pair,
            lambda: len(set(probe.find_gobs(MEAT_DROP_RES).keys()) - drops_before) > 0,
            12,
            "roast 2 output",
        )
        assert ok, "roast 2 produced no output drop (chat=%s)" % (probe.chat_lines[-4:],)
        drop = (set(probe.find_gobs(MEAT_DROP_RES).keys()) - drops_before).pop()
        drop_pos = probe.gobs[drop]["pos"]
        transferred = owner_of(cell_of(*drop_pos), nodes) == 0
        print(
            "[probe] roast 2 drop %s at %s cell %s owner node%d%s"
            % (
                drop,
                drop_pos,
                cell_of(*drop_pos),
                owner_of(cell_of(*drop_pos), nodes),
                " -> TRANSFERRED" if transferred else " (stayed on the authority)",
            )
        )
        if not transferred:
            probe.click_gob(drop, drop_pos)
            co_pump(pair, 1.0)

    # --- fallback: chop guest rim trees until a wood drop crosses ------
    path = "oven(roast1)" if transferred else "oven(roast2)"
    if not transferred:
        path = "tree-chop"
        wood_before = set(probe.find_gobs(WOOD_DROP_RES).keys())
        trees = {
            g: info
            for g, info in probe.gobs.items()
            if info.get("res", "").startswith("gfx/terobjs/trees/")
            and info.get("pos")
        }
        # Rim filter: a node-1 cell tree within 33 subtiles of an edge
        # whose axis neighbor belongs to node 0.
        def rim_distance(info):
            cx, cy = cell_of(*info["pos"])
            ox = info["pos"][0] - cx * 250
            oy = info["pos"][1] - cy * 250
            best = 999
            for edge, (nb, dist) in enumerate(
                (
                    ((cx - 1, cy), ox),
                    ((cx + 1, cy), 250 - 1 - ox),
                    ((cx, cy - 1), oy),
                    ((cx, cy + 1), 250 - 1 - oy),
                )
            ):
                if owner_of(nb, nodes) == 0 and dist < best:
                    best = dist
            _ = edge
            return best

        rim_trees = sorted(
            (
                (rim_distance(info), g, info)
                for g, info in trees.items()
                if rim_distance(info) <= 33
            ),
        )
        assert rim_trees, "no guest rim tree within jitter range of a peer cell"
        dist, tree, tree_info = rim_trees[0]
        print(
            "[probe] fallback chop tree %s at %s (%d subtiles from the peer edge)"
            % (tree, tree_info["pos"], dist)
        )
        for _chop in range(8):
            probe.click_gob(tree, tree_info["pos"])
            ok = co_wait(
                pair,
                lambda: len(set(probe.find_gobs(WOOD_DROP_RES).keys()) - wood_before) > 0,
                6,
                "chop wood drop",
            )
            if not ok:
                continue  # chop relay lost; retry
            new_wood = set(probe.find_gobs(WOOD_DROP_RES).keys()) - wood_before
            wood_before |= new_wood
            for d in new_wood:
                wpos = probe.gobs[d]["pos"]
                if owner_of(cell_of(*wpos), nodes) == 0:
                    drop, drop_pos = d, wpos
                    transferred = True
                    break
            if transferred:
                break
        assert transferred, "no wood drop ever crossed into a peer cell (8 chops)"

    # --- THE check: the boundary drop is a LOCAL gob on the probe's
    # node now - a plain click picks it up with no relay hop. Proof:
    # the restored stack appears in the probe's inventory (roasted
    # beef from the oven path, wood from the chop fallback).
    resname = probe.gobs[drop].get("res") or ""
    if resname == MEAT_DROP_RES:
        proof = lambda: probe.find_item_by_tooltip("Roasted Beef") is not None  # noqa: E731
        proof_what = "inventory stack 'Roasted Beef'"
        inv_res = "gfx/invobjs/meat"
    else:
        proof = lambda: probe.find_item_by_res("gfx/invobjs/wood") is not None  # noqa: E731
        proof_what = "inventory wood stack"
        inv_res = "gfx/invobjs/wood"
    probe.click_gob(drop, drop_pos)
    ok = co_wait(pair, proof, 6, "local pickup")
    assert ok, (
        "the boundary drop never restored %s (items=%s chat=%s)"
        % (proof_what, probe.item_info, probe.chat_lines[-4:])
    )
    print(
        "[probe] boundary drop %s picked up LOCALLY on node 0 (%s)"
        % (drop, proof_what)
    )
    print(
        "DROP TRANSFER: OK drop=%d cell=%s owner=node0 path=%s pos=%s inv=%s"
        % (drop, cell_of(*drop_pos), path, drop_pos, inv_res)
    )


if __name__ == "__main__":
    main()
