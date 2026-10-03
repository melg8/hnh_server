# HANDOFF — hnh_server Rust implementation

This file is the durable context-transfer protocol for LLM working sessions.
Read this FIRST in every new session. Append a dated entry at the end of
every session (what was done, what was verified, what is next). Never
delete entries.

## How to continue work in the next session

1. `git pull` the repo; read this file top to bottom.
2. Build and test: `cd server && cargo test` (must be green) and
   `cargo build --release`.
3. Run: `cd server && ./target/release/hnh-server --seed 42`
   (ports: 1871/tcp auth TLS, 1870/udp game, 1872/tcp resources HTTP;
   `../gameres/` must exist — it is generated from `lib/haven-res.jar`,
   see "Resource pack" below).
4. Verify end-to-end: `python3 server/scripts/test_client.py testuser`
   must print `WORLD ENTRY: OK`.
5. Load check: `./target/release/hnh-server --seed 42 --bots 300 --perf`
   — perf logs should show `tick_us` well below 100000.
6. Client: `ant jar` (JDK 21 + Ant 1.10), run
   `java -cp build/haven.jar:lib/* haven.MainFrame` — it connects to
   127.0.0.1 automatically (auth 1871, game 1870, resources 1872).
   `-Dhaven.autoplay=Player` skips charselect. `-Dhaven.pinnedcert`
   restores legacy certificate pinning.

## Architecture (as implemented)

- `server/crates/hnh-proto` — byte-exact wire protocol: `MessageBuf`
  (LE primitives, NUL strings, typed lists), MSG_*/RMSG_*/OD_* consts,
  reliability layer (`RelSender`/`RelReceiver`: 16-bit seq per direction,
  cumulative ACK, legacy backoff table 80/200/620/2000 ms, hold-back
  buffer), zlib MAPDATA assembly + MTU fragmentation. Auth frame codec.
- `server/crates/hnh-world` — `JavaRandom` (bit-exact 48-bit LCG port,
  verified against a real JDK 21), seed-fixed value-noise worldgen
  (tile_at is a pure function of seed+x+y; grids generate independently =
  grid-shard ready), `GridStore` (10 KB per 100x100 grid, LRU eviction,
  copy-on-write tile mutation).
- `server/crates/hnh-server` — binary:
  - `auth.rs`: TLS auth server (rustls, dev cert in `server/certs/`),
    AuthClient frame protocol, SHA-256 password digests, single-use
    cookies (5 min TTL), reusable tokens (30 d TTL). Dev policy: any
    username/password auto-provisions.
  - `net.rs`: UDP 1870. MSG_SESS handshake (PVER check, cookie consume,
    idempotent re-accept). Per-session driver tasks own reliability state;
    two outbound channels: reliable RMSG stream + raw MAPDATA/OBJDATA
    datagrams (matching the legacy split). MAPREQ/OBJACK/WDGMSG in.
  - `state.rs`: SoA gob storage (pos/res/frame/alive/kind/hp/speed/mv
    columns, generational ids packed into i32), Species table with
    per-species hp/speed/loot, tile speed rules, 10 Hz tick.
  - `game.rs`: single-owner game task. Bootstrap sequence per
    docs/mechanics/network/session-lifecycle.md (RESID before charlist,
    TILES before MAPDATA, HUD -> mapview -> GLOBLOB -> CATTR -> PAGINAE).
    Visibility manager (500 subtile radius, spawn/update/retract,
    ack-gated retransmit buffers per session). Movement (LINBEG/LINSTEP,
    per-tile speed, server-validated paths), wildlife AI (wander, flee,
    chase, attack), combat (auto-attack, damage = 5*str/10, HP quarters
    via OD_HEALTH, death -> loot drops + LP), vitals (hp/energy/stamina,
    starvation, respawn), inventory widgets (inv/item/xfer/drop), GLOBLOB
    (8 real hours per in-game day, 365-day year).
  - `res_http.rs`: HTTP file server for `gameres/` (GET `<name>.res`).
  - `bots.rs`: in-process load-test bots through the REAL UDP path.
  - `handoff.rs`: this file's appender.

## Resource pack

`gameres/` = `lib/haven-res.jar` (6301 .res files) overlaid with
`res/compiled/` (fork resources). Regenerate with:
```bash
unzip -o -q lib/haven-res.jar 'res/*' -d /tmp/hx && cp -rn /tmp/hx/res/* gameres/ \
  && cp -r res/compiled/* gameres/
```
`gameres/` is NOT in git (repo size); it is reproducible from the repo.

## Verified (session 2)

- Unit tests: protocol roundtrips, reliability in-order/out-of-order/dup,
  MAPDATA shape (reinflate == 10000 tiles + plots), fragmentation,
  JavaRandom == JDK 21 reference values, mkrandoom == JDK, worldgen
  determinism/variety, SHA-256 vectors, bootstrap RESID ordering,
  persistence roundtrip + seed-mismatch reset, frv uimsg encoding,
  predator engagement in saturated worlds. 25 tests green.
