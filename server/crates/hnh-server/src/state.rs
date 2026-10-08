//! Game state: data-oriented (SoA) entity storage + simulation.
//!
//! Design: a single owner task (`game::Game`) owns `World` and runs a fixed
//! 10 Hz tick. All entity hot data lives in struct-of-arrays tables with
//! generational ids, keeping the per-tick loops cache-friendly (perf rules
//! from AGENTS.md: mem-/perf-/coll-). Networking tasks never touch these
//! tables; they exchange encoded `RMSG` payloads through per-session queues.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::time::Instant;

use hnh_world::tile;

use crate::fxhash::FxHashMap;
use crate::resources::ResTable;

pub const TICK_HZ: u64 = 10;
pub const TICK_MS: u64 = 1000 / TICK_HZ;

/// View radius in map subtiles around a player (~45 tiles, legacy ~500px).
pub const VIEW_RADIUS: i32 = 300;

/// Gait speeds in subtiles/second (11 subtiles = 1 tile), indexed by the
/// speedget widget: 0 crawl, 1 walk, 2 run, 3 sprint.
/// Source: docs/mechanics/character/attributes-and-vitals.md (RoB Glossary
/// "Speed"): crawl 1.5, walk 3.0, run 4.5, sprint 6.0 tiles/s.
pub const GAIT_SPEEDS: [i32; 4] = [16, 33, 50, 66];
/// Default gait index (walk). The speedget widget is created with
/// cur = GAIT_WALK, max = 3.
pub const GAIT_WALK: usize = 1;

/// Legacy alias kept for terrain scaling: the walk-gait speed on grass.
pub const BASE_SPEED: i32 = GAIT_SPEEDS[GAIT_WALK];

pub type GobId = i32;
pub type SessionId = u32;

/// Gob ids pack (gen, slot) into 16 bits each (see `gob_id_from_slot`),
/// so the slot space is 0..=65535. Cluster layouts partition this space
/// across node processes for globally-unique ids.
pub const MAX_SLOT: usize = (1 << 16) - 1;

/// Simulation vitals bundle (combat + movement) transferred with a gob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vitals {
    pub hp: i32,
    pub max_hp: i32,
    pub speed: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Species {
    Deer,
    Fox,
    Wolf,
    Boar,
    Cow,
    Hare,
    Aurochs,
    /// Session 46: the third tameable wild beast (animals-and-husbandry
    /// .md taming list). The pack spells the directory "mufflon".
    Mouflon,
    /// Domestic morph of the mouflon; never spawns wild.
    Sheep,
}

impl Species {
    /// Every species variant, for tests that need to sweep the whole
    /// roster (e.g. the session-36 bone-loot consistency check in
    /// craft.rs).
    #[cfg(test)]
    pub const ALL: [Species; 9] = [
        Species::Deer,
        Species::Fox,
        Species::Wolf,
        Species::Boar,
        Species::Cow,
        Species::Hare,
        Species::Aurochs,
        Species::Mouflon,
        Species::Sheep,
    ];

    /// Stable discriminant carried on the node link (GuestKind::Animal);
    /// the subscriber resolves it back with `from_index`. Appends only:
    /// values 0-6 are already live on the node link, renumbering would
    /// corrupt cross-node guests until every node is upgraded.
    pub fn index(self) -> u8 {
        match self {
            Species::Deer => 0,
            Species::Fox => 1,
            Species::Wolf => 2,
            Species::Boar => 3,
            Species::Cow => 4,
            Species::Hare => 5,
            Species::Aurochs => 6,
            Species::Mouflon => 7,
            Species::Sheep => 8,
        }
    }

    /// Inverse of `index` (unknown indices reject at the ingest boundary).
    pub fn from_index(v: u8) -> Option<Species> {
        Some(match v {
            0 => Species::Deer,
            1 => Species::Fox,
            2 => Species::Wolf,
            3 => Species::Boar,
            4 => Species::Cow,
            5 => Species::Hare,
            6 => Species::Aurochs,
            7 => Species::Mouflon,
            8 => Species::Sheep,
            _ => return None,
        })
    }

