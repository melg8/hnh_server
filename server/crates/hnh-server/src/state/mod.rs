//! Game state: data-oriented (SoA) entity storage + simulation.
//!
//! Design: a single owner task (`game::Game`) owns `World` and runs a fixed
//! 10 Hz tick. All entity hot data lives in struct-of-arrays tables with
//! generational ids, keeping the per-tick loops cache-friendly (perf rules
//! from AGENTS.md: mem-/perf-/coll-). Networking tasks never touch these
//! tables; they exchange encoded `RMSG` payloads through per-session queues.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::time::Instant;

use hnh_world::tile;

use crate::fxhash::{FxHashMap, FxHashSet};
use crate::resources::ResTable;

pub const TICK_HZ: u64 = 10;
pub const TICK_MS: u64 = 1000 / TICK_HZ;

// Domain submodules (session 94, pure move out of the former
// single-file state.rs; see git history for the pre-split layout).
mod fight;
mod gathering;
mod gobs;
mod livestock;
mod perf;
mod player;
mod retrans;
mod session;
mod species;
mod trough;

pub use fight::*;
pub use gathering::*;
pub use gobs::*;
pub use livestock::*;
pub use perf::*;
pub use player::*;
pub use retrans::*;
pub use session::*;
pub use species::*;
pub use trough::*;

// retrans carries crate-internal tuning knobs the sweep reads by the
// historical `crate::state::` path; glob re-exports cover the pub items,
// these four keep their pub(crate) reach.
pub(crate) use retrans::{
    ACK_LAG_RTO_CAP_MS, RETRANS_SESSION_SHARE_MIN, RETRANS_SWEEP_BUDGET, RETRANS_THROTTLE_MS,
};

pub use gobs::gob_id_from_slot;
pub use gobs::split_gob_id;

pub type GobId = i32;
pub type SessionId = u32;

/// Walkability + per-terrain speed cap as a PERCENT of the mover's gait
/// speed (server-side rule; documented in map-and-terrain.md "Open
/// questions"). Returns None for impassable tiles; otherwise 0..100.
#[inline]
pub fn tile_speed_pct(t: u8) -> Option<i32> {
    match t {
        tile::DEEP_WATER | tile::WATER | tile::MOUNTAIN | tile::CAVE => None,
        tile::CONIFER | tile::BROADLEAF => Some(60),
        tile::SWAMP1 => Some(50),
        tile::SAND => Some(80),
        tile::MOOR | tile::HEATH => Some(90),
        _ => Some(100),
    }
}

/// Walkability check retained for existing callers: passable tiles are
/// exactly the tiles with a speed percentage.
#[inline]
pub fn tile_speed(t: u8) -> Option<i32> {
    tile_speed_pct(t).map(|_| 1)
}

/// Grazing check (session 47): the doc's feeding rule counts moor, heath
/// and grassland as food of quality level 10. Tamed production meters
/// advance only while the animal stands on one of these tiles; anything
/// else pauses production (no starvation deaths - documented policy).
#[inline]
pub fn tile_grazes(t: u8) -> bool {
    matches!(t, tile::GRASS | tile::MOOR | tile::HEATH)
}

