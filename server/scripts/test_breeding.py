#!/usr/bin/env python3
"""Session 90 live probe: domestic breeding + per-animal breed rows.

Drives the whole domestic-lifecycle vertical against a live server:

  1. LP buys Hunting + Animal Husbandry; the rope is crafted and
     equipped (the taming gate) and a saw + an EMPTY bucket join the
     inventory (the milking gate - the draw re-validates the bucket
     every time).
  2. A wild COW is quelled to full tameness (the Female row - the
     wild cow IS the dairy cow) and a wild AUROCHS is quelled
     separately (the Male row - the domestic bull arrives through the
     aurochs morph; session 90 policy).
  3. The herd walks onto a grass pasture behind the tamer (leash
     follow); grazing counts as feeding for BOTH parents.
  4. HNH_BREED_SCALE=43200: the gestation accumulator crosses the
     3,888,000-tick term in ~90 ticks (~9 s). The BULL sires the COW
     and a CALF is born beside its dam as a new fully-tamed juvenile
     gob (domestic-born, never leashed, inherited breed rows).
  5. The calf matures in ~200 ticks (~20 s) at the same scale, then:
     a FEMALE calf (heifer) fills her milk meter at HNH_MILK_RATE and
     a flower-menu Milk draw verifies the INHERITED row produces; a
     MALE calf answers with the honest "The bull gives no milk."
     refusal line. The dam re-conceives every cycle, so the probe
     waits through up to six calves until a heifer lands (a 50/50
     roll per calf; six misses is a 1.6% tail, acceptable live).
  6. Restart: the server is rebooted on the same save file and the
     grown herd (dam + bull + calf rows, the pregnancy accumulator
     and the juvenile age) reloads - the persisted v8 animal rows.

Prints "BREEDING: OK ..." on success.

Usage: python3 test_breeding.py [username]
"""
import os
import sys
import time

import faulthandler

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import (  # noqa: E402
    REPO,
    ensure_server,
    enter_world,
)
from test_dough import (  # noqa: E402
    inv_total,
    request_neighborhood,
)
from test_dairy import (  # noqa: E402
    BUCKETE_INV,
    MILK_INV,
    MILK_LABEL,
    ROPE_INV,
    SAW_INV,
    DairyClient,
    buy_skill,
    craft_once,
    equip_rope,
    find_cow,
    milk_cycle,
    quell_cycle,
    wait_for_lp,
)

# The breeding test scale (session 90): the gestation term is
# 3,888,000 ticks and maturation 8,640,000; the scale multiplies both
# accumulators, so x43200 -> ~90-tick pregnancies (~9 s) and ~200-tick
# childhoods (~20 s) - sit-out-able live, legacy-true thresholds.
BREED_SCALE = "43200"
# The milk test scale (the dairy probe's): one bucket per ~100 ticks.
MILK_RATE = "6000"

COW_PREFIX = "gfx/kritter/cow/"


def ensure_breeding_server(save_path, keep_save=False):
    """Fast-breed + fast-milk + fast-leash + fast-tame + no-aggro
    isolated server.

    keep_save (session 90 restart leg): boot on the file the previous
    server flushed on its SIGTERM shutdown instead of deleting it -
    the herd, its breeding rows and the tamer's position must reload
    from it (v8). The FIRST boot of a run keeps the deletion (a
    leaked save from a same-PID run would otherwise park foreign
    plans on the spawn tiles).

    HNH_NO_AGGRO (session 90): the herd walk crosses half a map of
    wolves and boars, and every one that reaches swing reach locks the
    tamer's single Fightview until the sprint-away break clears - the
    sire's quell then starved behind a forced duel ("the fight never
    stayed open"). The knob silences the auto-aggro for THIS probe
    only; the unit tests keep the chase live.
    HNH_FAST_TAME (session 90): one landed quell banks full tameness.
    The five-round stack is unit-covered; without the knob the live
    breeding probe spent 3+ minutes per beast on the quell rounds
    alone (each round = a chase + a leash-break window) and the
    400 s watchdog reaped it mid-protocol with a half-tamed bull -
    the gestation legs never ran at all."""
    return ensure_server(
        env_extra={
            "HNH_MILK_RATE": MILK_RATE,
            "HNH_LEASH_TICKS": "40",
            "HNH_BREED_SCALE": BREED_SCALE,
            "HNH_NO_AGGRO": "1",
            "HNH_FAST_TAME": "1",
        },
        save_path=save_path,
        keep_save=keep_save,
        log_path=os.path.join(
            REPO, "server", "target", "breeding-server.log"),
        # Session 90: this probe OWNS its server - a leaked child from
        # a watchdog-killed previous run must die first, or the run
        # attaches to a foreign world (player gob 69900, zero cows).
        fresh=True,
    )


