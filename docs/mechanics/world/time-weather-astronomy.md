# Time, Weather and Astronomy

> **Sources:** src/haven/Glob.java (Glob.blob, GMSG_TIME, GMSG_ASTRO, GMSG_LIGHT, amblight, defix), src/haven/Astronomy.java (Astronomy, dt, mp, yt, hh, mm, day, night, phase table), src/haven/Cal.java (Cal.render, hbr, moon images, sun/moon dial math, tooltip), src/haven/Session.java (RMSG_GLOBLOB, OD_LUMIN, handlerel), src/haven/MapView.java (mask.amb, checkmappos, draw), src/haven/Lumin.java (Lumin, off, sz, str), src/haven/ILM.java (ljusboll, subrend, setenv), src/haven/OCache.java (lumin), src/haven/Message.java (color, int32, uint8), src/haven/Utils.java (int32d), src/ender/timer/Timer.java (SERVER_RATIO, server, local), src/haven/resutil/GrowingPlant.java, "The Universe of Haven" (official H&H forum thread 35295), official client doc page (legacy.havenandhearth.com/portal/doc-src)

## Summary

The legacy client receives the entire world-clock state from the server in one periodic control message, `RMSG_GLOBLOB` ("GLOBLOB blob"), parsed by `Glob.blob` (src/haven/Glob.java lines 108-128). The blob carries up to three segment types in this fork:

- `GMSG_TIME` (0): an absolute server wall-clock snapshot as one little-endian `int32` unix-seconds value.
- `GMSG_ASTRO` (1): three fixed-point doubles scaled by 1e9 (`Glob.defix`, line 104): `dt` (fraction of the in-game day, 0..1), `mp` (moon-phase fraction within the lunar cycle, 0..1), `yt` (fraction of the in-game year, 0..1). The client derives hour/minute, day-of-year and night flag from these (`Astronomy` constructor, src/haven/Astronomy.java lines 58-66).
- `GMSG_LIGHT` (2): one RGBA color (`Message.color()`) - the ambient light tint applied to the whole scene (`Glob.amblight`, consumed by src/haven/MapView.java line 1968).

The in-game year is 365 days (`day = (int)(365 * yt)`), day and night are hard-cut at `dt = 0.25` and `dt = 0.75` (06:00 and 18:00 in-game), and the fork's timer utility hard-codes a 3x time ratio (`SERVER_RATIO = 3`, src/ender/timer/Timer.java line 10), i.e. one in-game day (24 in-game hours) passes in 8 real hours. There is **no weather model of any kind** in this client: no rain, cloud, wind or drought state exists in the protocol, the code, or the shipped resources. Ambient light color plus per-object light sources (`Lumin` gobs rendered through the light-mask pass `ILM`) is the entire "sky" simulation.

A Rust server must: own the calendar (day fraction, moon phase, year fraction, ambient light), push `GLOBLOB` snapshots on a fixed cadence, and drive all season- and time-dependent gameplay (crop growth, curio study, animal behavior) server-side, since the client performs no interpolation and no local simulation of any of it.

## The GLOBLOB blob: wire format and parsing

Dispatch: server relmsg type `RMSG_GLOBLOB = 4` (src/haven/Message.java line 37) arrives in `Session.handlerel` (src/haven/Session.java line 348) and calls `Glob.blob(msg)`. Parsing loops `while (!msg.eom())` over `uint8` segment ids (src/haven/Glob.java lines 108-128):

- Segment `GMSG_TIME = 0`: `Timer.server = msg.int32(); Timer.local = System.currentTimeMillis() / 1000;` - the server's unix time (seconds) at send, paired with the client's own unix time at receive. The only consumer is the fork's Ender timer suite, which uses the pair as a sync anchor (`start = server + SERVER_RATIO * (now - local)`, src/ender/timer/Timer.java lines 45-55). **The int32 is interpreted as a plain unix timestamp** - so the server should send `unix_seconds_as_int32` and should treat 2038 (int32 overflow) as a non-issue for a legacy fork.
- Segment `GMSG_ASTRO = 1`: three `int32` values, each divided by 1e9 by `Glob.defix`:
  - `dt`: fraction of the current in-game day. `0.0` = midnight, `0.25` = 06:00, `0.5` = noon, `0.75` = 18:00.
  - `mp`: moon-phase fraction, 0..1, wrapping over one lunar cycle. Phase index = `floor(mp * 8)` (src/haven/Cal.java line 64).
  - `yt`: year fraction, 0..1. Day of year = `floor(yt * 365)`.
  - `night = (dt < 0.25) || (dt > 0.75)` - computed client-side from `dt` (src/haven/Glob.java line 120), so the server never sends a night bit.
