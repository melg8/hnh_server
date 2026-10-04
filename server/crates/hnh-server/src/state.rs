//! Game state: data-oriented (SoA) entity storage + simulation.
//!
//! Design: a single owner task (`game::Game`) owns `World` and runs a fixed
//! 10 Hz tick. All entity hot data lives in struct-of-arrays tables with
//! generational ids, keeping the per-tick loops cache-friendly (perf rules
//! from AGENTS.md: mem-/perf-/coll-). Networking tasks never touch these
//! tables; they exchange encoded `RMSG` payloads through per-session queues.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use hnh_world::tile;

use crate::resources::ResTable;

pub const TICK_HZ: u64 = 10;
pub const TICK_MS: u64 = 1000 / TICK_HZ;

/// View radius in map subtiles around a player (~45 tiles, legacy ~500px).
pub const VIEW_RADIUS: i32 = 500;

/// Movement speed in subtiles/second for a player on grass at normal pace.
pub const BASE_SPEED: i32 = 44; // ~4 tiles/s

pub type GobId = i32;
pub type SessionId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Species {
    Deer,
    Fox,
    Wolf,
    Boar,
    Cow,
    Hare,
    Aurochs,
}

impl Species {
    pub fn resname(self) -> &'static str {
        match self {
            Species::Deer => "gfx/kritter/deer",
            Species::Fox => "gfx/kritter/fox",
            Species::Wolf => "gfx/kritter/wolf",
            Species::Boar => "gfx/kritter/boar",
            Species::Cow => "gfx/kritter/cow",
            Species::Hare => "gfx/kritter/hare",
            Species::Aurochs => "gfx/kritter/aurochs",
        }
    }

    pub fn max_hp(self) -> i32 {
        match self {
            Species::Hare => 20,
            Species::Deer | Species::Fox => 40,
            Species::Boar | Species::Cow => 70,
            Species::Wolf => 60,
            Species::Aurochs => 90,
        }
    }

    pub fn aggressive(self) -> bool {
        matches!(self, Species::Wolf | Species::Boar)
    }

    pub fn speed(self) -> i32 {
        match self {
            Species::Hare => 60,
            Species::Deer => 55,
            Species::Fox => 50,
            Species::Wolf => 48,
            Species::Boar => 40,
            Species::Cow | Species::Aurochs => 30,
        }
    }

    /// Loot dropped on death: (resource name, count, display label).
    pub fn loot(self) -> Vec<(&'static str, u32, &'static str)> {
        match self {
            Species::Deer | Species::Aurochs => vec![
                ("gfx/invobjs/meat", 3, self.meat_label()),
                ("gfx/invobjs/hide", 2, ""),
            ],
            Species::Cow => vec![
                ("gfx/invobjs/meat", 4, self.meat_label()),
                ("gfx/invobjs/hide", 3, ""),
            ],
            Species::Boar => vec![("gfx/invobjs/meat", 3, self.meat_label())],
            Species::Fox => vec![
                ("gfx/invobjs/meat", 1, self.meat_label()),
                ("gfx/invobjs/tail", 1, ""),
            ],
            Species::Wolf => vec![("gfx/invobjs/meat", 2, self.meat_label())],
            Species::Hare => vec![("gfx/invobjs/meat", 1, self.meat_label())],
        }
    }

    /// Server-sent display name for this species' raw meat. Values match
    /// fep.conf keys so eating resolves FEPs (food-and-fep.md: the server's
    /// item tooltip names must match the table keys). Wolf meat has no
    /// legacy fep.conf entry; the empty label falls back to the resource
    /// tooltip and grants no FEPs rather than inventing numbers.
    pub fn meat_label(self) -> &'static str {
        match self {
            Species::Deer => "Raw Deer Meat",
            Species::Cow | Species::Aurochs => "Beef",
            Species::Boar => "Boar Meat",
            Species::Fox => "Fox Meat",
            Species::Hare => "Rabbit Meat",
            Species::Wolf => "",
        }
    }

    // Name-driven lookup kept for spawn-table and tooling use; gameplay code
    // addresses species by enum value directly.
    #[allow(dead_code)]
    pub fn from_name(name: &str) -> Option<Species> {
        Some(match name {
            "deer" => Species::Deer,
            "fox" => Species::Fox,
            "wolf" => Species::Wolf,
            "boar" => Species::Boar,
            "cow" => Species::Cow,
            "hare" => Species::Hare,
            "aurochs" => Species::Aurochs,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Player {
        player: usize,
    },
    Animal {
        species: Species,
    },
    /// Harvestable tree; `stage` counts remaining harvests.
    Tree {
        harvests: u8,
    },
    /// Growing crop (docs/mechanics/livestock/farming-and-plants.md).
    /// `spec` indexes `farming::CROPS`; `stage` is the wire sdt byte.
    Crop {
        spec: u8,
        stage: u8,
    },
    /// Construction plan (crafting-and-building.md, building pipeline).
    /// `spec` indexes `build::BUILDABLES`; `stage` is the wire sdt byte
    /// re-rendered on every material delivery (crop growth pattern).
    Plan {
        spec: u8,
        stage: u8,
    },
    /// Finished station gob; `lit` is the wire sdt byte (0/1).
    Station {
        spec: u8,
        lit: bool,
    },
    /// Finished plain structure (no station behavior).
    Structure {
        spec: u8,
    },
    Stone,
    /// Item lying on the ground. `label` carries the display name so food
    /// keeps its fep.conf identity from ground to inventory.
    Drop {
        resname_idx: u16,
        ql: u8,
        label: &'static str,
    },
}

/// Linear movement state (OD_LINBEG / OD_LINSTEP).
#[derive(Debug, Clone, Copy)]
pub struct LinMove {
    pub sx: i32,
    pub sy: i32,
    pub tx: i32,
    pub ty: i32,
    /// Total steps for the move (client interpolates per step).
    pub steps: i32,
    /// Current progress in steps, monotonically increasing.
    pub step: i32,
}

/// Per-entity data. Hot columns are separate Vecs; extra state is in the
/// sparse `extras` map (cold path only).
pub struct Gobs {
    pub pos: Vec<(i32, i32)>,
    pub res_idx: Vec<u16>,
    pub frame: Vec<u32>,
    pub alive: Vec<bool>,
    pub kind: Vec<Kind>,
    pub hp: Vec<i32>,
    pub max_hp: Vec<i32>,
    pub speed: Vec<i32>,
    pub mv: Vec<Option<LinMove>>,
    /// Generation counter for id reuse safety.
    pub gen: Vec<u32>,
    free: Vec<usize>,
    /// Dirty-cell spatial index maintained by every mutator (spawn, kill,
    /// set_pos); see visidx.rs for the skip-proof semantics.
    pub vis: crate::visidx::VisIndex,
}

impl Gobs {
    pub fn new() -> Self {
        Gobs {
            pos: Vec::new(),
            res_idx: Vec::new(),
            frame: Vec::new(),
            alive: Vec::new(),
            kind: Vec::new(),
            hp: Vec::new(),
            max_hp: Vec::new(),
            speed: Vec::new(),
            mv: Vec::new(),
            gen: Vec::new(),
            free: Vec::new(),
            vis: crate::visidx::VisIndex::default(),
        }
    }

    /// Spawn returns a stable, never-reused-in-session gob id (slot index +
    /// generation baked into the id keeps ids unique across the process).
    pub fn spawn(
        &mut self,
        kind: Kind,
        pos: (i32, i32),
        res_idx: u16,
        hp: i32,
        speed: i32,
    ) -> GobId {
        let slot = match self.free.pop() {
            Some(s) => s,
            None => {
                self.pos.push((0, 0));
                self.res_idx.push(0);
                self.frame.push(0);
                self.alive.push(false);
                self.kind.push(Kind::Stone);
                self.hp.push(0);
                self.max_hp.push(0);
                self.speed.push(0);
                self.mv.push(None);
                self.gen.push(0);
                self.pos.len() - 1
            }
        };
        self.gen[slot] = self.gen[slot].wrapping_add(1);
        self.pos[slot] = pos;
        self.res_idx[slot] = res_idx;
        self.alive[slot] = true;
        self.kind[slot] = kind;
        self.hp[slot] = hp;
        self.max_hp[slot] = hp;
        self.speed[slot] = speed;
        self.mv[slot] = None;
        self.frame[slot] = 0;
        self.vis.insert(gob_id_from_slot(slot, self.gen[slot]), pos);
        gob_id_from_slot(slot, self.gen[slot])
    }

    pub fn kill(&mut self, id: GobId) -> bool {
        let (slot, _) = split_gob_id(id);
        if slot < self.alive.len() && self.alive[slot] {
            self.alive[slot] = false;
            self.mv[slot] = None;
            self.vis.remove(id);
            true
        } else {
            false
        }
    }

    /// The single position mutator outside `spawn`: reindexes the
    /// dirty-cell index so visibility skips stay provably correct.
    pub fn set_pos(&mut self, slot: usize, pos: (i32, i32)) {
        self.pos[slot] = pos;
        self.vis
            .reposition(gob_id_from_slot(slot, self.gen[slot]), pos);
    }

    #[inline]
    pub fn get(&self, id: GobId) -> Option<usize> {
        let (slot, g) = split_gob_id(id);
        if slot < self.alive.len() && self.alive[slot] && self.gen[slot] == g {
            Some(slot)
        } else {
            None
        }
    }
}

/// Gob id layout: high 16 bits generation, low 16 bits slot. This keeps the
/// wire int32 id unique per session lifetime while allowing slot reuse.
#[inline]
pub fn gob_id_from_slot(slot: usize, gen: u32) -> GobId {
    (((gen & 0xFFFF) as i32) << 16) | (slot as i32 & 0xFFFF)
}

#[inline]
pub fn split_gob_id(id: GobId) -> (usize, u32) {
    ((id & 0xFFFF) as usize, ((id >> 16) & 0xFFFF) as u32)
}

/// One inventory stack. `label` is the server-sent display name (item
/// widget tooltip, Item.name() precedence in food-and-fep.md); empty for
/// non-food items whose resource tooltip the client resolves locally.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InvStack {
    pub res: u16,
    pub count: u32,
    pub ql: u8,
    pub label: &'static str,
}

/// Character vitals and attributes (docs/mechanics/character/*).
#[derive(Debug, Clone)]
pub struct Player {
    pub name: String,
    pub gob: GobId,
    pub session: SessionId,
    /// Health 0..100 (SHP); reaching 0 knocks out then kills.
    pub hp: i32,
    /// Energy / hunger pool 0..100 ("hngr" meter).
    pub energy: i32,
    /// Stamina 0..100, drained by sprint/fight, refilled by energy.
    pub stamina: i32,
    /// Learning points (LP currency).
    pub lp: i32,
    /// Fractional LP accrual carry (ms toward the next point; see
    /// `skills::accrue`).
    pub lp_carry_ms: u64,
    /// Purchased non-incrementable skills (`gfx/hud/skills/` basenames).
    pub skills: HashSet<&'static str>,
    /// Base attributes (docs: str/agi/int/vit/con/psy/emp...).
    pub attrs: HashMap<String, i32>,
    /// Inventory stacks.
    pub inv: Vec<InvStack>,
    /// Food Event Point accumulators (integer tenths per attribute).
    pub fep: crate::craft::FepState,
    /// Currently open fight target (gob id) or none.
    pub fight_target: Option<GobId>,
    /// Attack cooldown in ticks.
    pub atk_cd: i32,
}

/// Live combat bars for one animal engaged with a player.
pub struct AnimalFight {
    /// Animal's offence bar toward the player (scaled percentage).
    pub off: i32,
    /// Animal's defence bar against player attacks (scaled percentage).
    pub def: i32,
}

/// A connected, in-world client's outbound message sinks.
///
/// Two channels mirror the legacy split: widget/control traffic rides the
/// reliable MSG_REL stream; MAPDATA/OBJDATA travel as raw datagrams with
/// their own ack/retransmit regimes (client re-requests lost grids, and
/// OBJACK gates gob state retransmission).
pub struct SessionOut {
    #[allow(dead_code)] // echoed in session teardown bookkeeping
    pub sid: SessionId,
    pub queue: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    pub raw: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    pub player_gob: Option<GobId>,
    /// Gobs currently streamed to this client.
    pub visible: HashSet<GobId>,
    /// Unacked OBJDATA blocks per gob (frame -> encoded block) for retransmit.
    pub unacked: HashMap<GobId, HashMap<u32, Vec<u8>>>,
    /// Widget id counter (session-local uint16 space).
    pub next_wid: u16,
    pub widgets: HashMap<u16, String>,
    /// Pending map requests (grid coord -> requested tick).
    #[allow(dead_code)] // pending MAPREQ dedup, consumed by grid streaming
    pub mapreqs: HashSet<(i32, i32)>,
    /// Session-local resource id table.
    pub res: ResTable,
    /// Fightview window state (widget id + per-opponent relations).
    pub fight: crate::fight::FightState,
    /// Open makewindow recipe id (`craft.rs::Recipe::id`), if any.
    pub craft_recipe: Option<String>,
    /// Widget id of the open makewindow, paired with `craft_recipe`.
    pub craft_window: Option<u16>,
    /// Inventory stack index whose right-click opened the flower menu,
    /// paired with the `sm` widget id (eat flow).
    pub item_menu: Option<(u16, usize)>,
    /// Item widget id -> inventory stack index (iact routing).
    pub item_wids: HashMap<u16, usize>,
    /// Open harvest flower menu: `sm` widget id -> target crop gob.
    pub crop_menu: Option<(u16, GobId)>,
    /// Widget id of the Area Chat window (`slenchat`), 0 = none.
    pub chat_wid: u16,
    /// Widget id of the party roster (`pv`), 0 = none.
    pub party_wid: u16,
    /// Open player flower menu and what it arms (party invite
    /// choreography; see `party::PlayerMenu`).
    pub player_menu: Option<(u16, crate::party::PlayerMenu)>,
    /// Plow Field pagina armed: next map click plows the tile.
    pub pending_plow: bool,
    /// Build pagina armed (`build::BUILDABLES` index): the mapview ghost
    /// is up and the next mapview `place` wdgmsg commits it.
    pub pending_build: Option<usize>,
    /// Open station flower menu: `sm` widget id -> target station gob.
    pub station_menu: Option<(u16, GobId)>,
    /// Item stack currently held on the cursor (take -> itemact flow).
    pub cursor: Option<InvStack>,
    /// Map grids this client already holds (MAPDATA re-send targeting).
    pub grids_seen: HashSet<(i32, i32)>,
    /// Last tick's player cell (dirty-cell skip decision; None = first
    /// tick, which always scans).
    pub vis_cell: Option<(i32, i32)>,
}

impl SessionOut {
    /// Widget id of the character sheet window, if created.
    pub fn chr_window(&self) -> Option<u16> {
        self.widgets
            .iter()
            .find(|(_, t)| t.as_str() == "chr")
            .map(|(id, _)| *id)
    }

    pub fn new_wid(&mut self, tag: &str) -> u16 {
        let id = self.next_wid;
        self.next_wid = self.next_wid.wrapping_add(1).max(1);
        self.widgets.insert(id, tag.to_owned());
        id
    }

    /// Queue a reliable RMSG sub-message payload.
    pub fn send(&self, payload: Vec<u8>) {
        // The only error is a closed receiver (session gone); dropping the
        // payload then is correct.
        let _ = self.queue.send(payload);
    }

    /// Send a raw datagram (MSG_MAPDATA / MSG_OBJDATA), unreliable.
    pub fn send_raw(&self, datagram: Vec<u8>) {
        let _ = self.raw.send(datagram);
    }
}

/// Walkability + speed multiplier per tile (server-side rule, not visible
/// in this client; documented in map-and-terrain.md "Open questions").
#[inline]
pub fn tile_speed(t: u8) -> Option<i32> {
    // Returns allowed speed in subtiles/s, or None for impassable.
    match t {
        tile::DEEP_WATER => None,
        tile::WATER => None,
        tile::MOUNTAIN | tile::CAVE => None,
        tile::CONIFER | tile::BROADLEAF => Some((BASE_SPEED * 6) / 10),
        tile::SWAMP1 => Some((BASE_SPEED * 5) / 10),
        tile::SAND => Some((BASE_SPEED * 8) / 10),
        tile::MOOR | tile::HEATH => Some((BASE_SPEED * 9) / 10),
        _ => Some(BASE_SPEED),
    }
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
    pub animal_fights: HashMap<GobId, AnimalFight>,
    /// Growing crops by gob id (farming tick + harvest lookup).
    pub crops: HashMap<GobId, crate::farm::CropState>,
    /// Tile -> crop gob occupying it (one crop per tile).
    pub crop_at: HashMap<(i32, i32), GobId>,
    /// Plowed (furrowed) tiles -> tilth state. `0` deadline = planted
    /// (never decays while the crop lives); a non-zero unix-ms deadline
    /// reverts the tile to grass when it passes.
    pub tilth: HashMap<(i32, i32), u64>,
    /// Construction plans by gob id (sinking + stage lookup).
    pub plans: HashMap<GobId, crate::build::PlanState>,
    /// Tile -> plan gob occupying it (one build site per tile).
    pub plan_at: HashMap<(i32, i32), GobId>,
    /// Finished stations by gob id (fuel/input/progress state).
    pub stations: HashMap<GobId, crate::build::StationState>,
    /// Tile -> finished structure gob occupying it.
    pub structure_at: HashMap<(i32, i32), GobId>,
    /// Formed parties (small vec; parties are capped and rare, linear
    /// scan by member is fine and keeps the hot paths untouched).
    pub parties: Vec<crate::party::PartyState>,
    /// Tick counter for deterministic scheduling.
    pub tick: u64,
    /// Deterministic RNG for AI (seeded from world seed).
    rng: hnh_world::JavaRandom,
    start_instant: Instant,
    /// Perf counters.
    pub perf: Perf,
}

#[derive(Default)]
pub struct Perf {
    pub last_tick_us: u128,
    pub max_tick_us: u128,
    /// Exponential moving average of tick cost (stable steady-state number
    /// for load reports; 50-tick half-life).
    pub mean_tick_us: u64,
    pub active_sessions: usize,
    pub visible_total: usize,
    pub spawned_objects: usize,
    /// Per-phase microseconds of the last tick: [movement, ai, combat,
    /// vitals, visibility]. Load-test hot-loop attribution.
    pub phase_us: [u128; 5],
    /// Visibility optimization counters (cumulative): total gob distance
    /// checks issued by scans, session-ticks skipped entirely, and the
    /// live cell count. Log-time proof the index is active.
    pub vis_gob_scans: u64,
    pub vis_skipped: u64,
    pub vis_cells: usize,
}

impl World {
    pub fn new(seed: u64) -> Self {
        World {
            seed,
            grids: hnh_world::GridStore::new(seed),
            gobs: Gobs::new(),
            res: ResTable::new(),
            players: Vec::new(),
            by_session: HashMap::new(),
            animal_gobs: Vec::new(),
            animal_fights: HashMap::new(),
            crops: HashMap::new(),
            crop_at: HashMap::new(),
            tilth: HashMap::new(),
            plans: HashMap::new(),
            plan_at: HashMap::new(),
            stations: HashMap::new(),
            structure_at: HashMap::new(),
            parties: Vec::new(),
            tick: 0,
            rng: hnh_world::JavaRandom::new(seed as i64),
            start_instant: Instant::now(),
            perf: Perf::default(),
        }
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.start_instant.elapsed().as_secs_f64()
    }

    /// Spawn deterministic world content for one grid: trees on forest
    /// tiles, stones elsewhere; density derived from tile + seeded rng.
    /// Called when a grid is first touched by gameplay.
    pub fn populate_grid(&mut self, gc: (i32, i32), out: &mut Vec<GobId>) {
        let grid = self.grids.grid(gc);
        let mut spawned = 0usize;
        for y in (0..100usize).step_by(1) {
            for x in 0..100usize {
                let t = grid.tile(x, y);
                let tx = gc.0 as i64 * 100 + x as i64;
                let ty = gc.1 as i64 * 100 + y as i64;
                let mut r = hnh_world::mkrandoom(tx as i32, ty as i32);
                let roll = r.next_bounded(1000);
                let res = match t {
                    tile::CONIFER if roll < 220 => {
                        let s = r.next_bounded(3);
                        match s {
                            0 => "gfx/terobjs/trees/fir",
                            1 => "gfx/terobjs/trees/pine",
                            _ => "gfx/terobjs/trees/atree",
                        }
                    }
                    tile::BROADLEAF if roll < 220 => {
                        let s = r.next_bounded(3);
                        match s {
                            0 => "gfx/terobjs/trees/birch",
                            1 => "gfx/terobjs/trees/maple/01",
                            _ => "gfx/terobjs/trees/oak",
                        }
                    }
                    tile::GRASS if roll < 8 => {
                        if r.next_bounded(2) == 0 {
                            "gfx/terobjs/bumlings/stone1"
                        } else {
                            "gfx/terobjs/bumlings/boulder"
                        }
                    }
                    tile::HEATH if roll < 6 => "gfx/terobjs/bumlings/stone1",
                    tile::MOOR if roll < 4 => "gfx/terobjs/bumlings/boulder",
                    _ => continue,
                };
                let res_idx = self.res.intern(res);
                // Object sits at tile corner subtile (tile * 11), matching
                // client flavor placement convention.
                let pos = ((tx as i32) * 11, (ty as i32) * 11);
                let kind = if res.contains("trees/") {
                    Kind::Tree { harvests: 5 }
                } else {
                    Kind::Stone
                };
                let id = self.gobs.spawn(kind, pos, res_idx, 100, 0);
                out.push(id);
                spawned += 1;
            }
        }
        self.perf.spawned_objects += spawned;
    }

    /// Spawn wildlife for a grid.
    pub fn populate_animals(&mut self, gc: (i32, i32), count: usize, out: &mut Vec<GobId>) {
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
                let species = match self.rng.next_bounded(7) {
                    0 => Species::Deer,
                    1 => Species::Fox,
                    2 => Species::Wolf,
                    3 => Species::Boar,
                    4 => Species::Cow,
                    5 => Species::Hare,
                    _ => Species::Aurochs,
                };
                let px = (gc.0 as i64 * 100 + x) as i32 * 11 + 5;
                let py = (gc.1 as i64 * 100 + y) as i32 * 11 + 5;
                let res_idx = self.res.intern(species.resname());
                let id = self.gobs.spawn(
                    Kind::Animal { species },
                    (px, py),
                    res_idx,
                    species.max_hp(),
                    species.speed(),
                );
                self.animal_gobs.push(id);
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
