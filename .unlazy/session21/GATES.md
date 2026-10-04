# session21 — directional animations, visible+animated animals, equipment paperdoll

User-reported defects (session 21 input):
1. Walk animation does not match the movement direction: the character
   spins around its own axis; must instead pick ONE direction and show
   only that direction's animation frames.
2. Animals are invisible (only their shadows render); no walk animation,
   no attack animation.
3. The client "was fully working against the original server" — audit
   whether earlier session hacks broke native client behavior; restore
   client-native mechanisms where they were bypassed.
4. The Equipment window is missing the character doll (spread arms and
   legs, i.e. the native paperdoll figure).

## Gates

### G1 server-builds-clean
cargo fmt check, clippy -D warnings, and all tests pass.

CHECK: cd server && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1 | tail -5
EXPECT: test result: ok

### G2 no-frame-streaming-hack
The server no longer streams per-frame OD_LAYERS updates for player
walking (session 20's 150 ms/frame hack); walking animation comes from
the client-native mechanism (gob movement state + direction), proven by
grepping the movement tick path for periodic layer/frame resends.

CHECK: rg -n "150" server/crates/hnh-server/src/game.rs | wc -l | grep -qx 0 && rg -n "fn tick_movement" -A 40 server/crates/hnh-server/src/game.rs | rg -c "encode_gob_block|OD_LAYERS" || true
EXPECT: (exit 0, no layer resend inside tick_movement)

### G3 direction-attribute-on-wire
When a move starts, the server transmits the movement direction to the
client in the native form the client's Composite/lin path reads (angle
or explicit direction), verified by a wire probe decoding LINBEG and
the direction attribute on the same datagram stream.

CHECK: python3 server/scripts/probe_direction.py 2>&1 | tail -3
EXPECT: DIRECTION WIRE: OK

### G4 real-client-walk-direction
On the real GL client, a walked path shows the character facing the
travel direction with a walk animation (no spinning), verified by
reading saved mid-walk screenshots for two opposite directions.

CHECK: (manual) read /tmp/client_walk_east.png and /tmp/client_walk_west.png from the e2e run
EXPECT: frames show the walking pose facing along the path in both directions

### G5 animals-render-and-animate
On the real GL client in a world with animals: animal gobs render with
sprites (not shadows only), and at least one animal walk and one
attack/engagement animation state is observed on the wire (pose/anim
attributes sent when a predator chases).

CHECK: python3 server/scripts/probe_animals.py 2>&1 | tail -3
EXPECT: ANIMALS WIRE: OK

### G6 real-client-animals-visible
On the real GL client screenshot in the animal area, animal sprites are
visible (screenshot pixel check or visual read), not shadow-only.

CHECK: (manual) read /tmp/client_animals.png from the e2e run
EXPECT: at least one non-shadow animal sprite visible

### G7 equipment-paperdoll
The Equipment window ("equ"/"epry") bootstrap includes the character
doll (spread-arms pose) per the native client widget tree; verified on
the real client by opening Equipment and reading the screenshot.

CHECK: (manual) e2e equip step opens the equipment window; read the screenshot
EXPECT: paperdoll figure visible with slots

### G8 regression-suite
Full movement/portrait/equip real-client e2e battery still passes
(MOVEMENT: MOVED, NO TELEPORT: OK, RAPID CLICKS: GLIDING,
PORTRAIT: OK, EQUIP OK) after the animation rework.

CHECK: (manual) run scripts/jogl/run-real-client-e2e.sh s21k s21
EXPECT: all pass lines green
