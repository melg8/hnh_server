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
    """First image layer's dimensions (the item icon). The Image layer
    header carries only z/subz/flags/id/offset; the pixel size lives in
    the embedded PNG's IHDR chunk (width/height at fixed offsets)."""
    for name, blob in layers:
        if name == "image":
            png = blob[11:]
            if len(png) >= 24 and png[:8] == b"\x89PNG\r\n\x1a\n":
                w, h = struct.unpack(">II", png[16:24])
                return (w, h)
    return None

def main():
    src_path, tgt_path, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
    tver, tlayers = parse_res(tgt_path)
    if src_path != "-":
        sver, slayers = parse_res(src_path)
        neg = next((b for n, b in slayers if n == "neg"), None)
        if neg is None:
            print(f"note: {src_path} has no neg layer; synthesizing")
    if any(n == "neg" for n, _ in tlayers):
        print(f"{tgt_path} already has a neg layer")
        return 0
    sz = image_size(tlayers) or (30, 30)
    # Neg layout (Resource.Neg / cdec = 2x int16 per coord):
    #   cc cdec(buf,0)   - image anchor (4 bytes)
    #   bc cdec(buf,4)   - hitbox base (4 bytes)
    #   bs cdec(buf,8)   - hitbox size (4 bytes)
    #   sz cdec(buf,12)  - sprite size (4 bytes)
    #   u8 endpoint count, then endpoints - zero for a flat icon.
    # Total 17 bytes.
    cc = (sz[0] // 2, sz[1] // 2)
    bc = (0, 0)
    bs = (11, 11)
    szc = (sz[0], sz[1])
    blob = struct.pack("<hh", *cc) + struct.pack("<hh", *bc) + \
        struct.pack("<hh", *bs) + struct.pack("<hh", *szc) + b"\x00"
    assert len(blob) == 17, len(blob)
    tlayers.append(("neg", blob))
    open(out_path, "wb").write(build_res(tver, tlayers))
    print(f"wrote {out_path} (cc={cc}, src ver {tver})")
    return 0

if __name__ == "__main__":
    sys.exit(main())
