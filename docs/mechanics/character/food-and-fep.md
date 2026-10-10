# Legacy Food, FEP and Hunger (Golden Blueprint)

> **Sources:** src/haven/CharWnd.java, src/haven/Config.java, src/haven/Item.java, src/haven/FlowerMenu.java, src/haven/Glob.java, src/haven/Message.java, src/haven/IMeter.java, src/haven/Session.java, src/haven/Buff.java, src/haven/Bufflist.java, src/union/JSBotUtils.java, src/union/jsbot/JSHaven.java, etc/needed/fep.conf, etc/res-bgload, Ring of Brodgar wiki (Hunger, Glossary, Attributes, Meatpie food page), Haven and Hearth Fandom wiki (FEP, Hitpoints)

## Summary

Food is the engine of character growth in legacy Haven and Hearth. Eating does four things, all server-side: it fills the **hunger/energy pool** (the `hngr` HUD meter), it feeds **Food Event Points (FEPs)** into per-attribute accumulators that eventually raise a base attribute, it can heal **hard HP** through the FEP table's `HHP` component, and it can push **buffs**. Eating is not a client concept: the client only shows a right-click flower menu whose options (including "Eat") are server-defined, renders the FEP bar from a server UI message, and displays per-food FEP tooltips computed from the local data file `etc/needed/fep.conf`.

This document is the blueprint a from-scratch Rust server needs: the FEP accumulation and attribute-gain algorithm, the `fep.conf` data format and its provenance (legacy table, not current-world numbers), the quality scaling law, the eat interaction flow, the hunger/energy economy and its coupling with stamina and health, starvation, drinking, and the buff wire format. The vitals meters themselves (health SHP/HHP/MHP, stamina, the meter widgets) are specified in `attributes-and-vitals.md` in this directory; the LP/study side of the character sheet is in `../skills/learning-points-and-curiosity.md`.

## The FEP system

### Concept

FEPs (Food Event Points) are the currency by which food raises the eight base attributes (`str`, `agil`, `intel`, `cons`, `perc`, `csm`, `dxt`, `psy` - the exact wire names are fixed by `src/haven/CharWnd.java:786-796` and repeated in the client helper lists at `CharWnd.java:175-176`, `963-990`). Every food grants a small vector of FEPs, e.g. Bear Salami gives Strength and Charisma points. The Fandom "FEP" page (the classic description, written for this era of the game) defines the loop:

1. The number of FEPs needed for the next attribute increase equals the character's **highest base attribute** (e.g. highest attribute 15 -> 15 FEPs required).
2. Accumulating FEPs of a *new* attribute type lowers the total required (variety, see below).
3. When the requirement is met, **which** attribute increases is chosen probabilistically, weighted by each attribute's share of the accumulated FEPs (the page's worked example: only Bear Salami eaten -> 57.14% Strength / 42.86% Charisma, matching its 4:3 FEP split in `etc/needed/fep.conf:11`).
4. After the increase, accumulated FEPs reset to zero; overflow beyond the requirement is lost (though it still influences which attribute is chosen).

The client exposes helpers over the server-fed FEP bar that pin down the same rules: `CharWnd.getMaxFepValue()`/`getMaxFepName()` return the highest of the eight base attributes (`CharWnd.java:963-990`) - the bot API wraps them as "max stat value/name" (`src/union/JSBotUtils.java:1042-1048`) - and `CharWnd.FoodMeter.getFepValue(id)` reads back one accumulator (`CharWnd.java:406-418`).

### The FEP bar widget and its wire message

The bar below the attributes in the character sheet is `CharWnd.FoodMeter` (`CharWnd.java:375-470`), a child of the server-created `chr` widget. The server updates it with the `food` UI message (`CharWnd.uimsg`, case `"food"`, `CharWnd.java:1031-1032`):

```
food: int32 cap, then triples: string id, int32 amount, Color col   (until end of args)
```

