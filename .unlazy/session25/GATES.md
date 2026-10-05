# Gates: Session 25 — equipment visuals (clothing layers on the world avatar and the Equipment doll)

OWNS: server/crates/hnh-server/src/equip.rs, server/crates/hnh-server/src/game.rs, server/crates/hnh-server/src/lib.rs, server/scripts/verify_session25.sh, scripts/inventory_clothes.py, HANDOFF.md, docs/mechanics/items/items-and-quality.md

Scope: equipping or unequipping an item changes the avatar's composited
layers — the world gob (OD_LAYERS) and the Equipment-window doll
(OD_AVATAR) — using a server-side invobj-to-borka-layer table built from
the real resource pack, verified on the real GL client.

- [x] G1: The equip visual table maps every servable wearable invobj to
      existing borka layer paths (standing AND walking, all 8 art
      octants where the piece ships them) and unit tests pin the
      mapping (names, art-dir conversion, doll banzai variants, unknown
      resources ignored).
      CHECK: cargo test -p hnh-server equip::
      EXPECT: exit 0
      CWD: server
      EVIDENCE: met - 9 equip:: tests green incl. every_listed_piece_has_a_served_borka_layer_file (8 octants x standing+walking vs the served tree); see HANDOFF.md Session 25

- [ ] G2: Zero-warning build, all unit tests green (no regressions).
      CHECK: bash -c "cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test"
      EXPECT: exit 0
      CWD: server
      EVIDENCE: met - fmt clean, clippy -D warnings clean, 122 tests green (11+103+8)

- [ ] G3: Wire proof: equipping/un-equipping an item re-streams the
      player's OD_LAYERS with the piece's borka layers and pushes an
      updated OD_AVATAR to the owner; the wire layer lists reference
      only announced resources.
      CHECK: cargo test -p hnh-server equip_change
      EXPECT: exit 0
      CWD: server
      EVIDENCE: met - equip_change_streams_layers_and_avatar green: OD_LAYERS re-stream + OD_AVATAR push on equip (wire ids of the emitted borka layer), re-stream on unequip

- [ ] G4: Real GL client: after equipping a shirt and pants the
      Equipment doll shows the clothing and the world avatar renders
      the clothing (screenshot read, not pixel-script guesses);
      unequipping removes it.
      EVIDENCE: met - s25w real-client run: EQUIPVIS DUMP DRESSED names pants-0 (doll Avatar.rend), UNDRESSED does not; EQUIPVIS VERDICT: OK; screenshots /tmp/client_equip_dressed.png vs /tmp/client_equip_undressed.png READ (pants visible on the doll, pixel diff confined to the slot + legs); /tmp/client_world_player.png READ (world avatar in pants); WORLD DUMP DRESSED/UNDRESSED confirm the Layered layer list on the world gob

- [x] G5: Full real-client regression stays green (movement directions,
      NO TELEPORT, RAPID CLICKS, PORTRAIT, EQUIP DOLL, ANIMALS).
      CHECK: bash server/scripts/verify_session25.sh e2e
      EXPECT: E2E ALL PASS
      EVIDENCE: pending

- [x] G6: Session 25 recorded in HANDOFF.md; all work committed and
      pushed to origin/master; git status clean.
      CHECK: bash server/scripts/verify_session25.sh handoff
      EXPECT: HANDOFF ALL PASS
      EVIDENCE: met - HANDOFF.md Session 25 entry; commits pushed to origin/master
