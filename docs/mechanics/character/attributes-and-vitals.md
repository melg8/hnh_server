# Legacy Character Attributes and Vitals (Golden Blueprint)

> **Sources:** src/haven/CharWnd.java, src/haven/Charlist.java, src/haven/Glob.java, src/haven/Session.java, src/haven/Message.java, src/haven/IMeter.java, src/haven/VMeter.java, src/haven/Speedget.java, src/haven/GobHealth.java, src/haven/OCache.java, src/haven/Fightview.java, src/haven/ComMeter.java, src/haven/ComWin.java, src/haven/KinInfo.java, src/haven/BuddyWnd.java, src/haven/Buff.java, src/haven/Bufflist.java, src/haven/MenuGrid.java, src/haven/Item.java, src/haven/FlowerMenu.java, src/haven/LinMove.java, src/union/JSBotUtils.java, src/union/jsbot/JSHaven.java, etc/res-bgload, etc/needed/fep.conf, Ring of Brodgar wiki (Attributes, Hunger, Glossary, Legacy:Combat_Actions, Legacy:Village_Claim), Haven and Hearth Fandom wiki (FEP, Hitpoints, Village), H&H official forum thread "What's with the HP?" (May 2009)

## Summary

This document is the authoritative blueprint of the **permanent and volatile character state** that a legacy Haven and Hearth server must track per character and push to this client: the base attributes, the skill values that hang next to them in the character sheet, the vitals meters (health, stamina, hunger/energy, plus the happiness and authority meters this client family renders), movement speed state, and the personal-belief sliders that modify some of them.

The unifying architecture, visible everywhere in the client, is: **the client renders, the server owns**. Attributes arrive as server-pushed `(name, base, comp)` triples (`src/haven/Glob.java`, `Glob.cattr`); all meters are server-created widgets whose values are 0-100 percentages or server-authored tooltip strings (`src/haven/IMeter.java`); the client never computes an attribute modifier, a heal, or a decay tick. A Rust server from scratch must therefore reproduce: the attribute wire message, the meter widget set and update rhythm, and the game rules behind them (FEP growth, SHP/HHP/MHP health, stamina/energy coupling, belief effects).

Companion documents: food-driven attribute growth and the hunger economy are detailed in `food-and-fep.md` (same directory); LP purchase of skill values in `../skills/learning-points-and-curiosity.md`; combat use of attributes and the fight-relation vitals in `../combat/combat-system.md`; item quality, which scales FEP and LP gains, in `../items/items-and-quality.md`.

## Character state widgets: who creates what

Everything in the character sheet is a server-created widget; the client only registers factories:

| Widget type (wire) | Client class | Created with |
| --- | --- | --- |
| `chr` | `src/haven/CharWnd.java` | position; optional study-widget id as `args[0]` (`CharWnd.java:66-74`) |
| `charlist` | `src/haven/Charlist.java` | height in rows (`Charlist.java:53-58`) |
| `im` | `src/haven/IMeter.java` | background resource name + `(color, value)` bar pairs (`IMeter.java:42-52`) |
| `vm` | `src/haven/VMeter.java` | amount 0-100 + RGB(A) color (`VMeter.java:37-54`) |
| `speedget` | `src/haven/Speedget.java` | `cur`, `max` speed indices (`Speedget.java:51-57`) |
| `buffs` | `src/haven/Bufflist.java` | position only (`Bufflist.java:41-47`) |
| `frv` | `src/haven/Fightview.java` | position (`Fightview.java:110-117`) |

The `charlist` widget is the login-side character selection list: the server pushes an `add` UI message per character with its name and avatar resource ids (`Charlist.uimsg`, case `"add"`, `Charlist.java:147-163`), and the client answers with the `play` widget message carrying the chosen character name (`Charlist.java:124`, `Charlist.java:137`). A from-scratch server must implement this handshake before the world loads.

## The attributes

### Wire format

Attributes arrive on their own session message, not as widgets. `Session.java:366-367` dispatches message type `RMSG_CATTR` (numeric value 9, `src/haven/Message.java:42`) to `Glob.cattr` (`src/haven/Glob.java:147-162`), which reads repeated triples until end of message:

```
string name; int32 base; int32 comp
```

- `base` is the permanent attribute value (the FEP-grown number).
- `comp` is the *composite/effective* value the server computed for the current instant, including equipment, buffs, wounds and belief modifiers.

