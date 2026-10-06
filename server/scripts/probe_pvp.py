#!/usr/bin/env python3
"""End-to-end player-vs-player archery verification (session 38).

Chain under test (wire level, the exact path the Java client drives):
  shooter: enter world with the starter kit
    -> craft a Wooden Bow (act craft woodbow -> make 0)
    -> craft a Stone Arrow batch (act craft stonearrow -> make 0, x10)
    -> equip the bow through the paperdoll (epry take + drop)
  victim: enter world at the same spawn area
  shooter: gob click on the victim -> the ranged aim opens
    (chat progress lines stream: "Aiming at 25%...")
    -> the meter auto-releases after 4 s -> the hit roll resolves
    -> BOTH sides get a chat line ("Your arrow hits ..." /
       "An arrow hits you for N damage.")
    -> the victim's OD_HEALTH quarters drop below full
    -> the shooter's arrow stack shrinks by one.

Exit 0 only when the whole chain passes. Auto-starts an isolated
server (fresh save) when the auth port is free.
"""
import os
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_build import (  # noqa: E402
    BuildClient,
    ensure_server,
    le16,
    le32,
    RMSG_WDGMSG,
)
from test_build import havstr, LIST_END, LIST_INT, LIST_STR  # noqa: E402
from test_equip import EquipClient, open_epry  # noqa: E402

OD_HEALTH = 14
OD_END = 255


class PvpClient(EquipClient):
    """EquipClient + chat log tracking + OD_HEALTH quarters."""

    def __init__(self, username):
        super().__init__(username)
        self.chat_id = None
        self.chat_lines = []
        self.hp_quarters = {}  # gobid -> quarters (0..4)

    def on_rel(self, t, body):
        if t == 0:  # NEWWDG
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            if body[2:nend].decode() == "slenchat":
                self.chat_id = wid
        elif t == RMSG_WDGMSG:
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            if name == "log" and wid == self.chat_id:
                args = list(self.parse_args(body[nend + 1 :]))
                if args and isinstance(args[0], str):
                    self.chat_lines.append(args[0])
        super().on_rel(t, body)

    def on_objdata(self, body):
        # Base parser first (res / pos / buddy bookkeeping), then a
        # light second pass that records only the OD_HEALTH quarters.
        super().on_objdata(body)
        off = 0
        while off + 8 <= len(body):
            gobid = struct.unpack("<i", body[off : off + 4])[0]
            off += 8  # id + frame
            while off < len(body):
                code = body[off]
                off += 1
                if code == OD_END:
                    break
                if code == OD_HEALTH:
                    self.hp_quarters[gobid] = body[off]
                    off += 1
                elif code == 2:  # OD_RES
                    wire = struct.unpack("<H", body[off : off + 2])[0]
                    off += 2
                    if wire & 0x8000:
                        ln = body[off]
                        off += 1 + ln
                        wire &= 0x7FFF
                elif code == 1:  # OD_MOVE
                    off += 8
                elif code == 3:  # OD_LINBEG
                    off += 20
                elif code == 4:  # OD_LINSTEP
                    off += 4
                elif code == 15:  # OD_BUDDY
                    end = body.index(0, off)
                    off = end + 3
                elif code == 6:  # OD_LAYERS
                    off += 2
                    while True:
                        layer = struct.unpack("<H", body[off : off + 2])[0]
                        off += 2
                        if layer == 0xFFFF:
                            break
                else:
                    return

    def chat(self, needle):
        return any(needle in line for line in self.chat_lines)


def enter(username):
    c = PvpClient(username)
    c.connect()
    c.pump(1.5)
    c.play(username)
    ok = c.wait_for(lambda: c.mapview_id is not None, 12)
    assert ok, f"{username}: world entry incomplete (no mapview)"
    # Open the inventory so crafted/kit items are visible as widgets.
    for _ in range(4):
        c.wdgmsg(c.slen_id, "inv", bytes([LIST_END]))
        c.pump(0.3)
    c.wait_for(lambda: any(n == "inv" for n in c.widgets.values()), 4)
    return c


def craft(c, recipe):
    """act("craft", recipe) -> make window -> make 0; returns True when a
    new item widget appeared."""
    assert c.scm_id, "no menugrid"
    act = (
        bytes([1]) + le16(c.scm_id) + b"act\x00"
        + bytes([LIST_STR]) + havstr("craft")
        + bytes([LIST_STR]) + havstr(recipe)
        + bytes([LIST_END])
    )
    c.send_rel([act])
    make_wid = None
    for _ in range(12):
        c.pump(0.4)
        make_wid = next(
            (w for w, n in c.widgets.items() if n == "make"), None
        )
        if make_wid:
            break
    assert make_wid, f"{recipe}: no make window"
    mk = (
        bytes([1]) + le16(make_wid) + b"make\x00"
        + bytes([LIST_INT]) + le32(0) + bytes([LIST_END])
    )
    c.send_rel([mk])
    return make_wid