- Segment `GMSG_LIGHT = 2`: `amblight = msg.color()` - four `uint8` (r, g, b, a), see `Message.color` (src/haven/Message.java lines 198-200).
- There is **no default case**: an unknown segment id is consumed silently, but the parser has no way to skip its payload, so the next payload byte would be misread as the following segment id and the whole blob desynchronizes. A server targeting this exact client must send only these three segment types, in any order, repeated as needed.

Note the precision contract: fixed-point 1e9 means one transmitted step of `dt` corresponds to about 86 microseconds of game time (86400 s / 1e9), far finer than any display needs. The client redraws the calendar widget only when the new `Astronomy` differs from the old (`Astronomy.equals` compares `dt`, `mp`, `yt`, `night` - src/haven/Astronomy.java lines 43-56; src/haven/Cal.java lines 82-89), so sending identical values repeatedly is a cheap no-op for clients.

## In-game calendar

Derived entirely client-side from `dt`/`yt` (src/haven/Astronomy.java lines 62-64):

- `hh = (int)(24 * dt)` - in-game hour 0..23.
- `mm = (int)(60 * (24 * dt - hh))` - in-game minute 0..59.
- `day = (int)(365 * yt)` - day of year 0..364. The calendar widget displays `String.format("Day %d,   %02d:%02d\nMoon: %s", day, hh, mm, phase)` (src/haven/Cal.java line 65).

Structural facts a server must honor:

- **A year is 365 in-game days.** There is no month structure, no leap day, and no year number anywhere in this client: `yt` wraps and the display resets to "Day 0". If the Rust server keeps an absolute year counter (it should, for persistence and lore), it is invisible to this client and free to define.
- **The client never advances time locally.** Between GLOBLOB arrivals the displayed clock is frozen; there is no interpolation, extrapolation or tick. The calendar only re-renders on a changed snapshot. The server should therefore push `GLOBLOB` on a steady cadence (the official cadence is unknown; a few times per in-game hour is a reasonable default - see Open questions).
- **Time ratio**: the only in-repo evidence for game-time speed is `SERVER_RATIO = 3` in the fork's timer (game clock advances 3 seconds per real second), implying a 24 h in-game day per 8 real hours and a 365-day year per 2920 real hours (about 121.7 real days). This matches community knowledge of official servers, but the ratio itself is a server config value - the protocol carries absolute fractions, so any ratio works with this client.
- Epoch/anchors: the protocol has no calendar epoch - only fractions and a unix wall-clock snapshot. The server picks the world's birthday; clients cannot tell.

## Day, night and darkness

- Night flag: `dt < 0.25 || dt > 0.75` (18:00 to 06:00 in-game). Everything else is day. There is no dawn/dusk gradient in the flag, but the ambient color can be varied continuously by the server to fake one.
- Ambient light: `Glob.amblight` (src/haven/Glob.java line 48) is applied every frame as the modulation color of the light-mask pass: `mask.amb = glob.amblight`, with `Config.nightvision` forcing a transparent (fully lit) mask (src/haven/MapView.java lines 1968-1969). The fork ships nightvision as a client toggle - meaning **a cheating client can ignore server darkness entirely**; night-danger mechanics must not rely on client rendering.
- The mask pass (`ILM`, src/haven/ILM.java) works like this: render a white texture; for every gob carrying a `Lumin` attribute, stamp a radial "light ball" sprite (`ljusboll`, 200 px, fully lit inner radius 50, linear falloff to opaque gray at the rim, halved gradient, src/haven/ILM.java lines 39-62) at `gob.sc + lum.off - (lum.sz, lum.sz)` sized `2*sz x 2*sz` (lines 83-96); the texture is then modulated with `amb` and composited over the scene. Bright `amb` = day; dark `amb` = night; light balls carve visibility around light sources.
- Light sources: any gob can be a light. The server attaches `Lumin(off: Coord, sz: int, str: int)` via object data `OD_LUMIN = 8` (src/haven/Session.java lines 53, 251-253): payload is `Coord off` (two `int32`; consumed in screen pixels - `ILM` adds it to the gob's screen position `gob.sc`, src/haven/ILM.java lines 89-94), `uint16 sz` (light radius in pixels - the mask ball is `2*sz` wide), `uint8 str` (stored on `Lumin`, unused by this fork's `ILM` renderer; upstream clients presumably use it as intensity). Typical sources: bonfires, torch posts, lamps, kilns while burning. The server decides which gobs get `Lumin` and when (e.g. only while lit), and should remove/refresh the attribute like any other gob attribute when the source changes state.

