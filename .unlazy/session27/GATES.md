# Gates: Session 27 — true multi-node process split (grid-owner cluster)

OWNS: server/crates/hnh-server/src/nodes.rs, server/crates/hnh-server/src/game.rs,
server/crates/hnh-server/src/state.rs, server/crates/hnh-server/src/main.rs,
server/crates/hnh-server/Cargo.toml, server/scripts/verify_session27.sh,
windows/start-cluster.bat, HANDOFF.md

Scope: the top handoff backlog item — the grid-owner partition becomes a REAL
process split. Multiple server processes form a cluster (`--cluster
host:port,host:port --node N`); each owns the VisIndex cells the rendezvous
hash (grid_owner.rs) assigns it and ticks ONLY the world gobs standing in its
own cells. Player gobs stay authored by their home node (the node the client
connected to) and publish themselves to the owner of whatever foreign cell
they stand in. Sessions see foreign-authority gobs as GUESTS: the owning node
publishes announce/update/retract for cells the session's node subscribed
(view-cell driven), and the subscriber feeds them through the same dirty-cell
visibility machinery as local gobs, so spawns, LINSTEP progress, pose changes
and retractions render identically. Gob ids are globally unique by
construction (per-node slot stride), so wire blocks never need remapping.
Single-node default (`--cluster` absent) is byte-for-byte the previous
behavior: all cells owned, no links, zero added tick cost.

- [ ] G1: nodes.rs — cluster membership parse, serde/bincoded length-prefixed
      node-link messages (Hello/Ping/Sub/Unsub/Chat/GuestAnnounce/GuestUpdate/
      GuestRetract/GuestTransfer), tokio TCP mesh with reconnect + Hello
      handshake, unit tests (codec roundtrip, membership parse/overlap errors).
      CHECK: cargo test -p hnh-server nodes::
      EXPECT: exit 0
      CWD: server

- [ ] G2: Globally-unique gob ids by construction: per-node slot stride in
      Gobs (free list holds only slots with slot % nodes == me) and
      Gobs::spawn_with_id for ownership transfers (forced (slot, gen) insert,
      free-list consistency). Unit tests: disjoint id spaces across nodes,
      spawn_with_id reuses the exact id, no free-list corruption.
      CHECK: cargo test -p hnh-server state::tests::spawn
      EXPECT: exit 0
      CWD: server

- [ ] G3: Authority filtering: with nodes > 1, animal/world-gob simulation
      (movement, AI, combat) only touches gobs whose cell this node owns;
      players are always authored by their home node. Unit test pins that a
      foreign-cell animal is skipped by tick_animals/tick_movement while a
      player in the same foreign cell keeps moving.
      CHECK: cargo test -p hnh-server authority
      EXPECT: exit 0
      CWD: server

- [ ] G4: Guests: subscribe/unsubscribe driven by session view cells;
      GuestAnnounce/Update/Retract ingestion into World.guests; guests flow
      through the SAME visibility machinery (vis index, scan merge, spawn
      blocks, LINSTEP progress, retract sweep, GC when unviewed and
      unsubscribed). Unit tests: announce -> session sees spawn after vis
      scan; update advances pose/position; unsub -> GC retract.
      CHECK: cargo test -p hnh-server guest
      EXPECT: exit 0
      CWD: server

- [ ] G5: Ownership transfer on animal cell crossing (old authority demotes
      the gob to a local guest and sends GuestTransfer; the new authority
      promotes it with the SAME id via spawn_with_id and keeps ticking it)
      and player territory publishing (home node announces players to the
      foreign cell owner, retracts on return). Unit tests cover both
      directions without losing the id.
      CHECK: cargo test -p hnh-server transfer
      EXPECT: exit 0
      CWD: server

- [ ] G6: Area chat crosses nodes through the mesh with the same radius
      semantics (the receiving node filters by the sender's guest position).
      CHECK: cargo test -p hnh-server chat_relay
      EXPECT: exit 0
      CWD: server

- [ ] G7: Two-process e2e on the real GL client: node0 + node1 on loopback,
      bots on node1, real client on node0 walks into node1-owned territory
      and SEES node1's animals/bot (wire gob dump + screenshot read, verdict
      lines CLUSTER VERDICT); the full single-node regression
      (verify_session26.sh subset) and the 300-session load sanity stay
      green.
      CHECK: bash server/scripts/verify_session27.sh
      EXPECT: exit 0
      CWD: .

- [ ] G8: HANDOFF.md Session 27 entry + docs/mechanics networking note
      (cluster mode), worklog appended, commits pushed to origin/master.