/// Full simulation world.
pub struct World {
    #[allow(dead_code)] // needed by persistence snapshot headers this session
    pub seed: u64,
    pub grids: hnh_world::GridStore,
    pub gobs: Gobs,
    /// resource idx -> resname (game-global).
    pub res: ResTable,
    pub players: Vec<Player>,
    /// session id -> index into players.
    pub by_session: HashMap<SessionId, usize>,
    /// Animals by gob id for quick lookup.
    pub animal_gobs: Vec<GobId>,
    /// Live animal engagements: offence/defence bars toward their target.
    pub animal_fights: FxHashMap<GobId, AnimalFight>,
    /// Home anchor per aggressive animal (session 83: the aggro
    /// leash). An aggressor spawns with one, chases only while it stays
    /// inside AGGRO_GIVEUP of it, and surrenders beyond - an unleashed
    /// chaser otherwise shadows the player forever (the live dairy
    /// probe's boar held a forced duel for whole minutes of chase).
    /// Runtime state: animals are not persisted, the anchor refills
    /// from the seed on every grid (re)spawn.
    pub animal_home: FxHashMap<GobId, (i32, i32)>,
    /// Aggressors that surrendered a chase and are walking home. While
    /// the id lives here the intent pass refuses a fresh aggro even
    /// when a player wanders past (hysteresis: the beast must first
    /// reach its home circle again) - otherwise a returning boar
    /// re-aggros on every step of the way back.
    pub animal_surrender: FxHashSet<GobId>,
    /// Growing crops by gob id (farming tick + harvest lookup).
    pub crops: FxHashMap<GobId, crate::farm::CropState>,
    /// Tile -> crop gob occupying it (one crop per tile).
    pub crop_at: HashMap<(i32, i32), GobId>,
    /// Plowed (furrowed) tiles -> tilth state. `0` deadline = planted
    /// (never decays while the crop lives); a non-zero unix-ms deadline
    /// reverts the tile to grass when it passes.
    pub tilth: HashMap<(i32, i32), u64>,
    /// Construction plans by gob id (sinking + stage lookup).
    pub plans: FxHashMap<GobId, crate::build::PlanState>,
    /// Tile -> plan gob occupying it (one build site per tile).
    pub plan_at: HashMap<(i32, i32), GobId>,
    /// Finished stations by gob id (fuel/input/progress state).
    pub stations: FxHashMap<GobId, crate::build::StationState>,
    /// Tile -> finished structure gob occupying it.
    pub structure_at: HashMap<(i32, i32), GobId>,
    /// Formed parties (small vec; parties are capped and rare, linear
    /// scan by member is fine and keeps the hot paths untouched).
    pub parties: Vec<crate::party::PartyState>,
    /// Foreign-authority gobs rendered for local sessions (cluster mode).
    /// The owning node streams their state; this node NEVER simulates a
    /// guest - it only advances movement interpolation deterministically
    /// from the linmove params (same arithmetic as local movers).
    pub guests: FxHashMap<GobId, GuestGob>,
    /// Cross-node interaction relay (session 28). On the ATTACKER's home
    /// node: local mirrors of the defence bars of guest animals being
    /// fought through the relay (the authoritative bars live on the
    /// owning node; FightBars messages re-sync this mirror, the fightview
    /// reads it). Keyed by the animal gob id.
    pub guest_fights: FxHashMap<GobId, AnimalFight>,
    /// On the TARGET's authority node: the guest player currently engaged
    /// with each relay-fought animal (same one-attacker cardinality as
    /// `animal_fights`). Keyed by the animal gob id, value = player gob
    /// id; PlayerHurt/KillCredit route back through `node_of_gob`.
    pub guest_attackers: FxHashMap<GobId, GobId>,
    /// Taming state per animal gob (session 45). An entry exists from the
    /// first successful Quell until the leash breaks (or damage kills the
    /// tameness). Tamed animals never re-enter `animal_fights` while the
    /// entry lives; the client renders the leash through OD_FOLLOW.
    /// Runtime world state: animals are spawned wildlife (not persisted),
    /// so taming state is equally session-world scope - recorded in the
    /// docs Open questions.
    pub tamed: FxHashMap<GobId, TameState>,
    /// Session 90: owner ACCOUNT keys (persist::save_key shape) for
    /// tamed rows whose tamer gob is not a live player - populated at
    /// restore, consumed (removed) when the owner logs in and the row
    /// re-binds to the fresh gob. Keeps the herd loyal across restarts
    /// without a per-beast String in TameState.
    pub tamed_owner: FxHashMap<GobId, String>,
    /// Placed Food Troughs (session 48): fodder stores keyed by gob id.
    /// Built through the build tree (build::BUILDABLES id "trough"),
    /// loaded by itemact, drained by animals feeding inside the radius.
    pub troughs: FxHashMap<GobId, TroughState>,
    /// Tick counter for deterministic scheduling.
    pub tick: u64,
    /// Logical world time in ms, advanced by TICK_MS each game tick (the
    /// movement clock all LinMove progress math is anchored to).
    pub now_ms: u64,
    /// Deterministic RNG for AI (seeded from world seed).
    rng: hnh_world::JavaRandom,
    start_instant: Instant,
    /// Perf counters.
    pub perf: Perf,
}

