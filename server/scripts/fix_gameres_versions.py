#!/usr/bin/env python3
"""Repair broken AButton parent-version fields in a legacy HnH resource pack.

Problem being solved
--------------------
Every ``action`` (AButton) resource layer carries, as its first fields, the
parent pagina reference encoded as:

    parent_name (NUL-terminated UTF-8)
    parent_ver  (uint16 LE)
    button_name (NUL-terminated UTF-8)
    skill       (NUL-terminated UTF-8)
    hotkey      (uint16 LE)
    ad_count    (uint16 LE)
    ad strings  (ad_count NUL-terminated UTF-8 strings)

Some files shipped inside the original ``lib/haven-res.jar`` pack are
corrupted: the two-byte ``parent_ver`` field was dropped at pack-build
time, so the first two characters of the button name leak into the
version field (for example ``paginae/atk/dodge.res`` encodes
``parent=paginae/atk/blk`` followed immediately by ``Dodge``; the client
reads ``'D', 'o'`` as version 28484).

Legacy clients tolerated the resulting "Wrong res version" delayed
load error as long as nobody requested the parent resource under a
different version. Once the server announces the parent pagina (for
example ``paginae/atk/blk``) with its real file version, the client's
by-name resource cache sees two competing versions for one name, drops
the good entry, and the failed replacement leaves a broken resource
that makes ``MenuGrid.getSubResources`` throw PaginaException - killing
the UI receive thread (black screen on world entry).

Fix strategy
------------
Scan every ``.res`` file, parse the layer framing, and for every
``action`` layer detect the dropped-version corruption: the byte
immediately after the version field's two bytes must be the start of a
NUL-terminated button name, and in a corrupted file the "version" bytes
are the first two characters of that same name (both printable ASCII).
Corrupted entries are repaired by splicing in the real file version of
the parent resource (1 when the parent is unknown), extending the layer
length accordingly.

The script is idempotent: a second run over the same pack finds nothing
to fix. Exit status is 0 when no corruption remains, 1 otherwise.
"""

import struct
import sys
from pathlib import Path

SIG = b"Haven Resource 1"

# Printable ASCII range used by resource names. A "version" whose high
# byte falls into this range is almost certainly text, not a number:
# real pack versions in this project never exceed a few thousand, and
# every known corrupt sample reads as two name characters.
ASCII_LO, ASCII_HI = 0x20, 0x7E


def res_version(data: bytes) -> int:
    """File version from the 16-byte signature + uint16 header."""
    if len(data) < 18 or data[:16] != SIG:
        return 1
    return struct.unpack("<H", data[16:18])[0]


def scan_layers(data: bytes):
    """Yield (name, body_offset, body_len) for each top-level layer."""
    off = 18
    n = len(data)
    while off + 1 <= n:
        end = data.find(b"\x00", off)
        if end < 0:
            return
        name = data[off:end].decode("utf-8", errors="replace")
        if end + 5 > n:
            return
        (body_len,) = struct.unpack("<i", data[end + 1 : end + 5])
        body_off = end + 5
        if body_len < 0 or body_off + body_len > n:
            return
        yield name, body_off, body_len
        off = body_off + body_len


def action_corruption(body: bytes):
    """Return True if this action layer carries the dropped-parent-version
    corruption."""
    end = body.find(b"\x00")
    if end < 0:
        return False
    # parent name ends at `end`; the version is the next two bytes.
    if end + 2 >= len(body):
        return False
    ver_hi = body[end + 2]
    return ASCII_LO <= ver_hi <= ASCII_HI


def repair_action(body: bytes, parent_ver: int) -> bytes:
    """Insert the missing uint16 parent version after the parent name."""
    end = body.find(b"\x00")
    ver = struct.pack("<H", parent_ver)
    return body[: end + 1] + ver + body[end + 1 :]


def rebuild(data: bytes, fixes):
    """Rebuild a resource file applying (body_offset, new_body) fixes."""
    fixes = sorted(fixes, key=lambda f: f[0])
    out = bytearray(data)
    delta = 0
    for body_off, new_body in fixes:
        start = body_off + delta
        # Old length field sits at start-4; read it to locate the body end.
        (old_len,) = struct.unpack("<i", out[start - 4 : start])
        end = start + old_len
        new_len = len(new_body)
        out[start - 4 : start] = struct.pack("<i", new_len)
        out[start:end] = new_body
        delta += new_len - old_len
    return bytes(out)


def parent_ver_for(gameres: Path, parent: str) -> int:
    path = gameres / (parent + ".res")
    if not path.is_file():
        return 1
    return res_version(path.read_bytes())


def main() -> int:
    roots = [Path(p) for p in sys.argv[1:]] or [Path("gameres")]
    total_fixed = 0
    total_broken = 0
    for root in roots:
        for path in sorted(root.rglob("*.res")):
            data = path.read_bytes()
            fixes = []
            report = []
            for name, body_off, body_len in scan_layers(data):
                if name != "action":
                    continue
                body = data[body_off : body_off + body_len]
                if not action_corruption(body):
                    continue
                total_broken += 1
                # Extract the parent name for the report (and the version).
                end = body.find(b"\x00")
                parent = body[:end].decode("utf-8", errors="replace")
                ver = parent_ver_for(root, parent)
                fixes.append((body_off, repair_action(body, ver)))
                report.append((parent, ver))
            if not fixes:
                continue
            fixed = rebuild(data, fixes)
            path.write_bytes(fixed)
            total_fixed += len(fixes)
            for parent, ver in report:
                print(f"fixed: {path} parent={parent!r} -> ver={ver}")
    print(
        f"gameres-versions: scanned={len(list(roots[0].rglob('*.res')))} "
        f"corrupt={total_broken} fixed={total_fixed}"
    )
    return 0 if total_broken == total_fixed else 1


if __name__ == "__main__":
    sys.exit(main())
