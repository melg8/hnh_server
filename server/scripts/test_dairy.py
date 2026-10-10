#!/usr/bin/env python3
"""Session 83 live probe: the dairy chain (butter legs of the bake set).

The last three shipped doughs - Apple Pie, Carrot Cake, Raisin
Butter-cake - all gate on Butter, and Butter gates on a bucket of milk
from a tamed cow. This probe drives the whole vertical in one run:

  1. LP accrual buys Hunting (200) + Animal Husbandry (400).
  2. Rope (string x3, starter kit) is crafted and EQUIPPED (the taming
     gate's rope check).
  3. A wild Cow/Aurochs is quelled five times (jump x2 -> 2 IP, seize
     x10 -> 30 advantage, quell selection; +20 tameness each, 100 =
     fully tamed; Aurochs morphs into the domestic Cow).
  4. The tamed cow grazes (grass tile) and fills the milk meter at the
     HNH_MILK_RATE test scale; three flower-menu Milk draws collect
     three bucket-milk units.
  5. Butter: milk -> butter + empty bucket, three times.
  6. The three butter doughs over the probe's own produce:
     apples (tree picks), carrots (plant the starter seeds -> mature
     harvest), raisins (two grape handfuls -> hand recipe), flour
     (wheat crop -> quern grind), water (bucket fill at a water tile).
  7. Each dough bakes in the same oven (BAKE_MAP) and the baked label
     resolves its fep.conf eat row (Apple Pie=CON:5, Carrot
     Cake=PER:7, Raisin Butter-Cake=CHA:7).

Prints "DAIRY: OK ..." on success.
"""

import os
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import (  # noqa: E402
    LIST_COORD,
    LIST_END,
    LIST_INT,
    LIST_STR,
    REPO,
    RMSG_DSTWDG,
    RMSG_WDGMSG,
    TILE_SPAN,
    WireClient,
    ensure_server,
    enter_world,
    havstr,
    le32,
)

# Reuse the dough probe's transport legs (walk/craft/eat/bake helpers
# and the DoughClient food flag) - the dairy chain is the same
# choreography with a taming front end.
from test_dough import (  # noqa: E402
    APPLE_INV,
    APPLE_DROP,
    APPLE_LABEL,
    APPLETREE_RES,
    BRANCH_INV,
    BUCKETE_INV,
    DoughClient,
    bake_dough,
    craft_once,
    eat_item,
    ensure_flour,
    gather_branches,
    inv_total,
    request_neighborhood,
    walk_to_gob,
    walk_to_grass,
)
from test_bake import (  # noqa: E402
    FLOUR_INV,
    OVEN_RES,
    QUERN_RES,
    build_station,
    find_and_fill_water,
    grind_flour,
)

GRAPEVINE_RES = "gfx/terobjs/plants/wine"
GRAPE_DROP = "gfx/terobjs/items/grapes"
GRAPE_INV = "gfx/invobjs/grapes"
RAISINS_INV = "gfx/invobjs/raisins"
RAISINS_LABEL = "Raisins"

MILK_INV = "gfx/invobjs/bucket-milk"
MILK_LABEL = "Bucket of Milk"
BUTTER_INV = "gfx/invobjs/butter"
BUTTER_LABEL = "Butter"
SAW_INV = "gfx/invobjs/saw"
ROPE_INV = "gfx/invobjs/rope"

COW_RES = "gfx/kritter/cow/cdv"
AUROCHS_RES = "gfx/kritter/aurochs/cdv"

CARROT_SEEDS = "gfx/invobjs/seed-carrot"
CARROT_PLANT = "gfx/terobjs/plants/carrot"
CARROT_INV = "gfx/invobjs/carrot"

