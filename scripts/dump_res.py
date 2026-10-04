#!/usr/bin/env python3
"""Dump the layer structure of a H&H .res file (images, anims, neg, code).

English-only comments per repo rules. Used to inspect animation resources
so the server can drive the client's native direction-selection mechanism.
"""
import struct
import sys
import zlib


def main(path):
    with open(path, 'rb') as f:
        data = f.read()
    off = 0

    def u8():
        nonlocal off
        v = data[off]
        off += 1
        return v

    def u16():
        nonlocal off
        v = struct.unpack_from('<H', data, off)[0]
        off += 2
        return v

    def u32():
        nonlocal off
        v = struct.unpack_from('<i', data, off)[0]
        off += 4
        return v

    def cstr():
        nonlocal off
        end = data.index(b'\x00', off)
        s = data[off:end].decode('utf-8', 'replace')
        off = end + 1
        return s

    ver = u16()
    print(f"res version {ver}")
    while off < len(data):
        start = off
        tag = cstr()
        ver2 = u16()
        # layer header: id (int16), then per-tag payload
        lid = u16() if tag not in ('code',) else None
        print(f"  layer '{tag}' v{ver2}" + (f" id={lid}" if lid is not None else ""),
              end=' ')
        if tag == 'image':
            z = u8()
            subtype = u8()
            o = (u16(), u16())
            sz = (u16(), u16())
            zcomp = u8()
            print(f"z={z} sub={subtype} o={o} sz={sz} comp={zcomp}", end='')
            if zcomp == 0:
                off += sz[0] * sz[1] * 4
            else:
                cl = u32()
                off += cl
            print(f" [data {off - (start)} bytes consumed]")
            continue
        if tag == 'anim':
            idir = u16()
            fd = u16()
            idur = u16()
            n = u16()
            frames = []
            for _ in range(n):
                ni = u16()
                frames.append(ni)
            print(f"idir={idir} fd={fd} dur={idur} frames={frames} ids={[u16() for _ in range(n)]}")
            continue
        if tag == 'neg':
            cc = (u16(), u16())
            sz = (u16(), u16())
            n = u16()
            hots = []
            for _ in range(n):
                ho = (u16(), u16())
                hs = (u16(), u16())
                hots.append((ho, hs))
            print(f"cc={cc} sz={sz} hots={hots}")
            continue
        if tag == 'code':
            n = u16()
            names = []
            for _ in range(n):
                names.append(cstr())
            print(f"code entries {names}")
            continue
        if tag == 'tspec':
            print(f"tspec ...")
            # skip unknown; stop
            print(" (tspec details skipped)")
            continue
        if tag in ('act',):
            n = u16()
            print(f"act entries {n}")
            continue
        if tag in ('sprite', 'spr'):
            print("(code pointer)")
            continue
        print("(generic, payload unknown - stopping)")
        break
    print(f"consumed {off}/{len(data)} bytes")


if __name__ == '__main__':
    main(sys.argv[1])
