#!/usr/bin/env python3
"""Compose full standing characters for all 8 art directions from the
borka part resources, so the ART's own facing convention can be verified
visually (which index depicts which screen octant).

Usage: dump_borka_dirs.py <out.png>
"""
import io
import struct
import sys

from PIL import Image, ImageDraw

PARTS = [
    "server/gameres/gfx/borka/body/standing/legs-{d}.res",
    "server/gameres/gfx/borka/body/standing/torso/male-{d}.res",
    "server/gameres/gfx/borka/body/standing/head-{d}.res",
    "server/gameres/gfx/borka/body/standing/arm/idle/left-{d}.res",
    "server/gameres/gfx/borka/body/standing/arm/idle/right-{d}.res",
    "server/gameres/gfx/borka/hair-karin/standing/hair-{d}.res",
]


def image_layers(path):
    """All (png_bytes, header) image layers of a .res."""
    data = open(path, "rb").read()
    sig = b"Haven Resource 1"
    assert data.startswith(sig), path
    off = len(sig) + 2
    out = []
    while off < len(data):
        nend = data.index(b"\x00", off)
        name = data[off:nend].decode("latin1")
        off = nend + 1
        (ln,) = struct.unpack("<i", data[off:off + 4])
        off += 4
        blob = data[off:off + ln]
        off += ln
        if name == "image":
            i = blob.find(b"\x89PNG\r\n\x1a\n")
            if i >= 0:
                out.append((blob[i:], blob[:i]))
    return out


def cdec(header, pos):
    b = header[pos]
    if b < 0x80:
        return b, pos + 1
    b2 = header[pos + 1]
    return (((b & 0x7F) << 8) | b2), pos + 2


def header_offsets(header):
    o = 7
    x, o = cdec(header, o)
    y, o = cdec(header, o)
    return x, y


def main():
    out = sys.argv[1]
    scale = 4  # nearest-neighbor upscale: 16x23 art needs magnification
    tiles = []
    for d in range(8):
        canvas = Image.new("RGBA", (200, 240), (0, 0, 0, 0))
        for tmpl in PARTS:
            path = tmpl.format(d=d)
            for png, header in image_layers(path):
                img = Image.open(io.BytesIO(png)).convert("RGBA")
                ox, oy = header_offsets(header)
                canvas.alpha_composite(img, (100 + ox, 120 + oy))
        tiles.append(canvas.resize((200 * scale, 240 * scale), Image.NEAREST))
    sheet = Image.new("RGBA", (200 * scale * 2, 262 * scale * 2), (46, 52, 60, 255))
    draw = ImageDraw.Draw(sheet)
    for d, tile in enumerate(tiles):
        col, row = d % 2, d // 2
        x0, y0 = col * 200 * scale, row * 262 * scale
        draw.text((x0 + 12, y0 + 4), f"art index {d}", fill=(255, 255, 120, 255))
        sheet.alpha_composite(tile, (x0, y0 + 20 * scale))
    sheet.save(out)
    print(f"saved {out}")


if __name__ == "__main__":
    main()
