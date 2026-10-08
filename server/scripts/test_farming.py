#!/usr/bin/env python3
"""End-to-end farming verification (docs/mechanics/livestock/farming-and-plants.md).

Chain under test (wire level, the same path the Java client drives):
  auth -> world entry -> Plow Field pagina act -> map click (tilth)
  -> inventory item "take" (cursor) -> mapview itemact (plant)
  -> crop gob spawn with sdt stage byte -> server-side stage growth
  (OD_RES re-sends) -> click crop -> Harvest flower menu -> yield items.

Auto-starts the server binary on an isolated save with a fast crop clock
(HNH_CROP_TIME_SCALE=10000000 -> 250 ms per stage) when 1871 is free;
otherwise reuses the already-running server (crop clock then follows its
configuration, so stages advance in minutes rather than milliseconds).

Modes: default runs the full plant-grow-harvest flow; `skillbot` verifies
the skill gate (planting refused without the Farming skill value, purchased
via the char sheet sattr contract at the legacy cost, then planting works,
unknown buys refused).

The transport, the session driver and the char-sheet/chat tracking live
in hnhlib.py.
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
    WireClient,
    enter_world,
    ensure_server,
    havstr,
    le32,
)


def ensure_farm_server():
    """Fast-crop-clock isolated server on a per-run save file."""
    return ensure_server(
        env_extra={"HNH_CROP_TIME_SCALE": "10000000"},
        save_path=os.path.join(
            REPO, "server", "target",
            "farm-test-save-%d.json" % (os.getpid() % 100000),
        ),
    )


class FarmClient(WireClient):
    """Farming scenario client.

    The historical scenario actions keep their tile-based signatures:
    click_tile/map_itemact take tile coords and convert to the tile's
    center subtile. Item tracking rides the shared item_info table.
    The char sheet stays closed until the flow asks for it (the old
    FarmClient never sent `chr` on slen bind).
    """

    def __init__(self, username):
        super().__init__(username, request_chr=False)

    def click_tile(self, tile):
        mc = (tile[0] * 11 + 5, tile[1] * 11 + 5)
        self.click_ground(*mc)

    def arm_plow(self):
        self.menu_act("plow")

    def map_itemact_tile(self, tile):
        mc = (tile[0] * 11 + 5, tile[1] * 11 + 5)
        self.map_itemact(mc)

    def find_item(self, tooltip):
        return self.find_item_by_tooltip(tooltip)


def buy_farming_value(c):
    """Raise the Farming skill value to 1 through the real char sheet
    contract (slen 'chr' -> sattr pairs). Returns the LP balance seen at
    sheet open."""
    slen_wid = next(w for w, n in c.widgets.items() if n == "slen")
    c.wdgmsg(slen_wid, "chr", bytes([LIST_END]))
    ok = c.wait_for(lambda: c.chr_id is not None and c.exp_seen is not None, 5)
    assert ok, "char sheet never opened (widgets=%s)" % sorted(set(c.widgets.values()))
    exp_before = c.exp_seen
    c.wdgmsg(
        c.chr_id,
        "sattr",
        bytes([LIST_STR]) + havstr("farming")
        + bytes([LIST_INT]) + le32(1)
        + bytes([LIST_END]),
    )
    ok = c.wait_for(lambda: c.attrs.get("farming", 0) >= 1, 5)
    assert ok, "farming value never raised (attrs=%s)" % (c.attrs,)
    return exp_before


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "farmbot"
    server_proc = ensure_farm_server()
    try:
        if mode == "skillbot":
            run_skillbot()
        else:
            run(mode)
    finally:
        if server_proc is not None:
            server_proc.terminate()
            server_proc.wait(timeout=10)


def run(mode):
    # Per-run character: fresh starter kit every time (a reused character
    # may have spent its seeds in an earlier run).
    username = "%s%d%d" % (mode, int(time.time()) % 100000, os.getpid() % 1000)
    c = enter_world(username, client_cls=FarmClient)

    # The Farming skill value gates planting: buy it through the char
    # sheet before the plow/plant loop (legacy cost 100 for point 1).
    buy_farming_value(c)
    print("farming skill value raised via sattr")

    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)

    # --- plow + plant: scan nearby tiles until a crop gob appears ---------
    wheat_item = c.find_item("Wheat Seeds")
    assert wheat_item is not None, "starter wheat seeds missing (items=%s widgets=%s)" % (
        {w: i["tt"] for w, i in c.item_info.items()}, sorted(set(c.widgets.values())))
    crop = None
    for dx in range(-2, 3):
        for dy in range(-2, 3):
            if abs(dx) + abs(dy) > 3:
                continue
            tile = (ptile[0] + dx, ptile[1] + dy)
            c.arm_plow()
            c.pump(0.25)
            c.click_tile(tile)
            c.pump(0.3)
            c.take_item(wheat_item)
            c.pump(0.25)
            c.map_itemact_tile(tile)
            if not c.wait_for(
                lambda: any(
                    (r or "").startswith("gfx/terobjs/plants/")
                    for r in (info["res"] for info in c.gobs.values())
                ),
                2.5,
            ):
                continue
            crops = c.find_gobs("gfx/terobjs/plants/wheat")
            if crops:
                crop = next(iter(crops.items()))
                break
    assert crop is not None, "no crop gob spawned on any candidate tile"
    (gob, info) = crop
    stage0 = info["sdt"]
    print("planted crop gob", gob, "stage sdt", list(stage0))
    # sdt must carry the stage byte; at 250 ms/stage the first tick may
    # already have advanced it, so only emptiness is a failure.
    assert stage0 != b"", "spawn block must carry a non-empty sdt stage byte"

    # --- growth: server advances stages (250 ms each at test scale) -------
    # Wheat matures at wire stage 3 (stages=3 in the crop table).
    ok = c.wait_for(lambda: c.gobs[gob]["sdt"] == b"\x03", 10)
    assert ok, "crop never reached maturity (sdt=%r)" % (c.gobs[gob]["sdt"],)
    print("growth: mature at stage byte", list(c.gobs[gob]["sdt"]))

    # --- harvest via flower menu ------------------------------------------
    ok = c.wait_for(
        lambda: any(w for w, n in c.widgets.items() if n == "sm"),
        1.0,
    )
    c.click_gob(gob, c.gobs[gob]["pos"])
    ok = c.wait_for(
        lambda: any(n == "sm" for n in c.widgets.values()), 4.0
    )
    assert ok, "harvest flower menu never opened"
    sm_wid = next(w for w, n in c.widgets.items() if n == "sm")
    before_wids = set(c.item_info)
    c.flower_choice(sm_wid)

    def new_yields():
        # refresh_inventory recreates every item widget, so yields show up
        # as NEW widget ids carrying the yield tooltip.
        return [
            info["tt"]
            for wid, info in c.item_info.items()
            if wid not in before_wids and info["tt"] in ("Straw", "Wheat Seeds")
        ]

    ok = c.wait_for(lambda: new_yields(), 5.0)
    assert ok, "no yield item landed in inventory"
    print("harvest yields:", sorted(set(new_yields())))
    print("FARMING FLOW: OK")


def run_skillbot():
    """Skill-gate verification: planting refused without the Farming skill
    value; sattr purchase at the exact legacy cost; planting then works;
    unknown catalog buys are refused."""
    username = "skill%d%d" % (int(time.time()) % 100000, os.getpid() % 1000)
    c = enter_world(username, client_cls=FarmClient)
    assert c.chat_id is not None, "Area Chat widget missing"

    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)
    wheat_item = c.find_item("Wheat Seeds")
    assert wheat_item is not None, "starter wheat seeds missing"

    def plant_gobs():
        return {
            g
            for g, info in c.gobs.items()
            if (info["res"] or "").startswith("gfx/terobjs/plants/")
        }

    def tile_free(tx, ty):
        # Skip tiles a previous run's persisted crop still occupies.
        for g in c.gobs.values():
            pos = g["pos"]
            if pos and (pos[0] // 11, pos[1] // 11) == (tx, ty):
                if (g["res"] or "").startswith("gfx/terobjs/plants/"):
                    return False
        return True

    def plant_at(tile):
        """Plow + act with the cursor seed; returns the set of NEW plant
        gobs that appeared (diff against the pre-action snapshot). The
        cursor persists between attempts, so a re-take is only sent when
        the previous attempt could not have left a seed armed."""
        before = plant_gobs()
        c.arm_plow()
        c.pump(0.25)
        c.click_tile(tile)
        c.pump(0.3)
        if not cursor_held:
            c.take_item(wheat_item)
            c.pump(0.25)
        c.map_itemact_tile(tile)
        c.wait_for(lambda: len(plant_gobs()) > len(before), 2.5)
        return plant_gobs() - before

    # The cursor is armed by the first take and stays armed: the refusal
    # path consumes nothing, and a successful plant consumes exactly one
    # unit out of the 5-seed stack.
    cursor_held = False

    # --- 1. planting is refused while the farming value is 0 -------------
    tile = next((tx, ty) for dx in range(1, 6) for tx, ty in [(ptile[0] + dx, ptile[1])] if tile_free(tx, ty))
    new_crops = plant_at(tile)
    cursor_held = True  # the take was sent; the refusal consumed nothing
    assert not new_crops, (
        "planting must be refused without the Farming skill (spawned=%s)"
        % (new_crops,)
    )
    refused = any("Farming skill" in t for t, _ in c.chat_lines)
    assert refused, "no refusal system line arrived (lines=%s)" % (c.chat_lines,)
    print("gate: planting refused with a colored system line")

    # --- 2. buy the farming point through the char sheet ------------------
    exp_before = buy_farming_value(c)
    print("farming purchased; LP", exp_before, "->", c.exp_seen)
    # Legacy curve: one point from 0 costs exactly 100 LP. Fast LP accrual
    # (HNH_LP_RATE) may add a few points between the two reads.
    assert exp_before - 100 <= c.exp_seen <= exp_before - 40, (
        "sattr charge off the legacy curve: %s -> %s" % (exp_before, c.exp_seen)
    )

    # The cursor still holds the seed (the gate consumed nothing).
    tile2 = next((tx, ty) for dx in range(1, 8) for tx, ty in [(ptile[0] + dx, ptile[1])] if tile_free(tx, ty) and (tx, ty) != tile)
    c.arm_plow()
    c.pump(0.25)
    c.click_tile(tile2)
    c.pump(0.3)
    c.map_itemact_tile(tile2)
    ok = c.wait_for(
        lambda: any(
            (r or "").startswith("gfx/terobjs/plants/")
            for r in (info["res"] for info in c.gobs.values())
        ),
        4.0,
    )
    assert ok, "planting still refused after buying the Farming skill"
    print("gate lifted: crop gob spawned after purchase")

    # --- 3. unknown catalog buy is refused --------------------------------
    c.wdgmsg(
        c.chr_id,
        "buy",
        bytes([LIST_STR]) + havstr("nosuchskill") + bytes([LIST_END]),
    )
    ok = c.wait_for(
        lambda: any("unknown to this server" in t for t, _ in c.chat_lines), 3
    )
    assert ok, "unknown skill buy was not refused (lines=%s)" % (c.chat_lines,)
    print("unknown buy refused with a system line")
    print("SKILL GATE: OK")


if __name__ == "__main__":
    main()
