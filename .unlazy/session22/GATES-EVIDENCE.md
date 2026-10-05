# session22 evidence

- G1 PASS: cargo fmt --check clean; clippy --workspace --all-targets
  -D warnings clean; workspace tests 11+75+8 green (94 total), Rust
  1.99.0.
- G2 PASS: art_dir_offsets_the_sprite_ring +
  pose_layers_compose_direction_and_kind +
  walk_layers_swap_between_walking_and_standing all green; grep count 3
  (function name + pinned digits walking/legs-2, walking/walking-4).
- G3 PASS: scripts/dump_directions.py sheet
  (/home/z/my-project/session22/fox_all8.png) read frame by frame:
  art 0 head-on front (symmetric, chest), art 1 down-left 3/4 (head
  left, chest visible), art 2 pure left profile, art 3 up-left 3/4
  (back visible), art 4 back (symmetric, no face), art 5 up-right 3/4
  back, art 6 pure right profile, art 7 down-right 3/4 front. Sprite N
  depicts movement octant N+1; both user data points fit exactly.
- G4 PASS: s22p e2e run on the real GL client (Xvfb, JOGL, JDK 8):
  EAST frame = down-right 3/4 front (octant 0 -> sprite 7);
  NORTH frame (screen up-right leg, delta (733,477)->(735,569) prior
  east, leg delta (-2,-92) = octant 6) = up-right 3/4 back (sprite 5);
  SOUTH frame (delta (45,175) = octant 2) = down-left 3/4 front
  (sprite 1); UP frame (delta (-53,-157)) = back view — the exact
  user-reported defect direction, previously showed the up-right set;
  LEFT leg retargeted around an obstacle (arrival delta (21,131) =
  octant 2) and its frame shows the down-left 3/4 front matching the
  actual travel. Charlist portrait renders head-on front (PORTRAIT
  LAYERS: ...legs-0). Pre-fix run (s22dir) showed legs-7 portrait,
  confirming the pre-fix path.
- G5 PASS: verdict lines in /tmp/client_s22p.log: MOVEMENT: MOVED,
  PORTRAIT LAYERS legs-0, EQUIP DOLL avagob=65536 ava-rend=OK,
  ANIMALS SCREENSHOT saved kritter (count 4).
- G6 PASS: ANIMALS SCREENSHOT: saved kritter at 748,272
  res=gfx/kritter/boar/body/walking/walking-2; the frame shows the boar
  mid-walk facing screen-left in pure left profile (sprite 2 = octant
  3), direction matching its travel.
