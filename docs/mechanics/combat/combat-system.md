# Legacy Haven & Hearth Combat System (Golden Blueprint)

> **Sources:** src/haven/Fightview.java, src/haven/ComMeter.java, src/haven/ComWin.java, src/haven/GiveButton.java, src/haven/Avaview.java, src/haven/Buff.java, src/haven/Bufflist.java, src/haven/Glob.java, src/haven/IMeter.java, src/haven/GobHealth.java, src/haven/OCache.java, src/haven/Equipory.java, src/haven/Item.java, src/haven/MenuGrid.java, src/haven/CharWnd.java, src/haven/MapView.java, src/haven/FlowerMenu.java, src/haven/KinInfo.java, src/haven/BuddyWnd.java, src/haven/Resource.java, src/haven/Session.java, src/haven/Message.java, src/union/JSBotUtils.java, Legacy:Combat_Actions (Ring of Brodgar wiki, 2021-08-20 revision), Legacy:Death (Ring of Brodgar wiki, 2021-03-08 revision), H&H official forum thread "Running Away in Combat" (June 2013), H&H official forum thread "RFC: Hunting Exploits" (January 2019), H&H official forum thread on corrected cooldowns (viewtopic.php?f=2&t=23357, referenced by Legacy:Combat_Actions), Haven & Hearth Fandom wiki "Village" page

## Summary

Legacy combat in Haven & Hearth is a **server-authoritative, tab-target, openings-based duel system**. When the server decides that two characters are fighting, it opens (or reuses) one fight window (widget type `frv`, implemented client-side by `src/haven/Fightview.java`) and pushes, for every opponent in the engagement, a small numeric state record called here a *relation*: balance, intensity, initiative points (both sides), offence and defence. The client renders these numbers as meters (`src/haven/ComMeter.java`, `src/haven/ComWin.java`) and does **not** compute any combat outcome itself; the server owns all state transitions.

Every combatant has an **offence bar** ("attack bar", the client colors it red) and a **defence bar** (colored blue). Damage is only dealt after the attacker's attack has consumed part of the defender's defence bar; community sources describe this as the *openings* model ("if you get hit with no preexisting openings the damage will still be zero for that hit", H&H forum "RFC: Hunting Exploits", January 2019). Moves manipulate the bars (gain IP, trade attack for defence, build advantage), maneuvers set the defender's response profile, and attacks spend the attack bar plus initiative points (IP) to convert openings into damage.

The canonical legacy damage formula, per the Ring of Brodgar wiki page *Legacy:Combat_Actions* (transcribed with attribution below), is:

```
damage = basedamage * ql * str / 10
```

where `basedamage` is the weapon's base damage, `ql` its quality, and `str` the attacker's Strength. Unarmed attacks use strength-only variants of this formula. Weapons therefore plug directly into the item quality system (see `../items/items-and-quality.md`), and Strength into the attribute system (see `../character/attributes-and-vitals.md`).