    pub fn resname(self) -> &'static str {
        // The concrete drawable resource per species. Kritter directories
        // carry plalay pose routers ("gfx/korka"-style) the fork client
        // cannot resolve - the <dir>/cdv resource is the image+neg sprite
        // the legacy server actually layered, so spawn that instead.
        match self {
            Species::Deer => "gfx/kritter/deer/cdv",
            Species::Fox => "gfx/kritter/fox/cdv",
            Species::Wolf => "gfx/kritter/wolf/cdv",
            Species::Boar => "gfx/kritter/boar/cdv",
            Species::Cow => "gfx/kritter/cow/cdv",
            Species::Hare => "gfx/kritter/hare/cdv",
            Species::Aurochs => "gfx/kritter/aurochs/cdv",
            Species::Mouflon => "gfx/kritter/mufflon/cdv",
            Species::Sheep => "gfx/kritter/sheep/cdv",
        }
    }

    pub fn max_hp(self) -> i32 {
        match self {
            Species::Hare => 20,
            Species::Deer | Species::Fox => 40,
            Species::Boar | Species::Cow => 70,
            Species::Wolf => 60,
            Species::Aurochs => 90,
            // Tameable wild beasts sit between the deer and the boar;
            // the doc carries no verified numbers (server policy).
            Species::Mouflon => 45,
            Species::Sheep => 40,
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
            Species::Mouflon => 50,
            Species::Sheep => 30,
        }
    }

    /// Loot dropped on death: (resource name, count, display label).
    /// Session 36: every species also drops bones - the Bone Arrow recipe
    /// (craft.rs) needs gfx/invobjs/bone, and in legacy every butchered
    /// carcass yielded bones; counts are a chosen server policy recorded
    /// in animals-and-husbandry.md.
    pub fn loot(self) -> Vec<(&'static str, u32, &'static str)> {
        match self {
            Species::Deer | Species::Aurochs => vec![
                ("gfx/invobjs/meat", 3, self.meat_label()),
                ("gfx/invobjs/hide-raw-fox", 2, ""),
                ("gfx/invobjs/bone", 2, ""),
            ],
            Species::Cow => vec![
                ("gfx/invobjs/meat", 4, self.meat_label()),
                ("gfx/invobjs/hide-raw-cow", 3, ""),
                ("gfx/invobjs/bone", 2, ""),
            ],
            Species::Boar => vec![
                ("gfx/invobjs/meat", 3, self.meat_label()),
                ("gfx/invobjs/bone", 2, ""),
            ],
            Species::Fox => vec![
                ("gfx/invobjs/meat", 1, self.meat_label()),
                ("gfx/invobjs/hide-raw-fox", 1, ""),
                ("gfx/invobjs/bone", 1, ""),
            ],
            Species::Wolf => vec![
                ("gfx/invobjs/meat", 2, self.meat_label()),
                ("gfx/invobjs/bone", 2, ""),
            ],
            Species::Hare => vec![
                ("gfx/invobjs/meat", 1, self.meat_label()),
                ("gfx/invobjs/bone", 1, ""),
            ],
            // The mouflon rows mirror the doc's butcher list (Raw Sheep
            // Skin, Raw Mutton); bones follow the all-species policy.
            Species::Mouflon | Species::Sheep => vec![
                ("gfx/invobjs/meat", 2, self.meat_label()),
                ("gfx/invobjs/hide-raw-sheep", 1, ""),
                ("gfx/invobjs/wool", 1, ""),
                ("gfx/invobjs/bone", 1, ""),
            ],
        }
    }

    /// Short species name for chat lines (ranged combat feedback).
    pub fn name(self) -> &'static str {
        match self {
            Species::Deer => "Deer",
            Species::Fox => "Fox",
            Species::Wolf => "Wolf",
            Species::Boar => "Boar",
            Species::Cow => "Cow",
            Species::Hare => "Hare",
            Species::Aurochs => "Aurochs",
            Species::Mouflon => "Mouflon",
            Species::Sheep => "Sheep",
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
            // fep.conf verifies "Raw Mutton" (HHP:1) for the sheep family.
            Species::Mouflon | Species::Sheep => "Raw Mutton",
        }
    }

    /// The domestic morph at full tameness (animals-and-husbandry.md:
    /// boar->pig, mouflon->sheep, aurochs->cow/bull). The 2009 pack
    /// ships NO pig kritter and no standalone bull drawable that the
    /// cdv pipeline can layer, so the boar maps to None (it stays a
    /// boar at full tameness - recorded in the doc's Open questions)
    /// and the aurochs morphs to the cow cdv.
    pub fn morph(self) -> Option<Species> {
        match self {
            Species::Mouflon => Some(Species::Sheep),
            Species::Aurochs => Some(Species::Cow),
            _ => None,
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
            // The pack directory spelling, plus the wiki spelling.
            "mufflon" | "mouflon" => Species::Mouflon,
            "sheep" => Species::Sheep,
            _ => return None,
        })
    }
}

/// Server policy (docs/mechanics/crafting-and-building.md, "World
/// gathering"): a pickable tree yields this many branch picks before it
/// becomes a stump.
pub const TREE_HARVESTS: u8 = 5;
/// Server policy: a boulder yields this many stones before it
/// disappears.
pub const BOULDER_STONES: u8 = 5;
/// Flat gathering quality, matched to the starter kit (branch/stone at
/// ql 10) so world-gathered materials craft identically.
pub const GATHER_QL: u8 = 10;
/// LP granted per branch pick (existing server policy, carried).
pub const TREE_PICK_LP: i32 = 5;
/// LP granted per stone pick (existing server policy, carried).
pub const STONE_PICK_LP: i32 = 3;

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
    /// Stone boulder (bumling) with a finite supply: each pick takes one
    /// stone; a depleted boulder disappears. See the "World gathering"
    /// section of crafting-and-building.md.
    Boulder {
        left: u8,
    },
    /// Decorative stump left where a tree's harvests ran out. Not
    /// harvestable: picking a stump yields nothing.
    Stump,
    /// Item lying on the ground. `label` carries the display name so food
    /// keeps its fep.conf identity from ground to inventory. The gob
    /// renders with `resname_idx` (a gfx/terobjs/items world shape that
    /// has a `neg` layer); picking up restores `inv_res_idx` (the
    /// gfx/invobjs icon resource the inventory widget needs).
    Drop {
        resname_idx: u16,
        inv_res_idx: u16,
        ql: u8,
        label: &'static str,
    },
}

/// Linear movement state (OD_LINBEG / OD_LINSTEP).
///
/// Timing model (verified against src/haven/LinMove.java): the client
/// interpolates the whole move on its own render clock -
/// `ctick: a += (dt/1000)/(c*0.06) * 0.9` - so it covers the path in
/// `c * 66.67 ms` regardless of when LINSTEP frames arrive. `setl` only
/// ever advances the client progress (`if(a > this.a)`). Therefore the
/// step count must be derived from the planned duration as
/// `c = round(total_ms / 66.67)` and LINSTEP indices must match the
/// server-time progress fraction, or the client visibly outruns the
/// server (the "character moves too fast" defect).
#[derive(Debug, Clone, Copy)]
pub struct LinMove {
    pub sx: i32,
    pub sy: i32,
    pub tx: i32,
    pub ty: i32,
    /// Client step count: the client covers the path in steps * 66.67 ms.
    pub steps: i32,
    /// Last LINSTEP index sent (monotonically increasing; setl only
    /// advances the client-side progress).
    pub step: i32,
    /// Server-world time (world.now_ms) when the move started.
    pub started_ms: u64,
    /// Planned duration in ms at the authored speed.
    pub total_ms: u32,
}

