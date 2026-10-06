# Gates: session 35 - drop authority transfer across cell boundaries

Scope: close the session-34 known limitation - a Kind::Drop spawned by
a node onto a cell it does not own (station output jitter across the
boundary, stone rubble, loot) was invisible to every player homed on
the cell owner. Mirror the animal authority-transfer path (GuestTransfer
with a full DropView payload + local demote; the owner claims the exact
id), prove it live through the real 2-node mesh, and re-measure the
single-node 1000-session window on the post-session-34 tree.

- [x] G1: drop transfer unit battery
  EVIDENCE: automatic-evidence=v1; definition-sha256=8d151b5474bf4d2534ae50c2f65efd4eba4762bb0372e358426fdfc3c8505250; exit=0; EXPECT=matched; output-sha256=91f5178d1502a3b62770c10f55c233961b50d1aa3b7c80fcbefc30eb63ecf147; output-bytes=73; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries
  A drop spawned in a foreign cell sends GuestTransfer to the cell's
  owner carrying the full payload (inventory resource name, quality,
  label) and demotes locally to a guest under the same id; the
  receiver claims it back into Kind::Drop with a working pickup
  payload; a drop on an OWNED cell never transfers.
  CHECK: bash server/scripts/verify_session35.sh drop-units
  CWD: /home/z/my-project/hnh_server
  EXPECT: DROP UNITS: OK

- [x] G2: live cross-boundary drop lifecycle through the real mesh
  EVIDENCE: automatic-evidence=v1; definition-sha256=94b963aa59675e54601be22806d5522ef072a1cc0d8ba504fcc7621d410812cf; exit=0; EXPECT=matched; output-sha256=4465e0dd2208048ab4a81a951164df7bb724570292e1b2b02034da730b9d136e; output-bytes=143; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries
  On a real 2-node cluster a rim oven's output drop (or a rim-tree
  chop drop) that crosses the cell boundary transfers to the peer:
  the probe's node claims it and a plain local click restores the
  stack into the probe inventory; both node logs carry the transfer
  pair (transferred on the spawner, claimed on the owner).
  CHECK: bash server/scripts/verify_session35.sh cluster-drop
  CWD: /home/z/my-project/hnh_server
  EXPECT: DROP TRANSFER: OK

- [x] G3: single-node 1000-session load window re-measured
  EVIDENCE: automatic-evidence=v1; definition-sha256=00054c548c135c4a52a7552bdf833e1d3421501c4386099352750fca130dbaa9; exit=0; EXPECT=matched; output-sha256=62cbc955e231892b8d5d08ffc96e7467eb0b16a82fde358c8912f2d20e88ee6a; output-bytes=118; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries
  A single node with --bots 1000 --saturated holds max_tick_us <
  100000 with at least 950 live sessions in the steady state.
  CHECK: bash server/scripts/verify_session35.sh load-1000
  CWD: /home/z/my-project/hnh_server
  EXPECT: 1000-BOT LOAD: OK

- [x] G4: full regression battery
  EVIDENCE: automatic-evidence=v1; definition-sha256=c096fb75b0de0e5d6851d1047cb8efd1c8dd38eb640b34ed286757b606ef531a; exit=0; EXPECT=matched; output-sha256=321572a68516eeff136f2385ecfe0332ac707b7122470a2ae8e5e6c213ae3a97; output-bytes=168; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries
  Every unit test passes, clippy --all-targets -D warnings is clean,
  and the session-34 phases (station-units + cluster-station) stay
  green on this tree.
  CHECK: bash server/scripts/verify_session35.sh regression
  CWD: /home/z/my-project/hnh_server
  EXPECT: REGRESSION: OK

- [x] G5: handoff record and push
  EVIDENCE: automatic-evidence=v1; definition-sha256=f319b49b4d692f725791925c7be1ede7dcd01786c8cb4b5437651f316700b00a; exit=0; EXPECT=matched; output-sha256=680d901f37f1249793d971cedc554ab9529b8594274664c70122698be63c3152; output-bytes=280; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries
  HANDOFF.md carries the session-35 entry, all work is committed, and
  HEAD is pushed to origin/master.
  CHECK: bash server/scripts/verify_session35.sh handoff
  CWD: /home/z/my-project/hnh_server
  EXPECT: HANDOFF: OK