AP_DOUGH_INV = "gfx/invobjs/dough-pie-apple"
AP_INV = "gfx/invobjs/pie-apple"
AP_WORLD = "gfx/terobjs/items/pie-apple"
AP_LABEL = "Apple Pie"
CC_DOUGH_INV = "gfx/invobjs/dough-cake-carrot"
CC_INV = "gfx/invobjs/cake-carrot"
CC_WORLD = "gfx/terobjs/items/cake-carrot"
CC_LABEL = "Carrot Cake"
RBC_DOUGH_INV = "gfx/invobjs/dough-cake-raisinbutter"
RBC_INV = "gfx/invobjs/cake-raisinbutter"
RBC_WORLD = "gfx/terobjs/items/cake-raisinbutter"
RBC_LABEL = "Raisin Butter-Cake"

# One bucket (100 milk units) per ~100 ticks at HNH_MILK_RATE=6000
# (legacy needs 60000 ticks = 100 real minutes; see state::milk_rate).
MILK_RATE = "6000"




def set_run_gait(c):
    """Speedget setspeed 3 (run, 66 subtiles/tick): the taming chase
    must outrun the fleeing aurochs (30) or the quell selection never
    gets inside the swing REACH (33) to resolve."""
    wid = c.widget_by_name("speedget")
    assert wid is not None, "speedget widget missing"
    c.wdgmsg(
        wid, "set", bytes([LIST_INT]) + le32(3) + bytes([LIST_END]))
    c.pump(0.3)


def ensure_dairy_server():
    """Fast-crop + fast-milk + fast-leash + fast-LP isolated server on
    a per-run save file (LP rate is hnhlib's default 1000x)."""
    return ensure_server(
        env_extra={
            "HNH_CROP_TIME_SCALE": "10000000",
            "HNH_MILK_RATE": MILK_RATE,
            # 4 s of ticks between quell cycles (legacy: 10 real
            # minutes per cycle x 5 = no e2e could sit it out).
            "HNH_LEASH_TICKS": "40",
        },
        save_path=os.path.join(
            REPO, "server", "target",
            "dairy-test-save-%d.json" % (os.getpid() % 100000),
        ),
    )


class DairyClient(DoughClient):
    """DoughClient + the epry paperdoll tracking (the rope equip flow,
    lifted from the equip probe's client) + the frv Fightview tracking
    (session 83: the taming choreography needs to know WHICH beast the
    live duel is with)."""

    def __init__(self, username):
        WireClient.__init__(self, username)
        self.food_msg = False
        self.epry_id = None
        self.epry_slots = None
        # Fightview (frv) tracking: the widget's live relation set and
        # the focused relation, mirrored from the widget's own
        # "new"/"del"/"cur" uimsg stream (args[0] is the target gob).
        self.frv_id = None
        self.frv_rels = set()
        self.frv_cur = None

    def _parse_set(self, args):
        slots = []
        o = 0
        while len(slots) < 16 and o < len(args):
            res = args[o]
            o += 1
            if isinstance(res, int) and res >= 0:
                ql = args[o] if o < len(args) else None
                o += 1
                tip = ""
                if o < len(args) and isinstance(args[o], str):
                    tip = args[o]
                    o += 1
                slots.append((self.resids.get(res, "?%d" % res), ql, tip))
            else:
                slots.append(None)
        while len(slots) < 16:
            slots.append(None)
        self.epry_slots = slots

    def on_rel(self, t, body):
        if t == 0:  # NEWWDG
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            if name == "epry":
                self.epry_id = wid
            elif name == "frv":
                self.frv_id = wid
                self.frv_rels = set()
                self.frv_cur = None
        elif t == RMSG_DSTWDG:
            wid = struct.unpack("<H", body[0:2])[0]
            if wid == self.frv_id:
                self.frv_id = None
                self.frv_rels = set()
                self.frv_cur = None
        elif t == RMSG_WDGMSG:
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            if wid == self.epry_id and name == "set":
                args = list(self.parse_args(body[nend + 1:]))
                self._parse_set(args)
            elif wid == self.frv_id and name in ("new", "del", "cur"):
                args = list(self.parse_args(body[nend + 1:]))
                if args and isinstance(args[0], int):
                    if name == "new":
                        self.frv_rels.add(args[0])
                        self.frv_cur = args[0]
                    elif name == "del":
                        self.frv_rels.discard(args[0])
                        if self.frv_cur == args[0]:
                            self.frv_cur = next(iter(self.frv_rels), None)
                    elif name == "cur":
                        self.frv_cur = args[0]
        super().on_rel(t, body)