`Glob.CAttr` is an `Observable`; the character-sheet widgets register as observers and re-render on every push (`CharWnd.java:76-97`). The union fork adds a console notice when `base` increases - "Your STR raised by N points" (`Glob.java:90-101`) - which is client-side sugar, but it documents the expectation that the server pushes an updated triple **at the moment a base attribute grows** (i.e. immediately after eating enough FEPs).

Server rule: push the full attribute set at login (the client builds its first `NAttr` widgets from whatever arrived), then push deltas whenever `base` or `comp` changes. Sending a triple with unchanged values is a no-op client-side (`Glob.java:90-92`).

### The eight base attributes

The Attributes tab is built by `baseval()` calls in the `CharWnd` constructor (`CharWnd.java:786-796`), which fix both the wire names and the rendering order:

| Wire name | Rendered label | Row order |
| --- | --- | --- |
| `str` | Strength | 1 |
| `agil` | Agility | 2 |
| `intel` | Intelligence | 3 |
| `cons` | Constitution | 4 |
| `perc` | Perception | 5 |
| `csm` | Charisma | 6 |
| `dxt` | Dexterity | 7 |
| `psy` | Psyche | 8 |

These eight strings also form the FEP-eligible attribute set used by the client helpers `getMaxFepValue`/`getMaxFepName` (`CharWnd.java:963-990`, local `attrNames` list at `CharWnd.java:175-176`). New characters start with base attributes of 10 (Ring of Brodgar Glossary, "Hearthlings start with Base Attributes of 10").

The current-world game later added a ninth attribute, Will (the Ring of Brodgar "Attributes" page lists nine sections including Will). **This client predates Will**: it is not in `attrNames`, has no rendering row, and a server targeting this client should not send a `will` key. The repo's `etc/needed/fep.conf` likewise has no WIL column, which is one of the markers that it is the legacy table (see `food-and-fep.md`).

### Rendering: base and composite

`NAttr` renders each attribute as two labels (`CharWnd.java:174-214`):

- the white `base` value (`lbl.settext(Integer.toString(attr.base))`, `CharWnd.java:209`);
- a second label shown only when `comp != base`: green (`buff` color, `CharWnd.java:52`) with tooltip `+N` when buffed, red (`debuff`, `CharWnd.java:51`) with tooltip `N` (the negative delta) when debuffed (`CharWnd.java:188-208`).

The Ring of Brodgar "Attributes" page (current world) confirms the semantic: equipment, elixirs and wounds move `comp` away from `base`, display is capped at a minimum of 1, and equipment bonuses cannot more than double an attribute (their example: Strength 20 can be pushed at most to 40 by gear). The *cap rule* is a wiki claim, not client code - the client renders whatever `comp` it receives.

### One extra attribute: expmod, "Learning Ability"

A ninth row renders `expmod` as a percentage: the label "Learning Ability:" with an anonymous `NAttr` subclass that prints `attr.comp` followed by `%`, red when below 100, green when above (`CharWnd.java:804-815`). Its composite value is the character's LP-learning multiplier: `CharWnd.getExpMode()` returns `expmod.comp / 100.0` (`CharWnd.java:1101-1107`) and the client multiplies curiosity LP gains by it (`src/haven/Item.java:339`, `CharWnd.Study.updateStudyLp` at `CharWnd.java:732-748`). The server owns `expmod` exactly like the eight base attributes (same `cattr` message); see `../skills/learning-points-and-curiosity.md` for its interaction with study.

### Attributes are not LP-buyable

Only the eleven *skill values* (`unarmed`, `melee`, `ranged`, `explore`, `stealth`, `sewing`, `smithing`, `carpentry`, `cooking`, `farming`, `survive`, instantiated via `skillval()` at `CharWnd.java:832-844`) are `SAttr` widgets with plus/minus buttons and an LP cost preview (`CharWnd.java:230-345`). The eight base attributes are plain `NAttr` rows: the only legacy ways to grow them are eating FEPs (server-side, see `food-and-fep.md`) and server-side belief/effect modifiers. The "Buy" button in the Attributes tab sends `sattr` with name/target pairs, but only for `SAttr` entries (`buysattrs`, `CharWnd.java:751-761`).

## What each attribute does

Client-verified effects are marked **[code]**; everything else is wiki knowledge - Ring of Brodgar "Attributes" (current world unless labeled legacy) and the cached Legacy:Combat_Actions page - and should be re-validated before being treated as legacy law.

