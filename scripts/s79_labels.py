#!/usr/bin/env python3
"""Read the tooltip (display name) layer of Haven .res files.

Usage: python3 scripts/s79_labels.py <res-file> [more.res ...]
Prints: <basename>\t<label>
"""
import struct
import sys
import os


def tooltip(path):
    with open(path, "rb") as f:
        data = f.read()
    if len(data) < 18:
        return None
    ver = struct.unpack_from("<H", data, 16)[0]
    off = 18
    # Standard resource layers: <name\0><int32 LE len><data>; the "tooltip"
    # layer payload is a length-prefixed UTF-8 string for most item resources.
    while off < len(data) - 5:
        nul = data.find(b"\0", off)
        if nul < 0 or nul > off + 128:
            return None
        name = data[off:nul].decode("latin1", "replace")
        head = nul + 1
        if head + 4 > len(data):
            return None
        (ln,) = struct.unpack_from("<i", data, head)
        if ln < 0 or head + 4 + ln > len(data):
            return None
        payload = data[head + 4 : head + 4 + ln]
        off = head + 4 + ln
        if name == "tooltip":
            try:
                return payload.rstrip(b"\0").decode("utf-8", "replace")
            except Exception:
                return None
    return None


for p in sys.argv[1:]:
    label = tooltip(p)
    print(f"{os.path.basename(p)}\t{label if label else '(none)'}")