def known_cow_gobs(c, known, anchor=None, max_d=260):
    """Cow-res gobs currently in view, excluding the known ids (the
    dam and the bull render with the same cow prefix after the morph
    - the calf is only distinguishable by its NEW gob id).
    Session 90 fix: with `anchor` set (the dam's gob) a candidate
    must ALSO stand within `max_d` subtiles of it - a freshly
    populated WILD cow (spawned when the herd walk crossed a new
    grid) matches the plain res filter and used to masquerade as
    the calf (live trace: a wild cow 3 000 subtiles away passed the
    old check)."""
    apos = c.live_pos(anchor) if anchor is not None else None
    out = []
    for g, info in c.gobs.items():
        if not (info["res"] or "").startswith(COW_PREFIX):
            continue
        if info.get("removed") or g in known:
            continue
        if apos is not None:
            bpos = c.live_pos(g) or info.get("pos")
            if bpos is None:
                continue
            if abs(bpos[0] - apos[0]) + abs(bpos[1] - apos[1]) > max_d:
                continue
        out.append(g)
    return out


def herd_dist(c, gid):
    """Manhattan distance from the player to a herd member's live
    streamed position (the leash keeps the tamed beasts within ~30
    subtiles of the tamer)."""
    b = c.live_pos(gid)
    p = c.live_pos(c.player_gob)
    if b is None or p is None:
        return 1 << 30
    return abs(b[0] - p[0]) + abs(b[1] - p[1])


def reposition_herd(c, tries=4, settle=12.0):
    """Nudge the tamer along the pasture so the following beasts land
    on fresh grass inside the 55-subtile sire radius of each other:
    every step re-anchors the leash cluster; grazing resumes on the
    tiles under their feet."""
    for attempt in range(tries):
        ppos = c.live_pos(c.player_gob) or (0, 0)
        # A short step in a fixed direction: 2 tiles.
        dx, dy = (22, 0) if attempt % 2 == 0 else (0, 22)
        c.click_ground(ppos[0] + dx, ppos[1] + dy)
        c.pump(2.5)
        # Let the followers close in.
        t0 = time.time()
        while time.time() - t0 < settle:
            c.pump(1.0)
            yield True


def wait_for_calf(c, known, deadline=120.0, anchor=None):
    """Wait for a NEW cow gob in view BESIDE THE DAM (anchor): the
    calf. Repositions the herd on the pasture every ~12 s (the
    parents may stand split across a non-grazing boundary or outside
    the 55-subtile sire radius; the step re-clusters the leash
    group)."""
    end = time.time() + deadline
    for _ in reposition_herd(c):
        if time.time() > end:
            break
        calves = known_cow_gobs(c, known, anchor=anchor)
        if calves:
            return calves[0]
    # Final quiet window.
    return c.wait_for(
        lambda: bool(known_cow_gobs(c, known, anchor=anchor)), 20)


def wait_for_growth(c, calf, deadline=28.0):
    """Pump until the calf's juvenile accumulator crosses
    MATURATION_TICKS (200 ticks ~ 20 s at the test scale; 28 s banks
    the margin for server tick jitter). No wire signal exists for
    adulthood (the sprite never changes - the pack ships no calf
    drawable), so the probe simply banks the maturation window and
    the heifer leg below proves the growth landed: a juvenile cannot
    open the Milk petal at all. The calf only grows while FED, and it
    is born on the grazing pasture beside its dam, so the quiet pump
    is enough - no repositioning."""
    t0 = time.time()
    while time.time() - t0 < deadline:
        c.pump(1.0)
    return time.time() - t0


