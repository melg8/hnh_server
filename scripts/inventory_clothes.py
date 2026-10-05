#!/usr/bin/env python3
"""Inventory the borka equipment layer paths.

Walks server/gameres/gfx/borka/<piece>/standing/ and prints, for each
equipment piece, the concrete relative layer file names (without .res)
so the server's equip-layer table can be built from real data instead
of guesses. Groups results by shape (the pattern with {dir} for the
art octant digit).
"""
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
    """Replace the trailing art octant digit with {dir}."""
    return re.sub(r"(\d+)\.res$", "{dir}", name)

def main() -> None:
    shapes = defaultdict(list)
    for piece in PIECES:
        standing = os.path.join(ROOT, piece, "standing")
        if not os.path.isdir(standing):
            shapes["MISSING"].append(piece)
            continue
        rels = []
        for dirpath, _dirs, files in os.walk(standing):
            for f in sorted(files):
                if f.endswith(".res"):
                    rel = os.path.relpath(os.path.join(dirpath, f), os.path.join(ROOT, piece))
                    rels.append(norm(rel))
        shape = "; ".join(sorted(set(rels)))
        shapes[shape].append(piece)
    for shape, ps in sorted(shapes.items()):
        print(f"[{len(ps)}] {','.join(sorted(ps))}")
        print(f"     {shape}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
