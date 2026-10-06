# Gates: session36-craft-bow-arrows

OWNS: server/crates/hnh-server/src/craft.rs, server/crates/hnh-server/src/game.rs, server/crates/hnh-server/src/equip.rs, server/crates/hnh-server/src/state.rs, docs/mechanics/crafting/crafting-and-building.md, docs/mechanics/livestock/animals-and-husbandry.md, scripts/verify_session36.sh, HANDOFF.md

Scope: bow-and-arrow craft chain end to end - woodbow/stonearrow/bonearrow recipes with per-type quality weights, animal bone drops, bow carrying pose on equip, plus docs updates.

- [x] G1: Unit battery green on the new recipes and pose wiring
  CHECK: cargo test --manifest-path server/Cargo.toml
  EXPECT: test result: ok
  EVIDENCE: 2026-10-06 ran `bash scripts/verify_session36.sh full` -> "FULL BATTERY: OK" (200 tests green across the workspace: 11 + 180 + 9).

- [x] G2: Lint and format clean
  CHECK: cargo fmt --all -- --check && cargo clippy --manifest-path server/Cargo.toml --all-targets -- -D warnings
  EXPECT: (no output)
  EVIDENCE: 2026-10-06 ran `bash scripts/verify_session36.sh lint` -> "LINT: OK" (fmt --check silent, clippy -D warnings clean after fixing q_note/ALL dead-code and a useless u32 conversion).

- [x] G3: New recipe table resolves the three new ids with RoB quality math
  CHECK: cargo test --manifest-path server/Cargo.toml woodbow -- --nocapture
  EXPECT: test result: ok
  EVIDENCE: 2026-10-06 `bash scripts/verify_session36.sh bow-units` -> "BOW UNITS: OK (4 tests)" + "RECIPE CONSISTENCY: OK": woodbow_quality_is_type_weighted asserts q17 = softcap((40+10)/2, ranged=10); stonearrow_bundles_ten_and_branch_weighs_double asserts a 10-batch at q20 = softcap((10*1+40*2)/3, survive=10); bow_equip_renders_carrying_pose asserts eq-bow carrying layers standing/walking/doll; starter_kit_covers_the_bow_chain asserts kit counts and pagina registration.

- [x] G4: Session verification script: recipes + carrying pose + bone loot probes
  CHECK: bash scripts/verify_session36.sh
  EXPECT: SESSION36 VERIFY: OK
  EVIDENCE: 2026-10-06 phases run individually (bow-units, full, lint, boot) all OK; final combined run in the session log; boot phase boots the release binary with the extended tables.