/// One foreign-authority gob (cluster mode). Carries everything the
/// subscriber needs to render, interpolate and retract it locally; the
/// owning node pushes GuestAnnounce/GuestUpdate/GuestRetract for cells
/// with subscribed viewers.
#[derive(Debug, Clone)]
pub struct GuestGob {
    pub pos: (i32, i32),
    /// Current linear move (same timing model as local movers; progress
    /// math is deterministic, so no per-tick streaming is needed).
    pub mv: Option<LinMove>,
    /// Wire frame counter for OBJDATA blocks (incremented on each
    /// finalizer/retarget forwarded from the owner).
    pub frame: u32,
    /// Pose state (walking vs standing layers).
    pub moving: bool,
    /// Movement octant 0..8 (see `game::move_dir`).
    pub facing: u8,
    pub kind: crate::nodes::GuestKind,
    /// Resource index interned locally at ingest.
    pub res_idx: u16,
    pub hp: i32,
    pub max_hp: i32,
    pub cell: (i32, i32),
    /// True when the owner pushed this guest because it stands in a cell
    /// I own (territory rule); such guests never GC on my side - the owner
    /// retracts them when the player goes home. Subscription guests (the
    /// default) GC when unviewed and unsubscribed.
    pub territory: bool,
    /// Last tick the owner refreshed this guest (diagnostics; also a GC
    /// backstop input for future policies).
    #[allow(dead_code)]
    pub last_seen_tick: u64,
}

impl World {
    pub fn new(seed: u64) -> Self {
        Self::with_layout(seed, NonZeroUsize::new(1).expect("one node"), 0)
    }

    /// Cluster layout variant: the gob slot partition (see `Gobs::with_layout`)
    /// and the empty guest table (foreign-authority gobs rendered for local
    /// sessions) both come from the cluster configuration.
    pub fn with_layout(seed: u64, nodes: NonZeroUsize, me: usize) -> Self {
        World {
            seed,
            grids: hnh_world::GridStore::new(seed),
            gobs: Gobs::with_layout(nodes, me),
            res: ResTable::new(),
            players: Vec::new(),
            by_session: HashMap::new(),
            animal_gobs: Vec::new(),
            animal_fights: FxHashMap::default(),
            animal_home: FxHashMap::default(),
            animal_surrender: FxHashSet::default(),
            crops: FxHashMap::default(),
            crop_at: HashMap::new(),
            tilth: HashMap::new(),
            plans: FxHashMap::default(),
            plan_at: HashMap::new(),
            stations: FxHashMap::default(),
            structure_at: HashMap::new(),
            parties: Vec::new(),
            guests: FxHashMap::default(),
            guest_fights: FxHashMap::default(),
            guest_attackers: FxHashMap::default(),
            tamed: FxHashMap::default(),
            tamed_owner: FxHashMap::default(),
            troughs: FxHashMap::default(),
            tick: 0,
            now_ms: 0,
            rng: hnh_world::JavaRandom::new(seed as i64),
            start_instant: Instant::now(),
            perf: Perf::default(),
        }
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.start_instant.elapsed().as_secs_f64()
    }

