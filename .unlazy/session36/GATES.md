# Gates: session36-craft-bow-arrows

OWNS: server/crates/hnh-server/src/craft.rs, server/crates/hnh-server/src/game.rs, server/crates/hnh-server/src/equip.rs, server/crates/hnh-server/src/state.rs, docs/mechanics/crafting/crafting-and-building.md, docs/mechanics/livestock/animals-and-husbandry.md, scripts/verify_session36.sh, HANDOFF.md

Scope: bow-and-arrow craft chain end to end - woodbow/stonearrow/bonearrow recipes with per-type quality weights, animal bone drops, bow carrying pose on equip, plus docs updates.

- [x] G1: Unit battery green on the new recipes and pose wiring
  CHECK: /home/z/.cargo/bin/cargo test --manifest-path server/Cargo.toml
  EXPECT: test result: ok
  CWD: /home/z/my-project/hnh_server
  EVIDENCE: automatic-evidence=v1; definition-sha256=c159227bdb98409c136e602c0a56e2f8c60e9fdda2a6637391e4a6753c210758; exit=0; EXPECT=matched; output-sha256=aa86f359a4f7e5bbeb7cee56eb4a3952ee452092e56bfe8f571143e9d0e79e87; output-bytes=13755; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries

- [x] G2: Lint and format clean
  CHECK: /home/z/.cargo/bin/cargo fmt --all -- --check && /home/z/.cargo/bin/cargo clippy --all-targets -- -D warnings && echo LINT-CLEAN
  EXPECT: LINT-CLEAN
  CWD: /home/z/my-project/hnh_server/server
  EVIDENCE: automatic-evidence=v1; definition-sha256=fb2ea70ad08c3ebed2406bb5e42bdc9da1a680bc036db3448bbc67456afebf6b; exit=0; EXPECT=matched; output-sha256=1d58269d26fca19142415547b36df937295b87c9e486f20c3079776038f1221d; output-bytes=84; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/server; path=cc94915413e1/11 entries

- [x] G3: New recipe table resolves the three new ids with RoB quality math
  CHECK: /home/z/.cargo/bin/cargo test --manifest-path server/Cargo.toml woodbow
  EXPECT: test result: ok
  CWD: /home/z/my-project/hnh_server
  EVIDENCE: automatic-evidence=v1; definition-sha256=696c5fe60d5e99884eca3825bc21f873beb54f55f17fa4304dfdc41f11f3208c; exit=0; EXPECT=matched; output-sha256=9514a6d6844fb0b52d70201290cbbad7d9d3913b24f32e6968a592014f5469b6; output-bytes=739; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries

- [x] G4: Session verification script: recipes + carrying pose + bone loot probes
  CHECK: bash scripts/verify_session36.sh all
  EXPECT: SESSION36 VERIFY: OK
  CWD: /home/z/my-project/hnh_server/server
  EVIDENCE: automatic-evidence=v1; definition-sha256=a04e33e414cba2c783a3bffb768fc7c0317cf55b2262af5a32e45d16e49b6cdf; exit=0; EXPECT=matched; output-sha256=d7de34bd15e7dd4b6f4eb918c2ea0ea62679387aa428fa132525c9ed38f3e500; output-bytes=255; shell=/bin/sh; cwd=/home/z/my-project/hnh_server/server; path=cc94915413e1/11 entries
