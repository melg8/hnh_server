#!/usr/bin/env python3
"""Inventory the borka equipment WALKING layer paths and diff them
against the standing shapes: catches pieces whose file prefix changes
between the two poses (e.g. shirt-barmor standing-N vs walking-N)."""
import os
import re
import sys
from collections import defaultdict

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "server", "gameres", "gfx", "borka")

PIECES = [
    "shirt-linen", "shirt-nettle", "shirt-ranger", "shirt-chainmail",
    "shirt-larmor", "shirt-barmor", "shirt-parmor",
    "pants-linen", "pants-nettle", "pants-ranger", "pants-larmor",
    "shoes-lboots", "shoes-ranger", "shoes-toffels", "shoes-clogs",
    "hat-straw", "hat-fox", "hat-top", "hat-chief", "hat-gandalf",
    "hat-high", "hat-sprucecap", "hat-pumpkin", "hat-working",
    "hat-bandit", "hat-chef", "hat-gauze",
    "helm-tusk", "helm-soldiers", "helm-hird", "helm-druid",
    "helm-miners-plain", "helm-miners-lit", "helm-miners-candle",
    "cape", "cape-bear", "cape-black", "cape-gandalf", "cape-ranger",
    "cloak-druid", "cloak-gandalf", "cloak-hide", "cloak-leather",
    "cloak-merchant", "cloak-necro", "cloak-toga",
    "belt-poor", "glove-poor", "backpack", "quiver",
    "eq-sword", "eq-wsword", "eq-bronzesword", "eq-knife", "eq-paxe",
    "eq-saxe", "eq-shield", "eq-torch", "eq-torch-lit", "eq-bow",
    "bow-ranger", "eq-staff", "eq-spear", "eq-scythe", "eq-shammer",
]

def norm(name: str) -> str:
    return re.sub(r"(\d+)\.res$", "{d}", name)

def shape(piece: str, pose: str) -> str:
    base = os.path.join(ROOT, piece, pose)
    if not os.path.isdir(base):
        return "MISSING"
    rels = []
    for dirpath, _dirs, files in os.walk(base):
        for f in sorted(files):
            if f.endswith(".res"):
                rels.append(norm(os.path.relpath(os.path.join(dirpath, f), os.path.join(ROOT, piece))))
    return "; ".join(sorted(set(rels)))

def main() -> None:
    for piece in PIECES:
        s = shape(piece, "standing")
        w = shape(piece, "walking")
        # Strip the pose segment before comparing: standing/torso-N must
        # correspond to walking/torso-N.
        sw = w.replace("/{pose}/", "/").replace("walking/", "{pose}/", 1)
        s2 = s.replace("standing/", "{pose}/", 1)
        s2 = s2.replace("standing/", "")  # inner standing-N files
        sw2 = sw.replace("walking/", "")
        if s2 != sw2 or w == "MISSING":
            print(f"DIFF {piece}:\n  standing: {s}\n  walking:  {w}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
