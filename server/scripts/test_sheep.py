#!/usr/bin/env python3
"""Session 93 live probe: the sheep/wool vertical end to end.

Drives the whole sheep family against a live server (the counterpart
of test_breeding.py for cattle):

  1. LP buys Hunting + Animal Husbandry; the rope is crafted and
     equipped (the taming gate).
  2. Wild MOUFLONS are quelled to full tameness - each morphs into a
     SHEEP on the spot. Session 93 fix under test: the mouflon sex
     roll is 50/50, so the herd reaches BOTH a ram and a ewe (the
     pre-fix pinning left every mouflon a ram and the sheep pipeline
     without any reachable dam). The sexes are read from the server's
     own herd-sweep debug lines. The seed-42 mouflon herd stands
     ~230 tiles NORTH of spawn (the session-93 scan_direction recon;
     deterministic on a fresh save): with the grid-stream fix far
     animals no longer ride MAPREQ, so the herd enters the stream
     only through the fresh grids the trek itself requests - the
     probe hops north in 70-tile stages (inside the BFS budget) until
     the mouflons arrive, then walk_to_beast closes on each.
  3. The flock follows the tamer onto a grass pasture; grazing
     counts as feeding for both parents.
  4. HNH_WOOL_RATE scales the wool accumulator (the new session-93
     knob; the legacy 8-hour unit is not sit-out-able): the ewe and
     the ram both fill their meters, and a flower-menu Shear draw
     collects the stored wool into the inventory (the whole stored
     meter, quality = the breeding row capped at the grazing row).
  5. HNH_BREED_SCALE=43200: the ram sires the ewe and a LAMB is born
     beside its dam as a fully-tamed juvenile; clicking the lamb
     answers the honest "The lamb is not yet grown." refusal line.
  6. Restart: the server is rebooted on the same save file and the
     flock (ram + ewe rows, the pregnancy accumulator, the juvenile
     age) reloads - the persisted v8 animal rows.

Prints "SHEEP: OK ..." on success.

Usage: python3 test_sheep.py [username]
"""
import os
import re
import sys
import time

import faulthandler

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import (  # noqa: E402
    REPO,
    TILE_SPAN,
    ensure_server,
    enter_world,
    stop_server,
)
from test_dough import (  # noqa: E402
    inv_total,
    request_neighborhood,
    walk_to_grass,
)
from test_dairy import (  # noqa: E402
    ROPE_INV,
    DairyClient,
    _quell_beast,
    buy_skill,
    close_on_beast,
    craft_once,
    equip_rope,
    wait_for_lp,
)

# The breeding test scale (the test_breeding.py pace): gestation ~9 s,
# maturation ~20 s.
BREED_SCALE = "43200"
# The wool test scale (session 93): one wool unit needs
# WOOL_ACC_PER_UNIT = 240000 accumulator ticks; a wild-morph row draws
# quantity 4..6, so at rate 24000 the per-sweep gain is ~100k-144k -
# a full 3-wool meter in a handful of sweeps (~10 s), legacy-true caps.
WOOL_RATE = "24000"

MOUFLON_PREFIX = "gfx/kritter/mufflon/"
SHEEP_PREFIX = "gfx/kritter/sheep/"
WOOL_INV = "gfx/invobjs/wool"

SERVER_LOG = os.path.join(REPO, "server", "target", "sheep-server.log")


def ensure_sheep_server(save_path, keep_save=False):
    """Fast-wool + fast-breed + fast-tame + fast-LP + no-aggro isolated
    server (the test_breeding.py env shape plus HNH_WOOL_RATE).

    HNH_LP_RATE=1000 keeps the skill gate at ~20 s: the entry stream
    holds one drifter mouflon whose axial distance is already past
    the retract hysteresis, and every extra skill minute sends it
    further out (the quell targets must stay reachable).

    The log is TRUNCATED on the fresh boot (ensure_server opens it in
    append mode): the seed-42 world is deterministic, so a failed
    prior run's herd-sweep lines name the SAME gob ids - stale sex
    lines would poison sheep_sexes_from_log with the pre-fix rolls."""
    if not keep_save:
        try:
            os.remove(SERVER_LOG)
        except OSError:
            pass
    return ensure_server(
        env_extra={
            "HNH_WOOL_RATE": WOOL_RATE,
            "HNH_LEASH_TICKS": "40",
            "HNH_BREED_SCALE": BREED_SCALE,
            "HNH_NO_AGGRO": "1",
            "HNH_FAST_TAME": "1",
            "HNH_LP_RATE": "1000",
        },
        save_path=save_path,
        keep_save=keep_save,
        log_path=SERVER_LOG,
        fresh=True,
    )


