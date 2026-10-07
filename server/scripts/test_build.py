#!/usr/bin/env python3
"""End-to-end building + station verification against a running server.

Flow (crafting-and-building.md, building pipeline + production stations):
  1. Auth + session + world entry (same wire path as test_client.py).
  2. act("oven") on the menugrid -> expect the mapview `place` uimsg
     (placement ghost drive).
  3. mapview wdgmsg `place` at a tile near the player -> expect the
     construction-plan gob (gfx/terobjs/oven, stage sdt 0).
  4. itemact with the held branch stack -> stage advance (sdt re-render).
  5. itemact with the held stone stack -> plan completes: the gob becomes
     the finished station; clicking it now offers the `Light` flower menu.
  6. Station flow: itemact branch (fuel), itemact meat (input), flower
     menu Light, wait for the tick job, pick up the output drop, verify
     the Roasted Beef label and the formula quality.

Exit 0 only when the whole chain passes; prints per-step diagnostics.
Modes: `buildbot` (steps 1-5, BUILD FLOW), `stationbot` (6, STATION
FLOW), `all` (both, default: buildbot first then a fresh character).
"""
import os
import subprocess
import sys
import time

# Shared wire harness: hnhlib.py is the single source of the transport
# plumbing (constants, auth, reliability walk, OBJDATA op table, session
# driver). This module keeps its historical CLI (buildbot/stationbot/
# persistbot/persistcheck modes) and re-exports the shared names so the
# probes that historically imported them from test_build keep working.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from hnhlib import (  # noqa: E402,F401
    REPO,
    BIN,
    GAME_PORT,
    AUTH_PORT,
    MSG_SESS,
    MSG_REL,
    MSG_ACK,
    MSG_BEAT,
    MSG_MAPREQ,
    MSG_MAPDATA,
    MSG_OBJDATA,
    MSG_OBJACK,
    MSG_CLOSE,
    RMSG_NEWWDG,
    RMSG_WDGMSG,
    RMSG_DSTWDG,
    RMSG_MAPIV,
    RMSG_GLOBLOB,
    RMSG_PAGINAE,
    RMSG_RESID,
    RMSG_PARTY,
    RMSG_SFX,
    RMSG_CATTR,
    RMSG_MUSIC,
    RMSG_TILES,
    RMSG_BUFF,
    OD_REM,
    OD_MOVE,
    OD_RES,
    OD_LINBEG,
    OD_LINSTEP,
    OD_SPEECH,
    OD_LAYERS,
    OD_DRAWOFF,
    OD_LUMIN,
    OD_AVATAR,
    OD_FOLLOW,
    OD_HOMING,
    OD_OVERLAY,
    OD_HEALTH,
    OD_BUDDY,
    OD_END,
    SESSERR_AUTH,
    PVER,
    LIST_END,
    LIST_INT,
    LIST_STR,
    LIST_COORD,
    LIST_COLOR,
    REQUIRED_CATTR,
    le16,
    le32,
    havstr,
    auth_cookie,
    ensure_server,
    parse_objdata,
    WireClient,
    enter_world,
    stop_server,
)

class BuildClient(WireClient):
    """Historical name: the build-flow probes keep their exact pre-chr
    request wire shape (the old BuildClient never sent `chr`)."""

    def __init__(self, username):
        super().__init__(username, request_chr=False)


