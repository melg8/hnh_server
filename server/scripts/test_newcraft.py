#!/usr/bin/env python3
"""Session 58 breadth batch: wire-level craft probe on the hnhlib harness.

Drives the FULL making protocol for the new recipes through the real
UDP path (the same flow test_craft.py drives for the stone axe):

  1. act("craft", "saw") -> `make` widget + `pop` (Branch x2 + Stone x1
     -> Saw) -> `make 0` -> the Saw item widget appears in the inventory.
  2. act("craft", "bucket") with the crafted saw present -> the Bucket
     lands: proves the session-46 tool gate accepts a CRAFTED tool.
  3. The fork paginae are served by the resource HTTP server with a
     valid res signature (string.res, tanhide.res) - the pages the
     client needs to render the new menu entries.

The tanhide/string recipe logic itself is covered by the cargo unit
battery (leather_chain_tans_and_consumes, string_spins_from_flax_fibres);
this probe is the wire-contract half of the AGENTS.md pyramid.

Usage: python3 test_newcraft.py [username]
Prints "NEWCRAFT: OK ..." on success.
"""
import os
import sys
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hnhlib import enter_world  # noqa: E402

LIST_END, LIST_INT = 0, 1
RES_HTTP = "http://127.0.0.1:1872"


def le32(v):
    import struct
    return struct.pack("<i", v)


def craft_once(c, recipe_id, expect_res, mode=0):
    """Open the make window, press craft, wait for the output item."""
    c.menu_act("craft", recipe_id)
    if not c.wait_for(lambda: any(n == "make" for n in c.widgets.values()), 8):
        return False, "no make widget for %s" % recipe_id
    make_wid = max(w for w, n in c.widgets.items() if n == "make")
    c.pump(0.3)
    c.wdgmsg(make_wid, "make", bytes([LIST_INT]) + le32(mode) + bytes([LIST_END]))
    ok = c.wait_for(lambda: c.find_item_by_res(expect_res) is not None, 8)
    return ok, "make %s -> %s" % (recipe_id, expect_res)


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "newcraft"
    c = enter_world(username)
    print("in world; driving the session-58 craft batch")

    # 1) Saw from the starter kit (Branch x2 + Stone x1).
    ok, why = craft_once(c, "saw", "gfx/invobjs/saw")
    assert ok, "saw craft failed: %s" % why
    print("SAW CRAFT: OK")

    # 2) Bucket with the crafted saw (tool gate accepts a crafted tool).
    c.menu_act("craft", "bucket")
    assert c.wait_for(lambda: any(n == "make" for n in c.widgets.values()), 8), \
        "no make widget for bucket"
    make_wid = max(w for w, n in c.widgets.items() if n == "make")
    c.pump(0.3)
    c.wdgmsg(make_wid, "make", bytes([LIST_INT]) + le32(0) + bytes([LIST_END]))
    assert c.wait_for(lambda: c.find_item_by_res("gfx/invobjs/buckete") is not None, 8), \
        "bucket not produced with the crafted saw"
    print("BUCKET WITH CRAFTED SAW: OK")

    # 3) Fork paginae served over the resource HTTP with res signature.
    for page in ("paginae/craft/string", "paginae/craft/tanhide"):
        with urllib.request.urlopen(
                "%s/%s.res" % (RES_HTTP, page), timeout=10) as r:
            head = r.read(16)
        assert head == b"Haven Resource 1", "bad signature for %s: %r" % (page, head)
    print("FORK PAGINAE SERVED: OK")

    c.sock.close()
    print("NEWCRAFT: OK (saw, bucket, fork paginae)")


if __name__ == "__main__":
    main()
