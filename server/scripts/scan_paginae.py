#!/usr/bin/env python3
"""Static paginae/craft scanner (no server needed).

Parses the action layer of every paginae/craft/*.res in the legacy pack
(may be given a directory; defaults to /tmp/hx/res/paginae/craft) and
prints one CSV-ish line per pagina:

    <file-stem> | <parent> | <display name> | <prereq> | <hotkey> | <ad...>

Action layer layout (matches src/haven/Resource.java AButton decoding,
verified against the documented rustroot.res decode in
docs/mechanics/crafting/crafting-and-building.md):

    "action\0" <uint16 flags?> <parent-name\0> <uint16 parent-ver>
    <name\0> <prereq\0> <hotkey byte> <uint16 ad-count> <ad\0 ...>
"""
import sys
from pathlib import Path

DEFAULT_DIR = Path("/tmp/hx/res/paginae/craft")


def read_cstr(buf: bytes, off: int) -> tuple[str, int]:
    end = buf.index(b"\x00", off)
    return buf[off:end].decode("utf-8", "replace"), end + 1


def parse_action(buf: bytes) -> dict | None:
    """Decode the 'action' (AButton) layer.

    File framing (Resource.load, src/haven/Resource.java): signature
    "Haven Resource 1", uint16 version, then repeated
    <layer-name\\0><int32 LE length><layer data>. AButton data layout
    (AButton(byte[]), src/haven/Resource.java:1023): parent cstr,
    uint16 parent ver, name cstr, prereq cstr, uint16 hotkey,
    uint16 ad count, ad cstrs.
    """
    i = buf.find(b"Haven Resource 1")
    if i != 0:
        return None
    off = 16 + 2
    while off < len(buf):
        end = buf.index(b"\x00", off)
        lname = buf[off:end].decode("utf-8", "replace")
        off = end + 1
        llen = int.from_bytes(buf[off:off + 4], "little")
        off += 4
        data = buf[off:off + llen]
        off += llen
        if lname != "action":
            continue
        parent, p = read_cstr(data, 0)
        p += 2  # uint16 parent version
        name, p = read_cstr(data, p)
        prereq, p = read_cstr(data, p)
        hk = int.from_bytes(data[p:p + 2], "little")
        p += 2
        n_ad = int.from_bytes(data[p:p + 2], "little")
        p += 2
        ad = []
        for _ in range(n_ad):
            s, p = read_cstr(data, p)
            ad.append(s)
        return {"parent": parent, "name": name, "prereq": prereq,
                "hotkey": chr(hk) if hk else "", "ad": ad}
    return None


def main() -> None:
    d = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_DIR
    rows = []
    for f in sorted(d.glob("*.res")):
        try:
            a = parse_action(f.read_bytes())
        except (ValueError, IndexError):
            a = None
        if a is None:
            rows.append((f.stem, "?", "?", "?", "?", "?"))
            continue
        rows.append((f.stem, a["parent"], a["name"], a["prereq"],
                     a["hotkey"], "|".join(a["ad"])))
    for r in rows:
        print(" | ".join(r))
    print(f"--- {len(rows)} paginae ---", file=sys.stderr)


if __name__ == "__main__":
    main()
