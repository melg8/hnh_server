# session21 — gate evidence

All leaf gates met with in-session evidence (commands + outputs recorded
in the session transcript and HANDOFF.md "Session 21").

- G1 server-builds-clean: MET. `cargo fmt --all -- --check` clean,
  `cargo clippy --all-targets -- -D warnings` 0 errors, `cargo test`
  11+74+8 = 93 tests green (run after each server change).
- G2 no-frame-streaming-hack: MET. tick_walk_pose deleted (the 150 ms
  frame cycler); the movement tick path contains no OD_LAYERS resend —
  streams fire only from start_move/finish on pose/direction CHANGE
  (pose_streamed one-byte dedupe). Verified by the unit test
  walk_layers_swap_between_walking_and_standing (no re-stream mid-walk).
- G3 direction-attribute-on-wire: MET. probe_direction.py ->
  "DIRECTION WIRE: OK": three legs (+x, +y, -x-y), each exactly one
  walking layer stream + one standing stream whose direction digit
  matches the quantized octant, plus the banzai doll set on spawn.
- G4 real-client-walk-direction: MET. WALKDIR EAST/NORTH/SOUTH ARRIVED
  with mid-walk frames read visually: east = front view, north = right
  profile, south = left profile; one direction per leg, no spinning.
- G5 animals-render-and-animate: MET. probe_animals.py ->
  "ANIMALS WIRE: OK": 330 kritter OD_LAYERS spawns, 0 flat cdv RES
  animals, 300+ walking-pose streams, bite overlay gfx/fx/bite observed
  on the player gob; 1517 fights opened in the saturated world.
- G6 real-client-animals-visible: MET. ANIMALS SCREENSHOT: saved kritter
  at 812,268 res=gfx/kritter/boar/body/walking/walking-3; the boar sprite
  (tusks + body) is visible in /tmp/client_animals.png (read visually).
- G7 equipment-paperdoll: MET. EQUIP DOLL: avagob=65536 ava-rend=OK;
  /tmp/client_equip.png + the full-window frame show the spread-arms
  (banzai) doll inside the Equipment window frame.
- G8 regression-suite: MET. One real-client run (s21zoo6/s21i) carried
  MOVEMENT: MOVED, SPEED 3.43 tiles/s (SPEED VERDICT: OK), NO TELEPORT:
  OK, RAPID CLICKS: GLIDING, PORTRAIT LAYERS present, plus the new
  WALKDIR/EQUIP/ANIMALS evidence; wire WORLD ENTRY OK; load smoke 300
  bots tick ~4.8 ms (budget 100 ms).
