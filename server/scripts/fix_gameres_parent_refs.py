#!/usr/bin/env python3
"""Align every action-layer parent_ver with the parent file's real version.

A companion to fix_gameres_versions.py. That script repairs the
"dropped parent_ver" corruption (the two version bytes were never
written, so name characters leak into the field). This script repairs a
second, silent class of pack defects: the parent_ver IS present but
STALE - it disagrees with the parent resource's actual on-disk version.
A strict client then requests the parent under the stale version, the
server serves the real file, and the version check kills the download
("Wrong res version (3 != 1)") which ends in a MenuGrid PaginaException
on world entry.

For every top-level "action" layer:
    parent_name (NUL-terminated) parent_ver (uint16 LE) button_name ...
if the parent .res exists in the pack and its real version differs from
the encoded parent_ver, the uint16 is rewritten IN PLACE (same length,
no layer re-framing needed). References to parents that do not exist in
the pack are left untouched.

Usage: fix_gameres_parent_refs.py [--check] [--using DIR] [ROOT...]
  --check       report only, write nothing (default when flag present)
  --using DIR   extra root used ONLY to resolve parent versions (repeatable).
                Partial overlays like res/compiled reference parents that
                live in the full pack (gameres); pass --using gameres.
  ROOT          pack root(s) to scan, default "gameres"

Exit status: 0 when no stale references remain, 1 otherwise.
"""

import struct
import sys
from pathlib import Path

SIG = b"Haven Resource 1"


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


def build_version_map(root: Path) -> dict:
    """name -> real on-disk version for every .res under root."""
    versions = {}
    for path in root.rglob("*.res"):
        rel = path.relative_to(root).as_posix()
        if rel.endswith(".res"):
            rel = rel[: -len(".res")]
        versions[rel] = res_version(path.read_bytes())
    return versions


def fix_root(root: Path, check: bool, global_versions: dict) -> int:
    versions = dict(global_versions)
    versions.update(build_version_map(root))  # local names win
    stale = 0
    for path in sorted(root.rglob("*.res")):
        data = path.read_bytes()
        patches = []  # (absolute offset, old bytes, new bytes)
        for name, body_off, body_len in scan_layers(data):
            if name != "action":
                continue
            body = data[body_off : body_off + body_len]
            end = body.find(b"\x00")
            if end < 0 or end + 3 > len(body):
                continue
            parent = body[:end].decode("utf-8", errors="replace")
            if parent not in versions:
                continue  # parent not shipped in this pack: leave as-is
            real = versions[parent]
            (encoded,) = struct.unpack("<H", body[end + 1 : end + 3])
            if encoded == real:
                continue
            patches.append((body_off + end + 1, encoded, real))
            stale += 1
            print(
                f"stale: {path.relative_to(root)} parent={parent!r} "
                f"encoded={encoded} real={real}"
            )
        if not patches or check:
            continue
        out = bytearray(data)
        for off, _old, new in patches:
            out[off : off + 2] = struct.pack("<H", new)
        path.write_bytes(bytes(out))
    return stale


def main() -> int:
    argv = sys.argv[1:]
    check = "--check" in argv
    using = []
    args = []
    it = iter(argv)
    for a in it:
        if a == "--check":
            continue
        if a == "--using":
            using.append(Path(next(it)))
            continue
        args.append(a)
    roots = [Path(p) for p in args] or [Path("gameres")]
    # Parent versions resolve against every declared root too, plus any
    # --using packs (an overlay root rarely ships its own parents).
    global_versions = {}
    for root in roots:
        global_versions.update(build_version_map(root))
    for extra in using:
        global_versions.update(build_version_map(extra))
    total = sum(
        fix_root(root, check, global_versions) for root in roots
    )
    if not check and total:
        # Re-scan after writing: the exit status must reflect what
        # REMAINS, not what was touched (idempotence witness).
        total = sum(fix_root(root, True, global_versions) for root in roots)
    scanned = len(list(roots[0].rglob("*.res")))
    print(f"gameres-parent-refs: scanned={scanned} stale={total}")
    return 1 if total else 0


if __name__ == "__main__":
    sys.exit(main())