def run_buildbot():
    username = "build%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    c = enter_world(username)
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)
    site = (ptile[0] + 1, ptile[1])  # one tile east

    # --- 1. build pagina -> placement ghost drive --------------------------
    c.menu_act("oven")
    ok = c.wait_for(lambda: c.place_seen is not None, 5)
    assert ok, "mapview never received the place uimsg"
    pres = c.place_seen[0] if c.place_seen else None
    assert pres == "gfx/terobjs/oven", "place uimsg names %r" % (pres,)
    ontile = c.place_seen[2] if len(c.place_seen) > 2 else None
    assert ontile == 1, "on-tile flag missing from place uimsg: %r" % (c.place_seen,)
    print("placement ghost driven:", c.place_seen)

    # --- 2. commit the placement -> plan gob --------------------------------
    mc = (site[0] * 11 + 5, site[1] * 11 + 5)
    c.send_place(mc, 1, 0)
    ok = c.wait_for(
        lambda: any(
            info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
            for info in c.gobs.values()
        ),
        5,
    )
    assert ok, "plan gob never spawned at the committed tile"
    plan = next(
        g for g, info in c.gobs.items()
        if info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
    )
    assert c.gobs[plan]["sdt"] == b"\x00", (
        "plan spawn must carry stage-0 sdt, got %r" % (c.gobs[plan]["sdt"],)
    )
    print("plan gob placed:", plan, "stage sdt", list(c.gobs[plan]["sdt"]))

    # --- 3. sink the stone stack: stage advances ----------------------------
    # Oven demand: stone x2 + branch x1 (3 units, 2 stages). Two stones
    # cross the 2/3 boundary: stage 0 -> 1 (sdt re-render).
    stone = c.find_item_by_res("gfx/invobjs/stone")
    assert stone is not None, "starter stone missing"
    c.take_item(stone)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "stage never advanced after stone sink (sdt=%r)" % (
        c.gobs[plan]["sdt"],)
    print("stones sunk: stage sdt ->", list(c.gobs[plan]["sdt"]))

    # --- 4. sink the branch stack: completion --------------------------------
    branch = c.find_item_by_res("gfx/invobjs/branch")
    assert branch is not None, "starter branch missing (items=%s)" % (
        [(i["res"], i["tt"]) for i in c.item_info.values()],)
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    # Completion converts the plan in place; wait for the sdt to drop back
    # to the station's unlit byte (0) after the stage-1 render.
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x00", 4)
    assert ok, "plan never completed (sdt=%r)" % (c.gobs[plan]["sdt"],)
    print("branch sunk: plan completed, gob", plan)

    # --- 5. the finished gob offers the Light flower menu -------------------
    c.click_gob(plan, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Light"], 4)
    assert ok, "station flower menu never offered Light (opts=%s)" % (c.sm_opts,)
    print("BUILD FLOW: OK")
    return c, plan, mc


def run_stationbot():
    username = "stat%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    c = enter_world(username)
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)

    # Build the oven first (same choreography as run_buildbot), scanning
    # candidate tiles because an earlier flow may own the first one.
    mc = None
    plan = None
    for dx in range(-2, 3):
        cand = (ptile[0] + dx, ptile[1] + 1)
        cmc = (cand[0] * 11 + 5, cand[1] * 11 + 5)
        c.menu_act("oven")
        ok = c.wait_for(lambda: c.place_seen is not None, 5)
        assert ok, "place uimsg missing"
        c.place_seen = None
        c.send_place(cmc, 1, 0)
        if c.wait_for(
            lambda: any(
                info["res"] == "gfx/terobjs/oven" and info["pos"] == cmc
                for info in c.gobs.values()
            ),
            2.5,
        ):
            mc = cmc
            plan = next(
                g for g, info in c.gobs.items()
                if info["res"] == "gfx/terobjs/oven" and info["pos"] == cmc
            )
            break
    assert plan is not None, "no free tile accepted an oven plan"
    stone = c.find_item_by_res("gfx/invobjs/stone")
    c.take_item(stone)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    # Wait out the stage-1 re-render: refresh_inventory recreates the item
    # widgets, so the branch lookup below needs the post-refresh ids.
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "stage never advanced after stone sink (sdt=%r)" % (
        c.gobs[plan]["sdt"],)
    branch = c.find_item_by_res("gfx/invobjs/branch")
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x00" and c.gobs[plan]["sdt"] != b"\x01", 4)
    assert ok, "oven never completed"
    print("oven built:", plan)

    # --- station: fuel delivery --------------------------------------------
    # Oven demand consumed stone x2 + branch x1; the branch stack carried
    # two units, so the leftover branch is still on the cursor (take moves
    # the whole stack). The next itemact delivers it as fuel.
    c.map_itemact(mc, plan)
    c.pump(0.5)
    print("fuel delivered")

    # --- station: input delivery -------------------------------------------
    meat = c.find_item_by_res("gfx/invobjs/meat")
    assert meat is not None, "starter meat missing"
    c.take_item(meat)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    c.pump(0.5)
    print("input delivered")

    # --- station: light via the flower menu ---------------------------------
    c.click_gob(plan, mc)
    ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Light"], 4)
    assert ok, "Light menu missing before lighting (opts=%s)" % (c.sm_opts,)
    c.flower_choice(c.sm_wid, 0)
    # The lit station re-renders with sdt byte 1.
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "station never re-rendered as lit (sdt=%r)" % (c.gobs[plan]["sdt"],)
    print("station lit; waiting for the tick job...")

    # --- output: the roast drop appears beside the oven ---------------------
    # The drop GOB renders with the gfx/terobjs/items world shape
    # (drop_world_res: inventory icon resources lack a `neg` layer),
    # so the world-gob search keys on the terobjs resource.
    drops_before = set(c.find_gobs("gfx/terobjs/items/meat").keys())
    ok = c.wait_for(
        lambda: len(set(c.find_gobs("gfx/terobjs/items/meat").keys()) - drops_before) > 0,
        10,
    )
    assert ok, "no output drop appeared"
    drop_id = (set(c.find_gobs("gfx/terobjs/items/meat").keys()) - drops_before).pop()
    drop_pos = c.gobs[drop_id]["pos"]
    print("output drop:", drop_id, "at", drop_pos)

    # --- pick up the output and verify label + quality ----------------------
    c.click_gob(drop_id, drop_pos)
    ok = c.wait_for(lambda: c.find_item_by_tooltip("Roasted Beef") is not None, 5)
    assert ok, "roasted output never reached the inventory (items=%s)" % (
        [(i["res"], i["tt"]) for i in c.item_info.values()],)
    out_wid = c.find_item_by_tooltip("Roasted Beef")
    ql = c.item_info[out_wid]["ql"]
    # Formula: (2*q_item + q_station + q_fuel)/4 = (2*10+10+10)/4 = 10.
    assert ql == 10, "output quality %r violates the station formula" % (ql,)
    print("STATION FLOW: OK")


