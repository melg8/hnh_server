#!/usr/bin/env python3
"""Generate the combat HUD icon resources the client Fightview requires.

src/haven/Fightview.java renders relation strings like
$img[gfx/hud/combat/bal]$img[gfx/hud/combat/ip]... The $img[] tag with no
explicit id resolves via RichText$Image with id -1, which looks for an
Image layer with id == -1 inside the resource. None of the four resources
(bal, ip, off, def) existed in any client source (base pack, fork
overlay, HTTP), and the missing layer crashed the GL render thread on
world entry ("Found no image with id -1 in gfx/hud/combat/bal(v-1)").

This script writes the missing resources into res/compiled/gfx/hud/
combat/ in the plain HRF container: b"Haven Resource 1", uint16
version, then repeated <layer-name\\0><int32 LE length><layer data> (see
server/scripts/scan_paginae.py). The image layer data follows
Resource$Image(byte[]) in src/haven/Resource.java:718:
int16 z, int16 subz, flags byte (bit 1 evokes the obsolete Layered
mode), int16 id, int16 o.x, int16 o.y, then an ImageIO-readable byte
stream (a flat PNG).

Run: python server/scripts/make_combat_icons.py
"""

import math
import struct
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
OUT_DIR = REPO / "res" / "compiled" / "gfx" / "hud" / "combat"
SIG = b"Haven Resource 1"

RGBA = 6  # color type: red/green/blue/alpha
DEPTH = 8  # bit depth per channel


def _chunk(tag: bytes, body: bytes) -> bytes:
    return (len(body).to_bytes(4, "big") + tag + body
            + zlib.crc32(tag + body).to_bytes(4, "big"))


def encode_png(width: int, height: int, rgba: bytes) -> bytes:
    ihdr = struct.pack(">IIBBBBB", width, height, DEPTH, RGBA, 0, 0, 0)
    # One filter byte (0 = None) per scanline before the raw pixels.
    raw = bytearray()
    slot = width * 4
    for y in range(height):
        raw.append(0)
        raw += rgba[y * slot:(y + 1) * slot]
    idat = zlib.compress(bytes(raw), 9)
    return (b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", ihdr)
            + _chunk(b"IDAT", idat) + _chunk(b"IEND", b""))


GLYPHS = {
    "B": ("11110",
          "10001",
          "10001",
          "11110",
          "10001",
          "10001",
          "11110"),
    "I": ("111",
          "010",
          "010",
          "010",
          "010",
          "010",
          "111"),
    "O": ("01110",
          "10001",
          "10001",
          "10001",
          "10001",
          "10001",
          "01110"),
    "D": ("11110",
          "10001",
          "10001",
          "10001",
          "10001",
          "10001",
          "11110"),
}

# 16x16 icon, glyph scaled 2x, framed by a 1px ring, id -1 for $img[].
# gfx/hud/combat/ip is NOT generated: the base pack (lib/haven-res.jar)
# already ships it with the id -1 image layer the $img[] tag resolves.
SIZE = 16
ICONS = {
    "bal": ("B", bytes((0xCC, 0x20, 0x20, 0xFF))),
    "off": ("O", bytes((0xCC, 0x80, 0x20, 0xFF))),
    "def": ("D", bytes((0x20, 0xA0, 0x60, 0xFF))),
}


def render_icon(glyph: str, colour: bytes) -> bytes:
    pat = GLYPHS[glyph]
    gw = len(pat[0]) * 2
    gh = len(pat) * 2
    gx = (SIZE - gw) // 2
    gy = (SIZE - gh) // 2
    px = bytearray(SIZE * SIZE * 4)
    alpha = colour[3]

    def put(x: int, y: int, r: int, g: int, b: int, a: int) -> None:
        off = (y * SIZE + x) * 4
        px[off:off + 4] = bytes((r, g, b, a))

    def ring(x: int, y: int, r: int, g: int, b: int, a: int) -> None:
        # Additive blend keeps the glyph readable over the frame.
        off = (y * SIZE + x) * 4
        px[off] = max(px[off], r)
        px[off + 1] = max(px[off + 1], g)
        px[off + 2] = max(px[off + 2], b)
        px[off + 3] = max(px[off + 3], a)

    for y in range(SIZE):
        for x in range(SIZE):
            dx = x - (SIZE - 1) / 2
            dy = y - (SIZE - 1) / 2
            d = math.hypot(dx, dy)
            if 6.2 <= d <= 7.2:
                ring(x, y, colour[0], colour[1], colour[2], alpha)
    for row, line in enumerate(pat):
        for col, bit in enumerate(line):
            if bit == "1":
                for sx in range(2):
                    for sy in range(2):
                        put(gx + col * 2 + sx, gy + row * 2 + sy,
                            colour[0], colour[1], colour[2], alpha)
    return encode_png(SIZE, SIZE, bytes(px))


def image_layer(png: bytes) -> bytes:
    # z, subz, flags, id, o.x, o.y - mirrors Resource$Image(byte[]) decode.
    layer = struct.pack("<hhBhhh", 0, 0, 0, -1, 0, 0)
    assert len(layer) == 11
    return layer + png


def write_res(name: str, body: bytes) -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    frame = bytearray()
    frame += SIG
    frame += struct.pack("<H", 1)
    frame += b"image\x00"
    frame += struct.pack("<I", len(body))
    frame += body
    path = OUT_DIR / f"{name}.res"
    path.write_bytes(bytes(frame))
    print(f"wrote {path} ({len(frame)} bytes)")


def main() -> None:
    for name, (glyph, colour) in ICONS.items():
        png = render_icon(glyph, colour)
        write_res(name, image_layer(png))


if __name__ == "__main__":
    main()