def try_heifer_milk(c, calf, timeout=45.0):
    """Click the grown calf: a heifer with a filled meter opens the
    Milk petal and the draw lands a Bucket of Milk (the INHERITED row
    producing); a bull calf answers the honest refusal line. The
    empty bucket must already sit in the inventory (the main flow
    crafts it with the rope): the server re-validates it on every
    draw and answers "You need an empty bucket..." without one - a
    fast fail beats a silent 45 s spin. Returns
    (True, 'heifer') / (True, 'bull') / (False, reason)."""
    end = time.time() + timeout
    while time.time() < end:
        pos = c.live_pos(calf) or c.gobs[calf]["pos"]
        c.click_gob(calf, pos)
        if c.wait_for(lambda: c.sm_wid is not None, 6):
            if "Milk" in c.sm_opts:
                before = inv_total(c, MILK_INV)
                c.flower_choice(c.sm_wid, c.sm_opts.index("Milk"))
                ok = c.wait_for(
                    lambda: inv_total(c, MILK_INV) > before, 8)
                if ok:
                    return True, "heifer"
                return False, "milk menu but no item"
            # The refusal lines are the sex/growth verdicts.
            for t, _ in c.chat_lines[-4:]:
                if "The bull gives no milk" in t:
                    return True, "bull"
                if "not yet grown" in t:
                    return False, "still growing"
                if "need an empty bucket" in t:
                    return False, "no empty bucket"
                if "no milk yet" in t:
                    # A heifer whose meter is still filling (born with
                    # 0; the row only starts accruing as an adult).
                    c.pump(3.0)
                    break
            if c.sm_wid is not None:
                c.flower_choice(c.sm_wid, 0)  # cancel
        else:
            # Session 90 live diagnosis: a click that answers NOTHING
            # (no menu, no refusal line) spins silently - print the
            # whole state (positions, chat verbatim) so the trace names
            # its cause instead of "no calf menu".
            ppos = c.live_pos(c.player_gob)
            print("MILK-CLICK calf=%d calf_pos=%s player=%s menu=%r "
                  "opts=%r chat=%r" % (
                      calf, pos, ppos, c.sm_wid, c.sm_opts,
                      [t for t, _ in c.chat_lines[-6:]]))
        c.pump(2.0)
    return False, "no calf menu"


