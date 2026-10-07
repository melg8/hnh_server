# Learning Points and Curiosity (Legacy Study System)

> **Sources:** src/haven/CharWnd.java, src/haven/Item.java, src/haven/Config.java, src/haven/Glob.java, src/haven/Session.java, src/haven/UI.java, src/haven/MenuGrid.java, src/haven/Resource.java, etc/needed/curio.conf, etc/needed/fep.conf, res/compiled/paginae/add/study.res, Ring of Brodgar wiki (Legacy:Learning_Points, Legacy:Skills, Curiosity)

## Summary

Learning Points (LP) are the currency of character progression on legacy Haven and Hearth servers (World 5 era, after the March 2011 rework that replaced the older grind-based experience system). This document is the authoritative server-side blueprint for everything LP-related:

- **Earning LP**: studying curiosity items ("curios") in the Study window over real time, plus small one-time awards for first-time acquisition of new object types (first pickup, first craft, first use as an ingredient).
- **Spending LP**: raising the 11 "incrementable" skill values in the Character Sheet (client-side predicted cost curve), buying non-incrementable skills from the server-defined skill catalog, and (per legacy wiki) property.
- **Attention budget**: the total attention cost (AT) of curios currently under study may not exceed the character's effective Intelligence; the client renders this as `attused/attlimit` in the Study tab.
- **Time-based study**: each curio has a study time in real hours (`TIME` in `etc/needed/curio.conf`); the server must advance progress on server-side timers, push per-item progress meters, and grant LP on completion.

Key client-side anchors, all verified in this repository:

- `src/haven/CharWnd.java` - the whole character sheet: `Study` inner widget, `SkillList` (available/current skills), `SAttr` (buyable skill values with cost prediction), `NAttr`, the `exp` (LP balance) and `studynum` (attention used) UI messages, and the `chr` widget factory with the optional study widget id argument.
- `src/haven/Item.java` - `curio_stat`, `qmult` (quality multiplier), `meter` (study progress 0-100), and the curio tooltip (`LP: ... in <time left> Att: ...`).
- `src/haven/Config.java` - `CuriosityStat` and `loadCurio()`, the parser for `etc/needed/curio.conf` (`Name=LP:<gain> AT:<attention> TIME:<hours>`).
- `src/haven/Glob.java` - `cattr` map (server-pushed attributes, including `intel` and `expmod`), `paginae.add(Resource.load("paginae/add/study"))`.
- `src/haven/MenuGrid.java` - `usecustom()` dispatches the `study` command to toggle the Study window.
- `res/compiled/paginae/add/study.res` - the "Study" action pagina resource (strings `Study`, `study`, parent `paginae/act/add`).

## The learning point economy

### What LP are

LP are a per-character integer balance. The client learns the balance from the server through the `chr` widget UI message `exp` (`CharWnd.uimsg`, case `"exp"`, `CharWnd.java:1002-1005`), and renders it in the Attributes tab next to the label "Learning Points:" (`explbl`, `CharWnd.java:802-803`). Despite the field name `exp` (a holdover from the pre-2011 experience system), it is the LP wallet: the client compares pending purchase costs against it (`updexp()`, `CharWnd.java:216-228`) and colors unaffordable entries red.

Legacy wiki (Ring of Brodgar, Legacy:Learning_Points) states LP are used to buy:

1. Incrementable skills (the 11 skill values, see below),
2. Non-incrementable skills (the "Available Skills" list in the Skills tab),
3. Property (claim/land related; mechanism not visible in this client - see Open questions).

### How LP are earned

1. **Curiosity study** - the main source. A curio placed in a study slot yields its LP when study completes (see the Study system section). The client predicts the pending gain in `CharWnd.Study.updateStudyLp()` (`CharWnd.java:732-748`):

   ```
   studylp += Math.round(it.curio_stat.baseLP * it.qmult * UI.instance.wnd_char.getExpMode())
   ```

   i.e. `LP_gain = round(baseLP * qmult * expmod/100)` summed over all curios currently in study slots.

