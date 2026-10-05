#!/usr/bin/env python3
"""Extract the first frame of every directional pose variant and montage
them into one labeled sheet, so the ART's own facing convention can be
verified visually (which index depicts which screen octant).

Usage: dump_directions.py <glob-template> <out.png>
  <glob-template> must contain {d} replaced by 0..7.

Example:
  dump_directions.py server/gameres/gfx/borka/body/standing/legs-{d}.res dirs.png
"""
import io
import struct
import sys
import glob

from PIL import Image, ImageDraw


def first_image_png(path):
    """Return (png_bytes, header) of the first image layer in a .res."""
    data = open(path, "rb").read()
    sig = b"Haven Resource 1"
    assert data.startswith(sig), path
    off = len(sig) + 2  # skip version
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
                # header before PNG: z(2) subz(2) fl(1) id(2) o(cdec..)
                return blob[i:], blob[:i]
    return None, None


def cdec_value(header, pos):
    """Client Utils.cdec: 1-byte when < 0x80, else 2-byte little-endian
    with the high bit dropped."""
    b = header[pos]
    if b < 0x80:
        return b, pos + 1
    b2 = header[pos + 1]
    return (((b & 0x7F) << 8) | b2), pos + 2


def header_offsets(header):
    """z(2) subz(2) fl(1) id(2) then cdec o.x, o.y."""
    o = 7
    x, o = cdec_value(header, o)
    y, o = cdec_value(header, o)
    return x, y


def main():
    tmpl, out = sys.argv[1], sys.argv[2]
    frames = []
    for d in range(8):
        path = tmpl.format(d=d)
        matches = glob.glob(path)
        if not matches:
            print(f"missing: {path}")
            frames.append(None)
            continue
        png, header = first_image_png(matches[0])
        img = Image.open(io.BytesIO(png)).convert("RGBA")
        ox, oy = header_offsets(header)
        frames.append((img, ox, oy, path))
    # Compose each frame at a shared origin so relative offsets stay true.
    pad = 90
    cw = max((f[0].width for f in frames if f), default=64) + pad * 2
    ch = max((f[0].height for f in frames if f), default=64) + pad * 2
    sheet = Image.new("RGBA", (cw * 4, (ch + 22) * 2), (34, 40, 34, 255))
    draw = ImageDraw.Draw(sheet)
    for d, f in enumerate(frames):
        col, row = d % 4, d // 4
        x0, y0 = col * cw, row * (ch + 22)
        draw.text((x0 + 6, y0 + 4), f"art index {d}", fill=(255, 255, 120, 255))
        tile = Image.new("RGBA", (cw, ch), (60, 60, 70, 255))
        if f is not None:
            img, ox, oy, path = f
            tile.paste(img, (pad + ox, pad + oy), img)
            draw.text((x0 + 6, y0 + ch - 16),
                      f"{path.split('/')[-1]} o=({ox},{oy})",
                      fill=(160, 220, 160, 255))
        sheet.paste(tile, (x0, y0 + 20))
    sheet.save(out)
    print(f"saved {out} ({cw}x{ch} per tile)")


if __name__ == "__main__":
    main()
