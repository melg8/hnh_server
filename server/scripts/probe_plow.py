"""Cross-node plow relay probe (session 32).

Connects a FarmClient to an already-running cluster node (node 0 of a
2-node cluster started by verify_session32.sh), decodes grid snapshots
out of the MSG_MAPDATA fragment stream, picks a grass tile whose VisIndex
cell is owned by the OTHER node (python port of grid_owner::owner_of -
rendezvous hashing, same constants), arms the plow pagina and clicks the
tile, then waits for the re-sent MAPDATA to show the PLOWED tile byte.

The companion shell script greps both node logs for the relay pair
("relay plow act sent" on the home node, "relay plow applied" on the
tile authority) which proves the full real path: UDP itemact click ->
home-node relay branch -> TCP mesh RelayPlowAct -> authority validation
+ mutation -> TileMutation broadcast -> home-node apply + MAPDATA
re-send -> client-visible furrow.

Usage: probe_plow.py <username> [game_port]
Prints "PLOW RELAY: OK ..." on success.
"""
import os
import struct
import sys
import time
import zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import test_farming as tf  # noqa: E402
from hnhlib import le32  # noqa: E402
from test_farming import FarmClient  # noqa: E402

MSG_MAPDATA = 5
TILE_GRASS = 13
TILE_PLOWED = 9
M64 = (1 << 64) - 1


def _u64(v):
    return v & M64


def _score(cell, node):
    # grid_owner::node_score: splitmix-style avalanche of the packed cell
    # key and the node index (sign-extended i32 coords into 64 bits).
    # Every product is truncated to 64 bits exactly like the Rust
    # wrapping_mul (verified against a rustc-compiled reference: an
    # unmasked 65th bit from 2 * 0x9E37.. silently flipped owners).
    h = _u64(_u64(cell[0]) * 0x9E3779B97F4A7C15)
    h ^= _u64(_u64(cell[1]) * 0xC2B2AE3D27D4EB4F)
    h ^= _u64(node * 0x165667B19E3779F9)
    h ^= h >> 30
    h = _u64(h * 0xBF58476D1CE4E5B9)
    h ^= h >> 27
    h = _u64(h * 0x94D049BB133111EB)
    h ^= h >> 31
    return h


def owner_of(cell, nodes):
    # grid_owner::owner_of: argmax, strict > so the lowest node wins ties.
    best, best_score = 0, -1
    for node in range(nodes):
        s = _score(cell, node)
        if node == 0 or s > best_score:
            best, best_score = node, s
    return best


def cell_of(x, y):
    # visidx::cell_of over the 250-subtile lattice (floor division).
    return (x // 250, y // 250)


class PlowProbe(FarmClient):
    def __init__(self, username, game_port):
        super().__init__(username)
        self.server = ("127.0.0.1", game_port)
        self.pending = {}  # pktid -> {"total": n, "chunks": {off: bytes}}
        self.grids = {}  # gc -> 10000 tile bytes (latest snapshot)

    # ---- MAPDATA fragment assembly ------------------------------------
    def on_datagram(self, data):
        if data[0] == MSG_MAPDATA:
            pktid = struct.unpack("<i", data[1:5])[0]
            off = struct.unpack("<H", data[5:7])[0]
            total = struct.unpack("<H", data[7:9])[0]
            chunk = data[9:]
            ent = self.pending.setdefault(pktid, {"total": total, "chunks": {}})
            ent["chunks"][off] = chunk
            got = sum(len(c) for c in ent["chunks"].values())
            if got >= total:
                buf = bytearray(total)
                for o, c in sorted(ent["chunks"].items()):
                    buf[o : o + len(c)] = c
                self._ingest_grid(bytes(buf))
                del self.pending[pktid]
            return
        super().on_datagram(data)

    def _ingest_grid(self, payload):
        gx = struct.unpack("<i", payload[0:4])[0]
        gy = struct.unpack("<i", payload[4:8])[0]
        nend = payload.index(0, 8)
        o = nend + 1
        while payload[o] != 255:  # plot flags table
            o += 2
        o += 1
        tiles = zlib.decompress(payload[o:])[:10000]
        self.grids[(gx, gy)] = tiles

    def mapreq_grid(self, gc):
        # Grid snapshot request. Named apart from WireClient.mapreq(gx,
        # gy), which the shared mapview-bind path calls.
        self.sock.sendto(bytes([4]) + le32(gc[0]) + le32(gc[1]), self.server)

    def tile_at(self, tx, ty):
        gc = (tx // 100, ty // 100)
        tiles = self.grids.get(gc)
        if tiles is None:
            return None
        return tiles[(ty % 100) * 100 + (tx % 100)]


def main():
    username = sys.argv[1] if len(sys.argv) > 1 else "plowbot"
    port = int(sys.argv[2]) if len(sys.argv) > 2 else 1870
    nodes = int(sys.argv[3]) if len(sys.argv) > 3 else 2
    c = PlowProbe(username, port)
    c.connect()
    print("session accepted")
    c.pump(1.5)
    c.play(username)
    ok = c.wait_for(lambda: c.mapview_id is not None and c.player_gob is not None, 12)
    assert ok, "world entry incomplete"
    print("world entry: player gob", c.player_gob)

    # Plow arming has no skill gate, but copy the proven farmbot prelude
    # (char sheet open + farming value) so the arm path is identical to
    # the one test_farming.py exercises end to end.
    tf.buy_farming_value(c)
    print("farming skill value raised via sattr")

    ppos = c.gobs[c.player_gob]["pos"] or (0, 0)
    ptile = (ppos[0] // 11, ppos[1] // 11)

    # Scan outward for a GRASS tile whose tile-center cell belongs to the
    # other node; request its grid and decode the snapshot. The relay
    # branch fires purely on the tile's cell owner, so the first hit is
    # a valid cross-node plow target.
    target = None
    for r in range(1, 26):
        for dy in range(-r, r + 1):
            for dx in range(-r, r + 1):
                if max(abs(dx), abs(dy)) != r:
                    continue
                tile = (ptile[0] + dx, ptile[1] + dy)
                cell = cell_of(tile[0] * 11 + 5, tile[1] * 11 + 5)
                if owner_of(cell, nodes) == 0:
                    continue  # my own cell: the local path, not the relay
                gc = (tile[0] // 100, tile[1] // 100)
                if gc not in c.grids:
                    c.mapreq_grid(gc)
                    c.wait_for(lambda: gc in c.grids, 4)
                if c.tile_at(*tile) != TILE_GRASS:
                    continue
                target = tile
                break
            if target:
                break
        if target:
            break
    assert target is not None, "no foreign-cell grass tile found near spawn"
    gc = (target[0] // 100, target[1] // 100)
    cell = cell_of(target[0] * 11 + 5, target[1] * 11 + 5)
    owner = owner_of(cell, nodes)
    print(
        "target tile=%s grid=%s cell=%s owner=node%d (foreign)" % (target, gc, cell, owner)
    )

    # Arm the plow pagina and click the foreign tile through the real
    # widget chain (same sequence the farmbot uses).
    c.arm_plow()
    c.pump(0.25)
    c.click_tile(target)
    ok = c.wait_for(lambda: c.tile_at(*target) == TILE_PLOWED, 6)
    if not ok:
        print("PLOW RELAY: FAIL (no re-sent MAPDATA with the PLOWED tile)")
        sys.exit(1)
    print("PLOW RELAY: OK tile=%s grid=%s owner=node%d" % (target, gc, owner))


if __name__ == "__main__":
    main()
