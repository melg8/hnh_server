# Session 46 Gates

Scope: close the session-45 NEXT taming-depth items that are verifiable
in this environment (Animal Husbandry skill gate, battle-intensity
de-escalation, species morph at full tameness) plus the carried recipe
tool/station plumbing. Weapon-slot-only rope check is NOT gated here:
slot semantics need real-client verification and stay documented as
policy (any equipped slot) - see animals-and-husbandry.md.

unlazy note: the caller requested "tree 99". Depth 99 would create
filler leaves; per the method, the closest honest decomposition for a
single 2-hour session is this solo ledger with independently runnable
gates.

## G1: the unit battery, fmt and clippy stay green

Runnable.
  CHECK: cd server && cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test --release
  EXPECT: test result: ok (258 tests)

## G2: the Animal Husbandry skill gates both purchase and quell

Runnable.
  CHECK: cd server && cargo test --release -- skills:: ahusb
  EXPECT: ahusb tests pass (prereq enforced in buy, quell refuses without the skill)

## G3: battle intensity de-escalates and gates the quell

Runnable.
  CHECK: cd server && cargo test --release intensity
  EXPECT: intensity tests pass (rises on blows, de-escalates when calm, quell requires a calm battle)

## G4: full tameness morphs the species in place

Runnable.
  CHECK: cd server && cargo test --release morph
  EXPECT: morph tests pass (mouflon->sheep, aurochs->cow, resource swap, non-morphing species stay)

## G5: tool requirement plumbing and new recipes

Runnable.
  CHECK: cd server && cargo test --release tool_req
  EXPECT: tool-requirement tests pass (recipe without the tool refuses, with the tool crafts)

## G6: wire probes on the release binary

Runnable.
  CHECK: cd server && cargo build --release && python3 scripts/test_client.py s46 && python3 scripts/probe_melee.py
  EXPECT: WORLD ENTRY: OK; MELEE WIRE: OK

## G7: docs + HANDOFF entry + commits pushed

Manual. animals-and-husbandry.md, crafting-and-building.md, skills doc
updated with session-46 implementation notes; HANDOFF.md entry appended;
commits on origin/master.
