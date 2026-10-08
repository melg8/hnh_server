#!/usr/bin/env python3
"""End-to-end melee PvP verification (session 39).

Chain under test (wire level, the exact path the Java client drives):
  attacker: enter world -> gob click on the victim
    -> the flower menu opens ("sm" widget, Invite/Fight/Cancel petals)
    -> petal 1 ("Fight") arms the melee duel
    -> chat: "You attack <victim>!" on the attacker, "<attacker>
       attacks you!" on the victim
    -> the frv fight window opens on BOTH sides ("frv" widgets)
    -> the attacker chases into reach and swings; swings chip the
       victim's defence until an opening passes damage through:
       chat "You hit <victim> for N damage." on the attacker,
       "<attacker> hits you for N damage." on the victim
    -> the victim's OD_HEALTH quarters drop below full.

Exit 0 only when the whole chain passes. Auto-starts an isolated
server (fresh save) when the auth port is free.
"""
import os
import struct
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from hnhlib import (  # noqa: E402
    le16,
    le32,
    RMSG_WDGMSG,
    LIST_END,
    LIST_INT,
)
from probe_pvp import PvpClient, enter, gob_click  # noqa: E402


def flower_choose(c, choice):
    """Send `cl <choice>` to the open flower menu (widget type "sm")."""
    sm_wid = next((w for w, n in c.widgets.items() if n == "sm"), None)
    assert sm_wid is not None, "no flower menu open: %r" % (c.widgets,)
    msg = (
        bytes([1]) + le16(sm_wid) + b"cl\x00"
        + bytes([LIST_INT]) + le32(choice) + bytes([LIST_END])
    )
    c.send_rel([msg])
    return sm_wid


def main():
    from probe_pvp import ensure_server

    ensure_server()
    ts = int(time.time())
    attacker = enter("meleeatk-%d" % ts)
    victim = enter("meleevic-%d" % ts)
    print("both players entered (mapview + inventory)")

    from test_equip import open_epry

    open_epry(attacker)
    open_epry(victim)
    assert attacker.ava_gob is not None and victim.ava_gob is not None, \
        "ava gob missing (attacker %r victim %r)" % (
            attacker.ava_gob, victim.ava_gob)
    print("attacker gob", attacker.ava_gob, "victim gob", victim.ava_gob)

    # Open the melee duel through the real click path: click the victim
    # gob, confirm the Fight petal on the flower menu.
    vgob = victim.ava_gob
    attacker.pump(2.0)
    vpos = attacker.gobs.get(vgob, {}).get("pos") or (0, 0)
    print("victim gob in attacker view:", vgob, "at", vpos)

    gob_click(attacker, vgob, vpos)
    ok = attacker.wait_for(
        lambda: any(n == "sm" for n in attacker.widgets.values()), 8
    )
    assert ok, "no flower menu after the player click: %r" % (
        attacker.widgets,)
    print("FLOWER MENU: OK")

    flower_choose(attacker, 1)
    ok = attacker.wait_for(lambda: attacker.chat("You attack"), 8)
    assert ok, "no attack line for the attacker: %r" % (
        attacker.chat_lines,)
    print("DUEL OPENED: OK (%r)" % next(
        l for l in attacker.chat_lines if "You attack" in l))

    ok = victim.wait_for(lambda: victim.chat("attacks you"), 8)
    assert ok, "no attack line for the victim: %r" % (victim.chat_lines,)
    print("VICTIM NOTIFIED: OK")

    # The frv fight window opens on both sides.
    ok = attacker.wait_for(
        lambda: any(n == "frv" for n in attacker.widgets.values()), 8
    )
    assert ok, "no fight window for the attacker: %r" % (attacker.widgets,)
    ok = victim.wait_for(
        lambda: any(n == "frv" for n in victim.widgets.values()), 8
    )
    assert ok, "no fight window for the victim: %r" % (victim.widgets,)
    print("FRV WINDOWS: OK (both sides)")

    # The attacker chases into reach and swings; the opening economy
    # needs ~3 swings to break a full defence bar. Generous timeout:
    # chase + 3-4 swings.
    ok = attacker.wait_for(lambda: attacker.chat("You hit"), 60)
    assert ok, "no landed hit for the attacker: %r" % (
        attacker.chat_lines,)
    hit_line = next(l for l in attacker.chat_lines if "You hit" in l)
    print("ATTACKER HIT LINE: %r" % hit_line)

    ok = victim.wait_for(lambda: victim.chat("hits you for"), 8)
    assert ok, "no hit line for the victim: %r" % (victim.chat_lines,)
    print("VICTIM HIT LINE: OK")

    # The victim's HP quarters dropped below full (100 - 5 = 95 -> 3/4).
    ok = victim.wait_for(
        lambda: victim.hp_quarters.get(victim.ava_gob, 4) < 4, 8
    )
    q = victim.hp_quarters.get(victim.ava_gob)
    assert ok, "victim HP quarters never dropped: %r" % (victim.hp_quarters,)
    print("VICTIM HP QUARTERS: %d/4" % q)

    print("MELEE WIRE: OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
