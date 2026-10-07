# Animals and Husbandry (Legacy Haven & Hearth)

> **Sources:** src/haven/Session.java, src/haven/OCache.java, src/haven/Gob.java, src/haven/GobHealth.java, src/haven/Homing.java, src/haven/Following.java, src/haven/LinMove.java, src/haven/MapView.java, src/haven/Avatar.java, src/haven/Fightview.java, src/haven/FlowerMenu.java, src/haven/Glob.java, src/haven/MCache.java, etc/needed/fep.conf; Ring of Brodgar wiki (legacy server): Legacy:Creatures, Legacy:Animal_Husbandry, Legacy:Taming, Legacy:Cattle, Legacy:Sheep, Legacy:Chicken_Coop, Legacy:Food_Trough, Legacy:Hunting, Legacy:Deer, Legacy:Bee_Hive, Legacy:Rabid_Jackalope, Legacy:Gold_Egg, Legacy:Horses, Legacy:Combat_101, Legacy:Combat_Actions

## Summary

Every animal in Haven & Hearth - wildlife, tameable wild beasts, and domestic
livestock - is a server-owned gob. The client has no animal AI at all: it renders
whatever movement model the server streams (linear moves, homing chases, following
leashes) and whatever health value the server attaches (`OD_HEALTH`,
`src/haven/Session.java`). All decisions - aggro, flee, chase, wander, tame,
breed, eat, produce - are server-side. This document collects the legacy rules a
Rust server must implement: the creature roster with aggression classes and
butchering yields, the taming protocol ("Quell the Beast", tameness accumulation),
domestic breeding and production rates (milk, wool, eggs, honey), feeding, and the
exact client-facing wire contract for animal movement and health, citing the
client symbols that pin the wire format down.

## Creature roster and resource family

The client refers to animal gobs by resource names containing `kritter/...`
("kritter" is the creature resource family): `src/haven/Avatar.java` distinguishes
players from creatures by `rend.hasImage("kritter")`, and `src/haven/MapView.java`
explicitly matches resource names `kritter/boar` and `kritter/bear` when drawing
danger-radius overlays. The local resource pack (`res/compiled/`) contains only a
partial resource set (arch/hud/fx) and no `gfx/kritter` entries; the full legacy
resource pack is needed for exact per-animal resource names (see Open questions).

Legacy creature roster (Ring of Brodgar, Legacy:Creatures), with aggression and
behavior classes:

| Creature | Aggro conditions | Behavior class | Pickable | Tameable |
| --- | --- | --- | --- | --- |
| Ants | Attacked, or its Ant Hill raided | Passive (swarm defender) | No | No |
| Aurochs | Attacked; 10%+ chance when its hair is plucked; herd collective aggro | Passive, herd mentality | No | Yes |
| Bear | Attacked and proximity | Aggressive | No | No |
| Boar | Attacked and proximity | Aggressive | No | Yes |
| Chicken | None | Passive | Yes | No |
| Deer | Attacked | Passive | No | No |
| Fox | Attacked | Passive | No | No |
| Mouflon | Attacked; herd collective aggro | Passive, herd mentality | No | Yes |
| Rabbit | None | Passive | Yes | No |
| Rat | None | Passive | Yes | No |
| Toad | None | Passive | Yes | No |
| Troll | Attacked and proximity; suspected instant aggro on the player who caused its spawn | Aggressive | No | No |
| Cattle (Cow/Bull) | Cannot be aggroed | Passive | No | Bred, not tamed |
| Sheep | Cannot be aggroed | Passive | No | Bred from mouflon |
| Emerald Dragonfly | Cannot be aggroed | Passive | Yes (curiosity, LP) | No |
| Silkmoth | Cannot be aggroed | Passive | Yes (Silkworm Egg) | No |
| Dryad | None | Passive, non-combat | No | No |

"Pickable" animals (chicken, rabbit, rat, toad) can be lifted into inventory
(2x2 slots for chicken/rabbit, 1x1 for rat/toad per Legacy:Hunting) and killed
there ("wring neck" flower-menu option for chicken/rabbit; rats and toads must be
struck). Herd mentality: attacking one mouflon or aurochs makes the whole herd
aggro the attacker. A missed opening shot still aggros the target (Legacy:Hunting),
so aggro evaluation must run on any offensive action, not only on hits.

## Aggro rules

Documented legacy aggro triggers:

1. Being attacked (universal; includes missed ranged shots).
2. Proximity for aggressive species: bears and boars attack anything close to them,
   even while already in combat with someone else (Legacy:Hunting). The union-fork
   client draws an aggro/leash circle of 100 map units around `kritter/boar` and
   `kritter/bear` gobs (`src/haven/MapView.java`, `drawBeastRadius`, which excludes
   `/cdv` resource variants); with `src/haven/MCache.java` defining a tile as 11x11
   map units (`tilesz`), 100 units is about 9 tiles. Treat this as a community
   approximation of the server's aggro radius, not an authoritative constant.
3. Collective aggro for herd animals (mouflon, aurochs): one attacked member pulls
   the herd.
4. Structural aggro: raiding an ant hill aggros its ants; deer destroy objects in
   their path while chasing (see below).
5. Taming interplay: a quelled beast re-aggros on its own after roughly 10 minutes
   (Legacy:Taming); attacking a taming-in-progress animal resets or reduces
   tameness.

Suggested server model (consistent with all of the above): per-species table
`{aggro_radius, aggro_on_damage: true, herd_shared: bool, leash_radius, disengage_distance}`,
a per-gob threat table holding recent attackers, and a herd share step that copies
the threat entry to herd members. "Hitting first" establishes the threat; for
aggressive species proximity alone creates threat.

## Animal AI and the movement wire contract

The client renders exactly three movement models, all created from object deltas
(`src/haven/Session.java`, `getobjdata`; handlers in `src/haven/OCache.java`):

- `OD_LINBEG` (type 3): payload `Coord s, Coord t, int c` - start a linear move from
  s to t; `c` is the nominal duration of the traverse in units of 1/15 s
  (`src/haven/LinMove.java`: progress `a += (dt/1000)/(c*0.06) * 0.9`, so the whole
  path takes `c/15` seconds). `OD_LINSTEP` (type 4): payload `int l` - authoritative
  progress correction, `setl(l)`; `l < 0` or `l >= c` cancels the move (the handler
  deletes the Moving attribute). A walking/wandering animal is sent as a series of
  LinMove segments with periodic Linstep corrections.
- `OD_HOMING` (type 11): payload `int oid` plus, unless oid is -1, `Coord tc` and
  `uint16 v`. oid -1 stops homing (`OCache.homostop`); oid -2 retargets to a fixed
  coordinate (`homocoord`); otherwise the gob homes onto gob `oid`.
  `src/haven/Homing.java` advances a remaining-distance accumulator by
  `(dt/1000)/0.06 * 0.9 * (v/100)`, i.e. a speed of `15 * v / 100` map units per
  second (~1.36 tiles/s at v=100), and re-projects onto the line from the gob
  toward the target's current position; any server `OD_MOVE` resets the accumulator
  (`Homing.move` sets `dist = 0`). A chasing predator is one HOMING broadcast with
  occasional corrections - no per-tick traffic.
- `OD_FOLLOW` (type 10): payload `int oid`; if oid != -1, `int8 szo` and `Coord off`
  follow. `src/haven/Following.java` simply renders the follower at the target
  gob's current position (`getc` returns `tgt.position()`; doff/szo are stored but
  unused in this fork). A leashed tamed animal following the player uses this
  model, so the server needs no movement traffic for followers beyond the initial
  OD_FOLLOW and occasional position fixes.

Implied AI tick structure (server-side, invisible to the client):

- Wander: idle wildlife occasionally emits LinMove segments to nearby reachable
  points (fenced/obstacle-aware pathing is server business; clients just lerp).
- Aggro: on threat acquisition, aggressive/passive-defensive animals emit HOMING at
  the species combat speed toward the attacker; on reaching melee range the animal
  enters the legacy combat relation with the player (see Health and combat below).
- Flee: wounded passive animals (deer, fox, boar at low HP) emit high-v HOMING away
  from the threat or a LinMove sprint; Legacy:Hunting documents that a fleeing deer
  returns passive but stays in combat, re-fleeing only when damaged again, and that
  fleeing targets can be cornered against water or player-built rings of
  constructions (branch ring of fires; foxes break single signposts as of world 4,
  bears destroy obstacles outright).
- Leash break (taming): a quelled beast follows the player (OD_FOLLOW) and
  re-aggros "within the span of about ten minutes" (Jorb, quoted in Legacy:Taming);
  implement as a tameness-decay timer that re-enables the animal's hostile state
  unless quelled again.
- Deer specials: deer "heal other animals and themselves for 40 + (6 * Deer Level)"
  HP (Legacy:Deer), heal any wounded animal nearby (Legacy:Hunting), and destroy
  objects with Soak below 25 that obstruct their path - only palisades and brick
  walls stop a charging deer (Legacy:Deer). Bears likewise smash obstacles and "do
  not run when they are about to die" (Legacy:Hunting).