2. **First-time acquisition LP** - per legacy wiki (Legacy:Learning_Points): the first contact with a new object type awards LP (picking your first branch off a tree yes, off the ground no; crafting certain objects such as Wooden Cutlery awards LP; symbel items all award LP; any item used to craft another item provides "acquisition LP"). Crucially, "items your ancestors have discovered won't give you Learning Points" - the discovered-set is tied to the family line, not just the character. The amounts and the exact trigger rules are not in the client; the server must own them (see Open questions).

3. No other LP income is evidenced in the client. The `expmod` attribute ("Learning Ability", see below) scales gains, it does not add to them.

### How LP are spent

**Raising skill values (incrementable skills).** The Attributes tab lists 11 "Skill Values" with +/- buttons (`CharWnd.skillval()`, `CharWnd.java:832-844`), built as `SAttr` widgets (`CharWnd.java:230-345`). Each click increments the target value and adds the client-side predicted cost; the "Buy" button sends a `sattr` widget message to the server (`buysattrs()`, `CharWnd.java:751-761`) carrying `(name, targetBaseValue)` pairs for every `SAttr`.

The client-predicted cost curve (`SAttr.inc()`/`SAttr.dec()`, `CharWnd.java:293-336`):

- One point: raising a skill value from `v` to `v+1` costs `100 * (v+1)` LP (`cost += tvalb * 100` after the increment).
- Bulk (ctrl-click to max affordable): closed form `cost = 50 * (k + n) * n` with `k = 2*base + 1`, which is exactly the arithmetic series `sum_{i=1..n} 100*(base+i)`.
- Shift-click steps 10 points using the same per-point series.

The server must charge exactly this cumulative amount and reject `sattr` messages that overreach the LP balance or skip intermediate values (the client prediction is advisory; the server is authoritative).

**Buying non-incrementable skills.** The Skills tab has two lists (`CharWnd.java:846-869`):

- `nsk` - "Available Skills": populated by the server through the `chr` UI message `nsk` (`CharWnd.java:1011-1022`) as `(skillName, cost)` pairs; each name is loaded as `Resource.load("gfx/hud/skills/" + name)`. Selecting an entry shows `Cost: <n>` and the skill's description text, which the client takes from the resource's `tooltip` layer (title) and `pagina` layer (body) in `SkillInfo.draw()` (`CharWnd.java:479-490`).
- `psk` - "Current Skills": populated by the `psk` message with names only (`CharWnd.java:1023-1030`).

The "Learn" button sends `wdgmsg("buy", <skill basename>)` (`buyskill()`, `CharWnd.java:763-766`), where the basename is the last path component of `gfx/hud/skills/<name>` (see `Resource.basename()`, `src/haven/Resource.java:323-328`). The server must resolve that basename to a skill definition, charge the LP cost it advertised in `nsk`, add the skill to the character, and re-push `nsk`/`psk` and the new `exp` balance.

The client ships **no** local skill catalog (there is no `gfx/hud/skills/` tree in `res/`), so the list, the costs, and any prerequisite ordering are entirely server-defined data. The 11 incrementable skill values are the only skill names the client hardcodes.

**Property.** Legacy wiki lists property as an LP sink; the legacy client shows no dedicated UI for it here. See Open questions.

### Learning Ability (`expmod`)

The Attributes tab shows a "Learning Ability:" percentage bound to the server-pushed character attribute `expmod` (`CharWnd.java:804-815`). It is a `Glob.CAttr` (base + compiled values streamed by the server, `src/haven/Glob.java:80-102` and `147-162`, delivered as `RMSG_CATTR` in `src/haven/Session.java:366-367`). Every LP-gain display divides it by 100 (`CharWnd.getExpMode()`, `CharWnd.java:1101-1107`), so the server is expected to scale all curiosity LP gains by `expmod_comp / 100`. What modifies `expmod` on legacy servers (beliefs, food, energy, world state) is not visible in the client; see Open questions.

## The curiosity (study) system

### Study window wiring

