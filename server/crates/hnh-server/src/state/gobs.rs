//! Gob storage and identity: the struct-of-arrays table, gob
//! kinds, linear movement, and the (gen, slot) id packing.

use std::num::NonZeroUsize;

use super::gathering::{ForageKind, OreKind};
use super::species::Species;
use super::GobId;

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
    /// Fruit-bearing apple tree (session 81, the apple pie chain):
    /// `left` counts the remaining apple picks; an exhausted tree
    /// degrades to a plain branch-yielding [`Kind::Tree`] (the legacy
    /// stage-6 apple tree yields Apples AND Branches from the same
    /// gob). Renders as `gfx/terobjs/trees/appletree`.
    FruitTree {
        left: u8,
    },
    /// Forageable wild plant (session 81, the dough ingredient
    /// chains): single pick consumes the plant and drops its item
    /// (legacy forageables are picked once; see farming-and-plants.md
    /// "Wild plants vs planted crops").
    Forage {
        forage: ForageKind,
    },
    /// Wild beehive (session 81, the honeybun chain): a bucket-gated
    /// honey source. Each pick consumes one empty bucket and one honey
    /// unit; at zero units further picks get the empty-hive refusal.
    /// The hive itself is a permanent fixture (never removed).
    BeeHive {
        honey: u8,
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
    /// Metal-bearing ore deposit on rocky terrain (the mining leg of the
    /// metal chain): each pick yields one ore item of `ore`; a depleted
    /// deposit disappears. Renders as the mining ore-heap sprite so
    /// players and probes can tell deposits from plain boulders.
    OreDeposit {
        ore: OreKind,
        left: u8,
    },
    /// Clay deposit on the sandy shore belt (session 69, the kiln
    /// chain's gathering leg): each pick yields one Clay item; a
    /// depleted deposit disappears. Renders as the same mining heap
    /// sprite as the ore deposits (the heap family is this pack's
    /// deposit-pile sprite set; distinguishing clay from ore needs the
    /// pickup - server policy recorded in the mechanics doc).
    ClayDeposit {
        left: u8,
    },
    /// Decorative stump left where a tree's harvests ran out. Not
    /// harvestable: picking a stump yields nothing.
    Stump,
    /// Item lying on the ground. `label` carries the display name so food
    /// keeps its fep.conf identity from ground to inventory. The gob
    /// renders with `resname_idx` (a gfx/terobjs/items world shape that
    /// has a `neg` layer); picking up restores `inv_res_idx` (the
    /// gfx/invobjs icon resource the inventory widget needs). `count`
    /// is the stack size one pickup grants (1 for the classic single
    /// drops; FORAGE_YIELD handfuls for the session-81 forage picks).
    Drop {
        resname_idx: u16,
        inv_res_idx: u16,
        ql: u8,
        label: &'static str,
        count: u32,
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
}