- Offloading: "Domesticated/Tamed animals don't seem to move when off-loaded.
  (Eating and breeding do continue.)" (Legacy:Animal_Husbandry). Server animals
  outside any player's sight range need not simulate movement; they must still
  consume fodder, gestate, and produce.

## Health, damage, and the combat relation

- Wire: `OD_HEALTH` (type 14) carries one `uint8` hp; `OCache.health` stores it as
  a `GobHealth` attribute (`src/haven/GobHealth.java`). The 0..4 scale is fixed:
  `GobHealth.asfloat()` is `hp / 4.0`, `getfx()` returns null at hp >= 4, and
  below 4 the sprite gets a red tint proportional to missing health. `Gob.getHealth`
  (`src/haven/Gob.java`) exposes it as a 0..100 percentage. The server should push
  health updates on damage/heal for any gob that can show a health bar (animals,
  players, and attackable structures).
- Player-side damage model (Legacy:Combat_101): hits deal SHP (soft, regenerating)
  and HHP (hard) damage; a character has three stacked health levels (knockout,
  death, max) plus regenerating defence and attack meters, combat advantage
  (balance), battle intensity, and initiative points. All of these are visible in
  `src/haven/Fightview.java`, whose `Relation` class carries `balance`,
  `intensity`, `offence`, `defence`, `initiative_points_self`,
  `initiative_points_other` - the exact state a server must track per combat
  relation, and the prerequisites channel for taming.
- Documented animal combat stats: deer HP = Level x 20, "butcher quality" =
  Level x 10, average wild level 10 (i.e. ~200 HP), agility 200 (Legacy:Deer).
  The same HP/level and quality formulas are a reasonable default for other
  wildlife but are only sourced for deer.
- Animals fight with the same combat-energy system as players (Legacy:Deer describes
  deer using a stomp attack, an initiative-generating move, and an attack/defence
  filling move; Legacy:Combat_101 describes the general energy system). Combat
  resolve is client-driven UI only; the server computes all openings and damage
  (legacy damage formula: `damage = basedamage * ql * str / 10`, per
  Legacy:Combat_Actions in the research cache).
- Shallow-water exploit to preserve: animals cannot hit players standing in
  (shallow) water (Legacy:Combat_101 tactics rely on it), so melee reach checks must
  treat water tiles as blocking for animals.

## Taming and domestication (Quell the Beast)

Legacy taming (Legacy:Animal_Husbandry, Legacy:Taming, including a quoted Jorb
post) applies to three wild species, each with a domestic morph:

- Boar -> Pig
- Mouflon -> Sheep
- Aurochs -> Cattle (Cow or Bull)

Protocol, step by step:

1. The tamer needs the Animal Husbandry skill (400 LP, requires Hunting). The skill
   unlocks the combat attack "Quell the Beast".
2. Prerequisites to fire Quell the Beast on the beast (Jorb's own list, quoted in
   Legacy:Taming): two initiative points available for the attack's IP cost, combat
   advantage fully in the tamer's favor (3 or more, i.e. the balance scale maxed),
   battle intensity reduced to 0, and a Rope equipped as the weapon. These are
   ordinary combat-action requirements evaluated against the tracked relation
   (`src/haven/Fightview.java` Relation fields).
3. Each successful Quell adds +20 Tameness to that specific animal (displayed as a
   yellow number like damage). 100 Tameness completes taming: five quell cycles.
4. After the first quell the beast is quelled, the battle ends, and the animal
   follows the tamer (leashed). The rope becomes bound to that animal and cannot
   tame another until the animal turns hostile again on its own.
5. Within about ten minutes (5-15, variable) the beast breaks its leash and
   re-aggros even if it cannot see or reach the tamer, as long as they are nearby.
   Quell again. Hitting or damaging the beast at any point removes some or all
   tameness gained.
6. At 100 tameness the animal "metamorphoses" in place into its domestic morph
   (aurochs becomes a cow or a bull), still following the tamer. Right-click
   flower-menu options Leash/Unleash control following thereafter (Legacy:Cattle).
7. Dual-wield trick: two ropes allow taming two animals concurrently, but taming a
   second animal from the same herd makes the first hostile; different herds are
   easier. Herd fences and signpost pens are the standard containment tool.

Tameness is per-animal persistent server state; the leash-break timer must survive
logout (the wiki's "lagg-relative" wording means the timer is game-time based, not
client-frame based).

## Domestic lifecycle and breeding

- Cattle (Legacy:Cattle): calves are born, not captured. A cow's gestation is 4.5
  real days; a calf matures in 10 real days. Rare twins. Breeding quality: each
  stat of the calf averages the parents' stats with a +20 to -5 random spread,
  softcapped by the bull's breeding quality (the wiki itself flags these numbers as
  needing verification - treat as approximate and tunable).