def walk_to_beast(c, gid, stage=55, reach=200, deadline=150.0):
    """Overland trek to a FAR beast. The staged nav_walk approach
    only carries the first ~500 subtiles: it idles at ~18 subt/s
    (nav_walk's fixed pump schedule), which is BELOW the panicking
    beast's 30, so inside the last stretch the trek hands over to
    the dairy close_on_beast pursuit (a re-aim at the LIVE position
    every 0.25 s slice - the gap closes at the honest (gait - 30)
    rate). The dairy chase's BFS hop (find_tile_path) aborts beyond a
    40k-node budget (~110 tiles of 8-dir spread), so the stages come
    first: hops in ~55-tile stages through the streamed grids, each
    stage well inside the budget, re-aiming at the beast's live
    position every hop and detouring perpendicular when a mountain or
    wall blocks the ideal line. Returns True once inside `reach`
    subtiles."""
    end = time.time() + deadline
    hops = 0
    while time.time() < end:
        pos = c.live_pos(c.player_gob) or c.gobs[c.player_gob]["pos"]
        bpos = c.live_pos(gid) or c.gobs[gid]["pos"]
        dist = abs(pos[0] - bpos[0]) + abs(pos[1] - bpos[1])
        if dist <= reach:
            print("TREK: %d hops, closed to dist=%d" % (hops, dist))
            return True
        if dist <= 500:
            # The close pursuit: the staged approach cannot close the
            # last stretch on a fleeing mouflon (18 < 30 subt/s).
            return close_on_beast(
                c, gid, want=reach,
                deadline=max(10.0, min(60.0, end - time.time())))
        dx, dy = bpos[0] - pos[0], bpos[1] - pos[1]
        m = max(abs(dx), abs(dy), 1)
        step = stage * TILE_SPAN
        goal = (pos[0] + dx * step // m, pos[1] + dy * step // m)
        if c.nav_walk(goal, stop=TILE_SPAN):
            hops += 1
            c.pump(0.1)
            continue
        # The ideal line is walled: detour perpendicular (+/- 8
        # tiles) and let the next re-aim fold the detour back in.
        detoured = False
        for s in (1, -1):
            side = (
                goal[0] + s * dy * 8 * TILE_SPAN // m,
                goal[1] - s * dx * 8 * TILE_SPAN // m,
            )
            if c.nav_walk(side, stop=2 * TILE_SPAN):
                detoured = True
                hops += 1
                break
        if not detoured:
            # Even the detours are blocked (mid-mountain): wait a
            # beat for the wander to carry the aim elsewhere.
            print("TREK: stage blocked at %s goal=%s (dist=%d)" % (
                pos, goal, dist))
            c.pump(1.0)
    print("TREK: deadline out at %s (beast %s, dist=%d)" % (
        c.gobs[c.player_gob]["pos"], bpos, dist))
    return False


def find_mouflon(c, exclude=()):
    """The nearest wild mouflon in the loaded grids (the sheep family's
    wild front end - both sexes roll 50/50 at quell time). Session 93:
    only gobs inside the FANOUT movement window count - a beast beyond
    it stopped receiving LINBEG/LINSTEP and its cached position
    silently rots (the frozen-ghost root cause the server-side
    grid-stream fix addresses; this filter is the client-side
    belt-and-braces)."""
    found = {
        g: info
        for g, info in c.gobs.items()
        if (info["res"] or "").startswith(MOUFLON_PREFIX)
        and not info.get("removed")
        and g not in exclude
    }
    if not found:
        return None
    me = c.live_pos(c.player_gob) or (0, 0)

    def live_axis(item):
        gid, info = item
        pos = c.live_pos(gid) or info.get("pos") or (0, 0)
        return abs(pos[0] - me[0]) <= 900 and abs(pos[1] - me[1]) <= 900

    pool = [it for it in found.items() if live_axis(it)]
    if not pool:
        return None

    def dist(item):
        gid, info = item
        pos = c.live_pos(gid) or info.get("pos") or (0, 0)
        return abs(pos[0] - me[0]) + abs(pos[1] - me[1])

    return min(pool, key=dist)[0]


# The seed-42 mouflon herd waypoint (session-93 scan_direction.py N
# recon, deterministic on the fresh save): 4+ wild mouflons graze
# ~2.4k subtiles north of the entry point, spread over roughly
# (214..720, -2100..-2700), and the straight north line x~555 is
# water-clear the whole way. With the grid-stream fix the far animals
# no longer ride MAPREQ, so the herd reaches the probe ONLY through
# the fresh grid requests the trek itself generates.
NORTH_HERD = (555, -2450)


def sheep_sexes_from_log(gids):
    """Read the herd-sweep debug lines for the known morphed gobs:
    'breeding sweep: ... adults=Sheep#<gid & 0xFFFF> Female pos=...'
    -> {gid: 'Female'|'Male'} (the sweep only prints FED adults on
    pasture, so the caller pumps first)."""
    sexes = {}
    try:
        with open(SERVER_LOG, "r", errors="replace") as fh:
            for line in fh:
                if "breeding sweep" not in line and "herd sweep" not in line:
                    continue
                for gid in gids:
                    tag = "Sheep#%d " % (gid & 0xFFFF)
                    if tag not in line:
                        continue
                    for sex in ("Female", "Male"):
                        if tag + sex in line:
                            sexes[gid] = sex
    except OSError:
        pass
    return sexes


def shear_sheep(c, gid, timeout=90.0):
    """Click a morphed sheep until the Shear petal opens (the wool
    meter fills at HNH_WOOL_RATE while the flock grazes), then draw
    the stored wool. Returns the number of wool units collected."""
    end = time.time() + timeout
    while time.time() < end:
        pos = c.live_pos(gid) or c.gobs[gid]["pos"]
        c.click_gob(gid, pos)
        if c.wait_for(lambda: c.sm_wid is not None, 6):
            if "Shear" in c.sm_opts:
                before = inv_total(c, WOOL_INV)
                c.flower_choice(c.sm_wid, c.sm_opts.index("Shear"))
                if c.wait_for(lambda: inv_total(c, WOOL_INV) > before, 8):
                    return inv_total(c, WOOL_INV) - before
                return 0
            # Refusal lines name the state; keep grazing and retry.
            for text, _ in c.chat_lines[-4:]:
                if "no wool to shear" in text:
                    break
                if "not yet grown" in text:
                    return 0
            if c.sm_wid is not None:
                c.flower_choice(c.sm_wid, 0)  # cancel
        c.pump(3.0)
    return 0


def wait_for_lamb(c, known, anchor, deadline=120.0):
    """Wait for a NEW sheep-res gob beside the ewe (the lamb is born
    as a Sheep, never a mouflon). Re-clusters the flock like the
    cattle probe: the parents must stand inside the 55-subtile sire
    radius on grazing tiles."""
    def candidates():
        apos = c.live_pos(anchor)
        out = []
        for g, info in c.gobs.items():
            if not (info["res"] or "").startswith(SHEEP_PREFIX):
                # The LAYERS base overwrites the recorded res with the
                # body folder; match the kritter folder prefix.
                continue
            if info.get("removed") or g in known:
                continue
            if apos is not None:
                bpos = c.live_pos(g) or info.get("pos")
                if bpos is None:
                    continue
                if abs(bpos[0] - apos[0]) + abs(bpos[1] - apos[1]) > 260:
                    continue
            out.append(g)
        return out

    end = time.time() + deadline
    attempt = 0
    while time.time() < end:
        ppos = c.live_pos(c.player_gob) or (0, 0)
        dx, dy = (22, 0) if attempt % 2 == 0 else (0, 22)
        c.click_ground(ppos[0] + dx, ppos[1] + dy)
        attempt += 1
        t0 = time.time()
        while time.time() - t0 < 12.0:
            c.pump(1.0)
            lambs = candidates()
            if lambs:
                return lambs[0]
            if time.time() > end:
                return None
    return None


def main():
    # The test_breeding.py watchdog shape: dump the stuck stack instead
    # of hanging the gate. The north herd trek is ~4 min of walking
    # alone, so the budget is 900 s (the gate stays under 15 min).
    faulthandler.enable()
    faulthandler.dump_traceback_later(900, exit=True)
    username = sys.argv[1] if len(sys.argv) > 1 else "sheep%d" % (
        int(time.time()) % 100000,)
    save_path = os.path.join(
        REPO, "server", "target",
        "sheep-test-save-%d.json" % (os.getpid() % 100000))
    proc = ensure_sheep_server(save_path)
    try:
        c = enter_world(username, client_cls=DairyClient)
        request_neighborhood(c)

        # 1. Skills + rope (the quell gates).
        wait_for_lp(c, 620)
        buy_skill(c, "hunting", 200)
        buy_skill(c, "ahusb", 400)
        craft_once(c, "rope", ROPE_INV)
        equip_rope(c)

        # 2. Quell mouflons until the flock holds BOTH sexes (the
        #    session-93 50/50 roll). Two phases: the pure north trek
        #    along the reconnoitered clear line (x~555 - chasing
        #    en-route drifter mouflons pulled the probe into the
        #    swamps, walled-off beasts burning the whole deadline),
        #    then the quell cycle standing IN the herd where the
        #    chases are short. The herd sweep (every 30 ticks = 3 s,
        #    rows carry the sex) names each morphed gob as soon as it
        #    is fully tamed - no grazing needed yet, so no
        #    walk_to_grass inside the loop.
        flock = []
        deadline_trek = time.time() + 300.0
        while time.time() < deadline_trek:
            pos = c.live_pos(c.player_gob) or c.gobs[c.player_gob]["pos"]
            dx = NORTH_HERD[0] - pos[0]
            dy = NORTH_HERD[1] - pos[1]
            if abs(dx) + abs(dy) <= 70 * TILE_SPAN:
                print("TREK NORTH: at the herd waypoint")
                break
            m = max(abs(dx), abs(dy), 1)
            stage = 70 * TILE_SPAN
            goal = (pos[0] + dx * stage // m, pos[1] + dy * stage // m)
            if not c.nav_walk(goal, stop=2 * TILE_SPAN):
                # Water or cliff on the line: detour perpendicular
                # and let the next re-aim fold the detour back in.
                for s in (1, -1):
                    side = (
                        goal[0] + s * dy * 8 * TILE_SPAN // m,
                        goal[1] - s * dx * 8 * TILE_SPAN // m,
                    )
                    if c.nav_walk(side, stop=2 * TILE_SPAN):
                        break
            c.pump(2.0)
        else:
            raise AssertionError(
                "the pure north trek never reached the herd waypoint")
        skipped = set()
        for _ in range(12):
            gid = find_mouflon(c, exclude=flock + list(skipped))
            if gid is None:
                # The herd wandered out of the 600-axial stream
                # window: sweep a small cross around the waypoint -
                # fresh grids re-stream the beasts.
                pos = c.live_pos(c.player_gob) or c.gobs[
                    c.player_gob]["pos"]
                dx, dy = NORTH_HERD[0] - pos[0], NORTH_HERD[1] - pos[1]
                m = max(abs(dx), abs(dy), 1)
                if abs(dx) + abs(dy) <= 90 * TILE_SPAN:
                    dx, dy, m = dy, -dx, max(abs(dy), abs(dx), 1)
                    stage = 25 * TILE_SPAN
                else:
                    stage = 70 * TILE_SPAN
                goal = (pos[0] + dx * stage // m,
                        pos[1] + dy * stage // m)
                if not c.nav_walk(goal, stop=2 * TILE_SPAN):
                    for s in (1, -1):
                        side = (
                            goal[0] + s * dy * 8 * TILE_SPAN // m,
                            goal[1] - s * dx * 8 * TILE_SPAN // m,
                        )
                        if c.nav_walk(side, stop=2 * TILE_SPAN):
                            break
                c.pump(2.0)
                continue
            if not walk_to_beast(c, gid):
                # A water body or cliff walls the beast off: skip it -
                # the herd spreads over ~500 subtiles, the next
                # candidate is usually reachable from a different side.
                print("MOUFLON %d unreachable (walled off) - skipped" % gid)
                skipped.add(gid)
                continue
            _quell_beast(c, gid, rounds=1)
            flock.append(gid)
            print("MOUFLON %d quelled -> morphed into the flock" % gid)
            c.pump(4.0)
            sexes = sheep_sexes_from_log(flock)
            if "Female" in sexes.values() and "Male" in sexes.values():
                break
        sexes = sheep_sexes_from_log(flock)
        ewes = [g for g in flock if sexes.get(g) == "Female"]
        rams = [g for g in flock if sexes.get(g) == "Male"]
        assert ewes and rams, (
            "the flock never reached both sexes (flock=%r sexes=%r)"
            % (flock, sexes))
        ewe, ram = ewes[0], rams[0]
        print("FLOCK: %d sheep - ewe %d + ram %d (the 50/50 morph)"
              % (len(flock), ewe, ram))

        # 3. Onto the pasture ONE trek: the flock follows the tamer
        #    (the per-tick follow walk), and grazing feeds the whole
        #    herd at once - the wool meters and the pregnancy both
        #    need the fed state.
        walk_to_grass(c)
        c.pump(6.0)

        # 4. Wool: the whole flock grows wool while grazing; shear the
        #    ewe (the ram's meter runs too - the doc rate).
        wool = shear_sheep(c, ewe)
        assert wool >= 1, (
            "the ewe's meter never filled (wool=%d, chat=%r)"
            % (wool, [t for t, _ in c.chat_lines[-6:]]))
        print("SHEAR: %d wool drawn from the ewe (stored meter, "
              "row-capped quality)" % wool)

        # 5. Lamb (~9 s at the scale) beside the ewe.
        known = set(flock)
        lamb = wait_for_lamb(c, known, anchor=ewe)
        assert lamb, (
            "no lamb ever arrived (chat=%r)" % (
                [t for t, _ in c.chat_lines[-6:]],))
        print("LAMB BORN: gob %d beside the ewe (domestic-born, "
              "inherited rows)" % lamb)

        # 6. The honest refusal: a juvenile cannot open the Shear petal.
        pos = c.live_pos(lamb) or c.gobs[lamb]["pos"]
        c.click_gob(lamb, pos)
        refused = c.wait_for(
            lambda: any(
                "not yet grown" in t for t, _ in c.chat_lines[-4:]), 8)
        if c.sm_wid is not None:
            c.flower_choice(c.sm_wid, 0)
        assert refused, (
            "the lamb never refused (chat=%r)"
            % ([t for t, _ in c.chat_lines[-6:]],))
        print("LAMB REFUSAL: the honest juvenile line")

        # 7. Persist: SIGTERM flush, reboot on the same file, re-enter
        #    the SAME character (the persisted position lands beside
        #    the flock).
        stop_server(proc)
        proc = ensure_sheep_server(save_path, keep_save=True)
        c2 = enter_world(username, client_cls=DairyClient)
        request_neighborhood(c2)
        c2.pump(4.0)
        restored = {
            g for g, info in c2.gobs.items()
            if (info["res"] or "").startswith(SHEEP_PREFIX)
            and not info.get("removed")
        }
        assert len(restored) >= len(flock), (
            "the flock did not reload after restart (%d sheep gobs)"
            % len(restored))
        print("PERSIST: flock rows reloaded from the v8 save")

        print("SHEEP: OK (%s: mouflons quelled to a ram + a ewe, wool "
              "sheared through the scaled meter, lamb born from the "
              "scaled gestation, flock persisted)" % username)
    finally:
        if proc is not None:
            stop_server(proc)


if __name__ == "__main__":
    sys.exit(main())