Combat ends by disengagement (moving out, mutual "give", or server rules), by knockdown (soft HP depleted), or by death (hard HP reaching 0, per *Legacy:Death*, after which the character is gone for good and can only be succeeded by a reincarnated descendant spawned at the ancestor's hearth fire).

This document describes the data the legacy client exchanges with the server (as visible in the client code of this repository), the rules of the system as documented by the legacy wiki, and the responsibilities a from-scratch Rust server must implement to be wire-compatible with this client.

## The fight window: Fightview and its wire protocol

### Widget lifecycle

The server creates the fight window with a `new widget` message (session message type `RMSG_NEWWDG`, value 0, in `src/haven/Message.java`; dispatch in `src/haven/Session.java`) carrying the widget type string `frv`. The client factory registered by `Fightview`'s static block (`Widget.addtype("frv", ...)`, src/haven/Fightview.java) instantiates it and assigns it to `UI.fight` (`src/haven/UI.java`, field `public Fightview fight`). Creation of this widget is what player-made bots treat as "I have been aggroed" (`src/union/JSBotUtils.java`, `OnWidgetRecieve`: `if (wdg instanceof Fightview) { haveAggro = true; }`), i.e. the fight window exists exactly while at least one engagement involving the player is active. When the last engagement ends, the server destroys the widget (`RMSG_DSTWDG`), which clears `haveAggro` again (src/union/JSBotUtils.java, `OnWidgetRemove`).

Consequences for a Rust server:

- One `Fightview` widget instance per client session, created on first engagement and destroyed when no relations remain.
- All per-opponent state is carried in widget messages (`RMSG_WDGMSG`), not in object attributes.

### Relations: per-opponent state records

`Fightview.Relation` (src/haven/Fightview.java, inner class) holds, per opponent:

| Field | Meaning |
|---|---|
| `gob_id` | The opponent's object id; the client resolves the gob through `Glob.oc` (src/haven/Glob.java, src/haven/OCache.java) and renders its avatar with `Avaview` (src/haven/Avaview.java, field `avagob`) |
| `balance` | Integer combat balance / advantage, displayed raw; the circular dial in `ComMeter` indexes an 11-image scale with `scales[(-rel.balance) + 5]`, which implies the valid range is `-5..+5` with 0 in the center |
| `intensity` | Integer combat intensity, displayed raw next to the balance dial (src/haven/ComMeter.java) |
| `offence` | The opponent's offence (attack bar) toward you, scaled by 100 (value 10000 = 100%) |
| `defence` | The opponent's defence bar against you, scaled by 100 |
| `initiative_points_self` | Your initiative points (IP) in this engagement |
| `initiative_points_other` | The opponent's IP in this engagement |
| `give` | Two-bit disengagement/willingness state, rendered by `GiveButton` (src/haven/GiveButton.java) |

The relation list `lsrel` is ordered; the server controls which relation is "current" (selected target). The widget is laid out for `height = 5` rows (src/haven/Fightview.java constant), so up to five opponents are shown comfortably; the list itself is unbounded server-side. Names and row colors come from the opponent's `KinInfo` attribute (`Relation.name()` uses `KinInfo.rendered()`, `Relation.color()` uses the `BuddyWnd.gc` palette, src/haven/BuddyWnd.java), so the server must keep buddy/kin data flowing for correct fight-window rendering.

### Server-to-client widget messages (uimsg)

`Fightview.uimsg` (src/haven/Fightview.java) defines the exact server-to-client protocol:

| Message | Arguments (in order) | Effect |
|---|---|---|
| `new` | `gob_id, balance, intensity, give_state, ip_self, ip_other, offence, defence` (all integers) | Adds a relation to the front of `lsrel` |
| `del` | `gob_id` | Removes the relation; raises `Fightview.Notfound` if unknown |
| `upd` | `gob_id, balance, intensity, give_state, ip_self, ip_other` | Updates the soft state of a relation (not off/def) |
| `updod` | `gob_id, offence, defence` | Updates the offence/defence bars of a relation |
| `cur` | `gob_id` | Moves the relation to the front and marks it `current`; if the gob is unknown, `current` is set to `null` |
| `atkc` | `ticks` | Sets `next_attack_time = now + ticks * 60 ms`, i.e. the argument is measured in 1/60-second ticks; drives the pink attack-cooldown bar in `ComWin` |
| `blk` | `resource_id` | Sets the current `maneuver` (resolved through `Session.getres`, `n2r` helper) |
| `atk` | `current_res_id, next_res_id` | Sets the `current_attack` and `next_attack` (queued attack) action resources |
| `offdef` | `offence, defence` | Sets the player's *own* offence/defence values (`fv.off`, `fv.def`), displayed by `ComMeter` |

Note the deliberate split between `upd` (balance/intensity/IP/give, sent on each discrete state change) and `updod`/`offdef` (the fast-moving bars, which the server can stream frequently without resending the rest). A Rust server should reproduce this split to keep bandwidth proportional to bar motion.

### Client-to-server widget messages (wdgmsg)

`Fightview.wdgmsg` (src/haven/Fightview.java) forwards exactly two messages upward:

- `click` `(gob_id, button)` - sent when the player clicks an opponent's avatar (`Avaview`) or the current-target avatar. The server should treat this as "select/focus that opponent" and answer with `cur`.
- `give` `(gob_id, button)` - sent when the player clicks a `GiveButton`. `GiveButton.mousedown` sends `wdgmsg("click", button)` (src/haven/GiveButton.java) with the raw mouse button; the server toggles the corresponding bit of that relation's two-bit give state and echoes the new state back inside the next `new`/`upd` message (`Relation.give(int state)` applies it to both the in-list button and the current-target button). `GiveButton.uimsg` also accepts a direct `ch` message to change the state.

The give button is rendered with left/right arrow overlays (`ol`/`or` when the bit is set, `sl`/`sr` when clear) and is tinted by state (0 = reddish, 1 = bluish, 2 = greenish). The two bits most plausibly represent the two directions of a disengagement handshake (one bit per side of the fight pair); the client code proves the state exists and is server-controlled, but the exact server-side resolution rule (what combination of bits ends an engagement) is not observable client-side - see Open questions.

## Combat meters: what the client renders

### ComMeter (the circular dial, top-center of the screen)

`src/haven/ComMeter.java` renders, for `fv.current` (the relation chosen by the server via `cur`):

- A dial texture `gfx/hud/combat/com/00..10` selected by `scales[(-rel.balance) + 5]`: 11 discrete balance positions, so `balance` is an integer in `-5..+5`. This is the only "dial" state; the server must clamp/serialize it accordingly.
- The intensity number at offset `(66, 25)` and the balance number at `(66, 40)` (rendered as plain integers).
- **Own bars**: `fv.off` (red, `offcol = (255,0,0)`) drawn leftward from `(54, 61)` with width `fv.off / 200` pixels, and `fv.def` (blue) from `(54, 71)`; both labeled with the value divided by 100 (so the displayed number is a percentage). Offence/defence values are therefore percentages scaled by 100; 100% = 10000.
- **Opponent bars**: `rel.offence` (red) rightward from `(80, 61)` and `rel.defence` (blue) from `(80, 71)`, same scaling, same labels.

The four bars are the concrete UI realization of the wiki's "attack bar" and "defense bar": your red bar is your offence against the current opponent, your blue bar is your defence against them, and the pair of bars on the right is the same state from the opponent's side as seen by the server.

### ComWin (the "Combat" window)

`src/haven/ComWin.java` is an `HWindow` titled `"Combat"` with two labeled sections, `"Attack:"` and `"Maneuver:"`:

- **Attack section**: shows `fv.current_attack` (icon from the resource's `imgc` layer, name from its `Resource.action` layer, i.e. `Resource.AButton.name`). When `fv.next_attack` is also set, its icon is drawn slightly offset on top of the current one - this is the queued attack. The attack names shown here ("Punch", "Sting", ...) come from server-defined action resources, so the server controls the naming.
- **Maneuver section**: shows `fv.maneuver` icon and `AButton` name.
- **IP meter**: the icon `gfx/hud/combat/ip` with the literal text `"<initiative_points_self>/<initiative_points_other>"` of the current relation. IP is a plain integer counter, not a 0-100 meter.
- **Attack cooldown bar**: while `now < fv.next_attack_time` a pink bar of width `(next_attack_time - now) / 100` pixels is drawn at `(200, 55)` - exactly 10 pixels per second of remaining cooldown. The cooldown is entirely client-visual: the server sends `atkc` with a tick count and is expected to enforce the real cooldown itself.

### Character meters relevant to combat

- `src/haven/IMeter.java` (widget type `im`): created with a background-resource name (the meter's identity, e.g. the health or energy meter art) and a list of `(color, value)` pairs with `value` in `0..100`; updated via the `set` uimsg and given a server-authored tooltip via `tt`. The union fork's bot layer identifies meters by substrings of the background resource name: `nrj` (energy/stamina), `hp`, `hngr` (hunger), `happy`, `auth` (src/union/JSBotUtils.java, `updateMeters`). The health meter's tooltip is parsed by that code as three numbers, which the bot labels SHP (soft HP, current), HHP (hard HP, maximum) and a percentage. So the server must (a) create the standard meters at login, and (b) refresh them via `set` whenever values change, including after combat damage.
- `src/haven/GobHealth.java` + `src/haven/OCache.java` (`health(int id, int frame, int hp)`, object-delta attribute `OD_HEALTH = 14` in src/haven/Session.java): other characters' health is a coarse 0..4 quarter scale (`asfloat()` returns `hp / 4.0`); the sprite is tinted increasingly red as hp decreases (`HpFx` blends toward red, `128 - (hp * 128) / 4`). This is what bystanders see over a fighting character; exact HP values stay server-side.

## The combat state model

This section maps the wiki terminology (Legacy:Combat_Actions) onto the wire fields above.

### Offence and defence bars ("openings")

- Every relation has two bar pairs (your off/def toward the opponent, mirrored). Your offence is consumed by your attacks; your defence is consumed by enemy attacks.
- "Using an attack will fully deplete the attack bar. The attacker must deplete the defense bar of the target before actual damage is inflicted on their HP." (Legacy:Combat_Actions). The defence consumed by an attack is given by the wiki twice, with slightly different phrasing: as `AW/BW/2` where `AW` is your attack's weight and `BW` the enemy maneuver's weight, and as `a*wA / wB*2` where `a` is the amount of offence you have (the red attack bar), `wA` the weight of your attack and `wB` the weight of the opponent's maneuver. Read together: *defence consumed = (offence spent x attack weight) / (opponent maneuver weight x 2)*, and "Your attack weight is multiplied by from 0.5 to 2.0, depending on the combat advantage" (all per Legacy:Combat_Actions). The exact parenthesization of the wiki formula is ambiguous - see Open questions.
- Damage therefore requires a pre-existing opening; a hit with no openings deals nothing ("if you get hit with no preexisting openings the damage will still be zero for that hit", H&H forum "RFC: Hunting Exploits", January 2019).
- "After attacking, you will return to using the move you were using before attacking" (Legacy:Combat_Actions) - i.e. attacks are transient overlays on the currently selected move, which matches the client's `current_attack` / `next_attack` (queued) model in `ComWin`.

### Balance (advantage)

`balance` is the integer advantage dial (-5..+5 on the client dial). Advantage feeds back into attack weight (x0.5 to x2.0), so a fighter who builds advantage hits proportionally harder through defence. Moves like `Seize The Day!` (+0.3 advantage), `Sidestep` (+1), `Invocation of Skuld` (+1), `Battle Cry` (+2) and `Cleave` (requires >= 3 advantage) manipulate or gate on it (per Legacy:Combat_Actions). The mapping from these fractional accumulations to the integer wire value is server-side and not documented in available sources (Open questions).

### Intensity

`intensity` is an integer that ramps up during a fight (raised by e.g. `Fan the Flames` +5, `Opportunity Knocks` +1; consumed by `Sting Like A Bee` and `Consume the Flames`, whose effects scale as `+10% * intensity attack` and `-1% * intensity SHP` respectively; `Battle Cry` requires intensity 10). Several cooldowns are modified by intensity ("only 2.0 seconds at 10 Intensity" for `Seize The Day!`, per Legacy:Combat_Actions). The client only displays it; all accrual/decay rules are server-side.

### Initiative points (IP)

IP is the fight's fuel economy: `initiative_points_self` / `initiative_points_other` per relation. Moves generate IP (`Charge!` +1, `Dash!` +1, `Call Down The Thunder` +1, `Feign Flight` +2, `To Arms!` +1*delta to the whole party), attacks and special moves spend it (`Sting` 2, `Chop` 4, `Sidestep` 4, `Opportunity Knocks` 5, `Knock His Teeth Out!` 6, `Valorous Strike` 6, `Battle Cry` 7, `Cleave` 8, `Invocation of Skuld` costs 3 but requires 10), and some effects steal or donate IP between fighters (`Throw Sand` -2 IP to opponent, `Float Like A Butterfly` +1 IP to opponent, `Valorous Strike` +2 IP to opponent). `Battle Cry` needs "at least 14 IP" according to its wiki note despite a 7 IP cost, implying IP can exceed visible thresholds; an IP cap, if any, is unknown (Open questions). IP is per-engagement state (it lives on the relation), so it should be reset when an engagement ends.

### The give handshake

Each relation carries the two-bit `give` state (see above). Legacy community practice treats the give buttons as the peaceful way to wind down a fight; a Rust server should at minimum model: client toggles a bit via `give`, server echoes via `new`/`upd`, and some combination (e.g. both bits set, or both fighters' toward-bits set) ends the engagement cleanly. The exact legacy rule is unverifiable from the client alone (Open questions).

## Damage formula (legacy)

The following is transcribed from *Legacy:Combat_Actions* (Ring of Brodgar wiki, Legacy namespace, revision 2021-08-20, cached at research-cache/page-legacy-combat-acts.md). All numbers in this section are the wiki's, not independently verified.

> Damage can be calculated by the following formula: `damage = basedamage * ql * str / 10`, where `ql` is the quality of the weapon, `str` is the strength of the hearthling, `basedamage` is the base damage of the weapon.

Worked example given by the wiki: a Soldier's Sword has base damage 400; wielded at quality 65 by a character with 319 Strength it deals `400 * 65 * 319 / 10 = 1517` damage.

> You can adjust for Personal Beliefs (Peaceful vs. Martial) by multiplying the result by 0.8 or 1.2 (or anywhere in between, depending on the Peaceful/Martial slider).

Unarmed attacks ignore weapon quality and the beliefs multiplier (the wiki lists Punch, Knock His Teeth Out! and Strangle as "Doesn't take into account: Personal Beliefs, hand weapons (cutthroat knuckles, soldier's gauntlet)"):

- **Punch**: `0.75 * 50 * sqrt(Strength / 10)`.
- **Knock His Teeth Out!**: `100 * sqrt(Strength / 10)` ("Twice the damage of Punch").
- **Strangle**: `0.75 * 50 * sqrt(Strength * 1.5 / 10)` ("Stronger than Punch, but has almost twice the cooldown time").

Implementation notes for a Rust server:

- Keep `basedamage` per weapon as resource data, quality on the item instance (the client carries `Item.quality` and the effective-quality override `q2`, `Item.get_quality()` returning `q2 > 0 ? q2 : quality`, src/haven/Item.java; see `../items/items-and-quality.md`), and Strength on the character (FEP-driven attribute; see `../character/attributes-and-vitals.md`).
- The beliefs slider is character state (`Personal Beliefs` page exists in `src/haven/CharWnd.java`'s character window with a `belief` pane); the 0.8..1.2 multiplier is applied server-side at damage time.
- Integer vs. floating-point division order (`/10` last) affects results; the wiki's example is consistent with exact integer arithmetic in the shown order, but the original server's arithmetic is unknown (Open questions).

## Attacks

Per Legacy:Combat_Actions: "Attacks are moves that inflict damage on your opponent. Using an attack will fully deplete the attack bar." Attacks require IP (except Punch), consume the defender's defence bar via the weight formula, and after the attack you revert to your previously selected move. All values below are the wiki's.

| Attack | Hotkey | Cooldown (wiki) | IP cost | Weight | Damage | Required skill | Notes (wiki) |
|---|---|---|---|---|---|---|---|
| Punch | P | 9.6 s | none | UnarmedCombat | `0.75 * 50 * sqrt(Str/10)` | Unarmed Combat | The most basic attack, as it doesn't require any IP |
| Knock His Teeth Out! | K | 7.2 s | 6 | UnarmedCombat | `100 * sqrt(Str/10)` | Brawling | Twice the damage of Punch |
| Strangle | G | 14.4 s | none | UnarmedCombat * 0.8 | `0.75 * 50 * sqrt(Str*1.5/10)` | Unarmed Combat (summary table says Brawling) | Stronger than Punch, nearly double cooldown |
| Sting | S | 12.0 s | 2 | MeleeCombat | main formula | Swordsmanship | Basic sword attack |
| Valorous Strike | V | 24.0 s | 6 | MeleeCombat * 1.75 | main formula | Swordsmanship | Resets Advantage to 0; gives +2 IP to opponent; "almost twice your MC as attack weight"; useful when the enemy has a lot of Advantage |
| Chop | C | 18.0 s | 4 | MeleeCombat | main formula | Militia Training | Basic axe attack |
| Cleave | L | 24.0 s | 8 | MeleeCombat * 1.5 | main formula | Militia Training | Requires >= 3 Advantage |

The wiki's own notes flag that "many of the cooldowns listed with the actions are incorrect" and link a forum thread with a partial correction list (viewtopic.php?f=2&t=23357); treat every cooldown as approximate. The client learns the chosen attack's cooldown only through the `atkc` tick message (src/haven/Fightview.java), so a reimplementation can simply keep its own corrected table.

Skill gating is server-side: the client has no knowledge of which attacks a character owns; the server decides which action pages to send (`RMSG_PAGINAE`) and must reject `act` commands for skills the character lacks.

## Maneuvers

Per Legacy:Combat_Actions: "Maneuvers are patterns for responding to an attack. Your selected maneuver determines how effective the attacks are on you, and have an effect every time the enemy uses a move." The client's single `maneuver` slot (`blk` message, rendered as "Maneuver:" in `ComWin`) holds exactly one maneuver at a time.

Most maneuvers use `delta = sqrt(Your UA / Their UA)` where UA is the Unarmed Combat skill of you and your opponent. "The weight of a maneuver is how much defensive weight you have when attacked while using that maneuver" - this is the `wB` of the defence-consumption formula.

| Maneuver | Hotkey | Weight | Effect | Required skill |
|---|---|---|---|---|
| Bloodlust | B | 0.5 * Unarmed Combat | +10% attack * delta | Warrior Spirit |
| Combat Meditation | M | Unarmed Combat | -0.2 Combat Advantage; temporary effect reducing your move cooldowns by 25% * delta | Fire & Ice |
| Death or Glory | G | Unarmed Combat | -10% defense, +delta Initiative | Valor |
| Dodge | D | Unarmed Combat | none | Unarmed Combat |
| Oak Stance | O | Unarmed Combat | -10% attack, +5% * delta defense; stops giving defense when you have no attack left | Tales by the Hearth |
| Shield | S | 1.25 * Melee Combat | none (weight is the value) | Militia Training; requires a shield equipped |

Note that *Dodge* and *Shield* exist purely through their weight (higher weight = less defence consumed by enemy attacks), which is why the wiki lists their effect as "none".

## Moves

Per Legacy:Combat_Actions: "Moves are actions in combat that have some effect on the battle, generally raising or lowering your attack or defense. Once a move has been performed on an opponent, you will continue using that move as long as the conditions are met." The move (unlike an attack) is persistent - it is what you fall back to after each attack.

| Move | Hotkey | Cooldown (wiki) | Requirements | Effects | Required skill |
|---|---|---|---|---|---|
| Charge! | H | 5.4 s | none | +1 IP, +3% Attack, -8% Defense | Brawling |
| Call Down The Thunder | T | 10.8 s | none (usable up to ~25 tiles from target) | +1 IP | Leadership |
| Dash! | A (summary table: "H (?)") | 10.8 s | none | +1 IP, +6% Defense | Valor |
| Flex | X | 4.32 s | must have 6 IP | +20% Attack, -0.5 Advantage | Warrior Spirit |
| Jump! | J | 4.32 s | none | +12% Attack | Unarmed Combat |
| Slide! | D | 7.56 s | none | -15% Attack, +15% Defense; if attack < 15%, defense gained equals attack remaining | Unarmed Combat |
| Float Like A Butterfly | B | 10.8 s | none | +7.5% Defense, +0.3 Advantage, +1 IP to opponent | Warrior Spirit |
| Push The Advantage | U | 10.0 s | +3 Advantage | +1 IP, -5% Stamina | Warrior Spirit (summary table: Soldier Training) |
| Seize The Day! | Z | 3.0 s (2.0 s at 10 Intensity, before Agility modifiers) | +5 Intensity and >= 75% Attack | -5% Defense, +0.3 Advantage, -5% Attack to opponent | Valor |
| Throw Sand | R | 17.0 s (wiki: "need confirm") | 1 IP | -40% Attack, -2 IP to opponent (the wiki marks only the IP effect as "to opponent"; the target of the attack reduction is not specified in the source) | Tricks & Ruses |
| Feign Flight | F | 5.4 s (wiki note says 12 s in text) | <= 10% Defense | +2 IP | Tricks & Ruses |

Observed design intent (useful for balancing a reimplementation): `Charge!` is the fast-but-risky IP engine, `Dash!` the slow defensive one, `Slide!` converts offence into defence, and the advantage moves (`Seize The Day!`, `Flex`, `Float Like A Butterfly`) trade one resource for advantage. The wiki explicitly calls `Float Like A Butterfly`, `Push The Advantage`, `Throw Sand` and `Feign Flight` weak for their costs.

## Special moves

Per Legacy:Combat_Actions: "Special Moves are actions that are performed once upon activation in order to gain some specific advantage in a fight by manipulating one or more of the fight variables." Unlike moves they do not persist.

| Special move | Hotkey | Cooldown (wiki) | Requirements | Cost | Effects | Required skill |
|---|---|---|---|---|---|---|
| Battle Cry | Y | 1.5 s at 10 Intensity | 7 IP, 10 Intensity (note: "You must have at least 14 IP to use this move!") | 7 IP | +100% Attack, -50% Defense, +2 Advantage | Tales by the Hearth |
| Consume the Flames | N | 5.0 s (unaffected by Intensity) | none | none | -1% * Intensity SHP damage to self, +1% Stamina restored to self, resets Intensity to 0 | Fire & Ice |
| Fan the Flames | N | 12.5 s (affected by Intensity before use) | none | none | +5 Intensity, -20% Stamina | Fire & Ice |
| Evil Eye | V | 24.0 s | none | 2 IP | +100% Attack to opponent (resets the opponent's attack), -2 Intensity | Tricks & Ruses |
| Invocation of Skuld | K | 8.0 s | 10 IP | 3 IP | +20% Attack, +1 Advantage | Tales by the Hearth |
| No Pain, No Gain | A | 15.0 s | none | none | -50% SHP to self, +50% Attack, -25% Defense to opponent | Soldier Training |
| Opportunity Knocks | O | 9.0 s | none | 5 IP | -30% Defense to opponent, +1 Intensity | Soldier Training |
| Sidestep | I | 10.0 s | none | 4 IP | +1 Advantage | Brawling |
| Sting Like A Bee | E | 5.0 s (unaffected by Intensity) | none | 6 IP | +10% * Intensity Attack to self, -8% Defense to self, resets Intensity to 0; usable in taming | Warrior Spirit |

The wiki notes an in-game tooltip bug for Battle Cry ("incorrectly states that it removes 50% of your opponent's Defense" - it is your own defence that is halved) and recommends `Invocation of Skuld` over `Sidestep`.

## Party and ranged combat actions

These actions reach beyond the current duel window and interact with the party system (`src/haven/Party.java`, `src/haven/Partyview.java`):

- **Stern Order** (S, ~15 s cooldown, 6 IP, Leadership): +1 * delta Advantage to all party members against the target; party leader only ("you can always use it if you're not in a party"); `delta` is computed from your Charisma versus the target's Charisma.
- **To Arms!** (T, 10 s cooldown, 3 IP, Leadership): +1 * delta IP to all party members against the target; same leader-only rule and Charisma-based delta.
- **Call Down The Thunder** (T, 10.8 s, +1 IP, Leadership): gains IP from a distance of "up to ~25 tiles from the target".

Other actions outside the duel economy:

- **Quell the Beast** (Q, Animal Husbandry): taming action usable against domesticatable animals; requires Combat Advantage >= 3, Combat Intensity 0, costs 2 initiative, and a rope. On success the animal is attached to the rope and follows, gaining tameness; at 100 tameness it becomes domesticated.
- **Shoot** (H, Hunting/Archery): bow+arrow or sling+stones. "Firing with a bow will deplete your attack meter, but the chance of success only depends on the accuracy meter" (Legacy:Combat_Actions). This is the one documented case of a ranged hit-chance meter separate from the openings bars.

### Implemented ranged model (server, session 37)

The Shoot action is live for bow-equipped players against local animals (`server/crates/hnh-server/src/archery.rs`):

- **Engagement**: clicking an animal while a `gfx/invobjs/bow` stack sits in any equipment slot opens the ranged path INSTEAD of the frv duel (a dry bow refuses with a chat line and never falls back to melee while equipped). A ground click cancels the aim; a melee click without a bow opens the duel as before.
- **Accuracy meter**: fills at 250/10000 per 100 ms combat tick (full aim in 4 s for the Wooden Bow; the RoB note "a Ranger's Bow aims at half the speed of a Wooden Bow" is a data row in `BOWS`, not code). Progress lines stream to chat at 25/50/75%.
- **Range**: shots land up to 132 units (~12 tiles); inside 300 units but beyond bow range the archer closes in and keeps the aim, beyond 300 the aim drops (same disengage radius as melee).
- **Release**: automatic at a full meter. One arrow (`gfx/invobjs/arrow-stone` or `arrow-bone`) is consumed per shot, hit or miss; the frv offence bar (attack meter) is zeroed per the documented rule; stamina -2.
- **Hit chance** (server policy - the legacy formula is not recoverable, see Open questions): `95 - 55 * (dist / 132) + min(20, Marksmanship/5)` percent, clamped 15..99 - point-blank 95%, 40% at max range for an unskilled archer.
- **Damage**: `75 * sqrt(q_bow / 10)` (Fandom Bow page - the same k*sqrt(x/10) shape as the unarmed maneuvers). A q10 Wooden Bow hits for 75 (a one-shot on Deer/Fox), q40 for 150. Arrows bypass the openings economy entirely (no defence-bar chip): the hit roll IS the resolution, per the accuracy-meter rule.
- **Aftermath**: the aim re-arms automatically while the target lives and arrows remain; a kill runs the standard death flow (loot drop, +10 LP, fight teardown).
- **Cross-node**: guest animals on other nodes are fully shootable - the aim/meter state stays on the shooter's node (session state), the hit roll happens there, and a `RelayAttack` with `chip = 0` carries the damage to the animal's authority, whose `relay_swing` applies it WITHOUT the openings gate (the marker semantics: chip > 0 = melee swing through the defence bar, chip = 0 = ranged, openings bypassed). Death runs the owner's relayed death flow (GuestRetract tears the aim down on the shooter's node).
- **Player-versus-player archery (server, session 38)**: bow carriers can aim at and shoot OTHER PLAYERS. Clicking a player while a bow is equipped opens the ranged aim instead of the party-invite menu (a self-click never aims; without a bow the click opens the party flower menu whose Fight petal arms the melee duel - see the melee PvP section below). The accuracy meter, range, chase, arrow economy and the hit roll are identical to the animal model above. A hit applies `75 * sqrt(q/10)` through the victim's armor absorption (`hurt_player`: armor.rs `dmg * K / (K + abs_total)`); both sides get a chat line and the victim's avatar plays `gfx/fx/hit`. A lethal arrow knocks the victim out (HP resets to the 50 floor, energy -10, fight state tears down) and the shooter's chat reports the defeat. Cross-node shots split authority exactly like the animal-bite flow: the hit roll stays on the shooter's node, a `PvpArrow { victim, attacker, dmg }` rides the node mesh to the VICTIM's home node (derived from the gob id's slot range), which owns armor/HP/knockout; the outcome answer `PvpArrowResult { shooter, killed }` streams back for the shooter's chat. The aim re-arms while the victim lives and arrows remain.

### Implemented melee PvP model (server, session 39)

The unarmed openings duel is live between two players, locally and across nodes (`game.rs` `start_pvp_melee` + the `Kind::Player` branch of `tick_combat`):

- **Engagement**: clicking another player with no bow opens the party flower menu, which now carries a **Fight** petal ("Invite to party" / "Fight" / "Cancel"; the invite petal drops out when it does not apply, e.g. the target is already partied or the clicker's party is full - the duel is never gated by party state). A cross-node GUEST player click opens a Fight-only menu (party membership has no cross-node relay). Confirming Fight arms the attacker (`fight_target`), drops any live ranged aim (melee and ranged are exclusive player state), and opens the frv fight window on BOTH sides: the victim immediately sees a relation on the attacker and can answer through the fight-window select (the frv `click` wdgmsg) without hunting for the flower menu - matching the legacy Fightview behavior of a two-sided duel. Both players get a chat line ("You attack <name>!" / "<name> attacks you!").
- **Openings economy**: identical arithmetic to the animal duel, but the chipped bar is the VICTIM'S SESSION defence bar (`FightState::own_def`) instead of an `animal_fights` row. Swings spend half the attacker's offence bar (`SWING_SPEND`), respect the `atkc` cooldown, chip `SWING_DEF_DMG * weight` (weight scales 0.5..2.0 with the relation's balance), and only an opening (bar at/below `OPENING_THRESHOLD`) passes damage to HP: `(5 * str / 10).max(1)`, applied through the victim's armor absorption. A landed hit resets the victim's defence bar to full - the same reset-on-break policy the animal bites use. Each swing costs 2 stamina and accrues IP on both relations (ip_self / ip_other).
- **Fresh-session defence**: `FightState::new()` starts `own_def` at FULL (the derive-Default 0 used to open every fresh player to instant damage - a latent bug found and fixed this session; animal bites now also WRITE the chipped bar back instead of dropping the local value).
- **Knockout**: a lethal swing drops the victim to the 50 HP floor, energy -10, clears their fight state (relations, bars, fight_target), tears down the attacker's duel, and chats "You have defeated your target!" on the attacker's side. Players never die as gobs.
- **Cross-node authority split** (the same split as PvpArrow): the attacker's node owns the aim bars, the frv window and the swing pacing; each swing ships one `PvpSwing { attacker, victim, chip, dmg }` to the VICTIM'S home node (`node_of_gob`), which owns the authoritative defence bar, armor, HP and the knockout path. The home node answers `PvpSwingResult { attacker, victim, def, landed, killed }`, and the attacker's node re-syncs its `guest_fights` mirror and the frv relation view from the answer (a lost frame self-heals on the next swing, exactly like the animal FightBars loop). The mirror also chips locally with the same arithmetic for UI prediction. Guest retraction (the victim walks out of view) closes the duel through the common guest-fight teardown path.

### Melee weapons (server, session 40)

`fight.rs` carries the weapon base-damage table and every swing path reads it through `Game::melee_dmg` (local PvP, animal fights, and the cross-node relays - the chip-0/cross-node swings ship the weapon number the same way):

- **Formula (server policy)**: `dmg = base * sqrt(q/10) * (str/10)` - the same QM sqrt scaling the armor and bow systems already use. The RoB linear formula `basedamage * ql * str / 10` does not reproduce its own worked example (see items-and-quality.md Open questions), so the pack-consistent sqrt model was chosen and is marked as policy.
- **Table**: the stone axe (`gfx/invobjs/axe`, the pack's craftable melee weapon) sits at base 15 - three unarmed blows at q10/str10, still far under the bow's 75. New weapons are one table row each.
- **Unarmed fallback**: no weapon in any of the 16 equipment slots keeps the legacy strength-only `(5 * str / 10).max(1)` (Punch family).
- **Slot policy**: the FIRST weapon found scanning the equipment slots wins (hand items live at slots 3/4; slot addressing is server-side policy per items-and-quality.md).

### PvP knockout consequences (server, session 40)

Legacy documents only the DEATH penalties (25-75% through the Tradition/Change slider); the knockout share was an open question. This server's written policy (all paths - local melee, arrows, and the relay authority split):

- **The loser** forfeits **10% of unused LP** (floor 0), applied on the victim's home node; a chat line reports the loss.
- **The winner** is flagged **CRIMINAL (assault) for 30 real minutes** (`CRIMINAL_MS`), refreshed by every new knockout. The flag is a live buff on the reliable stream: `RMSG_BUFF set` id 1 (real ids start at 1; the client's pseudo-buffs own -1..-3), icon `gfx/hud/buffs/thorn`, tooltip "Criminal (assault)", a countdown (`cmeter`/`cticks` in legacy 1/60 s ticks), and it re-streams on world entry (the client Glob is rebuilt per login). Expiry sweeps in the tick: `RMSG_BUFF rm` plus a chat line. The flag persists through the v5 save format (`criminal_until_ms`).

### Maneuver economy (server, session 40)

The `paginae/atk/*` buttons are live: world entry announces the root page plus every table entry, and MenuGrid sends `act("atk", id)` (verified against the pack's action layers - every ad pair is `["atk", <id>]`). `fight.rs` `MANEUVERS` (28 entries) and `game.rs` `on_maneuver`:

- **Attack selections** fill the two-slot queue the client renders (frv `atk [cur, next]`: the previous current slides into `next`, the selection becomes `cur`; -1 renders an empty slot). **Dodge** sets the stance slot (frv `blk [res]`). **Boosts** are pure IP/advantage plays.
- **IP economy** runs on the relation: cost from `ip_self`, gains to `ip_self`, and opponent deltas (`Throw Sand` -2, `Float Like A Butterfly` +1, `Valorous Strike` +2) apply to a LOCAL victim's own pool and stream both windows; a GUEST's authoritative pool stays on their home node, and the delta now crosses the mesh as `ManeuverDelta { attacker, victim, ip_opp }` (session 42): the victim's home node folds it into her relation row keyed by the attacker's guest gob (clamped at zero) and re-streams her window. The attacker's `ip_other` mirror remains the between-frames prediction.
- **Advantage** accumulates in TENTHS on the relation (`adv`, -50..+50) - fractional legacy gains like Seize The Day! +0.3 need the sub-integer source - and `sync_balance` rounds/clamps it to the wire dial (-5..+5). Advantage feeds the existing attack weight (x0.5..x2.0).
- **Gating** (documented RoB numbers): Cleave needs >= 3 advantage; Battle Cry needs >= 14 IP; Invocation of Skuld needs >= 10 IP; every cost is checked against the current relation before anything mutates. Refusals chat the reason and change nothing.
- All 28 IP costs/gains/advantage values with a legacy source carry it in the table comments (Sting 2, Chop 4, Sidestep 4/+1, Opportunity Knocks 5, Knock His Teeth Out! 6, Valorous Strike 6/+2 opp, Battle Cry 7/+2/needs 14, Cleave 8/needs 3 adv, Skuld 3/+1/needs 10, Charge! +1, Feign Flight +2, Throw Sand -2 opp, Butterfly +1 opp, Seize +0.3); the rest are this server's policy (see Open questions).

## How moves beat moves: the counterplay system

The task of describing legacy combat as "rock-paper-scissors" resolves, in the documented sources, into a *weight contest plus advantage feedback loop* rather than a directional high/low or slash/chain/blunt triangle:

1. Every attack has a weight (mostly a skill-derived base, sometimes multiplied: Valorous Strike 1.75x, Cleave 1.5x melee weight) and every maneuver has a defensive weight (Dodge = raw Unarmed Combat, Shield = 1.25x Melee Combat, Oak Stance = raw Unarmed Combat with a defensive bonus effect).
2. Defence consumed per attack = (offence spent x attack weight) / (defender maneuver weight x 2), with attack weight further multiplied x0.5..x2.0 by combat advantage (Legacy:Combat_Actions).
3. Advantage (balance) is built by advantage moves and spent by big attacks (Valorous Strike resets it to 0; Cleave requires >= 3), so a losing defender must first stabilize defence (maneuver with high weight, Slide!, Dash!) before rebuilding offence (Charge!, Jump!).
4. IP is the shared tempo currency: both fighters generate and spend it, and some effects transfer it between them.

No legacy source in the research cache documents the early H&H directional triangle (high/low lines, slash/chain/blunt damage types) as part of the legacy fight window; if the original server had per-limb or per-direction targeting, it is not visible in this client. See Open questions.

## Weapon types and quality

Documented weapon/attack families and their client-visible differences:

- **Unarmed** (fists) and **hand weapons** (cutthroat knuckles, soldier's gauntlet - the wiki notes unarmed attacks do not take hand weapons into account in its listed formulas): strength-based formulas, no IP cost for Punch.
- **Swords** (Sting, Valorous Strike; Swordsmanship skill).
- **Axes** (Chop, Cleave; Militia Training skill).
- **Bows and slings** (Shoot; accuracy-meter hit resolution, attack meter depletion).
- **Rope** (required equipment for Quell the Beast, the taming attack).
- **Shields** (enable the Shield maneuver, weight 1.25 * Melee Combat).

What the server must plug in from the item layer:

- **Quality** enters the damage formula linearly (`damage = basedamage * ql * str / 10`). The client models quality as `Item.quality` plus an effective-quality override `q2` (`Item.get_quality()`, src/haven/Item.java) and parses the textual `quality (\d+)` suffix in item descriptions. Quality mechanics, quality transfer on crafting, and the `q2` semantics are detailed in `../items/items-and-quality.md`.
- **Equipped state**: the client's equipment window (widget type `epry`, src/haven/Equipory.java) has 16 generic slots rendered as 1x1 inventories; what fits in which slot, which item is the "weapon in hand", and whether a shield/maneuver requirement is met are all server decisions. The server pushes each slot as `set` messages `(resource_id, quality[, tooltip])` and later per-slot `setres`/`settt` updates.
- **Weapon base damage** (`basedamage`) is per-weapon resource data; the legacy wiki only documents the Soldier's Sword value (400). Other values belong in the items doc as they are sourced.

## Armor and protection

The legacy wiki pages available to this task document armor only qualitatively; no per-piece damage-reduction numbers were found (they belong in Open questions). What can be stated with source backing:

- Armor is worn in the 16-slot equipment window (src/haven/Equipory.java) and each equipped piece is described to the client by a server-authored tooltip.
- The equipment window computes and displays an aggregate **"Armor class: def/abs"** line: it parses each equipped item's tooltip for the pattern `Armor class: (\d+)/(\d+)` (src/haven/Equipory.java, `calcAC()`), sums the two columns over all pieces, and renders `"<def>/<abs> (<def+abs>)"`. The two numbers are server-defined semantics carried inside tooltips; the community reads them as a defensive value and an absorbing value respectively. The important protocol fact: *the server owns the numbers, the client merely sums the tooltip strings* - a reimplementation can encode whatever protection model it wants behind these tooltips as long as the `Armor class: X/Y` text is present.
- Armor pieces reducing incoming damage is the community-documented behavior (Legacy:Combat_Actions describes attacks consuming defence before HP; armor enters the un-documented step between "defence consumed" and "HP lost"). Exact legacy reduction formulas are unknown.
- Armor quality presumably matters like weapon quality, but no legacy source in the cache states it for armor specifically.

## Evasion and hit chance

For melee duels, no hit-roll exists in the documented model: the attack always "hits" but its damage is gated by the openings system (defence bar consumption first; zero damage when no openings exist, per the forum sources cited above). The only documented hit-chance mechanic is the bow/sling **accuracy meter** ("the chance of success only depends on the accuracy meter", Legacy:Combat_Actions), whose client-side rendering is not present in this codebase's fight widgets and is presumably an attribute-driven meter server-side. If the original game had a dodge probability beyond maneuver weights, it is not documented in available sources (Open questions).

## Aggro initiation and criminality

- **Starting a fight**: the player targets someone by clicking their gob in the world (`src/haven/MapView.java` sends the `click` widget message with the clicked object id and offset to the server) and picks an interaction option from the flower menu; menu options are server-defined data (`src/haven/FlowerMenu.java` merely renders a `pop` of options pushed by the server). The server therefore decides when an attack action escalates into an engagement and then opens the `frv` fight window. Client-side, the fight window appearing is the aggro signal (src/union/JSBotUtils.java).
- **Combat actions as menu commands**: all attacks, maneuvers, moves and special moves are action resources delivered as paginae (`RMSG_PAGINAE`, src/haven/Session.java) and displayed in the menu grid (`src/haven/MenuGrid.java`). Each carries an `AButton` with a display name, a parent resource (menu nesting - the wiki's summary lists them under `M > K` attacks, `M > M` maneuvers, `M > O` moves, `M > S` special moves), a hotkey character (`AButton.hk`, e.g. P, K, G, S, V, C, L) and the server command array `AButton.ad`. Activating one sends `act` with the ad array to the server (src/haven/MenuGrid.java, `wdgmsg("act", (Object[]) actions)`).
- **Criminal acts toggle**: attacking a player who is not a valid target is a *criminal act*. The client models this as a special always-available buff: `MenuGrid.use` special-cases `ad[0] == "crime"` and toggles a client-side `Buff` with id `-1` (also `-2` for `tracking`, `-3` for `swim`), while still forwarding the `act` command to the server (src/haven/MenuGrid.java). The fork auto-activates `paginae/act/crime` on login when configured (src/haven/CharWnd.java, around the `Config.toggleCA` check). So the server sees the toggle as an `act` command and must track criminal-acts state per character.
- **Consequences**: interacting with village property without membership requires the criminal-acts state and generates *scents* (Fandom wiki "Village"); the same criminal framing governs attacking and looting (see Death section). Reputation and its relationship to attributes are covered in `../character/attributes-and-vitals.md`.
- **Kin colors**: fight-window rows and names are colored by the opponent's buddy-group via `KinInfo` and the `BuddyWnd.gc` palette (8 groups, from white/green/red to orange; src/haven/BuddyWnd.java, src/haven/Fightview.java `Relation.color()`), letting players see at a glance whether an opponent is kin, village-mate or stranger.

## Combat buffs

The buff channel is the server's general-purpose mechanism for timed combat effects (and much else):

- Wire: session message `RMSG_BUFF` (value 12, src/haven/Message.java) handled by `Glob.buffmsg` (src/haven/Glob.java): `clear` (wipe all), `set` (`id:int32, res_id:uint16, tooltip:string, ameter:int32, nmeter:int32, cmeter:int32, cticks:int32, major:uint8`), `rm` (`id`). Buffs live in `Glob.buffs` (a `TreeMap<Integer, Buff>`).
- Semantics per `src/haven/Buff.java` and `src/haven/Bufflist.java`:
  - `ameter`: optional 0..100 progress bar under the buff icon (rendered only for `major` buffs, of which the client shows up to 5).
  - `nmeter`: optional numeric overlay on the icon (e.g. a stack count).
  - `cmeter` + `cticks`: optional countdown; `cmeter` is a 0..100 percentage and `cticks` a remaining time in 1/60-second ticks (the client computes `total_seconds = cticks * 0.06` and drains the percentage against wall-clock time - see `Buff.GetTimeLeft` and the pie-slice rendering in `Bufflist.draw`). This is the second independent confirmation of the 60 ms tick.
  - `major`: whether the buff appears in the main buff bar vs. hidden bookkeeping.
  - `tooltip`: free server text shown on hover (with computed percent/time suffixes).
- Client-side pseudo-buffs with fixed negative ids (`crime` = -1, `tracking` = -2, `swim` = -3) are toggled in `src/haven/MenuGrid.java`; the server should avoid colliding real buff ids with these.
- Combat-relevant buffs pushed by the server in legacy play (per community documentation) include maneuver effects like Combat Meditation's cooldown reduction and Belief-derived effects; a Rust server should model buffs as (resource, tooltip, ameter, nmeter, cmeter/ticks, major) records exactly as above.

## Running away and disengagement

Documented and inferred behavior:

- The official forum thread "Running Away in Combat" (June 2013, cited in research-cache/combat.json) discusses that chasing a fleeing opponent is hard in this system and records the core rule proposal/observation that *a "moving" hit deals damage as if there were no defense bar, but does not actually deplete the defence bar* - i.e. hits on a fleeing target bypass the openings gate but do not create further openings. Whether this describes the shipped legacy server or a proposed patch is not fully clear from the snippet; treat it as the community's description of movement-hit behavior (Open questions).
- The `give` handshake (see above) is the deliberate disengagement path inside an engagement.
- `Call Down The Thunder` works at up to ~25 tiles (Legacy:Combat_Actions), which implies the server allows engagements to persist over long distances while most melee actions require adjacency; exact per-action range values are undocumented.
- Server responsibilities: decide when distance/movement breaks an engagement (remove the relation with `del`, destroy the fight window when the list empties), stop cooldowns, and preserve IP/balance only if the engagement resumes within some window (legacy behavior unknown - Open questions).

## Knockdown, death, and looting

Per *Legacy:Death* (Ring of Brodgar wiki, 2021-03-08 revision):

- "Your character dies when its Hard HP reaches 0. The finishing blow can be caused by A Thorn in the Foot, players with Murder, Creatures, Leeches, or Swimming."
- Death is final: "After your character dies, it's dead for good; it cannot be revived. It will turn into a Skeleton." A new character can be made as the deceased one's descendant; "When your new character enters the fire in the creation screen, they will be returned to the dead character's hearth fire" - the hearth-fire respawn applies to the reincarnated successor, not to the dead character. To inherit, the new character must read the ancestor's Rune Stone in the character creation room.
- Reincarnation penalties follow the Tradition/Change slider: Full Tradition = 25% loss of attributes, skills and learning points; Even = 50%; Full Change = 75%. Quoting Jorb (via the wiki): attribute and skill values are reduced by the appropriate percentage; LP spent purchasing skills is refunded as unused LP reduced by the percentage ("Murder costs 100k LP. Reincarnation at neutral tradition/change value means your character will start the game with 50k unused LP if it had Murder skill."); unused LP is reduced by the percentage; "Claims are inherited, quite simply. Unreduced, that is."
- **Looting**: "Corpses are left behind which can be looted but require the theft skill unless you are the reincarnated character of them." Corpses eventually turn into skeletons, which drop all carried items without needing the theft skill (unless the drop happens on a claim whose protection applies).
- The soft-HP/hard-HP split (SHP regenerates and knocks the character out when depleted; HHP is the mortal pool) is what the health meter's three-number tooltip carries (src/union/JSBotUtils.java); exact knockdown semantics at SHP 0 for legacy (who may act on a downed character, whether combat ends, what the downed player may do) are not documented in the cached sources - Open questions.

Server responsibilities around death:

1. On HHP <= 0: finalize the character (persist death state), convert the body into a corpse gob holding an inventory (the loot window the killer sees is just that gob's inventory UI), schedule the corpse-to-skeleton decay, and drop items per the theft/claim rules when the skeleton decays.
2. Block the dead character from acting; allow the account to create the descendant whose spawn point is the ancestor's hearth fire.
3. Apply the Tradition/Change reductions to attributes, skills and LP at reincarnation (see `../character/attributes-and-vitals.md`).
4. Track the *Murder* skill ownership (the 100k LP skill that enables player-killing blows) and the theft-skill gating of corpse looting.

## Server implementation notes

This section is the normative summary for the Rust server implementation.

### Tick model

- The legacy combat timing unit is the **60 ms tick** (1/60 s). Evidence: `atkc` multiplies its argument by 60 ms (src/haven/Fightview.java), and buff countdowns interpret `cticks` as `cticks * 0.06` seconds (src/haven/Buff.java, src/haven/Bufflist.java).
- Recommended model: a fixed 60 ms combat scheduler per engagement that advances cooldowns, intensity/balance accrual, and any per-tick buff effects; all cooldown values stored in integer ticks, serialized to clients exactly like the legacy `atkc` payload.

### Engagement state machine (per pair of fighters)

```
IDLE -> ENGAGED            server opens/reuses the frv widget; sends Relation "new"
                           (gob_id, balance=0, intensity=0, give, ip=0/0, off, def)
                           and marks it current with "cur" if it is the active target
ENGAGED -> RESOLVING       client "act" selects moves/attacks/maneuvers; server
                           validates, applies costs, replies with "atk"/"blk"/"atkc"
                           and streams bar motion via "offdef" (own) and "updod"
                           (opponent view), plus "upd" for balance/intensity/IP/give
ENGAGED -> DISENGAGED      distance break, mutual give, or server rule:
                           send "del" for the relation; destroy the frv widget
                           when the relation list becomes empty
SHP depleted -> KNOCKDOWN  (legacy details undocumented; see Open questions)
HHP depleted -> DEATH      per Legacy:Death; corpse gob + loot + reincarnation hooks
```

### Move selection flow (client -> server)

1. Server sends paginae resources containing `AButton` layers (name, parent, hotkey, `ad` array) for the actions the character may use (`RMSG_PAGINAE`).
2. Player activates an action; `MenuGrid` sends `act` with the ad array (src/haven/MenuGrid.java). Player may also click an opponent (`Fightview` `click`) to change focus.
3. Server validates: skill owned (Swordsmanship for Sting, etc.), maneuver/attack requirements (Advantage >= 3 for Cleave, <= 10% defence for Feign Flight, Intensity 10 for Battle Cry), IP cost, cooldown availability, and equipment (shield for Shield maneuver, rope for Quell the Beast).
4. Server applies state changes, then notifies: `atk` (current + queued attack resources) when an attack is armed, `blk` for maneuver changes, `atkc` for the resulting cooldown in ticks, `upd`/`updod`/`offdef` for the numeric consequences.
5. The client never validates anything beyond message well-formedness; a hostile client can send arbitrary `act` arrays, so validation must be complete server-side.

### Openings resolution loop (server-side per attack)

1. Attacker's offence bar is drained fully by the attack (wiki rule).
2. Defence consumed from the defender = (offence spent x attack weight x advantage multiplier) / (defender maneuver weight x 2), advantage multiplier in 0.5..2.0 from balance (per Legacy:Combat_Actions, formula parenthesization uncertain).
3. If the defender's defence bar was already partially consumed, the overflow converts into damage via `damage = basedamage * ql * str / 10` (modified by Personal Beliefs x0.8..x1.2, and by the attack-specific unarmed formulas where applicable), then armor reduction is applied (formula unknown).
4. Persistent-move effects (the currently selected move) and maneuver passive effects apply every time the enemy uses a move.

### Meters and persistence

- Stream `offdef`/`updod` on bar changes (they are the high-frequency channel), `upd` for the slower state, and refresh the standard `IMeter`s (health/stamina) via `set` after damage so the HUD stays correct.
- **Equipment damage persistence**: weapons and armor take wear in legacy gameplay; the client shows whatever the server encodes in item tooltips (`settt`, src/haven/Equipory.java) and item quality (`Item.quality`/`q2`, src/haven/Item.java). The server must persist per-item damage/quality in its item store and push tooltip updates when values change; the exact legacy wear rates are undocumented (Open questions).
- Buffs pushed through `RMSG_BUFF` must use `cticks` for anything time-limited so the client's countdown rendering stays honest.

### Criminality and social integration

- Track the criminal-acts toggle per character (from `act`-with-`crime` commands; see src/haven/MenuGrid.java).
- Decide and record, per attack, whether it was a criminal act (target kinship, village membership, claim context - see `../character/attributes-and-vitals.md` and the Fandom "Village" page for claim/scent rules).
- Feed `KinInfo` group updates so the fight window colors opponents correctly (src/haven/KinInfo.java, src/haven/BuddyWnd.java `gc` palette).

## Open questions

Exact values and rules that could not be sourced, each with a suggested way to determine it:

1. **Defence-consumption formula parenthesization**: `AW/BW/2` vs `a*wA/wB*2`. Determine by packet-capturing a legacy server session (or asking Loftar/Jorb) while varying offence, attack weight and maneuver weight.
2. **Corrected cooldown table**: the wiki itself says many cooldowns are wrong and points to forum thread viewtopic.php?f=2&t=23357 (partial list by "Oddity"). Fetch that thread and diff against the tables above.
3. **Balance serialization**: how fractional advantage accumulations (e.g. +0.3 per Seize The Day!) map onto the integer `-5..+5` wire value. Capture `upd` messages during scripted fights.
4. **IP cap and regen**: whether IP has a maximum (Battle Cry's "14 IP" note hints thresholds above its 7 cost) and whether it decays when not fighting. Capture IP values over long fights.
5. **Knockdown semantics at SHP = 0**: whether legacy players are knocked down/prone, what a downed player can do, and whether engagements end. Sources to check: official forum threads around 2009-2013 and the current-world "Death is gone??" thread (November 2017) for the SHP/HHP split discussion.
6. **Movement-hit rule**: whether "moving" hits really ignore the defence bar without depleting it (forum quote above) was shipped behavior in legacy. Determine from the full "Running Away in Combat" thread.
7. **Armor numbers**: per-piece "Armor class" def/abs values for legacy armor sets, and the damage-reduction formula applying them. Determine from legacy item tooltips (client resource dumps or wiki equipment pages such as the individual Legacy armor/weapon pages on ringofbrodgar.com).
8. **Weapon base-damage table**: `basedamage` per weapon (only Soldier's Sword = 400 is documented). Source from legacy item resources.
9. **Evasion/dodge rolls**: whether any probability-based dodge exists beyond maneuver weights, and the accuracy-meter formula for bows/slings. Check the official forum's archery threads and any client resource with an accuracy meter.
10. **Give-button resolution rule**: the exact combination of give bits that ends an engagement, and whether giving has costs (IP/advantage). Infer from a legacy server or original client-server captures.
11. **Disengagement distances**: per-action range values (melee adjacency, Call Down The Thunder ~25 tiles) and the rule for breaking engagement by distance. Scripted packet capture recommended.
12. **Agility/Intensity cooldown modifiers**: the wiki explicitly asks for "the exact formula for the effect of combat intensity and/or Agility on the cooldowns". Unresolved in all cached sources.
13. **UnarmedCombat/MeleeCombat "weights"**: the exact numeric meaning of skill-derived weights (is weight = skill level? skill level scaled?). Community sources say "weight" tracks a skill value; confirm against captures.
14. **Contradicted wiki entries**: Strangle's skill (Unarmed Combat in the detailed entry vs Brawling in the summary table), Dash!'s hotkey (A vs "H (?)"), Push The Advantage's skill (Warrior Spirit vs Soldier Training). Resolve against in-game tooltips on a legacy server.
15. **Equipment wear rates**: how attacks degrade weapons/armor and how that interacts with quality (`q2`). Determine from legacy item tooltip diffs over time.
16. **Early directional combat**: the high/low, slash/chain/blunt "triangle" sometimes associated with H&H's oldest combat iterations is not documented in the legacy fight-window sources used here. If a reimplementation wants it, treat it as a design decision, not a legacy fact.