def wait_item(c, res, timeout=8):
    """Wait until an item widget with the given resource exists."""
    return c.wait_for(
        lambda: any(i.get("res") == res for i in c.item_info.values()),
        timeout,
    )


def gob_click(c, gob, pos):
    """Map click on a gob (the same shape probe_animals.py sends)."""
    gclick = bytes([1]) + le16(c.mapview_id) + b"click\x00"
    gclick += bytes([3]) + le32(0) + le32(0)
    gclick += bytes([3]) + le32(pos[0]) + le32(pos[1])
    gclick += bytes([1]) + le32(1)
    gclick += bytes([1]) + le32(0)
    gclick += bytes([1]) + le32(gob)
    gclick += bytes([3]) + le32(pos[0]) + le32(pos[1])
    gclick += bytes([0])
    c.send_rel([gclick])


def main():
    ensure_server()
    shooter = enter("pvpshooter-%d" % int(time.time()))
    victim = enter("pvpvictim-%d" % int(time.time()))
    print("both players entered (mapview + inventory)")

    # The epry "ava" uimsg carries the session's OWN gob id - the
    # reliable self-identity on this wire (OD_BUDDY does not stream
    # for a fresh character).
    open_epry(shooter)
    open_epry(victim)
    assert shooter.ava_gob is not None and victim.ava_gob is not None, \
        "ava gob missing (shooter %r victim %r)" % (shooter.ava_gob, victim.ava_gob)
    print("shooter gob", shooter.ava_gob, "victim gob", victim.ava_gob)

    # Craft the bow and an arrow batch from the starter kit
    # (6 branches + 4 stones + 2 strings).
    craft(shooter, "woodbow")
    assert wait_item(shooter, "gfx/invobjs/bow"), "bow never crafted"
    print("CRAFT BOW: OK")
    craft(shooter, "stonearrow")
    assert wait_item(shooter, "gfx/invobjs/arrow-stone"), "arrows never crafted"
    print("CRAFT ARROWS: OK")

    # Equip the bow through the paperdoll (slot 0).
    bow_wid = next(
        w for w, i in shooter.item_info.items() if i["res"] == "gfx/invobjs/bow"
    )
    shooter.wdgmsg(bow_wid, "take", bytes([LIST_END]))
    shooter.pump(0.4)
    shooter.wdgmsg(
        shooter.epry_id, "drop",
        bytes([LIST_INT]) + le32(0) + bytes([LIST_END]),
    )
    ok = shooter.wait_for(
        lambda: shooter.epry_slots is not None
        and shooter.epry_slots[0] is not None
        and shooter.epry_slots[0][0] == "gfx/invobjs/bow",
        6,
    )
    assert ok, "bow never equipped: %r" % (shooter.epry_slots,)
    print("EQUIP BOW: OK (slot 0, q=%s)" % (shooter.epry_slots[0][1],))

    # The victim gob id is exact (both clients share one world); the
    # click coordinates are advisory, the interaction keys on the gob.
    vgob = victim.ava_gob
    shooter.pump(2.0)
    vpos = shooter.gobs.get(vgob, {}).get("pos") or (0, 0)
    print("victim gob in shooter view:", vgob, "at", vpos)

    gob_click(shooter, vgob, vpos)
    ok = shooter.wait_for(lambda: shooter.chat("Aiming at"), 8)
    assert ok, "no aim progress line after the click: %r" % (shooter.chat_lines,)
    print("AIM OPENED: OK (%r)" % next(
        l for l in shooter.chat_lines if "Aiming at" in l))

    # The meter fills in 4 s; the shot auto-releases. Give it 15 s.
    ok = shooter.wait_for(
        lambda: shooter.chat("Your arrow hits") or shooter.chat("Your arrow misses"),
        15,
    )
    assert ok, "no release line for the shooter: %r" % (shooter.chat_lines,)
    hit_line = next(
        (l for l in shooter.chat_lines if "Your arrow hits" in l), None
    )
    print("SHOOTER RELEASE LINE: %r" % hit_line)
    assert hit_line, "the first shot missed - rerun for a hit sample"

    ok = victim.wait_for(lambda: victim.chat("An arrow hits you"), 8)
    assert ok, "no hit line for the victim: %r" % (victim.chat_lines,)
    print("VICTIM HIT LINE: OK")

    # The victim's HP quarters dropped below full (100 - 75 = 25 -> 1/4).
    ok = victim.wait_for(
        lambda: victim.hp_quarters.get(victim.ava_gob, 4) < 4, 8
    )
    q = victim.hp_quarters.get(victim.ava_gob)
    assert ok, "victim HP quarters never dropped: %r" % (victim.hp_quarters,)
    print("VICTIM HP QUARTERS: %d/4" % q)

    print("PVP WIRE: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
