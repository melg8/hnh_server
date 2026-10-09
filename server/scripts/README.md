# server/scripts — probe and verification harness

Black-box wire probes for the hnh-server protocol, plus the shared
harness they run on. The canonical `cargo test` coverage lives in
`../crates/hnh-server/tests/` (unit + wire integration); these scripts
carry the richer scenario contracts (build flow, farming, party chat,
taming) that are easier to drive from python.

## The shared harness

- `hnhlib.py` — single source of the transport plumbing:
  - wire constants (byte-exact with `crates/hnh-proto/src/consts.rs`),
  - `auth_cookie()` (TLS auth handshake), `ensure_server()` (boots an
    isolated release server on a fresh save when none listens),
  - `parse_objdata()` — THE OBJDATA op decoder; semantics matched to
    the server's own `bots.rs parse_objdata` (OD_REM is a no-payload
    op, OD_OVERLAY 65535 is a removal without sdt, a flag-1 block
    carries no ops). When the server encoder grows an op, update this
    one function.
  - `WireClient` — session driver: reliable stream + cumulative ACK +
    hold-back, widget/RESID/gob/item tracking, the real client's
    bootstrap behaviors (3x3 MAPREQ on mapview bind, opt-in `chr`
    request on slen bind, batched MSG_OBJACK behind a flag), and an
    `on_event()` hook for script-specific widgets.
- `test_build.py` — build/station/persist scenario CLI and the
  `BuildClient` the station/equip probes build on.

## Probes

| script | contract | verdict line |
|--------|----------|--------------|
| test_client.py | bootstrap + cattr order + walk click | `WORLD ENTRY: OK` |
| probe_walk.py | ground click -> LINBEG/LINSTEP, exact arrival, gob-click no-walk | `MOVE PROBE: OK` |
| probe_direction.py | pose layers per movement octant | `DIRECTION WIRE: OK` |
| probe_animals.py | wildlife spawn/aggro wire | `ANIMALS WIRE: OK` |
| probe_melee.py / probe_pvp.py | PvP fight chain | `MELEE WIRE: OK` / PvP verdict |
| probe_drop.py | cross-node drop authority transfer | drop verdict |
| probe_plow.py | cross-node plow relay (TileMutation) | plow verdict |
| probe_guest_walk.py | wire client walks across peer-owned cells (needs a RUNNING 2-node cluster; guest evidence lives in the node logs) | `GUEST WALK: OK` |
| probe_station.py | station fuel/input/light/output | `STATION FLOW: OK` |
| test_build.py | build pipeline + persistence | `BUILD FLOW: OK` |
| test_farming.py | plow/plant/harvest | farming verdict |
| test_party_chat.py | party + chat relays | chat verdict |
| test_equip.py | equipment doll/world layers | equip verdict |
| test_newcraft.py | session-58 recipes end to end: saw, bucket-with-crafted-saw, fork paginae served | `NEWCRAFT: OK` |
| test_gather.py | world gathering: branch pick + boulder stone picks into inventory | `GATHER: OK` |
| test_feeding.py | trough build/load/lift/place/transfer (session 62) | `FEEDING FLOW: OK` |
| test_smelt.py | metal chain: ore deposit mine -> smelter build/fuel/light -> bar pickup (session 66) | `SMELT: OK` |
| test_kiln.py | clay chain: shore clay picks -> kiln build -> brick fired (session 70) | `KILN: OK` |
| test_bake.py | baking chain: saw+bucket crafts, wheat farm, bucket fill, quern grind, dough craft, oven bake (session 71; fast-crop isolated server) | `BAKE: OK` |
| load43.sh | 1000-bot duel cohort perf window | p95 histograms |

## Utilities

- `make-gameres.sh` - build the `gameres/` pack (jar extract + fork
  overlay) and run BOTH resource-version repair passes
  (`fix_gameres_versions.py`, `fix_gameres_parent_refs.py`) over the
  generated pack and the `res/compiled` overlay source; the Windows
  twin is `windows/make-gameres.ps1`, which does the same.
- `fix_gameres_parent_refs.py` - align every action-layer parent_ver
  with the parent resource's real file version (--check = report only,
  `--using DIR` resolves parent versions against a full pack when
  scanning a partial overlay like res/compiled). The legacy jar ships
  stale references (string.res -> clothmat ver 1 vs the real ver 3,
  tanhide.res -> leather ver 1 vs ver 2): a strict client requests the
  stale version over HTTP, rejects the served file, and MenuGrid
  throws PaginaException on world entry. Idempotent; exit 1 when
  anything remains stale.
- `scan_paginae.py` - static AButton decode of every
  paginae/craft/*.res (offline; needs `unzip -o -q lib/haven-res.jar
  'res/paginae/craft/*' -d /tmp/hx` first). Session-58 recipe
  inventory source of truth.
- `make_fork_paginae.py` - compose the fork craft paginae the pack
  lacks (string, tanhide) into `res/compiled/`; donor image layer +
  new AButton layer, layout verified by scan_paginae.py.
- `profile_guests.sh` - guest-scan profiling run (session-43 artifact;
  the O(guests) rescan cost it measured was removed in sessions 54/59
  (view-cell-bounded scan, packed guest pose batch) - kept for history.
- `profile_multinode.sh` - session-59 multi-node scaling harness:
  `MODE=single|cluster BOTS=<per node> ./profile_multinode.sh` boots
  the single-node baseline or a 2-node cluster (both nodes loaded) and
  prints per-node tick/phase/fan-out percentiles over a 60 s window.
  The gap #2/#7 profiling evidence came from this script.

## Attic

`attic/` holds the frozen one-off session gate scripts
(`verify_sessionNN.sh`, `verify_build.sh`, `verify_equip.sh`, ...).
They chain each other and assert historical wire behavior; nothing
runs them anymore and they are NOT maintained. Do not extend them:
write a probe on `hnhlib.py` (or a cargo wire test) instead.

## Adding a probe

Subclass `hnhlib.WireClient`, override `on_event(t, body)` (and
`on_objdata(body)` for extra gob-op tracking - call `parse_objdata`
first for your own event log, then `super().on_objdata(body)`), drive
the flow with the built-in action helpers
(`play`, `click_ground`, `click_gob`, `menu_act`, `send_place`,
`map_itemact`, `flower_choice`, `find_item_by_res`, ...). Do not
re-implement auth, the reliability walk, or the OBJDATA op table.
Shared contracts already tracked by `WireClient`: widgets + DSTWDG
bookkeeping, RESID map, gob state, item widgets, flower menus,
char sheet (`chr_id`/`exp_seen`/`attrs`), Area Chat (`chat_id`/
`chat_lines`), party roster (`pv_id`), OD_BUDDY names, cattr names.
