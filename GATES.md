# Session 47 Gates

Scope: close the two carried taming items that were verifiable here -
tamed-animal persistence (save v6) and tamed-animal production (milk /
wool meters + the Milk / Shear collection flows) - plus the
tile_overrides persistence bugfix found while wiring the animals in.
Food Trough / breeding / per-animal breed stats are NOT gated: they
need object + stat-row infrastructure and stay in the livestock doc's
Open questions.

unlazy note: the caller requested "tree 99". Depth 99 would create
filler leaves; per the method, the closest honest decomposition for a
single 2-hour session is this solo ledger with independently runnable
gates.

## G1: the unit battery, fmt and clippy stay green

Runnable.
  CHECK: cd server && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test --release
  EXPECT: test result: ok (267 tests across binaries: 247 hnh-server, 11 hnh-world, 9 hnh-proto)

## G2: production accrues on pasture only, at the documented rates

Runnable.
  CHECK: cd server && cargo test --release -- cow_production milk_caps wool_accrues
  EXPECT: milk q10 = 1 unit of 0.01 L per 600 ticks (0.1 L / 10 min),
  paused off-pasture with a frozen accumulator; the 10 L cap lands and
  stops banking time; wool mints through the accumulator and caps at 3.

## G3: the collection flows pay out correctly

Runnable.
  CHECK: cd server && cargo test --release -- milking shearing wild_and
  EXPECT: milk consumes a bucket, drains the meter, grants bucket-milk
  at q10; no bucket refuses with the meter intact; shear grants the
  whole wool stack and empties the meter; wild and mid-taming animals
  keep the fight path.

## G4: tamed animals survive restarts

Runnable.
  CHECK: cd server && cargo test --release -- tamed_animals_persist_roundtrip
  EXPECT: tameness, meters, accumulator and the domestic morph restore;
  full tameness never re-arms the leash, partial tameness re-arms it.

## G5: wire probes on the release binary

Runnable.
  CHECK: cd server && cargo build --release && python3 scripts/test_client.py s47 && python3 scripts/probe_melee.py && python3 scripts/probe_animals.py
  EXPECT: WORLD ENTRY: OK; MELEE WIRE: OK; ANIMALS WIRE: OK
