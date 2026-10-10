#!/usr/bin/env python3
"""Verify a file -> directory module split is a pure move (two levels).

Usage:
    python3 verify_split.py <old_path> <new_dir> [<git_ref>]
    (defaults: server/crates/hnh-server/src/state.rs
               server/crates/hnh-server/src/state  HEAD)

Level 1 (items): the set of top-level item definitions (fns, structs,
enums, impls, consts, statics, type aliases, traits) must match exactly
between the old file and the new module tree - nothing lost, nothing
gained (modulo parser noise on multi-line headers).

Level 2 (lines): the multiset of code lines (plumbing stripped: use /
mod / pub use / comments / blanks) must overlap >= 98% verbatim; the
residual should be visibility plumbing (pub(super) -> pub(crate)) and
cfg(test) markers.

Session 94 used this to verify the state.rs -> state/ split (99.4%
verbatim, item sets equal, test name sets identical via cargo test
-- --list). Reusable for future splits (craft.rs / stream.rs are the
next oversize candidates).
"""
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

ITEM_RE = re.compile(
    r"^(pub(?:\(crate\))?\s+)?(?:unsafe\s+)?(?:async\s+)?"
    r"(fn|struct|enum|impl|const|static|type|trait|mod|union)\b"
)


def top_level_items(text: str):
    """Yield (kind, name) for top-level items with naive brace tracking."""
    items = []
    depth = 0
    for i, line in enumerate(text.split("\n")):
        stripped = line.strip()
        if depth == 0:
            m = ITEM_RE.match(stripped)
            if m:
                kind = m.group(2)
                rest = stripped[m.end():].strip()
                name_m = re.match(r"[A-Za-z_][A-Za-z0-9_]*", rest)
                if kind == "impl":
                    impl_m = re.match(r".*?(?:for\s+)?([A-Za-z_][A-Za-z0-9_<>:]*)", rest)
                    name = impl_m.group(1) if impl_m else rest[:40]
                else:
                    name = name_m.group(0) if name_m else rest[:40]
                items.append((kind, name, i + 1))
        for ch in line:
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth = max(0, depth - 1)
    return items


PLUMB_PREFIX = (
    "use ", "pub use ", "mod ", "pub mod ", "pub(crate) use ",
    "//", "///", "//!",
)


def code_lines(text: str):
    out = []
    in_block = False
    for raw in text.split("\n"):
        s = raw.strip()
        if in_block:
            if "*/" in s:
                in_block = False
            continue
        if s.startswith("/*"):
            if "*/" not in s:
                in_block = True
            continue
        if not s or s.startswith(PLUMB_PREFIX):
            continue
        out.append(s)
    return Counter(out)


def main():
    old_path = sys.argv[1] if len(sys.argv) > 1 else "server/crates/hnh-server/src/state.rs"
    new_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else REPO / "server/crates/hnh-server/src/state"
    git_ref = sys.argv[3] if len(sys.argv) > 3 else "HEAD"

    if not new_dir.is_absolute():
        new_dir = REPO / new_dir

    old_text = subprocess.run(
        ["git", "show", f"{git_ref}:{old_path}"], cwd=REPO, capture_output=True, text=True
    ).stdout
    if not old_text:
        print(f"ERROR: {git_ref}:{old_path} not found or empty")
        sys.exit(2)

    new_texts = {f.name: f.read_text() for f in sorted(new_dir.glob("*.rs"))}
    if not new_texts:
        print(f"ERROR: no .rs files under {new_dir}")
        sys.exit(2)

    # Level 1: item sets
    old_items = {(k, n) for k, n, _ in top_level_items(old_text)
                 if k not in ("mod",)}
    new_items = {(k, n) for t in new_texts.values() for k, n, _ in top_level_items(t)
                 if k not in ("mod",)}
    lost = old_items - new_items
    gained = new_items - old_items
    print(f"[items] old={len(old_items)} new={len(new_items)} lost={len(lost)} gained={len(gained)}")
    for k, n in sorted(lost):
        print(f"  LOST  {k} {n}")
    for k, n in sorted(gained):
        print(f"  GAINED {k} {n}")

    # Level 2: code line multisets
    old_c = code_lines(old_text)
    new_c = Counter()
    for t in new_texts.values():
        new_c += code_lines(t)
    total_old = sum(old_c.values())
    total_new = sum(new_c.values())
    common = sum((old_c & new_c).values())
    only_old = old_c - new_c
    only_new = new_c - old_c
    pct = common / max(total_old, 1) * 100
    print(f"[lines] old={total_old} new={total_new} common={common} ({pct:.1f}% verbatim)")
    print(f"[lines] only-old={sum(only_old.values())} only-new={sum(only_new.values())}")

    ok_items = not lost and not gained
    ok_lines = pct >= 98.0
    print(f"\nVERDICT: items {'OK' if ok_items else 'FAIL'} | lines {'OK' if ok_lines else 'FAIL'}")
    if ok_items and ok_lines:
        print("PURE MOVE VERIFIED")
        sys.exit(0)
    sys.exit(1)


if __name__ == "__main__":
    main()
