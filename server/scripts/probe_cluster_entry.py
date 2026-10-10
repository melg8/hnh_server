#!/usr/bin/env python3
"""Cluster entry probe (session 86).

Enters the world THROUGH a chosen cluster node (auth/game ports as
arguments), creating the character if it does not exist. Combined with
a second run through the other node, this exercises the cross-node
character migration: CharQuery -> "char query: serving snapshot to
peer" on the owner node -> "char migration received: entering world"
on the entering node.

Verdict lines:
  WORLD ENTRY (node <auth-port>): OK

Optional 4th argument = host (session 87: the remote-profile smoke
enters through the OTHER machine's own loopback address, e.g.
  probe_cluster_entry.py user 1873 1874 127.0.0.2
"""

import sys

sys.path.insert(0, "/home/z/my-project/hnh_server/server/scripts")

from hnhlib import MSG_CLOSE, WireClient  # noqa: E402


def main() -> int:
    user = sys.argv[1] if len(sys.argv) > 1 else "clusterentry"
    auth_port = int(sys.argv[2]) if len(sys.argv) > 2 else 1871
    game_port = int(sys.argv[3]) if len(sys.argv) > 3 else 1870
    host = sys.argv[4] if len(sys.argv) > 4 else "127.0.0.1"

    c = WireClient(user, host=host, auth_port=auth_port, game_port=game_port)
    c.connect()
    c.pump(2.0)
    if "charlist" not in c.widgets.values():
        print(f"WORLD ENTRY (node {auth_port}): FAIL (no charlist)")
        return 1
    c.play(user)
    c.pump(6.0)
    if c.mapview_id is None:
        print(f"WORLD ENTRY (node {auth_port}): FAIL (no mapview)")
        return 1
    if not c.tiles:
        print(f"WORLD ENTRY (node {auth_port}): FAIL (no mapdata)")
        return 1
    print(f"WORLD ENTRY (node {auth_port}): OK")
    # Close the session CLEANLY so the character persists immediately
    # (the server saves on SessionClosed) and a re-entry through
    # another node takes the CharQuery migration path instead of an
    # "online" NACK. UDP is fire-and-forget: send twice, then drain.
    c.sock.sendto(bytes([MSG_CLOSE]), c.server)
    c.pump(0.5)
    c.sock.sendto(bytes([MSG_CLOSE]), c.server)
    c.pump(1.0)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