- The server creates the character sheet as widget type `chr` (factory registered in the `CharWnd` static block, `CharWnd.java:65-74`). Its first creation argument is optional: an integer widget id. If present, the client binds its internal `Study` widget to that server widget id via `ui.bind(study, studyid)` (`CharWnd.java:893-895`, `src/haven/UI.java:147-150`) and shows the "Study" tab button (`CharWnd.java:908-919`). **If the server never creates/binds the study widget, the Study tab does not exist** - the server must create it.
- The `Study` widget (`CharWnd.java:646-749`) is a 400x295 panel with two labels: "Attention:" showing `attused/attlimit`, and "Study LP:" showing the total predicted LP of everything currently slotted. The actual study slots are **server-created** `Inventory` child widgets containing server-created `Item` widgets; `updateStudyLp()` simply walks all child `Inventory` widgets and sums curio gains. The number and geometry of slots is therefore server data (see Open questions).
- Items inside the study inventories are server-bound widgets, so normal item drag messages (`take`, `drop`, `transfer`) flow to the server; `CharWnd.wdgmsg` deliberately swallows messages from any `Item`/`Inventory` sender that is *not* server-bound (`CharWnd.java:1078-1092`). The server must implement slot insert/remove as ordinary inventory operations against the study inventories.
- The pagina `paginae/add/study` (resource present at `res/compiled/paginae/add/study.res`, action name "Study", activation command `study`) toggles the floating Study window; `MenuGrid.usecustom()` routes the `study` command to `ui.wnd_study.toggle()` (`MenuGrid.java:449-452`). `Glob` also preloads the pagina locally (`Glob.java:66`); official servers additionally push paginas with `RMSG_PAGINAE` (`Glob.paginae`, `Glob.java:130-145`).
- Server-to-client messages the Study widget understands: `studynum` with one integer (total attention currently used); the client then refreshes the attention label and the "Study LP" sum (`CharWnd.java:1006-1008`).

### Attention budget

- The attention limit is the character's **effective (compiled) Intelligence**: the Study widget initializes `attlimit = ui.sess.glob.cattr.get("intel").comp` (`CharWnd.java:661`) and refreshes it every time the `intel` CAttr updates (`NAttr.update()`, `CharWnd.java:210-212`). Buffs/debuffs to INT therefore change the attention pool live.
- The used attention is server-pushed via `studynum`. The client does no local attention arithmetic beyond display; **all budget enforcement is server-side**.
- Ring of Brodgar (Curiosity page) confirms the design: "The sum of the Mental Weight of your curiosities cannot exceed your Attention Limit, which is equal to your modified Intelligence", and you cannot study more than one curiosity of each type at the same time (restudying the same type later is allowed). The server must enforce both rules at insert time: (a) `sum(AT of slotted curios) + AT(new) <= intel_comp`, (b) no duplicate curio display-name among slotted items.

### Curio data: etc/needed/curio.conf

