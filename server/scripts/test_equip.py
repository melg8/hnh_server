#!/usr/bin/env python3
"""End-to-end equipment (paperdoll) verification against a running server.

Flow (docs/mechanics/items/items-and-quality.md, Equipment section):
  1. Auth + session + world entry (same wire path as test_client.py).
  2. The bootstrap must carry the `epry` widget with a full "set" sync
     (16 slots, all empty for a fresh character) and the "ava" gob id.
  3. slen "equ" re-request opens/resyncs the same window.
  4. Take an inventory stack onto the cursor ("take" on the item widget),
     drop it onto slot 3 ("drop" on epry): the next "set" must show the
     item in slot 3 (res + quality + label tooltip).
  5. "take" slot 3 back: the slot empties and the stack returns to the
     cursor.
  6. Re-equip into slot 5 and disconnect; after a graceful server restart
     the persisted character must re-enter with slot 5 occupied
     (`persistcheck` mode).

Modes: `equipbot` (steps 1-6 up to the disconnect, EQUIP FLOW: OK),
`persistcheck` (re-login assertion, EQUIP PERSIST: OK).
"""
import os
import socket
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
from test_build import havstr, LIST_END, LIST_INT  # noqa: E402


def enter_equip_world(username):
    """enter_world from test_build, but tracking the epry widget state."""
    c = EquipClient(username)
    c.connect()
    print("session accepted")
    c.pump(1.5)
    c.play(username)
    ok = c.wait_for(
        lambda: c.mapview_id is not None and c.player_gob is not None, 12
    )
    if not ok and c.player_gob is None:
        mine = [
            g for g, info in c.gobs.items() if info["res"] == "gfx/borka/body"
        ]
        c.player_gob = mine[0] if mine else None
    assert ok or c.player_gob is not None, "world entry incomplete"
    assert c.slen_id is not None, "slen widget missing"
    # Open the inventory so an item can be taken onto the cursor.
    for _ in range(4):
        c.wdgmsg(c.slen_id, "inv", bytes([LIST_END]))
        c.pump(0.3)
    c.wait_for(lambda: any(n == "inv" for n in c.widgets.values()), 4)
    print("world entry: player gob", c.player_gob)
    return c


class EquipClient(BuildClient):
    """BuildClient + epry widget/slot tracking."""

    def __init__(self, username):
        super().__init__(username)
        self.epry_id = None
        self.epry_slots = None  # list of 16 entries: None or (resname, ql, tip)
        self.ava_gob = None

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
        self.epry_slots = slots

    def on_rel(self, t, body):
        # Record epry state before/after the base class bookkeeping.
        if t == 0:  # NEWWDG
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            if body[2:nend].decode() == "epry":
                self.epry_id = wid
        elif t == RMSG_WDGMSG:
            wid = struct.unpack("<H", body[0:2])[0]
            nend = body.index(0, 2)
            name = body[2:nend].decode()
            if wid == self.epry_id and name in ("set", "ava"):
                args = list(self.parse_args(body[nend + 1 :]))
                if name == "set":
                    self._parse_set(args)
                elif args and isinstance(args[0], int):
                    self.ava_gob = args[0]
        super().on_rel(t, body)


def open_epry(c):
    """Request the paperdoll through the slen button and wait for the sync."""
    assert c.slen_id is not None, "slen widget missing"
    for _ in range(4):
        c.wdgmsg(c.slen_id, "equ", bytes([LIST_END]))
        c.pump(0.3)
    ok = c.wait_for(
        lambda: c.epry_slots is not None and len(c.epry_slots) == 16 and c.ava_gob is not None,
        6,
    )
    assert ok, "epry widget with full 'set' + 'ava' never arrived"
    assert all(s is None for s in c.epry_slots), (
        "fresh character must start with an empty doll, got %r" % (c.epry_slots,)
    )
    print("epry: 16 slots + ava gob", c.ava_gob)


def equip_item(c, slot, expected_res):
    """Take the named stack onto the cursor and equip it into `slot`."""
    item_wid = next(
        (w for w, i in c.item_info.items() if i["res"] == expected_res), None
    )
    assert item_wid is not None, "no %s item widget to equip" % expected_res
    c.wdgmsg(item_wid, "take", bytes([LIST_END]))
    c.wait_for(lambda: any(n == "item" for n in c.widgets.values()), 4)
    c.pump(0.3)
    c.wdgmsg(c.epry_id, "drop", bytes([LIST_INT]) + le32(slot) + bytes([LIST_END]))
    ok = c.wait_for(
        lambda: c.epry_slots is not None
        and c.epry_slots[slot] is not None
        and c.epry_slots[slot][0] == expected_res,
        6,
    )
    assert ok, "slot %d never received %s: %r" % (slot, expected_res, c.epry_slots)
    ql, tip = c.epry_slots[slot][1], c.epry_slots[slot][2]
    assert isinstance(ql, int) and ql > 0, "quality must be positive"
    print("equipped %s into slot %d (q=%s, tip=%r)" % (expected_res, slot, ql, tip))
    return item_wid


def unequip_item(c, slot, expected_res):
    c.wdgmsg(c.epry_id, "take", bytes([LIST_INT]) + le32(slot) + bytes([LIST_END]))
    ok = c.wait_for(lambda: c.epry_slots is not None and c.epry_slots[slot] is None, 6)
    assert ok, "slot %d never emptied after take" % slot
    print("unequipped %s from slot %d" % (expected_res, slot))
    # The stack sits on the cursor; drop it back into the inventory window
    # (the server moves the cursor stack back into storage) so the next
    # equip can find it.
    inv_wid = next((w for w, n in c.widgets.items() if n == "inv"), None)
    assert inv_wid is not None, "inventory window missing"
    c.wdgmsg(inv_wid, "drop", bytes([LIST_END]))
    ok = c.wait_for(
        lambda: any(i["res"] == expected_res for i in c.item_info.values()), 6
    )
    assert ok, "%s never returned to the inventory" % expected_res


def run_equipbot():
    c = enter_equip_world("equipbot")
    open_epry(c)
    # slot 3: equip, verify, unequip.
    equip_item(c, 3, "gfx/invobjs/branch")
    unequip_item(c, 3, "gfx/invobjs/branch")
    # slot 5: equip again and leave it equipped for the persistence check.
    equip_item(c, 5, "gfx/invobjs/branch")
    # Give the server a moment, then drop the session so the graceful
    # shutdown path (SIGTERM from the gate script) snapshots the character.
    c.pump(1.0)
    c.sock.close()
    print("EQUIP FLOW: OK")


def run_persistcheck():
    c = enter_equip_world("equipbot")
    open_epry_prefilled = c.wait_for(
        lambda: c.epry_slots is not None
        and len(c.epry_slots) == 16
        and c.epry_slots[5] is not None
        and c.epry_slots[5][0] == "gfx/invobjs/branch",
        6,
    )
    assert open_epry_prefilled, (
        "restored character must re-enter with slot 5 equipped: %r" % (c.epry_slots,)
    )
    print("persisted equipment restored: slot 5 =", c.epry_slots[5])
    print("EQUIP PERSIST: OK")


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "all"
    if mode in ("equipbot", "all"):
        run_equipbot()
    if mode in ("persistcheck", "all"):
        run_persistcheck()
    return 0


if __name__ == "__main__":
    sys.exit(main())
