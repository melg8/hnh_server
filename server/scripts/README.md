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
- `test_build.py` — build/station/persist scenario CLI; also re-exports
  the harness names for the probes that import them
  (probe_melee/pvp/drop/plow, test_equip, probe_station).

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
| load43.sh | 1000-bot duel cohort perf window | p95 histograms |

## Utilities

- `make-gameres.sh` - build the `gameres/` pack (jar extract + fork
  overlay); the Windows twin is `windows/make-gameres.ps1`, which
  additionally calls `fix_gameres_versions.py`.
- `dump_paginae.py` - dump the paginae the server pushes at login.
- `scan_paginae.py` - static AButton decode of every
  paginae/craft/*.res (offline; needs `unzip -o -q lib/haven-res.jar
  'res/paginae/craft/*' -d /tmp/hx` first). Session-58 recipe
  inventory source of truth.
- `make_fork_paginae.py` - compose the fork craft paginae the pack
  lacks (string, tanhide) into `res/compiled/`; donor image layer +
  new AButton layer, layout verified by scan_paginae.py.
- `profile_guests.sh` - guest-scan profiling run (session-43 artifact;
  the O(guests) rescan issue it measures is still open in HANDOFF.md).

## Attic

`attic/` holds the frozen one-off session gate scripts
(`verify_sessionNN.sh`, `verify_build.sh`, `verify_equip.sh`, ...).
They chain each other and assert historical wire behavior; nothing
runs them anymore and they are NOT maintained. Do not extend them:
write a probe on `hnhlib.py` (or a cargo wire test) instead.

## Adding a probe

Subclass `hnhlib.WireClient`, override `on_event(t, body)` for your
widgets, drive the flow with the built-in action helpers
(`play`, `click_ground`, `click_gob`, `menu_act`, `send_place`,
`map_itemact`, `flower_choice`, `find_item_by_res`, ...). Do not
re-implement auth, the reliability walk, or the OBJDATA op table.

## Legacy self-contained scripts

`probe_animals.py`, `probe_direction.py`, `dump_paginae.py`,
`test_farming.py`, `test_party_chat.py` still carry their own plumbing
pre-hnhlib; they work, but a wire change must be patched in each until
they migrate. Migrating one = delete its `auth_cookie`/`decode_objdata`
transport blocks, subclass `WireClient`, keep the verdict logic.