- Sheep (Legacy:Sheep): lambs -> ewe/ram; same gestation (4.5 real days) and
  maturation (10 real days) as cattle; rare twins; rams impregnate ewes; keep rams
  separate to control breeding.
- Chickens (Legacy:Chicken_Coop): hens lay roughly one egg per real day inside a
  chicken coop; hens lay unfertilized without a cock; fertilized eggs show a white
  pie-fraction hatch timer overlay and only hatch while under the hen (the timer
  stops otherwise); chicks need a 2x2 tile free space to mature. Egg quality is the
  average of the parents' quality +/-5; a chick hatches at the egg's quality and
  only ever loses quality (one level per feeding interval where fodder quality is
  below the chick's), never gains.
- No horses: Legacy:Horses documents the 2010 April Fools' joke; legacy has no
  horses and no foals. There is no "foal capture" - young animals (calf, lamb,
  chick) exist only as bred offspring, and wild adults convert to domestic adults
  via taming. Wagons are pulled by cattle (Oxcarting).

## Feeding: troughs and grazing

- Food Trough (Legacy:Food_Trough): a 2x1 lift-able object (Animal Husbandry).
  Capacity 200 fodder units; radius 18 tiles; lift-and-right-click on another trough
  transfers fodder like a liquid. Fodder quality is the average quality of what was
  placed (q5 + q12 + q16 -> q11). Fodder items: any seeds, Apple, Apple Core,
  Mulberry, Bloated Bolete, Blueberries, Chantrelles, Carrots, Pumpkin Flesh,
  Giant Pumpkin (worth 16 seeds), Peapod, Straw, Poppy Flower, Beetroot,
  Beetroot Leaves.
- Grazing: animals also feed from moor, heath, and grassland tiles, which always
  count as food of quality level 10. Animals inside a trough's radius prefer the
  trough over grazing.
- Consumption rates (cattle, Legacy:Cattle): a non-pregnant cow eats 4.8 food units
  per in-game day; a pregnant cow eats 10x (48 units/day); a lactating cow eats
  4.8 plus 0.1 unit per liter of milk produced. Starvation should kill or stop
  production - the wiki states tamed animals "require food or grassland to
  survive and breed" (Legacy:Animal_Husbandry).

## Animal products and collection flows

- Milk (cows, Legacy:Cattle): cows store up to 10 L; production rate is
  `Milk Quantity * 0.01` L per 10 minutes (0.1 L/10 min at quantity 10; 2.0 L/10
  min at 200). Heifers (never-calved cows) produce nothing - a bull is required in
  the lifecycle. Calves drink stored milk, so reservations matter. Milking is a
  flower-menu interaction with a bucket. Wild aurochs can be made to give milk by
  feeding them Four-Leaf Clovers.
- Wool (sheep, Legacy:Sheep): sheep store up to 3 wool; production is 1 wool per 8
  real hours at Wool Quantity 5, faster with higher-quantity bred sheep.
  Shearing is the collection flow; wool quality follows the sheep's Wool Quality
  stat. Wild mouflons fed Four-Leaf Clovers also yield wool.
- Eggs (chickens, Legacy:Chicken_Coop): laid in the coop's internal 8x8 grid; a
  free tile directly below the hen is required; fertilized eggs hatch into chicks
  if they remain under the hen; unfertilized and even fertilized eggs can be eaten
  (Boiled Egg / Fried Egg in `etc/needed/fep.conf`).
- Honey and beeswax (Bee Keeping, Legacy:Bee_Hive): a beehive holds up to 1.0 L
  honey and 5 wax; harvest wax with the right-click harvest action, honey with a
  bucket; hives additionally accelerate crops in a 13-tile radius (see the farming
  document).
- Aurochs Hair: plucked from living aurochs (Animal Husbandry item); each pluck has
  a 10%+ chance to aggro the herd (Legacy:Creatures).

## Butchering and corpse yields

A killed animal leaves a corpse gob; right-clicking it offers the server-defined
flower menu (`src/haven/FlowerMenu.java`) with skin/butcher options (Hunting skill
required; Legacy:Hunting). Rules:

- Skin first, then butcher: butchering first ruins the hide. Both actions are
  interruptible - walking away mid-butchering leaves the remaining meat on the
  corpse, and a corpse still holding any meat looks unbutchered.
- Yields (Legacy:Creatures; deer cross-checked against Legacy:Deer):

| Creature | Butchering yields |
| --- | --- |
| Aurochs | Raw Hide, Intestines x4, Beef x6, Bone Material x6 |
| Bear | Raw Bear Hide, Bear Meat x8, Intestines x4, Bear Tooth x4, Bone Material x12 |
| Boar | Raw Hide, Intestines x2, Boar Meat x4, Bone Material x4, Boar Tusk x1-2 |
| Chicken | Raw Chicken Meat, Bone Material, Chicken Feather x3 |
| Deer | Raw Hide, Intestines x3, Raw Deer Meat x4, Bone Material x6, Deer Antlers (Legacy:Deer variant: Intestines x4, Raw Deer Meat x10) |
| Fox | Raw Fox Hide, Intestines x1, Fox Meat x2, Bone Material x2 |
| Mouflon | Raw Sheep Skin, Intestines, Raw Mutton, Bone Material (quantities undocumented) |
| Rabbit | Fresh Rabbit Fur, Rabbit Meat, Bone Material |
| Rat / Toad | Nothing |
| Troll | Fresh Troll's Hide x2, Raw Troll Meat x30, Trollbone x8, Troll Skull |
| Cattle | Milk (while alive), Raw Hide, Intestines x4, Beef x6, Bone Material x6 (Legacy:Cattle loot line adds Beef x10, Intestines x5) |
| Sheep | Wool (while alive), Raw Sheep Skin, Intestines x2, Raw Mutton x3, Bone Material x3 |

- Meat names are confirmed by `etc/needed/fep.conf`: Bear Meat, Boar Meat, Fox Meat,
  Rabbit Meat, Raw Chicken Meat, Raw Deer Meat, Raw Mutton, Raw Pork, Beef, and
  their roasted variants (Roasted Bear Meat STR:3.5, Roasted Deer Meat PER:2,
  Roasted Rabbit Meat AGI:1, Roasted Fox Meat INT:1, Roasted Troll Meat STR:15
  PSY:10, and so on).
- Small-game flow (Legacy:Hunting): pick up chicken/rabbit into inventory (2x2),
  "wring neck" via flower menu; rat/toad are 1x1 and must be punched. Killing
  wildlife grants Learning Points per combat phase; losing aggro forfeits earlier
  phases.
- Product quality: deer materials are all quality Level x 10, softcapped by
  Survival and (for skin/meat/intestines) the carving tool (Legacy:Deer); generalize
  as `yield_q = species_base(level) softcapped by Survival/tool`, with deer's
  documented base of level x 10.

## Rare animals and curiosities

- Rabid Jackalope (Legacy:Rabid_Jackalope): picking up a Rabbit may transform it
  into a Rabid Jackalope, a curiosity worth a base 10000 LP; wringing its neck
  turns it back into a normal Dead Rabbit. Server rule: on pickup, roll a rare
  chance and swap the gob/item resource.
- Gold Egg (Legacy:Gold_Egg): chickens in a coop rarely lay a Gold Egg (cock not
  required); a 400000-base-LP curiosity also usable as a Gold Nugget in jewelry
  crafting.
- Emerald Dragonfly (catchable, LP curiosity) and Silkmoth (drops Silkworm Egg) are
  non-combat pickable creatures; the Dryad exists as a passive world creature with
  no yields (Legacy:Creatures).
- No dragons, demons, or legendary bosses exist in legacy beyond the troll as the
  apex aggressive creature; "Dark Heart" (listed among the objects unlocked by the
  Hunting skill on the wiki) is an item, not a creature.

## Pens, ownership, and persistence

- Tamed and bred animals are persistent world objects that survive logout; tameness,
  pregnancy, milk/wool/egg accumulation, and fodder meters must be persisted per
  animal.
- Containment is physical: fences, gates, palisades, and walls block movement (deer
  only respect Soak >= 25 structures). The server's pathing must treat player
  constructions as obstacles, and aggressive animals must be able to destroy weak
  ones (soak checks on charge).
- Protection: animals inside a personal/village claim are protected from third
  parties by the claim/village law system (companion docs under
  `docs/mechanics/world/`); the wiki's animal pages do not document animal
  ownership attribution beyond claim context, so treat "who may milk/shear/kill a
  domestic animal" as claim-law-governed (see Open questions).
- Offscreen behavior (server optimization backed by the wiki note): skip movement
  simulation for animals with no nearby watchers, but keep consuming fodder,
  gestating, laying, and producing.

## Server implementation notes (animals)

- Entity model: animal entity keyed by gob id with components
  `{species, domestic_form, tameness: u8, rope_bound: bool, threat_table, hp,
  level, stats {agi, str, ...}, production {milk_l, wool, eggs}, pregnancy,
  hunger, leash_target}`.
- AI tick: coarse scheduler (1-5 s). State machine per animal:
  IDLE -> WANDER (emit OD_LINBEG/OD_LINSTEP) -> ALERT -> CHASE (OD_HOMING with
  species speed v) -> COMBAT (enter Fightview-style relation; melee only when
  target not in water) -> FLEE (high-v HOMING away) -> RETURN/LEASH-BREAK.
  Aggressive species enter ALERT on proximity; herd species copy threat to herd
  members; missed shots count as attacks for aggro purposes.
- Movement broadcasting: one HOMING broadcast per chase, corrections every few
  seconds or on path change; wander as short LinMove segments; followers as a
  single OD_FOLLOW. Respect the client's models exactly (`src/haven/Homing.java`,
  `src/haven/LinMove.java`, `src/haven/Following.java`) so client-side extrapolation
  stays smooth; Linstep corrections are the only anti-drift mechanism.
