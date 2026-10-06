# Gates: session 35 - drop authority transfer across cell boundaries

Scope: close the session-34 known limitation - a gob (station output
drop, stone rubble, any Kind::Drop) spawned by a node onto a cell it
does not own is invisible to every player homed on the cell owner
(peers never subscribe to their OWN cells). Mirror the animal
authority-transfer path: the spawner sends GuestTransfer to the cell
owner and demotes its copy to a guest; the owner claims the exact id
into its sim tables and publishes it back to its subscribers. Also
re-measure the single-node 1000-session window on the post-session-34
tree (the tick_guests batch rewrite should lift the single-node
numbers beyond the 600-bot evidence).

- [ ] G1: drop transfer unit battery
  A Kind::Drop spawned in a foreign cell (spawn jitter across a cell
  boundary) sends GuestTransfer to the cell's owner carrying the FULL
  drop payload (inventory resource name, quality, display label),
  demotes the local copy to a guest with the SAME id, and the
  receiving node claims it back into Kind::Drop with the exact id,
  the same position, the deterministic world shape, and a working
  pickup (drop_info intact).
  CHECK: cd /home/z/my-project/hnh_server/server && cargo test --release drop_transfer -- --nocapture
  CWD: /home/z/my-project/hnh_server/server
  EXPECT: 3 passed

- [ ] G2: live cross-boundary drop lifecycle through the real mesh
  On a real 2-node cluster, a station built near the shared spawn
  boundary produces an output drop that lands in the PEER's cell: the
  peer's probe session must SEE the drop (guest announce through the
  transfer path) and PICK IT UP through the relay (pickup ack restores
  the stack into the probe's inventory). This proves the transfer end
  to end: no more invisible boundary drops.
  CHECK: bash server/scripts/verify_session35.sh cluster-drop
  CWD: /home/z/my-project/hnh_server
  EXPECT: DROP TRANSFER: OK

- [ ] G3: single-node 1000-session load window re-measured
  A single node with --bots 1000 --saturated (the session-2/30 load
  gate shape) holds max_tick_us < 100000 in the steady state with all
  1000 sessions live, on the post-session-34 tree (batch tick_guests).
  CHECK: bash server/scripts/verify_session35.sh load-1000
  CWD: /home/z/my-project/hnh_server
  EXPECT: 1000-BOT LOAD: OK

- [ ] G4: full regression battery
  Every unit test passes (191+3), clippy --all-targets -D warnings
  clean, cargo fmt --check clean, and the session-34 verification
  phases (station-units + cluster-station) stay green on this tree.
  CHECK: bash server/scripts/verify_session35.sh regression
  CWD: /home/z/my-project/hnh_server
  EXPECT: REGRESSION: OK

- [ ] G5: handoff record and push
  HANDOFF.md carries the session-35 entry with evidence, the worklog
  is appended, and all commits are pushed to origin/master.
  CHECK: bash server/scripts/verify_session35.sh handoff
  CWD: /home/z/my-project/hnh_server
  EXPECT: HANDOFF: OK
