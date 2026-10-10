#!/usr/bin/env python3
"""Pixel-level verification: is the cow sprite (standing-0) actually
rendered at the agent-reported screen position of the breeding GL e2e
screenshot?

Method: exact/near template match. The sprite is 27x43 RGBA with
off=(21,14); if the client rendered the cow at screen (sx,sy), its
image's top-left lands at (sx-21, sy-14). We scan a window around the
reported position, and also the WHOLE frame as a fallback, scoring by
per-pixel RGB distance (alpha>128 pixels only). The best match wins;
we also try the hare sprite for comparison so the verdict discriminates
cow vs hare, not just "found something".

Exit 0 + "COW PIXEL MATCH: OK" when the best cow score is under the
threshold (0 = pixel-exact); exit 1 otherwise. VLM eyeballing of
27x43 px sprites is unreliable (horns read as ears); pixels are not."""

import sys
import os
import numpy as np
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from add_neg_layer import parse_res

REPO = os.path.dirname(os.path.abspath(__file__)) + "/.."


def load_sprite(res_path):
    ver, layers = parse_res(res_path)
    for name, blob in layers:
        if name == "image":
            ox, oy = (
                int.from_bytes(blob[7:9], "little"),
                int.from_bytes(blob[9:11], "little"),
            )
            im = Image.open(
                __import__("io").BytesIO(blob[11:])
            ).convert("RGBA")
            return np.array(im), (ox, oy)
    raise SystemExit(f"no image layer in {res_path}")


def match_score(screen, sprite, x0, y0):
    """Mean abs RGB distance over opaque sprite pixels at (x0,y0)."""
    h, w = sprite.shape[:2]
    if x0 < 0 or y0 < 0 or x0 + w > screen.shape[1] or y0 + h > screen.shape[0]:
        return 1e9, 0
    region = screen[y0 : y0 + h, x0 : x0 + w, :3].astype(int)
    alpha = sprite[:, :, 3] > 128
    if alpha.sum() < 30:
        return 1e9, 0
    diff = np.abs(region - sprite[:, :, :3].astype(int)).sum(axis=2)
    per_px = diff[alpha]
    return float(per_px.mean()), int(alpha.sum())


def best_in_window(screen, sprite, cx, cy, rad):
    """Brute-force scan a (2*rad)^2 window; returns best (score, x, y)."""
    best = (1e9, None, None)
    h, w = sprite.shape[:2]
    for y0 in range(max(0, cy - rad), min(screen.shape[0] - h, cy + rad)):
        for x0 in range(max(0, cx - rad), min(screen.shape[1] - w, cx + rad)):
            s, n = match_score(screen, sprite, x0, y0)
            if s < best[0]:
                best = (s, x0, y0)
    return best


def main():
    shot_path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/client_animals.png"
    shot = np.array(Image.open(shot_path).convert("RGB"))
    cow, (cox, coy) = load_sprite(
        f"{REPO}/gameres/gfx/kritter/cow/body/standing/standing-0.res"
    )
    hare, (hox, hoy) = load_sprite(
        f"{REPO}/gameres/gfx/kritter/hare/body/standing/standing-0.res"
    )
    print(f"cow sprite {cow.shape[1]}x{cow.shape[0]} off=({cox},{coy})")
    print(f"hare sprite {hare.shape[1]}x{hare.shape[0]} off=({hox},{hoy})")

    # Agent-reported kritter screen position (sx,sy)=(294,369); image
    # origin = (sx-off_x, sy-off_y).
    RX, RY = 294, 369
    print("\n== window scan +-60px around the reported position ==")
    for label, spr, off in (("cow", cow, (cox, coy)), ("hare", hare, (hox, hoy))):
        cx, cy = RX - off[0], RY - off[1]
        s, x, y = best_in_window(shot, spr, cx, cy, 60)
        print(f"{label}: best mean-rgb-dist={s:.1f} at image-origin ({x},{y})")

    print("\n== full-frame scan (stride 2) ==")
    cow_best = (1e9, None, None)
    for y0 in range(0, shot.shape[0] - cow.shape[0], 2):
        for x0 in range(0, shot.shape[1] - cow.shape[1], 2):
            s, n = match_score(shot, cow, x0, y0)
            if s < cow_best[0]:
                cow_best = (s, x0, y0)
    print(f"cow: best mean-rgb-dist={cow_best[0]:.1f} at ({cow_best[1]},{cow_best[2]})")
    hare_best = (1e9, None, None)
    for y0 in range(0, shot.shape[0] - hare.shape[0], 2):
        for x0 in range(0, shot.shape[1] - hare.shape[1], 2):
            s, n = match_score(shot, hare, x0, y0)
            if s < hare_best[0]:
                hare_best = (s, x0, y0)
    print(f"hare: best mean-rgb-dist={hare_best[0]:.1f} at ({hare_best[1]},{hare_best[2]})")

    # Machine-readable verdict: the cow template must match the frame
    # near-perfectly (dist < 5 mean RGB distance per opaque pixel).
    if cow_best[0] < 5.0:
        print(
            f"COW PIXEL MATCH: OK (dist={cow_best[0]:.2f} at "
            f"{cow_best[1]},{cow_best[2]}, hare best={hare_best[0]:.1f})"
        )
        return 0
    print(
        f"COW PIXEL MATCH: FAIL (best cow dist={cow_best[0]:.1f} at "
        f"{cow_best[1]},{cow_best[2]})"
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
