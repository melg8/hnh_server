# Gates: session 34 - build-transition publish, live station probe, 300/node load

Scope: fix the cluster publish gap on plan stage advance and completion
(guest peers never saw Structure -> Station), drive the full guest-oven
lifecycle through the real 2-node mesh (fuel, input, light, roast
output), land the deferred 300-bot-per-node load story, and keep the
whole regression battery green.

- [ ] G1: build transitions publish to cluster subscribers
  A plan's stage advance (sink_material) and its completion
  (complete_plan: Kind::Plan -> Kind::Station) re-publish GuestUpdate
  to the cell owner's subscribers: a peer watching the build sees the
  stage sdt byte move and, on completion, the guest row's class flips
  Structure -> Station with the StationView snapshot attached. Without
  this, a guest oven built by a peer stays a dead Structure forever
  (fuel/input/light relay all key off the Station class).
  CHECK: bash server/scripts/verify_session34.sh station-units
  CWD: /home/z/my-project/hnh_server
  EXPECT: STATION UNITS: OK
  EVIDENCE: pending

- [ ] G2: live guest-oven lifecycle through the real mesh
  On a real 2-node cluster (TCP mesh + real UDP), a builder character
  on node 1 builds an oven next to the shared spawn through the REAL
  build flow (place -> stone x2 -> branch x1), then a probe character
  homed on node 0 drives the full guest lifecycle: branch itemact ->
  "Fuel added to the oven." system line, meat itemact -> "Input
  loaded; right-click the oven to light it.", oven click -> Light
  flower menu (snapshot driven), choice -> the oven gob re-renders
  with lit sdt 1, and the roast output drop appears next to the oven
  on the authority.
  CHECK: bash server/scripts/verify_session34.sh cluster-station
  CWD: /home/z/my-project/hnh_server
  EXPECT: STATION RELAY: OK
  EVIDENCE: pending

- [ ] G3: relay pair present in both node logs
  The cluster phase must prove the real relay path end to end: the
  home node (node 0) logs "relay station item sent" + "relay station
  act sent", the authority (node 1) logs "relay station fueled" +
  "relay station input loaded" + "relay station lit".
  CHECK: bash server/scripts/verify_session34.sh cluster-station
  CWD: /home/z/my-project/hnh_server
  EXPECT: relay pair verified on both node logs
  EVIDENCE: pending

- [ ] G4: 300-bot cohorts per node stay within the tick budget
  A 2-node cluster with --bots 300 per node (600 sessions live, all
  walking/fighting through the real mesh) holds max_tick_us < 100000
  on BOTH nodes with the cohorts visible in the perf counters, and
  the sharded save persists bot characters on both nodes. This is the
  300+/node load story deferred from the session-33 handoff.
  CHECK: bash server/scripts/verify_session34.sh load-300
  CWD: /home/z/my-project/hnh_server
  EXPECT: 300/NODE LOAD: OK
  EVIDENCE: pending

- [x] G5: full regression battery
  Every unit test passes, the session-30 E2E (600-bot window +
  cluster 60+60 + shard persist + restart restore) stays green on
  this tree, and the session-32/33 verify scripts re-run green (wire,
  relay-plow, station phases).
  CHECK: bash server/scripts/verify_session34.sh regression
  CWD: /home/z/my-project/hnh_server
  EXPECT: REGRESSION: OK
  EVIDENCE: automatic-evidence=v1; definition-sha256=36a3690d797039af90afa20f71868967af51299caf2a7ace313664c9480af137; exit=0; EXPECT=matched; output-sha256=83228a92dd0b98ecfc3ab3781529b3bebc14d681f1a23eb3310e4891bb63401c; output-bytes=162; shell=/bin/sh; cwd=/home/z/my-project/hnh_server; path=cc94915413e1/11 entries

- [ ] G6: handoff + worklog updated, commits pushed
  HANDOFF.md carries a dated session-34 entry (what/why/evidence/
  NEXT), the worklog records the session, and every commit is pushed
  to origin/master.
  CHECK: bash server/scripts/verify_session34.sh handoff
  CWD: /home/z/my-project/hnh_server
  EXPECT: HANDOFF: OK
  EVIDENCE: pending