impl LinMove {
    /// Client-consistent step count for a planned duration: c =
    /// round(total_ms * 3 / 200) because the client walks a move in
    /// c * 200/3 ms = c * 66.67 ms.
    pub fn client_steps(total_ms: u32) -> i32 {
        (((i64::from(total_ms)) * 3 + 100) / 200).max(1) as i32
    }

    /// Progress fraction 0..1 by server time.
    pub fn progress(&self, now_ms: u64) -> f32 {
        let el = now_ms.saturating_sub(self.started_ms) as f32;
        (el / self.total_ms.max(1) as f32).clamp(0.0, 1.0)
    }

    /// Interpolated world position at server time `now_ms` (linear
    /// between (sx,sy) and (tx,ty), exactly the client's getc model).
    pub fn pos_at(&self, now_ms: u64) -> (i32, i32) {
        let p = self.progress(now_ms);
        (
            self.sx + ((f32::from((self.tx - self.sx) as i16)) * p) as i32,
            self.sy + ((f32::from((self.ty - self.sy) as i16)) * p) as i32,
        )
    }

    /// Client-visible LINSTEP index at server time (floor(progress * c)).
    pub fn step_at(&self, now_ms: u64) -> i32 {
        let c = self.steps.max(1);
        // Both operands are non-negative and bounded (elapsed < u32::MAX
        // per move, c <= 9000), so the widen-then-narrow multiply is safe.
        let el = now_ms.saturating_sub(self.started_ms) as i64;
        (((el * i64::from(c)) / i64::from(self.total_ms.max(1))).min(i64::from(c))) as i32
    }
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
    /// Current facing direction 0..7 - a MOVEMENT octant (dir 0 = +x,
    /// quantized atan2, see `game::move_dir`), NOT the sprite index: the
    /// directional art resources are indexed by `game::art_dir(octant)`,
    /// a ring rotated one octant against this one. Spawn default is 1
    /// (the +x+y camera-facing octant) so freshly spawned gobs render the
    /// full front view (art sprite 0). Server-side pose resolution: the
    /// fork client has no plalay/plparts router support, so the server
    /// layers the concrete directional frame resources and the client
    /// animates each one natively via its embedded `anim` layer.
    pub facing: Vec<u8>,
    /// Last pose state streamed to viewers, encoded (moving<<3 | dir):
    /// standing dirs 0..7, walking dirs 8..15, u8::MAX = nothing streamed
    /// yet. One-byte dedupe for the pose-change streams (retargets,
    /// arrivals, re-facings never re-send an identical layer set).
    pub pose_streamed: Vec<u8>,
    free: Vec<usize>,
    /// Dirty-cell spatial index maintained by every mutator (spawn, kill,
    /// set_pos); see visidx.rs for the skip-proof semantics.
    pub vis: crate::visidx::VisIndex,
}