def wait_for_lp(c, want, timeout=45):
    """Re-open the char sheet until the pushed `exp` balance reaches
    `want` (the LP accrual lands on every sheet open; HNH_LP_RATE=1000
    makes 620 LP arrive in ~20 s)."""
    slen_wid = next(w for w, n in c.widgets.items() if n == "slen")
    deadline = time.time() + timeout
    while time.time() < deadline:
        c.wdgmsg(slen_wid, "chr", bytes([LIST_END]))
        c.pump(1.0)
        if c.exp_seen is not None and c.exp_seen >= want:
            return c.exp_seen
        c.pump(1.0)
    raise AssertionError(
        "LP %s never reached %d" % (c.exp_seen, want))


def buy_skill(c, name, cost):
    """chr buy <skill>: the char sheet must be open first."""
    slen_wid = next(w for w, n in c.widgets.items() if n == "slen")
    c.wdgmsg(slen_wid, "chr", bytes([LIST_END]))
    ok = c.wait_for(lambda: c.chr_id is not None, 5)
    assert ok, "char sheet never opened"
    c.wdgmsg(
        c.chr_id, "buy",
        bytes([LIST_STR]) + havstr(name) + bytes([LIST_END]))
    ok = c.wait_for(
        lambda: any(("learned" in t or "already know" in t)
                    for t, _ in c.chat_lines), 6)
    assert ok, "buy %s never answered (chat=%r)" % (name, c.chat_lines[-4:])
    print("skill bought: %s (%d LP)" % (name, cost))


def equip_rope(c):
    """Open the paperdoll, take the rope onto the cursor, drop it into
    equipment slot 0 (the weapon slot - the taming gate scans every
    equip slot, but the docs name the rope-as-weapon)."""
    assert c.slen_id is not None, "slen widget missing"
    for _ in range(4):
        c.wdgmsg(c.slen_id, "equ", bytes([LIST_END]))
        c.pump(0.3)
    ok = c.wait_for(lambda: c.epry_id is not None and c.epry_slots is not None, 6)
    assert ok, "epry paperdoll never arrived"
    for _attempt in range(4):
        rope_wid = c.find_item_by_res(ROPE_INV)
        assert rope_wid is not None, "no rope item widget to equip"
        c.wdgmsg(rope_wid, "take", bytes([LIST_END]))
        ok = c.wait_for(
            lambda: any(n == "item" for n in c.widgets.values()), 4)
        assert ok, "the rope never reached the cursor"
        c.pump(0.3)
        c.wdgmsg(
            c.epry_id, "drop",
            bytes([LIST_INT]) + le32(0) + bytes([LIST_END]))
        if c.wait_for(
            lambda: c.epry_slots is not None
            and c.epry_slots[0] is not None
            and c.epry_slots[0][0] == ROPE_INV, 6):
            break
        c.pump(0.5)
    else:
        raise AssertionError("slot 0 never received the rope")
    print("rope equipped into slot 0 (q=%s)" % (c.epry_slots[0][1],))


AGGRESSIVE_RES = (
    "gfx/kritter/boar/",
    "gfx/kritter/wolf/",
    "gfx/kritter/bear/",
)