def run_persistbot():
    """Place a plan and sink a partial delivery, then leave it half-built
    for the restart gate (verify_build.sh persist-build)."""
    username = "pers%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    c = enter_world(username)
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)
    mc = ((ptile[0] + 1) * 11 + 5, ptile[1] * 11 + 5)
    c.menu_act("oven")
    ok = c.wait_for(lambda: c.place_seen is not None, 5)
    assert ok, "place uimsg missing"
    c.send_place(mc, 1, 0)
    ok = c.wait_for(
        lambda: any(
            info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
            for info in c.gobs.values()
        ),
        5,
    )
    assert ok, "plan gob missing"
    plan = next(
        g for g, info in c.gobs.items()
        if info["res"] == "gfx/terobjs/oven" and info["pos"] == mc
    )
    stone = c.find_item_by_res("gfx/invobjs/stone")
    c.take_item(stone)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x01", 4)
    assert ok, "partial sink did not advance the stage"
    print("PERSIST: OK")


def run_persistcheck():
    """After a restart: find the restored half-built plan and finish it —
    only possible if the credited materials survived."""
    username = "perc%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    c = enter_world(username)
    # The restored plan carries the stage-1 sdt byte (stone x2 credited).
    ok = c.wait_for(
        lambda: any(
            info["res"] == "gfx/terobjs/oven" and info["sdt"] == b"\x01"
            for info in c.gobs.values()
        ),
        8,
    )
    assert ok, "no half-built oven plan visible after restart (gobs=%d)" % len(c.gobs)
    plan = next(
        g for g, info in c.gobs.items()
        if info["res"] == "gfx/terobjs/oven" and info["sdt"] == b"\x01"
    )
    mc = c.gobs[plan]["pos"]
    branch = c.find_item_by_res("gfx/invobjs/branch")
    c.take_item(branch)
    c.pump(0.3)
    c.map_itemact(mc, plan)
    ok = c.wait_for(lambda: c.gobs[plan]["sdt"] == b"\x00", 5)
    assert ok, "restored plan would not complete (sdt=%r)" % (c.gobs[plan]["sdt"],)
    print("PERSIST: OK")


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "all"
    server_proc = ensure_server()
    try:
        if mode == "buildbot":
            run_buildbot()
        elif mode == "stationbot":
            run_stationbot()
        elif mode == "persistbot":
            run_persistbot()
        elif mode == "persistcheck":
            run_persistcheck()
        else:
            run_buildbot()
            print("---")
            run_stationbot()
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


if __name__ == "__main__":
    main()
