#!/usr/bin/env python3
"""Synthesize neg layers for every server-referenced invobj resource that
ships without one (the client's ImageSprite requires a neg; missing negs
threw "No negative found" on the render thread and blanked item drops).
Writes in place into gameres/ and mirrors into the res/compiled overlay."""
import struct, sys, os

RESOURCES = [
    "gfx/invobjs/branch",
    "gfx/invobjs/stone",
    "gfx/invobjs/axe",
    "gfx/invobjs/carrot",
    "gfx/invobjs/flaxfibre",
    "gfx/invobjs/flaxseed",
    "gfx/invobjs/flower-poppy",
    "gfx/invobjs/onion",
    "gfx/invobjs/pumpkinflesh",
    "gfx/invobjs/seed-carrot",
    "gfx/invobjs/seed-hemp",
    "gfx/invobjs/seed-poppy",
    "gfx/invobjs/seed-pumpkin",
    "gfx/invobjs/seed-wheat",
    "gfx/invobjs/straw",
    "gfx/invobjs/tea-fresh",
]


def parse_res(path):
    data = open(path, "rb").read()
    sig = b"Haven Resource 1"
    assert data.startswith(sig), path
    ver = struct.unpack("<H", data[len(sig):len(sig) + 2])[0]
    off = len(sig) + 2
    layers = []
    while off < len(data):
        nend = data.index(b"\x00", off)
        name = data[off:nend].decode("latin1")
        off = nend + 1
        (ln,) = struct.unpack("<i", data[off:off + 4])
        off += 4
        layers.append((name, data[off:off + ln]))
        off += ln
    return ver, layers


def build_res(ver, layers):
    out = b"Haven Resource 1" + struct.pack("<H", ver)
    for name, blob in layers:
        out += name.encode("latin1") + b"\x00" + struct.pack("<i", len(blob)) + blob
    return out


def image_size(layers):
    for name, blob in layers:
        if name == "image":
            png = blob[11:]
            if len(png) >= 24 and png[:8] == b"\x89PNG\r\n\x1a\n":
                w, h = struct.unpack(">II", png[16:24])
                return (w, h)
    return (30, 30)


def neg_blob(sz):
    cc = (sz[0] // 2, sz[1] // 2)
    return struct.pack("<hh", *cc) + struct.pack("<hh", 0, 0) + \
        struct.pack("<hh", 11, 11) + struct.pack("<hh", *sz) + b"\x00"


def add_neg(path):
    ver, layers = parse_res(path)
    if any(n == "neg" for n, _ in layers):
        return False
    layers.append(("neg", neg_blob(image_size(layers))))
    open(path, "wb").write(build_res(ver, layers))
    return True


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else "."
    overlay = os.path.join(root, "res", "compiled")
    fixed = 0
    for name in RESOURCES:
        path = os.path.join(root, "gameres", name + ".res")
        if add_neg(path):
            fixed += 1
            print(f"neg added: {name}")
            # Mirror into the compiled overlay for future pack rebuilds.
            dst = os.path.join(overlay, name + ".res")
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with open(path, "rb") as src, open(dst, "wb") as out:
                out.write(src.read())
        else:
            print(f"neg present: {name}")
    print(f"DONE: {fixed} resources patched")


if __name__ == "__main__":
    main()