def find_cow(c):
    """The nearest wild Cow or Aurochs in the loaded grids (both are
    the milk chain's front end - the aurochs morphs into the cow at
    full tameness). Animals spawn with an OD_RES cdv followed by the
    compositing OD_LAYERS whose base is gfx/kritter/<species>/body -
    the LAYERS base overwrites the recorded res, so the scan matches
    the kritter folder prefix, not the exact cdv path.
    Session 83: cows standing inside a boar/wolf/bear's aggro radius
    (~300 subtiles) are SKIPPED when any alternative exists - an
    aggressive chaser forces a duel on the tamer mid-protocol and the
    one-Fightview rule keeps it stuck until the chase is outrun."""
    found = {
        g: info
        for g, info in c.gobs.items()
        if (info["res"] or "").startswith(
            ("gfx/kritter/cow/", "gfx/kritter/aurochs/"))
        and not info.get("removed")
    }
    assert found, "no cow/aurochs spawned in the loaded grids (res=%r)" % (
        sorted({i["res"] for i in c.gobs.values() if i["res"]})[:12],)
    px, py = c.gobs[c.player_gob]["pos"]
    hostiles = [
        info["pos"] for info in c.gobs.values()
        if (info["res"] or "").startswith(AGGRESSIVE_RES)
        and not info.get("removed") and info["pos"]
    ]
    def hostile_dist(pos):
        return min(
            (abs(pos[0] - h[0]) + abs(pos[1] - h[1]) for h in hostiles),
            default=1 << 30)
    # Prefer a beast outside every hostile's ~300-subtile aggro circle.
    calm = [g for g in found if hostile_dist(found[g]["pos"]) > 400]
    pool = calm or list(found)
    if not calm:
        print("WARN: every cow stands inside an aggro circle; "
              "taking the nearest and relying on the sprint break")
    gid = min(
        pool,
        key=lambda g: abs(found[g]["pos"][0] - px)
        + abs(found[g]["pos"][1] - py))
    return gid


def close_on_beast(c, gid, want=30, deadline=30.0):
    """Pursue a fleeing beast to within `want` subtiles of its LIVE
    streamed position (session 83). The beast's out-of-fight panic hops
    (165 + jitter at the cow's 30 subt/s) lose ground to a run player
    (+20 subt/s) ONLY while the walk target keeps REFRESHING: a single
    snapshot walk lands where the beast USED to be, and the follow-up
    attack click is then refused by the server's swing-reach gate ("too
    far away"). Short re-aimed walk bursts until the live distance
    closes inside `want`."""
    end = time.time() + deadline
    last = 0.0
    while time.time() < end:
        bpos = c.live_pos(gid)
        ppos = c.live_pos(c.player_gob)
        if bpos is None or ppos is None:
            c.pump(0.3)
            continue
        dist = abs(bpos[0] - ppos[0]) + abs(bpos[1] - ppos[1])
        if time.time() - last > 2.0:
            last = time.time()
            print("CHASE: player=%s beast=%s dist=%d frv=%r" % (
                ppos, bpos, dist, c.frv_cur))
        if dist <= want:
            return True
        # A few click segments toward the beast's CURRENT position, then
        # re-read: the gap closes at the run-vs-panic speed margin.
        c.nav_walk(bpos, stop=want, max_clicks=3, log=lambda m: None)
    return False