- Integration: `scripts/test_client.py` performs TLS auth -> MSG_SESS ->
  charlist -> play -> full bootstrap -> 9x MAPREQ -> MAPDATA (9 fragments)
  -> OBJDATA stream (player + trees + animals) -> walk click. Prints
  `WORLD ENTRY: OK`.
- Load (session 2): 1000/1000 async bot sessions in a saturated world
  (3300+ animals) with live fights; steady-state tick ~37 ms vs 100 ms
  budget (max 76 ms) at worst-case single-area crowding; RSS ~190 MB.
  Bots run as tokio tasks (thread-per-bot hit the 748-thread ulimit at
  547 sessions in session 1's design).
- Sharding: `--shards N` opens N SO_REUSEPORT UDP sockets; kernel
  4-tuple hash pins each peer to one shard; `WORLD ENTRY: OK` passes
  through a shard.

## Architecture changes (session 2)

- `net.rs`: free fn `spawn(game_tx, shards)` binds N UDP sockets with
  SO_REUSEPORT via socket2; each shard owns a private recv loop and
  session table; accept log carries `shard=` for occupancy histograms.
- `persist.rs`: `SaveStore` keeps per-character snapshots in
  `../save/world.json` (version 1, atomic tmp+rename writes). Snapshots
  update on session close + 30 s autosave; SIGINT/SIGTERM follow a
  graceful shutdown path (`Cmd::Shutdown`) that flushes the file. Seed
  mismatch or corrupt file starts fresh characters. `HNH_SAVE_FILE`
  env var overrides the path.
- `fight.rs`: Fightview (frv) openings combat. One frv widget per
  session created on first engagement (destroyed when relations empty).
  Per-opponent relations carry balance -5..+5, intensity, two-bit give,
  IP both sides, offence/defence bars scaled x100. uimsg: new/del/upd/
  updod/cur/atkc/offdef; client wdgmsg click/give handled. Damage lands
  only through openings (defence broken), scaled by str and advantage.
- `bots.rs`: fully async (tokio tasks, no threads). Bots spread over
  home tile areas near the spawn, request their 3x3 grids with raw
  MAPREQ datagrams, walk, and get engaged by predators under
  `--saturated` (aggro radius widened to 900 subtiles there).
- `auth.rs`: self-signed dev certificate generated with rcgen at boot
  when certs/authsrv.key.pem is absent (fresh-clone one-command start).
- `main.rs`: `--shards N` flag; graceful SIGTERM/SIGINT shutdown.

## Known gaps / next steps (priority order)

1. **Crafting**: paginae actions and the Makewindow flow
   (`act("craft", ...)` -> make widget -> `make 0/1`) are stubbed
   (`on_menu_action` logs); fep.conf/curio.conf parsing not done.
2. **Grid-owner partitioning**: UDP shards are done; the game task is
   still the single simulation owner. Next scale-out step is splitting
   visibility/AI into grid-owner tasks (SoA layout ready, see state.rs).
3. **Farming/livestock**: crop growth stages via sdt + OD_RES are
   designed (see encode_gob_block) but no planting flow yet.
4. **Combat polish**: frv relations stream balance/intensity only
   passively (no move selection UI: attack/maneuver resources are not
   settable yet); IP accrues per swing, no move costs.
5. **Headless client GL**: the client runs on a real display; under
   Xvfb JOGL 1.1 needs Linux natives that this repo does not ship
   (Windows .dll only; Debian headless JRE also lacks libawt_xawt --
   use a full Temurin JDK for client smoke tests). Wire-protocol
   correctness is covered by scripts/test_client.py.
6. **flavor objects**: client-side flavor replication needs the tileset
   flavobjs tables + randoom parity; server gobs already cover clickable
   objects, so this is cosmetic-only for now.
7. **Party/buffs/chat**: wire builders exist (`wdg::` helpers), gameplay
   wiring pending.

## Session log

---
### Session 1 (2026-10-03, UTC+8)
- Read AGENTS.md + all docs/mechanics; implemented the full server per
  "Architecture" above; verified via unit + integration tests.
- Client: trustAll SSL, localhost defaults, autoplay hook, jar builds
  with JDK 21 + Ant 1.10.15.
- Load: 724 bots / 19 ms tick. Numbers above.
- Commits: "Add Rust server...", "Remove build artifacts...",
  "Client: local dev server support".

---

## Session end 1791053403

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791053436

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791053595

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055108

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055108

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055198

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055265

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055376

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055525

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055637

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055726

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055845

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791055940

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791056136

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791056246

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.


---

## Session end 1791056388

- Server exited cleanly (seed 42).
- See HANDOFF.md top section for current state.