    /// Populate statics for a grid. `filter` (cluster mode, session 33)
    /// spawns only content whose VisIndex cell belongs to this node -
    /// foreign-cell content is the owning node's to spawn and announce
    /// (Sub-driven populate on the authority), so no node carries shadow
    /// copies of the same tree/stone/boulder. Single-node mode passes
    /// None and materializes everything.
    pub fn populate_grid(
        &mut self,
        gc: (i32, i32),
        filter: Option<(usize, std::num::NonZeroUsize)>,
        out: &mut Vec<GobId>,
    ) {
        let grid = self.grids.grid(gc);
        let mut spawned = 0usize;
        for y in (0..100usize).step_by(1) {
            for x in 0..100usize {
                let t = grid.tile(x, y);
                let tx = gc.0 as i64 * 100 + x as i64;
                let ty = gc.1 as i64 * 100 + y as i64;
                let mut r = hnh_world::mkrandoom(tx as i32, ty as i32);
                let roll = r.next_bounded(1000);
                // Ore kind is rolled inside the match arm (its roll must not
                // disturb the rolls the other terrain arms take) and carried
                // out through this slot; None = not an ore deposit.
                let mut ore_kind: Option<OreKind> = None;
                // Clay deposits carry their flag the same way (session 69
                // kiln chain): the arm consumes no extra roll, so the
                // other terrain arms' roll sequences stay untouched.
                let mut clay = false;
                // Session 81 (dough chains) carries its statics the same
                // way: flags rolled out of the SAME per-tile roll (no
                // extra draws), so every pre-existing spawn is
                // bit-identical and saves keep their surroundings.
                let mut apple_tree = false;
                let mut forage: Option<ForageKind> = None;
                let mut hive = false;
                let res = match t {
                    tile::CONIFER if roll < 220 => {
                        let s = r.next_bounded(3);
                        match s {
                            0 => "gfx/terobjs/trees/fir",
                            // The pack has no pine; atree carries numbered
                            // image+neg frames (atree/01.res..06.res).
                            1 => "gfx/terobjs/trees/atree/01",
                            _ => "gfx/terobjs/trees/atree/02",
                        }
                    }
                    tile::BROADLEAF if roll < 220 => {
                        let s = r.next_bounded(3);
                        match s {
                            // birch/oak ship as numbered frame dirs
                            // (birch/01.res..), not as single resources.
                            0 => "gfx/terobjs/trees/birch/01",
                            1 => "gfx/terobjs/trees/maple/01",
                            _ => "gfx/terobjs/trees/oak/01",
                        }
                    }
                    tile::GRASS if roll < 8 => {
                        if r.next_bounded(2) == 0 {
                            "gfx/terobjs/bumlings/01"
                        } else {
                            "gfx/terobjs/bumlings/02"
                        }
                    }
                    // Session 81 (pirozhki chain): wild onion patches on
                    // the grass meadows - the onion crop's seed is its own
                    // harvest, so the crop chain needs a world seed source
                    // (the legacy seed ladder runs through WWW drying,
                    // which this server does not model). Roll band sits
                    // strictly above the bumling band; no extra draw.
                    tile::GRASS if roll < 20 => {
                        forage = Some(ForageKind::WildOnion);
                        ForageKind::WildOnion.world_res()
                    }
                    tile::HEATH if roll < 6 => "gfx/terobjs/bumlings/01",
                    // Session 81 (blueberry pie chain): blueberries grow
                    // on forest AND heath (farming-and-plants.md
                    // "Foraging" terrain binding). Band [6, 26) sits above
                    // the heath bumling band.
                    tile::HEATH if roll < 26 => {
                        forage = Some(ForageKind::Blueberry);
                        ForageKind::Blueberry.world_res()
                    }
                    // Session 81 (the dough chains): broadleaf forest
                    // carries the forageable set - blueberries and
                    // chantrelles (the doc's forest bindings), wild
                    // grapevines (the raisin chain's grape source) and
                    // sparse wild beehives (the honey source; the legacy
                    // buildable hive model is out of scope, recorded in
                    // crafting-and-building.md). Every band sits above the
                    // 220 tree band, so no roll that used to spawn a tree
                    // changes meaning. No arm consumes an extra draw.
                    tile::BROADLEAF if roll < 250 => {
                        forage = Some(ForageKind::Blueberry);
                        ForageKind::Blueberry.world_res()
                    }
                    tile::BROADLEAF if roll < 268 => {
                        forage = Some(ForageKind::Chantrelle);
                        ForageKind::Chantrelle.world_res()
                    }
                    tile::BROADLEAF if roll < 286 => {
                        forage = Some(ForageKind::Grapevine);
                        ForageKind::Grapevine.world_res()
                    }
                    tile::BROADLEAF if roll < 294 => {
                        hive = true;
                        "gfx/terobjs/bhive"
                    }
                    // Apple trees: roughly one tree in six of the forest
                    // carries fruit (band [294, 334) against the 220-wide
                    // regular-tree band).
                    tile::BROADLEAF if roll < 334 => {
                        apple_tree = true;
                        "gfx/terobjs/trees/appletree"
                    }
                    tile::MOOR if roll < 4 => "gfx/terobjs/bumlings/stal2",
                    // Metal chain, mining leg: rocky terrain carries ore
                    // deposits (the ore-heap sprite distinguishes them from
                    // plain boulders). MOUNTAIN and CAVE tiles are
                    // impassable, so a deposit is only placed where some
                    // 4-neighbor is walkable - the player must be able to
                    // stand next to it and pick (deterministic pure-terrain
                    // check, the same seed-derived tile function the grid
                    // itself came from). See the "World gathering" section
                    // of crafting-and-building.md.
                    tile::MOUNTAIN | tile::CAVE if roll < 30 => {
                        let (atx, aty) = (tx as i32, ty as i32);
                        let walkable_neighbor = [
                            self.grids.terrain_at(atx + 1, aty),
                            self.grids.terrain_at(atx - 1, aty),
                            self.grids.terrain_at(atx, aty + 1),
                            self.grids.terrain_at(atx, aty - 1),
                        ]
                        .into_iter()
                        .any(|nt| tile_speed(nt).is_some());
                        if !walkable_neighbor {
                            continue;
                        }
                        let ore = OreKind::from_roll(r.next_bounded(10) as u32);
                        ore_kind = Some(ore);
                        "gfx/terobjs/mining/heap"
                    }
                    // Kiln chain, clay leg (session 69): the sandy shore
                    // belt stands in for the legacy mudflats (Legacy:Clay
                    // digs clay from the flats by the water). Sand tiles
                    // are walkable, so the player can pick in place - no
                    // walkable-neighbor gate is needed. The 150/1000 roll
                    // gives a shore-heavy density (the nearest shore sits
                    // 51 tiles from the dev-seed spawn; the kiln's Clay
                    // x45 build demand needs several deposits per trip).
                    // See the "World gathering" section of
                    // crafting-and-building.md.
                    tile::SAND if roll < 150 => {
                        clay = true;
                        "gfx/terobjs/mining/heap"
                    }
                    _ => continue,
                };
                let res_idx = self.res.intern(res);
                // Object sits at tile corner subtile (tile * 11), matching
                // client flavor placement convention.
                let pos = ((tx as i32) * 11, (ty as i32) * 11);
                // Owner-filtered populate (session 33): foreign-cell
                // statics belong to the cell owner - never spawn a local
                // shadow copy of them.
                if let Some((me, nodes)) = filter {
                    if crate::grid_owner::owner_of(crate::visidx::cell_of(pos.0, pos.1), nodes)
                        != me
                    {
                        continue;
                    }
                }
                // Trees are pickable (branches), bumlings are boulders with
                // a stone supply, rocky-corner heaps are ore deposits, and
                // shore heaps are clay deposits; the session-81 statics
                // (herbs, hives, apple trees) dispatch on their roll flags
                // BEFORE the generic trees/ test - the appletree res would
                // otherwise swallow the apple gob into the branch economy.
                // See the "World gathering" section of
                // crafting-and-building.md.
                let kind = if let Some(ore) = ore_kind {
                    Kind::OreDeposit {
                        ore,
                        left: ORE_PICKS,
                    }
                } else if clay {
                    Kind::ClayDeposit { left: CLAY_PICKS }
                } else if let Some(f) = forage {
                    Kind::Forage { forage: f }
                } else if hive {
                    Kind::BeeHive {
                        honey: HIVE_HONEY_UNITS,
                    }
                } else if apple_tree {
                    Kind::FruitTree { left: APPLE_PICKS }
                } else if res.contains("trees/") {
                    Kind::Tree {
                        harvests: TREE_HARVESTS,
                    }
                } else {
                    Kind::Boulder {
                        left: BOULDER_STONES,
                    }
                };
                let id = self.gobs.spawn(kind, pos, res_idx, 100, 0);
                out.push(id);
                spawned += 1;
            }
        }
        self.perf.spawned_objects += spawned;
    }