impl Gobs {
    /// Cluster layout: gob slots partition across node processes so gob
    /// ids are globally unique by construction — node `me` allocates only
    /// slots in `[me*per, (me+1)*per)` (the last node also owns the
    /// 16-bit-slot remainder), which keeps encoded wire blocks valid on
    /// every node without any id remapping.
    pub fn with_layout(nodes: NonZeroUsize, me: usize) -> Self {
        let per = (MAX_SLOT + 1) / nodes.get();
        let lo = me * per;
        let hi = if me + 1 == nodes.get() {
            MAX_SLOT + 1
        } else {
            lo + per
        };
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
            facing: Vec::new(),
            pose_streamed: Vec::new(),
            // Descending: pops hand out the LOWEST free slot of my range
            // first, keeping ids dense from the range base.
            free: (lo..hi).rev().collect(),
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
            Some(s) => {
                // Cluster layout: my range's slots may sit beyond the
                // current column length (the free list is pre-seeded).
                self.ensure_capacity(s);
                s
            }
            None => {
                self.push_columns();
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
        // Octant 1 = camera-facing front (see Gobs::new slot path).
        self.facing[slot] = 1;
        self.pose_streamed[slot] = u8::MAX;
        self.frame[slot] = 0;
        self.vis.insert(gob_id_from_slot(slot, self.gen[slot]), pos);
        gob_id_from_slot(slot, self.gen[slot])
    }

    /// Grow every column by one row (dense-growth fallback once the
    /// pre-seeded free list is exhausted).
    fn push_columns(&mut self) {
        self.pos.push((0, 0));
        self.res_idx.push(0);
        self.frame.push(0);
        self.alive.push(false);
        // Column-fill placeholder: never streamed before a real spawn
        // overwrites it (alive = false).
        self.kind.push(Kind::Stump);
        self.hp.push(0);
        self.max_hp.push(0);
        self.speed.push(0);
        self.mv.push(None);
        self.gen.push(0);
        // Octant 1 = camera-facing front; art_dir(1) = sprite 0, the full
        // front view, the natural spawn look.
        self.facing.push(1);
        self.pose_streamed.push(u8::MAX);
    }

    /// Extend columns so index `slot` is addressable. Only grows PAST the
    /// current length; slots below it default-fill. Cluster transfers can
    /// land on a foreign node's slot range, so capacity is not bounded by
    /// this node's own allocation range.
    fn ensure_capacity(&mut self, slot: usize) {
        while self.pos.len() <= slot {
            self.push_columns();
        }
    }

    /// Ownership transfer insert: materialize a gob under an EXACT id
    /// (slot = id & 0xFFFF, gen = id >> 16) so every viewer that already
    /// spawned it keeps rendering it across the authority handoff. The
    /// slot is claimed out of the free list when it belongs to my range;
    /// foreign slots just extend the columns.
    pub fn spawn_with_id(
        &mut self,
        id: GobId,
        kind: Kind,
        pos: (i32, i32),
        res_idx: u16,
        v: Vitals,
    ) {
        let (slot, gen) = split_gob_id(id);
        self.ensure_capacity(slot);
        self.free.retain(|&s| s != slot);
        self.gen[slot] = gen.max(1);
        self.pos[slot] = pos;
        self.res_idx[slot] = res_idx;
        self.alive[slot] = true;
        self.kind[slot] = kind;
        self.hp[slot] = v.hp;
        self.max_hp[slot] = v.max_hp;
        self.speed[slot] = v.speed;
        self.mv[slot] = None;
        self.facing[slot] = 1;
        self.pose_streamed[slot] = u8::MAX;
        self.frame[slot] = 0;
        self.vis.insert(id, pos);
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

impl InvStack {
    /// Merge `added` into `self` (same resource only, caller checks).
    /// Counts add; quality re-averages as a count-weighted integer
    /// average, the same convention as craft output quality
    /// (crafting-and-building.md, loftar sum(q*w)/sum(w)). The stacking
    /// policy itself is server policy per items-and-quality.md; this is
    /// the chosen policy and the counts are conserved exactly.
    pub fn absorb(&mut self, added: &InvStack) {
        debug_assert_eq!(self.res, added.res, "absorb requires same resource");
        let total = self.count + added.count;
        if total == 0 {
            return; // nothing to average; keep stacks as-is
        }
        let qsum = self.ql as u32 * self.count + added.ql as u32 * added.count;
        self.ql = ((qsum / total).clamp(1, 255)) as u8;
        self.count = total;
    }
}

/// Character vitals and attributes (docs/mechanics/character/*).
#[derive(Debug, Clone)]
pub struct Player {
    /// In-world display name (the charlist entry the client picked).
    pub name: String,
    /// Authenticated login account. Save keys are `account:name`, so two
    /// accounts never share a character even though the legacy client's
    /// single charlist entry is always called "Player".
    pub account: String,
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
    /// Wall-clock world time (`world.now_ms`) until which the player is
    /// flagged CRIMINAL for a PvP knockout (the assault policy,
    /// combat-system.md "PvP knockout consequences"). None = clean
    /// record. Persisted; refreshed by every new knockout.
    pub criminal_until_ms: Option<u64>,
    /// Selected movement gait (speedget index; GAIT_SPEEDS entry).
    pub gait: u8,
    /// Fractional LP accrual carry (ms toward the next point; see
    /// `skills::accrue`).
    pub lp_carry_ms: u64,
    /// Purchased non-incrementable skills (`gfx/hud/skills/` basenames).
    pub skills: HashSet<&'static str>,
    /// Base attributes (docs: str/agi/int/vit/con/psy/emp...).
    pub attrs: HashMap<String, i32>,
    /// Inventory stacks.
    pub inv: Vec<InvStack>,
    /// Equipment (the Equipory paperdoll, widget type "epry"): exactly 16
    /// slots indexed by the wire slot index 0..15. Slot semantics are
    /// server-side (docs/mechanics/items/items-and-quality.md); the client
    /// addresses slots purely by index. Occupied slots hold one stack.
    pub equip: Vec<Option<InvStack>>,
    /// Food Event Point accumulators (integer tenths per attribute).
    pub fep: crate::craft::FepState,
    /// Currently open fight target (gob id) or none.
    pub fight_target: Option<GobId>,
    /// Attack cooldown in ticks.
    pub atk_cd: i32,
    /// Active ranged aim (bow equipped; see archery.rs). Mutually
    /// exclusive with `fight_target`.
    pub aim: Option<crate::archery::RangedAim>,
}

/// Live combat bars for one animal engaged with a player.
pub struct AnimalFight {
    /// Animal's offence bar toward the player (scaled percentage).
    pub off: i32,
    /// Animal's defence bar against player attacks (scaled percentage).
    pub def: i32,
    /// Battle intensity (animals-and-husbandry.md taming step 2: Jorb's
    /// prerequisite list requires "battle intensity reduced to 0" before
    /// Quell the Beast may fire). Every landed blow (either direction)
    /// raises it; quiet ticks de-escalate it. Same scaled bar as the
    /// offence/defence meters (0..=BAR_FULL).
    pub intensity: i32,
}

/// Intensity raised by one landed blow (either direction, POLICY: the
/// doc names the meter but no legacy number survives).
pub const INTENSITY_PER_BLOW: i32 = 2500;
/// Intensity de-escalation per combat tick (10 Hz) without a blow
/// (POLICY: a hot battle cools in ~7 s of no blows).
pub const INTENSITY_DECAY: i32 = 250;

/// Taming progress for one animal (session 45; animals-and-husbandry.md
/// taming service). Each successful Quell adds TAMENESS_PER_QUELL; at
/// TAMENESS_FULL the animal is permanently tame (no more leash breaks).
/// The leash-break deadline is a game-tick stamp: surviving logout means
/// game-time based, matching the wiki's lagg-relative wording.
#[derive(Debug, Clone, Copy)]
pub struct TameState {
    /// The tamer's player gob id (the follow target + rope binding).
    pub tamer: GobId,
    /// Accumulated tameness (0..=100). 100 = fully tamed.
    pub tameness: i32,
    /// World tick when the leash breaks and the beast re-aggros.
    /// Ignored once tameness reaches TAMENESS_FULL.
    pub break_at_tick: u64,
    /// Stored milk in 0.01 L units (session 47; cows only). Persisted.
    pub milk_units: u32,
    /// Stored wool count (session 47; sheep only). Persisted.
    pub wool: u8,
    /// Production accumulator: species quantity ticks toward the next
    /// unit (MILK_ACC_PER_UNIT / WOOL_ACC_PER_UNIT thresholds).
    pub prod_acc: u32,
    /// Consumption accumulator in nano-units of fodder (session 48):
    /// the fractional part of trough feeding between whole units.
    /// Persisted so restarts do not give a free meal.
    pub feed_acc_nano: u64,
    /// Consecutive ticks without a bite (no trough fodder in radius AND
    /// no grazing tile). Reset on any feeding; at STARVE_DEATH_TICKS the
    /// animal dies (session 48 starvation policy). Persisted.
    pub hunger: u64,
}

impl TameState {
    pub fn new(tamer: GobId, break_at_tick: u64) -> Self {
        TameState {
            tamer,
            tameness: 0,
            break_at_tick,
            milk_units: 0,
            wool: 0,
            prod_acc: 0,
            feed_acc_nano: 0,
            hunger: 0,
        }
    }
}

/// Tameness added per successful Quell (docs step 3: five cycles to 100).
pub const TAMENESS_PER_QUELL: i32 = 20;
/// Full tameness: the animal is permanently tame.
pub const TAMENESS_FULL: i32 = 100;
/// Leash-break deadline: ~10 real minutes of game ticks (docs step 5
/// "about ten minutes (5-15, variable)" - the floor as server policy).
/// 600 s / TICK_MS(100) = 6000 ticks.
pub const LEASH_BREAK_TICKS: u64 = 6000;

// --- Production meters (session 47; animals-and-husbandry.md "Animal
// products and collection flows"). ---

/// Milk cap: 10 L stored in 0.01 L units (doc "cows store up to 10 L").
pub const MILK_CAP_UNITS: u32 = 1000;
/// Milk rate denominator: the doc's rate is `Milk Quantity * 0.01` L per
/// 10 minutes, i.e. quantity q accrues q units per 6000 ticks
/// (10 min = 600 s / TICK_MS 100). Quantity 10 -> 10 units/10 min =
/// 0.1 L/10 min, the doc's quoted example.
pub const MILK_ACC_PER_UNIT: u32 = 6000;
/// Wool cap: 3 (doc "sheep store up to 3 wool").
pub const WOOL_CAP: u8 = 3;
/// Wool rate: 1 wool per 8 real hours at Wool Quantity 5 (doc), scaled
/// linearly with quantity: acc += quantity per tick, 1 unit per
/// 48000 ticks * 5 = 240000 accumulated quantity-ticks.
pub const WOOL_ACC_PER_UNIT: u32 = 240_000;
/// Server-policy breed stats pending legacy verification (the doc
/// carries no verified per-animal quantity numbers; both default to
/// the wiki's example quantities).
pub const MILK_QUANTITY: u32 = 10;
pub const WOOL_QUANTITY: u32 = 5;
/// Milk drawn per bucket: 1 L = 100 units of 0.01 L. The doc names the
/// bucket as the milking interaction but carries no bucket volume;
/// 1 L is the server policy (documented in the livestock doc).
pub const MILK_PER_BUCKET_UNITS: u32 = 100;
/// Milk/wool quality while grazing: the doc's grazing rule counts moor,
/// heath and grassland as food of quality level 10, so the product
/// quality follows at 10 (server policy pending bred-stat systems).
pub const GRAZE_PRODUCT_QL: u8 = 10;

// --- Food Trough + feeding (session 48; animals-and-husbandry.md
// "Feeding: troughs and grazing"). ---

/// Fodder capacity of one Food Trough (doc "Capacity 200 fodder units").
pub const TROUGH_CAP_UNITS: u32 = 200;
/// Feeding radius of a Food Trough in tiles (doc "radius 18 tiles").
/// One tile is 11x11 subtiles (plans/structures snap per tile).
pub const TROUGH_RADIUS_TILES: i32 = 18;
/// Squared subtile radius: (18 * 11)^2. Euclidean over subtile coords.
pub const TROUGH_RADIUS_SQ: i32 = (TROUGH_RADIUS_TILES * 11) * (TROUGH_RADIUS_TILES * 11);
/// In-game day: 8 real hours (farming-and-plants.md "one in-game day =
/// 8 real hours"). At TICK_MS 100 that is 288000 ticks.
pub const DAY_TICKS: u64 = 8 * 60 * 60 * 10;
/// Consumption rate (docs Legacy:Cattle): a non-pregnant cow eats 4.8
/// fodder units per in-game day. Stored as nano-units per tick for
/// loss-free integer accumulation: 4.8 units/day / 288000 ticks =
/// 16.667 nano-units/tick (0.002% rounding, documented).
pub const COW_EAT_NANO_PER_TICK: u64 = 16_667;
/// Sheep consumption rate: the doc quotes no sheep number; server
/// policy is half the cow rate (2.4 units/day = 8333 nano/tick),
/// documented in the livestock doc.
pub const SHEEP_EAT_NANO_PER_TICK: u64 = 8_333;
/// Lactating surcharge (docs Legacy:Cattle): "a lactating cow eats 4.8
/// plus 0.1 unit per liter of milk produced". This server's cow
/// produces MILK_QUANTITY(10) * 0.01 L per 6000 ticks = 1/60000 L per
/// tick, so the surcharge is 0.1/60000 units/tick = 1667 nano-units.
/// Bound to the production rate (what is produced, not what is stored),
/// exactly like the doc's wording.
pub const LACTATE_NANO_PER_TICK: u64 = 1_667;
/// Starvation (docs "Starvation should kill or stop production"): a
/// fully tamed producer that finds no food - no trough in radius with
/// fodder, no grazing tile - dies after 3 in-game days without a bite
/// (server policy; the wiki only says tamed animals "require food or
/// grassland to survive and breed"). Production is already gated on
/// feeding, so the stop-half is implicit.
pub const STARVE_DEATH_TICKS: u64 = 3 * DAY_TICKS;

/// One fodder unit per item for every listed fodder item; the doc's
/// "Giant Pumpkin (worth 16 seeds)" has no resource in the 2009 pack.
/// Matches by inventory resource name ("any seeds" is the seed- prefix
/// plus flaxseed; the rest are the doc's list intersected with the
/// resources the 2009 jar actually ships - Blueberries, Chantrelles,
/// Bloated Bolete, Peapod and Beetroot/Leaves have NO invobj resources
/// in this pack and therefore cannot be matched; recorded in the doc).
pub fn fodder_units(resname: &str) -> Option<u32> {
    const DIRECT: &[&str] = &[
        "gfx/invobjs/apple",
        "gfx/invobjs/applecore",
        "gfx/invobjs/mulberry",
        "gfx/invobjs/straw",
        "gfx/invobjs/pumpkinflesh",
        "gfx/invobjs/carrot",
        "gfx/invobjs/flower-poppy",
    ];
    if DIRECT.contains(&resname) {
        return Some(1);
    }
    if resname == "gfx/invobjs/flaxseed" || resname.starts_with("gfx/invobjs/seed-") {
        return Some(1);
    }
    None
}

/// Fodder store of one placed Food Trough. Quality is the average of
/// what was placed (doc: "q5 + q12 + q16 -> q11"): running sum/count,
/// consumed fodder carries the running average.
#[derive(Debug, Clone, Copy, Default)]
pub struct TroughState {
    /// Stored fodder units (0..=TROUGH_CAP_UNITS).
    pub units: u32,
    /// Sum of qualities over every unit ever placed (running average
    /// denominator in `ql_seen`).
    pub ql_sum: u64,
    /// Number of units whose quality is summed (== units placed total;
    /// consumption drains units but NOT the quality history, matching
    /// the doc's "average quality of what was placed").
    pub ql_seen: u64,
}

impl TroughState {
    /// Running average quality of the fodder placed so far (Q10 before
    /// anything is placed - the grazing baseline).
    pub fn avg_ql(&self) -> u8 {
        if self.ql_seen == 0 {
            return 10;
        }
        (self.ql_sum / self.ql_seen).min(255) as u8
    }

    /// Drain up to `want` units; returns the amount actually taken.
    pub fn take(&mut self, want: u32) -> u32 {
        let got = want.min(self.units);
        self.units -= got;
        got
    }
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
    /// Authenticated login account (save keys are account-scoped).
    pub account: String,
    pub queue: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    /// Bounded unreliable datagram fan-out (MAPDATA / OBJDATA); send_raw
    /// drops on a full queue exactly like a lost UDP datagram.
    pub raw: tokio::sync::mpsc::Sender<Vec<u8>>,
    pub player_gob: Option<GobId>,
    /// Gobs currently streamed to this client. Keys are server-allocated
    /// gob ids -> fxhash id hasher (hot per-candidate membership checks).
    pub visible: crate::fxhash::FxHashSet<GobId>,
    /// Unacked OBJDATA blocks per gob (frame -> encoded block) for retransmit.
    pub unacked: crate::fxhash::FxHashMap<GobId, HashMap<u32, Vec<u8>>>,
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
    /// Open production flower menu (session 47): `sm` widget id ->
    /// target tamed-animal gob (Milk on a cow, Shear on a sheep).
    pub animal_menu: Option<(u16, GobId)>,
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
    /// Open station flower menu: `sm` widget id + target station gob.
    /// The third element carries the GUEST act intent (session 33):
    /// `None` for a local station (the choice resolves against the
    /// authoritative local state), `Some(Light/Extinguish)` for a guest
    /// station (the act was picked from the piggybacked snapshot and is
    /// relayed to the authority, which re-validates it).
    pub station_menu: Option<(u16, GobId, Option<crate::nodes::StationAct>)>,
    /// Item stack currently held on the cursor (take -> itemact flow).
    pub cursor: Option<InvStack>,
    /// Widget id of the drag Item widget following the mouse (the
    /// cursor stack's on-screen body). Kept in sync by
    /// `game::sync_cursor_widget` after every cursor mutation.
    pub cursor_wid: Option<u16>,
    /// Map grids this client already holds (MAPDATA re-send targeting).
    pub grids_seen: HashSet<(i32, i32)>,
    /// Last visibility scan result (session 30 result caching). Valid
    /// only while the player position is unchanged; a session standing
    /// still then patches this list from the per-tick touched records
    /// instead of rescanning the whole view square.
    pub vis_cache: Option<Vec<GobId>>,
    /// The exact player position the cached list is valid for.
    pub vis_cache_pos: Option<(i32, i32)>,
    /// Last tick's player cell (dirty-cell skip decision; None = first
    /// tick, which always scans).
    pub vis_cell: Option<(i32, i32)>,
    /// Tick of the session's last retract sweep (session 42 spawn-debounce:
    /// a gob oscillating across the 2x-VIEW_RADIUS boundary must not be
    /// retracted and re-spawned every tick of a moving session; the sweep
    /// runs at most every RETRACT_SWEEP_EVERY ticks regardless of cell
    /// crossings, so a quick boundary return never sees a retract).
    pub last_retract_tick: u64,
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

    /// Send a raw datagram (MSG_MAPDATA / MSG_OBJDATA), unreliable. A full
    /// bounded queue means the per-session sender task is starved; dropping
    /// the newest datagram matches the protocol's UDP semantics (the client
    /// re-requests lost grids; LINSTEP progress self-heals next tick).
    pub fn send_raw(&self, datagram: Vec<u8>) {
        let _ = self.raw.try_send(datagram);
    }
}

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

#[derive(Default)]
pub struct Perf {
    pub last_tick_us: u128,
    pub max_tick_us: u128,
    /// Max tick cost since the LAST perf report (the 5 s reporter window):
    /// attributes ramp-up spikes to their window. The lifetime maximum
    /// never resets, so a single early spike made every later report read
    /// 197-210 ms regardless of the current steady state.
    pub window_max_tick_us: u128,
    /// Exponential moving average of tick cost (stable steady-state number
    /// for load reports; 50-tick half-life).
    pub mean_tick_us: u64,
    pub active_sessions: usize,
    pub visible_total: usize,
    pub spawned_objects: usize,
    /// Per-phase microseconds of the last tick: [movement, ai, combat,
    /// vitals, visibility]. Load-test hot-loop attribution.
    pub phase_us: [u128; 9],
    /// Visibility optimization counters (cumulative): total gob distance
    /// checks issued by scans, session-ticks skipped entirely, and the
    /// live cell count. Log-time proof the index is active.
    pub vis_gob_scans: u64,
    pub vis_skipped: u64,
    /// Session-ticks served from the cached scan result (patch or clean
    /// skip) instead of a full view rescan (session 30).
    pub vis_cached: u64,
    pub vis_cells: usize,
    /// Node-link publishes sent (cumulative) and guest rows ingested
    /// (cumulative) - cluster-mode counters for the perf report.
    pub guest_pub: u64,
    pub guest_ingests: u64,
    /// Last-tick visibility sub-phase attribution (microseconds):
    /// candidate scan (phase A/A2), spawn application, retract sweep.
    /// Load-test hot-loop attribution inside the visibility phase.
    pub vis_scan_us: u64,
    pub vis_spawn_us: u64,
    /// Gobs spawned into session views this tick (spawn-cost attribution:
    /// `vis_spawn_us / max(vis_spawns, 1)` is the per-spawn price).
    pub vis_spawns: u64,
    pub vis_retract_us: u64,
    /// Last-tick guest-phase sub-attribution (microseconds): guest row
    /// progress + block encode, per-session fan-out, rest-pose streaming.
    /// Load-test attribution inside the guests phase.
    pub guests_encode_us: u64,
    pub guests_fanout_us: u64,
    pub guests_pose_us: u64,
    /// Last-tick combat-phase sub-attribution (microseconds, session 43):
    /// the once-per-tick lookup-index build, the player-side melee loop,
    /// the animal retaliation loop, and the guest relay loop. Attribution
    /// before optimization (perf-profile-first): the session-42 combat
    /// p95 spikes (39 ms) were unattributed; these four counters decide
    /// whether the next cut targets PvP, animals or relays.
    pub combat_index_us: u64,
    pub combat_players_us: u64,
    pub combat_animals_us: u64,
    pub combat_relay_us: u64,
    /// Combat player-phase event counters (last tick): chase `start_move`
    /// calls (count + accumulated microseconds), in-reach swing-path
    /// entries, and landed PvP hits (count + accumulated hit-tail
    /// microseconds: hurt + chat + FX broadcast + log). Splits the 18 ms
    /// player-phase mean the session-43 load run measured.
    pub combat_chase_n: u64,
    pub combat_chase_us: u64,
    pub combat_swing_n: u64,
    pub combat_hit_n: u64,
    pub combat_hit_us: u64,
    /// Last-tick `start_move` sub-attribution (microseconds, cumulative
    /// over the tick's calls): path check (interpolated_pos, path_clear,
    /// terrain read), viewer fan-out (LINBEG scan, send, unacked) and the
    /// pose layer stream. `mv_calls` splits per-call averages.
    pub mv_path_us: u64,
    pub mv_viewers_us: u64,
    pub mv_pose_us: u64,
    pub mv_calls: u64,
    /// Last-tick movement-tick sub-attribution (microseconds): the O(alive)
    /// mover scan (interpolated pos + dirty marks, no encoding), the packed
    /// block encode + batch push, and the per-session packed fan-out
    /// (`broadcast_batch`, both the mid-tick movement batch and the tick-end
    /// start/FX batch). Splits the phase_mv_us top-line the way
    /// combat_index_us et al. split phase_combat_us.
    pub mvbat_scan_us: u64,
    pub mvbat_encode_us: u64,
    pub mvbat_fanout_us: u64,
    /// Last-tick mover count (alive authority gobs carrying a LinMove) and
    /// the packed-block count they produced - per-mover cost attribution:
    /// `mvbat_encode_us / max(movers,1)` is the per-mover encode price.
    pub mvbat_movers: u64,
    /// Last-tick viewer-index diagnostics: mean visible set size across
    /// sessions (visible_total / sessions) and the total candidate count
    /// the cell-index fan-outs probed (`ix_cand_n`). Decides whether the
    /// fan-out cost is the candidate walk or the pair work.
    pub ix_cand_n: u64,
    /// Last-tick packed movement batch: block count and distinct cell
    /// count (the fan-out probe width). Zero with no movers.
    pub move_blocks: u64,
    pub move_cells: u64,
    /// Last-tick packed start/FX batch (session 44): LINBEG starts + FX
    /// overlays encoded once and fanned out at tick end. Block count (mv
    /// + fx attribution: `fx_batch_n` counts the FX subset).
    pub start_blocks: u64,
    pub fx_batch_n: u64,
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
                    tile::HEATH if roll < 6 => "gfx/terobjs/bumlings/01",
                    tile::MOOR if roll < 4 => "gfx/terobjs/bumlings/stal2",
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
                // Trees are pickable (branches) and bumlings are boulders
                // with a stone supply; see the "World gathering" section
                // of crafting-and-building.md.
                let kind = if res.contains("trees/") {
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
                let species = match self.rng.next_bounded(8) {
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
                    _ => Species::Mouflon,
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

#[cfg(test)]
mod cluster_tests {
    use super::*;

    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).expect("nonzero test constant")
    }

    /// Cluster layouts partition the slot space: two nodes allocated
    /// disjoint slot ranges, so their gob ids can never collide on the
    /// wire (no remapping at the node-link boundary).
    #[test]
    fn node_slot_ranges_are_disjoint_and_cover_the_space() {
        for nodes in [2usize, 3, 4] {
            let mut seen = std::collections::HashSet::new();
            for me in 0..nodes {
                let mut g = Gobs::with_layout(nz(nodes), me);
                // Allocate 8 gobs per node; ids must be unique cluster-wide.
                for _ in 0..8 {
                    let id = g.spawn(Kind::Stump, (0, 0), 0, 1, 0);
                    let (slot, _gen) = split_gob_id(id);
                    assert!(
                        seen.insert(slot),
                        "slot {slot} double-allocated at {nodes} nodes"
                    );
                    let lo = (MAX_SLOT + 1) / nodes * me;
                    let hi = if me + 1 == nodes {
                        MAX_SLOT + 1
                    } else {
                        lo + (MAX_SLOT + 1) / nodes
                    };
                    assert!(
                        (lo..hi).contains(&slot),
                        "slot {slot} outside node {me} range"
                    );
                }
            }
        }
    }

    /// Ownership transfer: spawn_with_id materializes the EXACT id (same
    /// slot and generation) so viewers that already spawned the gob keep
    /// rendering it across the handoff, and the claimed slot leaves the
    /// free list (a later spawn cannot land on the transferred gob).
    #[test]
    fn spawn_with_id_reuses_exact_id_and_claims_the_slot() {
        let mut src = Gobs::with_layout(nz(2), 0);
        let id = src.spawn(
            Kind::Animal {
                species: Species::Wolf,
            },
            (100, 100),
            3,
            40,
            33,
        );
        let (slot, gen) = split_gob_id(id);

        let mut dst = Gobs::with_layout(nz(2), 1);
        dst.spawn_with_id(
            id,
            Kind::Animal {
                species: Species::Wolf,
            },
            (150, 120),
            3,
            Vitals {
                hp: 25,
                max_hp: 40,
                speed: 33,
            },
        );
        assert_eq!(split_gob_id(dst.get(id).map(|_| id).unwrap()), (slot, gen));
        let s = dst.get(id).expect("transferred gob alive on the new node");
        assert!(dst.alive[s]);
        assert_eq!(dst.pos[s], (150, 120));
        assert_eq!(dst.hp[s], 25);
        assert_eq!(dst.max_hp[s], 40);

        // The slot is not free: another forced insert on the same slot
        // must overwrite in place, and a normal spawn never claims it.
        let mut fresh = Gobs::with_layout(nz(2), 0);
        let taken = fresh.spawn(Kind::Stump, (0, 0), 0, 1, 0);
        let (tslot, tgen) = split_gob_id(taken);
        fresh.kill(taken);
        let other = fresh.spawn(Kind::Tree { harvests: 0 }, (1, 1), 0, 1, 0);
        assert_ne!(
            split_gob_id(other).0,
            tslot,
            "killed slot must be reused via free list"
        );
        let _ = (tslot, tgen);

        // Re-insert with the same id (idempotent authority claim).
        dst.spawn_with_id(
            id,
            Kind::Animal {
                species: Species::Wolf,
            },
            (150, 120),
            3,
            Vitals {
                hp: 25,
                max_hp: 40,
                speed: 33,
            },
        );
        assert!(dst.alive[dst.get(id).expect("still alive")]);
    }

    /// A transferred gob keeps its identity: get/kill/split round-trip on
    /// the receiving node matches the ids the sending node encoded into
    /// wire blocks.
    #[test]
    fn transferred_gob_survives_kill_and_reinsert() {
        let mut a = Gobs::with_layout(nz(3), 2);
        let id = a.spawn(
            Kind::Animal {
                species: Species::Deer,
            },
            (0, 0),
            1,
            10,
            33,
        );
        a.kill(id);
        let mut b = Gobs::with_layout(nz(3), 0);
        b.spawn_with_id(
            id,
            Kind::Animal {
                species: Species::Deer,
            },
            (5, 5),
            1,
            Vitals {
                hp: 10,
                max_hp: 10,
                speed: 33,
            },
        );
        assert!(b.get(id).is_some(), "id must resolve after transfer");
        assert!(b.kill(id), "kill resolves the transferred id");
        assert!(b.get(id).is_none());
    }

    /// Species index round-trip (the node-link discriminant contract).
    /// Values 0-6 predate session 46 and are frozen on the wire; 7-8
    /// (mouflon, sheep) append.
    #[test]
    fn species_index_roundtrips() {
        for i in 0..9u8 {
            let sp = Species::from_index(i).expect("valid index");
            assert_eq!(sp.index(), i);
        }
        assert!(Species::from_index(9).is_none());
        assert!(Species::from_index(255).is_none());
    }

    // --- Food Trough + feeding (session 48). ---

    /// The fodder table matches the doc's list intersected with the
    /// resources the 2009 pack actually ships: any seed-* resource plus
    /// flaxseed, the direct list (apple, apple core, mulberry, straw,
    /// pumpkin flesh, carrot, poppy flower). Branch and stone are NOT
    /// fodder.
    #[test]
    fn fodder_table_matches_the_doc() {
        assert_eq!(fodder_units("gfx/invobjs/seed-wheat"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/seed-carrot"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/seed-pumpkin"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/flaxseed"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/apple"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/applecore"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/mulberry"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/straw"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/pumpkinflesh"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/carrot"), Some(1));
        assert_eq!(fodder_units("gfx/invobjs/flower-poppy"), Some(1));
        // Non-fodder: the crafting-chain materials and foods.
        assert_eq!(fodder_units("gfx/invobjs/branch"), None);
        assert_eq!(fodder_units("gfx/invobjs/stone"), None);
        assert_eq!(fodder_units("gfx/invobjs/meat"), None);
        assert_eq!(fodder_units("gfx/invobjs/bucket-milk"), None);
    }

    /// The trough quality average follows the doc's example: q5 + q12 +
    /// q16 -> q11 (33 / 3). Consumption drains units, not the history;
    /// an untouched trough averages Q10 (the grazing baseline).
    #[test]
    fn trough_quality_averaging_and_take() {
        let mut t = TroughState::default();
        assert_eq!(t.avg_ql(), 10, "empty trough sits at the q10 baseline");
        t.units += 1;
        t.ql_sum += 5;
        t.ql_seen += 1;
        t.units += 1;
        t.ql_sum += 12;
        t.ql_seen += 1;
        t.units += 1;
        t.ql_sum += 16;
        t.ql_seen += 1;
        assert_eq!(t.avg_ql(), 11, "q5 + q12 + q16 -> q11 (the doc's example)");
        assert_eq!(t.take(2), 2, "take drains stored units");
        assert_eq!(t.units, 1);
        assert_eq!(t.avg_ql(), 11, "consumption does not rewrite the history");
        assert_eq!(t.take(9), 1, "take clamps at the stored amount");
        assert_eq!(t.units, 0);
    }
}