- `cap` and `amount` are in **tenths of a FEP**; the client divides by 10 for display (`getFepValue` returns `amount/10`, `CharWnd.java:412`; tooltip shows `cap/10.0`, `CharWnd.java:460-467`).
- Each triple is one colored segment: attribute id, accumulated tenths, and the segment color (the client's own `FEPColorMap` defaults for `STR..PSY` are gray placeholders, `src/haven/Config.java:146-153`, so the colors in the message are server-chosen).
- The tooltip is composed as `(1.2 str + 3.4 con) = 4.6 of 15.0` (`CharWnd.java:462-467`), i.e. per-attribute accumulators, their sum, and the current requirement.

So the server must maintain per-attribute FEP accumulators, a requirement value (highest base attribute, adjusted by variety - below), and push the whole vector on every eat or decay event that changes it.

### The data file: etc/needed/fep.conf

The per-food FEP table ships with this repo at `etc/needed/fep.conf` (111 entries). The client parses exactly this format at startup (`Config.loadFEP`, `src/haven/Config.java:247-265`, file name `fep.conf` in the working directory): each line is

```
DisplayName=ATTR:delta ATTR:delta ...
```

- `DisplayName` is lowercased before lookup (`Config.java:258`) and matched against the item's display name - `Item.name()` prefers the server-sent tooltip string, then the resource tooltip layer (`src/haven/Item.java:265-273`). For the client tooltip to show FEPs, the server's item tooltip names must therefore match the table keys (case-insensitively).
- `delta` values are floats; fractional FEPs are legal (`Peapod=STR:0.1 PER:0.9`, `etc/needed/fep.conf:63`).
- Attribute keys seen in the file: `STR, AGI, INT, CON, PER, CHA, DEX, PSY`, plus the special key `HHP`.
- `HHP` means **hitpoint healing**: eating the food restores hard HP directly. Representative rows: `Bark Bread=CON:5 HHP:0.2` (`fep.conf:9`), `Bear Meat=HHP:1` (`:10`), `Raw Chicken Meat=HHP:5` (`:74`), `Harmesan Cheese=CHA:25 HHP:5` (`:53`), `Shewbread=STR:4 HHP:5` (`:102`), `Rat on a Stick=PSY:0.2 HHP:0.8` (`:73`).

Flagged extremes of the legacy table (useful as sanity anchors for a re-implementation): `Midnight Blue Cheese=STR:75` (`:57`), `Jorbonzola=INT:50 PSY:12` (`:55`), `Sunlit Stilton=CHA:100` (`:105`), `Bierwurst=STR:10 CON:10` (`:15`). Raw meats carry the largest HHP values (`HHP:1` to `HHP:5`), consistent with the community rule that raw meat is the early hard-HP healer.

**Provenance and current-world drift.** This table is the *legacy* FEP dataset: it has no WIL column (the ninth, current-world attribute) and its numbers differ from the live game's published tables. Examples: the file gives `Cellar Cheddar=STR:12 AGI:3` (`:33`) while the current Ring of Brodgar attribute pages list Cellar Cheddar at STR 8-10 and no AGI; `Brodgar Blue Cheese=STR:15` (`:27`) vs current 8; `Midnight Blue Cheese=STR:75` vs current 50. **A legacy server must treat `fep.conf` as the truth and explicitly must not import current-world FEP tables.** Conversely, the file is not obviously exhaustive for legacy (no plain `Bread`, no drinks) - see Open questions.

### Quality scaling

Food FEPs scale with item quality by the same square-root law as LP: the client computes `qmult = Math.sqrt((double) q / 10)` in the `Item` constructor (`src/haven/Item.java:412`) and displays `FEP = table value * qmult` in food tooltips (`Item.calcFEP`, `Item.java:278-288`). Effective multipliers: 1.0 at quality 10, 2.0 at quality 40, 3.0 at quality 90. The server's actual FEP grant must use the identical formula and rounding behavior or tooltips will lie. (See also `../items/items-and-quality.md`, which documents `qmult` for the item system at large.)

## How eating works (client/server flow)

1. The player right-clicks a food item; the client sends the `iact` widget message (`Item.mousedown`, button 3, `src/haven/Item.java:516`), or `itemact` when interacting with a held item (`Item.iteminteract`, `Item.java:539-542`).
2. The server answers with a flower menu: widget type `sm` (`src/haven/FlowerMenu.java:45-54`) whose option strings are fully server-defined - this is where "Eat" (and "Drink", etc.) comes from.
3. Clicking a petal sends back `cl` with the option index (`FlowerMenu.Petal.mousedown`, `FlowerMenu.java:89-92`).
4. The server then performs the eat transaction atomically: capacity check against the hunger/energy pool, add the food's hunger value, apply FEP deltas (table value x quality multiplier, modified by food efficiency/satiation), apply any `HHP` healing, start satiation decay for the food's category, push buffs, and emit the state updates below.
5. Expected state updates after an eat:
   - `food` UI message to the `chr` widget (new accumulators/cap) - `CharWnd.java:1031-1032`;
   - `set`/`tt` on the `hngr` IMeter (new percentage and absolute value);
   - `cattr` message if an attribute actually increased (the client announces "Your STR raised by N points", `src/haven/Glob.java:90-101`);
   - `set`/`tt` on the `hp` meter if HHP changed;
   - `buff` messages for any food effects.

The client never predicts any of this; there is no eating code outside the menu plumbing.

## The hunger / energy economy

### The meter

The hunger meter is the server-created `im` widget whose background resource is `gfx/hud/meter/hngr` (`etc/res-bgload:1229`). The union reference server's tooltip carries two numbers: the bar percentage and the *absolute* food value - the bot parses `buf[1]` as `playerHungry`, the "absolute hunger" (`src/union/JSBotUtils.java:117-121`; the JS API comment for `jGetHungry` says "absolute hunger", `src/union/jsbot/JSHaven.java:596-601`). The existence of an absolute value above any 100% cap is confirmed by gameplay documentation: foods "fill N% of the energy bar" and can exceed it many times over (RoB "Meatpie" page: "This food fills 700% of the energy bar").

### Energy: the work battery

The community model (RoB Glossary, HUD section - current world, but the architecture is inherited from this client's era):

- **Energy is a measure of how much work the character can do.** Eating food raises it; it is spent primarily by stamina regeneration and by drinking.
- **Stamina coupling**: stamina recovers over time by drawing from the energy pool (glossary phrasing: "stamina regenerate 10% per 10% energy"), and drinking water/beverages restores stamina fast at an extra energy cost. This is why eating well also means regenerating stamina and, indirectly, soft HP.
- **Thresholds** (glossary numbers, in absolute energy units where maximum is a fixed constant): at 8000+ the character is in the "healing" state and SHP regenerates; at 5000 or below hard labor (digging, mining) is unavailable; at 2000 or below the character *starves* and slowly loses health; at 0 the character rapidly loses HHP and dies quickly. **Maximum energy is never lost or gained.**
- The Fandom "Hitpoints" page gives the same gate in hunger-level terms: SHP "will slowly regenerate, up to the character's current HHP value, as long as ... hunger is above Very Hungry".

These exact numbers are current-world references for a model the legacy server must reproduce with its own constants (see Open questions); the qualitative ordering - full belly heals, empty belly starves, zero kills - is the invariant to implement.

### Hunger levels and food efficiency

As the energy pool drains, the character passes through named hunger levels that act as **FEP-efficiency multipliers** on further eating. The RoB "Hunger" page tabulates the current world: Ravenous (300% FEP efficiency, slowest decay), Famished (200%), Hungry (150%), Content (100%), Full (90%), Stuffed (75%), Overstuffed (50%, fastest decay - about 1% per 7 seconds vs Ravenous' 1% per 2 hours). The same page states the legacy difference explicitly: "Unlike legacy haven hunger levels are NOT effected by ingame activities other than eating foods" (sic) - i.e. **in legacy, hunger decay depended on in-game activity** (working, traveling, swimming drained food faster), in addition to real-time decay. A legacy server needs an activity-aware drain model; the legacy rates themselves are not in any recovered source.

### Satiation / variety (the "gluttony" mechanic)

Eating the same foods repeatedly is penalized; dietary variety is rewarded:

- Fandom "FEP": accumulating FEPs of different attributes "will lower the total number required" for the next increase (their example: needing X FEPs of pure Strength drops below X once Dexterity FEPs enter the bar). This is the legacy formulation of the mechanic and it operates on the cap that the server already sends in the `food` message.
- RoB "Hunger" (current world): the "variety bonus" "works similarly to legacy where each new food type eaten will decrease the amount of FEPs in order to level-up an attribute. It is increased by higher food efficiency, but is also reduced by your highest Base attribute (max stat)."
- The current world renders this as per-category "Food Satiations" percentages on the character sheet (RoB Glossary); this legacy client has **no such widget** - in legacy the mechanic is visible only as a shrinking requirement in the FEP bar tooltip.

Server model to implement: each food belongs to a satiation category; each category's satiation rises when eaten and decays over time; the FEP requirement sent in the `food` message is `max(base attributes)` scaled down by a variety factor computed from the number/spread of recently eaten categories and the current food-efficiency level. The precise legacy variety formula is not in any recovered source (Open questions).

## Starvation mechanics

Synthesis of the sources above into the rules a legacy server should implement:

1. Energy above the healing threshold: SHP regenerates toward HHP; FEP efficiency is at its lowest (overstuffed).
2. Energy below the hard-labor threshold: digging/mining disabled (stamina actions fail).
3. Energy below the starving threshold: SHP starts draining ("Starve, which will slowly drain your health", RoB Glossary); SHP regeneration stops (hunger at or below "Very Hungry", Fandom "Hitpoints").
4. Energy at zero: rapid HHP loss and quick death (RoB Glossary). Death flows through the mortal-pool rules in `../combat/combat-system.md` (corpse, inheritance, reincarnation).
5. Knocked-out characters (SHP 0) take all damage on HHP, so starving while unconscious accelerates death (Fandom "Hitpoints").

All thresholds, drain rates, and the legacy activity multipliers are server constants to be pinned down (Open questions); the state machine above is the documented shape.

## Drinking water and beverages

- There is **no thirst meter** in this client: the HUD gauges are `hp`, `nrj`, `hngr`, `happy`, `auth` only (`etc/res-bgload:1227-1231`).
- Drink containers exist as content: the craft paginae include `paginae/craft/waterskin`, `paginae/craft/waterflask`, `paginae/craft/winebottle`, `paginae/craft/wineglass` (`etc/res-bgload:1898-1904`), and the world ships wine plants and a winepress (`etc/res-bgload:1525`, `:1577`).
- Documented effect of drinking: beverages (water, tea, milk, wine, beer) restore **stamina** quickly, at the cost of extra energy (RoB Glossary "Stamina"/"Energy"). Drinking is the intended fast-recovery lever while food remains the slow battery.
- Alcohol: the current game has an "Alcohol" page and drink buffs, but no toxicity/alcohol meter or drunk-state UI exists in this client; any legacy inebriation would have to be expressed through the generic buff system (next section). Treat legacy alcohol effects as server-defined and unresolved (Open questions).

## Buffs from eating

Buffs are the generic server-pushed effect channel, and food effects (feast bonuses, symbel sitting, drink effects) ride on it. Wire format (`RMSG_BUFF`, type 12, `src/haven/Message.java:45`, dispatched at `src/haven/Session.java:380-381`; handler `Glob.buffmsg`, `src/haven/Glob.java:164-201`):

- `set`: `int32 id`, `uint16 resource id`, `string tooltip override`, `int32 ameter`, `int32 nmeter`, `int32 cmeter`, `int32 cticks`, `uint8 major`.
- `rm`: `int32 id`. `clear`: no payload.
- `ameter` (0-100) draws the small bar under the buff icon, `nmeter` a numeric overlay, `cmeter` a radial countdown; `cticks` is the total duration in server ticks of 0.06 seconds - the client computes remaining time as `cticks * 0.06 - elapsed` (`Buff.java:66-79`, `Bufflist.java:85-95`). A food buff lasting T seconds is therefore sent as `cticks = round(T / 0.06)`.
- Only `major` buffs render in the tray, capped at five visible icons (`Bufflist.java:38`, `97-98`); minor buffs are still tracked in `Glob.buffs` and can drive tooltips/logic.
- The tooltip shown is the server's override string, else the resource tooltip/action name (`Bufflist.tooltip`, `Bufflist.java:103-147`).

The client displays whatever the server pushes - there is no eating-specific buff code - so a legacy server defines its food/symbel/feast buff catalog as resources plus these messages. Community documentation describes symbel-feeding bonuses and feast-at-bonfire Charisma effects (RoB Glossary/Attributes), but the legacy buff resource names and magnitudes are unrecovered (Open questions).

## Server implementation notes

1. **Data**: embed a parsed copy of `etc/needed/fep.conf` (`Name=ATTR:float ...`, lowercase name keys); add per-food server data it does not carry: satiation category, hunger-fill (energy) value, edible/drinkable flag, and any buff ids. Names must match the item tooltip strings the server itself sends (`Item.name()` precedence, `Item.java:265-273`).
2. **Per-character state**: FEP accumulators per attribute (integer tenths), current requirement `cap`, per-category satiation levels with decay timers, absolute energy value (fixed maximum), hunger level, HHP-heal ledger.
3. **Eat transaction**: flower-menu driven (steps in the eating section); validate capacity (define the legacy overfill rule), grant `FEP * sqrt(q/10) * food-efficiency`, accumulate satiation, apply `HHP` healing to the hard pool, then push `food` + `hngr` meter + `hp` meter + `cattr` (on actual gains) + buffs.
4. **Attribute gain algorithm**: when accumulated FEPs reach `cap`, pick the attribute by weighted draw over the accumulator shares, `base += 1`, reset accumulators (overflow lost), recompute `cap = max(base attributes) * variety factor`, push `cattr` and `food`.
5. **Energy economy**: tick decay (real time x activity multipliers), feed stamina regen from energy, implement the healing/hard-labor/starvation/death thresholds, and expose the percentage + absolute pair on the `hngr` tooltip.
6. **Drinks**: implement drink interactions (flower menus on containers/fountains), stamina restoration with an energy cost, and container content meters via the item `meter` UI message (`src/haven/Item.java:489-493` renders a 0-100 fill with a `(N%)` tooltip suffix, `Item.java:331-333`).
7. **Buffs**: catalog food/symbel buffs as resources; push `set`/`rm`/`clear` with correct `cticks` scaling (0.06 s per tick) and `major` flags.
8. **Rounding discipline**: the client's visible predictions (`FEP * qmult` tooltips) must match server grants; keep the same float-to-int rounding as `Item.calcFEP` (`Item.java:284-285`).

## Server implementation notes (this repo)

Consolidated in session 91: the two per-session chronicle sections
are demoted to subsections under one H2; content preserved.

- fep.conf is parsed at boot (Config.loadFEP format, lowercase keys,
  HHP special-cased); the table is resolved through cwd- and
  exe-relative candidates so the binary boots from anywhere.
- Eat transaction: item `iact` -> `sm` widget with a server-defined
  "Eat" petal -> `cl 0` -> consume one unit, add energy (server policy
  fill = clamp(10 + 1.5*FEP-total, ..., 60) into the 0..100 pool),
  apply HHP as direct hard-pool points (doc note 10 assumption),
  grant FEPs scaled by sqrt(q/10) into integer-tenth accumulators.
- Requirement check: cap = highest of the eight base attributes
  (tenths x10); on reaching it a weighted draw over accumulator shares
  picks the attribute (+1) and the accumulators reset (overflow lost),
  matching the Fandom loop.
- The `food` uimsg is pushed to the chr widget as (cap-tenths, then
  (id, tenths, RGBA color) triples) - exactly CharWnd.FoodMeter.update's
  contract; ids are the lowercase attribute ids also used by CATTR.
- Per-species meat labels double as fep.conf keys: Cow/Aurochs "Beef",
  Deer "Raw Deer Meat", Boar "Boar Meat", Fox "Fox Meat", Hare "Rabbit
  Meat". Wolf meat has no legacy entry, so it carries no label and
  resolves no FEPs (nothing invented).
- Server-side satiation/variety and activity-driven decay are NOT
  modeled yet (open questions 2/5 below still stand); the energy pool
  drains via the existing vitals tick.

### Server implementation notes (this repo, sessions 79+81: fep.conf gaps)
- Session 79: plain `Bread` shipped no row in the 2009 file, so the
  S71 baked loaf resolved no FEP and the eat path silently bailed
  (the `eat: no fep entry` debug line). Server policy: `Bread=CON:5`
  (the baked-goods band, Apple Pie). This closes the provenance
  promise the session-79 fep.conf comment made.
- Session 81: the dough ingredient chains added raw `Apple=CON:1`
  (the raw-fruit band, Blueberries=INT:1; the wild apple tree drops
  them) and `Bucket of Honey=AGI:1` (one-fifth of the Honey Bun band;
  the wild hive's bucket-gated harvest). Same rule as Bread: a label
  the server can produce must resolve an fep.conf row or eating it
  silently does nothing - any new food source requires a table row
  in the same change set.
- Session 81 side effect: the raw forage items (Blueberries,
  Chantrelles, Grapes, Yellow Onion) all matched existing 2009 rows
  verbatim; the forage registry's labels double as the fep.conf keys
  so the raw handfuls stay eatable straight off the bush.

## Open questions

1. **Legacy hunger-fill values per food.** `fep.conf` carries FEPs and HHP but not the energy fill; the legacy per-food satiation/energy table (current-world RoB "Food Satiations"/food pages have replacement data) must be reconstructed from legacy-era pages or the reference server.
2. **Legacy hunger decay rates and activity multipliers.** Legacy decay depended on in-game activity (RoB "Hunger" note), but no rate table survives. Determine from legacy forum guides (e.g. "starvation" discussions) or by instrumenting a reference server.
3. **Legacy threshold constants.** The 8000/5000/2000/0 energy thresholds and the fixed energy maximum are current-world glossary numbers; the legacy values (and the legacy name of the "Very Hungry" gate) are unconfirmed.
4. **FEP id strings on the wire.** The `food` message's element `id` strings are server-chosen; presumably lowercase attribute names (`str`, `con`, ...), matching `getFepValue(id)` usage, but the exact casing/spelling should be captured from a reference server before freezing the Rust enum.
5. **Legacy variety/satiation formula.** The precise cap reduction from dietary variety (and the category list) is undocumented; only the qualitative rule survives (Fandom "FEP", RoB "Hunger").
6. **Eat-when-full rule.** Whether legacy allowed overfilling the energy bar arbitrarily (current world: yes, foods fill 700%+) and any "too full to eat" gate is unrecorded in client code.
7. **fep.conf completeness for legacy.** The file lacks entries for staples such as plain Bread and for drinks, and even for foods the current wiki documents (Meatpie); determine whether the legacy server used a larger table (the official server's data) and source the remainder.
8. **Legacy alcohol/inebriation.** No toxicity meter or drunk buff is evidenced in this client; whether legacy wine/beer applied buffs is unresolved. Check legacy-era forum threads and the current RoB "Alcohol" page for the lineage.
9. **Symbel/feast buff catalog.** The buff resources and magnitudes for sitting at a symbel or feasting at a bonfire (Charisma-based) are server data not present in the repo; needs field data or resource dumps.
10. **HHP unit semantics.** Whether the `HHP` value in `fep.conf` heals hard-HP points directly (assumed here, consistent with raw-meat healing folklore) or a percentage of MHP; verify against a reference server.