`Config.loadCurio()` (`Config.java:214-245`) reads `curio.conf` line by line as UTF-8 (the shipped file starts with a UTF-8 BOM; two display names use non-ASCII letters - the German spelling of Edelweiss with a sharp s, and Volva's Wand with a slashed o - so treat the file as UTF-8 everywhere). Format:

```
<Display Name>=LP:<float gain> AT:<int attention> TIME:<float hours>
```

Parsed into `Config.CuriosityStat { double baseLP; int studyTime; int attention; }` where `studyTime = TIME_hours * 60` (minutes; `Config.java:202-212`). Lookup is by the item's **display name**: `Item.name()` returns the resource's `tooltip` layer text (`Item.java:265-273`), and the curio table is consulted with that exact string (`Item.java:414-416`). The server's authoritative table must therefore be keyed by the exact display names of the curio resources, matching the client file.

The shipped `etc/needed/curio.conf` has 67 entries and is the legacy reference extract; the server should load the same table (or a superset). Lines tolerate trailing whitespace (the parser splits on whitespace around the `key:value` tokens). Sample rows (5 of 67; see the file for the full table):

```
Ant Empress=LP:2000 AT:6 TIME:3.33
Ant Queen=LP:500 AT:3 TIME:1.33
Ant Soldiers=LP:200 AT:2 TIME:0.67
Bear Tooth=LP:750 AT:25 TIME:28
Emerald Dragonfly=LP:800 AT:2 TIME:4
```

Derived per-hour efficiency (simple arithmetic on the rows above, not server constants): Emerald Dragonfly 200 LP/h at AT 2; Ant Queen ~375 LP/h at AT 3; Bear Tooth ~27 LP/h at AT 25. Long-time/high-AT curios are LP-slow but attention-cheap per point of gain; cheap low-AT curios fill leftover attention. (For comparison, `etc/needed/fep.conf` is the adjacent food table - same `Name=` keying, different payload, e.g. `Ant Empress=DEX:5 PSY:4`, `Ant Larvae=AGI:1`, `Bear Meat=HHP:1` - it governs FEPs, not LP; see docs on food for that file.)

### Quality multiplier

`Item` computes `qmult = Math.sqrt((double) q / 10)` from the quality value the server assigns to the item widget (`Item.java:412`). Consequences the server must mirror:

- `qmult` is 1.0 at quality 10, 2.0 at quality 40, 3.0 at quality 90 - LP gain grows with the **square root** of quality.
- Items with quality 0 (or unqualified items) fail the `qmult > 0` guard (`Item.java:337-345`; q=0 gives qmult 0, negative quality gives NaN), so the client suppresses the LP display entirely; the server should treat them as worthless for study.
- The tooltip prediction is `Math.round(baseLP * qmult * expmode)` (`Item.java:339`), identical to the Study tab sum. The server's grant formula must match this rounding exactly, or players will see predicted LP that never arrive.
- Note: the modern (Hafen-era) wiki describes a linear `BaseLP * Quality/10` formula; this legacy client clearly encodes `sqrt(q/10)`. For a legacy server, implement `sqrt(q/10)` so display and reality agree (flagged in Open questions).

### Study progress over time

- Progress arrives per item as the `meter` UI message: an integer 0-100 (`Item.java:489-493`). The client draws it as a fill bar on the item icon (`Item.java:204-214`) and appends `(meter%)` to the tooltip (`Item.java:331-333`).
- While `meter > 0`, the curio tooltip estimates remaining time as `min2hours(studyTime - (int)(studyTime * meter/100))` in minutes (`Item.java:340-343`), with `studyTime` in minutes from `curio.conf`. This confirms progress is linear in time over the full `TIME` window.
- The client has no local study timer; **the server owns time**. Recommended model: each slotted curio stores `startedAt` (server clock) and the stat snapshot; `progress% = floor(100 * elapsed / (TIME*3600s))`; push `meter` updates on a modest cadence and on every change of interest (exact legacy cadence unknown, see Open questions).
- On completion: grant `round(baseLP * sqrt(q/10) * expmod/100)` LP, remove/consume the curio from the study slot, push the updated inventory, the new `exp` balance, and `studynum`. Legacy anecdotes ("after 3 days I got theft", Legacy:Learning_Points) show multi-real-day study is normal, which implies study must keep progressing while the character is offline - i.e. pure server-side wall-clock progression, recomputed lazily on login if preferred.
- Multiple curios study **in parallel** (the attention budget, not slot count, is the aggregate constraint in the client model; each curio runs its own timer).

## Skills: incrementable vs non-incrementable

The legacy Character Sheet hardcodes exactly 11 incrementable skill values (`skillval()` calls, `CharWnd.java:834-844`), which map 1:1 to the Ring of Brodgar Legacy:Skills "Incrementable Skills" list:

| Client id (`gfx/hud/charsh/<id>` icon) | Display name | Legacy effect summary (from Legacy:Skills) |
|---|---|---|
| `unarmed` | Unarmed Combat | Effectiveness of unarmed moves (punches, dodging) |
| `melee` | Melee Combat | Effectiveness of weapon moves; shield block chance |
| `ranged` | Marksmanship | Aim speed with sling/bows; softcap on bow crafting |
| `explore` | Exploration | With Perception: herb/item find rates; scent tracking strength (PER*Exploration) |
| `stealth` | Stealth | Visibility and quantity of crime scents you leave (INT*Stealth) |
| `sewing` | Sewing | Quality softcap for leather/silk crafts (sqrt(DEX*Sewing)) |
| `smithing` | Smithing | Quality softcap for smithing (sqrt(STR*Smithing)); jewelry (sqrt(PSY*Smithing)) |
| `carpentry` | Carpentry | Quality softcap for wooden items/boards |
| `cooking` | Cooking | Quality softcap for non-oven cooking recipes |
| `farming` | Farming | Harvested seed quality (+-5 randomization); growth cap at skill+3 |
| `survive` | Survival | Herb quality hardcap; butchery yields (meat/bone/hide); ground-resource hardcap |

These are the values buyable with LP through `SAttr` (cost curve above). Structurally, each skill value is itself a server-pushed `Glob.CAttr`: `SAttr extends NAttr`, and `NAttr` subscribes to `ui.sess.glob.cattr.get(nm)` (`CharWnd.java:76-97`, `174-186`), so the server streams skill values named `unarmed`, `melee`, `ranged`, `explore`, `stealth`, `sewing`, `smithing`, `carpentry`, `cooking`, `farming`, `survive` over the same CATTR channel as the eight base attributes. Legacy community sources classify them as "incrementable" - i.e. they can also improve through use; such raises would arrive as ordinary CATTR updates, so the use-to-improvement rules are server-side. The exact legacy formulas for raises-through-use are not recoverable from this client (see Open questions).

Non-incrementable skills (theft, coinage, and the rest of the legacy tree) exist only as server data rendered through `nsk`/`psk` and bought with `buy`. Their resource names follow `gfx/hud/skills/<name>` and each must carry a `tooltip` layer (name) plus a `pagina` layer (description), since `SkillInfo` renders exactly those two layers.

### Per-attribute coupling

The client shows attributes with a base and a compiled value (`Glob.CAttr` has `base` and `comp`; the Attributes tab shows the delta in green/red, see `NAttr.update()`, `CharWnd.java:188-213`). Skills modulate *effective* attributes through the compiled value (e.g. the crafting softcaps above are computed from skill values and attributes like DEX/STR/PSY), and combat/exploration formulas combine them multiplicatively (PER*Exploration, INT*Stealth). The server computes all of this; the client only displays `base` vs `comp`. There is no client-visible table of "skill X gives +N attribute points"; any fixed per-skill attribute bonuses would be server data (see Open questions).

## Server implementation notes

The implementing server must model, persist, and expose:

1. **LP balance per character** (integer). Push via the `chr` widget message `exp` on every change (buy, study completion, acquisition award). This is the single wallet for skill values, skill purchases, and any property charges.
2. **`expmod` CAttr** ("Learning Ability", percent) streamed like every other attribute (name string + int32 base + int32 comp on the CATTR message). The client treats 100 as neutral (colors below 100 as debuff, above 100 as buff, `CharWnd.java:806-814`), so default to 100. Scale all study gains by `comp/100`.
3. **Attributes** (`str`, `agil`, `intel`, `cons`, `perc`, `csm`, `dxt`, `psy`) with base and compiled values; `intel.comp` is the attention limit. Attributes arrive through the same CATTR stream (see the attributes doc).
4. **The 11 skill values** as server-side integers per character, accepting `sattr` (list of name/target pairs) with cost `sum_{i=1..n} 100*(current+i)` per skill, charged atomically; reject insufficient balances and non-contiguous targets. After applying, push updated effective values and the new `exp`.
5. **Non-incremental skill catalog** (server data): resource name under `gfx/hud/skills/`, LP cost, description (tooltip/pagina layers in the resource), prerequisites if any. Handle `buy` messages: validate availability, charge, add to the character's learned set, re-push `nsk` (name/cost pairs) and `psk` (names).
6. **Curio table**: server-side equivalent of `etc/needed/curio.conf`, keyed by resource display (tooltip) name: `baseLP` (double), `attention` (int), `studySeconds` (TIME hours * 3600). Keep the values byte-compatible with the shipped file for authenticity; treat unknown names as non-curios.
7. **Study state per character**: a list of study entries `{slot, item, quality, startedAt/elapsed}` living in the server-created study inventories (children of the bound study widget id, passed as the `chr` creation argument). Enforce at insert: curio recognized, `qmult > 0`, no duplicate display name slotted, `sum(AT) <= intel.comp` after insertion. Track used attention and push `studynum` whenever the slot set changes.
8. **Study tick engine**: wall-clock driven; recompute progress lazily (on login, on demand) and/or on a timer; push `meter` (0-100) per studying item; on 100: award LP with the exact client formula (`Math.round(baseLP * sqrt(q/10) * expmod/100)`), consume the curio, update `exp` and `studynum`. Progression must survive restarts and continue offline (persist `startedAt` in wall-clock terms).

   Ordering gotcha: `Study`'s constructor reads `ui.sess.glob.cattr.get("intel").comp` without a null check (`CharWnd.java:661`), and `getExpMode()` only tolerates a missing `expmod` by falling back to 1.0 (`CharWnd.java:1101-1107`). The server must push the `intel` CAttr (and ideally `expmod`) before or together with the `chr` widget creation, or early clients will NPE when opening the character sheet.
9. **Acquisition LP**: per-family-line discovered-set of object types; award fixed LP on first acquisition (pickup of never-seen world object types, first craft of certain recipes, symbel crafting, ingredient use). Amounts and trigger taxonomy are server data (open question), but the mechanism - a persistent set consulted before awarding - must exist, including ancestor sharing so re-discoveries in the same line yield nothing.
10. **Persistence**: LP balance, expmod, attributes, skill values, learned skills, study slots (item identity, quality, elapsed), discovered-set, per character and family line, in the server database; the client holds none of it.
11. **Cheat surface**: the client predicts costs and gains for display only. Never trust client-side arithmetic: recompute attention sums, duplicate checks, cost curves, and LP grants server-side. Note the client already refuses to forward messages from unbound `Item`/`Inventory` widgets (`CharWnd.wdgmsg`), but the server must still validate every `sattr`/`buy`/item-move it receives.

### Implemented state (this server)

- **Wallet + skill values + catalog are live.** `skills.rs` owns the 11
  client-hardcoded skill-value names, the legacy sattr cost curve
  (`sattr_cost`: point from 0 costs 100; the bulk closed form
  `50*(k+n)*n`, `k=2*from+1`, matches `SAttr` exactly; no-op pairs cost 0
  so the client's send-every-SAttr batch stays valid), and a 9-entry
  non-incrementable catalog (names restricted to `gfx/hud/skills/*.res`
  verified present in `lib/haven-res.jar`; costs are server-defined data,
  see the open questions).
- **Prerequisites are enforced data (session 46)**: each catalog entry
  carries an optional `prereq` catalog name checked inside `buy()`
  before any charge. The first gated entry is `ahusb` (Animal
  Husbandry, 400 LP, requires Hunting - the documented legacy values,
  animals-and-husbandry.md taming step 1); the refusal path returns
  `BuyError::Prerequisite` and the wire handler chats "You need to
  know Hunting first." Ahusb additionally gates Quell the Beast
  (`skills::can_quell`).
- **Wire flow**: opening the character sheet pushes `exp`, `nsk`, `psk`;
  `sattr` is priced first and applied all-or-nothing (rejects unknown
  names, targets below the current value, anything past 100, and
  underfunded batches, refreshing `exp` so the client re-prices); `buy`
  charges via the catalog and re-pushes `exp`/`nsk`/`psk`. Refusals are
  delivered as Area Chat system lines (communication.md).
- **After `sattr`, the FULL CATTR snapshot is re-pushed** (not just
  vitals): `SAttr` widgets re-render from their `cattr` entry, so a
  vitals-only push would leave the sheet showing stale values. Caught by
  the wire e2e (`skillbot`), not by review.
- **Planting gate**: planting a seed on a plowed tile requires the
  `farming` skill value >= 1 (attrs key `farming`; fresh chars have no
  entry, i.e. 0). Refusal keeps the cursor stack and sends the system
  line "You need the Farming skill ...". One point from 0 costs exactly
  100 LP - the fresh-character wallet - so the gate is passable out of
  the box.
- **Passive LP accrual (deliberate deviation)**: the curiosity study
  system is not implemented yet, so the server grants a trickle of 2 LP
  per online minute (`skills::accrue`, integer-millisecond carry;
  remainder resets at the i32::MAX saturation point). `HNH_LP_RATE`
  multiplies the rate (0 or malformed disables accrual; the wire e2e
  uses 1000 to compress time). Action LP from tree/stone harvesting
  (implemented earlier) also refreshes the open sheet's `exp`.
- **Persistence**: `SavedPlayer.skills` (v2 additive, basenames) and the
  `attrs` map (skill values) round-trip; unknown saved skill names from
  older catalogs load as nothing.
- **Deferred** (blueprint items above still open): study/curio engine,
  acquisition LP and the family-line discovered set, property, and any
  expmod/attention interactions.

## Open questions

- **Did legacy curiosities cost Experience to study?** The current-game RoB table has an "Experience Cost" column and the Curiosity page says study "allows hearthlings to gain Learning Points at the cost of Experience Points"; the legacy client (`curio.conf`, `CuriosityStat`, tooltips) shows no EXP cost anywhere. Determine by inspecting legacy server behavior/records or 2011-2016 forum threads; if yes, the server needs an additional per-attribute or global EXP pool that study consumes.
- **Quality exponent on the legacy server**: client predicts `sqrt(q/10)`; the Hafen-era wiki says `Quality/10`. Legacy servers presumably matched the legacy client (`sqrt`); confirm from legacy records, then lock the formula.
- **Study slot count and layout**: the slots are server-created `Inventory` children of the study widget; the legacy count is not encoded in the client. Determine from legacy screenshots/packets (the Study panel is 400x295, which bounds the layout).
- **Meter update cadence**: how often the legacy server pushed `meter` (per tick? on integer percent change?). Any modest cadence is client-compatible; pick one and document it.
- **Interruption semantics**: when a curio is removed from a slot mid-study, does the server keep its progress (so re-slotting resumes) or reset it? Does a removed item still tick? Determine from legacy behavior; the client supports either (meter is per-item state).
- **Attention over-commit**: if INT drops (debuff) below the used attention, does the legacy server pause the lowest-priority curios or allow the over-commit until items are removed? Client displays `attused/attlimit` without complaint either way.
- **`expmod` (Learning Ability) sources in legacy**: which systems modify it (personal beliefs sliders, food, energy, world events) and its observed range. The forum guide "A Guide on Using Learning Points" (2014) hints belief sliders influence LP income; unverified.
- **Acquisition LP details**: award amounts per object type, the exact trigger set (pickup vs craft vs ingredient use), and how the ancestor-knowledge set is inherited across descendants (full sharing? per-line? pruned?).
- **Legacy non-incremental skill catalog and costs**: the full list of `gfx/hud/skills/*` resources with LP costs and prerequisites (e.g. Theft, Coinage, and the rest of the legacy tree) is server data; reconstruct from legacy resources or wiki (Legacy:Skill_List page no longer exists on RoB).
- **Raises-through-use for the 11 incrementable skills**: which actions raise which skill value, by how much, and any caps; legacy wiki describes their effects but not the training rules.
- **Property purchase with LP**: what "buying property" charged (claims? plots?) and through which widget; not visible in this client.
- **Fixed per-skill attribute bonuses**: whether any legacy skill granted flat attribute bonuses (int/psy etc.) beyond the softcap formulas; not evidenced client-side.

## Verification index (client symbols cited)

- `CharWnd` widget factory `chr` with `studyid` arg: `src/haven/CharWnd.java:65-74`.
- `CharWnd.Study` (attlimit init from `intel`, `setattnlimit`, `setattnused`, `updateStudyLp`): `src/haven/CharWnd.java:646-749`.
- `CharWnd.uimsg` cases `exp`, `studynum`, `nsk`, `psk`: `src/haven/CharWnd.java:1002-1030`.
- `CharWnd.SAttr` cost prediction and `buysattrs()`/`buyskill()`: `src/haven/CharWnd.java:230-345`, `751-766`.
- `CharWnd.getExpMode()` (expmod/100): `src/haven/CharWnd.java:1100-1107`.
- `Item.qmult = sqrt(q/10)`, curio lookup by tooltip name, tooltip `LP: ... Att: ...`, `meter` message: `src/haven/Item.java:265-273`, `290-348`, `412-416`, `489-493`.
- `Config.CuriosityStat`, `Config.loadCurio()` parsing `Name=LP AT TIME`: `src/haven/Config.java:202-245`.
- `Glob.CAttr`, `Glob.cattr`, `Glob.paginae`, `paginae/add/study` preload: `src/haven/Glob.java:45-46`, `66`, `80-102`, `130-162`.
- `Session` CATTR dispatch: `src/haven/Session.java:366-367`.
- `MenuGrid.usecustom` study toggle: `src/haven/MenuGrid.java:449-452`.
- `UI.bind` and `UI.wnd_study`: `src/haven/UI.java:45`, `147-150`.
- Study pagina resource strings (`Study`, `study`, `paginae/act/add`): `res/compiled/paginae/add/study.res`.