## Astronomy: sun, moon and phases

What the client renders in the calendar widget (src/haven/Cal.java lines 58-75):

- A clock dial of radius `hbr = 23` px. The sun sprite is drawn at angle `(dt + 0.75) * 2*PI` and the moon sprite at `(dt + 0.25) * 2*PI` on the dial (using `Coord.sc(a, r) = (cos(a)*r, -sin(a)*r)`, src/haven/Coord.java line 68, so the dial's y axis points down). Worked positions: the sun sits at the east point of the dial at 06:00 (`dt = 0.25`), reaches the top at noon (`dt = 0.5`) and the bottom at 18:00/midnight; the moon mirrors it, topping the dial at midnight (`dt = 0`) while the sun is at the bottom. Sun and moon are always exactly half a turn apart.
- Eight moon images `gfx/hud/calendar/m00`..`m07`; phase index `mp_i = (int)(a.mp * 8)`, truncated (so `mp` in [i/8, (i+1)/8) shows phase i). Phase names in display order (src/haven/Astronomy.java lines 34-41): New Moon, Waxing Crescent, First Quarter, Waxing Gibbous, Full Moon, Waning Gibbous, Last Quarter, Waning Crescent. New Moon begins at `mp = 0`.
- There is no star field in this fork: `Astronomy` carries no star data and the renderer draws none. (Later upstream clients added star rendering; a Rust server targeting this client needs nothing beyond `dt`/`mp`/`yt`.)
- Lunar cycle length: not encoded. The server advances `mp` at whatever rate makes one full cycle span its chosen number of days; the client only ever sees snapshots. Community sources associate full moons with specific in-game events, but the period itself is server configuration (see Open questions).
- Gameplay coupling: moon phase and night affect gameplay only where the server makes it (e.g. dream catchers, night-aggressive wildlife on some servers). Nothing in this client enforces or displays such rules beyond the calendar widget.

## Weather

There is no weather in this client, at any layer:

- `Grep` over `src/haven/**` finds no occurrences of rain, cloud, weather, storm, wind or drought symbols; the `GLOBLOB` parser accepts only the three segments listed above; the shipped resource trees (`res/raw/res`, `res/compiled`) contain no weather/rain/cloud resources.
- The closest mechanisms are: (a) `GMSG_LIGHT` ambient tint - fog/dusk/gloom can be faked by tinting the scene; (b) sound effects via `RMSG_SFX` (src/haven/Session.java line 361) - a server could play rain loops; (c) tile/object state (dried fields, puddles as gobs).
- Gameplay lore (droughts harming crops, rain watering fields) is therefore a *pure server-side* concern in this fork: implement weather as server state with optional ambience (amb light tint + sfx), and surface season/weather effects only through game rules (crop growth multipliers, `../livestock/farming-and-plants.md`) since no weather HUD exists.
- If weather display is desired without client changes, the pragmatic channel is buffs (`RMSG_BUFF`, src/haven/Glob.java `buffmsg`) - a "Raining" buff icon pushed to all players in the affected area.

## Seasons and growth

- The only seasonal signal in the protocol is `yt` (year fraction). This fork's client uses it solely for the day-of-year display; there are no season names, tile-color changes, or temperature effects client-side.
- All season-dependent gameplay is server rules: crop planting windows and growth speed (cross-link `../livestock/farming-and-plants.md`), plant stage advancement timers (growth stages are sent as the first byte of plant sprite dynamic data, `sdt.uint8()`, src/haven/resutil/GrowingPlant.java), animal breeding/feed tables, wild-respawn rates.
- Recommendation for the Rust server: define seasons as fixed `yt` ranges (e.g. four quarters of the 365-day year, or the community's 3-month convention), drive growth multipliers from them, and keep `yt` monotonic. Since clients freeze between snapshots, also re-send plant stage updates rather than relying on any client-side clock.

## Time sync semantics for the server

- The server is authoritative for both the wall clock (`GMSG_TIME`) and the calendar (`GMSG_ASTRO`). The client keeps a single snapshot (`Glob.ast`, `Timer.server`/`Timer.local`) and performs **no** dead reckoning; correctness between snapshots is the server's cadence choice.
- `GMSG_TIME` is what the fork's timers use to convert real elapsed time into game time (`SERVER_RATIO * real_elapsed`), and what player-facing timer paginae (item study, craft timers) assume. Send it as unix seconds at send time, and keep the game clock a fixed multiple of real time so client-side conversions stay valid.
- On login, send a full `GLOBLOB` (all three segments) before or with the first map/object data, so night rendering and the calendar are correct from frame one; then refresh on a timer. The message is tiny and idempotent; clients re-render only on change.
- If the server pauses (snapshot restores, maintenance), a large `dt` jump will simply snap the client's clock and lighting - acceptable with this client; announce via chat if needed.

## Server implementation notes

1. World clock: keep `game_time_ms` as a monotonic integer; derive `dt = (game_time_of_day / day_length) mod 1`, `yt = (day_of_year / 365)`, `mp = (elapsed_in_lunar_cycle / cycle_length) mod 1` with `day_length`, `lunar_cycle`, and the game/real ratio as config (legacy defaults to consider: 3x ratio, 365-day year).
2. Fixed-point encoding: `segment value = round(fraction * 1e9) as i32` - little-endian on the wire (src/haven/Utils.java `int32d`); clamp to [0, 999999999].
3. `GLOBLOB` cadence: full snapshot on login; then every 2-5 s of real time (cheap; identical values are no-ops for clients). Ensure the parser invariant: only segment ids 0/1/2, each immediately followed by its exact payload.
4. Ambient light policy: pick `amblight` from a small keyframe table over `dt` (bright day -> dark night with dawn/dusk ramps) and multiply by weather if you add any. Remember nightvision clients bypass darkness: put actual gameplay checks (visibility, aggro range, sleep) server-side.
5. Lights: attach `Lumin` attributes to light-source gobs (payload `off: (i32, i32)`, `sz: u16`, `str: u8`); tune `sz` against the 200 px ball texture (ball covers `2*sz` screen px); re-send on state change (lit/extinguished). `str` is accepted but ignored by this fork - set it anyway for forward compatibility.
6. Seasons: define `yt` ranges per season; expose `season(now)` to farming/animal systems; advance plant stages by server timers with season multipliers.
7. Moon: advance `mp` continuously; if any server rule keys on phase, compute it from `mp` (phase index = `floor(mp*8)`), never from day parity.
8. Persistence: store absolute game time (e.g. ms since epoch plus year counter) so restarts do not rewind the calendar; `GMSG_TIME` snapshots should follow real unix time to keep the fork's timers sane.

## Open questions

- Official `GLOBLOB` cadence and burst behavior (per tick? every N seconds? on change only?). Determine by logging relmsg type 4 timestamps against a live legacy server.
- Lunar cycle length on official servers (days per full `mp` cycle) and the canonical phase anchor (`mp = 0` at year start?). Determine by recording `GMSG_ASTRO` snapshots over several real hours and fitting the slope of `mp`.
- The exact official time ratio (3x is inferred from the fork timer, `src/ender/timer/Timer.java` SERVER_RATIO) and whether the official server ever deviated (variable day length events). Determine the same way from the slope of `dt`.
- Whether official servers ever used further `GLOBLOB` segments (later upstream clients know more segment ids, e.g. for weather and placement). For this fork the parser silently desyncs on unknown ids, so decode traffic before reusing any id.
- The intended meaning of `Lumin.str` (intensity? flicker seed?) - unused by this fork's `ILM`; decide its semantics for the Rust server or leave it constant.
- Whether any legacy-era weather existed server-side (drought/rain events affecting crops) despite no client support: check patch notes and forum archives; the client gives no hook beyond ambience.
- Season boundary conventions in the legacy community (equal quarters of `yt` vs. named months) for use in the Rust server's season table; affects only rule tuning, not the protocol.