def break_hostile_fight(c, deadline=45.0):
    """Sprint away from a FORCED duel (an aggressive boar/wolf reached
    swing reach before our click: the one-Fightview rule means the
    server keeps that fight until it ends). Run top gait is 66
    subtiles/s against the boar's 55: the gap grows ~11/s, and the
    combat pass tears the fight down once an axis distance passes
    DISENGAGE (300 subtiles) - about 30 s of sprinting. The frv widget
    disappearing is the break signal."""
    end = time.time() + deadline
    while time.time() < end:
        if c.frv_id is None or not c.frv_rels:
            return True
        hostile = c.frv_cur or next(iter(c.frv_rels))
        hpos = c.live_pos(hostile)
        ppos = c.live_pos(c.player_gob)
        if not (hpos and ppos):
            c.pump(0.5)
            continue
        dx, dy = ppos[0] - hpos[0], ppos[1] - hpos[1]
        d = max(abs(dx) + abs(dy), 1)
        # Run directly away from the hostile's LIVE position in short
        # re-aimed bursts (the chaser keeps closing otherwise).
        far = (ppos[0] + dx * 700 // d, ppos[1] + dy * 700 // d)
        c.nav_walk(far, stop=60, max_clicks=2, log=lambda m: None)
    return c.frv_id is None or not c.frv_rels


def open_fight(c, gid, tries=6):
    """Close in on the beast and click (opens the fight window).
    Session 83: the server opens melee engagement only from swing reach
    (33 subtiles - the same gate the swing cadence applies), so the
    approach must land INSIDE that radius against the beast's LIVE
    position: pursue with refreshing walk targets (close_on_beast),
    then click. The frv window confirms the fight actually lives AND
    is with the WANTED beast: an aggressive chaser may have forced a
    duel on us first (one Fightview - our click is then refused), so
    a mismatched window is sprint-broken and the approach retried."""
    for _ in range(tries):
        pos = c.live_pos(gid)
        if pos is None:
            raise AssertionError("the beast vanished from the view")
        if not close_on_beast(c, gid, want=30, deadline=30.0):
            # The beast crossed water/rock or outran the deadline: wait
            # a beat for it to wander somewhere reachable.
            c.pump(2.0)
            continue
        if c.frv_id is not None and c.frv_rels and c.frv_cur != gid:
            # A boar/wolf forced its duel on us before the click: the
            # server (correctly) refuses to open ours while it lives.
            break_hostile_fight(c)
            if not close_on_beast(c, gid, want=30, deadline=15.0):
                continue
        c.click_gob(gid, c.live_pos(gid) or c.gobs[gid]["pos"])
        if c.wait_for(lambda: c.frv_cur == gid, 3):
            return
        ppos = c.live_pos(c.player_gob)
        cpos = c.live_pos(gid)
        print("OPEN-FIGHT retry: frv_id=%s rels=%r cur=%r dist=%s chat=%r" % (
            c.frv_id, sorted(c.frv_rels), c.frv_cur,
            None if not (ppos and cpos) else (
                abs(ppos[0] - cpos[0]) + abs(ppos[1] - cpos[1])),
            [t for t, _ in c.chat_lines[-4:]]))
        c.pump(1.0)
    raise AssertionError("the fight never stayed open (skittish beast)")


def quell_cycle(c):
    """Five quell rounds: close in on the beast, click (opens the
    fight), build the maneuver pool (jump x2 -> 2 IP, seize x10 ->
    advantage 30), select quell, wait for the colored tameness line.
    The offence bar survives between fights (fight_open never resets
    it), so every round resolves within a second.

    Between rounds the beast walks the HNH_LEASH_TICKS window on its
    leash and then breaks it (chat line); a click before the break
    hits the collection menu instead of a fight, so the loop waits
    for the break before re-opening the fight."""
    gid = find_cow(c)
    set_run_gait(c)
    for rnd in range(5):
        # The beast wanders between rounds: re-close and re-open the
        # fight, confirming the frv window this time (a click from
        # beyond DISENGAGE is closed by the tick before the client
        # ever sees the window).
        open_fight(c, gid)
        for _ in range(2):
            c.menu_act("atk", "jump")
            c.pump(0.15)
        for _ in range(10):
            c.menu_act("atk", "seize")
            c.pump(0.15)
        # Let the battle cool: a beast bite mid-approach heats the fight
        # (+2500 intensity) and the quell gate demands intensity 0; the
        # decay is 250/tick, so ~1.5 s of quiet pumping clears a bite.
        c.pump(1.5)
        c.menu_act("atk", "quell")
        # The quell only RESOLVES on a swing tick inside REACH (33
        # subtiles). While the fight lives the server chases the
        # fleeing beast FOR the player (tick_combat re-aims
        # start_move at the beast every time the player's move
        # completes), and the beast's in-fight panic hops stay small
        # (session 83), so the run gait closes the gap on its own.
        # DO NOT nav_walk here: a tile click REPLACES the server's
        # chase target with a point at the beast's position when the
        # click was made - the gap stops closing and the quell never
        # resolves. Just pump and let the server's chase run.
        # A refused selection ("too heated" - a landed seize swing
        # added +2500 intensity) leaves atk_cur EMPTY: with the
        # session-83 no-attack-no-swing rule the duel then idles
        # safely, so simply RE-SELECT every couple of seconds until
        # the gate takes and the quell swing lands.
        want = "Tameness: %d/100" % ((rnd + 1) * 20)
        deadline = time.time() + 60
        rearm = 0.0
        while not any(want in t for t, _ in c.chat_lines):
            assert time.time() < deadline, (
                "quell round %d never landed (want %r; chat=%r)"
                % (rnd + 1, want, c.chat_lines[-6:]))
            c.pump(0.5)
            heated = any(
                "too heated" in t for t, _ in c.chat_lines[-3:])
            if heated and time.time() - rearm > 2.0:
                rearm = time.time()
                c.menu_act("atk", "quell")
        print("QUELL %d/5: %s" % (rnd + 1, want))
        if rnd < 4:
            # The quelled beast follows on its leash, then breaks it
            # (HNH_LEASH_TICKS=40 -> ~4 s) and turns wild again: wait
            # for the break line before the next fight (before it the
            # click opens the Milk menu, not a fight).
            ok = c.wait_for(
                lambda: any("breaks its leash" in t for t, _ in c.chat_lines), 20)
            assert ok, "the leash never broke (chat=%r)" % (c.chat_lines[-6:],)
    ok = c.wait_for(
        lambda: any(
            ("fully tamed" in t) or ("domestic form" in t)
            for t, _ in c.chat_lines), 6)
    assert ok, "the full-tame line never arrived (chat=%r)" % (
        c.chat_lines[-6:],)
    print("TAMING: fully tamed (5 quells, rope equipped, AH bought)")
    return gid


def milk_cycle(c, gid, draws=3):
    """Flower-menu Milk draws: wait for the meter (grazing fills a
    bucket every ~100 ticks at the test scale), click, choose Milk,
    verify the bucket-milk item. The empty bucket returns with every
    butter craft, so one bucket serves every draw."""
    got = 0
    while got < draws:
        c.pump(3.0)
        c.click_gob(gid, c.gobs[gid]["pos"])
        ok = c.wait_for(lambda: c.sm_wid is not None, 6)
        assert ok, "the cow menu never opened (chat=%r)" % (
            c.chat_lines[-4:],)
        if "Milk" not in c.sm_opts:
            # "The cow has no milk yet" - the meter is still filling
            # (or the beast grazed onto a non-grass tile; it wanders).
            c.flower_choice(c.sm_wid, 0)  # cancel
            c.pump(4.0)
            continue
        before = inv_total(c, MILK_INV)
        c.flower_choice(c.sm_wid, c.sm_opts.index("Milk"))
        ok = c.wait_for(
            lambda: inv_total(c, MILK_INV) > before, 8)
        assert ok, "the milk never landed (items=%r)" % (
            [(i["res"], i["tt"]) for i in c.item_info.values()],)
        got += 1
        print("MILK DRAW %d/%d: %s in inventory" % (got, draws, MILK_LABEL))
    assert inv_total(c, MILK_INV) >= draws, "milk quota unmet"


def butter_leg(c, gid, want=3):
    """Milk -> butter x3 (each craft consumes one bucket-milk and
    returns the empty bucket for the next draw)."""
    craft_once(c, "saw", SAW_INV)
    craft_once(c, "bucket", BUCKETE_INV)
    milk_cycle(c, gid, draws=want)
    while inv_total(c, BUTTER_INV) < want:
        craft_once(c, "butter", BUTTER_INV)
    assert inv_total(c, BUTTER_INV) >= want, "butter quota unmet"
    print("BUTTER: crafted x%d (buckets returned each craft)" % want)


def apple_leg(c, want=2):
    """Two apple picks off the nearest apple tree (the dairy set needs
    two units for the apple-pie dough)."""
    gid, _ = walk_to_gob(c, APPLETREE_RES, "apple tree")
    got = 0
    while got < want:
        seen = {
            g for g, info in c.gobs.items()
            if info["res"] == APPLE_DROP and not info.get("removed")}
        before = inv_total(c, APPLE_INV)
        c.click_gob(gid, c.gobs[gid]["pos"])
        ok = c.wait_for(
            lambda: any(
                g not in seen and not info.get("removed")
                for g, info in c.gobs.items()
                if info["res"] == APPLE_DROP), 6)
        assert ok, "no apple drop spawned"
        drop_id = sorted(
            g for g, info in c.gobs.items()
            if info["res"] == APPLE_DROP and not info.get("removed")
            and g not in seen)[0]
        c.click_gob(drop_id, c.gobs[drop_id]["pos"])
        ok = c.wait_for(lambda: inv_total(c, APPLE_INV) > before, 8)
        assert ok, "the apple never reached the inventory"
        got = inv_total(c, APPLE_INV)
    print("APPLES: %d picked" % got)
    assert got >= want


def grape_and_raisin_leg(c):
    """Two grape handfuls (3 units each) -> two raisin packs."""
    from test_dough import pick_and_pickup  # local import: same module
    for _ in range(2):
        pick_and_pickup(
            c, GRAPEVINE_RES, GRAPE_DROP, GRAPE_INV, "Grapes", 3)
    craft_once(c, "raisins", RAISINS_INV)
    assert inv_total(c, RAISINS_INV) >= 2, "raisins quota unmet"
    print("RAISINS: crafted x2 (two grape handfuls)")


def carrot_leg(c, want=2):
    """Plant the five starter Carrot Seeds, wait for maturity, harvest
    through the flower menu (mature yield 1-3 carrots + seeds)."""
    from test_bake import buy_farming_value
    buy_farming_value(c)
    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // TILE_SPAN, ppos[1] // TILE_SPAN)
    seed_wid = c.find_item_by_tooltip("Carrot Seeds")
    assert seed_wid is not None, "starter carrot seeds missing"

    planted = 0
    for dx in range(-2, 3):
        for dy in range(-2, 3):
            if planted >= 5 or abs(dx) + abs(dy) > 3:
                continue
            tile = (ptile[0] + dx, ptile[1] + dy)
            c.arm_plow()
            c.pump(0.25)
            c.click_tile(tile)
            c.pump(0.3)
            c.take_item(seed_wid)
            c.pump(0.25)
            c.map_itemact_tile(tile)
            if c.wait_for(
                lambda: any(
                    (info["res"] or "") == CARROT_PLANT
                    for info in c.gobs.values()), 2.5):
                planted += 1
                c.pump(0.2)
    crops = [g for g, i in c.gobs.items() if i["res"] == CARROT_PLANT]
    assert len(crops) >= 2, "too few carrots planted (%d)" % len(crops)
    print("planted %d carrots" % len(crops))

    deadline = time.time() + 15
    for gob in crops:
        while c.gobs[gob]["sdt"] != b"\x03":
            assert time.time() < deadline, "carrot %s never matured" % gob
            c.pump(0.25)
    print("all carrots mature")

    for gob in crops:
        if inv_total(c, CARROT_INV) >= want + 2:
            break
        c.click_gob(gob, c.gobs[gob]["pos"])
        ok = c.wait_for(lambda: c.sm_wid is not None, 4)
        assert ok, "carrot harvest menu never opened"
        c.flower_choice(c.sm_wid)
        c.pump(0.5)
    total = inv_total(c, CARROT_INV)
    assert total >= want, "carrot quota unmet (%d < %d)" % (total, want)
    print("CARROTS: harvested %d (+%d seeds back)" % (
        total, inv_total(c, CARROT_SEEDS)))


def butter_dough_leg(c, quern, oven, dough_inv, world_res, label,
                     recipe, what):
    """One butter dough: fuel + water fill + flour quota, craft, bake
    in the shared oven, eat the baked label's fep row."""
    gather_branches(c, inv_total(c, BRANCH_INV) + 2)
    assert inv_total(c, BUCKETE_INV) >= 1, "no empty bucket for water"
    find_and_fill_water(c)
    ensure_flour(c, quern[0], quern[1])
    assert inv_total(c, BUTTER_INV) >= 1, "butter spent by an earlier leg"
    before_butter = inv_total(c, BUTTER_INV)
    craft_once(c, recipe, dough_inv)
    assert inv_total(c, dough_inv) >= 2, "the %s made < 2" % what
    assert inv_total(c, BUTTER_INV) == before_butter - 1, (
        "the %s craft must consume exactly one butter" % what)
    print("%s DOUGH: crafted x%d (one butter consumed)" % (
        what.upper(), inv_total(c, dough_inv)))
    bake_dough(c, oven[0], oven[1], dough_inv, world_res, label, what)
    eat_item(c, label, what)
    print("EAT %s: food uimsg seen (%s row live)" % (
        what.upper(), label))


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "dairy%d" % (
        int(time.time()) % 100000,)
    proc = ensure_dairy_server()
    try:
        c = enter_world(username, client_cls=DairyClient)
        request_neighborhood(c)

        # 1. Skills: Hunting + Animal Husbandry (the quell gates).
        wait_for_lp(c, 620)
        buy_skill(c, "hunting", 200)
        buy_skill(c, "ahusb", 400)

        # 2. Rope: the third starter string spins the taming gate.
        craft_once(c, "rope", ROPE_INV)
        equip_rope(c)

        # 3-4. Taming + milking + butter (the dairy vertical).
        gid = quell_cycle(c)
        butter_leg(c, gid, want=3)

        # 5. The butter doughs' own produce.
        apple_leg(c, want=2)
        grape_and_raisin_leg(c)
        carrot_leg(c, want=2)

        # 6. Stations: one quern + one oven serve all three doughs
        #    (built once, reused like the dough probe's pie leg).
        gather_branches(c, inv_total(c, BRANCH_INV) + 4)
        (quern, qmc) = build_station(
            c, "quern", QUERN_RES,
            [("gfx/invobjs/stone", 2), ("gfx/invobjs/branch", 1)])
        (oven, omc) = build_station(
            c, "oven", OVEN_RES,
            [("gfx/invobjs/stone", 2), ("gfx/invobjs/branch", 1)])

        # 7. The three butter doughs: craft -> bake -> eat, in one oven.
        butter_dough_leg(
            c, (quern, qmc), (oven, omc), AP_DOUGH_INV, AP_WORLD,
            AP_LABEL, "apdough", "apple pie")
        butter_dough_leg(
            c, (quern, qmc), (oven, omc), CC_DOUGH_INV, CC_WORLD,
            CC_LABEL, "ccdough", "carrot cake")
        butter_dough_leg(
            c, (quern, qmc), (oven, omc), RBC_DOUGH_INV, RBC_WORLD,
            RBC_LABEL, "rbcdough", "raisin butter-cake")

        print("DAIRY: OK (%s: cow tamed + milked, butter x3,"
              " apple pie + carrot cake + raisin butter-cake baked"
              " + eaten)" % username)
    finally:
        from hnhlib import stop_server
        if proc is not None:
            stop_server(proc)


if __name__ == "__main__":
    sys.exit(main())