- Spawning: biome tables - deer in forests (Legacy:Deer Location), WWW/forage
  terrains per the farming document; bears/boars in deep forest; mouflon/aurochs in
  grassland/heath (documented indirectly by herd-pen hunting practices); rats/toads
  near settlements and swamps. Exact spawn densities are undocumented; expose as
  data.
- Taming service: validate combat-action prerequisites (2 IP available, advantage
  >= 3, intensity == 0, rope equipped, Animal Husbandry skill), add +20 tameness,
  start the 10-minute leash-break timer, handle tameness loss on damage, and perform
  the species morph at 100 (boar->pig, mouflon->sheep, aurochs->cow/bull).
- Production loop: per-animal timers as documented (milk `q * 0.01` L/10 min up to
  10 L; wool 1/8 h at quantity 5 up to 3; eggs ~1/day per hen with the coop-grid
  constraints; honey/wax per hive). Consumption drains the shared trough within 18
  tiles first, then grazing tiles at quality 10.
- Corpse pipeline: on death create a corpse gob with a skin/butcher menu; enforce
  skin-before-butcher, interruption with partial yields, and per-yield quality
  (level x 10 base for deer; Survival/tool softcaps).
- Persistence: animals, troughs, coops, and hives are claim-adjacent persistent
  structures; write them with the same durability as tiles.