| Attribute | Effects |
| --- | --- |
| Strength (str) | **[code, legacy]** Melee/unarmed damage: `damage = basedamage * ql * str / 10` with strength-only unarmed variants `0.75 * 50 * sqrt(str/10)` (punch), `100 * sqrt(str/10)` (kick) and the 1.5x claw variant (Legacy:Combat_Actions). **[wiki]** Mining ability scales with strength and average tool quality (pickaxes count double); destroying objects deals `sqrt(strength)` plus tool bonus; movement speed while carrying loads; resistance to being pushed; softcap on metal/steel crafting quality together with Smithing. |
| Agility (agil) | **[wiki]** Close-combat tempo: higher agility than your opponent lowers your attack cooldowns and raises theirs by the same amount; reduces an enemy's minimum ranged damage. |
| Intelligence (intel) | **[code]** Study attention limit: `attlimit = ui.sess.glob.cattr.get("intel").comp` (`CharWnd.java:661`), updated live via `NAttr.update` for `intel` (`CharWnd.java:210-212`) and `Study.setattnlimit` (`CharWnd.java:722-725`). The server must enforce `sum(curio AT) <= intel.comp` (see `../skills/learning-points-and-curiosity.md`). **[wiki]** Authority gain together with Charisma; scent strength of criminal acts together with Stealth; quality of starter tools. |
| Constitution (cons) | **[wiki, current]** Raises max hitpoints; reduces stamina lost while swimming. Max-HP formula per the Fandom "Hitpoints" page: `MHP = 100 * sqrt(CON/10)` (so CON 10 = 100 MHP, CON 100 = ~316). Legacy-era confirmation that CON governs max HP is implicit in the belief system (below) and community pages; the exact legacy formula is an open question. |
| Perception (perc) | **[wiki]** Foraging detection; noticing criminal scents; ranged combat minimum damage (the wiki itself notes the ranged claim is contested). |
| Charisma (csm) | **[wiki]** Authority gain together with Intelligence; maximum party size; quest rewards; feast bonuses at a bonfire. |
| Dexterity (dxt) | **[wiki]** Crafting quality softcaps (pottery; sausages with Cooking; sewing/tanning with Sewing); lockpicking with Intelligence and Stealth. |
| Psyche (psy) | **[wiki]** Quality of jewelry, gemcutting, silk items; rites-and-ritual curiosities; pigment softcap `Psyche * Cooking/2`. |

## The vitals meters

### The meter framework