    /// Spawn wildlife for a grid. `filter` mirrors populate_grid's
    /// owner rule (session 33): animals in foreign cells are spawned by
    /// the cell owner and cross the wire as guests - a non-owner node
    /// never spawns them (its rng would place a DIFFERENT animal at a
    /// different spot than the owner's roll, and the two copies would
    /// desync every boundary view).
    pub fn populate_animals(
        &mut self,
        gc: (i32, i32),
        filter: Option<(usize, std::num::NonZeroUsize)>,
        count: usize,
        out: &mut Vec<GobId>,
    ) {
        let grid = self.grids.grid(gc);
        for _ in 0..count {
            // Find a walkable tile.
            let mut tries = 0;
            while tries < 20 {
                let x = self.rng.next_bounded(100) as i64;
                let y = self.rng.next_bounded(100) as i64;
                let t = grid.tile(x as usize, y as usize);
                tries += 1;
                if tile_speed(t).is_none() {
                    continue;
                }
                let species = match self.rng.next_bounded(10) {
                    0 => Species::Deer,
                    1 => Species::Fox,
                    2 => Species::Wolf,
                    3 => Species::Boar,
                    4 => Species::Cow,
                    5 => Species::Hare,
                    6 => Species::Aurochs,
                    // Session 46: the third tameable wild beast. Sheep
                    // NEVER spawns wild - it is reached only through the
                    // mouflon morph (animals-and-husbandry.md).
                    7 => Species::Mouflon,
                    // Session 77: the bear (the sausage chain's big-meat
                    // source) and the hen (the only Raw Chicken Meat
                    // source) join the wild roster at the same flat
                    // weight policy - the coop will take over hens later
                    // (doc Open questions).
                    8 => Species::Bear,
                    _ => Species::Hen,
                };
                let px = (gc.0 as i64 * 100 + x) as i32 * 11 + 5;
                let py = (gc.1 as i64 * 100 + y) as i32 * 11 + 5;
                if let Some((me, nodes)) = filter {
                    if crate::grid_owner::owner_of(crate::visidx::cell_of(px, py), nodes) != me {
                        continue;
                    }
                }
                let res_idx = self.res.intern(species.resname());
                let id = self.gobs.spawn(
                    Kind::Animal { species },
                    (px, py),
                    res_idx,
                    species.max_hp(),
                    species.speed(),
                );
                self.animal_gobs.push(id);
                // The aggro-leash anchor: born where the beast spawns.
                self.animal_home.insert(id, (px, py));
                out.push(id);
                break;
            }
        }
    }

