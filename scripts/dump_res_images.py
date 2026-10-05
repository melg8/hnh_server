#!/usr/bin/env python3
"""Dump the image layers of a .res file with the client's exact header
layout (Resource.Image constructor): z int16@0, subz int16@2, flags
byte@4, id int16@5, off int16@7 + int16@9, image bytes from 11."""
import struct
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from add_neg_layer import parse_res  # noqa: E402


def main() -> int:
    for path in sys.argv[1:]:
        ver, layers = parse_res(path)
        print(f"{path} (v{ver}):")
        for name, blob in layers:
            if name != "image":
                continue
            z = struct.unpack("<h", blob[0:2])[0]
            subz = struct.unpack("<h", blob[2:4])[0]
            flags = blob[4]
            ident = struct.unpack("<h", blob[5:7])[0]
            ox = struct.unpack("<h", blob[7:9])[0]
            oy = struct.unpack("<h", blob[9:11])[0]
            payload = len(blob) - 11
            png = blob[11:15] == b"\x89PNG"
            print(
                f"  image id={ident} z={z} subz={subz} flags={flags:#04x} "
                f"off=({ox},{oy}) png={png} payload={payload}B"
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