All vitals are `IMeter` widgets created by the server (widget type `im`, `IMeter.java:42-52`). Creation arguments are: the background resource name (the meter's identity), then `(Color, int value)` pairs, one per bar, with each `value` in 0-100 (`IMeter.java:44-49`). Updates arrive as the `set` UI message with fresh `(color, value)` pairs (`IMeter.java:94-99`), and the server can replace the tooltip at any time with the `tt` UI message (`IMeter.java:100-101`). Bars are painted from the same origin with widths proportional to `value` (`IMeter.java:83-88`), i.e. **layered**, later bars covering earlier ones - a layout that suits a full-width "current maximum" bar behind a shorter "current" bar; the exact bar stack for each gauge is server data (Open questions).

The client also supports a vertical meter, `VMeter` (widget type `vm`, `VMeter.java:37-54`): creation args are `amount` (0-100) plus an RGB or RGBA color, updates via the `set` message with a new amount. Nothing in the stock HUD creates one; it exists as a general-purpose server widget a legacy server may use for any 0-100 gauge.

Note on task scoping: `CharWnd` contains **no** health/stamina/energy meters. Its only meter is the FEP bar (`CharWnd.FoodMeter`, `CharWnd.java:375-470`), which is food data, not a vitals gauge; the vitals meters live on the HUD as server-created `im` widgets. Combat meters (`ComMeter`, `ComWin`) are client-rendered from `Fightview` state and are covered in `../combat/combat-system.md`.

### The known meters and their identities

A meter has no id string other than its background resource path, sent as creation argument 0. The preload list `etc/res-bgload` fixes the resource inventory (`etc/res-bgload:1227-1231`):

```
gfx/hud/meter/auth
gfx/hud/meter/happy
gfx/hud/meter/hngr
gfx/hud/meter/hp
gfx/hud/meter/nrj
```

and the union fork's bot layer (`src/union/JSBotUtils.java`, `updateMeters`, `JSBotUtils.java:100-140`) identifies them by substring of that path and parses the server-authored tooltips:

| Substring | Meter | Tooltip the union server sends | Parsed as |
| --- | --- | --- | --- |
| `hp` | Health | three `/`-separated numbers | soft HP, hard HP, max HP (`JSBotUtils.java:110-116`) |
| `nrj` | Stamina/fatigue | one percentage | stamina percent (`JSBotUtils.java:106-109`) |
| `hngr` | Hunger (food/energy) | two `%`-delimited numbers | percentage, absolute food value (`JSBotUtils.java:117-121`) |
| `happy` | Happiness | two `%`-delimited numbers | happiness, "towards" (`JSBotUtils.java:122-132`) |
| `auth` | Village authority | `current/max` | authority points (`JSBotUtils.java:133-138`) |

The meanings of the health numbers are corroborated by the JS bot API comments in `src/union/jsbot/JSHaven.java`: `jGetSHP` returns the player's soft HP ("softHP", `JSHaven.java:604-609`), `jGetHHP` the hard HP ("hardHP", `JSHaven.java:611-617`), `jGetMHP` the maximum HP ("maximum HP quantity", `JSHaven.java:619-625`), `jGetHungry` the character's *absolute* hunger (`JSHaven.java:596-601`), and `jGetStamina` the character's fatigue ("fatigue", `JSHaven.java:627-633`). Meter creation order and parent widgets are server decisions; the client renders them wherever they are attached.

### Health: SHP / HHP / MHP

Legacy health is three numbers, confirmed by the May 2009 official-forum thread "What's with the HP?" ("Your soft hp is limited by your hard hp, just like soft and hard hp is limited by max hp. The difference is that hard hp takes a lot more time [to recover]") and by the Fandom "Hitpoints" page and RoB Glossary (current world, same model):

- **SHP (soft HP)** - the current/pain pool. Regenerates over time toward the current HHP whenever the character's hunger is above the "Very Hungry" level (Fandom "Hitpoints"). Reaching 0 knocks the character unconscious for roughly a minute (Fandom "Hitpoints"); combat moves reference self-inflicted SHP costs (`-1% * Intensity SHP damage to yourself`, `-50% SHP to yourself` in Legacy:Combat_Actions).
- **HHP (hard HP)** - the mortal pool / current maximum. Does not regenerate naturally; healed by leeches and gauze (current world) and directly by food via the FEP table's HHP component (`etc/needed/fep.conf`, e.g. `Raw Chicken Meat=HHP:5`; see `food-and-fep.md`). Most attacks chip a little HHP, and while unconscious all damage lands on HHP (Fandom "Hitpoints"). HHP 0 means permanent death (see `../combat/combat-system.md`, Death section).
- **MHP (max HP)** - the ceiling of HHP. 100 on a new character; raised by Constitution; the Life/Death belief slider moves it by up to 20% in either direction (Fandom "Hitpoints", current-world statement).

**Per-limb HP: not in this client.** The HUD has a single health `im` meter (whatever bar layout the server pushes), and no body-part HP list exists anywhere in the client code. Other creatures get a coarse five-step (0-4) health level through the object-attribute path: `OCache.health` (`src/haven/OCache.java:323-328`) stores it in `GobHealth` (`src/haven/GobHealth.java:31-60`), which tints the sprite redder the lower the value and exposes `asfloat() = hp/4`; `Gob.getHealth()` converts it to a 0-100 percent (`src/haven/Gob.java:154-160`). A legacy server should therefore model whole-body SHP/HHP/MHP for players and the five-step health level for animal/monster gobs.

### Stamina (nrj)

The stamina meter is the fatigue gauge for actions. Community-documented behavior (RoB Glossary "Stamina", current world; legacy is presumed close):

- Activities drain it; as it drops below thresholds, actions become unavailable: below 50% no fourth speed (sprint), below 29% no digging, below 25% no third speed (run), below 10% only crawling, below 5% no movement. Ordinary work auto-stops around 10% so the character does not strand itself.
- Recovery is drawn from the food/energy pool ("stamina regenerate 10% per 10% energy" - i.e. regen rate scales with how well fed the character is).
- Drinking beverages (water, tea, milk, wine, beer) restores stamina quickly at an extra energy cost.
- Maximum stamina is a constant; Constitution only reduces its loss while swimming.

The client-side contract: the server pushes `set` on the `nrj` meter (one bar, percentage) and may raise/lower the available movement speeds through `speedget`'s `max` UI message (`Speedget.java:93-101`), which is exactly how "too tired to sprint" should be expressed.

### Hunger / energy (hngr)

The hunger meter is the food-energy gauge; its tooltip carries both the percentage and the absolute value (the bot's "absolute hunger"). Semantics, decay, starvation thresholds, and its tight coupling with stamina are the subject of `food-and-fep.md` in this directory. Summary: eating raises it, time (and, in legacy, in-game activity - see the RoB "Hunger" page note "Unlike legacy haven hunger levels are NOT effected by ingame activities other than eating foods") drains it, high energy enables SHP regeneration, zero energy rapidly burns HHP and kills.

### The extra meters: happy and auth

- **auth** - village authority, a village-level resource, not a personal vital: the Legacy:Village_Claim page documents the village idol draining 5000 authority per in-game day with an initial pool of 125,000 and a cap of 250,000. The meter tooltip is `current/max`.
- **happy** - two percentages ("happiness" and "towards" in the bot's naming). No client code explains them and no legacy wiki page recovered so far documents them; treat as server-defined affect/mood state and see Open questions.

## Movement speed

Speed is server-driven end to end:

- The `speedget` widget (created by the server with `cur` and `max`, `Speedget.java:51-57`) renders four gaits labeled `crawl`, `walk`, `run`, `sprint` (`Speedget.java:38-49`); the player's chosen gait is sent back with the `set` widget message (`Speedget.setspeed`, `Speedget.java:116-121`), and the server corrects it via `cur`/`max` UI messages.
- Actual displacement is server-authored motion data on the player gob (`src/haven/LinMove.java` and friends), so per-gait tile-per-second values are server constants. Current-world reference values (RoB Glossary "Speed"): crawl 1.5, walk 3.0, run 4.5, sprint 6.0 tiles per second, with faster gaits consuming stamina more quickly and terrain capping the usable gait.
- Weight: there is no general inventory-encumbrance system (RoB Glossary: "There is no encumbrance or weight system from having a full inventory or wearing heavy armor"), but Strength affects movement speed *while carrying objects* (RoB "Attributes", Strength section).

## Personal beliefs and their vitals effects

The Beliefs tab renders six sliders as `Belief` widgets (`CharWnd.java:873-886`), each bound to an ordinary `CAttr` entry from the same `cattr` message stream (`CharWnd.java:80-85`), rendered as `left / right` with a marker at `comp` (possibly inverted, `CharWnd.java:99-172`):

| Wire name | Left pole | Right pole |
| --- | --- | --- |
| `life` | death | life |
| `night` | night | day |
| `civil` | barbarism | civilization |
| `nature` | nature | industry |
| `martial` | martial | peaceful |
| `change` | tradition | change |

Clicking an arrow sends the `believe` widget message with `(name, delta)` (`Belief.buy`, `CharWnd.java:147-151`). The server validates the change, updates the underlying attribute pair, and starts a lockout during which further changes are refused - the client greys the change buttons while the lockout lasts (`Belief.update`, `CharWnd.java:153-171`). The lockout is communicated as the `btime` UI message (`CharWnd.java:1033-1034`) and rendered by `BTimer` with real seconds = `(btime - 1) / 3` (`CharWnd.java:347-373`).

Documented effects of the sliders on this document's subject matter: the barbarism/civilization slider carries a Constitution modifier (which therefore moves MHP), and the death/life slider moves MHP by up to +/-20% (Fandom "Hitpoints", current world). The tradition/change slider gates attribute/skill/LP inheritance on death and reincarnation (see `../combat/combat-system.md`, Death and reincarnation hooks).

## Combat-facing vitals (summary)

During an engagement the server opens a `frv` widget and pushes per-opponent relations carrying `balance`, `intensity`, `offence`, `defence`, and both sides' initiative points (`Fightview.Relation`, `Fightview.java:56-61`, update paths in `Fightview.uimsg` at `Fightview.java:230-289`); the client draws them in `ComMeter` (`ComMeter.java:58-95`, bars scaled by 1/100) and `ComWin`. Damage lands on the SHP/HHP pools described above. Full rules, moves, and the legacy damage formula are in `../combat/combat-system.md`.

## Reputation, karma and crime

- There is **no karma/reputation meter** in this client. A source search for `karma`, `rep`, `crime` surfaces only the *criminal acts toggle*: the `paginae/act/crime` action resource (`CharWnd.java:943-947` auto-toggles it at login when configured; `src/haven/MenuGrid.java:375-377` gives the toggle a local buff with sentinel id `-1`, as with tracking `-2` and swim `-3`, `MenuGrid.java:378-394`).
- The gameplay consequence of that toggle is server-side: with criminal acts active the character can interact with (and steal from) other people's claims and leave *scents* that Perception/Stealth/Intelligence govern (Fandom "Village" page; RoB "Attributes" Intelligence/Perception rows). Scents are the legacy tracking system's data, not a rendered meter.
- Personal notoriety is expressed indirectly (kin colors in `src/haven/KinInfo.java`, buddy groups in `src/haven/BuddyWnd.java`); the `auth` meter is village authority, not reputation.

## Server implementation notes

A from-scratch Rust server must, per character:

1. **Attributes**: store `(name -> {base, comp})` for the eight base attributes plus `expmod` (and the six belief pairs). Compute `comp` from base + equipment/buff/wound/belief modifiers on every relevant change. Push `RMSG_CATTR` (type 9) triples at login and on every change; skip no-op pushes.
2. **Attribute growth**: base attributes grow only through the FEP pipeline (`food-and-fep.md`), never through the `sattr` message; the `sattr` widget message applies only to the eleven skill values and must be priced with the LP cost curve (see `../skills/learning-points-and-curiosity.md`).
3. **Meters**: create at login one `im` widget per gauge with the stock background resources (`gfx/hud/meter/hp|nrj|hngr|happy|auth`), then push `set` (and `tt` for the tooltip payloads) on change. Keep the tooltip formats the ecosystem expects: health `SHP/HHP/MHP`, hunger `%` + absolute, authority `current/max`.
4. **Health**: maintain SHP (regenerable, knockdown at 0), HHP (mortal pool, food/leech/gauze healing only), MHP (CON- and belief-scaled ceiling). Apply unconsciousness on SHP 0 and death on HHP 0 with the corpse/inheritance flow of `../combat/combat-system.md`. Push a 0-4 health level per damaged non-player gob via the object-attribute health message.
5. **Stamina/energy**: maintain the fatigue pool with activity drains and threshold gating; express action unavailability both through the `nrj` meter and by clamping `speedget`'s `max`; couple regen to the food pool.
6. **Speed**: accept `speedget` `set` requests, clamp to `max`, and author movement with the per-gait velocities.
7. **Beliefs**: implement `believe` with per-slider magnitude limits, a change lockout pushed as `btime`, and the MHP/CON side effects.
8. **Character selection**: implement the `charlist` widget with `add` messages and the `play` reply before spawning into the world.
9. **Attention limit**: enforce the study-attention budget against `intel.comp` live (see `../skills/learning-points-and-curiosity.md`).

## Open questions

1. **Legacy MHP formula.** Current world: `MHP = 100 * sqrt(CON/10)` (Fandom "Hitpoints"). The legacy constant (was it the same square-root curve? linear?) is undocumented in recovered sources. How to determine: official forum threads 2008-2011 around "Max HP", or data-mining the legacy client's official server by observing MHP tooltips across known CON values.
2. **Meter bar layout.** How many bars each `im` meter carries and their colors (e.g. does the health meter show two layered bars - SHP over HHP - or one?). Determine by logging `NEWWDG`/`set` messages from the union reference server.
3. **happy meter semantics.** What the two percentages mean ("happiness"/"towards" is bot folklore). Candidate sources: union server code or forums discussing the meter; else leave server-defined.
4. **Legacy hunger/activity decay law.** Legacy hunger was affected by in-game activities (RoB "Hunger" note), but the rates per activity and per hunger level are unknown. Determine from legacy-era forum guides or by instrumenting the reference server.
5. **Attribute start values.** Current world starts all attributes at 10 (RoB Glossary). Legacy starting spread (uniform 10?) needs confirmation from legacy-era sources.
6. **Equipment modifier caps in legacy.** The "gear cannot more than double an attribute" rule is a current-world wiki statement; verify for legacy from the legacy Equipment Table pages.
7. **Belief effect magnitudes.** Which sliders moved which attributes in legacy, and by how much, beyond the Life/Death MHP +/-20% and barbarism/civilization CON claims (both current-world statements).
8. **Sprint velocity and terrain caps for legacy** (current world: 6.0 tiles/s sprint). Determine from legacy data or protocol traces.
9. **`hngr` tooltip exact format.** The bot only proves "two numbers with `%` separators"; the literal server string (e.g. `34.5% (12345)`) should be captured from a reference server before freezing the Rust implementation.