## Server implementation notes (this repo, session 45)

- Taming MVP is live (single-node authority): the Quell pagina
  (paginae/atk/quell) gates and resolves through the real combat
  model. Selection-time gates: 2 IP available (req_ip 2), advantage
  >= 3 (req_adv 30 tenths), target is a LOCAL animal, a rope
  (gfx/invobjs/rope) equipped in ANY slot (weapon-slot-only is a
  documented NEXT check), the tamer's rope not already bound, and the
  beast not already quelled. Guest (cross-node) animals refuse quell
  in the MVP.
- Resolution: the queued quell intercepts the animal swing cadence
  (same offence-bar economics as a normal swing; no defence chip, no
  damage) and applies +20 tameness (TAMENESS_PER_QUELL). The battle
  ends on the first quell (out of animal_fights, fight window torn
  down), the beast follows the tamer client-side via a batched
  OD_FOLLOW broadcast (gob ids are global - no per-session patching;
  Following.java renders it), and the rope binds (one partially-tamed
  beast per tamer; binding ends at full tameness or on a break).
- Leash lifecycle: the break deadline is game-tick based (10 minutes
  = 6000 ticks, the docs' 5-15 min floor as policy) rearmed on every
  quell below 100; at 100 (TAMENESS_FULL) the beast never breaks.
  Damaging the beast kills ALL tameness (server policy) and frees the
  rope. The tick sweep (before the batch fan-out) breaks due leashes,
  sends the OD_FOLLOW removal (oid -1), and chats the tamer.
- Tamed animals skip animal AI entirely (no wander, no aggro) while
  the tame row lives; tame rows never outlive local authority (a
  transferred animal drops the row + follow render). Animals are
  spawned wildlife in this server (not persisted), so taming state is
  runtime world state with the same scope - recorded in Open
  questions.

## Server implementation notes (this repo, session 45)

- Taming MVP is live (single-node authority): the Quell pagina
  (paginae/atk/quell) gates and resolves through the real combat
  model. Selection-time gates: 2 IP available (req_ip 2), advantage
  >= 3 (req_adv 30 tenths), target is a LOCAL animal, a rope
  (gfx/invobjs/rope) equipped in ANY slot (weapon-slot-only is a
  documented NEXT check), the tamer's rope not already bound, and the
  beast not already quelled. Guest (cross-node) animals refuse quell
  in the MVP.
- Resolution: the queued quell intercepts the animal swing cadence
  (same offence-bar economics as a normal swing; no defence chip, no
  damage) and applies +20 tameness (TAMENESS_PER_QUELL). The battle
  ends on the first quell (out of animal_fights, fight window torn
  down), the beast follows the tamer client-side via a batched
  OD_FOLLOW broadcast (gob ids are global - no per-session patching;
  Following.java renders it), and the rope binds (one partially-tamed
  beast per tamer; binding ends at full tameness or on a break).
- Leash lifecycle: the break deadline is game-tick based (10 minutes
  = 6000 ticks, the docs' 5-15 min floor as policy) rearmed on every
  quell below 100; at 100 (TAMENESS_FULL) the beast never breaks.
  Damaging the beast kills ALL tameness (server policy) and frees the
  rope. The tick sweep (before the batch fan-out) breaks due leashes,
  sends the OD_FOLLOW removal (oid -1), and chats the tamer.
- Tamed animals skip animal AI entirely (no wander, no aggro) while
  the tame row lives; tame rows never outlive local authority (a
  transferred animal drops the row + follow render). Animals are
  spawned wildlife in this server (not persisted), so taming state is
  runtime world state with the same scope - recorded in Open
  questions.

## Server implementation notes (this repo, session 46)

- Animal Husbandry gate (docs step 1): `ahusb` joins the skill
  catalog with the documented legacy cost (400 LP) and prerequisite
  (requires Hunting); the prerequisite is enforced inside `buy()`, so
  the wallet can never be charged out of order, and the nsk list
  renders it from the shipped `gfx/hud/skills/ahusb.res`.
  `quell_gate` refuses the quell selection without the skill.
- Battle intensity (docs step 2, Jorb's list): every animal fight row
  (AnimalFight) carries an intensity bar. A landed blow in EITHER
  direction raises it by INTENSITY_PER_BLOW (2500/10000, policy);
  every combat tick without a blow de-escalates it by
  INTENSITY_DECAY (250 - a hot fight cools in ~7 s). `quell_gate`
  refuses the selection while intensity > 0, so the working pattern
  is: build advantage, stop swinging, wait out the de-escalation,
  quell.
- Species morph (docs step 6): at full tameness the animal
  metamorphoses in place. Species::morph() maps mouflon -> sheep and
  aurochs -> cow; the boar maps to None because the 2009 pack ships
  no pig kritter (policy recorded below). The morph rewrites the
  Kind, the drawable resource (sheep/cow cdv), max_hp and speed,
  clamps the current hp (no healing), and broadcasts a headerless
  OD_RES block through the packed start batch with a per-session wire
  id patch - the native client re-render path (Session.java OD_RES =
  2 -> OCache.cres -> ResDrawable reset).
- Roster: Species gains Mouflon (index 7) and Sheep (index 8). The
  node-link discriminant stays append-only (0-6 frozen); the mouflon
  joins the wild spawn roll, the sheep NEVER spawns wild (it is only
  reached through the morph, matching the wiki's "Must domesticate a
  Mouflon to obtain a Sheep"). Sheep-family loot carries the wool
  that feeds the session-46 cloth craft chain.

## Server implementation notes (this repo, session 47)

- Production meters (docs "Animal products and collection flows"):
  TameState carries `milk_units` (0.01 L units, cows), `wool` (sheep)
  and a shared integer accumulator `prod_acc`. Milk accrues at the
  doc's `Milk Quantity * 0.01` L / 10 min: quantity 10 (server policy,
  no verified bred-stat numbers) adds 10 units per 6000 ticks, i.e.
  exactly the doc's 0.1 L / 10 min example. Wool accrues 1 per 8 real
  hours at Wool Quantity 5, scaled linearly with quantity (acc += q
  per tick; one unit per 240000 quantity-ticks). Both meters cap
  exactly as documented (10 L / 3 wool); at the cap the accumulator
  stops banking time, so production resumes from zero after collection.
- Grazing gate (docs "Feeding: troughs and grazing"): the production
  sweep (two-phase, O(tamed), same shape as tick_animals) advances a
  meter only while the animal stands on a moor/heath/grassland tile -
  the doc's quality-10 foods. Off the pasture production PAUSES and
  the accumulator freezes (no starvation deaths; the Food Trough
  object is not implemented yet, so free grazing is the only feeding
  path - see Open questions). Products inherit the grazing quality
  (q10, GRAZE_PRODUCT_QL).
- Collection flows: clicking a fully tamed producer opens the
  collection flower menu instead of the fight window (tamed livestock
  cannot be aggroed, so a producer never fights). Cow: the Milk petal
  consumes ONE empty bucket (`gfx/invobjs/buckete` - inventory first,
  then any equipment slot, the same any-slot policy as the crafting
  tool scan and the taming rope check), drains MILK_PER_BUCKET_UNITS
  (1 L = 100 units; the doc names the bucket but carries no volume -
  1 L is server policy) and grants `gfx/invobjs/bucket-milk` at q10.
  Sheep: the Shear petal is barehand (the doc names no shear tool) and
  grants the whole stored `gfx/invobjs/wool` stack. An empty meter
  never opens a menu - the click answers with a hint chat line instead
  ("The cow has no milk yet." / "The sheep has no wool to shear.");
  the menu re-validates the live meter and the bucket on the choice,
  so a stale menu cannot overdraw the cow.
- Tamed-animal persistence (docs "Pens, ownership, and persistence"):
  rows with tameness > 0 persist as SavedAnimal (species index, tile,
  hp clamped to the species max on load, tameness, tamer key, meters,
  accumulator; save v6, additive). The saved species IS the domestic
  morph - a fully tamed mouflon reloads as a sheep. The tamer gob id
  cannot survive restarts (gob ids are runtime identities), so the row
  stores the tamer's character save key when online; a fully tamed
  beast never re-arms its leash window, a partially tamed one re-arms
  it at load, and either way the tamer binding re-establishes on the
  next quell (apply_quell overwrites the row's tamer). Spawned
  wildlife is seed-regenerated and never saved.
- Persistence bugfix found while wiring animals in: `flush()` never
  copied `tile_overrides` into the save document, so furrows and other
  terraforming silently reverted on every restart even though the
  in-memory world_state carried them. Both fields now round-trip.

## Open questions (animals)

- Taming state (post session-47): the "battle intensity == 0",
  Animal Husbandry skill, species-morph prerequisite and tamed-state
  persistence are now implemented (session 47 saves tameness rows +
  the production meters; the domestic morph survives as the saved
  species). Still open: the intensity meter's client rendering (the
  bar exists only server-side; the legacy Fightview rendered
  intensity from the uimsg relation, which this server keeps at 0).
- Tamed-animal production depth: breed stats (Milk Quantity / Wool
  Quality per animal) are flat server-policy constants (10 / 5); the
  Food Trough object, fodder transfer and starvation are not
  implemented (free grazing on moor/heath/grass is the only feeding
  path), and breeding/gestation is out of scope until animals carry
  per-animal stat rows.
- The boar morph is unreachable: the 2009 pack ships no pig kritter
  directory, so a fully tamed boar stays a boar. When a pig drawable
  surfaces (a later resource pack or the legacy client's own pack),
  add (Boar, Pig) to Species::morph(). The aurochs morph lands on the
  cow cdv; a standalone bull rendering (cow/bull.res) is a possible
  future sex-based variant, unverified against the cdv layering.

- Session 36 bone drops: every species now drops `gfx/invobjs/bone` on
  death (Deer/Aurochs/Cow/Boar/Wolf x2, Fox/Hare x1) so the Bone Arrow
  recipe has an in-world source. The legacy butcher table above carries
  bigger numbers (Bone Material x6 deer/cattle, x4 boar, x2 fox), but
  this server's death-drop policy scales the whole loot table down
  (meat x3/x4 against legacy x10) and the bone counts follow the same
  proportion; reconcile all counts against the Legacy butcher pages
  when a source is reachable.
- Exact aggro radii and leash/disengage distances per species; the only numeric hint
  is the union client's 100-unit (about 9-tile) circle for boar/bear
  (`src/haven/MapView.java`). Determine by observation or emulator prior art.
- Full per-species movement speeds (the `v` parameter of OD_HOMING) and wander
  cadence; not documented anywhere. Capture from a legacy session.
- Mouflon butcher quantities (the wiki leaves them as "x?"); recover from the
  legacy client resource files or a capture.
- Whether wild-spawned cow/bull/sheep/pig exist in legacy or every domestic animal
  traces to a tamed wild adult (the wiki implies the latter: "Must domesticate a
  Mouflon to obtain a Sheep").
- Animal ownership attribution inside claims (who may interact with whose cow) and
  whether animals inherit the owner's claim protection when led outside - not
  documented; decide policy after reading `docs/mechanics/world/` claim documents.
- The breeding quality formula's exact spread and softcap (the wiki's own "+20 ->
  -5 (someone please check my numbers there)" is flagged as unverified).
- Whether troll spawn is player-proximity-aggro from creation ("suspected instant
  agro to responsible player for its spawn" per Legacy:Creatures) and what triggers
  troll spawns.
- Exact critical (`kritter/*`) resource names per species and their stage/variant
  sprite structure; requires the full legacy resource pack (the local `res/` tree
  has none of them).
