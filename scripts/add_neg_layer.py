#!/usr/bin/env python3
"""Add a missing `neg` layer to a Haven .res resource file.

The client's ItemSprite requires a `neg` layer for every inventory item
resource; a resource without one throws "No negative found" inside
ResDrawable init, which (pre-fix) escaped the session reader thread and
killed the whole session (the frozen-character bug).

Usage: add_neg_layer.py <source-res-with-neg> <target-res> <out-res>
Copies the neg layer from the source, keeping the target's image center
cc (computed from the target's image layer) when provided by --center.
"""
import struct, sys

def parse_res(path):
    data = open(path, "rb").read()
    sig = b"Haven Resource 1"
    assert data.startswith(sig), path
    off = len(sig)
    ver = struct.unpack("<H", data[off:off+2])[0]
    off += 2
    layers = []
    while off < len(data):
        nend = data.index(b"\x00", off)
        name = data[off:nend].decode("latin1")
        off = nend + 1
        (ln,) = struct.unpack("<i", data[off:off+4])
        off += 4
        blob = data[off:off+ln]
        off += ln
        layers.append((name, blob))
    return ver, layers

def build_res(ver, layers):
    out = b"Haven Resource 1" + struct.pack("<H", ver)
    for name, blob in layers:
        out += name.encode("latin1") + b"\x00" + struct.pack("<i", len(blob)) + blob
    return out

def image_size(layers):
    """First image layer's dimensions (the item icon)."""
    for name, blob in layers:
        if name == "image":
            # Image layer: u16 z, i8 subz, i8 fl, u16 id, coord o, coord ssz
            z = struct.unpack("<h", blob[0:2])[0]
            off = 2 + 1 + 1      # skip subz + fl
            _id = struct.unpack("<h", blob[off:off+2])[0]; off += 2
            off += 8              # skip coord o
            w, h = struct.unpack("<ii", blob[off:off+8])
            return (w, h)
    return None

def main():
    src_path, tgt_path, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
    sver, slayers = parse_res(src_path)
    tver, tlayers = parse_res(tgt_path)
    neg = next((b for n, b in slayers if n == "neg"), None)
    if neg is None:
        print(f"{src_path} has no neg layer")
        return 1
    if any(n == "neg" for n, _ in tlayers):
        print(f"{tgt_path} already has a neg layer")
        return 0
    sz = image_size(tlayers) or (30, 30)
    # Neg layout: cc (2xi32), bc (2xi32), bs (2xi32), sz (2xi32), u8 epcount.
    # cc is the image center: the legacy item icons anchor at (w/2, h/2).
    # bc/bs describe the ground hitbox in screen-projection units; reuse the
    # source's hitbox shape but keep it small for a flat icon.
    cc = (sz[0] // 2, sz[1] // 2)
    bc = (0, 0)
    bs = (11, 11)
    szc = (sz[0], sz[1])
    blob = struct.pack("<ii", *cc) + struct.pack("<ii", *bc) + \
        struct.pack("<ii", *bs) + struct.pack("<ii", *szc) + b"\x00"
    tlayers.append(("neg", blob))
    open(out_path, "wb").write(build_res(tver, tlayers))
    print(f"wrote {out_path} (cc={cc}, src ver {tver})")
    return 0

if __name__ == "__main__":
    sys.exit(main())
