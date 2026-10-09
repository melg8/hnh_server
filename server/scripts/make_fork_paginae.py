#!/usr/bin/env python3
"""Build the fork craft paginae the 2009 pack lacks (session 58).

The legacy pack ships no pagina whose action layer says
`ad = ["craft", "string"]` or `ad = ["craft", "tanhide"]`, but both
recipes are part of the pack's item economy (String and Leather are
real invobj resources; the leather tier pages already reference them
as skill prereqs). The server needs a clickable page per recipe id, so
this script composes fork pagina resources into `res/compiled/`:

  paginae/craft/string.res  - icon borrowed from gfx/invobjs/string,
                              parent paginae/craft/clothmat
  paginae/craft/tanhide.res - icon borrowed from gfx/invobjs/leather,
                              parent paginae/craft/leather

Session 81 adds the raisins page (the sun-dried hand recipe has no
shipped page either):

  paginae/craft/raisins.res - icon borrowed from gfx/invobjs/raisins,
                              parent paginae/craft/baking

File framing and the AButton layer layout follow src/haven/Resource.java
(signature, uint16 version, layers of <name\\0><int32 len><data>;
AButton = parent cstr, uint16 parent ver, name cstr, prereq cstr,
uint16 hotkey, uint16 ad count, ad cstrs) - the same layout the
session-58 static scanner (server/scripts/scan_paginae.py) decodes.
"""
import struct
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
JAR_SRC = REPO / "lib" / "haven-res.jar"
EXTRACT = Path("/tmp/hx58/res")
OUT = REPO / "res" / "compiled" / "paginae" / "craft"


def cstr(s: str) -> bytes:
    return s.encode("utf-8") + b"\x00"


def action_layer(parent: str, parent_ver: int, name: str, prereq: str,
                 ad: list[str]) -> bytes:
    """Return the ACTION LAYER DATA only (write_res adds the framing).

    `parent_ver` MUST be the parent's real on-disk version: a strict
    client loads the parent at exactly that version and dies with
    "Wrong res version" when the pack serves a different one (the
    session-80 root cause; fix_gameres_parent_refs.py --check guards
    the same contract).
    """
    data = cstr(parent)
    data += struct.pack("<H", parent_ver)
    data += cstr(name)
    data += cstr(prereq)
    data += struct.pack("<H", 0)  # hotkey: none (valid; see hirdhelm)
    data += struct.pack("<H", len(ad))
    for a in ad:
        data += cstr(a)
    return data


def res_version(path: Path) -> int:
    """File version (uint16 at offset 16, after the 16-byte signature)."""
    d = path.read_bytes()
    if d[:16] != b"Haven Resource 1" or len(d) < 18:
        return 1
    return struct.unpack("<H", d[16:18])[0]


def parent_version(parent: str) -> int:
    """Resolve the parent's real version: the compiled overlay wins
    over the jar extract (the overlay is the pack the server serves)."""
    overlay = (REPO / "res" / "compiled").joinpath(parent + ".res")
    if overlay.exists():
        return res_version(overlay)
    jar_copy = (EXTRACT / "res").joinpath(parent + ".res")
    if jar_copy.exists():
        return res_version(jar_copy)
    raise SystemExit(f"parent {parent} not found in overlay or jar extract")


def read_layers(path: Path) -> tuple[int, list[tuple[str, bytes]]]:
    d = path.read_bytes()
    assert d[:16] == b"Haven Resource 1", f"bad sig in {path}"
    ver = struct.unpack("<H", d[16:18])[0]
    off = 18
    out = []
    while off < len(d):
        e = d.index(b"\x00", off)
        nm = d[off:e].decode()
        off = e + 1
        ln = struct.unpack("<i", d[off:off + 4])[0]
        off += 4
        out.append((nm, d[off:off + ln]))
        off += ln
    return ver, out


def write_res(path: Path, ver: int, layers: list[tuple[str, bytes]]) -> None:
    body = b"Haven Resource 1" + struct.pack("<H", ver)
    for nm, data in layers:
        body += cstr(nm) + struct.pack("<i", len(data)) + data
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(body)


def main() -> None:
    # The jar entries are rooted at res/ (make-gameres.sh unpacks the
    # same way: `unzip -d $TMP/res` then `cp $TMP/res/res/...`), so the
    # donor paths below resolve under EXTRACT/"res".
    # Probe one file per unzip pattern so a partial extract (a prior
    # run without the paginae pattern) re-extracts.
    probe = EXTRACT / "res" / "gfx" / "invobjs" / "string.res"
    probe_pages = EXTRACT / "res" / "paginae" / "craft" / "baking.res"
    if not (probe.exists() and probe_pages.exists()):
        import subprocess
        EXTRACT.mkdir(parents=True, exist_ok=True)
        subprocess.run(["unzip", "-o", "-q", str(JAR_SRC),
                        "res/gfx/invobjs/*", "res/paginae/*",
                        "-d", str(EXTRACT)], check=True)
    jobs = [
        # (out file, icon donor res, parent, name, prereq, ad)
        ("string.res", "gfx/invobjs/string.res",
         "paginae/craft/clothmat", "String", "cloth", ["craft", "string"]),
        ("tanhide.res", "gfx/invobjs/leather.res",
         "paginae/craft/leather", "Leather", "leather", ["craft", "tanhide"]),
        # Session 81: the raisin leg's hand recipe page - parented into
        # the baking family (the dough pages it feeds), prereq "bake"
        # like the apdough/ccdough siblings.
        ("raisins.res", "gfx/invobjs/raisins.res",
         "paginae/craft/baking", "Raisins", "bake", ["craft", "raisins"]),
    ]
    for out_name, donor, parent, name, prereq, ad in jobs:
        ver, donor_layers = read_layers(EXTRACT / "res" / donor)
        icons = [(nm, d) for nm, d in donor_layers if nm == "image"]
        assert icons, f"no image layer in {donor}"
        # Parent version must match the pack's real file version (see
        # action_layer); res_version resolves overlay-over-jar.
        pver = parent_version(parent)
        # Version 1 for the fork page (the donor's icon carries no
        # version contract of its own; 1 keeps parent-linking simple).
        write_res(OUT / out_name, 1,
                  icons + [("action", action_layer(parent, pver, name,
                                                    prereq, ad))])
        print(f"wrote {OUT / out_name} ({len(icons)} image layer(s), "
              f"parent {parent} v{pver})")


if __name__ == "__main__":
    main()