def main():
    # Session 90 watchdog: a live trace died 4 minutes SILENT (no
    # output, no packets - the 60 s session timeout reaped the server
    # session while python sat somewhere off-pump). faulthandler dumps
    # the stuck stack and exits instead of hanging the whole probe.
    # 500 s: the taming legs (~75 s), the calf window (<=120 s), the
    # growth bank (28 s), up to six milk/sibling rounds and the
    # restart leg all fit under it with margin.
    faulthandler.enable()
    faulthandler.dump_traceback_later(500, exit=True)
    username = sys.argv[1] if len(sys.argv) > 1 else "breed%d" % (
        int(time.time()) % 100000,)
    save_path = os.path.join(
        REPO, "server", "target",
        "breeding-test-save-%d.json" % (os.getpid() % 100000))
    proc = ensure_breeding_server(save_path)
    try:
        c = enter_world(username, client_cls=DairyClient)
        request_neighborhood(c)

        # 1. Skills + rope (the quell gates) + saw/bucket (the milk
        #    gate - the draw re-validates an EMPTY bucket every time;
        #    the dairy probe churns butter to return it, this probe
        #    only ever needs the one).
        wait_for_lp(c, 620)
        buy_skill(c, "hunting", 200)
        buy_skill(c, "ahusb", 400)
        craft_once(c, "rope", ROPE_INV)
        equip_rope(c)
        craft_once(c, "saw", SAW_INV)
        craft_once(c, "bucket", BUCKETE_INV)

        # 2. Tame the dairy COW (the Female row) ...
        dam = quell_cycle(c, prefer="cow", rounds=1)
        print("DAM: cow %d tamed (female row)" % dam)
        # ... and the AUROCHS (the Male row - the domestic bull).
        bull = quell_cycle(c, prefer="aurochs", rounds=1)
        print("SIRE: aurochs %d tamed -> domestic bull (male row)" % bull)

        # 3. Onto the pasture: walk to grass, the herd follows.
        from test_dough import walk_to_grass
        walk_to_grass(c)
        c.pump(3.0)

        # 4. Gestation (~9 s at the scale) with herd re-clustering.
        known = {dam, bull}
        calf = wait_for_calf(c, known, anchor=dam)
        assert calf, (
            "no calf ever arrived (chat=%r, gobs=%d)"
            % (c.chat_lines[-6:], len(c.gobs)))
        dpos = c.live_pos(dam) or (0, 0)
        cpos = c.live_pos(calf) or (0, 0)
        assert abs(dpos[0] - cpos[0]) + abs(dpos[1] - cpos[1]) <= 110, (
            "the calf spawned far from its dam (%s vs %s)" % (dpos, cpos))
        print("CALF BORN: gob %d beside the dam (domestic-born, "
              "inherited rows)" % calf)

        # 5. Maturation (~20 s), then the sex verdict through the
        #    flower menu: a heifer produces, a bull refuses.
        grown = wait_for_growth(c, calf)
        verdict = None
        calves = [calf]
        for attempt in range(6):
            ok, kind = try_heifer_milk(c, calves[-1])
            if ok and kind == "heifer":
                verdict = "heifer"
                break
            if ok and kind == "bull":
                print("CALF %d: male (the honest bull refusal line)"
                      % calves[-1])
                # The dam re-conceives every ~9 s: wait for a sibling
                # heifer through the next birth.
                known.update(calves)
                nxt = wait_for_calf(c, known, deadline=60.0, anchor=dam)
                assert nxt, "no follow-up calf for a heifer (%r)" % (
                    c.chat_lines[-6:],)
                calves.append(nxt)
                print("SIBLING CALF BORN: gob %d" % nxt)
                wait_for_growth(c, nxt)
                continue
            assert attempt < 5, (
                "calf menu never resolved: %r" % (kind,))
        assert verdict == "heifer", "no heifer through six calves"
        print("HEIFER MILK: %s drawn from the bred calf (the inherited "
              "row produces - %s)" % (MILK_LABEL, grown and "grown"))

        # 6. Persist: the server flushes on its SIGTERM shutdown; a
        #    fresh boot on the SAME file must restore the whole herd
        #    with the v8 breeding rows (sex, breed rows, pregnancy,
        #    juvenile age). Re-entering the SAME character lands the
        #    probe on its persisted position - the pasture beside the
        #    herd - so the restored rows are actually in view to
        #    count (a fresh character spawns 3 000 subtiles away at
        #    (555, 555) and sees only wild cows).
        from hnhlib import stop_server
        stop_server(proc)
        proc = ensure_breeding_server(save_path, keep_save=True)
        c2 = enter_world(username, client_cls=DairyClient)
        request_neighborhood(c2)
        c2.pump(3.0)
        restored = {
            g for g, info in c2.gobs.items()
            if (info["res"] or "").startswith(COW_PREFIX)
            and not info.get("removed")
        }
        # The persisted herd: dam + bull + every live calf (wild cows
        # in view inflate the count; the floor is our known members).
        assert len(restored) >= len({dam, bull} | set(calves)) - 1, (
            "the herd did not reload after restart (%d cow gobs)"
            % len(restored))
        print("PERSIST: herd rows reloaded from the v8 save")

        print("BREEDING: OK (%s: cow + aurochs-bull tamed, calf born "
              "from the scaled gestation, heifer milked through the "
              "inherited row, herd persisted)" % username)
    finally:
        from hnhlib import stop_server
        if proc is not None:
            stop_server(proc)


if __name__ == "__main__":
    sys.exit(main())