    /// Index of the party containing `gob`, if any. Parties are rare and
    /// capped small; a linear scan beats a secondary index here.
    pub fn party_idx(&self, gob: GobId) -> Option<usize> {
        self.parties.iter().position(|p| p.contains(gob))
    }

    pub fn player_mut(&mut self, sid: SessionId) -> Option<&mut Player> {
        let idx = *self.by_session.get(&sid)?;
        self.players.get_mut(idx)
    }

    pub fn player(&self, sid: SessionId) -> Option<&Player> {
        self.players.get(*self.by_session.get(&sid)?)
    }

    /// Deterministic in-game clock: 8 real hours per in-game day (SERVER_RATIO=3),
    /// 365-day year, epoch anchored at session start.
    pub fn astro(&self) -> (i32, i32, i32, i32) {
        // returns (unix_secs, dt_e9, mp_e9, yt_e9)
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i32)
            .unwrap_or(0);
        let day_len_secs = 8.0 * 3600.0;
        let elapsed = self.elapsed_secs();
        let dt = ((elapsed % day_len_secs) / day_len_secs * 1e9) as i32;
        // Moon phase cycles every 8 in-game days.
        let moon_cycle = 8.0 * day_len_secs;
        let mp = ((elapsed % moon_cycle) / moon_cycle * 1e9) as i32;
        // Year: 365 in-game days.
        let year_secs = 365.0 * day_len_secs;
        let yt = ((elapsed % year_secs) / year_secs * 1e9) as i32;
        (unix, dt, mp, yt)
    }

    pub fn next_ai_rand(&mut self, bound: i32) -> i32 {
        self.rng.next_bounded(bound)
    }
}

/// Walk a straight-line path check: sample tiles along the segment.
pub fn path_clear(world: &mut World, sx: i32, sy: i32, tx: i32, ty: i32) -> bool {
    let dist = ((tx - sx).abs() + (ty - sy).abs()).max(1);
    let steps = (dist / 11).clamp(1, 1000);
    for i in 0..=steps {
        let x = sx + (tx - sx) * i / steps;
        let y = sy + (ty - sy) * i / steps;
        let gc = (x.div_euclid(1100), y.div_euclid(1100));
        let ix = (x.div_euclid(11)).rem_euclid(100) as usize;
        let iy = (y.div_euclid(11)).rem_euclid(100) as usize;
        let grid = world.grids.grid(gc);
        if tile_speed(grid.tile(ix, iy)).is_none() {
            return false;
        }
    }
    true
}
