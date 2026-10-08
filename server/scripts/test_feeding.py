#!/usr/bin/env python3
"""End-to-end Food Trough lift / place / fodder transfer verification.

Chain under test (wire level, the same path the Java client drives;
docs/mechanics/livestock/animals-and-husbandry.md "Feeding: troughs and
grazing" + the session-62 implementation notes):
  auth -> world entry -> build pagina "trough" -> place -> branch x4
  -> itemact wheat seeds (one fodder unit per click)
  -> click the trough -> the "Lift" flower menu -> the gob retracts
  -> map click places the carried trough back down
  -> second trough: build, load, lift
  -> click the first trough while carrying -> "like a liquid" transfer
  -> the system lines name every outcome.

Auto-starts the server binary on an isolated save when 1871 is free;
otherwise reuses the already-running server.

Prints FEEDING FLOW: OK.
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import (  # noqa: E402
    LIST_END,
    LIST_INT,
    WireClient,
    enter_world,
    ensure_server,
    stop_server,
)


class TroughBot(WireClient):
    """Build-flow client specialized for trough choreography."""

    def build_trough(self, dx):
        """Build one completed trough dx tiles east of the player.
        Returns (gob, mc) of the finished station."""
        ppos = self.gobs[self.player_gob]["pos"] or (0, 0)
        ptile = (ppos[0] // 11, ppos[1] // 11)
        mc = None
        plan = None
        for dy in range(0, 4):
            cand = (ptile[0] + dx, ptile[1] + dy)
            cmc = (cand[0] * 11 + 5, cand[1] * 11 + 5)
            self.menu_act("trough")
            ok = self.wait_for(lambda: self.place_seen is not None, 5)
            assert ok, "place uimsg missing (trough build pagina)"
            self.place_seen = None
            self.send_place(cmc, 1, 0)
            if self.wait_for(
                lambda: any(
                    info["res"] == "gfx/terobjs/trough"
                    and info["pos"] == cmc
                    for info in self.gobs.values()
                ),
                5,
            ):
                plan = next(
                    g
                    for g, info in self.gobs.items()
                    if info["res"] == "gfx/terobjs/trough" and info["pos"] == cmc
                )
                mc = cmc
                break
            print("  site refused (%s,%s), trying the next" % cand)
        assert plan is not None, "no candidate site accepted a trough plan"

        # Sink the branch demand (x4) from the starter stack.
        branch = self.find_item_by_res("gfx/invobjs/branch")
        assert branch is not None, "starter branch missing"
        self.take_item(branch)
        self.pump(0.3)
        self.map_itemact(mc, plan)
        # Completion drops the plan sdt back to 0 and the store opens.
        ok = self.wait_for(
            lambda: self.gobs.get(plan, {}).get("sdt") == b"\x00", 5
        )
        assert ok, "trough plan never completed (sdt=%r)" % (
            self.gobs.get(plan, {}).get("sdt"),
        )
        assert self.return_cursor(), "inventory window missing for cursor return"
        self.pump(0.3)
        return plan, mc

    def load_fodder(self, gob, mc, units, seed_res):
        """Load `units` fodder units one click at a time from the named
        seed stack (one fodder unit per itemact)."""
        seed = self.find_item_by_res(seed_res)
        assert seed is not None, "%s missing" % seed_res
        added = 0
        for _ in range(units):
            # The cursor drains by one per click; re-take while the
            # cursor still holds seeds (refresh_inventory recreates the
            # widgets, so resolve the live wid every take).
            self.take_item(seed)
            self.pump(0.25)
            self.map_itemact(mc, gob)
            ok = self.wait_for(
                lambda: any(
                    "Fodder added to the trough." in t
                    for t, _ in self.chat_lines[-1:]
                ),
                4,
            )
            assert ok, "fodder click %d never acknowledged" % (added + 1)
            added += 1
        assert added == units
        assert self.return_cursor(), "cursor left armed after loading"
        self.pump(0.3)

    def click_trough(self, gob):
        info = self.gobs[gob]
        self.click_gob(gob, info["pos"])


def main():
    server_proc = ensure_server()
    try:
        c = enter_world("feed%d%d" % (int(time.time()) % 100000, os.getpid() % 1000),
                        client_cls=TroughBot)

        # --- trough 1: build + load 5 fodder units -----------------------
        t1, mc1 = c.build_trough(1)
        print("trough 1 built:", t1, "at", mc1)
        c.load_fodder(t1, mc1, 5, "gfx/invobjs/seed-wheat")
        print("trough 1 loaded: 5 fodder units")

        # --- click trough 1 -> the Lift menu -> the gob retracts ---------
        c.click_trough(t1)
        ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Lift"], 4)
        assert ok, "the trough menu never offered Lift (opts=%s)" % (c.sm_opts,)
        c.flower_choice(c.sm_wid)
        ok = c.wait_for(lambda: c.gobs[t1].get("removed"), 5)
        assert ok, "the lifted trough gob was never retracted"
        ok = c.wait_for(
            lambda: any("You lift the trough (5 fodder units)." in t
                        for t, _ in c.chat_lines),
            4,
        )
        assert ok, "no lift system line (lines=%s)" % (c.chat_lines[-3:],)
        print("lifted trough 1: gob retracted, carry started")

        # --- map click places the carried trough back down ---------------
        ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
        ptile = (ppos[0] // 11, ppos[1] // 11)
        site = (ptile[0] + 1, ptile[1] + 2)
        cmc = (site[0] * 11 + 5, site[1] * 11 + 5)
        c.send_place(cmc, 1, 0)
        ok = c.wait_for(
            lambda: any(
                info["res"] == "gfx/terobjs/trough" and info["pos"] == cmc
                for info in c.gobs.values()
            ),
            5,
        )
        assert ok, "the carried trough was never placed back"
        t1b = next(
            g for g, info in c.gobs.items()
            if info["res"] == "gfx/terobjs/trough" and info["pos"] == cmc
        )
        ok = c.wait_for(
            lambda: any("You place the trough (5 fodder units)." in t
                        for t, _ in c.chat_lines),
            4,
        )
        assert ok, "no place-back system line"
        print("placed back trough 1 as gob", t1b, "at", cmc)

        # --- trough 2: build + load 2 + lift ------------------------------
        t2, mc2 = c.build_trough(2)
        c.load_fodder(t2, mc2, 2, "gfx/invobjs/seed-carrot")
        c.click_trough(t2)
        ok = c.wait_for(lambda: c.sm_wid is not None and c.sm_opts == ["Lift"], 4)
        assert ok, "trough 2 menu missing"
        c.flower_choice(c.sm_wid)
        ok = c.wait_for(lambda: c.gobs[t2].get("removed"), 5)
        assert ok, "trough 2 was never retracted"
        print("lifted trough 2 (2 fodder units carried)")

        # --- click trough 1 while carrying -> the liquid transfer ---------
        c.click_trough(t1b)
        ok = c.wait_for(
            lambda: any("Transferred 2 fodder units." in t
                        for t, _ in c.chat_lines),
            5,
        )
        assert ok, "no transfer system line (lines=%s)" % (c.chat_lines[-4:],)
        print("transfer: carried 2 units moved into trough 1")
        print("FEEDING FLOW: OK")
        return 0
    finally:
        stop_server(server_proc)


if __name__ == "__main__":
    sys.exit(main())
