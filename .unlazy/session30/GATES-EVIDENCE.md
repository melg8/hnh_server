# GATES evidence — session 30

Recorded after the final tree (post e2e). All evidence re-measured on
the committed tree; commands run per the repo conventions.

- G1 relay-static: MET.
  CHECK ran: bash server/scripts/verify_session30.sh s30final
  -> "RELAY-STATIC VERDICT: OK" (5/5 unit tests; mesh codec roundtrips
     included in the 160-test battery).
- G2 cursor-merge: MET. "CURSOR-MERGE VERDICT: OK" (3/3).
- G3 vis-cache: MET.
  "VIS-CACHE UNIT: OK" + 600-bot window p50=8411us max=49985us
  (budget 100000us). Worst-case parity measured separately at 1000
  walking bots (p50 21.6ms vs 20.7ms pre-change baseline, same box).
- G4 cluster-load: MET.
  "CLUSTER LOAD VERDICT: OK" (nodes at 12-14ms max tick), 60+60 cohorts
  persisted in both shards, full cluster restart restored 60+60 bot
  snapshots. "SESSION30 E2E: OK".
- G5 unit battery: MET. 160 tests green (11 proto + 141 server incl.
  9 new + 8 world), `cargo test --all` exit 0.
- G6 clippy: MET. `cargo clippy --all-targets -- -D warnings` exit 0.
- G7 fmt: MET. `cargo fmt --all -- --check` exit 0.
- G8 wire e2e: MET. test_client "WORLD ENTRY: OK" + test_craft
  "EAT FLOW: OK" on the final tree.
- G9 real client (manual): MET. After sandbox re-provisioning
  (deploy-agent-env.sh + ant jar, JDK8): MOVEMENT: MOVED, WALKDIR x5
  ARRIVED, SPEED 3.43 tiles/s OK, NO TELEPORT OK, RAPID CLICKS GLIDING,
  PORTRAIT layers present, EQUIPVIS OK, CURSOR OK, GROUNDDROP OK.
- G10 craft wiring (stretch): SKIPPED, honestly.
  Ring of Brodgar is behind a Cloudflare JS challenge; fandom/wiki
  mirrors carry no recipe tables for the remaining paginae. Per
  AGENTS.md, recipe numbers are not invented - the item stays in the
  handoff NEXT list.

ABANDON: none.
