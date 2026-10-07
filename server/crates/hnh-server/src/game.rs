//! The game task: owns the world simulation, streams state to sessions.
//!
//! One task owns the whole `World` (single-writer, SoA layout) and runs a
//! fixed 10 Hz tick. Session tasks feed it commands via an mpsc channel and
//! receive encoded `RMSG` payloads through per-session queues. This split
//! keeps the hot loop allocation-light and the network tasks wait-free.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use tracing::{debug, info, trace};

use hnh_proto::consts::*;
use hnh_proto::MessageBuf;

use crate::craft::FepAttr;
use crate::farm;
use crate::resources::wdg::{self, ListVal};
use crate::state::*;

use hnh_world::tile;

// Despawn horizon for dropped items; consumed by the item-despawn pass
// landing together with persistence.
#[allow(dead_code)]
pub const ITEM_DROP_LIFETIME_TICKS: u64 = 20 * 300; // 5 minutes

/// Avatar part resource templates. `{pose}` is `standing` or `walking`;
/// `{dir}` is the art pack's 8-direction sprite index (0..7), produced
/// from a movement octant by `art_dir` (NOT the raw octant: the art ring
/// is rotated one octant against the movement ring - see `art_dir`).
/// Every directional resource embeds its full animation client-side
/// (standing = 1 frame, walking = 8 frames @100 ms through the
/// resource's own `anim` layer), so the server selects ONE direction
/// set per pose and NEVER streams frames: cycling the direction sets in
/// sequence is what made the avatar spin around its own axis (session
/// 21 defect).
const AVATAR_PART_TEMPLATES: [&str; 6] = [
    "gfx/borka/body/{pose}/legs-{dir}",
    "gfx/borka/body/{pose}/torso/male-{dir}",
    "gfx/borka/body/{pose}/head-{dir}",
    "gfx/borka/body/{pose}/arm/idle/left-{dir}",
    "gfx/borka/body/{pose}/arm/idle/right-{dir}",
    "gfx/borka/hair-karin/{pose}/hair-{dir}",
];

/// Equipment-window doll pose: standing with banzai arms (spread),
/// sprite index 0 = the art pack's full front view (the +x+y octant,
/// straight at the camera). Drawn by `Equipory.cdraw` from the gob's
/// `Avatar` attribute (OD_AVATAR), which is distinct from the world
/// drawable (OD_LAYERS) - the doll keeps the spread-arms pose while the
/// world avatar walks.
const AVATAR_DOLL_TEMPLATES: [&str; 6] = [
    "gfx/borka/body/standing/legs-0",
    "gfx/borka/body/standing/torso/male-0",
    "gfx/borka/body/standing/head-0",
    "gfx/borka/body/standing/arm/banzai/left-0",
    "gfx/borka/body/standing/arm/banzai/right-0",
    "gfx/borka/hair-karin/standing/hair-0",
];

/// The avatar base resource every OD_LAYERS player block references (a
/// load gate client-side: it carries only the plalay router the fork
/// client drops, and is never sprite-created).
const AVATAR_BASE: &str = "gfx/borka/body";

/// Quantize a movement vector into the movement octant (0..7).
/// Pure, deterministic, unit-tested: dir 0 = +x, dir 2 = +y, dir 4 = -x,
/// dir 6 = -y (counterclockwise atan2 octants). These are MOVEMENT
/// octants, not sprite indices: the directional art resources are
/// indexed by a ring rotated one octant against this one - convert with
/// `art_dir` before composing layer names (session 22 defect: feeding
/// the octant straight into the sprite index shifted every walk
/// animation one octant clockwise on screen).
pub fn move_dir((sx, sy): (i32, i32), (tx, ty): (i32, i32)) -> u8 {
    let (dx, dy) = (tx - sx, ty - sy);
    if dx == 0 && dy == 0 {
        return 0;
    }
    let deg = (dy as f64).atan2(dx as f64).to_degrees();
    // Euclidean division keeps the wraparound exact at both ends of the
    // -180..180 range: -180 deg lands on dir 4, +180 deg on dir 4 too.
    let octant = ((deg + 22.5).div_euclid(45.0)) as i32;
    octant.rem_euclid(8) as u8
}

/// Map a movement octant (`move_dir`) to the art pack's directional
/// sprite index. The art ring is rotated one octant against the movement
/// ring: sprite 0 is the full FRONT view (the +x+y camera-facing octant),
/// sprite 4 the full back, the pure left/right profiles sit at sprites
/// 2/6, and the walking-into-frame 3/4 views fill the odd slots.
/// Verified by decoding the fox standing sprites (art 0 = head-on front,
/// art 1 = down-left 3/4, art 2 = pure left profile, art 3 = up-left
/// 3/4, art 4 = back, art 5 = up-right 3/4, art 6 = pure right profile,
/// art 7 = down-right 3/4; scripts/dump_directions.py) and by the user
/// report this fixes: walking up (octant 5) showed the up-right set
/// (sprite 5 = octant 6), walking left (octant 3) showed the up-left
/// set (sprite 3 = octant 4) - i.e. sprite N always depicts octant N+1,
/// so displaying octant D needs sprite (D - 1) mod 8.
#[inline]
pub fn art_dir(octant: u8) -> u8 {
    (octant.wrapping_add(7)) & 7
}

/// All concrete pose layer names, materialized ONCE as leaked 'static
/// strings (bounded set: 2 poses x 8 dirs x 6 avatar parts, 1 doll set,
/// 7 species x 2 poses x 8 dirs) so ResTable interns by reference and
/// every stream path composes layers with zero allocations (data-
/// oriented: flat fixed-size tables indexed by pose/dir/species).
struct PoseTable {
    /// [pose][dir][part]: pose 0 = standing, 1 = walking.
    avatar: [[[&'static str; 6]; 8]; 2],
    /// The equipment-window doll set (banzai arms, camera facing).
    doll: [&'static str; 6],
    /// [species][pose][dir], one body part per kritter pose.
    kritter: [[[&'static str; 8]; 2]; 9],
    /// [species] pose-router base resources.
    kritter_base: [&'static str; 9],
}

static POSES: std::sync::OnceLock<PoseTable> = std::sync::OnceLock::new();

/// Species index order must mirror the enum declaration order (state.rs).
/// Session 46 appends the mufflon (the pack's directory spelling) and
/// the sheep - both ship body pose directories like the original seven.
const SPECIES_FOLDERS: [&str; 9] = [
    "deer", "fox", "wolf", "boar", "cow", "hare", "aurochs", "mufflon", "sheep",
];

impl PoseTable {
    fn build() -> PoseTable {
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        let mut avatar: [[[&'static str; 6]; 8]; 2] = Default::default();
        for (pi, pose) in ["standing", "walking"].into_iter().enumerate() {
            for d in 0u8..8 {
                // Table stays indexed by movement octant; the emitted
                // resource name carries the art sprite index.
                let dir_char = (b'0' + art_dir(d)) as char;
                for (ti, t) in AVATAR_PART_TEMPLATES.into_iter().enumerate() {
                    avatar[pi][d as usize][ti] = leak(
                        t.replace("{pose}", pose)
                            .replace("{dir}", &dir_char.to_string()),
                    );
                }
            }
        }
        let mut kritter: [[[&'static str; 8]; 2]; 9] = Default::default();
        for (si, sp) in SPECIES_FOLDERS.into_iter().enumerate() {
            for (pi, pose) in ["standing/standing", "walking/walking"]
                .into_iter()
                .enumerate()
            {
                for d in 0u8..8 {
                    let art = art_dir(d);
                    kritter[si][pi][d as usize] =
                        leak(format!("gfx/kritter/{sp}/body/{pose}-{art}"));
                }
            }
        }
        PoseTable {
            avatar,
            doll: AVATAR_DOLL_TEMPLATES,
            kritter,
            kritter_base: [
                "gfx/kritter/deer/body",
                "gfx/kritter/fox/body",
                "gfx/kritter/wolf/body",
                "gfx/kritter/boar/body",
                "gfx/kritter/cow/body",
                "gfx/kritter/hare/body",
                "gfx/kritter/aurochs/body",
                "gfx/kritter/mufflon/body",
                "gfx/kritter/sheep/body",
            ],
        }
    }
}

fn poses() -> &'static PoseTable {
    POSES.get_or_init(PoseTable::build)
}

/// Concrete avatar layer names for one pose + direction.
fn avatar_pose_layers(moving: bool, dir: u8) -> &'static [&'static str; 6] {
    &poses().avatar[usize::from(moving)][(dir & 7) as usize]
}

/// Equipment doll layer names (banzai pose, camera facing).
fn avatar_doll_layers() -> &'static [&'static str; 6] {
    &poses().doll
}

/// Kritter pose-router base per species (a load gate client-side, never
/// sprite-created: it carries only the plalay router layer).
fn kritter_base(sp: Species) -> &'static str {
    poses().kritter_base[sp as usize]
}

/// The one kritter body pose part for a species + pose + direction. Each
/// directional resource embeds its animation (standing = 1 frame, walking
/// = 8 frames @50 ms), so a single layer carries the whole pose and the
/// client animates it natively.
fn kritter_pose_layer(sp: Species, moving: bool, dir: u8) -> &'static str {
    poses().kritter[sp as usize][usize::from(moving)][(dir & 7) as usize]
}

/// Commands session tasks send into the game task.
pub enum Cmd {
    Wdgmsg {
        sid: SessionId,
        wid: u16,
        name: String,
        args: Vec<hnh_proto::ListArg>,
    },
    MapReq {
        sid: SessionId,
        gc: (i32, i32),
    },
    ObjAck {
        sid: SessionId,
        acks: Vec<(GobId, u32)>,
    },
    SessionClosed {
        sid: SessionId,
    },
    ReportPerf {},
    /// Graceful stop: flush persistence and exit the loop.
    Shutdown {},
    /// Inbound node-link message (cluster mode only).
    NodeMsg(crate::nodes::NodeMsg),
}

pub struct Game {
    pub world: World,
    pub sessions: HashMap<SessionId, SessionOut>,
    pub rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    pub net_rx: tokio::sync::mpsc::UnboundedReceiver<crate::net::NetCmd>,
    pub saturated: bool,
    next_sid: SessionId,
    /// Sequence for one-shot FX overlay ids (masked to 15 bits; the wire
    /// id shifts left once for the persist flag, session 21 bite FX).
    overlay_seq: u32,
    /// Grids already populated with objects/animals.
    populated: HashSet<(i32, i32)>,
    /// Character persistence store (loaded snapshots + live updates).
    pub save: crate::persist::SaveStore,
    /// Sessions waiting on a cluster character migration (save key held by
    /// a peer). A CharData reply or the deadline advances the entry.
    pending_joins: HashMap<SessionId, PendingJoin>,
    /// Parsed etc/needed/fep.conf (food -> FEP vector).
    pub fep: crate::craft::FepTable,
    /// Number of parallel grid-owner workers used by the tick (data-parallel
    /// intent computation over SoA columns; apply stays on the game task).
    pub workers: usize,
    /// Packed movement-block scratch (session 41): one shared byte buffer
    /// + cell index for both the local-mover and the guest fan-out.
    ///
    /// Reused across ticks via `clear` (no hot-path allocator churn).
    move_scratch: crate::move_batch::MoveBatch,
    /// Packed start/FX block scratch (session 44): LINBEG move starts and
    /// one-shot FX overlays emitted anywhere in the tick (combat chase,
    /// hit tails, click handling) encode ONCE into this batch and fan out
    /// through `broadcast_batch` at the END of the tick - one datagram per
    /// session per tick instead of one per viewer per event. Same reuse
    /// contract as `move_scratch`.
    start_scratch: crate::move_batch::MoveBatch,
    /// Scratch for `broadcast_batch`'s session anchor positions (taken/
    /// restored; avoids two per-tick allocations at the 1000-session
    /// scale - two batch fan-outs per tick).
    fan_scratch: Vec<(SessionId, (i32, i32))>,
    /// Combat-phase lookup indexes (session 43): rebuilt once per tick in
    /// one O(players) pass, reused across ticks (mem-reuse-collections).
    ///
    /// Replaces the per-tick O(N) `players` scans the melee paths ran
    /// per attacker/per animal (O(N^2) aggregate at the 1000-session
    /// load scale) with O(1) slot-indexed lookups:
    /// - `player_of_slot`: gob slot -> player index + 1 (0 = no player).
    ///   Gob slots are dense and players never leave `world.players`
    ///   mid-tick (knockout resets bars, removal happens in the logout
    ///   path), so the map stays valid for the whole combat phase.
    /// - `engaged_of_slot`: gob slot of an animal fight target ->
    ///   player index + 1, first (lowest) player index wins - the same
    ///   "first engaged player" semantics the linear `find` had.
    combat_ix: CombatIndex,
    /// Milliseconds of online time per granted LP (skills.rs accrual;
    /// precomputed once from HNH_LP_RATE, u64::MAX = disabled).
    lp_ms_per_lp: u64,
    /// Multi-node cluster state. `None` in the default single-node process:
    /// every cell is owned by node 0, authority checks short-circuit and no
    /// mesh socket exists (zero added tick cost vs the single-node build).
    pub cluster: Option<Cluster>,
}

/// A session whose world entry waits on a cluster character migration:
/// the save key lives on a peer, which answers `NodeMsg::CharData`. If no
/// answer arrives before the deadline the entry proceeds without a
/// snapshot (fresh spawn) - a downed peer must not block logins.
struct PendingJoin {
    account: String,
    chosen: String,
    deadline: std::time::Instant,
    /// Peers that answered (CharNack, or CharData which removes the join
    /// outright). Entry proceeds early once every peer answered - the
    /// deadline only bounds a dead-link cluster.
    answered: HashSet<usize>,
    /// Tick of the next CharQuery re-broadcast (mesh links buffer across
    /// reconnects; a retry closes the query/reply race on a link that is
    /// still negotiating).
    next_retry: u64,
}

/// Re-broadcast an unanswered CharQuery every 700 ms (10 Hz tick).
const CHAR_QUERY_RETRY_TICKS: u64 = 7;
/// A pending join gives up after 6 s and enters fresh (a downed cluster
/// must not block logins forever).
const CHAR_QUERY_DEADLINE_MS: u64 = 6_000;

/// Per-tick combat lookup indexes (see `Game::combat_ix`). Both vectors
/// keep capacity between ticks: `resize` fills only the delta, so the
/// steady-state build is a single linear pass with no allocator traffic.
#[derive(Default)]
struct CombatIndex {
    /// Gob slot -> player index + 1 (0 = no player at that slot).
    player_of_slot: Vec<u32>,
    /// Gob slot of a fight target -> engaged player index + 1. Only
    /// players with a live `fight_target` enter the map; the first
    /// (lowest) player index wins a shared target (the old linear
    /// `find` semantics).
    engaged_of_slot: Vec<u32>,
    /// Scratch snapshot of `animal_gobs` entries with a live engagement,
    /// in `animal_gobs` order (the old retaliation loop's iteration
    /// order). Rebuilt each tick; taken and restored to reuse capacity.
    engaged_animals: Vec<GobId>,
    /// Scratch snapshot of the guest_attackers rows (the relay
    /// retaliation loop iterates a copy: rows mutate mid-loop).
    relay_rows: Vec<(GobId, GobId)>,
    /// Scratch snapshot of session ids for the 5 Hz fight-bar streaming
    /// pass (rows mutate mid-loop through `sessions.get_mut`).
    bar_sids: Vec<SessionId>,
}

/// Movement fan-out square half-width (subtiles): a still-visible mover
/// sits inside the 2x VIEW_RADIUS retract hysteresis; the retract sweep
/// runs at most every 8 ticks, so an inter-sweep full-speed drift (8 x 50
/// subtile/tick) must be covered too. The rectangle test against this
/// span decides whether a cell can contain any still-visible mover; the
/// exact `visible.contains` filter stays authoritative per block.
const FANOUT_SPAN: i32 = 2 * VIEW_RADIUS + 8 * 50;

/// Minimum tick gap between retract sweeps of one session (session 42
/// spawn-debounce: see `Game::retract_sweep_due`). 8 ticks = 0.8 s.
const RETRACT_SWEEP_EVERY: u64 = 8;

/// LINSTEP progress frames ship every Nth tick (session 41): the client
/// interpolates the linmove locally from LINBEG (deterministic timing
/// model), so the per-tick server push is only a counter re-sync. At 2
/// the correction lands at 5 Hz and the (session, mover) pair fan-out -
/// the largest remaining fan-out cost in the duel-cohort cluster - and
/// the progress datagram traffic both halve; a lost datagram self-heals
/// within 2N ticks. Finalizers always ship (they end the move).
const LINSTEP_EVERY_TICKS: u64 = 2;

impl PendingJoin {
    /// True once every OTHER node answered the CharQuery.
    fn peers_answered(&self, nodes: usize) -> bool {
        nodes > 1 && self.answered.len() >= nodes - 1
    }
}

/// Live multi-node state (session 27): peer subscriptions, guest
/// publishing bookkeeping and the mesh handle. See `nodes.rs` for the wire
/// and `grid_owner.rs` for the ownership function.
pub struct Cluster {
    pub me: usize,
    pub nodes: std::num::NonZeroUsize,
    pub mesh: crate::nodes::Mesh,
    /// Per-peer subscribed cells: what THAT peer's sessions view inside MY
    /// cells (I stream guest updates for these).
    pub peer_subs: HashMap<usize, HashSet<(i32, i32)>>,
    /// My current subscriptions per peer: cells I view that the peer owns
    /// (mirrored locally to diff out Sub/Unsub deltas).
    pub my_subs: HashMap<usize, HashSet<(i32, i32)>>,
    /// Foreign owner node currently holding each of my abroad players
    /// (home node keeps authority and streams updates to that owner).
    pub player_abroad: HashMap<GobId, usize>,
}

/// Publish event kind for a local gob's guest stream.
#[derive(Clone, Copy)]
enum GuestEv {
    Announce,
    Update,
    Retract,
}

impl Game {
    pub fn new(
        seed: u64,
        rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
        net_rx: tokio::sync::mpsc::UnboundedReceiver<crate::net::NetCmd>,
        saturated: bool,
        save_path: std::path::PathBuf,
    ) -> Self {
        // The save path is resolved by main (HNH_SAVE_FILE env or
        // cwd-independent repo-root candidates); tests pass a throwaway path
        // and simply never touch the store.
        let save = crate::persist::SaveStore::load(&save_path, seed);
        // fep.conf ships with the repo (repo-root etc/); HNH_FEP_CONF moves
        // it for tests. A missing file degrades to "no food resolves" rather
        // than wedging the boot (food-and-fep.md server note 1). Candidates
        // cover both `cargo run` (cwd = server/) and direct binary launches.
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(env_path) = std::env::var("HNH_FEP_CONF") {
            candidates.push(std::path::PathBuf::from(env_path));
        }
        candidates.push(std::path::PathBuf::from("../etc/needed/fep.conf"));
        candidates.push(std::path::PathBuf::from("etc/needed/fep.conf"));
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                // target/release -> server/target/release/../../.. -> repo root
                candidates.push(dir.join("../../../etc/needed/fep.conf"));
                candidates.push(dir.join("../../etc/needed/fep.conf"));
            }
        }
        let mut fep = crate::craft::FepTable::default();
        for cand in &candidates {
            match std::fs::read_to_string(cand) {
                Ok(text) => match crate::craft::FepTable::parse(&text) {
                    Ok(t) => {
                        info!(path = %cand.display(), foods = t.len(), "fep.conf loaded");
                        fep = t;
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(path = %cand.display(), error = %e, "invalid fep.conf: trying next candidate");
                    }
                },
                Err(_) => continue,
            }
        }
        if fep.len() == 0 {
            tracing::warn!("no fep.conf found in candidates: food grants disabled");
        }
        let mut world = World::new(seed);
        // Restore terraforming overrides (furrows) into the grid store
        // before any grid is generated on demand.
        world.grids.overrides = save.world_state.tile_overrides.iter().copied().collect();
        // Restore persisted crops + furrows so the world stays persistent
        // across restarts (SavedCrop carries the spec index and stage).
        let saved_crops = save.world_state.crops.clone();
        for saved in saved_crops {
            let tile = saved.tile;
            if world.crop_at.contains_key(&tile) {
                continue;
            }
            if (saved.spec as usize) >= crate::farm::CROPS.len() {
                tracing::warn!(spec = saved.spec, tile = ?tile, "saved crop spec out of range: dropped");
                continue;
            }
            let res_static: &'static str = Box::leak(saved.res.clone().into_boxed_str());
            let res_idx = world.res.intern(res_static);
            let gob = world.gobs.spawn(
                Kind::Crop {
                    spec: saved.spec,
                    stage: saved.stage,
                },
                (tile.0 * 11 + 5, tile.1 * 11 + 5),
                res_idx,
                1,
                0,
            );
            world.crops.insert(
                gob,
                crate::farm::CropState {
                    spec: saved.spec,
                    stage: saved.stage,
                    seed_ql: saved.seed_ql,
                    soil_ql: saved.soil_ql,
                    next_stage_at: saved.next_stage_at,
                },
            );
            world.crop_at.insert(tile, gob);
        }
        let saved_tilth = save.world_state.tilth.clone();
        for (tile, deadline) in saved_tilth {
            world.tilth.insert(tile, deadline);
        }
        // Restore construction plans (half-built sites keep credited
        // materials; SavedPlan is resource-name based so process-local
        // id renumbering cannot corrupt it).
        for saved in &save.world_state.plans {
            if (saved.spec as usize) >= crate::build::BUILDABLES.len() {
                tracing::warn!(spec = saved.spec, tile = ?saved.tile, "saved plan spec out of range: dropped");
                continue;
            }
            if world.plan_at.contains_key(&saved.tile)
                || world.structure_at.contains_key(&saved.tile)
            {
                continue;
            }
            let buildable = &crate::build::BUILDABLES[saved.spec as usize];
            let res_idx = world.res.intern(buildable.res);
            let credited: Vec<crate::build::Credited> = saved
                .credited
                .iter()
                .filter_map(|(res, count, ql_sum)| {
                    // Registry names are 'static; saved names must match
                    // a demand line to keep the accounting honest.
                    buildable
                        .demand
                        .iter()
                        .find(|(r, _)| r == &res.as_str())
                        .map(|(r, _)| crate::build::Credited {
                            res: r,
                            count: *count,
                            ql_sum: *ql_sum,
                        })
                })
                .collect();
            let stage = crate::build::stage_for(buildable, &credited);
            let gob = world.gobs.spawn(
                Kind::Plan {
                    spec: saved.spec,
                    stage,
                },
                (saved.tile.0 * 11 + 5, saved.tile.1 * 11 + 5),
                res_idx,
                buildable.hp,
                0,
            );
            world.plans.insert(
                gob,
                crate::build::PlanState {
                    spec: saved.spec,
                    tile: saved.tile,
                    credited,
                },
            );
            world.plan_at.insert(saved.tile, gob);
        }
        // Restore finished structures and stations.
        for saved in &save.world_state.structures {
            if (saved.spec as usize) >= crate::build::BUILDABLES.len() {
                tracing::warn!(spec = saved.spec, tile = ?saved.tile, "saved structure spec out of range: dropped");
                continue;
            }
            if world.plan_at.contains_key(&saved.tile)
                || world.structure_at.contains_key(&saved.tile)
            {
                continue;
            }
            let buildable = &crate::build::BUILDABLES[saved.spec as usize];
            let res_idx = world.res.intern(buildable.res);
            let is_station = buildable.station.is_some();
            let gob = world.gobs.spawn(
                if is_station {
                    Kind::Station {
                        spec: saved.spec,
                        lit: false,
                    }
                } else {
                    Kind::Structure { spec: saved.spec }
                },
                (saved.tile.0 * 11 + 5, saved.tile.1 * 11 + 5),
                res_idx,
                buildable.hp,
                0,
            );
            world.structure_at.insert(saved.tile, gob);
            if is_station {
                let input = saved.input.as_ref().and_then(|(_res, ql, label)| {
                    // The label must still map to a known roast chain for
                    // the station to accept it as work in progress.
                    crate::craft::roast_result(label).map(|_| {
                        (world.res.intern("gfx/invobjs/meat"), *ql, {
                            // Leak the label into the process-static table
                            // (one entry per restored station input).
                            let leaked: &'static str = Box::leak(label.clone().into_boxed_str());
                            leaked
                        })
                    })
                });
                world.stations.insert(
                    gob,
                    crate::build::StationState {
                        spec: saved.spec,
                        fuel: saved.fuel,
                        fuel_ql_sum: saved.fuel_ql_sum,
                        fuel_seen: saved.fuel_seen,
                        input,
                        lit: false,
                        progress: saved.progress,
                        quality: saved.quality,
                    },
                );
            } else if buildable.id == "trough" {
                // Session 48: restore the fodder store the trough was
                // flushed with (units + the quality history).
                world.troughs.insert(
                    gob,
                    crate::state::TroughState {
                        units: saved.fodder_units.min(crate::state::TROUGH_CAP_UNITS),
                        ql_sum: saved.fodder_ql_sum,
                        ql_seen: saved.fodder_seen,
                    },
                );
            }
        }
        let restored = world.crops.len();
        if restored > 0 {
            info!(crops = restored, "persisted crops restored");
        }
        // Restore tamed animals (session 47): the saved species IS the
        // domestic morph, hp clamps to the species max, and the tame row
        // re-arms its leash window (partial tameness) or never breaks
        // (full tameness). The tamer gob id cannot survive restarts
        // (gob ids are runtime identities); the binding re-establishes
        // on the tamer's next quell.
        for saved in &save.world_state.animals {
            let Some(species) = crate::state::Species::from_index(saved.species) else {
                tracing::warn!(species = saved.species, tile = ?saved.tile, "saved animal species out of range: dropped");
                continue;
            };
            let res_idx = world.res.intern(species.resname());
            let gob = world.gobs.spawn(
                Kind::Animal { species },
                (saved.tile.0 * 11 + 5, saved.tile.1 * 11 + 5),
                res_idx,
                saved.hp.clamp(1, species.max_hp()),
                species.speed(),
            );
            world.animal_gobs.push(gob);
            if saved.tameness > 0 {
                let break_at = if saved.tameness >= crate::state::TAMENESS_FULL {
                    0
                } else {
                    world.tick + crate::state::LEASH_BREAK_TICKS
                };
                let mut tame = crate::state::TameState::new(0, break_at);
                tame.tameness = saved.tameness;
                tame.milk_units = saved.milk_units;
                tame.wool = saved.wool;
                tame.prod_acc = saved.prod_acc;
                tame.feed_acc_nano = saved.feed_acc_nano;
                tame.hunger = saved.hunger;
                world.tamed.insert(gob, tame);
            }
        }
        if !save.world_state.animals.is_empty() {
            info!(
                animals = save.world_state.animals.len(),
                "persisted tamed animals restored"
            );
        }
        if !world.plans.is_empty() || !world.stations.is_empty() {
            info!(
                plans = world.plans.len(),
                structures = world.stations.len()
                    + world
                        .structure_at
                        .len()
                        .saturating_sub(world.stations.len()),
                "persisted build sites restored"
            );
        }
        Game {
            world,
            sessions: HashMap::new(),
            rx,
            net_rx,
            saturated,
            next_sid: 1,
            overlay_seq: 0,
            populated: HashSet::new(),
            save,
            pending_joins: HashMap::new(),
            fep,
            workers: 1,
            move_scratch: crate::move_batch::MoveBatch::default(),
            start_scratch: crate::move_batch::MoveBatch::default(),
            fan_scratch: Vec::new(),
            combat_ix: CombatIndex::default(),
            lp_ms_per_lp: {
                // HNH_LP_RATE scales the passive accrual (skills.rs);
                // malformed values disable accrual rather than wedge boot.
                let rate = std::env::var("HNH_LP_RATE")
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(1.0);
                crate::skills::ms_per_lp(rate)
            },
            cluster: None,
        }
    }

    /// Cluster-mode constructor: the world gob-slot layout, the mesh and
    /// the per-peer subscription tables all derive from the shared
    /// membership. The mesh task itself is spawned by main (async), which
    /// forwards inbound frames as `Cmd::NodeMsg`.
    pub fn new_clustered(
        seed: u64,
        rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
        net_rx: tokio::sync::mpsc::UnboundedReceiver<crate::net::NetCmd>,
        saturated: bool,
        save_path: std::path::PathBuf,
        cfg: &crate::nodes::ClusterConfig,
        mesh: crate::nodes::Mesh,
    ) -> Self {
        let mut g = Self::new(seed, rx, net_rx, saturated, save_path);
        let nodes = std::num::NonZeroUsize::new(cfg.node_count()).expect("cluster >= 2 nodes");
        g.world = World::with_layout(seed, nodes, cfg.me);
        g.cluster = Some(Cluster {
            me: cfg.me,
            nodes,
            mesh,
            peer_subs: HashMap::new(),
            my_subs: HashMap::new(),
            player_abroad: HashMap::new(),
        });
        info!(
            me = cfg.me,
            nodes = cfg.node_count(),
            listen = %cfg.listen(),
            "cluster node starting"
        );
        g
    }

    pub fn alloc_sid(&mut self) -> SessionId {
        let sid = self.next_sid;
        self.next_sid = self.next_sid.wrapping_add(1).max(1);
        sid
    }

    pub async fn run(mut self) {
        let mut tick_timer = tokio::time::interval(Duration::from_millis(TICK_MS));
        tick_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_glob = 0u64;
        let mut autosave = tokio::time::interval(Duration::from_secs(30));
        autosave.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        info!(hz = TICK_HZ, "game loop started");
        loop {
            tokio::select! {
                _ = autosave.tick() => {
                    self.autosave();
                }
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(Cmd::Shutdown {}) => break,
                        Some(c) => self.handle_cmd(c),
                        None => break,
                    }
                }
                ncmd = self.net_rx.recv() => {
                    match ncmd {
                        Some(crate::net::NetCmd::Accept {
                            username,
                            game_tx,
                            raw_tx,
                            reply,
                        }) => {
                            let sid = self.alloc_sid();
                            self.session_connected(sid, username, game_tx, raw_tx);
                            let _ = reply.send(sid);
                        }
                        Some(other) => {
                            let cmd: Cmd = other.into();
                            self.handle_cmd(cmd);
                        }
                        None => break,
                    }
                }
                _ = tick_timer.tick() => {
                    let t0 = Instant::now();
                    self.tick();
                    let us = t0.elapsed().as_micros();
                    self.world.perf.last_tick_us = us;
                    if us > self.world.perf.max_tick_us {
                        self.world.perf.max_tick_us = us;
                    }
                    last_glob += 1;
                    if last_glob >= TICK_HZ * 5 {
                        last_glob = 0;
                        self.push_globlob();
                    }
                }
            }
        }
        info!("game loop stopping");
        self.save_all_and_flush();
    }

    /// Snapshot every online player, then write the save file. Called on the
    /// 30 s autosave cadence and at shutdown.
    fn autosave(&mut self) {
        self.save_all_and_flush();
    }

    fn save_all_and_flush(&mut self) {
        let seed = self.world.seed;
        for p in &self.world.players {
            if let Some(slot) = self.world.gobs.get(p.gob) {
                let pos = self.world.gobs.pos[slot];
                let inv_named: Vec<(String, u32, u8)> = p
                    .inv
                    .iter()
                    .map(|s| {
                        (
                            self.world
                                .res
                                .name(s.res)
                                .unwrap_or("gfx/invobjs/unknown")
                                .to_owned(),
                            s.count,
                            s.ql,
                        )
                    })
                    .collect();
                let labels: Vec<String> = p.inv.iter().map(|s| s.label.to_owned()).collect();
                let equip_named: Vec<(usize, String, u32, u8, String)> = p
                    .equip
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, e)| {
                        let s = e.as_ref()?;
                        Some((
                            slot,
                            self.world
                                .res
                                .name(s.res)
                                .unwrap_or("gfx/invobjs/unknown")
                                .to_owned(),
                            s.count,
                            s.ql,
                            s.label.to_owned(),
                        ))
                    })
                    .collect();
                self.save.snapshot(p, pos, inv_named, labels, equip_named);
            }
        }
        // World-state snapshot: growing crops + furrowed tiles + build sites.
        let mut crops = Vec::with_capacity(self.world.crops.len());
        for (gob, state) in &self.world.crops {
            let Some(slot) = self.world.gobs.get(*gob) else {
                continue;
            };
            let Kind::Crop { spec, .. } = self.world.gobs.kind[slot] else {
                continue;
            };
            let (posx, posy) = self.world.gobs.pos[slot];
            let res = self
                .world
                .res
                .name(self.world.gobs.res_idx[slot])
                .unwrap_or("gfx/terobjs/plants/wheat")
                .to_owned();
            crops.push(crate::persist::SavedCrop {
                res,
                tile: (posx.div_euclid(11), posy.div_euclid(11)),
                spec,
                stage: state.stage,
                seed_ql: state.seed_ql,
                soil_ql: state.soil_ql,
                next_stage_at: state.next_stage_at,
            });
        }
        self.save.world_state.crops = crops;
        self.save.world_state.tilth = self.world.tilth.iter().map(|(t, d)| (*t, *d)).collect();
        self.save.world_state.tile_overrides = self
            .world
            .grids
            .overrides
            .iter()
            .map(|(t, v)| (*t, *v))
            .collect();
        // Build sites: half-built plans keep their credited materials;
        // finished structures keep quality and station state.
        let mut plans = Vec::new();
        for plan in self.world.plans.values() {
            plans.push(crate::persist::SavedPlan {
                spec: plan.spec,
                tile: plan.tile,
                credited: plan
                    .credited
                    .iter()
                    .map(|c| (c.res.to_owned(), c.count, c.ql_sum))
                    .collect(),
            });
        }
        self.save.world_state.plans = plans;
        let mut structures = Vec::new();
        for (gob, station) in &self.world.stations {
            let Some(slot) = self.world.gobs.get(*gob) else {
                continue;
            };
            let (posx, posy) = self.world.gobs.pos[slot];
            structures.push(crate::persist::SavedStructure {
                spec: station.spec,
                tile: (posx.div_euclid(11), posy.div_euclid(11)),
                quality: station.quality,
                fuel: station.fuel,
                fuel_ql_sum: station.fuel_ql_sum,
                fuel_seen: station.fuel_seen,
                fodder_units: 0,
                fodder_ql_sum: 0,
                fodder_seen: 0,
                input: station.input.map(|(r, q, l)| {
                    (
                        self.world
                            .res
                            .name(r)
                            .unwrap_or("gfx/invobjs/unknown")
                            .to_owned(),
                        q,
                        l.to_owned(),
                    )
                }),
                progress: station.progress,
            });
        }
        for (tile, gob) in &self.world.structure_at {
            if self.world.stations.contains_key(gob) {
                continue; // already captured with its station state
            }
            let Some(slot) = self.world.gobs.get(*gob) else {
                continue;
            };
            let Kind::Structure { spec } = self.world.gobs.kind[slot] else {
                continue;
            };
            let quality = self
                .world
                .res
                .name(self.world.gobs.res_idx[slot])
                .map(|_| 10) // plain structures: natural default Q10
                .unwrap_or(10);
            // The Food Trough (session 48) is a plain structure that
            // carries a fodder store - snapshot it with the row.
            let fodder = self.world.troughs.get(gob).copied();
            structures.push(crate::persist::SavedStructure {
                spec,
                tile: *tile,
                quality,
                fuel: 0,
                fuel_ql_sum: 0,
                fuel_seen: 0,
                input: None,
                progress: 0,
                fodder_units: fodder.map(|t| t.units).unwrap_or(0),
                fodder_ql_sum: fodder.map(|t| t.ql_sum).unwrap_or(0),
                fodder_seen: fodder.map(|t| t.ql_seen).unwrap_or(0),
            });
        }
        self.save.world_state.structures = structures;
        // Tamed animals (session 47): tameness > 0 rows only - spawned
        // wildlife is seed-regenerated, but the tame state, the meters
        // and the domestic morph are runtime state that must survive
        // restarts (animals-and-husbandry.md: "Tameness is per-animal
        // persistent server state").
        let mut animals = Vec::with_capacity(self.world.tamed.len());
        for (gob, tame) in &self.world.tamed {
            if tame.tameness <= 0 {
                continue;
            }
            let Some(slot) = self.world.gobs.get(*gob) else {
                continue;
            };
            let Kind::Animal { species } = self.world.gobs.kind[slot] else {
                continue;
            };
            let (px, py) = self.world.gobs.pos[slot];
            // The tamer's save key when it is an online character;
            // offline tamers re-bind on the next quell (apply_quell
            // overwrites the row's tamer).
            let tamer_key = self
                .world
                .players
                .iter()
                .find(|p| p.gob == tame.tamer)
                .map(|p| crate::persist::save_key(&p.account, &p.name))
                .unwrap_or_default();
            animals.push(crate::persist::SavedAnimal {
                species: species.index(),
                tile: (px.div_euclid(11), py.div_euclid(11)),
                hp: self.world.gobs.hp[slot],
                tameness: tame.tameness,
                tamer_key,
                milk_units: tame.milk_units,
                wool: tame.wool,
                prod_acc: tame.prod_acc,
                feed_acc_nano: tame.feed_acc_nano,
                hunger: tame.hunger,
            });
        }
        self.save.world_state.animals = animals;
        if let Err(e) = self.save.flush(seed) {
            tracing::warn!(error = %e, "autosave failed");
        }
    }

    fn handle_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Wdgmsg {
                sid,
                wid,
                name,
                args,
            } => self.on_wdgmsg(sid, wid, &name, args),
            Cmd::MapReq { sid, gc } => self.on_mapreq(sid, gc),
            Cmd::ObjAck { sid, acks } => self.on_objack(sid, acks),
            Cmd::SessionClosed { sid } => self.on_session_closed(sid),
            Cmd::ReportPerf {} => self.report_perf(),
            // Handled in the run loop; reaching handle_cmd means no loop is
            // running (e.g. during tests), so this is a no-op.
            Cmd::Shutdown {} => {}
            Cmd::NodeMsg(msg) => self.on_node_msg(msg),
        }
    }

    fn report_perf(&self) {
        let ph = self.world.perf.phase_us;
        info!(
            players = self.world.players.len(),
            animals = self.world.animal_gobs.len(),
            tick_us = self.world.perf.last_tick_us,
            mean_tick_us = self.world.perf.mean_tick_us,
            max_tick_us = self.world.perf.max_tick_us,
            sessions = self.world.perf.active_sessions,
            gobs = self.world.gobs.alive.iter().filter(|a| **a).count(),
            spawned = self.world.perf.spawned_objects,
            phase_mv_us = ph[0] as u64,
            phase_ai_us = ph[1] as u64,
            phase_combat_us = ph[2] as u64,
            phase_vitals_us = ph[3] as u64,
            phase_vis_us = ph[4] as u64,
            phase_farm_us = ph[5] as u64,
            phase_station_us = ph[6] as u64,
            phase_cluster_us = ph[7] as u64,
            phase_guests_us = ph[8] as u64,
            vis_gob_scans = self.world.perf.vis_gob_scans,
            vis_skipped = self.world.perf.vis_skipped,
            vis_cached = self.world.perf.vis_cached,
            vis_cells = self.world.perf.vis_cells,
            guests = self.world.guests.len(),
            guest_pub = self.world.perf.guest_pub,
            guest_ingests = self.world.perf.guest_ingests,
            vis_scan_us = self.world.perf.vis_scan_us,
            vis_spawn_us = self.world.perf.vis_spawn_us,
            vis_spawns = self.world.perf.vis_spawns,
            vis_retract_us = self.world.perf.vis_retract_us,
            guests_encode_us = self.world.perf.guests_encode_us,
            guests_fanout_us = self.world.perf.guests_fanout_us,
            guests_pose_us = self.world.perf.guests_pose_us,
            combat_index_us = self.world.perf.combat_index_us,
            combat_players_us = self.world.perf.combat_players_us,
            combat_animals_us = self.world.perf.combat_animals_us,
            combat_relay_us = self.world.perf.combat_relay_us,
            combat_chase_n = self.world.perf.combat_chase_n,
            combat_chase_us = self.world.perf.combat_chase_us,
            combat_swing_n = self.world.perf.combat_swing_n,
            combat_hit_n = self.world.perf.combat_hit_n,
            combat_hit_us = self.world.perf.combat_hit_us,
            mv_path_us = self.world.perf.mv_path_us,
            mv_viewers_us = self.world.perf.mv_viewers_us,
            mv_pose_us = self.world.perf.mv_pose_us,
            mv_calls = self.world.perf.mv_calls,
            ix_cand_n = self.world.perf.ix_cand_n,
            move_blocks = self.world.perf.move_blocks,
            move_cells = self.world.perf.move_cells,
            grid_gens = self.world.grids.gen_count,
            grid_hits = self.world.grids.hit_count,
            "perf"
        );
    }

    // ------------------------------------------------------------------
    // Session lifecycle
    // ------------------------------------------------------------------

    /// Register a newly accepted session; shows the character list.
    /// `tx` is the sink the game task writes outgoing RMSG payloads into;
    /// the session task owns the receiver. `account` is the authenticated
    /// login user (character save keys are account-scoped).
    pub fn session_connected(
        &mut self,
        sid: SessionId,
        account: String,
        tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        raw_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    ) {
        let mut out = SessionOut {
            sid,
            account: account.clone(),
            queue: tx,
            raw: raw_tx,
            player_gob: None,
            visible: HashSet::new(),
            unacked: HashMap::new(),
            next_wid: 100,
            widgets: HashMap::new(),
            mapreqs: HashSet::new(),
            res: crate::resources::ResTable::new(),
            fight: crate::fight::FightState::new(),
            craft_recipe: None,
            craft_window: None,
            item_menu: None,
            item_wids: HashMap::new(),
            crop_menu: None,
            animal_menu: None,
            chat_wid: 0,
            party_wid: 0,
            player_menu: None,
            pending_plow: false,
            pending_build: None,
            station_menu: None,
            cursor: None,
            cursor_wid: None,
            grids_seen: HashSet::new(),
            vis_cell: None,
            vis_cache: None,
            vis_cache_pos: None,
            last_retract_tick: 0,
        };
        // Character selection UI (session-lifecycle.md 3.1).
        let w_bg = out.new_wid("img");
        let w_logo = out.new_wid("img");
        let w_list = out.new_wid("charlist");
        // Avatar layer RESIDs must be announced before the charlist add.
        // The login portrait (Charlist -> Avaview -> AvaRender) flattens
        // `Resource.layers(imgc)` of every listed resource, so the layers
        // must be IMAGE-bearing standing frames - the pose-router
        // resources ("gfx/borka/body" et al) carry no imgc layers and
        // would leave the portrait blank ("no face" bug report). The
        // same frame set layers the in-world avatar, so the login card
        // and the world character match. Octant 1 is the camera-facing
        // front octant: art_dir(1) = sprite 0, the head-on front view the
        // login card expects.
        let portrait_layers = avatar_pose_layers(false, 1);
        let mut layer_ids = Vec::with_capacity(portrait_layers.len());
        for name in portrait_layers {
            let global = self.world.res.intern(name);
            let w = out.res.wire_named(global, name);
            if let Some((n, v)) = out.res.pending_announce(w) {
                out.send(wdg::resid(w, n, v));
                out.res.mark_announced(w);
            }
            layer_ids.push(w);
        }
        info!(layers = ?portrait_layers, "charlist portrait layers announced");
        out.send(wdg::new_wdg(
            w_bg,
            "img",
            0,
            0,
            0,
            &[ListVal::S("gfx/ccscr".into())],
        ));
        out.send(wdg::new_wdg(
            w_logo,
            "img",
            274,
            10,
            0,
            &[ListVal::S("gfx/logo2".into())],
        ));
        out.send(wdg::new_wdg(
            w_list,
            "charlist",
            300,
            200,
            0,
            &[ListVal::I(6)],
        ));
        // One character per account.
        let mut add = MessageBuf::new();
        add.uint8(RMSG_WDGMSG)
            .uint16(w_list)
            .string("add")
            .lstr("Player");
        for id in &layer_ids {
            add.lint(*id as i32);
        }
        add.lend();
        out.send(add.finish());
        self.sessions.insert(sid, out);
    }

    fn on_wdgmsg(&mut self, sid: SessionId, wid: u16, name: &str, args: Vec<hnh_proto::ListArg>) {
        let Some(out) = self.sessions.get(&sid) else {
            return;
        };
        let wtag = out.widgets.get(&wid).cloned();
        debug!(
            sid,
            wid,
            name,
            wtag = wtag.as_deref().unwrap_or("?"),
            nargs = args.len(),
            "wdgmsg in"
        );
        match (wtag.as_deref(), name) {
            (Some("charlist"), "play") => {
                let chosen = args
                    .first()
                    .and_then(|a| a.as_str())
                    .unwrap_or("Player")
                    .to_owned();
                self.enter_world(sid, chosen);
            }
            (Some("speedget"), "set") => {
                // Speedget.setspeed: the player picked a gait (0..3).
                // Clamp against the documented gait table and apply to the
                // mover speed so subsequent walks use it.
                let gait = args
                    .first()
                    .and_then(|a| a.as_int())
                    .unwrap_or(GAIT_WALK as i32)
                    .clamp(0, 3) as usize;
                if let Some(&pidx) = self.world.by_session.get(&sid) {
                    self.world.players[pidx].gait = gait as u8;
                }
                if let Some(slot) = self
                    .world
                    .by_session
                    .get(&sid)
                    .map(|&pi| self.world.players[pi].gob)
                    .and_then(|g| self.world.gobs.get(g))
                {
                    self.world.gobs.speed[slot] = GAIT_SPEEDS[gait];
                }
                // Confirm the UI selection (Speedget uimsg "cur").
                if let Some(w) = self.speedget_wid(sid) {
                    if let Some(o) = self.sessions.get_mut(&sid) {
                        o.send(crate::fight::uimsg(w, "cur", &[gait as i32]));
                    }
                }
                trace!(sid, gait, "gait set");
            }
            (Some("slen"), "inv") => {
                self.open_inventory(sid);
            }
            (Some("slen"), "equ") => {
                self.open_epry(sid);
            }
            (Some("slen"), "chr") => self.open_char_sheet(sid),
            (Some("slen"), _) | (None, "bud") => {}
            (Some("epry"), "drop") => self.epry_drop(sid, &args),
            (Some("epry"), "take") => self.epry_take(sid, &args),
            (Some("epry"), "itemact") | (Some("epry"), "transfer") | (Some("epry"), "iact") => {
                // Activate/transfer semantics on equipped items are not
                // modeled yet; acknowledge silently so the client stays
                // responsive.
            }
            (Some("invwnd") | Some("inv"), "drop") => self.inv_drop(sid, wid, &args),
            // "take" originates from the item widget itself (Item.mousedown).
            (Some("item"), "take") => self.inv_take(sid, wid),
            (Some("item"), "iact") => self.on_item_iact(sid, wid),
            (Some("mapview"), "itemact") => self.on_map_itemact(sid, &args),
            (Some("mapview"), "drop") => self.on_map_drop(sid),
            (Some("mapview"), "click") => self.on_map_click(sid, &args),
            (Some("mapview"), "place") => self.on_map_place(sid, &args),
            (Some("scm"), "act") => {
                let action: Vec<String> = args
                    .iter()
                    .filter_map(|a| a.as_str().map(str::to_owned))
                    .collect();
                self.on_menu_action(sid, &action);
            }
            (Some("make"), "make") => {
                let mode = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                self.on_make_cmd(sid, mode);
            }
            (Some("frv"), "click") | (Some("frv"), "give") => {
                self.on_frv_msg(sid, name, &args);
            }
            (Some("sm"), "cl") => {
                let choice = args.first().and_then(|a| a.as_int()).unwrap_or(-1);
                self.on_flower_choice(sid, wid, choice);
            }
            (Some("slenchat"), "msg") => {
                let line = args.first().and_then(|a| a.as_str()).unwrap_or("");
                self.on_chat_msg(sid, line);
            }
            (Some("pv"), "leave") => self.party_leave(sid),
            (Some("chr"), "buy") => {
                let name = args.first().and_then(|a| a.as_str()).unwrap_or("");
                self.on_skill_buy(sid, name);
            }
            (Some("chr"), "sattr") => self.on_skill_attrs(sid, &args),
            _ => {
                trace!(sid, wid, name, "unhandled wdgmsg");
            }
        }
    }

    /// Snapshot of the CAttr entries the client's CharWnd constructor
    /// requires. Names must match CharWnd.baseval/skillval/Belief exactly:
    /// `CharWnd$Attr.<init>` does `glob.cattr.get(nm)` and dereferences the
    /// result without a null check, so any missing name NPEs the client the
    /// moment SlenHud.binded() requests the char sheet after entering.
    /// Internal short keys (agi/int/con/per/cha/dex) map to the client's
    /// long names (agil/intel/cons/perc/csm/dxt); skills and beliefs are
    /// synthesized (no progression system behind them yet) and expmod
    /// defaults to 100 (learning ability percent).
    fn char_attr_snapshot(&self, sid: SessionId) -> Vec<(&'static str, i32, i32)> {
        let Some(p) = self.world.player(sid) else {
            return Vec::new();
        };
        let base = |k: &str| p.attrs.get(k).copied().unwrap_or(10);
        let zero = |k: &str| p.attrs.get(k).copied().unwrap_or(0);
        let mut v: Vec<(&'static str, i32, i32)> = vec![
            ("pts", p.lp, p.lp),
            ("hp", 100, p.hp),
            ("energy", 100, p.energy),
            ("stamina", 100, p.stamina),
            ("str", base("str"), base("str")),
            ("agil", base("agi"), base("agi")),
            ("intel", base("int"), base("int")),
            ("cons", base("con"), base("con")),
            ("perc", base("per"), base("per")),
            ("csm", base("cha"), base("cha")),
            ("dxt", base("dex"), base("dex")),
            ("psy", base("psy"), base("psy")),
        ];
        v.push(("expmod", base("expmod"), base("expmod")));
        for s in [
            "unarmed",
            "melee",
            "ranged",
            "explore",
            "stealth",
            "sewing",
            "smithing",
            "carpentry",
            "cooking",
            "farming",
            "survive",
        ] {
            v.push((s, zero(s), zero(s)));
        }
        for b in ["life", "night", "civil", "nature", "martial", "change"] {
            v.push((b, zero(b), zero(b)));
        }
        v
    }

    /// Phase 3.2 (session-lifecycle.md): enter the world after `play`.
    fn enter_world(&mut self, sid: SessionId, name: String) {
        self.enter_world_inner(sid, name, true);
    }

    /// World entry. `allow_defer` gates the cluster migration wait: a
    /// session whose save key lives on a peer defers once (CharQuery
    /// broadcast); the reply or the deadline re-enters with `false`, which
    /// proceeds with whatever state is locally available.
    fn enter_world_inner(&mut self, sid: SessionId, chosen: String, allow_defer: bool) {
        let account = self
            .sessions
            .get(&sid)
            .map(|o| o.account.clone())
            .unwrap_or_default();
        let key = crate::persist::save_key(&account, &chosen);
        // Legacy saves (pre-account keying) stored characters under the
        // bare display name; adopt the snapshot into this account's
        // namespace so an existing single-node world survives the upgrade.
        if !self.save.players.contains_key(&key) && self.save.players.contains_key(&chosen) {
            if let Some(mut snap) = self.save.players.remove(&chosen) {
                info!(%chosen, %account, "adopting legacy save key");
                snap.name = key.clone();
                self.save.players.insert(key.clone(), snap);
            }
        }
        // Cluster: the key may live on a peer's shard. Ask before spawning
        // fresh - deferring keeps the charlist widgets up and the client
        // waits out a LAN round trip (bounded by the pending deadline).
        // The query re-broadcasts every CHAR_QUERY_RETRY_MS from the tick
        // drain: a link that is still negotiating buffers the retry and
        // answers as soon as the mesh converges.
        if allow_defer
            && self.cluster_nodes() > 1
            && !self.save.players.contains_key(&key)
            && self.pending_joins.iter().all(|(_, j)| j.account != account)
        {
            if let Some(c) = self.cluster.as_ref() {
                info!(sid, %key, "save key not local: querying cluster peers");
                self.pending_joins.insert(
                    sid,
                    PendingJoin {
                        account,
                        chosen,
                        deadline: std::time::Instant::now()
                            + std::time::Duration::from_millis(CHAR_QUERY_DEADLINE_MS),
                        answered: HashSet::new(),
                        // First retry one cadence after the initial query.
                        next_retry: CHAR_QUERY_RETRY_TICKS,
                    },
                );
                c.mesh.broadcast_except(
                    c.nodes.get(),
                    c.me,
                    crate::nodes::NodeMsg::CharQuery {
                        from: c.me,
                        name: key,
                    },
                );
                return;
            }
        }
        let name = chosen;
        // Destroy selection widgets.
        let widget_ids: Vec<u16> = {
            let Some(out) = self.sessions.get(&sid) else {
                return;
            };
            out.widgets
                .iter()
                .filter(|(_, t)| t.as_str() == "img" || t.as_str() == "charlist")
                .map(|(id, _)| *id)
                .collect()
        };
        if let Some(out) = self.sessions.get(&sid) {
            for id in widget_ids {
                out.send(wdg::dst_wdg(id));
            }
        }
        // Restore the persisted character when one exists for this save
        // key; the saved world position overrides the fresh-spawn search.
        let saved_state = self.save.players.get(&key).map(|saved| {
            let mut restored_inv = Vec::with_capacity(saved.inv.len());
            for (n, (resname, count, ql)) in saved.inv.iter().enumerate() {
                let idx = self.world.res.intern(leak_static(resname));
                let label = saved
                    .inv_labels
                    .get(n)
                    .map(|s| leak_static(s))
                    .unwrap_or("");
                restored_inv.push(InvStack {
                    res: idx,
                    count: *count,
                    ql: *ql,
                    label,
                });
            }
            let restored_skills: HashSet<&'static str> = saved
                .skills
                .iter()
                .filter_map(|s| crate::skills::catalog_get(s).map(|d| d.name))
                .collect();
            let mut restored_equip: Vec<Option<InvStack>> = vec![None; 16];
            for (slot, resname, count, ql, label) in &saved.equip {
                let idx = self.world.res.intern(leak_static(resname));
                let s = (*slot).min(15);
                restored_equip[s] = Some(InvStack {
                    res: idx,
                    count: *count,
                    ql: *ql,
                    label: leak_static(label),
                });
            }
            (
                saved.pos,
                saved.hp,
                saved.energy,
                saved.stamina,
                saved.lp,
                saved.attrs.clone(),
                restored_inv,
                restored_skills,
                restored_equip,
            )
        });
        let (spawn_pos, hp, energy, stamina, lp, attrs, inv, restored_skills, restored_equip) =
            match &saved_state {
                Some((pos, hp, energy, stamina, lp, attrs, inv, skills, equip)) => {
                    info!(sid, %name, "restoring persisted character");
                    (
                        *pos,
                        *hp,
                        *energy,
                        *stamina,
                        *lp,
                        attrs.clone(),
                        inv.clone(),
                        skills.clone(),
                        equip.clone(),
                    )
                }
                None => {
                    let mut fresh = HashMap::new();
                    // All eight base attributes (CharWnd lists str..psy; the
                    // FEP requirement is the highest of them).
                    for k in ["str", "agi", "int", "con", "per", "cha", "dex", "psy"] {
                        fresh.insert(k.to_owned(), 10);
                    }
                    fresh.insert("hp".to_owned(), 100);
                    fresh.insert("energy".to_owned(), 100);
                    fresh.insert("lp".to_owned(), 0);
                    (
                        self.find_spawn_position(),
                        100,
                        100,
                        100,
                        100,
                        fresh,
                        Vec::new(),
                        HashSet::new(),
                        vec![None; 16],
                    )
                }
            };
        let res_body = self.world.res.intern("gfx/borka/body");
        let _res_head = self.world.res.intern("gfx/borka/head");
        let _res_hair = self.world.res.intern("gfx/borka/hair");
        let gob = self.world.gobs.spawn(
            Kind::Player { player: usize::MAX },
            spawn_pos,
            res_body,
            hp.max(1),
            BASE_SPEED,
        );
        let player_idx = self.world.players.len();
        if let Some(slot) = self.world.gobs.get(gob) {
            self.world.gobs.kind[slot] = Kind::Player { player: player_idx };
        }
        // Restore the criminal flag from the save (None on fresh
        // characters or older saves; the buff re-streams on world entry).
        let criminal_until_ms = self
            .save
            .players
            .get(&key)
            .and_then(|s| s.criminal_until_ms);
        self.world.players.push(Player {
            name: name.clone(),
            account,
            gob,
            session: sid,
            hp,
            energy,
            stamina,
            lp,
            criminal_until_ms,
            lp_carry_ms: 0,
            gait: GAIT_WALK as u8,
            skills: restored_skills,
            attrs,
            inv,
            equip: restored_equip,
            fep: crate::craft::FepState::default(),
            fight_target: None,
            atk_cd: 0,
            aim: None,
        });
        self.world.by_session.insert(sid, player_idx);

        // Starter kit for fresh characters (server policy; legacy gave
        // nothing but the dev flow needs craftable ingredients on hand).
        // Labels on food keep the fep.conf identity for the eat flow.
        if self.world.players[player_idx].inv.is_empty() {
            let kit: &[(&str, u32, u8, &'static str)] = &[
                // Session 36: 6 branches + 2 stones + 2 string let a fresh
                // character craft one Wooden Bow (4 branch + 1 string) and
                // one batch of Stone Arrows (1 stone + 2 branch) out of the
                // box - the whole bow chain is playable immediately.
                ("gfx/invobjs/branch", 6, 10, ""),
                ("gfx/invobjs/stone", 4, 10, ""),
                ("gfx/invobjs/string", 2, 10, ""),
                ("gfx/invobjs/meat", 1, 10, "Beef"),
                // Farming starter seeds: the plow pagina is pushed to
                // every session, so the full plant-grow-harvest loop is
                // playable out of the box.
                ("gfx/invobjs/seed-wheat", 5, 10, "Wheat Seeds"),
                ("gfx/invobjs/seed-carrot", 5, 10, "Carrot Seeds"),
                // Starter clothing: wearable pieces render on the avatar
                // (equip.rs) and give the Equipment doll something to
                // show right away.
                ("gfx/invobjs/linenpants", 1, 10, "Linen Pants"),
                ("gfx/invobjs/linenshirt", 1, 10, "Linen Shirt"),
            ];
            for (resname, count, ql, label) in kit {
                let gidx = self.world.res.intern(resname);
                self.world.players[player_idx].inv.push(InvStack {
                    res: gidx,
                    count: *count,
                    ql: *ql,
                    label,
                });
            }
        }

        // --- HUD + world bootstrap (order matters; lifecycle doc 3.2) ---
        let player_gob = gob;
        // Snapshot CharWnd attributes before the session out-queue is
        // borrowed: they must reach the client before the `chr` widget is
        // created (SlenHud.binded requests it immediately).
        let attr_entries = self.char_attr_snapshot(sid);
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.player_gob = Some(player_gob);
        // RESID announcements for everything referenced so far.
        let pending: Vec<(u16, &'static str, u16)> = (0..out.res.wire_count() as u16)
            .filter_map(|w| out.res.pending_announce(w).map(|(n, v)| (w, n, v)))
            .collect();
        for (w, n, v) in pending {
            out.send(wdg::resid(w, n, v));
        }
        // Tilesets before first MAPDATA; the version must be the real file
        // version - the client hard-rejects mismatched announces.
        for (id, name, _ver) in hnh_world::TILESETS {
            out.send(wdg::tiles(*id, name, crate::resources::file_version(name)));
        }
        // HUD widgets. The mapview MUST be created before the slen HUD:
        // this fork's SlenHud constructor builds the MinimapPanel, which
        // captures `ui.mapview` at creation time - with the old order
        // (slen first) the minimap held a null MapView and the first
        // real render tick died with an NPE in MiniMap.draw, freezing
        // the client right after entering the world (the render-only
        // path no headless probe ever exercised).
        let w_mv = out.new_wid("mapview");
        let w_slen = out.new_wid("slen");
        let w_scm = out.new_wid("scm");
        let w_speed = out.new_wid("speedget");
        let w_buffs = out.new_wid("buffs");
        out.send(wdg::new_wdg(
            w_mv,
            "mapview",
            0,
            0,
            0,
            &[
                ListVal::I(0),
                ListVal::C(spawn_pos.0, spawn_pos.1),
                ListVal::I(player_gob),
            ],
        ));
        out.send(wdg::new_wdg(w_slen, "slen", 0, 0, 0, &[]));
        out.send(wdg::new_wdg(w_scm, "scm", 0, 0, 0, &[]));
        out.send(wdg::new_wdg(
            w_speed,
            "speedget",
            0,
            0,
            0,
            // cur = walk (docs: RoB Glossary "Speed"), max = sprint index.
            &[ListVal::I(GAIT_WALK as i32), ListVal::I(3)],
        ));
        out.send(wdg::new_wdg(w_buffs, "buffs", 0, 0, 0, &[]));
        // Area Chat window (ChatHW factory): title "Area Chat" hides the
        // client close button; closable = 0 keeps the window permanent.
        let w_chat = out.new_wid("slenchat");
        out.send(wdg::new_wdg(
            w_chat,
            "slenchat",
            0,
            0,
            0,
            &[ListVal::S("Area Chat".to_owned()), ListVal::I(0)],
        ));
        out.chat_wid = w_chat;
        // Vitals meters parented to slen: hp (red), energy (yellow),
        // stamina (green).
        let w_hp = out.new_wid("vm");
        let w_en = out.new_wid("vm");
        let w_st = out.new_wid("vm");
        out.send(wdg::new_wdg(
            w_hp,
            "vm",
            90,
            10,
            w_slen,
            &[
                ListVal::I(100),
                ListVal::I(255),
                ListVal::I(0),
                ListVal::I(0),
            ],
        ));
        out.send(wdg::new_wdg(
            w_en,
            "vm",
            109,
            10,
            w_slen,
            &[
                ListVal::I(100),
                ListVal::I(255),
                ListVal::I(255),
                ListVal::I(0),
            ],
        ));
        out.send(wdg::new_wdg(
            w_st,
            "vm",
            128,
            10,
            w_slen,
            &[
                ListVal::I(100),
                ListVal::I(0),
                ListVal::I(255),
                ListVal::I(0),
            ],
        ));
        // Equipment paperdoll (Equipory, widget type "epry"): the
        // user-reported missing doll. Created during bootstrap with a full
        // "set" sync and the "ava" avatar gob binding.
        let w_epry = out.new_wid("epry");
        out.send(wdg::new_wdg(w_epry, "epry", 0, 0, 0, &[]));
        // Global state.
        let (unix, dt, mp, yt) = self.world.astro();
        out.send(wdg::globlob(unix, dt, mp, yt, Some((255, 255, 255, 255))));
        // Full CharWnd attribute set (client-name mapping) before any
        // chance of the `chr` widget being created.
        out.send(wdg::cattr(&attr_entries));
        // Menu paginae: base actions plus every implemented craft recipe
        // and the build tree (RMSG_PAGINAE; parents resolve from the
        // served resource pack: paginae/act/build -> paginae/build/cons
        // -> paginae/build/<id>; ad strings are the Buildable ids).
        let mut pages: Vec<&'static str> =
            vec!["paginae/act/add", "paginae/add/study", "paginae/act/plow"];
        pages.push("paginae/craft/roastmeat");
        pages.extend([
            "paginae/act/build",
            "paginae/build/cons",
            "paginae/build/oven",
            "paginae/build/smelter",
            "paginae/build/trough",
        ]);
        for r in crate::craft::RECIPES {
            pages.push(r.pagina);
        }
        // Fight-window maneuver buttons (paginae/atk/*): the root page
        // plus every implemented maneuver of the fight.rs table. Server
        // policy: all buttons are visible from the start (skill gating
        // is a future Open question in combat-system.md).
        pages.push("paginae/atk/atk");
        for m in crate::fight::MANEUVERS {
            pages.push(m.res);
        }
        pages.push("paginae/atk/blk");
        // One PAGINAE frame per entry: the 30-entry announce crosses the
        // reliability-layer fragmentation path, and the real client's
        // in-frame loop desynced mid-frame in the probe environment
        // (pagina read at a shifted offset picked up a garbage version -
        // "Wrong res version (1 != 28484)"). Per-entry frames carry the
        // same entries with no in-frame cursor to desync.
        for p in &pages {
            out.send(wdg::paginae_add(std::slice::from_ref(p)));
        }
        // Initial paperdoll contents ("set" + "ava") now that the player
        // and the epry widget both exist.
        self.send_epry_state(sid);
        // Restore the criminal-state buff icon on reconnects (the Glob
        // is rebuilt client-side on every world entry).
        self.stream_criminal_buff(sid);
        info!(sid, %name, gob, "player entered world");
    }

    fn find_spawn_position(&mut self) -> (i32, i32) {
        // Scan outward from (550, 550) for a walkable tile center.
        for r in 0..40i32 {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let tx = 50 + dx;
                    let ty = 50 + dy;
                    let gc = (tx.div_euclid(100), ty.div_euclid(100));
                    let ix = tx.rem_euclid(100) as usize;
                    let iy = ty.rem_euclid(100) as usize;
                    let g = self.world.grids.grid(gc);
                    if tile_speed(g.tile(ix, iy)).is_some() {
                        return (tx * 11 + 5, ty * 11 + 5);
                    }
                }
            }
        }
        (550, 550)
    }

    // ------------------------------------------------------------------
    // Map
    // ------------------------------------------------------------------

    fn on_mapreq(&mut self, sid: SessionId, gc: (i32, i32)) {
        // Track which grids this client holds (tile-mutation re-sends).
        if let Some(out) = self.sessions.get_mut(&sid) {
            out.grids_seen.insert(gc);
        }
        // Populate the grid the first time anyone looks at it (session
        // 33: owner-filtered - my cells' content only; foreign-cell
        // statics/animals arrive as guests from the cell owner through
        // the Sub-driven populate, so no shadow copies ever spawn here).
        let mut spawned = Vec::new();
        let first_touch = !self.populated.contains(&gc);
        if first_touch {
            self.populated.insert(gc);
            let filter = self.cluster.as_ref().map(|c| (c.me, c.nodes));
            self.world.populate_grid(gc, filter, &mut spawned);
            let animals = if self.saturated { 40 } else { 4 };
            self.world
                .populate_animals(gc, filter, animals, &mut spawned);
        }
        let payload = {
            let grid = self.world.grids.grid(gc);
            let p = hnh_proto::MapGridPayload {
                gc,
                mnm: grid.mnm.clone(),
                tiles: grid.tiles.as_slice().to_vec(),
                plot_flags: vec![],
                plots: vec![],
            };
            p.encode()
        };
        // Announce new RESIDs from freshly spawned content, then spawn.
        if let Some(out) = self.sessions.get_mut(&sid) {
            let pktid = (self.world.tick & 0x3FFFFFFF) as i32;
            let frags = hnh_proto::fragment_payload(MSG_MAPDATA, pktid, &payload, 1200);
            for f in frags {
                out.send_raw(f);
            }
        }
        for id in spawned {
            self.stream_spawn(sid, id);
        }
        info!(sid, ?gc, "mapdata sent");
    }

    // ------------------------------------------------------------------
    // Object streaming (visibility, OBJDATA encoding)
    // ------------------------------------------------------------------

    /// Encode the full snapshot block for one gob (spawn + refresh).
    /// Returns None for dead gobs (callers retract instead).
    fn encode_gob_block(
        &mut self,
        sid: SessionId,
        id: GobId,
        include_res: bool,
    ) -> Option<Vec<u8>> {
        let slot = self.world.gobs.get(id)?;
        let pos = self.world.gobs.pos[slot];
        let res_idx = self.world.gobs.res_idx[slot];
        let frame = self.world.gobs.frame[slot];
        let kind = self.world.gobs.kind[slot];
        let hp = self.world.gobs.hp[slot];
        let max_hp = self.world.gobs.max_hp[slot];
        let mv = self.world.gobs.mv[slot];
        // Equipped pieces read before the session borrow (the names feed
        // both the world drawable and the doll attribute below).
        let equip: Vec<&'static str> = match kind {
            Kind::Player { player } => self.player_equip_names(player),
            _ => Vec::new(),
        };
        let res_name = self
            .world
            .res
            .name(res_idx)
            .unwrap_or("gfx/terobjs/bumlings/01");
        let out = self.sessions.get_mut(&sid)?;
        let wire_res = out.res.wire_named(res_idx, res_name);
        let mut m = MessageBuf::new();
        m.uint8(MSG_OBJDATA);
        m.uint8(0); // flags
        m.int32(id);
        m.int32(frame as i32);
        // Players and animals must NOT be announced via OD_RES: the pose
        // router bases (gfx/borka/body, gfx/kritter/<sp>/body) carry no neg
        // layer, so ResDrawable's eager ImageSprite creation throws "No
        // negative found" inside the client's session reader thread and
        // kills it. Both render through OD_LAYERS (Layered drawable) of
        // concrete image-bearing pose parts below.
        let is_player = matches!(kind, Kind::Player { .. });
        let is_animal = matches!(kind, Kind::Animal { .. });
        if include_res && !is_player && !is_animal {
            // OD_RES with the resource; sprite dynamic data for plants.
            m.uint8(OD_RES).uint16(wire_res | 0x8000);
            let sdt = match kind {
                Kind::Tree { harvests } => vec![harvests],
                Kind::Crop { stage, .. } => vec![stage],
                Kind::Plan { stage, .. } => vec![stage],
                Kind::Station { lit, .. } => vec![lit as u8],
                _ => Vec::new(),
            };
            if sdt.is_empty() {
                // Rewrite: OD_RES without sdt needs the resid without flag.
                // We already wrote the flag; simplest fix is sdt of len 0
                // is not allowed, so restart the buffer correctly.
                m = MessageBuf::new();
                m.uint8(MSG_OBJDATA).uint8(0).int32(id).int32(frame as i32);
                m.uint8(OD_RES).uint16(wire_res);
            } else {
                m.uint8(sdt.len() as u8).bytes(&sdt);
            }
        }
        // Movement.
        match mv {
            Some(lm) => {
                m.uint8(OD_LINBEG)
                    .coord(lm.sx, lm.sy)
                    .coord(lm.tx, lm.ty)
                    .int32(lm.steps);
                m.uint8(OD_LINSTEP).int32(lm.step);
            }
            None => {
                m.uint8(OD_MOVE).coord(pos.0, pos.1);
            }
        }
        // Composited drawables (players + animals): server-side pose
        // resolution of concrete directional frame resources. Player
        // blocks append the equipped pieces' clothing layers (equip.rs)
        // to both the world drawable and the doll attribute.
        if is_player || is_animal {
            let moving = mv.is_some();
            let facing = self.world.gobs.facing[slot];
            m.uint8(OD_LAYERS);
            if let Kind::Player { player } = kind {
                // Base = the body router (load gate, never sprite-created).
                m.uint16(wire_res);
                for part in avatar_pose_layers(moving, facing) {
                    let gi = self.world.res.intern(part);
                    let w = out.res.wire_named(gi, part);
                    m.uint16(w);
                }
                for part in crate::equip::world_layers(&equip, moving, facing) {
                    let gi = self.world.res.intern(part);
                    let w = out.res.wire_named(gi, part);
                    m.uint16(w);
                }
                m.uint16(65535);
                if let Some(p) = self.world.players.get(player) {
                    // Avatar attribute (OD_AVATAR): drives the Equipment
                    // window doll (Equipory.cdraw reads Avatar.rend of the
                    // viewer's own gob) and the isPlayer checks. The own
                    // viewer gets the banzai doll pose; everyone else gets
                    // the standing idle set.
                    let own = out.player_gob == Some(id);
                    let doll: Vec<&'static str> = if own {
                        avatar_doll_layers()
                            .iter()
                            .copied()
                            .chain(crate::equip::doll_layers(&equip))
                            .collect()
                    } else {
                        avatar_pose_layers(false, facing)
                            .iter()
                            .copied()
                            .chain(crate::equip::world_layers(&equip, false, facing))
                            .collect()
                    };
                    m.uint8(OD_AVATAR);
                    for part in doll {
                        let gi = self.world.res.intern(part);
                        let w = out.res.wire_named(gi, part);
                        m.uint16(w);
                    }
                    m.uint16(65535);
                    m.uint8(OD_BUDDY).string(&p.name).uint8(0).uint8(0);
                }
            } else if let Kind::Animal { species } = kind {
                // Kritter pose parts: one body part per species (the pack
                // ships standing-N/walking-N directional sets for all of
                // them). Spawned through OD_LAYERS so each pose embeds its
                // walk animation and the sprite actually renders (the old
                // flat cdv spawn left shadow-only gobs - session 21).
                let base = kritter_base(species);
                let bi = self.world.res.intern(base);
                let bw = out.res.wire_named(bi, base);
                m.uint16(bw);
                let part = kritter_pose_layer(species, moving, facing);
                let gi = self.world.res.intern(part);
                let w = out.res.wire_named(gi, part);
                m.uint16(w);
                m.uint16(65535);
            }
        }
        // Health tint.
        let quarters = ((hp * 4) / max_hp.max(1)).clamp(0, 4) as u8;
        m.uint8(OD_HEALTH).uint8(quarters);
        m.uint8(OD_END);
        Some(m.finish())
    }

    /// Stream a spawn (full state) for one gob to one session, announcing
    /// its resource id first if the session has not seen it.
    fn stream_spawn(&mut self, sid: SessionId, id: GobId) {
        // Cluster guests take their own spawn path (state lives in the
        // guest table, not the SoA columns).
        if self.world.gobs.get(id).is_none() {
            if self.world.guests.contains_key(&id) {
                self.stream_guest_spawn(sid, id);
            }
            return;
        }
        let slot = self.world.gobs.get(id).expect("checked above");
        let res_idx = self.world.gobs.res_idx[slot];
        // Gob render facts read before the session borrow (players carry
        // equipped piece names into the spawn block below).
        let kind = self.world.gobs.kind[slot];
        let moving = self.world.gobs.mv[slot].is_some();
        let facing = self.world.gobs.facing[slot];
        let equip: Vec<&'static str> = match kind {
            Kind::Player { player } => self.player_equip_names(player),
            _ => Vec::new(),
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        if !out.visible.insert(id) {
            return;
        }
        let res_name = self
            .world
            .res
            .name(res_idx)
            .unwrap_or("gfx/terobjs/bumlings/01");
        let wire = out.res.wire_named(res_idx, res_name);
        if let Some((name, ver)) = out.res.pending_announce(wire) {
            let msg = wdg::resid(wire, name, ver);
            out.send(msg);
            out.res.mark_announced(wire);
        }
        // Composited drawables: announce the base + every concrete pose
        // resource the OD_LAYERS block references before the spawn block.
        // The client resolves OD_LAYERS ids through these RESIDs; without
        // them the avatar renders invisible ("no doll").
        let layers: Vec<&'static str> = match kind {
            Kind::Player { .. } => {
                let mut v = Vec::with_capacity(13 + 4 * equip.len());
                v.push(AVATAR_BASE);
                v.extend(avatar_pose_layers(moving, facing).iter().copied());
                v.extend(crate::equip::world_layers(&equip, moving, facing));
                v.extend(avatar_doll_layers().iter().copied());
                v.extend(crate::equip::doll_layers(&equip));
                v
            }
            Kind::Animal { species } => {
                vec![
                    kritter_base(species),
                    kritter_pose_layer(species, moving, facing),
                ]
            }
            _ => Vec::new(),
        };
        for layer_name in layers {
            let gi = self.world.res.intern(layer_name);
            let w = out.res.wire_named(gi, layer_name);
            if let Some((name, ver)) = out.res.pending_announce(w) {
                out.send(wdg::resid(w, name, ver));
                out.res.mark_announced(w);
            }
        }
        // Encode and register the spawn block (separate borrow scope).
        if let Some(block) = self.encode_gob_block(sid, id, true) {
            let frame = self.world.gobs.frame[slot];
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.send_raw(block.clone());
                Self::record_unacked(out, id, frame, block);
            }
        }
    }

    fn stream_retract(&mut self, sid: SessionId, id: GobId) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        if out.visible.remove(&id) {
            let frame = self
                .world
                .gobs
                .frame
                .get(split_gob_id(id).0)
                .copied()
                .unwrap_or(0);
            let mut m = MessageBuf::new();
            m.uint8(MSG_OBJDATA)
                .uint8(0)
                .int32(id)
                .int32(frame as i32)
                .uint8(OD_REM)
                .uint8(OD_END);
            out.send_raw(m.finish());
        }
        out.unacked.remove(&id);
    }

    /// Per-tick visibility update: spawns, retractions, movement deltas.
    /// Visibility: parallel in-range candidate scan (phase A, read-only
    /// over the SoA columns), then serial spawn/move/retract application
    /// (phase B, mutates session state and streams wire blocks).
    fn update_visibility(&mut self) {
        let session_ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        // --- Phase A: scan-kind decision + candidate positions.
        //
        // Session 30 result caching: a session whose EXACT position is
        // unchanged since its last scan reuses that scan's result list.
        // Nothing in view was touched -> the result is provably unchanged
        // (skip); something was touched -> patch the list (leavers and
        // deaths re-filtered out by current position, enterers added from
        // the touched records). A session that moved (or has no cache
        // yet) runs the full scan_visible and refills the cache.
        let candidates: Vec<(SessionId, (i32, i32))> = session_ids
            .iter()
            .filter_map(|sid| {
                let player_gob = self.sessions[sid].player_gob?;
                let pslot = self.world.gobs.get(player_gob)?;
                Some((*sid, self.world.gobs.pos[pslot]))
            })
            .collect();
        // to_scan slot 4: true = Patch (cached), false = Full rescan.
        let mut to_scan: Vec<(SessionId, (i32, i32), bool, bool)> = Vec::new();
        for (sid, (px, py)) in candidates {
            let cell = crate::visidx::cell_of(px, py);
            let cell_moved = self.sessions[&sid].vis_cell != Some(cell);
            let cache_valid =
                self.sessions[&sid].vis_cache_pos == Some((px, py)) && self.vis_cache_len(sid) > 0;
            if !cache_valid {
                // Position changed (or no cache yet): full rescan.
                self.world.perf.vis_skipped += 1; // full scans issued
                if let Some(out) = self.sessions.get_mut(&sid) {
                    out.vis_cell = Some(cell);
                }
                to_scan.push((sid, (px, py), cell_moved, false));
                continue;
            }
            // Size guard: patch work is proportional to the touched set.
            // When it approaches the view population (a dense-mover view,
            // e.g. a 1000-bot herd walking), a full rescan is cheaper than
            // re-examining every touched id - cap the patch at 128.
            let touched_n = self
                .world
                .gobs
                .vis
                .touched_count_in_view(px, py, VIEW_RADIUS);
            if touched_n > 0 && touched_n <= 128 {
                // Position unchanged, few touched gobs in view: patch.
                self.world.perf.vis_cached += 1;
                if let Some(out) = self.sessions.get_mut(&sid) {
                    out.vis_cell = Some(cell);
                }
                to_scan.push((sid, (px, py), cell_moved, true));
            } else if touched_n == 0 {
                // Position unchanged, nothing touched in view: the
                // result is provably unchanged. No candidate work this
                // tick; the retract sweep keeps its own cadence.
                self.world.perf.vis_cached += 1;
                if let Some(out) = self.sessions.get_mut(&sid) {
                    out.vis_cell = Some(cell);
                }
                if cell_moved || self.world.tick.is_multiple_of(8) {
                    self.retract_sweep_due(sid, px, py);
                }
                self.world.perf.visible_total += self.sessions[&sid].visible.len();
            } else {
                // Dense view: the cache exists but a full rescan wins.
                self.world.perf.vis_skipped += 1;
                if let Some(out) = self.sessions.get_mut(&sid) {
                    out.vis_cell = Some(cell);
                }
                to_scan.push((sid, (px, py), cell_moved, false));
            }
        }
        // --- Phase A2: grid-owner-partitioned candidate scan (parallel
        // when multiple sessions are present). Scan indices group by the
        // VisIndex-cell owner (grid_owner.rs) so one rayon task walks one
        // node's slice of the lattice — the same partitioning a multi-node
        // deployment hands to its owning node processes. Results reorder
        // back into to_scan order before phase B; the exact distance
        // filter is unchanged. ---
        let scan_t = Instant::now();
        let in_range: Vec<Vec<GobId>> = if self.workers > 1 && to_scan.len() > 8 {
            let nodes = std::num::NonZeroUsize::new(self.workers).expect("workers >= 1");
            let parts = crate::grid_owner::partition_by_owner(
                |&i| crate::visidx::cell_of(to_scan[i].1 .0, to_scan[i].1 .1),
                (0..to_scan.len()).collect::<Vec<usize>>(),
                nodes,
            );
            let mut by_index: Vec<(usize, Vec<GobId>)> = parts
                .par_iter()
                .map(|part| {
                    part.iter()
                        .map(|&i| (i, self.scan_for_entry(&to_scan[i])))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<Vec<_>>>()
                .into_iter()
                .flatten()
                .collect();
            by_index.sort_unstable_by_key(|(i, _)| *i);
            by_index.into_iter().map(|(_, v)| v).collect()
        } else {
            to_scan.iter().map(|e| self.scan_for_entry(e)).collect()
        };
        self.world.perf.vis_gob_scans += in_range.iter().map(|v| v.len() as u64).sum::<u64>();
        self.world.perf.vis_scan_us = scan_t.elapsed().as_micros() as u64;
        self.world.perf.vis_cells = self.world.gobs.vis.cell_count();
        // --- Phase B: serial application per session. ---
        // Movement deltas are NOT re-sent here: LINSTEP progress streams
        // from tick_movement's batch_move_broadcast every tick (10 Hz), so
        // a per-session needs_move rescan duplicated every progress frame
        // AND re-cloned it into unacked - at 800 sessions x ~200 movers
        // that was the dominant vis-phase cost.
        let mut spawn_us: u128 = 0;
        let mut retract_us: u128 = 0;
        let mut spawn_count: u64 = 0;
        for ((sid, (px, py), cell_moved, _kind), cand) in to_scan.into_iter().zip(in_range) {
            let spawn_t = Instant::now();
            for id in &cand {
                // Check-only here: stream_spawn performs the insert and
                // skips already-present ids; inserting before calling it
                // would suppress the spawn block entirely (the avatar
                // bug: the client never received its own gob).
                let is_new = !self.sessions[&sid].visible.contains(id);
                if is_new {
                    spawn_count += 1;
                    self.stream_spawn(sid, *id);
                }
            }
            // Retractions use a 2x VIEW_RADIUS hysteresis (a gob between
            // R and 2R stays spawned but off-screen), so a per-tick sweep
            // is wasted work: the sweep itself is debounced to once every
            // RETRACT_SWEEP_EVERY ticks regardless of cell crossings
            // (session 42 spawn churn fix). Deaths retract immediately via
            // broadcast_retract.
            spawn_us += spawn_t.elapsed().as_micros();
            let retract_t = Instant::now();
            if cell_moved || self.world.tick.is_multiple_of(8) {
                self.retract_sweep_due(sid, px, py);
            }
            // The result list becomes the session's cache (moved into
            // the session, no clone). Stored in scan order; the patch
            // path sorts its own working copy (see patch_vis_cache).
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.vis_cache = Some(cand);
                out.vis_cache_pos = Some((px, py));
            }
            retract_us += retract_t.elapsed().as_micros();
            self.world.perf.visible_total += self.sessions[&sid].visible.len();
        }
        self.world.perf.vis_spawn_us = spawn_us as u64;
        self.world.perf.vis_spawns = spawn_count;
        self.world.perf.vis_retract_us = retract_us as u64;
    }

    /// Current cached-list length for a session (0 = no cache).
    fn vis_cache_len(&self, sid: SessionId) -> usize {
        self.sessions
            .get(&sid)
            .and_then(|o| o.vis_cache.as_ref())
            .map_or(0, |v| v.len())
    }

    /// Resolve one to_scan entry to its in-range list (Phase A2 helper,
    /// pure read, rayon-friendly). Full = scan_visible; Patch = re-filter
    /// the cached list by current positions and add touched enterers.
    fn scan_for_entry(&self, e: &(SessionId, (i32, i32), bool, bool)) -> Vec<GobId> {
        // Slot 4: true = Patch (patch the cached result), false = Full.
        // The cached list is BORROWED (read) - no per-tick clone on the
        // hot path; all access here is immutable so rayon shares &self.
        let (sid, (px, py), _moved, patch) = (e.0, e.1, e.2, e.3);
        match self.sessions.get(&sid).and_then(|o| o.vis_cache.as_deref()) {
            Some(cached) if patch => self.patch_vis_cache(px, py, cached),
            _ => self.scan_visible(px, py),
        }
    }

    /// Patch a cached scan result for an unmoved session: keep every
    /// cached id still alive and in range (this re-filters leavers and
    /// purges deaths by construction), then add touched ids that are now
    /// in range and were not cached (enterers).
    ///
    /// The cached list is kept SORTED by the caller, so membership tests
    /// are binary searches - no per-tick HashSet allocation. The result
    /// is sorted + deduped before returning (touched lists may carry a
    /// boundary crosser twice).
    fn patch_vis_cache(&self, px: i32, py: i32, cached: &[GobId]) -> Vec<GobId> {
        let in_range = |id: GobId| -> bool {
            let gpos = self
                .world
                .gobs
                .get(id)
                .map(|slot| self.world.gobs.pos[slot])
                .or_else(|| self.world.guests.get(&id).map(|g| g.pos));
            match gpos {
                Some((gx, gy)) => (gx - px).abs() <= VIEW_RADIUS && (gy - py).abs() <= VIEW_RADIUS,
                None => false, // dead/retracted: never kept
            }
        };
        let mut out = Vec::with_capacity(cached.len() + 16);
        for &id in cached {
            if in_range(id) {
                out.push(id);
            }
        }
        // Sort the working copy BEFORE membership tests: the cached list
        // itself is stored in scan order (fill-time sorting would tax
        // every Full scan; the patch is the only consumer that needs
        // order). Touched lists may carry a boundary crosser twice.
        out.sort_unstable();
        out.dedup();
        for id in self.world.gobs.vis.touched_in_view(px, py, VIEW_RADIUS) {
            if in_range(id) && out.binary_search(&id).is_err() {
                out.push(id);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Spawn-churn debounce (session 42): gate the retract sweep to at
    /// most one run per RETRACT_SWEEP_EVERY ticks per session. The old
    /// `cell_moved` trigger swept a fast-moving session EVERY tick, so a
    /// gob oscillating across the 2x VIEW_RADIUS boundary was retracted
    /// and re-spawned on every crossing - the duel cohort at 1000 bots
    /// produced ~420 spawns/tick (mean) and made the spawn phase the
    /// dominant vis cost. With the 0.8 s grace a quick boundary return
    /// never sees a retract at all, while a genuinely departed gob still
    /// disappears well under a second late (it is 2 view radii off-screen
    /// by then). Deaths bypass this gate via broadcast_retract.
    fn retract_sweep_due(&mut self, sid: SessionId, px: i32, py: i32) {
        let tick = self.world.tick;
        let due = {
            let Some(out) = self.sessions.get(&sid) else {
                return;
            };
            tick.saturating_sub(out.last_retract_tick) >= RETRACT_SWEEP_EVERY
        };
        if !due {
            return;
        }
        self.retract_sweep(sid, px, py);
        if let Some(out) = self.sessions.get_mut(&sid) {
            out.last_retract_tick = tick;
        }
    }

    /// The retract sweep for one session (2x VIEW_RADIUS hysteresis; dead
    /// and gone ids retract too). Runs on the cell-crossing/8-tick
    /// cadence from both the Clean and the scan paths.
    fn retract_sweep(&mut self, sid: SessionId, px: i32, py: i32) {
        let to_retract: Vec<GobId> = {
            let Some(out) = self.sessions.get_mut(&sid) else {
                return;
            };
            out.visible
                .iter()
                .filter(|&&id| {
                    // Position: local gob columns first, cluster
                    // guests second; neither = dead, must retract.
                    let gpos = self
                        .world
                        .gobs
                        .get(id)
                        .map(|slot| self.world.gobs.pos[slot])
                        .or_else(|| self.world.guests.get(&id).map(|g| g.pos));
                    match gpos {
                        Some((gx, gy)) => {
                            (gx - px).abs() > VIEW_RADIUS * 2 || (gy - py).abs() > VIEW_RADIUS * 2
                        }
                        None => true, // dead gobs get retracted too
                    }
                })
                .copied()
                .collect()
        };
        for id in to_retract {
            self.stream_retract(sid, id);
        }
    }

    /// Pure in-range gob scan around a point (no mutation; rayon-friendly).
    /// In-range gob scan around a point: query the dirty-cell index for
    /// the view cells, then apply the exact distance filter (cells are
    /// coarse buckets; the filter preserves the old O(all gobs) result).
    /// Cluster guests merge in (foreign-authority gobs rendered locally);
    /// the guest table only holds gobs some local session subscribed to,
    /// so the scan cost stays bounded by what this node actually views.
    fn scan_visible(&self, px: i32, py: i32) -> Vec<GobId> {
        let candidates = self.world.gobs.vis.gobs_in_view(px, py, VIEW_RADIUS);
        let mut out = Vec::new();
        for id in candidates {
            let Some(slot) = self.world.gobs.get(id) else {
                continue;
            };
            let (gx, gy) = self.world.gobs.pos[slot];
            if (gx - px).abs() > VIEW_RADIUS || (gy - py).abs() > VIEW_RADIUS {
                continue;
            }
            out.push(id);
        }
        for (&id, g) in &self.world.guests {
            if (g.pos.0 - px).abs() > VIEW_RADIUS || (g.pos.1 - py).abs() > VIEW_RADIUS {
                continue;
            }
            out.push(id);
        }
        out
    }

    fn on_objack(&mut self, sid: SessionId, acks: Vec<(GobId, u32)>) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        for (id, frame) in acks {
            if let Some(per_gob) = out.unacked.get_mut(&id) {
                per_gob.retain(|f, _| *f > frame);
                if per_gob.is_empty() {
                    out.unacked.remove(&id);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Multi-node cluster (grid-owner process split, session 27).
    //
    // Authority rule: animals and world gobs are simulated by the owner of
    // the VisIndex cell they stand in (grid_owner::owner_of); players are
    // ALWAYS simulated by their home node (the node their UDP session
    // landed on). Foreign-authority gobs render locally as guests through
    // the same visibility machinery — see nodes.rs for the wire contract.
    // ------------------------------------------------------------------

    fn is_cluster(&self) -> bool {
        self.cluster.is_some()
    }

    /// Owning node of one VisIndex cell (0 in single-node mode).
    fn cell_owner(&self, cell: (i32, i32)) -> usize {
        match &self.cluster {
            Some(c) => crate::grid_owner::owner_of(cell, c.nodes),
            None => 0,
        }
    }

    /// Simulation authority for the gob at `slot`. Players in MY gob table
    /// are my sessions' players — homed here by definition. Everything else
    /// follows its cell's owner.
    fn is_authority_slot(&self, slot: usize) -> bool {
        match &self.cluster {
            None => true,
            Some(c) => {
                if matches!(self.world.gobs.kind[slot], Kind::Player { .. }) {
                    return true;
                }
                let cell = crate::visidx::cell_of(
                    self.world.gobs.pos[slot].0,
                    self.world.gobs.pos[slot].1,
                );
                crate::grid_owner::owner_of(cell, c.nodes) == c.me
            }
        }
    }

    /// Home node of a gob id: cluster slot ranges partition [0, MAX_SLOT]
    /// in equal strides (Gobs::with_layout), so the slot index maps
    /// directly onto its allocating node (the last node owns the
    /// remainder). Single-node mode is always node 0.
    fn node_of_gob(&self, id: GobId) -> usize {
        match &self.cluster {
            None => 0,
            Some(c) => {
                let slot = (id & 0xFFFF) as usize;
                let per = (crate::state::MAX_SLOT + 1) / c.nodes.get();
                (slot / per).min(c.nodes.get() - 1)
            }
        }
    }

    /// Node-link message dispatch (cluster mode only).
    fn on_node_msg(&mut self, msg: crate::nodes::NodeMsg) {
        use crate::nodes::NodeMsg;
        match msg {
            NodeMsg::Ping => {}
            NodeMsg::Hello { .. } => {} // handshake handled by the mesh
            NodeMsg::Sub { from, cells } => {
                let owned: Vec<(i32, i32)> = {
                    let Some(c) = self.cluster.as_mut() else {
                        return;
                    };
                    let owned: Vec<(i32, i32)> = cells
                        .into_iter()
                        .filter(|&cell| crate::grid_owner::owner_of(cell, c.nodes) == c.me)
                        .collect();
                    tracing::debug!(from, cells = owned.len(), "peer subscribed");
                    // The sender ships DIFFS (tick_cluster sends only the
                    // added cells; Unsub removes), so apply incrementally.
                    // Session 34: the old whole-set replace silently
                    // dropped every previously subscribed cell the first
                    // time a moving session's view produced a second Sub
                    // - cross-node updates stopped flowing for cells the
                    // peer still subscribes to (a lit guest oven never
                    // re-rendered).
                    c.peer_subs
                        .entry(from)
                        .or_default()
                        .extend(owned.iter().copied());
                    owned
                };
                // Session 33 (Sub-driven populate): the subscriber's own
                // MAPREQ never materialized MY content for these cells
                // (owner-filtered populate) - materialize it now and
                // announce everything I hold there, so the subscriber
                // renders one authoritative copy per gob.
                self.populate_for_subscriber(from, owned);
            }
            NodeMsg::Unsub { from, cells } => {
                let Some(c) = self.cluster.as_mut() else {
                    return;
                };
                if let Some(subs) = c.peer_subs.get_mut(&from) {
                    for cell in cells {
                        subs.remove(&cell);
                    }
                    if subs.is_empty() {
                        c.peer_subs.remove(&from);
                    }
                }
            }
            NodeMsg::Chat { from, at, text } => self.deliver_remote_chat(&from, at, &text),
            NodeMsg::GuestAnnounce(st) => self.ingest_guest(st),
            NodeMsg::GuestUpdate(st) => self.ingest_guest(st),
            NodeMsg::GuestRetract { id } => self.remove_guest(id),
            NodeMsg::GuestTransfer(st) => self.promote_transfer(st),
            NodeMsg::RelayAttack {
                attacker,
                target,
                chip,
                dmg,
            } => self.relay_swing(attacker, target, chip, dmg),
            NodeMsg::FightBars { id, def } => {
                // Authoritative defence bar from the animal's owner; the
                // mirror self-heals from it (the fightview reads this).
                if let Some(af) = self.world.guest_fights.get_mut(&id) {
                    af.def = def.clamp(0, crate::fight::BAR_FULL);
                    tracing::debug!(id, def, "relay fight bars synced");
                }
            }
            NodeMsg::PlayerHurt {
                player_gob,
                dmg,
                from,
            } => {
                // Animal retaliation against one of MY session players;
                // armor absorption and the knockout path live here.
                if let Some(pidx) = self.world.players.iter().position(|p| p.gob == player_gob) {
                    tracing::debug!(player_gob, dmg, from, "relay bite applied");
                    self.hurt_player(pidx, dmg, from);
                    // Native bite visual on the victim (the owner node's
                    // own overlay covers only ITS local viewers).
                    self.fx_overlay_broadcast(player_gob, "gfx/fx/bite");
                }
            }
            NodeMsg::PvpArrow {
                victim,
                attacker,
                dmg,
            } => {
                // Cross-node PvP arrow (session 38): one of MY session
                // players was hit by a foreign archer. Armor absorption,
                // HP and the knockout path live here (the same
                // hurt_player authority split as PlayerHurt); the
                // shooter's node gets the outcome back so its chat can
                // report the defeat.
                if let Some(pidx) = self.world.players.iter().position(|p| p.gob == victim) {
                    tracing::debug!(victim, attacker, dmg, "pvp arrow applied");
                    let vsid = self.world.players[pidx].session;
                    let knocked = self.hurt_player(pidx, dmg, attacker);
                    self.chat_line(
                        vsid,
                        &format!("An arrow hits you for {dmg} damage."),
                        Some((255, 128, 128)),
                    );
                    self.fx_overlay_broadcast(victim, "gfx/fx/hit");
                    if knocked {
                        // Authority side of the knockout consequences: the
                        // victim's LP share lives here (home node); the
                        // shooter's criminal flag is applied when the
                        // PvpArrowResult answer reaches ITS node.
                        self.knockout_lp_loss(pidx);
                    }
                    if let Some(c) = self.cluster.as_ref() {
                        let home = self.node_of_gob(attacker);
                        c.mesh.send(
                            home,
                            crate::nodes::NodeMsg::PvpArrowResult {
                                shooter: attacker,
                                killed: knocked,
                            },
                        );
                    }
                }
            }
            NodeMsg::PvpArrowResult { shooter, killed } => {
                // The victim's home node answered my shot.
                if let Some(p) = self.world.players.iter().find(|p| p.gob == shooter) {
                    if killed {
                        let sid = p.session;
                        self.chat_line(
                            sid,
                            "You have defeated your target!",
                            Some((192, 255, 192)),
                        );
                        // Winner's share of the knockout consequences
                        // (server policy): the criminal flag lives on
                        // the shooter's home node - here.
                        if let Some(widx) = self.world.players.iter().position(|p| p.gob == shooter)
                        {
                            self.flag_criminal(widx);
                        }
                    }
                }
            }
            NodeMsg::PvpSwing {
                attacker,
                victim,
                chip,
                dmg,
            } => {
                // Cross-node melee PvP (session 39): one of MY session
                // players is being swung at by a foreign attacker. The
                // victim's defence bar, armor, HP and the knockout path
                // are all authoritative here; the applied outcome goes
                // back as PvpSwingResult so the attacker's mirror and
                // chat stay truthful.
                if let Some(pidx) = self.world.players.iter().position(|p| p.gob == victim) {
                    tracing::debug!(attacker, victim, chip, dmg, "pvp swing applied");
                    let vsid = self.world.players[pidx].session;
                    let (landed, def_after) = {
                        let Some(vout) = self.sessions.get_mut(&vsid) else {
                            return;
                        };
                        let breaking = vout.fight.own_def <= crate::fight::OPENING_THRESHOLD;
                        vout.fight.own_def = (vout.fight.own_def - chip).max(0);
                        let landed =
                            breaking || vout.fight.own_def <= crate::fight::OPENING_THRESHOLD;
                        if landed {
                            vout.fight.own_def = crate::fight::BAR_FULL;
                        }
                        (landed, vout.fight.own_def)
                    };
                    let def_now = def_after;
                    let mut killed = false;
                    if landed {
                        killed = self.hurt_player(pidx, dmg, attacker);
                        // The attacker is published here as a guest while
                        // both players see each other; fall back to an
                        // anonymous line if the view already dropped.
                        let aname = self
                            .world
                            .guests
                            .get(&attacker)
                            .and_then(|g| g.kind.player_name())
                            .unwrap_or("Someone")
                            .to_owned();
                        self.chat_line(
                            vsid,
                            &format!("{aname} hits you for {dmg} damage."),
                            Some((255, 128, 128)),
                        );
                        self.fx_overlay_broadcast(victim, "gfx/fx/hit");
                        if killed {
                            // Authority side of the knockout consequences:
                            // the victim's LP share lives here (home
                            // node); the attacker's criminal flag is
                            // applied from the PvpSwingResult answer.
                            self.knockout_lp_loss(pidx);
                        }
                    }
                    if let Some(c) = self.cluster.as_ref() {
                        let home = self.node_of_gob(attacker);
                        c.mesh.send(
                            home,
                            crate::nodes::NodeMsg::PvpSwingResult {
                                attacker,
                                victim,
                                def: def_now,
                                landed,
                                killed,
                            },
                        );
                    }
                }
            }
            NodeMsg::ManeuverDelta {
                attacker,
                victim,
                ip_opp,
            } => {
                // The foreign attacker's node relayed a maneuver's
                // opponent-pool delta (session 42): my session player's
                // IP pool is authoritative here, keyed by the attacker's
                // guest gob. Fold and re-stream the victim's window; the
                // attacker's own window already applied the mirror
                // prediction.
                if ip_opp == 0 {
                    return;
                }
                let Some(pidx) = self.world.players.iter().position(|p| p.gob == victim) else {
                    return;
                };
                let vsid = self.world.players[pidx].session;
                let vupd = self.sessions.get_mut(&vsid).and_then(|vout| {
                    let rel = vout.fight.rel_mut(attacker)?;
                    rel.ip_self = (rel.ip_self + ip_opp).max(0);
                    Some(vec![
                        rel.gob,
                        rel.balance,
                        rel.intensity,
                        rel.give,
                        rel.ip_self,
                        rel.ip_other,
                    ])
                });
                if let Some(vupd) = vupd {
                    if let Some(vout) = self.sessions.get_mut(&vsid) {
                        if let Some(w) = vout.fight.widget {
                            let b = crate::fight::uimsg(w, "upd", &vupd);
                            vout.send(b);
                        }
                    }
                }
            }
            NodeMsg::PvpSwingResult {
                attacker,
                victim,
                def,
                landed,
                killed,
            } => {
                // The victim's home node answered my swing: re-sync the
                // local mirror (the fightview reads it) and close the
                // narrative on a landed hit / knockout.
                if let Some(p) = self.world.players.iter().find(|p| p.gob == attacker) {
                    let sid = p.session;
                    if let Some(mf) = self.world.guest_fights.get_mut(&victim) {
                        mf.def = def.clamp(0, crate::fight::BAR_FULL);
                    }
                    if let Some(out) = self.sessions.get_mut(&sid) {
                        if let Some(rel) = out.fight.rel_mut(victim) {
                            rel.defence = def.clamp(0, crate::fight::BAR_FULL);
                        }
                    }
                    if landed {
                        let vname = self
                            .world
                            .guests
                            .get(&victim)
                            .and_then(|g| g.kind.player_name())
                            .unwrap_or("your target");
                        self.chat_line(
                            sid,
                            &format!("You hit {vname} for damage."),
                            Some((192, 255, 192)),
                        );
                        if killed {
                            self.chat_line(
                                sid,
                                "You have defeated your target!",
                                Some((192, 255, 192)),
                            );
                            let pidx = self.world.players.iter().position(|p| p.gob == attacker);
                            if let Some(pidx) = pidx {
                                self.world.players[pidx].fight_target = None;
                                // Winner's share of the knockout
                                // consequences (server policy): the
                                // criminal flag lives on the attacker's
                                // home node - here.
                                self.flag_criminal(pidx);
                            }
                            self.world.guest_fights.remove(&victim);
                            self.fight_del(sid, victim);
                        }
                    }
                }
            }
            NodeMsg::KillCredit { player_gob, lp } => {
                if let Some(p) = self.world.players.iter_mut().find(|p| p.gob == player_gob) {
                    p.lp += lp;
                    let sid = p.session;
                    self.push_cattr(sid);
                }
            }
            NodeMsg::RelayStaticAct {
                player,
                target,
                act,
            } => {
                self.relay_static(player, target, act);
            }
            NodeMsg::StaticAck { player, stack, lp } => {
                // Authority applied a relayed static act for MY session
                // player: grant the stack/lp exactly like the local path.
                if let Some(pidx) = self.world.players.iter().position(|p| p.gob == player) {
                    let sid = self.world.players[pidx].session;
                    if let Some(s) = stack {
                        let res_name = leak_static(s.res.as_str());
                        let gidx = self.world.res.intern(res_name);
                        self.grant_pickup(
                            sid,
                            InvStack {
                                res: gidx,
                                count: s.count,
                                ql: s.ql,
                                label: leak_static(s.label.as_str()),
                            },
                        );
                    }
                    if lp > 0 {
                        self.world.players[pidx].lp += lp;
                        self.push_cattr(sid);
                        self.push_lp_msgs(sid);
                    }
                }
            }
            NodeMsg::RelayPlantAct {
                player,
                tx,
                ty,
                spec,
                seed_ql,
            } => {
                self.relay_plant(player, tx, ty, spec, seed_ql);
            }
            NodeMsg::PlantAck { player, ok } => {
                // The authority planted the crop: NOW the seed leaves the
                // cursor. A failure ack (or silence from a downed peer)
                // keeps the seed - the player can retry.
                if !ok {
                    return;
                }
                if let Some(pidx) = self.world.players.iter().position(|p| p.gob == player) {
                    let sid = self.world.players[pidx].session;
                    if let Some(cursor) = self.sessions.get(&sid).and_then(|o| o.cursor) {
                        let mut cursor = cursor;
                        cursor.count = cursor.count.saturating_sub(1);
                        if let Some(out) = self.sessions.get_mut(&sid) {
                            out.cursor = if cursor.count == 0 {
                                None
                            } else {
                                Some(cursor)
                            };
                        }
                        self.refresh_inventory(sid);
                        self.sync_cursor_widget(sid);
                    }
                }
            }
            NodeMsg::RelayPlowAct { player, tx, ty } => {
                self.relay_plow(player, tx, ty);
            }
            NodeMsg::PlowAck { player, ok } => {
                // The authority plowed the furrow: NOW the stamina leaves
                // the player (never before the ack - a refused or lost
                // relay costs nothing, exactly like a local refusal).
                if !ok {
                    return;
                }
                if let Some(p) = self.world.players.iter_mut().find(|p| p.gob == player) {
                    p.stamina = (p.stamina - 10).max(0);
                }
            }
            NodeMsg::TileMutation { tx, ty, tile } => {
                self.apply_remote_tile_mutation(tx, ty, tile);
            }
            NodeMsg::RelayStationAct {
                player,
                target,
                act,
            } => {
                self.relay_station_act(player, target, act);
            }
            NodeMsg::StationAck { player, result } => {
                // Authority outcome for MY session player's station menu
                // choice. Refusals render the SAME system lines the local
                // path emits; success and stale views stay silent.
                let Some(pidx) = self.world.players.iter().position(|p| p.gob == player) else {
                    return;
                };
                let sid = self.world.players[pidx].session;
                match result {
                    crate::nodes::StationResult::Lit
                    | crate::nodes::StationResult::Extinguished
                    | crate::nodes::StationResult::Stale => {}
                    crate::nodes::StationResult::NeedsFuel => {
                        self.system_line(sid, "The oven needs fuel first.");
                    }
                    crate::nodes::StationResult::NeedsInput => {
                        self.system_line(sid, "The oven needs an input before lighting.");
                    }
                }
            }
            NodeMsg::RelayStationItem {
                player,
                target,
                stack,
            } => {
                self.relay_station_item(player, target, stack);
            }
            NodeMsg::StationItemAck { player, result } => {
                // Authority outcome for MY session player's fuel/input
                // delivery. FuelAdded/InputLoaded consume ONE cursor unit
                // NOW (never before the ack - a refused or lost relay
                // must not destroy the item, the seed-safe pattern).
                use crate::nodes::StationItemResult;
                let Some(pidx) = self.world.players.iter().position(|p| p.gob == player) else {
                    return;
                };
                let sid = self.world.players[pidx].session;
                match result {
                    StationItemResult::FuelAdded => {
                        self.consume_cursor_unit(sid);
                        self.system_line(sid, "Fuel added to the oven.");
                    }
                    StationItemResult::InputLoaded => {
                        self.consume_cursor_unit(sid);
                        self.system_line(sid, "Input loaded; right-click the oven to light it.");
                    }
                    StationItemResult::BusyLit => {
                        self.system_line(sid, "The fire is burning; wait for it to finish.");
                    }
                    StationItemResult::InputFull => {
                        self.system_line(sid, "The oven already holds an input.");
                    }
                    StationItemResult::NotProcessable => {
                        self.system_line(sid, "The oven cannot process that.");
                    }
                    StationItemResult::Gone => {}
                }
            }
            NodeMsg::CharQuery { from, name } => {
                // Cluster character migration, two-phase (query -> data ->
                // ack). A peer holding the snapshot OFFLINE re-serves it on
                // every query until the CharAck confirms adoption; a peer
                // without the key (or with the character ONLINE - a live
                // player keeps its home) answers CharNack.
                if self.cluster.is_none() || from == self.cluster_me() {
                    return;
                }
                let online = self
                    .world
                    .players
                    .iter()
                    .any(|p| crate::persist::save_key(&p.account, &p.name) == name);
                if online || !self.save.players.contains_key(&name) {
                    debug!(from, %name, "char query: nack (missing or online)");
                    let me = self.cluster_me();
                    self.cluster_mesh().send(
                        from,
                        NodeMsg::CharNack {
                            to: from,
                            from: me,
                            name,
                        },
                    );
                    return;
                }
                let Some(snap) = self.save.players.get(&name).cloned() else {
                    return;
                };
                info!(from, %name, "char query: serving snapshot to peer");
                let me = self.cluster_me();
                self.cluster_mesh().send(
                    from,
                    NodeMsg::CharData {
                        to: from,
                        from: me,
                        name,
                        snap,
                    },
                );
            }
            NodeMsg::CharData {
                to,
                from,
                name,
                snap,
            } => {
                if self.cluster.is_none() || to != self.cluster_me() {
                    return;
                }
                // Mark the peer answered even if the join is gone: a
                // duplicate CharData after a retry needs no further nacks.
                if let Some((_, join)) = self
                    .pending_joins
                    .iter_mut()
                    .find(|(_, j)| crate::persist::save_key(&j.account, &j.chosen) == name)
                {
                    join.answered.insert(from);
                }
                let Some((sid, chosen)) = self
                    .pending_joins
                    .iter()
                    .find(|(_, j)| crate::persist::save_key(&j.account, &j.chosen) == name)
                    .map(|(s, j)| (*s, j.chosen.clone()))
                else {
                    debug!(%name, "late CharData with no pending join: dropped");
                    return;
                };
                self.pending_joins.remove(&sid);
                info!(sid, %name, pos = ?snap.pos, "char migration received: entering world");
                self.save.players.insert(name.clone(), snap);
                // Confirm adoption so the holder drops its copy.
                self.cluster_mesh().send(from, NodeMsg::CharAck { name });
                self.enter_world_inner(sid, chosen, false);
            }
            NodeMsg::CharAck { name } => {
                // The requester adopted the snapshot: the migration is
                // durable. Drop the local copy and persist the removal.
                if self.cluster.is_none() || !self.save.players.contains_key(&name) {
                    return;
                }
                info!(%name, "char ack: dropping migrated snapshot");
                self.save.players.remove(&name);
                if let Err(e) = self.save.flush(self.world.seed) {
                    tracing::warn!(error = %e, "char migration flush failed");
                }
            }
            NodeMsg::CharNack { to, from, name } => {
                if self.cluster.is_none() || to != self.cluster_me() || from == self.cluster_me() {
                    return;
                }
                let nodes = self.cluster_nodes();
                let Some((sid, all_answered, chosen)) = self
                    .pending_joins
                    .iter_mut()
                    .find(|(_, j)| crate::persist::save_key(&j.account, &j.chosen) == name)
                    .map(|(s, j)| {
                        j.answered.insert(from);
                        let done = j.peers_answered(nodes);
                        (*s, done, if done { Some(j.chosen.clone()) } else { None })
                    })
                else {
                    debug!(%name, "late CharNack with no pending join: dropped");
                    return;
                };
                if !all_answered {
                    return;
                }
                self.pending_joins.remove(&sid);
                if let Some(chosen) = chosen {
                    info!(sid, %name, "char query: every peer answered, entering fresh");
                    self.enter_world_inner(sid, chosen, false);
                }
            }
        }
    }

    /// My node index (single-node mode: 0).
    fn cluster_me(&self) -> usize {
        self.cluster.as_ref().map(|c| c.me).unwrap_or(0)
    }

    /// Peer count of the cluster (single-node mode: 0).
    fn cluster_nodes(&self) -> usize {
        self.cluster.as_ref().map(|c| c.nodes.get()).unwrap_or(0)
    }

    /// Mesh handle; single-node mode has none, so callers must only use
    /// this after an `is_cluster()` check.
    fn cluster_mesh(&self) -> &crate::nodes::Mesh {
        &self
            .cluster
            .as_ref()
            .expect("BUG: cluster_mesh called outside cluster mode")
            .mesh
    }

    /// Render state of the local gob at `slot` as a wire guest state.
    /// The render resource NAME of a local gob (its world shape, not the
    /// inventory icon) as carried by GuestKind::Static. Leaks into the
    /// interned-name arena like every other cross-node string.
    fn static_res_name(&self, slot: usize) -> String {
        self.world
            .res
            .name(self.world.gobs.res_idx[slot])
            .unwrap_or("gfx/terobjs/items/branch")
            .to_string()
    }

    fn guest_state_from_slot(&self, id: GobId, slot: usize) -> Option<crate::nodes::GuestState> {
        use crate::nodes::{GuestKind, GuestLinMove, GuestState};
        let kind = match self.world.gobs.kind[slot] {
            Kind::Animal { species } => GuestKind::Animal {
                species: species.index(),
            },
            Kind::Player { player } => GuestKind::Player {
                name: self.world.players.get(player)?.name.clone(),
                equip: self
                    .player_equip_names(player)
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect(),
            },
            // Statics publish too (session 30): drops near a cell boundary
            // must be visible AND clickable across nodes. The render name
            // is the gob's world shape (res_idx); the class tag is stable
            // for the gob's lifetime, so subscribers never need an update
            // to pick the right relay act.
            // Session 35: the Drop arm carries the FULL drop payload
            // (DropView) so an authority transfer can rebuild Kind::Drop on
            // the receiving node; plain guest publishes leave it None
            // (subscribers render from res_name and relay Pickup acts).
            Kind::Drop {
                inv_res_idx,
                ql,
                label,
                ..
            } => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Drop,
                crop: None,
                station: None,
                stage: None,
                drop: Some(crate::nodes::DropView {
                    inv_res: self.world.res.name(inv_res_idx).unwrap_or("").to_owned(),
                    ql,
                    label: label.to_owned(),
                }),
            },
            Kind::Tree { .. } => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Tree,
                crop: None,
                station: None,
                stage: None,
                drop: None,
            },
            Kind::Stone => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Stone,
                crop: None,
                station: None,
                stage: None,
                drop: None,
            },
            // Plans/structures: renderable but no relay act today (their
            // menus are session UI on the authority side).
            // Stations (session 33): publish with the Station class + a
            // state snapshot (spec, lit, fuel, has_input) piggybacked on
            // the payload - the home node opens the Light/Extinguish
            // menu locally from it, and every state change re-publishes
            // so subscribers re-render the lit sprite from the sdt byte.
            // Crops (session 31): publish with the Crop class + the
            // (spec, stage) payload - the home node opens the harvest
            // menu locally and the stage re-renders on the subscriber
            // through the sdt byte in the guest spawn/update blocks.
            Kind::Station { spec, lit } => {
                let view = self
                    .world
                    .stations
                    .get(&id)
                    .map(|st| crate::nodes::StationView {
                        spec: st.spec,
                        lit: st.lit,
                        fuel: st.fuel,
                        has_input: st.input.is_some(),
                    });
                GuestKind::Static {
                    res_name: self.static_res_name(slot),
                    class: crate::nodes::StaticClass::Station,
                    crop: None,
                    station: view.or(Some(crate::nodes::StationView {
                        spec,
                        lit,
                        fuel: 0,
                        has_input: false,
                    })),
                    stage: None,
                    drop: None,
                }
            }
            // Plans publish their construction stage (session 34) so a
            // peer watching a build re-renders the plan sprite on every
            // credited material. Structures carry no stage: their final
            // form is the plain sprite. Both publish the Structure class
            // - no relay act - until completion re-publishes the real
            // kind (a finished oven becomes a Station with its snapshot).
            Kind::Plan { stage, .. } => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Structure,
                crop: None,
                station: None,
                stage: Some(stage),
                drop: None,
            },
            Kind::Structure { .. } => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Structure,
                crop: None,
                station: None,
                stage: None,
                drop: None,
            },
            Kind::Crop { spec, stage } => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Crop,
                crop: Some((spec, stage)),
                station: None,
                stage: None,
                drop: None,
            },
        };
        Some(GuestState {
            id,
            pos: self.world.gobs.pos[slot],
            mv: self.world.gobs.mv[slot].map(|lm| GuestLinMove {
                sx: lm.sx,
                sy: lm.sy,
                tx: lm.tx,
                ty: lm.ty,
                steps: lm.steps,
                step: lm.step,
                started_ms: lm.started_ms,
                total_ms: lm.total_ms,
            }),
            moving: self.world.gobs.mv[slot].is_some(),
            facing: self.world.gobs.facing[slot],
            kind,
            hp: self.world.gobs.hp[slot],
            max_hp: self.world.gobs.max_hp[slot],
            speed: self.world.gobs.speed[slot],
        })
    }

    /// Recipient peers for a local gob's guest stream: every peer
    /// subscribed to the gob's cell, plus (for abroad players) the owner
    /// of the cell the player stands in.
    fn publish_targets(&self, id: GobId, slot: usize) -> Vec<usize> {
        let Some(c) = &self.cluster else {
            return Vec::new();
        };
        let cell = crate::visidx::cell_of(self.world.gobs.pos[slot].0, self.world.gobs.pos[slot].1);
        let mut targets: Vec<usize> = c
            .peer_subs
            .iter()
            .filter(|(_, cells)| cells.contains(&cell))
            .map(|(p, _)| *p)
            .collect();
        if matches!(self.world.gobs.kind[slot], Kind::Player { .. }) {
            if let Some(&owner) = c.player_abroad.get(&id) {
                if !targets.contains(&owner) {
                    targets.push(owner);
                }
            }
        }
        targets
    }

    /// The grids a VisIndex cell touches (session 33). A cell spans 250
    /// subtiles, a grid 1100 (100 tiles x 11), so one cell touches one
    /// or two grids per axis - at most four grids total.
    fn grids_touching_cell(cell: (i32, i32)) -> Vec<(i32, i32)> {
        let (cx, cy) = cell;
        let gx0 = (cx * 250).div_euclid(1100);
        let gx1 = (cx * 250 + 249).div_euclid(1100);
        let gy0 = (cy * 250).div_euclid(1100);
        let gy1 = (cy * 250 + 249).div_euclid(1100);
        let mut out = Vec::with_capacity(4);
        for gx in gx0..=gx1 {
            for gy in gy0..=gy1 {
                out.push((gx, gy));
            }
        }
        out
    }

    /// Sub-driven populate on the authority (session 33). The subscriber
    /// materialized the TILES of the grids it looks at (deterministic,
    /// identical on every node) but spawned no content for MY cells
    /// (owner-filtered populate). Here I materialize my part of every
    /// touched grid (idempotent per grid) and announce every gob I hold
    /// in the subscribed cells - freshly spawned AND pre-existing - so
    /// the subscriber's view starts from the one authoritative copy.
    fn populate_for_subscriber(&mut self, from: usize, cells: Vec<(i32, i32)>) {
        if cells.is_empty() {
            return;
        }
        // (a) Materialize my part of every grid the cells touch.
        let filter = self.cluster.as_ref().map(|c| (c.me, c.nodes));
        let mut fresh: Vec<GobId> = Vec::new();
        let mut touched: HashSet<(i32, i32)> = HashSet::new();
        for cell in &cells {
            for gc in Self::grids_touching_cell(*cell) {
                if touched.insert(gc) && !self.populated.contains(&gc) {
                    self.populated.insert(gc);
                    self.world.populate_grid(gc, filter, &mut fresh);
                    let animals = if self.saturated { 40 } else { 4 };
                    self.world.populate_animals(gc, filter, animals, &mut fresh);
                }
            }
        }
        // (b) Announce everything I hold in the subscribed cells: the
        // freshly spawned content plus anything that existed earlier
        // (stations, structures, previously populated statics). The cell
        // center +/- 124 subtiles spans exactly one VisIndex cell.
        let mut announce: Vec<GobId> = fresh;
        for &(cx, cy) in &cells {
            let ids = self
                .world
                .gobs
                .vis
                .gobs_in_view(cx * 250 + 125, cy * 250 + 125, 124);
            for id in ids {
                if !announce.contains(&id) {
                    announce.push(id);
                }
            }
        }
        let count = announce.len();
        for id in announce {
            self.publish(id, GuestEv::Announce);
        }
        tracing::debug!(from, gobs = count, "subscriber populate announced");
    }

    /// Publish one local gob event to interested peers. Announce = full
    /// state (new viewer/owner), Update = movement/pose delta, Retract =
    /// death/removal.
    fn publish(&mut self, id: GobId, ev: GuestEv) {
        use crate::nodes::NodeMsg;
        if !self.is_cluster() {
            return;
        }
        // A Retract may target an ALREADY-KILLED gob (every death path
        // kills first, then retracts): resolve through the split id with
        // a generation check, so a reused slot never retracts a stranger.
        // Before session 30 this early-returned on dead gobs, so remote
        // retracts never fired at all - subscribers had to wait for their
        // own GC sweep. Announce/Update of a dead gob stays a no-op.
        let slot = match self.world.gobs.get(id) {
            Some(slot) => slot,
            None => {
                let (slot, gen) = crate::state::split_gob_id(id);
                if slot >= self.world.gobs.alive.len() {
                    return;
                }
                if self.world.gobs.gen[slot] != gen {
                    return; // slot was reused: the id is ancient history
                }
                if !matches!(ev, GuestEv::Retract) {
                    return;
                }
                slot
            }
        };
        match ev {
            GuestEv::Retract => {
                for peer in self.publish_targets(id, slot) {
                    self.cluster
                        .as_ref()
                        .expect("checked above")
                        .mesh
                        .send(peer, NodeMsg::GuestRetract { id });
                    self.world.perf.guest_pub += 1;
                }
            }
            GuestEv::Announce | GuestEv::Update => {
                let Some(st) = self.guest_state_from_slot(id, slot) else {
                    return;
                };
                for peer in self.publish_targets(id, slot) {
                    let msg = match ev {
                        GuestEv::Announce => NodeMsg::GuestAnnounce(st.clone()),
                        GuestEv::Update => NodeMsg::GuestUpdate(st.clone()),
                        GuestEv::Retract => unreachable!("routed above"),
                    };
                    self.cluster
                        .as_ref()
                        .expect("checked above")
                        .mesh
                        .send(peer, msg);
                    self.world.perf.guest_pub += 1;
                }
            }
        }
    }

    /// Per-tick cluster maintenance: subscription diffs, player territory
    /// publishing, animal authority transfer on cell crossing, guest GC.
    fn tick_cluster(&mut self) {
        use crate::nodes::NodeMsg;
        if !self.is_cluster() {
            return;
        }
        let me = self.cluster.as_ref().expect("cluster").me;

        // --- Subscription maintenance (every 10 ticks): my sessions' view
        // cells unioned, filtered per peer to the cells that peer owns,
        // diffed against the current subscription set.
        if self.world.tick.is_multiple_of(10) {
            let wanted = self.wanted_view_cells();
            let nodes = self.cluster.as_ref().expect("cluster").nodes;
            let n = nodes.get();
            let mut sends: Vec<(usize, NodeMsg)> = Vec::new();
            {
                let c = self.cluster.as_mut().expect("cluster");
                for peer in 0..n {
                    if peer == c.me {
                        continue;
                    }
                    let want: HashSet<(i32, i32)> = wanted
                        .iter()
                        .copied()
                        .filter(|&cell| crate::grid_owner::owner_of(cell, nodes) == peer)
                        .collect();
                    let cur = c.my_subs.entry(peer).or_default();
                    let added: Vec<(i32, i32)> = want.difference(cur).copied().collect();
                    let removed: Vec<(i32, i32)> = cur.difference(&want).copied().collect();
                    if !added.is_empty() {
                        sends.push((
                            peer,
                            NodeMsg::Sub {
                                from: me,
                                cells: added,
                            },
                        ));
                    }
                    if !removed.is_empty() {
                        sends.push((
                            peer,
                            NodeMsg::Unsub {
                                from: me,
                                cells: removed,
                            },
                        ));
                    }
                    *cur = want;
                }
            }
            for (peer, msg) in sends {
                self.cluster.as_ref().expect("cluster").mesh.send(peer, msg);
            }
        }

        // --- Player territory publishing: a local player standing in a
        // foreign cell is announced to that cell's owner (the only node
        // whose sessions can possibly see the player); back home, the
        // foreign owner is told to retract.
        let player_cells: Vec<(GobId, usize)> = self
            .world
            .players
            .iter()
            .filter_map(|p| {
                let slot = self.world.gobs.get(p.gob)?;
                let cell = crate::visidx::cell_of(
                    self.world.gobs.pos[slot].0,
                    self.world.gobs.pos[slot].1,
                );
                Some((p.gob, self.cell_owner(cell)))
            })
            .collect();
        for (pgob, owner) in player_cells {
            let abroad = self
                .cluster
                .as_ref()
                .expect("cluster")
                .player_abroad
                .clone();
            let prev = abroad.get(&pgob).copied();
            if Some(owner) == prev {
                continue;
            }
            if owner == me {
                // Back on home ground: retract from the previous owner.
                if let Some(old) = prev {
                    self.cluster
                        .as_ref()
                        .expect("cluster")
                        .mesh
                        .send(old, NodeMsg::GuestRetract { id: pgob });
                    self.cluster
                        .as_mut()
                        .expect("cluster")
                        .player_abroad
                        .remove(&pgob);
                }
            } else {
                // Retract from the OLD owner if the player switched foreign
                // cells owned by different nodes, then announce to the new.
                if let Some(old) = prev.filter(|&o| o != owner) {
                    self.cluster
                        .as_ref()
                        .expect("cluster")
                        .mesh
                        .send(old, NodeMsg::GuestRetract { id: pgob });
                }
                self.cluster
                    .as_mut()
                    .expect("cluster")
                    .player_abroad
                    .insert(pgob, owner);
                self.publish(pgob, GuestEv::Announce);
            }
        }

        // --- Animal authority transfer: an animal standing in a foreign
        // cell moves to its cell's owner (full state, SAME id), and the
        // local copy demotes to a guest so local viewers never flicker.
        let animal_ids = self.world.animal_gobs.clone();
        for id in animal_ids {
            let Some(slot) = self.world.gobs.get(id) else {
                continue;
            };
            let cell =
                crate::visidx::cell_of(self.world.gobs.pos[slot].0, self.world.gobs.pos[slot].1);
            let owner = self.cell_owner(cell);
            if owner == me {
                continue;
            }
            let Some(st) = self.guest_state_from_slot(id, slot) else {
                continue;
            };
            self.cluster
                .as_ref()
                .expect("cluster")
                .mesh
                .send(owner, NodeMsg::GuestTransfer(st.clone()));
            // Demote: copy into the guest table, drop from every sim table,
            // kill the gob row, and re-index the id as a guest. The wire
            // frame counter carries over so emitted finalizers stay ahead
            // of what viewers already applied.
            let frame = self.world.gobs.frame[slot];
            let res_idx = self.world.gobs.res_idx[slot];
            let mv = st.mv.map(|g| LinMove {
                sx: g.sx,
                sy: g.sy,
                tx: g.tx,
                ty: g.ty,
                steps: g.steps,
                step: g.step,
                started_ms: g.started_ms,
                total_ms: g.total_ms,
            });
            let cell = crate::visidx::cell_of(st.pos.0, st.pos.1);
            self.world.animal_fights.remove(&id);
            self.world.guests.insert(
                id,
                crate::state::GuestGob {
                    pos: st.pos,
                    mv,
                    frame,
                    moving: st.moving,
                    facing: st.facing,
                    kind: st.kind,
                    res_idx,
                    hp: st.hp,
                    max_hp: st.max_hp,
                    cell,
                    territory: false,
                    last_seen_tick: self.world.tick,
                },
            );
            self.world.gobs.kill(id);
            self.world.gobs.vis.insert(id, st.pos);
            self.world.animal_gobs.retain(|&a| a != id);
            // Tame rows never outlive local authority (the follow render
            // and AI skip are node-local; cross-node leashes are an open
            // MVP limitation recorded in the docs).
            if self.world.tamed.remove(&id).is_some() {
                self.stream_follow_off(id);
            }
            tracing::debug!(id, owner, "animal authority transferred");
        }

        // --- Drop authority transfer (session 35): a drop spawned by
        // THIS node onto a cell it does not own (a station output drop
        // whose spawn jitter crossed the cell boundary, stone rubble,
        // loot) is invisible to every player homed on the owner - peers
        // only subscribe to OUR cells, never their own. Mirror the
        // animal path: hand the full drop state (GuestTransfer with the
        // DropView payload) to the cell's owner and demote the local
        // copy to a guest. The owner claims it via promote_transfer and
        // publishes it back to everyone subscribed to that cell - so
        // both sides' players see and can pick up the same drop, with
        // ONE authority deciding the pickup race.
        let mut foreign_drops: Vec<GobId> = Vec::new();
        for slot in 0..self.world.gobs.kind.len() {
            if !self.world.gobs.alive[slot]
                || !matches!(self.world.gobs.kind[slot], Kind::Drop { .. })
            {
                continue;
            }
            let id = crate::state::gob_id_from_slot(slot, self.world.gobs.gen[slot]);
            let cell =
                crate::visidx::cell_of(self.world.gobs.pos[slot].0, self.world.gobs.pos[slot].1);
            if self.cell_owner(cell) != me {
                foreign_drops.push(id);
            }
        }
        for id in foreign_drops {
            let Some(slot) = self.world.gobs.get(id) else {
                continue;
            };
            let Some(st) = self.guest_state_from_slot(id, slot) else {
                continue;
            };
            let owner = crate::grid_owner::owner_of(
                crate::visidx::cell_of(st.pos.0, st.pos.1),
                self.cluster.as_ref().expect("cluster").nodes,
            );
            // The drop is static: no mv, no walking pose, one hit point.
            let frame = self.world.gobs.frame[slot];
            let res_idx = self.world.gobs.res_idx[slot];
            let cell = crate::visidx::cell_of(st.pos.0, st.pos.1);
            self.cluster
                .as_ref()
                .expect("cluster")
                .mesh
                .send(owner, crate::nodes::NodeMsg::GuestTransfer(st.clone()));
            self.world.guests.insert(
                id,
                crate::state::GuestGob {
                    pos: st.pos,
                    mv: None,
                    frame,
                    moving: false,
                    facing: st.facing,
                    kind: st.kind,
                    res_idx,
                    hp: 1,
                    max_hp: 1,
                    cell,
                    territory: false,
                    last_seen_tick: self.world.tick,
                },
            );
            self.world.gobs.kill(id);
            self.world.gobs.vis.insert(id, st.pos);
            tracing::debug!(id, owner, "drop authority transferred");
        }

        // --- Guest GC (every 50 ticks): a guest nobody renders and
        // nobody subscribes can never come back on its own (its owner
        // only streams to subscribed cells) — retract and drop it.
        if self.world.tick.is_multiple_of(50) {
            let subscribed: HashSet<(i32, i32)> = self
                .cluster
                .as_ref()
                .expect("cluster")
                .my_subs
                .values()
                .flatten()
                .copied()
                .collect();
            let any_visible =
                |g: &Self, id: GobId| g.sessions.values().any(|o| o.visible.contains(&id));
            let stale: Vec<GobId> = self
                .world
                .guests
                .iter()
                .filter(|(id, g)| {
                    !g.territory && !subscribed.contains(&g.cell) && !any_visible(self, **id)
                })
                .map(|(id, _)| *id)
                .collect();
            for id in stale {
                self.remove_guest(id);
            }
        }
    }

    /// Union of the view cells of all local sessions (subscription basis).
    fn wanted_view_cells(&self) -> HashSet<(i32, i32)> {
        let mut out = HashSet::new();
        for out_session in self.sessions.values() {
            let Some(pg) = out_session
                .player_gob
                .and_then(|id| self.world.gobs.get(id))
            else {
                continue;
            };
            let (px, py) = self.world.gobs.pos[pg];
            let span = VIEW_RADIUS;
            let (cx0, cx1) = (
                (px - span).div_euclid(crate::visidx::CELL),
                (px + span).div_euclid(crate::visidx::CELL),
            );
            let (cy0, cy1) = (
                (py - span).div_euclid(crate::visidx::CELL),
                (py + span).div_euclid(crate::visidx::CELL),
            );
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    out.insert((cx, cy));
                }
            }
        }
        out
    }

    /// Ingest a foreign-authority gob (announce or update): store/replace
    /// the guest row, keep the dirty-cell index honest so the vis scan
    /// spawns/retracts it for local sessions, and emit wire finalizers to
    /// sessions already rendering it when the movement state changes.
    fn ingest_guest(&mut self, st: crate::nodes::GuestState) {
        use crate::nodes::GuestKind;
        let cell = crate::visidx::cell_of(st.pos.0, st.pos.1);
        let (moving, facing, pos, mv) = (st.moving, st.facing, st.pos, st.mv);
        let kind = st.kind.clone();
        let id = st.id;
        let existed = self.world.guests.contains_key(&id);
        // Resolve render resource + inventory of layers locally.
        let res_idx = match &kind {
            GuestKind::Animal { species } => {
                let sp = match crate::state::Species::from_index(*species) {
                    Some(sp) => sp,
                    None => {
                        tracing::warn!(id, species, "guest animal species out of range");
                        return;
                    }
                };
                self.world.res.intern(sp.resname())
            }
            GuestKind::Player { .. } => self.world.res.intern("gfx/borka/body"),
            GuestKind::Static { res_name, .. } => {
                self.world.res.intern(leak_static(res_name.as_str()))
            }
        };
        let mv_lin = mv.map(|g| LinMove {
            sx: g.sx,
            sy: g.sy,
            tx: g.tx,
            ty: g.ty,
            steps: g.steps,
            step: g.step,
            started_ms: g.started_ms,
            total_ms: g.total_ms,
        });
        // Determine which local sessions already render this gob and what
        // changed, BEFORE mutating (wire finalizers mirror local movement:
        // LINBEG on new move, OD_MOVE on finish, OD_LAYERS on pose flip).
        let viewers: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&id))
            .map(|(s, _)| *s)
            .collect();
        let (pose_flipped, move_changed) = match self.world.guests.get(&id) {
            Some(old) => (
                old.moving != moving || old.facing != facing,
                old.mv.is_some() != mv_lin.is_some(),
            ),
            None => (false, false),
        };
        // HP change on an EXISTING guest (relay fight damage landed on the
        // owner): stream OD_HEALTH so local viewers see the health bar
        // move without waiting for the owner's own broadcast (which only
        // covers ITS local sessions).
        let hp_changed = self
            .world
            .guests
            .get(&id)
            .map(|g| g.hp != st.hp)
            .unwrap_or(false);
        // Kind payload flip (session 34): Structure -> Station on plan
        // completion, a construction stage advance, a station's lit byte,
        // a crop's stage. The row is replaced wholesale below, so compare
        // the OLD row against the incoming kind BEFORE the insert. A flip
        // re-renders every viewer's sprite (OD_RES + fresh sdt byte) -
        // the wire mirror of the owner's local restage_gob path. Before
        // session 34 the existing-guest path only streamed pose, move and
        // hp deltas, so a guest oven's lit byte and a guest plan's stage
        // NEVER re-rendered for players already watching the gob.
        let kind_changed = self
            .world
            .guests
            .get(&id)
            .map(|g| g.kind != kind)
            .unwrap_or(false);
        self.world.guests.insert(
            id,
            crate::state::GuestGob {
                pos,
                mv: mv_lin,
                frame: self.world.guests.get(&id).map(|g| g.frame).unwrap_or(0),
                moving,
                facing,
                kind,
                res_idx,
                hp: st.hp,
                max_hp: st.max_hp,
                cell,
                territory: false,
                last_seen_tick: self.world.tick,
            },
        );
        if !existed {
            // New guest: dirty-cell insert so the vis scan spawns it.
            self.world.gobs.vis.insert(id, pos);
            self.world.perf.guest_ingests += 1;
            tracing::debug!(id, ?moving, "guest ingested");
            return;
        }
        // Existing guest: reposition the index (both cells dirty) and
        // stream the same finalizers the owner's viewers got.
        self.world.gobs.vis.reposition(id, pos);
        if pose_flipped || move_changed {
            self.world.guests.get_mut(&id).expect("just inserted").frame += 1;
            let frame = self.world.guests.get(&id).expect("just inserted").frame;
            let mut m = MessageBuf::new();
            m.uint8(MSG_OBJDATA).uint8(0).int32(id).int32(frame as i32);
            match &mv_lin {
                Some(lm) => {
                    m.uint8(OD_LINBEG)
                        .coord(lm.sx, lm.sy)
                        .coord(lm.tx, lm.ty)
                        .int32(lm.steps);
                }
                None => {
                    m.uint8(OD_MOVE).coord(pos.0, pos.1);
                }
            }
            m.uint8(OD_LINSTEP)
                .int32(mv_lin.map(|g| g.steps).unwrap_or(0));
            m.uint8(OD_END);
            let block = m.finish();
            for sid in &viewers {
                if let Some(out) = self.sessions.get_mut(sid) {
                    out.send_raw(block.clone());
                    Self::record_unacked(out, id, frame, block.clone());
                }
            }
            // Pose flip streams the new layer set (same server-side pose
            // resolution as local movers), batched into one datagram per
            // viewer session.
            if pose_flipped {
                self.stream_guest_poses_batched(viewers.iter().map(|sid| (*sid, id)).collect());
            }
        }
        if hp_changed {
            let g = self.world.guests.get_mut(&id).expect("existing guest");
            g.frame += 1;
            let frame = g.frame;
            let quarters = ((g.hp * 4) / g.max_hp.max(1)).clamp(0, 4) as u8;
            let mut m = MessageBuf::new();
            m.uint8(MSG_OBJDATA)
                .uint8(0)
                .int32(id)
                .int32(frame as i32)
                .uint8(OD_HEALTH)
                .uint8(quarters)
                .uint8(OD_END);
            let block = m.finish();
            for sid in &viewers {
                if let Some(out) = self.sessions.get_mut(sid) {
                    out.send_raw(block.clone());
                    Self::record_unacked(out, id, frame, block.clone());
                }
            }
        }
        if kind_changed {
            // Full-block re-render (OD_RES with the fresh sdt byte, pose,
            // layers, health): the same wire shape as a fresh guest
            // spawn, so OCache rebuilds the sprite exactly like the
            // owner's local restage_gob path does for its own viewers.
            // Every viewer here is by construction already rendering the
            // gob (the visible set was snapshotted above).
            if let Some(g) = self.world.guests.get_mut(&id) {
                g.frame += 1;
            }
            let frame = self.world.guests.get(&id).map(|g| g.frame).unwrap_or(0);
            for sid in &viewers {
                if let Some(block) = self.encode_guest_block(*sid, id, true) {
                    if let Some(out) = self.sessions.get_mut(sid) {
                        out.send_raw(block.clone());
                        Self::record_unacked(out, id, frame, block);
                    }
                }
            }
        }
    }

    /// Remove a guest entirely (owner retract or GC): drop the row, clean
    /// the vis index, and retract it from every session rendering it.
    fn remove_guest(&mut self, id: GobId) {
        if self.world.guests.remove(&id).is_none() {
            return;
        }
        self.world.gobs.vis.remove(id);
        let sids: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&id))
            .map(|(s, _)| *s)
            .collect();
        for sid in sids {
            self.stream_retract(sid, id);
        }
        // Relay bookkeeping (session 28): a retracted ANIMAL closes the
        // fight of any local player engaged with it (its authoritative HP
        // is gone — death retract or out-of-cell GC); a retracted PLAYER
        // guest drops its relay rows so the animal stops retaliating.
        if self.world.guest_fights.remove(&id).is_some() {
            for pidx in 0..self.world.players.len() {
                if self.world.players[pidx].fight_target == Some(id) {
                    let sid = self.world.players[pidx].session;
                    self.world.players[pidx].fight_target = None;
                    self.fight_del(sid, id);
                }
            }
        }
        if self.world.guest_attackers.remove(&id).is_some() {
            // `id` was an animal with a relay fight row here.
            self.world.animal_fights.remove(&id);
        }
        let attacking_mine: Vec<GobId> = self
            .world
            .guest_attackers
            .iter()
            .filter(|(_, &p)| p == id)
            .map(|(&a, _)| a)
            .collect();
        for a in attacking_mine {
            self.world.guest_attackers.remove(&a);
            self.world.animal_fights.remove(&a);
        }
    }

    /// Authority handoff inbound: materialize the transferred gob under
    /// its EXACT id and take over simulation.
    fn promote_transfer(&mut self, st: crate::nodes::GuestState) {
        use crate::nodes::GuestKind;
        let crate::nodes::GuestState {
            id,
            pos,
            mv,
            moving,
            facing,
            kind,
            hp,
            max_hp,
            speed,
        } = st;
        // Only animals and drops transfer (players stay homed; trees and
        // stones never spawn on a foreign cell - the world generator
        // places statics inside their own cell). Anything else arriving
        // here is a peer bug — reject rather than corrupt local tables.
        let (spawn_kind, res_idx) = match kind {
            GuestKind::Animal { species } => match crate::state::Species::from_index(species) {
                Some(sp) => (
                    Kind::Animal { species: sp },
                    self.world.res.intern(sp.resname()),
                ),
                None => {
                    tracing::warn!(id, species, "transfer species out of range");
                    return;
                }
            },
            // Session 35 drop transfer: the cell's owner claims a drop
            // spawned by a peer (station output jitter across the cell
            // boundary, stone rubble, loot). The world render shape is
            // re-derived from the inventory resource name with the SAME
            // deterministic function the spawner used (drop_world_res),
            // so both nodes agree on the sprite without carrying it.
            GuestKind::Static {
                class: crate::nodes::StaticClass::Drop,
                drop: Some(view),
                ..
            } => {
                let inv_res_idx = self.world.res.intern(leak_static(&view.inv_res));
                let world_res = drop_world_res(&view.inv_res);
                let res_idx = self.world.res.intern(world_res);
                let label = leak_static(&view.label);
                (
                    Kind::Drop {
                        resname_idx: res_idx,
                        inv_res_idx,
                        ql: view.ql,
                        label,
                    },
                    res_idx,
                )
            }
            other => {
                tracing::warn!(?other, id, "transfer of a non-transferable guest rejected");
                return;
            }
        };
        let was_guest = self.world.guests.remove(&id).is_some();
        self.world
            .gobs
            .spawn_with_id(id, spawn_kind, pos, res_idx, Vitals { hp, max_hp, speed });
        if let Some(slot) = self.world.gobs.get(id) {
            self.world.gobs.facing[slot] = facing;
            self.world.gobs.pose_streamed[slot] = if moving { 8 + facing } else { facing };
            if let Some(g) = mv {
                self.world.gobs.mv[slot] = Some(LinMove {
                    sx: g.sx,
                    sy: g.sy,
                    tx: g.tx,
                    ty: g.ty,
                    steps: g.steps,
                    step: g.step,
                    started_ms: g.started_ms,
                    total_ms: g.total_ms,
                });
            }
        }
        match spawn_kind {
            Kind::Animal { .. } => {
                if !self.world.animal_gobs.contains(&id) {
                    self.world.animal_gobs.push(id);
                }
                tracing::debug!(id, "animal authority claimed");
            }
            Kind::Drop { .. } => tracing::debug!(id, "drop authority claimed"),
            _ => unreachable!("the match above only yields Animal or Drop"),
        }
        let _ = was_guest;
        // My subscribers may already render this gob (border viewers):
        // announce so their sessions re-acquire it if it left their view
        // while it was a guest elsewhere.
        self.publish(id, GuestEv::Announce);
    }

    /// Area chat from a remote node: same radius filter, but against the
    /// SENDER's position carried on the message.
    fn deliver_remote_chat(&mut self, from: &str, at: (i32, i32), text: &str) {
        let line = format!("{from}: {text}");
        let recipients: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, out)| {
                if out.chat_wid == 0 {
                    return false;
                }
                let Some(gob) = out.player_gob else {
                    return false;
                };
                let Some(slot) = self.world.gobs.get(gob) else {
                    return false;
                };
                crate::chat::within_radius(
                    self.world.gobs.pos[slot],
                    at,
                    crate::chat::AREA_CHAT_RADIUS,
                )
            })
            .map(|(s, _)| *s)
            .collect();
        for r in recipients {
            self.chat_line(r, &line, None);
        }
    }

    /// Guest movement progress: identical timing model to local movers
    /// (the owner authored the linmove params; progress math is pure), so
    /// no per-tick streaming from the owner is needed — each subscriber
    /// derives LINSTEP locally for its viewing sessions.
    ///
    /// Session 34 batch shape: blocks are encoded once per moving guest,
    /// then ONE pass over the sessions merges every visible block into
    /// ONE datagram per session. Session 41 packs the blocks into one
    /// shared buffer indexed by vis cell (`move_batch`): the per-session
    /// fan-out iterates only the non-empty cells (a few dozen for a
    /// moving crowd) and rejects whole cells with one rectangle test —
    /// the O(sessions x movers) hash-probe fan-out this replaces was the
    /// dominant guests-phase cost in the 2x300 duel-cohort cluster
    /// (p95 7.9 ms at 200 sessions, linear in the session count).
    /// LINSTEP progress frames are deliberately NOT recorded in
    /// `unacked`: each frame is superseded next tick, so a lost datagram
    /// self-heals within 100 ms.
    fn tick_guests(&mut self) {
        if self.world.guests.is_empty() {
            return;
        }
        let now = self.world.now_ms;
        let encode_t = Instant::now();
        let ids: Vec<GobId> = self.world.guests.keys().copied().collect();
        let mut batch = std::mem::take(&mut self.move_scratch);
        batch.clear();
        let mut finished_ids: Vec<GobId> = Vec::new();
        // Progress outcome for one guest this tick.
        enum GuestMove {
            /// Move finished this tick (finalizer block).
            Finish,
            /// Still moving, LINSTEP counter advanced to `i32`.
            Step(i32),
            /// Still moving within the same LINSTEP index: no block.
            Quiet,
        }
        for id in ids {
            let (mv, pos, frame) = {
                let Some(g) = self.world.guests.get_mut(&id) else {
                    continue;
                };
                let Some(lm) = g.mv else {
                    continue;
                };
                g.frame += 1;
                let frame = g.frame;
                let elapsed = now.saturating_sub(lm.started_ms);
                if elapsed >= u64::from(lm.total_ms) {
                    (GuestMove::Finish, (lm.tx, lm.ty), frame)
                } else {
                    let (cx, cy) = lm.pos_at(now);
                    let l = lm.step_at(now);
                    let advanced = l > lm.step;
                    g.mv = Some(LinMove { step: l, ..lm });
                    // Progress frames ship on the LINSTEP_EVERY_TICKS
                    // cadence only; the server-side counter still advances
                    // every tick.
                    let ship = self.world.tick.is_multiple_of(LINSTEP_EVERY_TICKS);
                    (
                        if advanced && ship {
                            GuestMove::Step(l)
                        } else {
                            GuestMove::Quiet
                        },
                        (cx, cy),
                        frame,
                    )
                }
            };
            // Apply: position + dirty-cell index.
            {
                let g = self.world.guests.get_mut(&id).expect("checked above");
                g.pos = pos;
            }
            self.world.gobs.vis.reposition(id, pos);
            match mv {
                GuestMove::Finish => {
                    let g = self.world.guests.get_mut(&id).expect("checked above");
                    g.moving = false;
                    // Headerless block (see broadcast_batch).
                    let mut m = MessageBuf::new();
                    m.uint8(0)
                        .int32(id)
                        .int32(frame as i32)
                        .uint8(OD_MOVE)
                        .coord(pos.0, pos.1)
                        .uint8(OD_LINSTEP)
                        .int32(0)
                        .uint8(OD_END);
                    batch.push(
                        id,
                        frame,
                        crate::visidx::cell_of(pos.0, pos.1),
                        true,
                        &m.finish(),
                    );
                    finished_ids.push(id);
                }
                GuestMove::Step(l) => {
                    let mut m = MessageBuf::new();
                    m.uint8(0)
                        .int32(id)
                        .int32(frame as i32)
                        .uint8(OD_LINSTEP)
                        .int32(l)
                        .uint8(OD_END);
                    batch.push(
                        id,
                        frame,
                        crate::visidx::cell_of(pos.0, pos.1),
                        false,
                        &m.finish(),
                    );
                }
                GuestMove::Quiet => {}
            }
        }
        self.world.perf.guests_encode_us = encode_t.elapsed().as_micros() as u64;
        // Fan-out: each session merges its visible blocks into one
        // datagram; finalizers also land in `unacked` (retransmittable).
        let fanout_t = Instant::now();
        self.broadcast_batch(&batch);
        self.move_scratch = batch;
        self.world.perf.guests_fanout_us = fanout_t.elapsed().as_micros() as u64;
        // Rest pose for finished movers: the standing layer block per
        // viewer, batched into one datagram per session (rare - only on
        // movement finalization; statics skip).
        let pose_t = Instant::now();
        let mut pose_jobs: Vec<(SessionId, GobId)> = Vec::new();
        for id in finished_ids {
            for (sid, out) in &self.sessions {
                if out.visible.contains(&id) {
                    pose_jobs.push((*sid, id));
                }
            }
        }
        self.stream_guest_poses_batched(pose_jobs);
        self.world.perf.guests_pose_us = pose_t.elapsed().as_micros() as u64;
    }

    /// Spawn block for a guest (mirrors `encode_gob_block`'s player/animal
    /// branches reading the GuestGob row instead of the SoA columns).
    fn encode_guest_block(&mut self, sid: SessionId, id: GobId, restage: bool) -> Option<Vec<u8>> {
        use crate::nodes::GuestKind;
        let g = self.world.guests.get(&id)?.clone();
        let out = self.sessions.get_mut(&sid)?;
        // A restage block re-renders a gob the session ALREADY sees (kind
        // flip / sdt byte change); a fresh spawn only fires once.
        if !restage && !out.visible.insert(id) {
            return None;
        }
        let mut m = MessageBuf::new();
        m.uint8(MSG_OBJDATA)
            .uint8(0)
            .int32(id)
            .int32(g.frame as i32);
        if let GuestKind::Static {
            res_name,
            crop,
            station,
            stage,
            ..
        } = &g.kind
        {
            let name = leak_static(self.world.res.name(g.res_idx).unwrap_or(res_name.as_str()));
            let w = out.res.wire_named(g.res_idx, name);
            // Crops carry their growth stage as the sprite sdt byte -
            // the same wire shape the local path emits for plants
            // (wire id | 0x8000, then len + bytes; OCache rebuilds the
            // sprite on a stage change). Stations (session 33) carry
            // their lit byte the same way, so a lit oven re-renders on
            // every re-published GuestUpdate without a new OD kind.
            // Construction plans (session 34) carry their build stage
            // the same way, so a peer watching a build sees the same
            // stage sprite the local restage path emits.
            if let Some(view) = station {
                m.uint8(OD_RES).uint16(w | 0x8000);
                m.uint8(1).uint8(view.lit as u8);
            } else {
                match (crop, stage) {
                    (Some((_spec, cstage)), _) => {
                        m.uint8(OD_RES).uint16(w | 0x8000);
                        m.uint8(1).uint8(*cstage);
                    }
                    (None, Some(pstage)) => {
                        m.uint8(OD_RES).uint16(w | 0x8000);
                        m.uint8(1).uint8(*pstage);
                    }
                    (None, None) => {
                        m.uint8(OD_RES).uint16(w);
                    }
                }
            }
        }
        match &g.mv {
            Some(lm) => {
                m.uint8(OD_LINBEG)
                    .coord(lm.sx, lm.sy)
                    .coord(lm.tx, lm.ty)
                    .int32(lm.steps);
                m.uint8(OD_LINSTEP).int32(lm.step);
            }
            None => {
                m.uint8(OD_MOVE).coord(g.pos.0, g.pos.1);
            }
        }
        // Composited drawables (players + animals only): server-side
        // pose resolution, the mirror of the local encode_gob_block
        // branch. Statics render from OD_RES alone and carry NO
        // OD_LAYERS - the local path never writes one for them, and a
        // bare 0xFFFF terminator without a base u16 breaks every
        // strict OD sequence parser (the session-34 probe caught the
        // test-build client crashing on guest static blocks).
        match &g.kind {
            GuestKind::Player { name, equip } => {
                m.uint8(OD_LAYERS);
                let base = "gfx/borka/body";
                let bi = self.world.res.intern(base);
                m.uint16(out.res.wire_named(bi, base));
                let equip_static: Vec<&'static str> =
                    equip.iter().map(|s| leak_static(s)).collect();
                for part in avatar_pose_layers(g.moving, g.facing) {
                    let gi = self.world.res.intern(part);
                    m.uint16(out.res.wire_named(gi, part));
                }
                for part in crate::equip::world_layers(&equip_static, g.moving, g.facing) {
                    let gi = self.world.res.intern(part);
                    m.uint16(out.res.wire_named(gi, part));
                }
                m.uint16(65535);
                // Non-own viewer: standing doll set (the own viewer's gob
                // is always local-homed, never a guest).
                let doll: Vec<&'static str> = avatar_pose_layers(false, g.facing)
                    .iter()
                    .copied()
                    .chain(
                        crate::equip::world_layers(&equip_static, false, g.facing)
                            .iter()
                            .copied(),
                    )
                    .collect();
                m.uint8(OD_AVATAR);
                for part in doll {
                    let gi = self.world.res.intern(part);
                    m.uint16(out.res.wire_named(gi, part));
                }
                m.uint16(65535);
                m.uint8(OD_BUDDY).string(name).uint8(0).uint8(0);
            }
            GuestKind::Animal { species } => {
                m.uint8(OD_LAYERS);
                let sp = crate::state::Species::from_index(*species)?;
                let base = kritter_base(sp);
                let bi = self.world.res.intern(base);
                m.uint16(out.res.wire_named(bi, base));
                let part = kritter_pose_layer(sp, g.moving, g.facing);
                let gi = self.world.res.intern(part);
                m.uint16(out.res.wire_named(gi, part));
                m.uint16(65535);
            }
            GuestKind::Static { .. } => {
                // Statics render from OD_RES alone; no OD_LAYERS (see
                // the comment above the match).
            }
        }
        let quarters = ((g.hp * 4) / g.max_hp.max(1)).clamp(0, 4) as u8;
        m.uint8(OD_HEALTH).uint8(quarters);
        m.uint8(OD_END);
        Some(m.finish())
    }

    /// Stream a guest spawn to one session: RESID announcements first
    /// (mirror stream_spawn's announce logic), then the encoded block.
    fn stream_guest_spawn(&mut self, sid: SessionId, id: GobId) {
        use crate::nodes::GuestKind;
        let Some(g) = self.world.guests.get(&id).cloned() else {
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        // Announce the render resource(s) this session has not seen yet.
        let mut layers: Vec<&'static str> = Vec::new();
        match &g.kind {
            GuestKind::Animal { species } => {
                if let Some(sp) = crate::state::Species::from_index(*species) {
                    layers.push(kritter_base(sp));
                    layers.push(kritter_pose_layer(sp, g.moving, g.facing));
                }
            }
            GuestKind::Player { equip, .. } => {
                let equip_static: Vec<&'static str> =
                    equip.iter().map(|s| leak_static(s)).collect();
                layers.push("gfx/borka/body");
                layers.extend(avatar_pose_layers(g.moving, g.facing).iter().copied());
                layers.extend(crate::equip::world_layers(
                    &equip_static,
                    g.moving,
                    g.facing,
                ));
                layers.extend(avatar_doll_layers().iter().copied());
                layers.extend(crate::equip::doll_layers(&equip_static));
            }
            GuestKind::Static { .. } => {}
        }
        let static_res = matches!(g.kind, GuestKind::Static { .. });
        for layer_name in layers {
            let gi = self.world.res.intern(layer_name);
            let w = out.res.wire_named(gi, layer_name);
            if let Some((name, ver)) = out.res.pending_announce(w) {
                let msg = wdg::resid(w, name, ver);
                out.send(msg);
                out.res.mark_announced(w);
            }
        }
        if static_res {
            let name = self
                .world
                .res
                .name(g.res_idx)
                .unwrap_or("gfx/terobjs/items/branch");
            let w = out.res.wire_named(g.res_idx, name);
            if let Some((rname, ver)) = out.res.pending_announce(w) {
                let msg = wdg::resid(w, rname, ver);
                out.send(msg);
                out.res.mark_announced(w);
            }
        }
        if let Some(block) = self.encode_guest_block(sid, id, false) {
            let frame = self.world.guests.get(&id).map(|g| g.frame).unwrap_or(0);
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.send_raw(block.clone());
                Self::record_unacked(out, id, frame, block);
            }
        }
    }

    /// Stream the rest-pose blocks for finished guest movers, batched:
    /// ONE OBJDATA datagram per session carries ALL of that session's
    /// finished guests' layer blocks. The duel-cohort crowd finalizes
    /// dozens of guests per tick with ~300 viewers each - the per-(guest,
    /// viewer) datagram and the per-call GuestGob clone were the dominant
    /// pose-phase cost (p50 11 ms at 300 sessions). Guest rows are read
    /// in place (no clone); wire ids resolve through the session table.
    fn stream_guest_poses_batched(&mut self, mut jobs: Vec<(SessionId, GobId)>) {
        use crate::nodes::GuestKind;
        if jobs.is_empty() {
            return;
        }
        jobs.sort_unstable();
        let mut i = 0;
        while i < jobs.len() {
            let sid = jobs[i].0;
            let mut j = i;
            while j < jobs.len() && jobs[j].0 == sid {
                j += 1;
            }
            let Some(out) = self.sessions.get_mut(&sid) else {
                i = j;
                continue;
            };
            // Datagram materializes lazily: sessions whose guests all
            // vanished allocate nothing.
            let mut m: Option<MessageBuf> = None;
            for (_, id) in &jobs[i..j] {
                let Some(g) = self.world.guests.get(id) else {
                    continue;
                };
                // Statics have no pose (OD_RES alone renders them): an
                // empty layer list would carry the same bare-0xFFFF defect
                // the session-34 probe caught in the spawn block.
                if matches!(g.kind, GuestKind::Static { .. }) {
                    continue;
                }
                let frame = g.frame;
                let moving = g.moving;
                let facing = g.facing;
                let kind = &g.kind;
                let mm = m.get_or_insert_with(|| MessageBuf::with_capacity(256));
                mm.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(*id)
                    .int32(frame as i32)
                    .uint8(OD_LAYERS);
                match kind {
                    GuestKind::Player { equip, .. } => {
                        let base = "gfx/borka/body";
                        let bi = self.world.res.intern(base);
                        mm.uint16(out.res.wire_named(bi, base));
                        for part in avatar_pose_layers(moving, facing) {
                            let gi = self.world.res.intern(part);
                            mm.uint16(out.res.wire_named(gi, part));
                        }
                        // equip names are leaked already; no GuestGob clone.
                        let equip_static: Vec<&'static str> =
                            equip.iter().map(|s| leak_static(s)).collect();
                        for part in crate::equip::world_layers(&equip_static, moving, facing) {
                            let gi = self.world.res.intern(part);
                            mm.uint16(out.res.wire_named(gi, part));
                        }
                        mm.uint16(65535);
                    }
                    GuestKind::Animal { species } => {
                        let Some(sp) = crate::state::Species::from_index(*species) else {
                            continue;
                        };
                        let base = kritter_base(sp);
                        let bi = self.world.res.intern(base);
                        mm.uint16(out.res.wire_named(bi, base));
                        let part = kritter_pose_layer(sp, moving, facing);
                        let gi = self.world.res.intern(part);
                        mm.uint16(out.res.wire_named(gi, part));
                        mm.uint16(65535);
                    }
                    GuestKind::Static { .. } => {}
                }
                mm.uint8(OD_END);
            }
            if let Some(m) = m {
                out.send_raw(m.finish());
            }
            i = j;
        }
    }

    // Player commands
    // ------------------------------------------------------------------

    fn on_map_click(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
        trace!(sid, nargs = args.len(), "map click received");
        // click(c0, mc, button, modflags[, gobid, gobrc]): c0 is a screen
        // coordinate; only mc (second coord) is the world-space target.
        let mc = args.iter().filter_map(|a| a.as_coord()).nth(1);
        // Ints in order: button, modflags[, gobid]; coords are filtered out.
        let mut ints = args.iter().filter_map(|a| a.as_int());
        let button = ints.next().unwrap_or(0);
        let _modflags = ints.next().unwrap_or(0);
        let gobid = args.get(4).and_then(|a| a.as_int());
        let Some((_x, y)) = mc else { return };
        let (mx, my) = mc.expect("BUG: mc checked above");
        let Some(player_gob) = self.sessions.get(&sid).and_then(|o| o.player_gob) else {
            return;
        };
        if button == 1 {
            // Armed Plow Field pagina takes precedence: plow instead of walk.
            let plow_armed = self
                .sessions
                .get(&sid)
                .map(|o| o.pending_plow)
                .unwrap_or(false);
            if plow_armed {
                if let Some(out) = self.sessions.get_mut(&sid) {
                    out.pending_plow = false;
                }
                self.plow_tile(sid, Self::tile_coord(mx, my));
                return;
            }
            if let Some(target) = gobid {
                self.player_interact(sid, player_gob, target, (mx, my));
            } else {
                self.player_walk(sid, player_gob, (mx, my));
            }
        }
        let _ = y;
    }

    fn player_walk(&mut self, sid: SessionId, player_gob: GobId, target: (i32, i32)) {
        // A ground click cancels any active aim (the player chose to
        // move instead of holding the draw).
        if let Some(p) = self.world.player_mut(sid) {
            p.aim = None;
        }
        let Some(slot) = self.world.gobs.get(player_gob) else {
            return;
        };
        // Shared movement entry point: retargets from the interpolated
        // position when already moving (no destination teleport on rapid
        // clicks), applies the gait speed and the terrain cap, and derives
        // the client-consistent step count. See start_move.
        if !self.start_move(slot, target) {
            trace!(sid, tx = target.0, ty = target.1, "walk refused");
        }
    }

    fn player_interact(
        &mut self,
        sid: SessionId,
        player_gob: GobId,
        target: GobId,
        _at: (i32, i32),
    ) {
        let Some(tslot) = self.world.gobs.get(target) else {
            // Cluster: foreign-authority gobs live in the guest table.
            // Animals there are attackable through the interaction relay
            // (the fight UI stays local; the bars/HP stay on the owner).
            if let Some(g) = self.world.guests.get(&target) {
                match &g.kind {
                    crate::nodes::GuestKind::Animal { species } => {
                        if let Some(sp) = crate::state::Species::from_index(*species) {
                            // Bow carriers take the ranged path against
                            // guests too (the shot relays to the owner).
                            if self.start_aim(sid, target) {
                                return;
                            }
                            self.start_fight(sid, target, sp);
                            return;
                        }
                    }
                    // Statics (session 30): the click routes to the target's
                    // authority through the relay; the act is picked from
                    // the STABLE class tag, the authority re-validates it
                    // against its own Kind.
                    crate::nodes::GuestKind::Static {
                        class,
                        crop,
                        station,
                        ..
                    } => {
                        // Crops (session 31): the harvest menu is session UI
                        // and lives on the HOME node - open it locally from
                        // the guest view's (spec, stage); the chosen act is
                        // relayed when the menu is acted on.
                        if let (crate::nodes::StaticClass::Crop, Some((spec, stage))) =
                            (class, *crop)
                        {
                            if (spec as usize) < farm::CROPS.len()
                                && stage >= farm::CROPS[spec as usize].early_stage
                            {
                                self.show_crop_menu(sid, target, spec, stage);
                            } else {
                                debug!(sid, stage, "guest crop not harvestable yet");
                            }
                            return;
                        }
                        // Stations (session 33): the Light/Extinguish menu is
                        // session UI too - open it locally from the
                        // piggybacked snapshot; the chosen act relays to the
                        // authority, which re-validates against its own state.
                        if *class == crate::nodes::StaticClass::Station {
                            if let Some(view) = station {
                                self.show_station_menu(sid, target, view.lit);
                            } else {
                                debug!(sid, "guest station without a snapshot");
                            }
                            return;
                        }
                        let act = match class {
                            crate::nodes::StaticClass::Drop => {
                                Some(crate::nodes::StaticAct::Pickup)
                            }
                            crate::nodes::StaticClass::Tree => Some(crate::nodes::StaticAct::Chop),
                            crate::nodes::StaticClass::Stone => Some(crate::nodes::StaticAct::Mine),
                            crate::nodes::StaticClass::Crop
                            | crate::nodes::StaticClass::Station
                            | crate::nodes::StaticClass::Structure => None,
                        };
                        if let Some(act) = act {
                            if let Some(c) = self.cluster.as_ref() {
                                let authority =
                                    self.cell_owner(crate::visidx::cell_of(g.pos.0, g.pos.1));
                                c.mesh.send(
                                    authority,
                                    crate::nodes::NodeMsg::RelayStaticAct {
                                        player: player_gob,
                                        target,
                                        act,
                                    },
                                );
                                debug!(sid, target, ?act, authority, "relay static act sent");
                            }
                            return;
                        }
                    }
                    // PvP archery (session 38): a bow carrier aims at a
                    // cross-node player; the hit roll stays here (aim is
                    // session state) and the damage relays to the VICTIM's
                    // home node, whose hurt_player path owns armor/HP/
                    // knockout. Melee carriers (session 39) get the Fight
                    // flower menu instead - the duel relays one PvpSwing
                    // per swing through the same authority split.
                    crate::nodes::GuestKind::Player { .. } => {
                        if self.start_aim(sid, target) {
                            return;
                        }
                        self.open_guest_fight_menu(sid, target);
                    }
                }
            }
            trace!(sid, target, "interact target gone");
            return;
        };
        trace!(sid, target, kind = ?self.world.gobs.kind[tslot], "player_interact");
        match self.world.gobs.kind[tslot] {
            Kind::Tree { harvests } => {
                if harvests > 0 {
                    self.world.gobs.kind[tslot] = Kind::Tree {
                        harvests: harvests - 1,
                    };
                    self.world.gobs.frame[tslot] += 1;
                    let pos = self.world.gobs.pos[tslot];
                    self.spawn_drop_near(pos, "gfx/invobjs/wood", 10, "");
                    if let Some(p) = self.world.player_mut(sid) {
                        p.lp += 5;
                    }
                    self.push_cattr(sid);
                    // Refresh the char sheet LP balance if it is open.
                    self.push_lp_msgs(sid);
                } else {
                    // Tree exhausted: remove and leave a stump.
                    let pos = self.world.gobs.pos[tslot];
                    self.world.gobs.kill(target);
                    let stump = self.world.res.intern("gfx/terobjs/trees/log");
                    let id = self.world.gobs.spawn(Kind::Stone, pos, stump, 1, 0);
                    self.broadcast_spawn(id);
                }
            }
            Kind::Stone => {
                let pos = self.world.gobs.pos[tslot];
                self.world.gobs.kill(target);
                self.spawn_drop_near(pos, "gfx/invobjs/stone", 10, "");
                if let Some(p) = self.world.player_mut(sid) {
                    p.lp += 3;
                }
                self.push_cattr(sid);
                // Refresh the char sheet LP balance if it is open.
                self.push_lp_msgs(sid);
            }
            Kind::Drop { .. } => {
                // Pick up: move into inventory. The stack carries the
                // INVENTORY resource (drop.0), not the gob's terobjs
                // render shape (see spawn_drop_near). grant_pickup
                // redirects onto a same-resource cursor stack and merges
                // into same-resource inventory stacks.
                if let Some(drop) = self.world.gobs.kind[tslot].drop_info() {
                    self.grant_pickup(
                        sid,
                        InvStack {
                            res: drop.0,
                            count: drop.1,
                            ql: drop.2,
                            label: drop.3,
                        },
                    );
                }
                self.world.gobs.kill(target);
                self.broadcast_retract(target);
            }
            Kind::Animal { species } => {
                // Fully tamed domestic producers open the collection menu
                // instead of the fight window (session 47): Milking a cow
                // / shearing a sheep are flower-menu interactions (docs
                // "Animal products and collection flows"). Mid-taming
                // beasts and non-producers keep the fight path.
                if self.open_animal_menu(sid, target, species) {
                    return;
                }
                // Bow-equipped players take the ranged path instead of
                // the fight window (archery.rs; the aim meter is the
                // accuracy meter of Legacy:Combat_Actions).
                if self.start_aim(sid, target) {
                    return;
                }
                self.start_fight(sid, target, species);
            }
            Kind::Crop { .. } => {
                self.open_crop_menu(sid, target);
            }
            Kind::Plan { spec, stage } => {
                // Feedback click on a construction plan: the remaining
                // demand as a chat line (the client has no plan UI).
                let buildable = &crate::build::BUILDABLES[spec as usize];
                let lines = buildable
                    .demand
                    .iter()
                    .filter_map(|(res, need)| {
                        let credited = self
                            .world
                            .plans
                            .get(&target)
                            .map(|p| {
                                p.credited
                                    .iter()
                                    .find(|c| c.res == *res)
                                    .map(|c| c.count)
                                    .unwrap_or(0)
                            })
                            .unwrap_or(0);
                        let left = need.saturating_sub(credited);
                        (left > 0).then(|| format!("{} x{}", res, left))
                    })
                    .collect::<Vec<_>>();
                let _ = stage;
                let msg = if lines.is_empty() {
                    format!("The {} is being built.", buildable.id)
                } else {
                    format!("The {} needs: {}", buildable.id, lines.join(", "))
                };
                self.system_line(sid, &msg);
            }
            Kind::Station { .. } => {
                self.open_station_menu(sid, target);
            }
            Kind::Structure { spec } => {
                let buildable = &crate::build::BUILDABLES[spec as usize];
                self.system_line(sid, &format!("A fine {} stands here.", buildable.id));
            }
            Kind::Player { .. } => {
                // PvP archery (session 38): a bow carrier takes the
                // ranged path against a player target (self-clicks are
                // refused inside start_aim and fall through to the
                // party menu, which also ignores them). Without a bow
                // the click stays the party-invite menu.
                if self.start_aim(sid, target) {
                    return;
                }
                self.open_party_invite_menu(sid, target);
            }
        }
    }

    // ------------------------------------------------------------------
    // Chat + party (docs/mechanics/network/communication.md)
    // ------------------------------------------------------------------

    /// Relay one area-chat line from a player to every session in radius.
    fn on_chat_msg(&mut self, sid: SessionId, raw: &str) {
        let Some(text) = crate::chat::sanitize(raw) else {
            return;
        };
        let (sender_pos, sender_name) = {
            let Some(pidx) = self.world.by_session.get(&sid).copied() else {
                return;
            };
            let p = &self.world.players[pidx];
            let Some(slot) = self.world.gobs.get(p.gob) else {
                return;
            };
            (self.world.gobs.pos[slot], p.name.clone())
        };
        let line = format!("{}: {}", sender_name, text);
        // Snapshot the recipient list before sending: the send path only
        // touches each session's outbound queue, but a disjoint snapshot
        // keeps the borrow checker happy without cloning sessions.
        let recipients: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, out)| {
                if out.chat_wid == 0 {
                    return false;
                }
                let Some(gob) = out.player_gob else {
                    return false;
                };
                let Some(slot) = self.world.gobs.get(gob) else {
                    return false;
                };
                // The sender's own distance is 0, so they hear their echo.
                crate::chat::within_radius(
                    self.world.gobs.pos[slot],
                    sender_pos,
                    crate::chat::AREA_CHAT_RADIUS,
                )
            })
            .map(|(s, _)| *s)
            .collect();
        for r in recipients {
            self.chat_line(r, &line, None);
        }
        // Cluster: relay the line to every peer (each node re-filters by
        // the sender's position for its own sessions).
        if self.is_cluster() {
            let c = self.cluster.as_ref().expect("cluster");
            c.mesh.broadcast_except(
                c.nodes.get(),
                c.me,
                crate::nodes::NodeMsg::Chat {
                    from: sender_name,
                    at: sender_pos,
                    text: text.to_string(),
                },
            );
        }
    }

    /// Push one "log" line to a session's Area Chat window. The chat
    /// uimsg arg list is (text[, color[, urgent]]); `None` renders the
    /// client default color.
    fn chat_line(&mut self, sid: SessionId, text: &str, color: Option<(u8, u8, u8)>) {
        let Some(out) = self.sessions.get(&sid) else {
            return;
        };
        let wid = out.chat_wid;
        if wid == 0 {
            return;
        }
        let mut args = vec![ListVal::S(text.to_owned())];
        if let Some((r, g, b)) = color {
            args.push(ListVal::Col(r, g, b, 255));
        }
        out.send(wdg::wdgmsg(wid, "log", &args));
    }

    /// Server-to-player notification via the Area Chat window (soft red).
    fn system_line(&mut self, sid: SessionId, text: &str) {
        let (r, g, b) = crate::chat::SYSTEM_COLOR;
        self.chat_line(sid, text, Some((r, g, b)));
    }

    /// Click on another player: open the clicker's invite flower menu.
    fn open_party_invite_menu(&mut self, sid: SessionId, target: GobId) {
        let clicker_gob = match self.sessions.get(&sid).and_then(|o| o.player_gob) {
            Some(g) => g,
            None => return,
        };
        if target == clicker_gob {
            return;
        }
        // Party-invite applicability does NOT gate the Fight option
        // (session 39): an already-partied or full-party target can
        // still be dueled, so the refusals below only drop the invite
        // petal from the menu instead of blocking it entirely.
        let mut invite_ok = self.world.party_idx(target).is_none();
        if invite_ok {
            if let Some(pidx) = self.world.party_idx(clicker_gob) {
                let party = &self.world.parties[pidx];
                if party.leader != clicker_gob || party.members.len() >= crate::party::MAX_MEMBERS {
                    invite_ok = false;
                }
            }
        }
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        // One flower menu at a time per session.
        if let Some((old, _)) = out.player_menu {
            out.send(wdg::dst_wdg(old));
        }
        if let Some((old, _)) = out.crop_menu {
            out.send(wdg::dst_wdg(old));
        }
        if let Some((old, _)) = out.item_menu {
            out.send(wdg::dst_wdg(old));
        }
        let w = out.new_wid("sm");
        let petals: Vec<ListVal> = if invite_ok {
            vec![
                ListVal::S("Invite to party".to_owned()),
                ListVal::S("Fight".to_owned()),
                ListVal::S("Cancel".to_owned()),
            ]
        } else {
            vec![
                ListVal::S("Fight".to_owned()),
                ListVal::S("Cancel".to_owned()),
            ]
        };
        out.send(wdg::new_wdg(w, "sm", -1, -1, 0, &petals));
        out.player_menu = Some((w, crate::party::PlayerMenu::InviteTarget(target)));
    }

    /// Melee duel offer on a CROSS-NODE guest player (session 39): the
    /// same flower menu, minus the party petal - party membership has no
    /// cross-node relay, so Fight is the only offer available.
    fn open_guest_fight_menu(&mut self, sid: SessionId, target: GobId) {
        let clicker_gob = match self.sessions.get(&sid).and_then(|o| o.player_gob) {
            Some(g) => g,
            None => return,
        };
        if target == clicker_gob {
            return;
        }
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        if let Some((old, _)) = out.player_menu {
            out.send(wdg::dst_wdg(old));
        }
        let w = out.new_wid("sm");
        out.send(wdg::new_wdg(
            w,
            "sm",
            -1,
            -1,
            0,
            &[
                ListVal::S("Fight".to_owned()),
                ListVal::S("Cancel".to_owned()),
            ],
        ));
        out.player_menu = Some((w, crate::party::PlayerMenu::FightTarget(target)));
    }

    /// Flower menu petal on a party menu: confirm/cancel the clicker's
    /// invite or the invitee's join.
    fn on_party_menu_choice(&mut self, sid: SessionId, wid: u16, choice: i32) {
        let action = self
            .sessions
            .get(&sid)
            .and_then(|o| o.player_menu)
            .filter(|(w, _)| *w == wid)
            .map(|(_, a)| a);
        let Some(action) = action else {
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.player_menu = None;
        out.send(wdg::dst_wdg(wid));
        // NOTE: the petal index is NOT validated here - each action below
        // knows its own menu layout (the Fight petal is index 1 on the
        // invite menu, 0 on the guest menu) and cancels on anything else.
        match action {
            crate::party::PlayerMenu::InviteTarget(target) => {
                // Petal 0 invites; petal 1 opens the melee duel (session
                // 39); anything else cancels.
                if choice == 0 {
                    self.send_party_invitation(sid, target);
                } else if choice == 1 {
                    self.start_pvp_melee(sid, target);
                } else {
                    let out = self.sessions.get_mut(&sid);
                    if let Some(out) = out {
                        out.send(wdg::wdgmsg(wid, "cancel", &[]));
                    }
                }
            }
            crate::party::PlayerMenu::JoinParty { leader } => {
                if choice == 0 {
                    self.join_party(leader, sid);
                } else {
                    let out = self.sessions.get_mut(&sid);
                    if let Some(out) = out {
                        out.send(wdg::wdgmsg(wid, "cancel", &[]));
                    }
                }
            }
            crate::party::PlayerMenu::FightTarget(target) => {
                if choice == 0 {
                    self.start_pvp_melee(sid, target);
                } else {
                    let out = self.sessions.get_mut(&sid);
                    if let Some(out) = out {
                        out.send(wdg::wdgmsg(wid, "cancel", &[]));
                    }
                }
            }
        }
    }

    /// The clicker confirmed: offer membership to the target player.
    fn send_party_invitation(&mut self, inviter_sid: SessionId, target: GobId) {
        let tidx = match self.world.players.iter().position(|p| p.gob == target) {
            Some(i) => i,
            None => return,
        };
        let (target_sid, target_name) = {
            let t = &self.world.players[tidx];
            (t.session, t.name.clone())
        };
        let inviter_name = self
            .world
            .by_session
            .get(&inviter_sid)
            .and_then(|i| self.world.players.get(*i))
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Someone".to_owned());
        let leader_gob = self
            .sessions
            .get(&inviter_sid)
            .and_then(|o| o.player_gob)
            .unwrap_or(target);
        // Re-check the target is still partyless when the menu was open.
        if self.world.party_idx(target).is_some() {
            self.system_line(inviter_sid, "That player is already in a party.");
            return;
        }
        let Some(out) = self.sessions.get_mut(&target_sid) else {
            return;
        };
        if let Some((old, _)) = out.player_menu {
            out.send(wdg::dst_wdg(old));
        }
        let w = out.new_wid("sm");
        out.send(wdg::new_wdg(
            w,
            "sm",
            -1,
            -1,
            0,
            &[
                ListVal::S(format!("Join {}'s party", inviter_name)),
                ListVal::S("Decline".to_owned()),
            ],
        ));
        out.player_menu = Some((
            w,
            crate::party::PlayerMenu::JoinParty { leader: leader_gob },
        ));
        self.system_line(
            target_sid,
            &format!("{} invites you to join their party.", inviter_name),
        );
        let _ = target_name;
    }

    /// The invitee accepted: create or extend the leader's party.
    fn join_party(&mut self, leader_gob: GobId, joiner_sid: SessionId) {
        let Some(jidx) = self.world.by_session.get(&joiner_sid).copied() else {
            return;
        };
        let joiner_gob = self.world.players[jidx].gob;
        if self.world.party_idx(joiner_gob).is_some() {
            self.system_line(joiner_sid, "You are already in a party.");
            return;
        }
        let pidx = match self.world.party_idx(leader_gob) {
            Some(i) => i,
            None => {
                self.world
                    .parties
                    .push(crate::party::PartyState::new(leader_gob));
                self.world.parties.len() - 1
            }
        };
        match self.world.parties[pidx].add(joiner_gob) {
            Ok(()) => {}
            Err(crate::party::PartyError::Full) => {
                self.system_line(joiner_sid, "That party is full.");
                return;
            }
            Err(crate::party::PartyError::AlreadyMember) => return,
        }
        let leader_sid = self
            .world
            .players
            .iter()
            .find(|p| p.gob == leader_gob)
            .map(|p| p.session);
        self.sync_party(pidx);
        self.system_line(joiner_sid, "You joined the party.");
        if let Some(s) = leader_sid {
            let joiner_name = self.world.players[jidx].name.clone();
            self.system_line(s, &format!("{} joined your party.", joiner_name));
        }
    }

    /// Broadcast the party state to every member (RMSG_PARTY records) and
    /// lazily create the `pv` roster widget for members who lack one.
    fn sync_party(&mut self, pidx: usize) {
        let party = self.world.parties[pidx].clone();
        let mut records: Vec<wdg::PartyRec> = vec![wdg::PartyRec::List(&party.members)];
        records.push(wdg::PartyRec::Leader(party.leader));
        for (i, m) in party.members.iter().enumerate() {
            let pos = self.world.gobs.get(*m).map(|s| self.world.gobs.pos[s]);
            records.push(wdg::PartyRec::Member {
                gob: *m,
                pos,
                color: crate::party::color_for(i),
            });
        }
        let payload = wdg::party(&records);
        for m in party.members.iter() {
            let Some(pslot) = self.world.players.iter().position(|p| p.gob == *m) else {
                continue;
            };
            let s = self.world.players[pslot].session;
            let Some(out) = self.sessions.get_mut(&s) else {
                continue;
            };
            if out.party_wid == 0 {
                let own = out.player_gob.unwrap_or(*m);
                let w = out.new_wid("pv");
                out.send(wdg::new_wdg(w, "pv", 10, 150, 0, &[ListVal::I(own)]));
                out.party_wid = w;
            }
            out.send(payload.clone());
        }
    }

    /// Leave-party button on the roster widget.
    fn party_leave(&mut self, sid: SessionId) {
        let Some(gob) = self.sessions.get(&sid).and_then(|o| o.player_gob) else {
            return;
        };
        self.party_leave_gob(gob);
    }

    /// Remove a gob from its party, transfer leadership or disband, and
    /// clear the client-side roster state of everyone involved.
    fn party_leave_gob(&mut self, gob: GobId) {
        let Some(pidx) = self.world.party_idx(gob) else {
            return;
        };
        let party = self.world.parties[pidx].clone();
        let leaver_sid = self
            .world
            .players
            .iter()
            .find(|p| p.gob == gob)
            .map(|p| p.session);
        let removal = self.world.parties[pidx].remove(gob);
        match removal {
            crate::party::Removal::Disbanded => {
                self.world.parties.remove(pidx);
                // Close every member's roster and clear client state.
                for m in &party.members {
                    let Some(pslot) = self.world.players.iter().position(|p| p.gob == *m) else {
                        continue;
                    };
                    let s = self.world.players[pslot].session;
                    let Some(out) = self.sessions.get_mut(&s) else {
                        continue;
                    };
                    if out.party_wid != 0 {
                        out.send(wdg::dst_wdg(out.party_wid));
                        out.party_wid = 0;
                    }
                    out.send(wdg::party(&[wdg::PartyRec::List(&[])]));
                }
            }
            crate::party::Removal::LeaderChanged { new_leader } => {
                self.sync_party(pidx);
                if let Some(pslot) = self.world.players.iter().position(|p| p.gob == new_leader) {
                    let s = self.world.players[pslot].session;
                    self.system_line(s, "You are now the party leader.");
                }
            }
            crate::party::Removal::Removed => {
                self.sync_party(pidx);
            }
        }
        if let Some(s) = leaver_sid {
            let Some(out) = self.sessions.get_mut(&s) else {
                return;
            };
            if out.party_wid != 0 {
                out.send(wdg::dst_wdg(out.party_wid));
                out.party_wid = 0;
            }
            out.send(wdg::party(&[wdg::PartyRec::List(&[])]));
            self.system_line(s, "You left the party.");
        }
    }

    /// Widget id of the session's open character sheet, if any.
    fn chr_window(&self, sid: SessionId) -> Option<u16> {
        self.sessions.get(&sid)?.chr_window()
    }

    /// Push the LP balance + skill lists to an open character sheet
    /// (CharWnd `exp`/`nsk`/`psk` uimsgs). Only catalog names whose pack
    /// resource exists are pushed; `nsk` carries (name, cost) pairs of
    /// everything the character does not own yet.
    fn push_lp_msgs(&mut self, sid: SessionId) {
        let Some(wid) = self.chr_window(sid) else {
            return;
        };
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return;
        };
        let p = &self.world.players[pidx];
        let lp = p.lp;
        let mut owned: Vec<&'static str> = p.skills.iter().copied().collect();
        owned.sort_unstable();
        let available: Vec<(&'static str, i32)> = crate::skills::CATALOG
            .iter()
            .filter(|s| !p.skills.contains(s.name))
            .map(|s| (s.name, s.cost))
            .collect();
        let Some(out) = self.sessions.get(&sid) else {
            return;
        };
        out.send(wdg::wdgmsg(wid, "exp", &[ListVal::I(lp)]));
        let nsk_args: Vec<ListVal> = available
            .iter()
            .flat_map(|(n, c)| [ListVal::S(n.to_string()), ListVal::I(*c)])
            .collect();
        out.send(wdg::wdgmsg(wid, "nsk", &nsk_args));
        let psk_args: Vec<ListVal> = owned.iter().map(|n| ListVal::S(n.to_string())).collect();
        out.send(wdg::wdgmsg(wid, "psk", &psk_args));
    }

    /// chr "buy": purchase a non-incrementable skill from the catalog.
    fn on_skill_buy(&mut self, sid: SessionId, name: &str) {
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return;
        };
        let outcome = {
            let p = &mut self.world.players[pidx];
            crate::skills::buy(&mut p.skills, &mut p.lp, name)
        };
        match outcome {
            Ok(def) => {
                info!(sid, skill = def.name, "skill purchased");
                self.system_line(sid, &format!("You learned {}.", def.label));
            }
            Err(crate::skills::BuyError::Unknown) => {
                debug!(sid, skill = name, "buy refused: unknown skill");
                self.system_line(sid, "That skill is unknown to this server.");
            }
            Err(crate::skills::BuyError::Owned) => {
                self.system_line(sid, "You already know that skill.");
            }
            Err(crate::skills::BuyError::TooExpensive) => {
                self.system_line(sid, "Not enough learning points.");
            }
            Err(crate::skills::BuyError::Prerequisite) => {
                let def = crate::skills::catalog_get(name);
                let prereq = def
                    .and_then(|d| d.prereq)
                    .and_then(crate::skills::catalog_get);
                let label = prereq.map(|d| d.label).unwrap_or("another skill");
                self.system_line(sid, &format!("You need to know {label} first."));
            }
        }
        self.push_lp_msgs(sid);
    }

    /// chr "sattr": raise incrementable skill values. The client sends
    /// EVERY SAttr as (name, targetBaseValue) pairs on each Buy click —
    /// untouched ones carry their current value and are skipped here.
    /// The batch is priced first and applied all-or-nothing (the client
    /// prediction is advisory; the server is authoritative).
    fn on_skill_attrs(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
        let mut pairs: Vec<(&str, i32)> = Vec::new();
        let mut it = args.iter();
        while let (Some(nm), Some(tv)) = (it.next(), it.next()) {
            if let (Some(nm), Some(tv)) = (nm.as_str(), tv.as_int()) {
                pairs.push((nm, tv));
            }
        }
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return;
        };
        let mut total: i64 = 0;
        let mut plan: Vec<(&str, i32, i32)> = Vec::new();
        for (nm, target) in pairs {
            if !crate::skills::SKILL_VALUES.contains(&nm) {
                self.system_line(sid, "Unknown skill value.");
                return;
            }
            let from = self.world.players[pidx].attrs.get(nm).copied().unwrap_or(0);
            if target == from {
                continue;
            }
            let Some(cost) = crate::skills::sattr_cost(from, target) else {
                self.system_line(sid, "That skill value is out of range.");
                return;
            };
            total += cost as i64;
            plan.push((nm, from, target));
        }
        let wallet = self.world.players[pidx].lp as i64;
        if total > wallet {
            self.system_line(sid, "Not enough learning points.");
            // Refresh the balance the client priced the batch against.
            self.push_lp_msgs(sid);
            return;
        }
        // i64 total of <= 11 bounded costs always fits i32; try_from keeps
        // the numeric-safety rule explicit.
        let total = i32::try_from(total).unwrap_or(i32::MAX);
        {
            let p = &mut self.world.players[pidx];
            for (nm, _from, to) in &plan {
                p.attrs.insert(nm.to_string(), *to);
            }
            p.lp = p.lp.saturating_sub(total);
        }
        if !plan.is_empty() {
            info!(
                sid,
                spent = total,
                raises = plan.len(),
                "skill values raised"
            );
        }
        // Re-push the FULL attribute snapshot: CharWnd SAttr widgets
        // re-render when their cattr entry updates, and skill values just
        // changed (push_cattr only carries vitals).
        let snapshot = self.char_attr_snapshot(sid);
        let Some(out) = self.sessions.get(&sid) else {
            return;
        };
        out.send(wdg::cattr(&snapshot));
        self.push_lp_msgs(sid);
    }

    /// True when the player's incrementable skill value `name` is >= `min`.
    fn has_skill_value(&self, sid: SessionId, name: &str, min: i32) -> bool {
        self.world
            .player(sid)
            .map(|p| p.attrs.get(name).copied().unwrap_or(0) >= min)
            .unwrap_or(false)
    }

    fn start_fight(&mut self, sid: SessionId, target: GobId, species: Species) {
        // Cluster: the target's authority lives on another node. The fight
        // UI and the attacker's offence bar stay LOCAL (they are session
        // state); the animal's defence bar and HP stay on its owner. Each
        // swing relays a RelayAttack there and the authoritative FightBars
        // answer re-syncs the local mirror (`world.guest_fights`).
        if self.world.guests.contains_key(&target) {
            if let Some(p) = self.world.player_mut(sid) {
                p.fight_target = Some(target);
                p.atk_cd = 0;
            }
            self.fight_open(sid, target);
            self.world.guest_fights.insert(
                target,
                crate::state::AnimalFight {
                    off: 0,
                    def: crate::fight::BAR_FULL,
                    intensity: 0,
                },
            );
            info!(sid, target, ?species, "relay fight started");
            return;
        }
        if let Some(p) = self.world.player_mut(sid) {
            p.fight_target = Some(target);
            p.atk_cd = 0;
        }
        self.fight_open(sid, target);
        self.world
            .animal_fights
            .entry(target)
            .or_insert_with(|| crate::state::AnimalFight {
                off: 0,
                def: crate::fight::BAR_FULL,
                intensity: 0,
            });
        info!(sid, target, ?species, "fight started");
    }

    /// Open the unarmed melee duel on another PLAYER (session 39 PvP).
    /// The attacker's offence bar and the fight UI stay LOCAL (session
    /// state, exactly like the animal fight); the victim's defence bar
    /// is the victim session's `own_def` when both players are local,
    /// or lives on the victim's home node when the target is a guest
    /// (each swing relays a PvpSwing there and the PvpSwingResult
    /// answer re-syncs the local mirror).
    fn start_pvp_melee(&mut self, sid: SessionId, target: GobId) {
        // Self-clicks never engage (the menu guard already refuses them,
        // this is the belt-and-braces path for direct callers).
        if self.world.player(sid).map(|p| p.gob) == Some(target) {
            return;
        }
        // Melee and ranged are exclusive player state; drop any live aim.
        if let Some(p) = self.world.player_mut(sid) {
            p.aim = None;
        }
        // Cross-node guest player: mirror the animal relay-fight setup.
        if self.world.guests.contains_key(&target) {
            let vname = self
                .world
                .guests
                .get(&target)
                .and_then(|g| g.kind.player_name())
                .unwrap_or("someone")
                .to_owned();
            if let Some(p) = self.world.player_mut(sid) {
                p.fight_target = Some(target);
                p.atk_cd = 0;
            }
            self.fight_open(sid, target);
            self.world
                .guest_fights
                .entry(target)
                .or_insert_with(|| crate::state::AnimalFight {
                    off: 0,
                    def: crate::fight::BAR_FULL,
                    intensity: 0,
                });
            self.chat_line(sid, &format!("You attack {vname}!"), Some((255, 200, 128)));
            info!(sid, target, "pvp relay duel started");
            return;
        }
        // Local player target: open the duel on BOTH sides - the victim
        // gets a relation on the attacker immediately (legacy Fightview
        // opens both ways) so they can select the attacker in the fight
        // window and answer without hunting for the flower menu first.
        let Some(vpidx) = self.world.players.iter().position(|p| p.gob == target) else {
            return;
        };
        let vsid = self.world.players[vpidx].session;
        let vname = self.world.players[vpidx].name.clone();
        let agob = self.world.player(sid).map(|p| p.gob);
        let aname = self.world.player(sid).map(|p| p.name.clone());
        let Some(agob) = agob else { return };
        if let Some(p) = self.world.player_mut(sid) {
            p.fight_target = Some(target);
            p.atk_cd = 0;
        }
        self.fight_open(sid, target);
        self.fight_open(vsid, agob);
        self.chat_line(sid, &format!("You attack {vname}!"), Some((255, 200, 128)));
        if let Some(aname) = aname {
            self.chat_line(
                vsid,
                &format!("{aname} attacks you!"),
                Some((255, 128, 128)),
            );
        }
        info!(sid, target, vsid, "pvp melee duel started");
    }

    // ------------------------------------------------------------------
    // Bow ranged combat (archery.rs)
    // ------------------------------------------------------------------

    /// Begin ranged aiming at `target` when the player has an equipped
    /// bow. Returns true when the ranged path owns the click (aim
    /// started, or refused with chat feedback); false falls through to
    /// the melee fight window. Consuming the click keeps a bow carrier
    /// out of melee engagements entirely, matching the legacy split
    /// between the Shoot action and the openings fight.
    fn start_aim(&mut self, sid: SessionId, target: GobId) -> bool {
        // PvP guard (session 38): never aim at yourself - the click
        // falls through to the caller's default path (the party menu).
        if self.world.player(sid).map(|p| p.gob) == Some(target) {
            return false;
        }
        let bow_gidx = self.world.res.intern("gfx/invobjs/bow");
        let found = self.world.player(sid).and_then(|p| {
            p.equip
                .iter()
                .flatten()
                .find(|s| s.res == bow_gidx)
                .map(|s| s.ql)
        });
        let Some(bow_ql) = found else {
            return false;
        };
        let rate = crate::archery::BOWS
            .iter()
            .find(|(r, _)| *r == "gfx/invobjs/bow")
            .map(|(_, rate)| *rate)
            .unwrap_or(crate::archery::AIM_RATE_WOODBOW);
        // Arrows are mandatory: a dry bow may not aim.
        let arrow_gidx: Vec<u16> = crate::archery::ARROWS
            .iter()
            .map(|a| self.world.res.intern(a))
            .collect();
        let has_arrows = self
            .world
            .player(sid)
            .map(|p| {
                p.inv
                    .iter()
                    .any(|s| arrow_gidx.contains(&s.res) && s.count > 0)
            })
            .unwrap_or(false);
        if !has_arrows {
            self.chat_line(sid, "You have no arrows to shoot.", Some((255, 128, 128)));
            return true;
        }
        // Drop any melee engagement first (the two states are exclusive).
        let old_target = self
            .world
            .player_mut(sid)
            .and_then(|p| p.fight_target.take());
        if let Some(old) = old_target {
            self.fight_del(sid, old);
        }
        if let Some(p) = self.world.player_mut(sid) {
            p.aim = Some(crate::archery::RangedAim::new(target, bow_ql, rate));
        }
        self.chat_line(
            sid,
            "You draw your bow and start aiming...",
            Some((192, 255, 192)),
        );
        info!(sid, target, bow_ql, "ranged aim started");
        true
    }

    /// One combat tick of an active aim: chase an out-of-range target,
    /// fill the accuracy meter (chat progress lines), auto-release at a
    /// full meter. Works for local animals AND cross-node guests (the
    /// guest table feeds the same range checks; the shot relays to the
    /// animal's authority node).
    fn tick_aim(
        &mut self,
        pidx: usize,
        sid: SessionId,
        pgob: GobId,
        mut aim: crate::archery::RangedAim,
    ) {
        const CHASE_DROP: i32 = 300; // same disengage radius as melee
        let Some(pslot) = self.world.gobs.get(pgob) else {
            self.world.players[pidx].aim = None;
            return;
        };
        // Guest targets (foreign authority): position from the guest
        // table; local targets from the gob store.
        let guest_pos = self.world.guests.get(&aim.target).map(|g| g.pos);
        if guest_pos.is_none() && self.world.gobs.get(aim.target).is_none() {
            self.world.players[pidx].aim = None;
            self.chat_line(sid, "Your target is gone.", Some((255, 200, 128)));
            return;
        }
        let (px, py) = self.world.gobs.pos[pslot];
        let (tx, ty) = match guest_pos {
            Some(p) => p,
            None => {
                let tslot = self.world.gobs.get(aim.target).expect("checked above");
                self.world.gobs.pos[tslot]
            }
        };
        let dist = (px - tx).abs().max((py - ty).abs());
        if dist > CHASE_DROP {
            self.world.players[pidx].aim = None;
            self.chat_line(
                sid,
                "You lower your bow; the target escaped.",
                Some((255, 200, 128)),
            );
            return;
        }
        if dist > crate::archery::BOW_RANGE {
            // In sight but out of range: close in, keep the aim.
            if self.world.gobs.mv[pslot].is_none() {
                self.start_move(pslot, (tx, ty));
            }
            self.world.players[pidx].aim = Some(aim);
            return;
        }
        aim.meter = (aim.meter + aim.rate).min(crate::archery::AIM_FULL);
        let percent = aim.meter * 100 / crate::archery::AIM_FULL;
        for r in crate::archery::AIM_REPORTS {
            if percent >= r && aim.reported < r {
                self.chat_line(sid, &format!("Aiming at {r}%..."), Some((192, 255, 192)));
                aim.reported = r;
            }
        }
        if aim.meter >= crate::archery::AIM_FULL {
            let roll = self.world.next_ai_rand(100) as u32;
            self.shoot_arrow(pidx, sid, aim, roll);
            return;
        }
        self.world.players[pidx].aim = Some(aim);
    }

    /// Release one arrow at the aim target. `roll` is the 0..99 hit
    /// roll (rng in production, fixed in tests). The arrow is consumed
    /// whether the shot lands or not; the attack meter is depleted per
    /// Legacy:Combat_Actions. Aim continues while the target lives and
    /// arrows remain.
    fn shoot_arrow(
        &mut self,
        pidx: usize,
        sid: SessionId,
        aim: crate::archery::RangedAim,
        roll: u32,
    ) {
        let arrow_gidx: Vec<u16> = crate::archery::ARROWS
            .iter()
            .map(|a| self.world.res.intern(a))
            .collect();
        let arrow_slot = self.world.players[pidx]
            .inv
            .iter()
            .position(|s| arrow_gidx.contains(&s.res) && s.count > 0);
        let Some(aslot) = arrow_slot else {
            self.world.players[pidx].aim = None;
            self.chat_line(sid, "You have no arrows to shoot.", Some((255, 128, 128)));
            return;
        };
        // Consume exactly one arrow.
        {
            let stack = &mut self.world.players[pidx].inv[aslot];
            stack.count -= 1;
            let empty = stack.count == 0;
            if empty {
                self.world.players[pidx].inv.remove(aslot);
            }
        }
        // Deplete the attack meter (frv offence bar).
        if let Some(out) = self.sessions.get_mut(&sid) {
            out.fight.own_off = 0;
        }
        self.world.players[pidx].stamina = (self.world.players[pidx].stamina - 2).max(0);
        let target = aim.target;
        // Guest target (foreign authority): position, species and
        // liveness come from the guest table; the damage rides a
        // RelayAttack (chip 0 = ranged, bypasses the openings gate on
        // the authority side) instead of the local damage path.
        // Session 38: player targets (local Kind::Player, guest
        // GuestKind::Player) resolve to the PvP path - the victim's
        // armor/HP/knockout live on ITS home node (hurt_player), so a
        // cross-node hit rides a PvpArrow there.
        enum ShotTarget {
            Animal {
                species: Species,
                slot: Option<usize>,
            },
            Player {
                name: String,
                pidx: Option<usize>,
            },
        }
        let guest = self.world.guests.get(&target).cloned();
        let (tgt, tpos) = match &guest {
            Some(g) => match &g.kind {
                crate::nodes::GuestKind::Animal { species } => {
                    match Species::from_index(*species) {
                        Some(sp) => (
                            ShotTarget::Animal {
                                species: sp,
                                slot: None,
                            },
                            g.pos,
                        ),
                        None => {
                            self.world.players[pidx].aim = None;
                            return;
                        }
                    }
                }
                crate::nodes::GuestKind::Player { name, .. } => (
                    ShotTarget::Player {
                        name: name.clone(),
                        pidx: None,
                    },
                    g.pos,
                ),
                _ => {
                    self.world.players[pidx].aim = None;
                    return;
                }
            },
            None => match self.world.gobs.get(target) {
                Some(s) => match self.world.gobs.kind[s] {
                    crate::state::Kind::Animal { species } => (
                        ShotTarget::Animal {
                            species,
                            slot: Some(s),
                        },
                        self.world.gobs.pos[s],
                    ),
                    crate::state::Kind::Player { player } => (
                        ShotTarget::Player {
                            name: self
                                .world
                                .players
                                .get(player)
                                .map(|p| p.name.clone())
                                .unwrap_or_default(),
                            pidx: Some(player),
                        },
                        self.world.gobs.pos[s],
                    ),
                    _ => {
                        self.world.players[pidx].aim = None;
                        return;
                    }
                },
                None => {
                    self.world.players[pidx].aim = None;
                    return;
                }
            },
        };
        let (px, py) = self
            .world
            .gobs
            .get(self.world.players[pidx].gob)
            .map(|s| self.world.gobs.pos[s])
            .unwrap_or((0, 0));
        let dist = (px - tpos.0).abs().max((py - tpos.1).abs());
        let marks = self.world.players[pidx]
            .attrs
            .get("marks")
            .copied()
            .unwrap_or(0);
        let chance = crate::archery::hit_chance(dist, marks);
        let dmg = crate::archery::bow_damage(aim.bow_ql);
        if roll < chance as u32 {
            // Destructure the target into plain data first: the
            // damage paths below need &mut self.
            let (is_player, tname, vidx) = match &tgt {
                ShotTarget::Animal { species, .. } => (false, species.name().to_string(), None),
                ShotTarget::Player { name, pidx, .. } => (true, name.clone(), *pidx),
            };
            match vidx {
                // LOCAL PvP shot: armor absorption, HP and the
                // knockout path all live on this node.
                Some(vidx) => {
                    let vsid = self.world.players[vidx].session;
                    let knocked = self.hurt_player(vidx, dmg, self.world.players[pidx].gob);
                    self.fx_overlay_broadcast(target, "gfx/fx/hit");
                    self.chat_line(
                        vsid,
                        &format!("An arrow hits you for {dmg} damage."),
                        Some((255, 128, 128)),
                    );
                    self.chat_line(
                        sid,
                        &format!("Your arrow hits {tname} for {dmg} damage."),
                        Some((192, 255, 192)),
                    );
                    if knocked {
                        self.chat_line(
                            sid,
                            &format!("You have defeated {tname}!"),
                            Some((192, 255, 192)),
                        );
                        // PvP knockout consequences (server policy):
                        // LP loss on the loser, criminal flag on the
                        // winner (combat-system.md).
                        self.knockout_lp_loss(vidx);
                        self.flag_criminal(pidx);
                    }
                }
                None if is_player => {
                    // CROSS-NODE PvP shot: the hit roll already happened
                    // here; the victim's armor/HP/knockout live on ITS
                    // home node (node_of_gob from the gob id).
                    self.chat_line(
                        sid,
                        &format!("Your arrow hits {tname} for {dmg} damage."),
                        Some((192, 255, 192)),
                    );
                    if let Some(c) = self.cluster.as_ref() {
                        let home = self.node_of_gob(target);
                        c.mesh.send(
                            home,
                            crate::nodes::NodeMsg::PvpArrow {
                                victim: target,
                                attacker: self.world.players[pidx].gob,
                                dmg,
                            },
                        );
                    }
                }
                _ => {
                    // Animal target. Cross-node shot: the authority
                    // applies the damage (chip 0 marks the ranged
                    // bypass).
                    self.chat_line(
                        sid,
                        &format!("Your arrow hits the {tname} for {dmg} damage."),
                        Some((192, 255, 192)),
                    );
                    let tslot = match &tgt {
                        ShotTarget::Animal { slot, .. } => *slot,
                        ShotTarget::Player { .. } => None,
                    };
                    match (guest.is_some(), tslot) {
                        (true, _) => {
                            if let Some(c) = self.cluster.as_ref() {
                                let authority =
                                    self.cell_owner(crate::visidx::cell_of(tpos.0, tpos.1));
                                c.mesh.send(
                                    authority,
                                    crate::nodes::NodeMsg::RelayAttack {
                                        attacker: self.world.players[pidx].gob,
                                        target,
                                        chip: 0,
                                        dmg,
                                    },
                                );
                            }
                        }
                        (false, Some(ts)) => {
                            self.damage_animal(pidx, sid, target, ts, dmg);
                        }
                        (false, None) => {}
                    }
                }
            }
        } else {
            self.chat_line(sid, "Your arrow misses.", Some((255, 200, 128)));
        }
        // Keep aiming while the target lives and arrows remain. Guest
        // liveness comes from the guest table (a kill arrives as a
        // GuestRetract from the authority).
        let alive = match &guest {
            Some(_) => self.world.guests.contains_key(&target),
            None => self.world.gobs.get(target).is_some(),
        };
        let more_arrows = self.world.players[pidx]
            .inv
            .iter()
            .any(|s| arrow_gidx.contains(&s.res) && s.count > 0);
        if alive && more_arrows {
            self.world.players[pidx].aim =
                Some(crate::archery::RangedAim::new(target, aim.bow_ql, aim.rate));
        } else {
            self.world.players[pidx].aim = None;
            if !alive {
                self.chat_line(sid, "You lower your bow.", Some((192, 255, 192)));
            }
        }
    }

    // ------------------------------------------------------------------
    // Fightview (frv) widget protocol
    // ------------------------------------------------------------------

    /// Send one frv uimsg to the session (no-op without a fight widget).
    fn fight_uimsg(&mut self, sid: SessionId, name: &str, args: &[i32]) {
        if let Some(out) = self.sessions.get_mut(&sid) {
            if let Some(w) = out.fight.widget {
                let b = crate::fight::uimsg(w, name, args);
                out.send(b);
            }
        }
    }

    /// Open (or reuse) the fight window and add a relation for `target`.
    fn fight_open(&mut self, sid: SessionId, target: GobId) {
        let existing = self.sessions.get(&sid).and_then(|out| out.fight.widget);
        let widget = match existing {
            Some(w) => Some(w),
            None => self.sessions.get_mut(&sid).map(|out| {
                let w = out.new_wid("frv");
                out.fight.widget = Some(w);
                let b = wdg::new_wdg(w, "frv", 0, 0, 0, &[]);
                out.send(b);
                w
            }),
        };
        let Some(widget) = widget else { return };
        let exists = self
            .sessions
            .get(&sid)
            .map(|out| out.fight.rel(target).is_some())
            .unwrap_or(false);
        if !exists {
            let rel = crate::fight::FightRel::new(target);
            let args = vec![
                rel.gob,
                rel.balance,
                rel.intensity,
                rel.give,
                rel.ip_self,
                rel.ip_other,
                rel.offence,
                rel.defence,
            ];
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.fight.rels.push(rel);
                out.send(crate::fight::uimsg(widget, "new", &args));
            }
        }
        // Focus the fresh relation.
        self.fight_uimsg(sid, "cur", &[target]);
    }

    /// Remove one relation; destroy the widget when the list empties.
    fn fight_del(&mut self, sid: SessionId, gob: GobId) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        let Some(widget) = out.fight.widget else {
            return;
        };
        out.fight.rels.retain(|r| r.gob != gob);
        out.send(crate::fight::uimsg(widget, "del", &[gob]));
        if out.fight.rels.is_empty() {
            let w = out.fight.widget.take();
            if let Some(w) = w {
                out.send(wdg::dst_wdg(w));
            }
        }
    }

    /// Handle client->server frv wdgmsg (click / give).
    /// One fight-window maneuver: `act("atk", id)` from the MenuGrid
    /// (the paginae/atk/* buttons). Validates the IP/advantage
    /// requirements against the CURRENT relation, applies the
    /// costs/gains, mirrors the opponent-side IP delta to a local
    /// victim, and streams the frv updates (`upd`, `atk`, `blk`).
    /// Guest targets keep their authoritative IP on their home node
    /// (documented server policy in combat-system.md).
    fn on_maneuver(&mut self, sid: SessionId, id: &str) {
        let Some(m) = crate::fight::maneuver(id) else {
            debug!(sid, id, "unknown maneuver id");
            return;
        };
        let Some(&pidx) = self.world.by_session.get(&sid) else {
            return;
        };
        let pgob = self.world.players[pidx].gob;
        let Some(target) = self.world.players[pidx].fight_target else {
            self.chat_line(sid, "You are not fighting anyone.", Some((255, 128, 128)));
            return;
        };
        // Requirements and costs first (refusals never mutate state).
        let refuse: Option<String> = {
            let Some(out) = self.sessions.get(&sid) else {
                return;
            };
            let Some(rel) = out.fight.rel(target) else {
                return;
            };
            if rel.ip_self < m.req_ip {
                Some(format!(
                    "You need at least {} initiative points for that.",
                    m.req_ip
                ))
            } else if rel.adv < m.req_adv {
                Some("You need more advantage for that.".to_owned())
            } else if rel.ip_self < m.ip_cost {
                Some("Not enough initiative points.".to_owned())
            } else {
                None
            }
        };
        if let Some(why) = refuse {
            self.chat_line(sid, &why, Some((255, 128, 128)));
            return;
        }
        // Quell the Beast (session 45 taming): target-specific gates on
        // top of the static IP/advantage requirements (animals-and-
        // husbandry.md taming service). Refusals chat and mutate nothing.
        if id == "quell" {
            let why = self.quell_gate(pidx, target);
            if let Some(why) = why {
                self.chat_line(sid, &why, Some((255, 128, 128)));
                return;
            }
        }
        // Opponent-side IP delta FIRST: a LOCAL victim's own pool changes
        // (their rel(pgob).ip_self) and their window re-streams; a
        // GUEST's authoritative pool lives on their home node (the
        // attacker's mirror applies the prediction below).
        let mut new_opp_ip: Option<i32> = None;
        if m.ip_opp != 0 {
            let vsid = self
                .world
                .players
                .iter()
                .find(|p| p.gob == target)
                .map(|p| p.session);
            if let Some(vsid) = vsid {
                let vupd = self.sessions.get_mut(&vsid).and_then(|vout| {
                    let rel = vout.fight.rel_mut(pgob)?;
                    rel.ip_self = (rel.ip_self + m.ip_opp).max(0);
                    Some((
                        rel.ip_self,
                        vec![
                            rel.gob,
                            rel.balance,
                            rel.intensity,
                            rel.give,
                            rel.ip_self,
                            rel.ip_other,
                        ],
                    ))
                });
                if let Some((pool, vupd)) = vupd {
                    new_opp_ip = Some(pool);
                    if let Some(vout) = self.sessions.get_mut(&vsid) {
                        if let Some(w) = vout.fight.widget {
                            let b = crate::fight::uimsg(w, "upd", &vupd);
                            vout.send(b);
                        }
                    }
                }
            } else if let Some(c) = self.cluster.as_ref() {
                // The victim is a foreign session player (a guest gob
                // here): her IP pool is authoritative on her home node.
                // Relay the opponent-pool delta so her fight window stays
                // truthful (session 42 ManeuverDelta; the attacker's own
                // window below applies the mirror prediction).
                let home = self.node_of_gob(target);
                if home != c.me {
                    c.mesh.send(
                        home,
                        crate::nodes::NodeMsg::ManeuverDelta {
                            attacker: pgob,
                            victim: target,
                            ip_opp: m.ip_opp,
                        },
                    );
                }
            }
        }
        // Apply the user's side: IP economy, advantage, attack queue.
        let upd = {
            let Some(out) = self.sessions.get_mut(&sid) else {
                return;
            };
            // Attack queue / stance FIRST (disjoint fields from the
            // relation list; rel_mut borrows the whole fight state).
            match m.kind {
                crate::fight::ManeuverKind::Attack => {
                    // The two-slot queue: the current attack slides into
                    // `next`, the selection becomes `current` (Fightview
                    // renders atk [cur, next]).
                    let old = out.fight.atk_cur;
                    out.fight.atk_cur = Some(m.res);
                    out.fight.atk_next = old;
                }
                crate::fight::ManeuverKind::Block => {
                    out.fight.blk = Some(m.res);
                }
                crate::fight::ManeuverKind::Boost => {}
            }
            let Some(rel) = out.fight.rel_mut(target) else {
                return;
            };
            rel.ip_self = (rel.ip_self - m.ip_cost + m.ip_gain).max(0);
            rel.adv = (rel.adv + m.adv).clamp(-50, 50);
            rel.sync_balance();
            // The opponent pool view: the victim's fresh value when the
            // delta applied locally, else the mirror prediction.
            rel.ip_other = new_opp_ip.unwrap_or((rel.ip_other + m.ip_opp).max(0));
            vec![
                rel.gob,
                rel.balance,
                rel.intensity,
                rel.give,
                rel.ip_self,
                rel.ip_other,
            ]
        };
        // Stream the user's own window: the relation update plus the
        // attack-queue / stance slot. Intern the pagina resources
        // BEFORE the mutable session borrow (res.intern borrows the
        // world store).
        let (cur, next, blk) = {
            let Some(out) = self.sessions.get(&sid) else {
                return;
            };
            (out.fight.atk_cur, out.fight.atk_next, out.fight.blk)
        };
        let cur_gi = cur.map(|n| (n, self.world.res.intern(n)));
        let next_gi = next.map(|n| (n, self.world.res.intern(n)));
        let blk_gi = blk.map(|n| (n, self.world.res.intern(n)));
        if let Some(out) = self.sessions.get_mut(&sid) {
            if let Some(w) = out.fight.widget {
                let b = crate::fight::uimsg(w, "upd", &upd);
                out.send(b);
                match m.kind {
                    crate::fight::ManeuverKind::Attack => {
                        let wc = cur_gi.map(|(n, gi)| announce_res(out, gi, n)).unwrap_or(-1);
                        let wn = next_gi
                            .map(|(n, gi)| announce_res(out, gi, n))
                            .unwrap_or(-1);
                        let b = crate::fight::uimsg(w, "atk", &[wc, wn]);
                        out.send(b);
                    }
                    crate::fight::ManeuverKind::Block => {
                        let wb = blk_gi.map(|(n, gi)| announce_res(out, gi, n)).unwrap_or(-1);
                        let b = crate::fight::uimsg(w, "blk", &[wb]);
                        out.send(b);
                    }
                    crate::fight::ManeuverKind::Boost => {}
                }
            }
        }
    }

    /// Handle client->server frv wdgmsg (click / give).
    fn on_frv_msg(&mut self, sid: SessionId, name: &str, args: &[hnh_proto::ListArg]) {
        let ints: Vec<i32> = args.iter().filter_map(|a| a.as_int()).collect();
        match name {
            "click" => {
                // Select that opponent; answer with `cur`. Selecting a
                // melee opponent drops any active ranged aim (the two
                // combat modes are exclusive player state).
                if let Some(&gob) = ints.first() {
                    if let Some(p) = self.world.player_mut(sid) {
                        p.aim = None;
                        p.fight_target = Some(gob);
                    }
                    if let Some(out) = self.sessions.get_mut(&sid) {
                        if let Some(w) = out.fight.widget {
                            let b = crate::fight::uimsg(w, "cur", &[gob]);
                            out.send(b);
                        }
                    }
                }
            }
            "give" => {
                // Toggle one bit of the two-bit handshake; echo via upd.
                let (Some(gob), Some(button)) = (ints.first().copied(), ints.get(1).copied())
                else {
                    return;
                };
                let bit: i32 = if button != 0 { 2 } else { 1 };
                if let Some(out) = self.sessions.get_mut(&sid) {
                    if let Some(rel) = out.fight.rel_mut(gob) {
                        rel.give ^= bit;
                        let upd = vec![
                            rel.gob,
                            rel.balance,
                            rel.intensity,
                            rel.give,
                            rel.ip_self,
                            rel.ip_other,
                        ];
                        if let Some(w) = out.fight.widget {
                            let b = crate::fight::uimsg(w, "upd", &upd);
                            out.send(b);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Spawn an item drop gob near `at`.
    ///
    /// Two resources per drop: the gob RENDERS with a gfx/terobjs/items
    /// world shape (inventory item resources have no `neg` layer, so the
    /// real client fails their sprite with "No negative found" and the
    /// drop is invisible - measured on the GL client, session 26), while
    /// Kind::Drop::inv_res_idx keeps the gfx/invobjs icon resource so
    /// picking up restores the exact original stack.
    fn spawn_drop_near(
        &mut self,
        at: (i32, i32),
        resname: &'static str,
        ql: u8,
        label: &'static str,
    ) {
        let inv_res_idx = self.world.res.intern(resname);
        let world_res = drop_world_res(resname);
        let res_idx = self.world.res.intern(world_res);
        let jitter = |w: &mut World| (w.next_ai_rand(7) - 3) * 11;
        let jx = jitter(&mut self.world);
        let jy = jitter(&mut self.world);
        let id = self.world.gobs.spawn(
            Kind::Drop {
                resname_idx: res_idx,
                inv_res_idx,
                ql,
                label,
            },
            (at.0 + jx, at.1 + jy),
            res_idx,
            1,
            0,
        );
        self.broadcast_spawn(id);
    }

    fn broadcast_spawn(&mut self, id: GobId) {
        let sids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for sid in sids {
            if self.sessions[&sid].visible.contains(&id) || self.session_in_range(sid, id) {
                self.stream_spawn(sid, id);
            }
        }
        // Cluster: tell subscribed peers about the new gob (they render it
        // as a guest for their sessions).
        self.publish(id, GuestEv::Announce);
    }

    fn broadcast_retract(&mut self, id: GobId) {
        let sids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for sid in sids {
            self.stream_retract(sid, id);
        }
        // Cluster: subscribers drop their guest copy.
        self.publish(id, GuestEv::Retract);
    }

    fn session_in_range(&self, sid: SessionId, id: GobId) -> bool {
        let Some(pslot) = self.world.gobs.get(id) else {
            return false;
        };
        let Some(out) = self.sessions.get(&sid) else {
            return false;
        };
        let Some(pg) = out.player_gob else {
            return false;
        };
        let Some(pp) = self.world.gobs.get(pg) else {
            return false;
        };
        let (px, py) = self.world.gobs.pos[pp];
        let (gx, gy) = self.world.gobs.pos[pslot];
        (gx - px).abs() <= VIEW_RADIUS && (gy - py).abs() <= VIEW_RADIUS
    }

    // ------------------------------------------------------------------
    // Inventory
    // ------------------------------------------------------------------

    fn open_inventory(&mut self, sid: SessionId) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        let existing = out
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "invwnd")
            .map(|(id, _)| *id);
        if existing.is_none() {
            let w = out.new_wid("invwnd");
            // The client's Inventory factory requires the grid size
            // (Coord isz, cells); an empty arg list crashes its create()
            // (ArrayIndexOutOfBounds) and kills the whole UI thread.
            // 4 columns x 8 rows matches the refresh_inventory layout.
            out.send(wdg::new_wdg(w, "inv", 350, 250, 0, &[ListVal::C(4, 8)]));
            self.refresh_inventory(sid);
        }
    }

    fn inv_window(&self, sid: SessionId) -> Option<u16> {
        self.sessions
            .get(&sid)?
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "invwnd")
            .map(|(id, _)| *id)
    }

    // ------------------------------------------------------------------
    // Cursor drag widget (session 26): the held stack rendered at the
    // pointer. Legacy flow: Item.mousedown sends `take`; the server
    // replies with a drag Item widget (drag=1 + grab offset) parented to
    // the root; the client's Item constructor grabs the mouse and the
    // widget follows it (Item.java drag constructor). Without it the
    // held stack is invisible until dropped.
    // ------------------------------------------------------------------

    /// Keep the drag Item widget in sync with the cursor stack: create
    /// it when a stack is picked up, refresh `num` when the count
    /// changes, destroy it when the cursor empties. Call after any flow
    /// that mutates `out.cursor` (take, drop, itemact consumption).
    fn sync_cursor_widget(&mut self, sid: SessionId) {
        let has_cursor = self.sessions.get(&sid).and_then(|o| o.cursor).is_some();
        let has_widget = self.sessions.get(&sid).and_then(|o| o.cursor_wid).is_some();
        match (has_cursor, has_widget) {
            (false, _) => self.hide_cursor_widget(sid),
            (true, false) => self.create_cursor_widget(sid),
            (true, true) => {
                let Some(stack) = self.sessions.get(&sid).and_then(|o| o.cursor) else {
                    return;
                };
                let Some(out) = self.sessions.get_mut(&sid) else {
                    return;
                };
                let w = out.cursor_wid.expect("BUG: has_widget checked");
                out.send(wdg::wdgmsg(w, "num", &[ListVal::I(stack.count as i32)]));
            }
        }
    }

    /// Destroy the drag Item widget (cursor emptied). The cursor stack
    /// itself is untouched.
    fn hide_cursor_widget(&mut self, sid: SessionId) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        if let Some(w) = out.cursor_wid.take() {
            out.send(wdg::dst_wdg(w));
            out.widgets.remove(&w);
        }
    }

    /// Create the drag Item widget for the current cursor stack.
    fn create_cursor_widget(&mut self, sid: SessionId) {
        let Some(stack) = self.sessions.get(&sid).and_then(|o| o.cursor) else {
            return;
        };
        let res_name = self
            .world
            .res
            .name(stack.res)
            .unwrap_or("gfx/invobjs/stone");
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        if out.cursor_wid.is_some() {
            return; // one drag widget at a time
        }
        let wire = out.res.wire_named(stack.res, res_name);
        if let Some((name, ver)) = out.res.pending_announce(wire) {
            out.send(wdg::resid(wire, name, ver));
            out.res.mark_announced(wire);
        }
        let w = out.new_wid("item");
        out.cursor_wid = Some(w);
        // Item factory args (Item.java): res, q, drag flag, drag Coord
        // (grab offset), tooltip, num. Parent 0 = root: the drag item
        // floats over every window.
        out.send(wdg::new_wdg(
            w,
            "item",
            0,
            0,
            0,
            &[
                ListVal::I(wire as i32),
                ListVal::I(stack.ql as i32),
                ListVal::I(1),
                ListVal::C(0, 0),
                // Server tooltip = display name; food-and-fep.md Item.name()
                // precedence makes this the fep.conf lookup key for food.
                ListVal::S(stack.label.to_owned()),
                ListVal::I(stack.count as i32),
            ],
        ));
    }

    /// Rebuild inventory items: destroy old item widgets, create new ones.
    /// The cursor drag widget is not touched (it is not an inventory
    /// item); refresh_inventory runs on every cursor flow, so a stale
    /// dst here would kill the drag widget the sync helper just made.
    fn refresh_inventory(&mut self, sid: SessionId) {
        let Some(inv_wid) = self.inv_window(sid) else {
            return;
        };
        let items: Vec<InvStack> = self
            .world
            .player(sid)
            .map(|p| p.inv.clone())
            .unwrap_or_default();
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        let old: Vec<u16> = out
            .widgets
            .iter()
            .filter(|(_, t)| t.as_str() == "item")
            .map(|(id, _)| *id)
            .filter(|id| Some(*id) != out.cursor_wid)
            .collect();
        for id in old {
            out.send(wdg::dst_wdg(id));
        }
        out.item_wids.clear();
        for (n, stack) in items.iter().enumerate() {
            let res_name = self
                .world
                .res
                .name(stack.res)
                .unwrap_or("gfx/invobjs/stone");
            let wire = out.res.wire_named(stack.res, res_name);
            if let Some((name, ver)) = out.res.pending_announce(wire) {
                out.send(wdg::resid(wire, name, ver));
                out.res.mark_announced(wire);
            }
            let w = out.new_wid("item");
            out.item_wids.insert(w, n);
            let x = 15 + (n as i32 % 4) * 40;
            let y = 15 + (n as i32 / 4) * 40;
            out.send(wdg::new_wdg(
                w,
                "item",
                x,
                y,
                inv_wid,
                &[
                    ListVal::I(wire as i32),
                    ListVal::I(stack.ql as i32),
                    ListVal::I(0),
                    // Server tooltip = display name; food-and-fep.md Item.name()
                    // precedence makes this the fep.conf lookup key for food.
                    ListVal::S(stack.label.to_owned()),
                    ListVal::I(stack.count as i32),
                ],
            ));
        }
    }

    fn inv_drop(&mut self, sid: SessionId, _wid: u16, _args: &[hnh_proto::ListArg]) {
        // Inventory "drop": the client sends it when the held (cursor) item
        // is released onto an inventory grid; the stack returns to storage.
        // Ground drops ride the mapview `drop` wdgmsg instead.
        let stack = {
            let Some(out) = self.sessions.get_mut(&sid) else {
                return;
            };
            out.cursor.take()
        };
        let Some(stack) = stack else {
            return;
        };
        self.hide_cursor_widget(sid);
        self.grant_pickup(sid, stack);
    }

    /// Place a picked-up stack (ground-drop click, cursor release onto the
    /// inventory grid, or a relayed cross-node pickup ack): when the cursor
    /// already drags the SAME resource, the counts merge onto the cursor
    /// (redirection - no failed pickup, one drag stack); otherwise the
    /// stack stores into the inventory, merging into an existing
    /// same-resource stack when one exists (items-and-quality.md leaves
    /// stacking policy to the server; count-weighted quality average, see
    /// InvStack::absorb).
    fn grant_pickup(&mut self, sid: SessionId, stack: InvStack) {
        // Cursor redirection: same resource on the cursor absorbs the new
        // stack; the drag widget's count syncs through sync_cursor_widget.
        let cursor_same = self
            .sessions
            .get(&sid)
            .and_then(|o| o.cursor.as_ref())
            .is_some_and(|c| c.res == stack.res);
        if cursor_same {
            if let Some(out) = self.sessions.get_mut(&sid) {
                if let Some(c) = out.cursor.as_mut() {
                    c.absorb(&stack);
                }
            }
            self.sync_cursor_widget(sid);
            return;
        }
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            // No session player (should not happen for a pickup click):
            // dropping the stack would lose items; keep it safe instead.
            debug!(sid, "grant_pickup without session player");
            return;
        };
        // Inventory merge: the first same-resource stack absorbs; a new
        // resource creates its own stack.
        let existing = self.world.players[pidx]
            .inv
            .iter_mut()
            .find(|s| s.res == stack.res);
        match existing {
            Some(s) => s.absorb(&stack),
            None => self.world.players[pidx].inv.push(stack),
        }
        self.refresh_inventory(sid);
    }

    // ------------------------------------------------------------------
    // Equipment (the Equipory paperdoll, docs/mechanics/items/
    // items-and-quality.md): widget type "epry", 16 wire-indexed slots,
    // full-state "set" sync + "ava" avatar gob binding. Equipping rides
    // the cursor item: "drop" onto a slot stores it, "take" retrieves it.
    // ------------------------------------------------------------------

    fn epry_window(&self, sid: SessionId) -> Option<u16> {
        self.sessions
            .get(&sid)?
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "epry")
            .map(|(id, _)| *id)
    }

    /// The equipped pieces' inventory resource names for one player, in
    /// slot order. Only resources the equip::table knows how to render
    /// pass the filter (the equip module drops non-wearables itself; the
    /// empty-name fallback guards a stale resource index after a pack
    /// change).
    fn player_equip_names(&self, player: usize) -> Vec<&'static str> {
        self.world.players[player]
            .equip
            .iter()
            .flatten()
            .map(|s| self.world.res.name(s.res).unwrap_or(""))
            .filter(|n| !n.is_empty())
            .collect()
    }

    /// Create the paperdoll window if absent, then resync its contents.
    fn open_epry(&mut self, sid: SessionId) {
        if self.epry_window(sid).is_none() {
            let Some(out) = self.sessions.get_mut(&sid) else {
                return;
            };
            let w = out.new_wid("epry");
            out.send(wdg::new_wdg(w, "epry", 0, 0, 0, &[]));
        }
        self.send_epry_state(sid);
    }

    /// Full paperdoll resync: RMSG_WDGMSG "set" (for each of the 16 slots
    /// in order: -1, or wire resid + quality + optional tooltip) followed
    /// by "ava" (the avatar gob the window previews).
    fn send_epry_state(&mut self, sid: SessionId) {
        let Some(w) = self.epry_window(sid) else {
            return;
        };
        let Some(&pidx) = self.world.by_session.get(&sid) else {
            return;
        };
        // Phase 1 (world borrow): snapshot the equipped stacks together
        // with their resource names.
        let equipped: Vec<Option<(InvStack, &'static str)>> = self.world.players[pidx]
            .equip
            .iter()
            .map(|slot| {
                slot.map(|s| {
                    let name = self.world.res.name(s.res).unwrap_or("gfx/invobjs/unknown");
                    (s, name)
                })
            })
            .collect();
        let gob = self.world.players[pidx].gob;
        // Phase 2 (session borrow): wire ids, announcements, uimsgs.
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        let mut args: Vec<ListVal> = Vec::with_capacity(48);
        for slot in &equipped {
            match slot {
                Some((s, name)) => {
                    let wire = out.res.wire_named(s.res, name);
                    if let Some((n, v)) = out.res.pending_announce(wire) {
                        out.send(wdg::resid(wire, n, v));
                        out.res.mark_announced(wire);
                    }
                    args.push(ListVal::I(wire as i32));
                    args.push(ListVal::I(s.ql as i32));
                    // Armor pieces append the "Armor class: D/A" tooltip
                    // line; Equipory.calcAC parses and sums it per slot
                    // (combat-system.md: the server owns the numbers).
                    let tt = match crate::armor::ac_line(name, i32::from(s.ql)) {
                        Some(line) if !s.label.is_empty() => {
                            format!("{}\n{}", s.label, line)
                        }
                        Some(line) => line,
                        None => s.label.to_owned(),
                    };
                    if !tt.is_empty() {
                        args.push(ListVal::S(tt));
                    }
                }
                None => args.push(ListVal::I(-1)),
            }
        }
        out.send(wdg::wdgmsg(w, "set", &args));
        out.send(wdg::wdgmsg(w, "ava", &[ListVal::I(gob)]));
    }

    /// epry "drop" (slot): store the held cursor item into slot `ep`.
    /// Slot -1 (window background) is a deliberate no-op.
    fn epry_drop(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
        let ep = args.first().and_then(|a| a.as_int()).unwrap_or(-1);
        if !(0..16).contains(&ep) {
            return;
        }
        let Some(&pidx) = self.world.by_session.get(&sid) else {
            return;
        };
        if self.world.players[pidx].equip[ep as usize].is_some() {
            return; // slot occupied
        }
        let stack = {
            let Some(out) = self.sessions.get_mut(&sid) else {
                return;
            };
            out.cursor.take()
        };
        let Some(stack) = stack else {
            return; // empty hand
        };
        self.hide_cursor_widget(sid);
        self.world.players[pidx].equip[ep as usize] = Some(stack);
        self.send_epry_state(sid);
        self.stream_equipment_change(pidx);
    }

    /// epry "take" (slot): pick the equipped item back onto the cursor.
    fn epry_take(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
        let ep = args.first().and_then(|a| a.as_int()).unwrap_or(-1);
        if !(0..16).contains(&ep) {
            return;
        }
        let Some(&pidx) = self.world.by_session.get(&sid) else {
            return;
        };
        if self
            .sessions
            .get(&sid)
            .map(|o| o.cursor.is_some())
            .unwrap_or(true)
        {
            return; // hand already full
        }
        let Some(stack) = self.world.players[pidx].equip[ep as usize].take() else {
            return; // empty slot
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.cursor = Some(stack);
        self.send_epry_state(sid);
        self.stream_equipment_change(pidx);
        self.sync_cursor_widget(sid);
    }

    /// Broadcast one player's equipment change to every viewer: the
    /// world drawable (OD_LAYERS) re-streams with the piece layers and
    /// the doll attribute (OD_AVATAR) recomposites on the owner.
    fn stream_equipment_change(&mut self, pidx: usize) {
        let gob = self.world.players[pidx].gob;
        let Some(slot) = self.world.gobs.get(gob) else {
            return;
        };
        self.stream_pose(slot);
        self.stream_avatar(slot);
    }

    // ------------------------------------------------------------------
    // Crop farming (docs/mechanics/livestock/farming-and-plants.md)
    // ------------------------------------------------------------------

    /// Inventory "take": move one stack onto the cursor. The client then
    /// aims with the mouse; a map click arrives as mapview `itemact`.
    fn inv_take(&mut self, sid: SessionId, wid: u16) {
        let stack_idx = self
            .sessions
            .get(&sid)
            .and_then(|o| o.item_wids.get(&wid).copied());
        let Some(stack_idx) = stack_idx else {
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        if out.cursor.is_some() {
            return; // one cursor item at a time
        }
        let Some(stack) = self
            .world
            .player(sid)
            .and_then(|p| p.inv.get(stack_idx).copied())
        else {
            return;
        };
        if let Some(p) = self.world.player_mut(sid) {
            p.inv.remove(stack_idx);
        }
        out.cursor = Some(stack);
        self.refresh_inventory(sid);
        self.sync_cursor_widget(sid);
    }

    /// MapView `itemact(cc0, mc, modflags[, gobid, gobrc])`: the player
    /// clicked the map with an item on the cursor. cc0 is a screen
    /// coordinate; the world-space target is the second coord (mc).
    fn on_map_itemact(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
        let mc = args.iter().filter_map(|a| a.as_coord()).nth(1);
        let Some((mx, my)) = mc else { return };
        let Some(cursor) = self.sessions.get(&sid).and_then(|o| o.cursor) else {
            return;
        };
        let label = cursor.label;
        // Gob-targeted itemact (client sends [cc, mc, modflags, gobid,
        // gobrc] when the click lands on a gob; MapView.iteminteract):
        // plans sink the held material, stations take fuel or input.
        if let Some(gob) = args.get(3).and_then(|a| a.as_int()) {
            if self.world.plans.contains_key(&gob) {
                self.sink_material(sid, gob, cursor);
                self.sync_cursor_widget(sid);
                return;
            }
            if self.world.stations.contains_key(&gob) {
                self.station_itemact(sid, gob, cursor);
                self.sync_cursor_widget(sid);
                return;
            }
            // Food Trough (session 48): fodder deliveries top up the
            // store. Cross-node troughs are an open policy (like guest
            // quell) - the local authority owns its own troughs.
            if self.world.troughs.contains_key(&gob) {
                self.trough_itemact(sid, gob, cursor);
                self.sync_cursor_widget(sid);
                return;
            }
            // Guest station (session 33): fuel/input delivery relays the
            // held stack to the station's authority. The cursor stack is
            // NOT consumed before the ack (seed-safe): the authority
            // answers StationItemAck and the home node consumes exactly
            // one unit on FuelAdded/InputLoaded, keeping the whole stack
            // on every refusal - parity with the local refusal paths.
            if let Some(crate::nodes::GuestKind::Static {
                class: crate::nodes::StaticClass::Station,
                ..
            }) = self.world.guests.get(&gob).map(|g| &g.kind)
            {
                let res_name = self
                    .world
                    .res
                    .name(cursor.res)
                    .unwrap_or("gfx/invobjs/stone")
                    .to_owned();
                let player_gob = self.world.player(sid).map(|p| p.gob);
                let pos = self.world.guests.get(&gob).map(|g| g.pos);
                if let (Some(player_gob), Some(pos), Some(c)) =
                    (player_gob, pos, self.cluster.as_ref())
                {
                    let authority = self.cell_owner(crate::visidx::cell_of(pos.0, pos.1));
                    c.mesh.send(
                        authority,
                        crate::nodes::NodeMsg::RelayStationItem {
                            player: player_gob,
                            target: gob,
                            stack: crate::nodes::StaticStack {
                                res: res_name,
                                count: 1,
                                ql: cursor.ql,
                                label: cursor.label.to_owned(),
                            },
                        },
                    );
                    debug!(sid, gob, authority, "relay station item sent");
                }
                return;
            }
            // Fall through to the map-space behaviors below for other
            // gob kinds (legacy iteminteract semantics).
        }
        match farm::spec_by_seed_label(label) {
            Some(spec) => {
                self.plant_seed(sid, spec, Self::tile_coord(mx, my), cursor);
                self.sync_cursor_widget(sid);
            }
            None => {
                // Not a seed: legacy map click with a cursor item drops it.
                let pos = self
                    .world
                    .player(sid)
                    .and_then(|p| self.world.gobs.get(p.gob))
                    .map(|slot| self.world.gobs.pos[slot]);
                if let Some(pos) = pos {
                    let name = self
                        .world
                        .res
                        .name(cursor.res)
                        .unwrap_or("gfx/invobjs/stone")
                        .to_owned();
                    self.spawn_drop_near(pos, leak_static(&name), cursor.ql, cursor.label);
                    if let Some(out) = self.sessions.get_mut(&sid) {
                        out.cursor = None;
                    }
                }
                self.sync_cursor_widget(sid);
            }
        }
    }

    /// MapView `drop(modflags)`: release the held stack onto the ground
    /// near the player (Item.java drag release onto the map target;
    /// legacy spawns a ground gob). The drag widget is destroyed and the
    /// cursor cleared; the dropped gob follows the normal pickup/despawn
    /// path (`Kind::Drop`).
    fn on_map_drop(&mut self, sid: SessionId) {
        let Some(cursor) = self.take_cursor_stack(sid) else {
            return;
        };
        self.hide_cursor_widget(sid);
        let pos = self
            .world
            .player(sid)
            .and_then(|p| self.world.gobs.get(p.gob))
            .map(|slot| self.world.gobs.pos[slot]);
        if let Some(pos) = pos {
            let name = self
                .world
                .res
                .name(cursor.res)
                .unwrap_or("gfx/invobjs/stone")
                .to_owned();
            self.spawn_drop_near(pos, leak_static(&name), cursor.ql, cursor.label);
            info!(
                sid,
                label = cursor.label,
                count = cursor.count,
                "ground drop"
            );
        }
        self.refresh_inventory(sid);
    }

    /// Remove the cursor stack from the session (widget not touched;
    /// callers own the drag-widget sync).
    fn take_cursor_stack(&mut self, sid: SessionId) -> Option<InvStack> {
        self.sessions.get_mut(&sid).and_then(|o| o.cursor.take())
    }

    /// Consume ONE unit from the cursor stack (relay ack path): a
    /// multi-unit stack keeps the rest, an exhausted stack clears the
    /// cursor. Inventory refresh + drag-widget sync match the local
    /// itemact path so the client UI never sees a stale cursor.
    fn consume_cursor_unit(&mut self, sid: SessionId) {
        if let Some(cursor) = self.sessions.get_mut(&sid).and_then(|o| o.cursor.as_mut()) {
            cursor.count = cursor.count.saturating_sub(1);
            let rest = cursor.count;
            if rest == 0 {
                self.sessions.get_mut(&sid).expect("cursor above").cursor = None;
            }
        }
        self.refresh_inventory(sid);
        self.sync_cursor_widget(sid);
    }

    /// Widget id of this session's mapview, if created.
    fn mapview_wid(out: &SessionOut) -> Option<u16> {
        out.widgets
            .iter()
            .find(|(_, t)| t.as_str() == "mapview")
            .map(|(id, _)| *id)
    }

    /// Widget id of this session's speedget, if created.
    fn speedget_wid(&self, sid: SessionId) -> Option<u16> {
        self.sessions
            .get(&sid)?
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "speedget")
            .map(|(id, _)| *id)
    }

    /// Item right-click (`iact`): open a flower menu for foods.
    fn on_item_iact(&mut self, sid: SessionId, wid: u16) {
        let stack_idx = self
            .sessions
            .get(&sid)
            .and_then(|o| o.item_wids.get(&wid).copied());
        let Some(stack_idx) = stack_idx else {
            return;
        };
        let label = self
            .world
            .player(sid)
            .and_then(|p| p.inv.get(stack_idx))
            .map(|s| s.label)
            .unwrap_or("");
        if label.is_empty() || self.fep.get(label).is_none() {
            debug!(sid, label, "iact on non-food item: no menu");
            return;
        }
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        // One flower menu at a time per session.
        if let Some((old, _)) = out.item_menu {
            out.send(wdg::dst_wdg(old));
        }
        let w = out.new_wid("sm");
        out.send(wdg::new_wdg(
            w,
            "sm",
            -1,
            -1,
            0,
            &[ListVal::S("Eat".to_owned())],
        ));
        out.item_menu = Some((w, stack_idx));
    }

    /// Flower menu petal click: `cl <i>`; petal 0 confirms.
    fn on_flower_choice(&mut self, sid: SessionId, wid: u16, choice: i32) {
        // Player party menus take precedence (opened last, one menu at a
        // time per session; the openers close any earlier menu).
        let player_menu = self
            .sessions
            .get(&sid)
            .and_then(|o| o.player_menu)
            .map(|(w, _)| w);
        if player_menu == Some(wid) {
            self.on_party_menu_choice(sid, wid, choice);
            return;
        }
        // Crop harvest menus take precedence over the item eat menu.
        let crop_menu = self
            .sessions
            .get(&sid)
            .and_then(|o| o.crop_menu)
            .map(|(w, _)| w);
        if crop_menu == Some(wid) {
            self.harvest_crop(sid, wid, choice);
            return;
        }
        // Tamed-animal production menus (session 47): Milk / Shear.
        let animal_menu = self
            .sessions
            .get(&sid)
            .and_then(|o| o.animal_menu)
            .map(|(w, _)| w);
        if animal_menu == Some(wid) {
            self.apply_animal_choice(sid, wid, choice);
            return;
        }
        // Station Light/Extinguish menus.
        let station_menu = self
            .sessions
            .get(&sid)
            .and_then(|o| o.station_menu)
            .map(|(w, _, _)| w);
        if station_menu == Some(wid) {
            self.apply_station_choice(sid, wid, choice);
            return;
        }
        let pending = self
            .sessions
            .get(&sid)
            .and_then(|o| o.item_menu)
            .filter(|(w, _)| *w == wid);
        let Some((_, stack_idx)) = pending else {
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.item_menu = None;
        out.send(wdg::dst_wdg(wid));
        if choice != 0 {
            // Confirm the cancel client-side (FlowerMenu.uimsg "cancel").
            out.send(wdg::wdgmsg(wid, "cancel", &[]));
            return;
        }
        out.send(wdg::wdgmsg(wid, "act", &[ListVal::I(0)]));
        self.eat_item(sid, stack_idx);
    }

    /// Apply one unit of food: energy fill, FEP grant, HHP healing,
    /// attribute gain on reaching the requirement (fandom FEP loop).
    fn eat_item(&mut self, sid: SessionId, stack_idx: usize) {
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return;
        };
        let Some(stack) = self.world.players[pidx].inv.get(stack_idx).copied() else {
            return;
        };
        let Some(feps) = self.fep.get(stack.label) else {
            debug!(sid, label = stack.label, "eat: no fep entry");
            return;
        };
        let total_fep: f32 = feps
            .iter()
            .filter(|(a, _)| *a != FepAttr::Hhp)
            .map(|(_, v)| v)
            .sum();
        let hhp: f32 = feps
            .iter()
            .find(|(a, _)| *a == FepAttr::Hhp)
            .map(|(_, v)| *v)
            .unwrap_or(0.0);
        // Consume one unit, then roll the attribute draw (single world
        // borrow at a time; the RNG lives on World).
        let roll = self.world.next_ai_rand(1_000_000) as u32;
        {
            let inv = &mut self.world.players[pidx].inv;
            inv[stack_idx].count -= 1;
            if inv[stack_idx].count == 0 {
                inv.remove(stack_idx);
            }
        }
        let p = &mut self.world.players[pidx];
        // Energy fill: server policy (legacy per-food fill unknown,
        // food-and-fep.md open question 1) scaled by the food's FEP total.
        let fill = (10.0f32 + total_fep * 1.5).min(60.0) as i32;
        p.energy = (p.energy + fill).min(100);
        // HHP heals the hard pool directly (fep.conf semantics, doc note 10).
        if hhp > 0.0 {
            p.hp = (p.hp + hhp.round() as i32).min(100);
        }
        // Grant FEPs (tenths), then check the attribute requirement.
        p.fep.grant(feps, stack.ql);
        let cap = p
            .attrs
            .iter()
            .filter(|(k, _)| crate::craft::FepAttr::from_key(&k.to_uppercase()).is_some())
            .map(|(_, v)| *v)
            .max()
            .unwrap_or(10);
        // Pre-rolled weighted draw (single call in pick_gain).
        let mut rng = || roll;
        if p.fep.total() >= cap * 10 {
            if let Some(gain) = p.fep.pick_gain(&mut rng) {
                *p.attrs.entry(gain.to_owned()).or_insert(10) += 1;
                info!(sid, attr = gain, "attribute raised by food");
            }
            p.fep.reset();
        }
        self.refresh_inventory(sid);
        self.push_food_msg(sid);
        self.push_cattr(sid);
        info!(sid, label = stack.label, fill, "ate food");
    }

    /// Push the `food` uimsg on the chr widget: cap in tenths, then
    /// (id, tenths, color) triples (CharWnd.FoodMeter.update contract).
    fn push_food_msg(&mut self, sid: SessionId) {
        let chr_wid = match self.sessions.get(&sid).and_then(|o| o.chr_window()) {
            Some(w) => w,
            None => return,
        };
        let Some(p) = self.world.player(sid) else {
            return;
        };
        let cap = p
            .attrs
            .iter()
            .filter(|(k, _)| crate::craft::FepAttr::from_key(&k.to_uppercase()).is_some())
            .map(|(_, v)| *v)
            .max()
            .unwrap_or(10)
            * 10;
        let mut entries: Vec<(&'static str, i32)> =
            p.fep.acc.iter().map(|(k, v)| (*k, *v)).collect();
        entries.sort_unstable_by_key(|(k, _)| *k);
        let mut args: Vec<ListVal> = vec![ListVal::I(cap)];
        for (id, tenths) in entries {
            let attr = FepAttr::from_key(&id.to_uppercase()).unwrap_or(FepAttr::Str);
            let (r, g, b, a) = attr.color();
            args.push(ListVal::S(id.to_owned()));
            args.push(ListVal::I(tenths));
            args.push(ListVal::Col(r, g, b, a));
        }
        if let Some(out) = self.sessions.get_mut(&sid) {
            out.send(wdg::wdgmsg(chr_wid, "food", &args));
        }
    }

    /// Open the character sheet window (`chr`) and feed its FEP bar.
    fn open_char_sheet(&mut self, sid: SessionId) {
        // CharWnd attributes must precede the `chr` newwidget: the client
        // constructor dereferences glob.cattr.get(name) for every listed
        // attribute and crashes on the first missing one.
        let attr_entries = self.char_attr_snapshot(sid);
        let wid = {
            let Some(out) = self.sessions.get_mut(&sid) else {
                return;
            };
            if let Some(w) = out.chr_window() {
                w
            } else {
                let w = out.new_wid("chr");
                out.send(wdg::cattr(&attr_entries));
                out.send(wdg::new_wdg(w, "chr", 30, 30, 0, &[]));
                w
            }
        };
        // LP balance + skill lists ride the sheet every time it opens
        // (the client prices purchases against the pushed exp balance).
        self.push_lp_msgs(sid);
        let _ = wid;
        self.push_food_msg(sid);
    }

    // ------------------------------------------------------------------
    // Simulation tick
    // ------------------------------------------------------------------

    fn tick(&mut self) {
        self.world.tick += 1;
        self.world.perf.move_blocks = 0;
        self.world.perf.move_cells = 0;
        self.world.perf.start_blocks = 0;
        self.world.perf.fx_batch_n = 0;
        self.world.perf.mv_path_us = 0;
        self.world.perf.mv_viewers_us = 0;
        self.world.perf.mv_pose_us = 0;
        self.world.perf.mv_calls = 0;
        self.world.perf.ix_cand_n = 0;
        // Cluster character migrations: re-broadcast unanswered queries on
        // a fixed cadence (a link still negotiating buffers the retry and
        // answers once the mesh converges); past the deadline, enter with
        // local state (fresh spawn) - logins never wedge on a peer.
        if !self.pending_joins.is_empty() {
            let now = std::time::Instant::now();
            let tick = self.world.tick;
            let mut expired: Vec<(SessionId, String)> = Vec::new();
            let mut retries: Vec<String> = Vec::new();
            for (sid, join) in self.pending_joins.iter_mut() {
                if join.deadline <= now {
                    expired.push((*sid, join.chosen.clone()));
                } else if tick >= join.next_retry {
                    join.next_retry = tick + CHAR_QUERY_RETRY_TICKS;
                    retries.push(crate::persist::save_key(&join.account, &join.chosen));
                }
            }
            if !retries.is_empty() {
                if let Some(c) = self.cluster.as_ref() {
                    for name in retries {
                        debug!(%name, "char query retry");
                        c.mesh.broadcast_except(
                            c.nodes.get(),
                            c.me,
                            crate::nodes::NodeMsg::CharQuery { from: c.me, name },
                        );
                    }
                }
            }
            for (sid, chosen) in expired {
                self.pending_joins.remove(&sid);
                info!(sid, %chosen, "char migration timed out: entering without snapshot");
                self.enter_world_inner(sid, chosen, false);
            }
        }
        // Movement clock: all LinMove progress math anchors to this world
        // time (deterministic across ticks; no wall-clock dependence).
        self.world.now_ms = self.world.tick * TICK_MS;
        // Per-phase attribution keeps the data-oriented hot loops honest:
        // regressions show up in the phase histogram, not just the total.
        let mut phase_us = [0u128; 9];
        let t0 = Instant::now();
        self.tick_movement();
        phase_us[0] = t0.elapsed().as_micros();
        let t1 = Instant::now();
        self.tick_animals();
        phase_us[1] = t1.elapsed().as_micros();
        let t2 = Instant::now();
        self.tick_combat();
        phase_us[2] = t2.elapsed().as_micros();
        let t3 = Instant::now();
        self.tick_vitals();
        phase_us[3] = t3.elapsed().as_micros();
        let t4 = Instant::now();
        self.update_visibility();
        phase_us[4] = t4.elapsed().as_micros();
        // Farming scheduler (crop growth, tilth decay) is a cheap scan of
        // the live crop set only; no work with an empty map.
        let t5 = Instant::now();
        self.tick_farming();
        phase_us[5] = t5.elapsed().as_micros();
        // Production stations: bounded by the live station set.
        let t6 = Instant::now();
        self.tick_stations();
        phase_us[6] = t6.elapsed().as_micros();
        // Cluster maintenance (subs/abroad/transfer/GC) + guest movement
        // interpolation: both no-op without a cluster configuration.
        let t7 = Instant::now();
        self.tick_cluster();
        phase_us[7] = t7.elapsed().as_micros();
        let t8 = Instant::now();
        self.tick_guests();
        phase_us[8] = t8.elapsed().as_micros();
        // Criminal-flag expiry: a rare-event O(players) scan kept out of
        // the phase histogram (it is empty in the steady state).
        self.tick_criminal_expiry();
        // Leash-break sweep (session 45): a rare-event scan of the tame
        // table (empty in the steady state; only beasts mid-taming live
        // here). Runs before the batch fan-out so the follow-removal
        // blocks ride the same packed datagram.
        if !self.world.tamed.is_empty() {
            let tick = self.world.tick;
            let mut broke: Vec<GobId> = Vec::new();
            for (&id, tame) in self.world.tamed.iter() {
                if tame.break_at_tick != 0 && tick >= tame.break_at_tick {
                    broke.push(id);
                }
            }
            for id in broke {
                self.break_leash(id, "and re-attacks");
            }
        }
        // Production sweep (session 47; session 48 adds the Food Trough;
        // animals-and-husbandry.md "Feeding: troughs and grazing" +
        // "Animal products and collection flows"): fully tamed cows
        // accrue milk, fully tamed sheep accrue wool, but only while
        // FED - a trough with fodder inside the 18-tile radius wins
        // over grazing (the doc's "animals inside a trough's radius
        // prefer the trough over grazing"), grazing tiles (moor, heath,
        // grassland) are the fallback. Feeding drains the trough at the
        // Legacy:Cattle rates; an unfed animal starves (session 48
        // policy: death after 3 in-game days without a bite). Same
        // rare-event shape as the leash sweep: O(tamed) with the steady
        // state empty; runs each tick so the meters keep game-time
        // based pacing (doc: "lagg-relative" wording -> game time).
        if !self.world.tamed.is_empty() {
            // Phase A0 (immutable): collect fully tamed producers with
            // species + position.
            let candidates: Vec<(GobId, Species, (i32, i32))> = self
                .world
                .tamed
                .iter()
                .filter_map(|(&id, tame)| {
                    // Only fully tamed domestic producers feed and
                    // produce; mid-taming beasts still run the leash
                    // protocol (wild animals forage on their own).
                    if tame.tameness < crate::state::TAMENESS_FULL {
                        return None;
                    }
                    let slot = self.world.gobs.get(id)?;
                    let Kind::Animal { species } = self.world.gobs.kind[slot] else {
                        return None;
                    };
                    if !matches!(species, Species::Cow | Species::Sheep) {
                        return None;
                    }
                    Some((id, species, self.world.gobs.pos[slot]))
                })
                .collect();
            // Phase A1 (mutable): feeding resolution. The trough scan
            // is O(troughs) per producer (both populations are small);
            // tile_at may generate a grid on demand, so tile reads run
            // here too - after the tamed iteration ended (two-phase
            // pass, same shape as tick_animals).
            let mut fed: Vec<(GobId, Species)> = Vec::new();
            let mut starved: Vec<GobId> = Vec::new();
            for (id, species, pos) in candidates {
                // 1. Trough preference: the nearest trough with fodder
                //    inside TROUGH_RADIUS (18 tiles = 198 subtiles,
                //    euclidean over subtile coords).
                let mut best: Option<(GobId, i64)> = None;
                for (&tid, trough) in self.world.troughs.iter() {
                    if trough.units == 0 {
                        continue;
                    }
                    let Some(tslot) = self.world.gobs.get(tid) else {
                        continue;
                    };
                    let (tx, ty) = self.world.gobs.pos[tslot];
                    let (dx, dy) = (i64::from(tx - pos.0), i64::from(ty - pos.1));
                    let dist_sq = dx * dx + dy * dy;
                    if dist_sq <= i64::from(crate::state::TROUGH_RADIUS_SQ)
                        && best.is_none_or(|(_, bd)| dist_sq < bd)
                    {
                        best = Some((tid, dist_sq));
                    }
                }
                let rate_nano = match species {
                    Species::Cow => {
                        // Lactating surcharge (0.1 unit per L produced):
                        // this cow produces milk while fed, so the
                        // surcharge applies in the same tick.
                        crate::state::COW_EAT_NANO_PER_TICK + crate::state::LACTATE_NANO_PER_TICK
                    }
                    _ => crate::state::SHEEP_EAT_NANO_PER_TICK,
                };
                if let Some((tid, _)) = best {
                    // Drain whole units as the accumulator crosses one;
                    // the fractional part stays banked (persisted).
                    let Some(tame) = self.world.tamed.get_mut(&id) else {
                        continue;
                    };
                    tame.feed_acc_nano += rate_nano;
                    if tame.feed_acc_nano >= 1_000_000_000 {
                        let want = (tame.feed_acc_nano / 1_000_000_000) as u32;
                        let got = self
                            .world
                            .troughs
                            .get_mut(&tid)
                            .map(|t| t.take(want))
                            .unwrap_or(0);
                        tame.feed_acc_nano -= u64::from(got) * 1_000_000_000;
                    }
                    tame.hunger = 0;
                    fed.push((id, species));
                    continue;
                }
                // 2. Grazing fallback: the standing tile counts as
                //    quality-10 food (tile_at takes SUBTILE coordinates
                //    and does the per-tile division itself).
                let grazing = self
                    .tile_at(pos)
                    .map(crate::state::tile_grazes)
                    .unwrap_or(false);
                let Some(tame) = self.world.tamed.get_mut(&id) else {
                    continue;
                };
                if grazing {
                    tame.hunger = 0;
                    fed.push((id, species));
                } else {
                    // 3. Starvation: no trough fodder, no grazing tile.
                    //    Production is gated on feeding below, so the
                    //    doc's "kill or stop production" reduces to the
                    //    death timer here.
                    tame.hunger = tame.hunger.saturating_add(1);
                    if tame.hunger >= crate::state::STARVE_DEATH_TICKS {
                        starved.push(id);
                    }
                }
            }
            // Phase B (mutable): accrue the meters for fed animals.
            for (id, species) in fed {
                let Some(tame) = self.world.tamed.get_mut(&id) else {
                    continue;
                };
                match species {
                    Species::Cow => {
                        tame.prod_acc = tame.prod_acc.saturating_add(crate::state::MILK_QUANTITY);
                        while tame.prod_acc >= crate::state::MILK_ACC_PER_UNIT
                            && tame.milk_units < crate::state::MILK_CAP_UNITS
                        {
                            tame.prod_acc -= crate::state::MILK_ACC_PER_UNIT;
                            tame.milk_units += 1;
                        }
                        // At the cap the accumulator stops banking time:
                        // production resumes from zero after milking.
                        if tame.milk_units >= crate::state::MILK_CAP_UNITS {
                            tame.prod_acc = 0;
                        }
                    }
                    Species::Sheep => {
                        tame.prod_acc = tame.prod_acc.saturating_add(crate::state::WOOL_QUANTITY);
                        while tame.prod_acc >= crate::state::WOOL_ACC_PER_UNIT
                            && tame.wool < crate::state::WOOL_CAP
                        {
                            tame.prod_acc -= crate::state::WOOL_ACC_PER_UNIT;
                            tame.wool += 1;
                        }
                        if tame.wool >= crate::state::WOOL_CAP {
                            tame.prod_acc = 0;
                        }
                    }
                    _ => {}
                }
            }
            // Phase C (terminal): starvation deaths. Same teardown
            // shape as the damage path (kill, retract, drop the fight
            // rows); no corpse and no loot (the corpse pipeline is not
            // implemented - server policy, documented in the doc).
            for id in starved {
                self.starve_kill(id);
            }
        }
        // The dirty set served this tick's visibility pass; spawn marks
        // after this point (farming/station drops) dirty the next pass.
        self.world.gobs.vis.clear_dirty();
        // Session 44: fan out the tick's accumulated LINBEG starts and FX
        // overlays (chase, hit tails, click handling) through the packed
        // cell-indexed batch - ONE combined datagram per session per tick,
        // encoded once per block. Visibility is fresh from the pass above,
        // so the fan-out filter sees the exact post-update view. The batch
        // MUST be cleared after the fan-out: the restore keeps its
        // capacity for reuse, and stale blocks would be re-sent (and
        // re-fanned) every following tick - an O(tick^2) datagram
        // explosion measured at +100 ms/tick before this fix.
        if !self.start_scratch.is_empty() {
            self.world.perf.start_blocks = self.start_scratch.len() as u64;
            let mut batch = std::mem::take(&mut self.start_scratch);
            self.broadcast_batch(&batch);
            batch.clear();
            self.start_scratch = batch;
        }
        let perf = &mut self.world.perf;
        perf.active_sessions = self.sessions.len();
        perf.phase_us = phase_us;
        // Exponential moving average keeps a stable steady-state number.
        perf.mean_tick_us = if perf.mean_tick_us == 0 {
            perf.last_tick_us as u64
        } else {
            (perf.mean_tick_us * 49 + perf.last_tick_us as u64) / 50
        };
    }

    fn tick_movement(&mut self) {
        let now = self.world.now_ms;
        // Encoded OD blocks for this tick's progress and finalization go
        // into the packed cell-indexed batch (see `move_batch` and
        // `broadcast_batch`): blocks are encoded once, then each session
        // probes only the non-empty cells - the O(sessions x movers)
        // per-session scan this replaces dominated the tick budget at the
        // 400+ mover scale (68 ms of movement phase measured at 426
        // players) and re-appeared at the duel-cohort load scale.
        let mut batch = std::mem::take(&mut self.move_scratch);
        batch.clear();
        let mut finished: Vec<(usize, i32, i32)> = Vec::new();
        for slot in 0..self.world.gobs.alive.len() {
            if !self.world.gobs.alive[slot] {
                continue;
            }
            // Cluster: foreign-authority gobs do not move here (their owner
            // simulates them; locally they are guests advanced by
            // tick_guests). Single-node: always true, branch predicts.
            if !self.is_authority_slot(slot) {
                continue;
            }
            let Some(lm) = self.world.gobs.mv[slot] else {
                continue;
            };
            // Active mover: keep its cell dirty so viewers receive LINSTEP
            // progress and boundary exits are caught.
            self.world
                .gobs
                .vis
                .mark_mover(gob_id_from_slot(slot, self.world.gobs.gen[slot]));
            let elapsed = now.saturating_sub(lm.started_ms);
            if elapsed >= u64::from(lm.total_ms) {
                // Move finished: the client's own interpolation already
                // rests at the destination (a = 1); the final LINSTEP with
                // l >= c removes the client Moving attribute there.
                finished.push((slot, lm.tx, lm.ty));
            } else {
                // Interpolated logical position: visibility scans, combat
                // reach, and re-click retargeting all see the on-path
                // position, never the destination ahead of time.
                let (cx, cy) = lm.pos_at(now);
                self.world.gobs.set_pos(slot, (cx, cy));
                let l = lm.step_at(now);
                if l > lm.step {
                    let id = gob_id_from_slot(slot, self.world.gobs.gen[slot]);
                    let frame = self.world.gobs.frame[slot];
                    self.world.gobs.mv[slot] = Some(LinMove { step: l, ..lm });
                    // Progress frames ship on the LINSTEP_EVERY_TICKS
                    // cadence (see tick_guests); the client interpolates
                    // locally between corrections.
                    if self.world.tick.is_multiple_of(LINSTEP_EVERY_TICKS) {
                        // Block layout: [fl][id i32][frame i32][ops..OD_END]
                        // - NO per-block MSG header. The fan-out datagram
                        // carries ONE MSG_OBJDATA type byte followed by
                        // consecutive blocks (Session.getobjdata loops
                        // exactly this shape).
                        let mut m = MessageBuf::new();
                        m.uint8(0)
                            .int32(id)
                            .int32(frame as i32)
                            .uint8(OD_LINSTEP)
                            .int32(l)
                            .uint8(OD_END);
                        batch.push(
                            id,
                            frame,
                            crate::visidx::cell_of(cx, cy),
                            false,
                            &m.finish(),
                        );
                    }
                }
            }
        }
        for (slot, tx, ty) in finished {
            let id = gob_id_from_slot(slot, self.world.gobs.gen[slot]);
            let steps = self.world.gobs.mv[slot].map(|lm| lm.steps).unwrap_or(0);
            self.world.gobs.mv[slot] = None;
            self.world.gobs.frame[slot] += 1;
            self.world.gobs.set_pos(slot, (tx, ty));
            // Pin the destination into the client's gob.rc, THEN remove the
            // Moving attribute: linstep(l >= c) alone would drop Moving and
            // position() would fall back to the STALE pre-move rc - the
            // avatar visibly snapped back to its start point (the measured
            // "walks then rubber-bands home" defect).
            let frame = self.world.gobs.frame[slot];
            let mut m = MessageBuf::new();
            m.uint8(0)
                .int32(id)
                .int32(frame as i32)
                .uint8(OD_MOVE)
                .coord(tx, ty)
                .uint8(OD_LINSTEP)
                .int32(steps)
                .uint8(OD_END);
            batch.push(id, frame, crate::visidx::cell_of(tx, ty), true, &m.finish());
            // Rest pose: the standing set of the current facing (players
            // and animals both composite directional pose parts).
            let dir = self.world.gobs.facing[slot];
            if self.world.gobs.pose_streamed[slot] != dir {
                self.world.gobs.pose_streamed[slot] = dir;
                self.stream_pose(slot);
            }
            // Cluster: publish the arrival to subscribed peers (and the
            // cell owner, for players standing abroad).
            self.publish(id, GuestEv::Update);
        }
        self.broadcast_batch(&batch);
        self.move_scratch = batch;
    }

    /// Movement fan-out for one packed batch: every viewing session
    /// receives ONE combined OBJDATA datagram carrying its visible blocks
    /// (the wire format allows consecutive gob blocks per datagram; the
    /// client's recv_objdata loops them). The session walks only the
    /// batch's non-empty cells and rejects whole cells with one rectangle
    /// test against its 2x-retract-hysteresis square (`FANOUT_SPAN`);
    /// `visible.contains` stays the exact per-block filter. Finalizer
    /// blocks land in `unacked` per gob exactly like the old per-block
    /// path so OBJACK retransmission keeps working; progress frames are
    /// deliberately NOT recorded (each is superseded by the next tick's
    /// frame, a lost datagram self-heals within 100 ms).
    fn broadcast_batch(&mut self, batch: &crate::move_batch::MoveBatch) {
        if batch.is_empty() {
            return;
        }
        self.world.perf.move_blocks += batch.len() as u64;
        self.world.perf.move_cells += batch.cell_count() as u64;
        // Session anchor positions (avatar gob slot -> SoA position).
        // Taken/restored scratch: this runs twice per tick at most (the
        // movement batch mid-tick, the start/FX batch at tick end) and
        // the `sessions` iteration order is arbitrary, so the buffer is
        // just overwritten in place each call.
        let mut sids_pos = std::mem::take(&mut self.fan_scratch);
        sids_pos.clear();
        for (sid, out) in self.sessions.iter() {
            if let Some(pg) = out.player_gob {
                if let Some(slot) = self.world.gobs.get(pg) {
                    sids_pos.push((*sid, self.world.gobs.pos[slot]));
                }
            }
        }
        for (sid, (px, py)) in &sids_pos {
            let Some(out) = self.sessions.get_mut(sid) else {
                continue;
            };
            // Datagram is materialized lazily: sessions with no visible
            // blocks allocate nothing.
            let mut m: Option<MessageBuf> = None;
            for (cell, idxs) in batch.cells() {
                if !crate::move_batch::cell_intersects_axis(cell.0, *px, FANOUT_SPAN)
                    || !crate::move_batch::cell_intersects_axis(cell.1, *py, FANOUT_SPAN)
                {
                    continue;
                }
                for &i in idxs {
                    let (id, frame, fin) = batch.block_info(i);
                    if !out.visible.contains(&id) {
                        continue;
                    }
                    let bytes = batch.block_bytes(i);
                    // One MSG_OBJDATA type byte opens the datagram; the
                    // blocks inside are headerless ([fl][id][frame][ops])
                    // - exactly what Session.getobjdata loops over.
                    let m = m.get_or_insert_with(|| {
                        let mut m = MessageBuf::with_capacity(512);
                        m.uint8(MSG_OBJDATA);
                        m
                    });
                    match batch.block_patch(i) {
                        Some(patch) => {
                            // Session-local wire ids: resolve the name from
                            // the game-global table, allocate the session
                            // wire id (first use also queues the RMSG_RESID
                            // announcement), rewrite the placeholder bytes,
                            // ship the per-session copy.
                            let mut patched = bytes.to_vec();
                            for (global, off) in patch.entries() {
                                if let Some(name) = self.world.res.name(*global) {
                                    let w = out.res.wire_named(*global, name);
                                    if let Some((rn, rv)) = out.res.pending_announce(w) {
                                        out.send(crate::resources::wdg::resid(w, rn, rv));
                                        out.res.mark_announced(w);
                                    }
                                    let o = *off as usize;
                                    patched[o..o + 2].copy_from_slice(&w.to_le_bytes());
                                }
                            }
                            m.bytes(&patched);
                            if fin {
                                Self::record_unacked(out, id, frame, patched);
                            }
                        }
                        None => {
                            m.bytes(bytes);
                            if fin {
                                Self::record_unacked(out, id, frame, bytes.to_vec());
                            }
                        }
                    }
                }
            }
            if let Some(m) = m {
                out.send_raw(m.finish());
            }
        }
        self.fan_scratch = sids_pos;
    }

    /// Record an OBJDATA block for per-gob retransmission. The per-gob
    /// map is CAPPED at the last 4 frames: sessions that never OBJACK
    /// (load bots, slow clients mid-lag) would otherwise grow it without
    /// bound - measured OOM driver at the 1000-session scale (~40 MB/s of
    /// finalizer blocks before the cap).
    fn record_unacked(out: &mut SessionOut, id: GobId, frame: u32, block: Vec<u8>) {
        const UNACKED_CAP: usize = 4;
        let per = out.unacked.entry(id).or_default();
        per.insert(frame, block);
        while per.len() > UNACKED_CAP {
            let min = match per.keys().copied().min() {
                Some(f) => f,
                None => break,
            };
            per.remove(&min);
        }
    }

    /// One-shot FX overlay broadcast: adds `res_name` (e.g. gfx/fx/bite)
    /// as a NON-persistent overlay on the gob for every current viewer.
    /// The client removes the overlay itself once the resource's animation
    /// completes one cycle (Gob.ctick drops a finished non-persistent
    /// overlay), so no deletion message is ever needed.
    ///
    /// Session 44: the block encodes ONCE into the packed start batch
    /// (with the session-local wire id left as a patch placeholder - the
    /// fan-out rewrites it per session and first-announces the resource
    /// there). The old per-viewer encode/clone loop spent its budget on
    /// repeated encoding and one allocation per viewer.
    fn fx_overlay_broadcast(&mut self, id: GobId, res_name: &'static str) {
        let Some(slot) = self.world.gobs.get(id) else {
            return;
        };
        let frame = self.world.gobs.frame[slot];
        let frame_i32 = frame as i32;
        let gi = self.world.res.intern(res_name);
        let (px, py) = self.world.gobs.pos[slot];
        self.overlay_seq = self.overlay_seq.wrapping_add(1);
        // Wire id: bit 0 = the persist flag (0 = one-shot), the rest is the
        // client-side overlay id (15-bit sequence keeps it comfortably
        // positive).
        let olid = ((self.overlay_seq & 0x7FFF) << 1) as i32;
        // Encode with the global index as the wire placeholder; record the
        // byte offset of that uint16 so the fan-out can patch it per
        // session. Headerless block layout: [fl][id(4)][frame(4)]
        // [OD_OVERLAY][olid(4)][wire(2) <- patch offset][OD_END].
        let patch_off = 1 + 4 + 4 + 1 + 4;
        let mut m = MessageBuf::new();
        m.uint8(0)
            .int32(id)
            .int32(frame_i32)
            .uint8(OD_OVERLAY)
            .int32(olid)
            .uint16(gi)
            .uint8(OD_END);
        self.start_scratch.push_patched(
            id,
            frame,
            crate::visidx::cell_of(px, py),
            true,
            Some(crate::move_batch::Patch::One {
                slot: [(gi, patch_off)],
            }),
            &m.finish(),
        );
        self.world.perf.fx_batch_n += 1;
    }

    /// Resolve and stream one composited-drawable pose (OD_LAYERS) for
    /// the gob at `slot`. The layer set derives from the gob kind +
    /// current pose state (moving -> walking set of `facing`, standing
    /// set otherwise; players 6 parts, animals 1 part). No frame
    /// streaming: each directional resource embeds its own animation, so
    /// this fires only on pose/direction CHANGES.
    ///
    /// Session 44: the block encodes ONCE with every wire id as a global
    /// index placeholder (Patch::Many) and fans out through the packed
    /// start batch at tick end; the fan-out resolves each session's wire
    /// ids, first-announces unseen resources, and lands the patched
    /// block in `unacked`. The old per-viewer encode/announce loop was
    /// the top fan-out cost (840-1370 us/call at the 1000-bot scale).
    fn stream_pose(&mut self, slot: usize) {
        let id = gob_id_from_slot(slot, self.world.gobs.gen[slot]);
        let kind = self.world.gobs.kind[slot];
        let moving = self.world.gobs.mv[slot].is_some();
        let facing = self.world.gobs.facing[slot];
        let (base_name, layer_names): (&'static str, Vec<&'static str>) = match kind {
            Kind::Player { player } => {
                let equip = self.player_equip_names(player);
                (
                    AVATAR_BASE,
                    avatar_pose_layers(moving, facing)
                        .iter()
                        .copied()
                        .chain(crate::equip::world_layers(&equip, moving, facing))
                        .collect(),
                )
            }
            Kind::Animal { species } => (
                kritter_base(species),
                vec![kritter_pose_layer(species, moving, facing)],
            ),
            _ => return,
        };
        let base_global = self.world.res.intern(base_name);
        let frame_i32 = self.world.gobs.frame[slot] as i32;
        let (px, py) = self.world.gobs.pos[slot];
        // Headerless block: [fl][id][frame][OD_LAYERS][wire u16 xN][ffff]
        // [ff]; every wire slot is a global-index placeholder recorded as
        // a patch entry (offset 9 = fl+id+frame, then +1 for OD_LAYERS).
        let mut m = MessageBuf::new();
        m.uint8(0).int32(id).int32(frame_i32).uint8(OD_LAYERS);
        let mut entries: Vec<(u16, u32)> = Vec::with_capacity(layer_names.len() + 1);
        entries.push((base_global, m.len() as u32));
        m.uint16(base_global);
        for n in layer_names.iter() {
            let gi = self.world.res.intern(n);
            entries.push((gi, m.len() as u32));
            m.uint16(gi);
        }
        m.uint16(65535).uint8(OD_END);
        let t_p = Instant::now();
        self.start_scratch.push_patched(
            id,
            self.world.gobs.frame[slot],
            crate::visidx::cell_of(px, py),
            true,
            Some(crate::move_batch::Patch::Many { entries }),
            &m.finish(),
        );
        let perf = &mut self.world.perf;
        perf.mv_pose_us += t_p.elapsed().as_micros() as u64;
    }

    /// Stream the Equipment-doll avatar attribute (OD_AVATAR) of the
    /// player gob at `slot` to every viewer: the owner receives the
    /// banzai doll set (+ the equipped pieces' doll layers), other
    /// viewers the standing idle set. Fires on equip/unequip so the doll
    /// recomposites live (Equipory.cdraw re-reads Avatar.rend).
    fn stream_avatar(&mut self, slot: usize) {
        let id = gob_id_from_slot(slot, self.world.gobs.gen[slot]);
        let Kind::Player { player } = self.world.gobs.kind[slot] else {
            return;
        };
        let facing = self.world.gobs.facing[slot];
        let frame_i32 = self.world.gobs.frame[slot] as i32;
        let equip = self.player_equip_names(player);
        let viewers: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&id))
            .map(|(s, _)| *s)
            .collect();
        for v in viewers {
            let Some(out) = self.sessions.get_mut(&v) else {
                continue;
            };
            let own = out.player_gob == Some(id);
            let layers: Vec<&'static str> = if own {
                avatar_doll_layers()
                    .iter()
                    .copied()
                    .chain(crate::equip::doll_layers(&equip))
                    .collect()
            } else {
                avatar_pose_layers(false, facing)
                    .iter()
                    .copied()
                    .chain(crate::equip::world_layers(&equip, false, facing))
                    .collect()
            };
            let mut announces: Vec<Vec<u8>> = Vec::new();
            let mut wire_ids: Vec<u16> = Vec::with_capacity(layers.len());
            for n in &layers {
                let gi = self.world.res.intern(n);
                let w = out.res.wire_named(gi, n);
                if let Some((rn, rv)) = out.res.pending_announce(w) {
                    announces.push(wdg::resid(w, rn, rv));
                    out.res.mark_announced(w);
                }
                wire_ids.push(w);
            }
            for a in announces {
                out.send(a);
            }
            let mut m = MessageBuf::new();
            m.uint8(MSG_OBJDATA).uint8(0).int32(id).int32(frame_i32);
            m.uint8(OD_AVATAR);
            for w in &wire_ids {
                m.uint16(*w);
            }
            m.uint16(65535).uint8(OD_END);
            out.send_raw(m.finish());
        }
    }

    /// The client-visible position of a gob right now: the interpolated
    /// on-path position for movers (LinMove::pos_at, the same math as the
    /// client's LinMove.getc), or the stored position for idle gobs.
    fn interpolated_pos(&self, slot: usize) -> (i32, i32) {
        match self.world.gobs.mv[slot] {
            Some(lm) => lm.pos_at(self.world.now_ms),
            None => self.world.gobs.pos[slot],
        }
    }

    /// World tile at a subtile coordinate (None outside the generated
    /// area; grid loading is deterministic, see GridStore).
    fn tile_at(&mut self, (x, y): (i32, i32)) -> Option<u8> {
        let gc = (x.div_euclid(1100), y.div_euclid(1100));
        let ix = (x.div_euclid(11)).rem_euclid(100) as usize;
        let iy = (y.div_euclid(11)).rem_euclid(100) as usize;
        Some(self.world.grids.grid(gc).tile(ix, iy))
    }

    /// Begin (or retarget) a linear move for the gob at `slot` toward
    /// `target`. Shared by player clicks, animal AI, and combat chase so
    /// every mover uses one timing model (`LinMove::client_steps`) and
    /// one retarget rule: when already moving, the new move starts from
    /// the CURRENTLY INTERPOLATED position - never from the old
    /// destination, which is what teleported the avatar on rapid clicks.
    fn start_move(&mut self, slot: usize, target: (i32, i32)) -> bool {
        // Sub-phase attribution (session 43): the combat chase path pays
        // ~3 ms per start at the 1000-bot scale; these counters split
        // path check, viewer fan-out, pose stream and publish.
        let t0 = Instant::now();
        let (sx, sy) = self.interpolated_pos(slot);
        self.world.gobs.set_pos(slot, (sx, sy));
        let (tx, ty) = (
            target.0.clamp(sx - 5000, sx + 5000),
            target.1.clamp(sy - 5000, sy + 5000),
        );
        if !path_clear(&mut self.world, sx, sy, tx, ty) {
            let perf = &mut self.world.perf;
            perf.mv_path_us += t0.elapsed().as_micros() as u64;
            perf.mv_calls += 1;
            return false;
        }
        let dist = ((tx - sx).abs() + (ty - sy).abs()).max(1);
        let speed = self.world.gobs.speed[slot].max(1);
        // Terrain caps the gait speed (percent of the mover's own speed,
        // read at the starting tile; see map-and-terrain.md).
        let pct = self
            .tile_at((sx, sy))
            .and_then(crate::state::tile_speed_pct)
            .unwrap_or(100);
        let eff = (speed * pct / 100).max(1);
        let total_ms = ((i64::from(dist) * 1000) / i64::from(eff)).clamp(60, 600_000) as u32;
        let steps = LinMove::client_steps(total_ms);
        self.world.gobs.mv[slot] = Some(LinMove {
            sx,
            sy,
            tx,
            ty,
            steps,
            step: 0,
            started_ms: self.world.now_ms,
            total_ms,
        });
        self.world.gobs.frame[slot] += 1;
        let frame = self.world.gobs.frame[slot];
        let id = gob_id_from_slot(slot, self.world.gobs.gen[slot]);
        // Session 44: the LINBEG block is identical for every viewer, so
        // it is encoded ONCE into the packed start batch; the fan-out
        // (cell rectangle + `visible.contains` + one combined datagram
        // per session per tick) happens in `broadcast_batch` at tick end.
        // The old per-viewer encode/clone loop was the measured chase
        // cost (2.4-3.0 ms per start at the 1000-bot scale).
        let t_v = Instant::now();
        {
            let cell = crate::visidx::cell_of(sx, sy);
            // Headerless block: [fl][id][frame][OD_LINBEG][coords][ff] -
            // the fan-out datagram carries the single MSG_OBJDATA type
            // byte (see broadcast_batch).
            let mut m = MessageBuf::new();
            m.uint8(0)
                .int32(id)
                .int32(frame as i32)
                .uint8(OD_LINBEG)
                .coord(sx, sy)
                .coord(tx, ty)
                .int32(steps)
                .uint8(OD_END);
            // fin = true: LINBEG carries the authoritative frame, it
            // lands in `unacked` for OBJACK retransmission exactly like
            // the old per-viewer path did.
            self.start_scratch.push(id, frame, cell, true, &m.finish());
        }
        {
            let perf = &mut self.world.perf;
            perf.mv_viewers_us += t_v.elapsed().as_micros() as u64;
        }
        trace!(id, sx, sy, tx, ty, steps, total_ms, "move started");
        // Face the travel direction and swap to the walking pose set.
        // One layer stream per pose+direction: each directional resource
        // embeds its full walk cycle, so the client animates natively and
        // the server never streams frames. The pose state encodes as
        // (moving<<3 | dir): standing dirs 0..7, walking dirs 8..15 - a
        // single byte dedupes retargets, arrival, and re-facings.
        let dir = move_dir((sx, sy), (tx, ty));
        let walking = 8 + dir;
        self.world.gobs.facing[slot] = dir;
        if self.world.gobs.pose_streamed[slot] != walking {
            self.world.gobs.pose_streamed[slot] = walking;
            let t_p = Instant::now();
            self.stream_pose(slot);
            let perf = &mut self.world.perf;
            perf.mv_pose_us += t_p.elapsed().as_micros() as u64;
        }
        // Cluster: the move start/retarget is a guest update for subscribed
        // peers (and the cell owner, for players standing abroad).
        let id = gob_id_from_slot(slot, self.world.gobs.gen[slot]);
        self.publish(id, GuestEv::Update);
        {
            let perf = &mut self.world.perf;
            perf.mv_calls += 1;
        }
        true
    }

    /// Authority-side application of a relayed planting act (session 31).
    /// The seed physically lives on the home node's cursor (its quality
    /// rides the act); the TILE state (tilth, occupancy) is authoritative
    /// HERE. Same validation order as the local plant_seed path, minus
    /// the skill gate (session state stays on the home node) - reach
    /// parity note: the local path also has no explicit reach check, the
    /// client can only aim inside its own view. Refusals stay silent:
    /// the home node's cursor keeps the seed, matching a local refusal.
    fn relay_plant(&mut self, player: GobId, tx: i32, ty: i32, spec: u8, seed_ql: u8) {
        if spec as usize >= farm::CROPS.len() {
            debug!(tx, ty, spec, "relay plant refused: unknown spec");
            return;
        }
        if !self.world.tilth.contains_key(&(tx, ty)) {
            debug!(tx, ty, "relay plant refused: tile not plowed");
            return;
        }
        if self.world.crop_at.contains_key(&(tx, ty)) {
            debug!(tx, ty, "relay plant refused: tile occupied");
            return;
        }
        let spec_data = &farm::CROPS[spec as usize];
        let res_idx = self.world.res.intern(spec_data.gob_res);
        let now = unix_ms();
        let state = crate::farm::CropState {
            spec,
            stage: 0,
            seed_ql,
            soil_ql: crate::farm::soil_quality(tx, ty),
            next_stage_at: now + farm::stage_duration_ms(spec_data).as_millis() as u64,
        };
        let gob = self.world.gobs.spawn(
            Kind::Crop { spec, stage: 0 },
            (tx * 11 + 5, ty * 11 + 5),
            res_idx,
            1,
            0,
        );
        self.world.crops.insert(gob, state);
        self.world.crop_at.insert((tx, ty), gob);
        // Planting clears the tilth decay timer (legacy quirk, same as
        // the local path).
        self.world.tilth.insert((tx, ty), 0);
        self.broadcast_spawn(gob);
        if let Some(c) = self.cluster.as_ref() {
            let home = self.node_of_gob(player);
            c.mesh
                .send(home, crate::nodes::NodeMsg::PlantAck { player, ok: true });
        }
        info!(gob, tx, ty, spec = spec_data.gob_res, "relay plant applied");
    }

    /// Authority-side application of a relayed plow act (session 32). The
    /// tile's grid state is authoritative HERE: validate against this
    /// node's own grid, mutate (override recorded for persistence), start
    /// the tilth clock, answer PlowAck - the home node drains the stamina
    /// only on the ok ack, exactly like a local plow drains it at act
    /// time - and broadcast TileMutation so every peer holding the grid
    /// renders the furrow. Refusals answer PlowAck ok=false (silent on
    /// the home side, parity with a local refusal).
    fn relay_plow(&mut self, player: GobId, tx: i32, ty: i32) {
        let gc = (tx.div_euclid(100), ty.div_euclid(100));
        let lx = tx.rem_euclid(100) as usize;
        let ly = ty.rem_euclid(100) as usize;
        let tile = self.world.grids.grid(gc).tile(lx, ly);
        if tile != tile::GRASS {
            debug!(tx, ty, tile, "relay plow refused: not grass");
            self.answer_plow(player, false);
            return;
        }
        if self.world.crop_at.contains_key(&(tx, ty)) {
            debug!(tx, ty, "relay plow refused: tile occupied");
            self.answer_plow(player, false);
            return;
        }
        self.mutate_tile_local(gc, lx, ly, tx, ty, tile::PLOWED);
        let now = unix_ms();
        self.world
            .tilth
            .insert((tx, ty), now + crate::farm::tilth_decay_ms());
        self.answer_plow(player, true);
        info!(tx, ty, "relay plow applied");
    }

    /// Unicast PlowAck to the acting player's home node (no-op without a
    /// cluster; the local path never goes through here).
    fn answer_plow(&mut self, player: GobId, ok: bool) {
        if let Some(c) = self.cluster.as_ref() {
            let home = self.node_of_gob(player);
            c.mesh
                .send(home, crate::nodes::NodeMsg::PlowAck { player, ok });
        }
    }

    /// Unicast StationAck to the acting player's home node (session 33;
    /// no-op without a cluster - the local menu path never relays).
    fn answer_station(&mut self, player: GobId, result: crate::nodes::StationResult) {
        if let Some(c) = self.cluster.as_ref() {
            let home = self.node_of_gob(player);
            c.mesh
                .send(home, crate::nodes::NodeMsg::StationAck { player, result });
        }
    }

    /// Authority side of the station menu relay (session 33): the player
    /// homed on the sender chose Light/Extinguish from the snapshot the
    /// HOME node rendered. The station's fuel/input/lit state is
    /// authoritative HERE: validate the act against the local
    /// StationState (a stale act changes nothing and answers Stale),
    /// apply the same transitions as the local menu path, re-render the
    /// lit sprite and re-publish to subscribers.
    fn relay_station_act(&mut self, player: GobId, target: GobId, act: crate::nodes::StationAct) {
        let Some(station) = self.world.stations.get(&target).cloned() else {
            debug!(target, ?act, "relay station act: target gone");
            self.answer_station(player, crate::nodes::StationResult::Stale);
            return;
        };
        match act {
            crate::nodes::StationAct::Extinguish => {
                if !station.lit {
                    // The view said lit but the job already ended (or was
                    // never lit): nothing to extinguish, silent parity.
                    debug!(target, "relay extinguish on an unlit station: stale");
                    self.answer_station(player, crate::nodes::StationResult::Stale);
                    return;
                }
                let st = self
                    .world
                    .stations
                    .get_mut(&target)
                    .expect("BUG: station checked above");
                st.lit = false;
                st.progress = 0;
                self.set_station_lit(target, false);
                self.answer_station(player, crate::nodes::StationResult::Extinguished);
                info!(target, "relay station extinguished");
            }
            crate::nodes::StationAct::Light => {
                if station.lit {
                    debug!(target, "relay light on a lit station: stale");
                    self.answer_station(player, crate::nodes::StationResult::Stale);
                    return;
                }
                if station.fuel < crate::build::FUEL_PER_JOB {
                    self.answer_station(player, crate::nodes::StationResult::NeedsFuel);
                    return;
                }
                if station.input.is_none() {
                    self.answer_station(player, crate::nodes::StationResult::NeedsInput);
                    return;
                }
                let st = self
                    .world
                    .stations
                    .get_mut(&target)
                    .expect("BUG: station checked above");
                st.lit = true;
                st.progress = 0;
                self.set_station_lit(target, true);
                self.answer_station(player, crate::nodes::StationResult::Lit);
                info!(target, "relay station lit");
            }
        }
    }

    /// Authority side of the fuel/input relay (session 33): the player
    /// homed on the sender clicked the station with a held stack. The
    /// stack physically lives on the HOME node's cursor - HERE we only
    /// validate against the local StationState and mutate the station
    /// counters/slot; the home node consumes one cursor unit on the ok
    /// ack. Refusals answer with the same lines the local path emits.
    fn relay_station_item(
        &mut self,
        player: GobId,
        target: GobId,
        stack: crate::nodes::StaticStack,
    ) {
        use crate::nodes::StationItemResult;
        let Some(station) = self.world.stations.get(&target).cloned() else {
            debug!(target, "relay station item: target gone");
            self.answer_station_item(player, StationItemResult::Gone);
            return;
        };
        let buildable = &crate::build::BUILDABLES[station.spec as usize];
        let Some(station_spec) = buildable.station.as_ref() else {
            self.answer_station_item(player, StationItemResult::Gone);
            return;
        };
        // Fuel deliveries load while lit too (local path order: fuel
        // check first, then the lit/input gates for the roast slot).
        if station_spec.fuel.contains(&stack.res.as_str()) {
            let st = self
                .world
                .stations
                .get_mut(&target)
                .expect("BUG: station checked above");
            st.fuel += 1;
            st.fuel_ql_sum += stack.ql as u64;
            st.fuel_seen += 1;
            self.publish(target, GuestEv::Update);
            self.answer_station_item(player, StationItemResult::FuelAdded);
            info!(target, "relay station fueled");
            return;
        }
        if station.lit {
            self.answer_station_item(player, StationItemResult::BusyLit);
            return;
        }
        if station.input.is_some() {
            self.answer_station_item(player, StationItemResult::InputFull);
            return;
        }
        if crate::craft::roast_result(leak_static(stack.label.as_str())).is_none() {
            self.answer_station_item(player, StationItemResult::NotProcessable);
            return;
        }
        let res_name = leak_static(stack.res.as_str());
        let res_idx = self.world.res.intern(res_name);
        let st = self
            .world
            .stations
            .get_mut(&target)
            .expect("BUG: station checked above");
        st.input = Some((res_idx, stack.ql, leak_static(stack.label.as_str())));
        self.publish(target, GuestEv::Update);
        self.answer_station_item(player, StationItemResult::InputLoaded);
        info!(target, label = stack.label, "relay station input loaded");
    }

    /// Unicast StationItemAck to the acting player's home node.
    fn answer_station_item(&mut self, player: GobId, result: crate::nodes::StationItemResult) {
        if let Some(c) = self.cluster.as_ref() {
            let home = self.node_of_gob(player);
            c.mesh.send(
                home,
                crate::nodes::NodeMsg::StationItemAck { player, result },
            );
        }
    }

    /// Authority-side application of a relayed static interaction
    /// (session 30). The clicking player is homed on the sender; the
    /// target's lifecycle is authoritative HERE. Validates the act
    /// against the real Kind (the sender's guest view may lag a hop),
    /// applies the same logic as the local interact path, and answers
    /// with StaticAck so the player's inventory/LP update on the home
    /// node exactly once.
    fn relay_static(&mut self, player: GobId, target: GobId, act: crate::nodes::StaticAct) {
        use crate::nodes::{StaticAct, StaticStack};
        let Some(tslot) = self.world.gobs.get(target) else {
            // Gone between click and relay hop: the home node learns the
            // truth from GuestRetract; ack nothing (idempotent no-op).
            debug!(target, ?act, "relay static: target gone");
            return;
        };
        // Each leg returns (stacks, lp); a harvest yields several stacks,
        // the other legs at most one. An empty vec = nothing to ack.
        let result: Option<(Vec<Option<StaticStack>>, i32)> =
            match (&self.world.gobs.kind[tslot], act) {
                (Kind::Drop { .. }, StaticAct::Pickup) => self
                    .relay_pickup(target, tslot)
                    .map(|(s, lp)| (vec![s], lp)),
                (Kind::Tree { harvests }, StaticAct::Chop) => self
                    .relay_chop(target, tslot, *harvests)
                    .map(|(s, lp)| (vec![s], lp)),
                (Kind::Stone, StaticAct::Mine) => {
                    self.relay_mine(target, tslot).map(|(s, lp)| (vec![s], lp))
                }
                (Kind::Crop { .. }, StaticAct::HarvestCrop) => Some((
                    self.relay_crop_harvest(target, tslot)
                        .into_iter()
                        .map(Some)
                        .collect(),
                    0,
                )),
                _ => {
                    // Stale guest view (class vs kind drift): drop the act,
                    // never trust the sender's classification.
                    debug!(target, ?act, "relay static: act/kind mismatch");
                    None
                }
            };
        let Some((stacks, lp)) = result else {
            return;
        };
        if let Some(c) = self.cluster.as_ref() {
            let home = self.node_of_gob(player);
            for (i, stack) in stacks.into_iter().enumerate() {
                // LP rides the FIRST ack only: single-stack legs emit one
                // ack, and a crop harvest yields several stacks but never
                // LP - so every ack after the first carries lp = 0.
                let ack_lp = if i == 0 { lp } else { 0 };
                c.mesh.send(
                    home,
                    crate::nodes::NodeMsg::StaticAck {
                        player,
                        stack,
                        lp: ack_lp,
                    },
                );
            }
        }
    }

    /// Crop-harvest leg (session 31): the authority decides mature vs
    /// unripe from ITS crop state (the guest view may lag a stage), rolls
    /// the SAME quality/yield tables as the local path, removes the crop,
    /// restores tilth and returns every yielded stack as resource NAMES.
    /// Crops never grant LP.
    fn relay_crop_harvest(
        &mut self,
        target: GobId,
        tslot: usize,
    ) -> Vec<crate::nodes::StaticStack> {
        let Kind::Crop { stage, spec } = self.world.gobs.kind[tslot] else {
            return Vec::new();
        };
        let Some(state) = self.world.crops.get(&target).copied() else {
            return Vec::new();
        };
        let Some(spec_data) = farm::CROPS.get(spec as usize) else {
            return Vec::new();
        };
        let mature = stage >= spec_data.stages;
        // Quality roll: seed q + [-5,+5], soil below seed caps at +2
        // (docs "Quality model") - identical arithmetic to the local path.
        let roll = farm::roll_from_uniform(self.world.next_ai_rand(11) as u32);
        let ql = farm::quality_roll(state.seed_ql, state.soil_ql, roll);
        let yields: Vec<farm::Yield> = if mature {
            spec_data.mature_yields.to_vec()
        } else {
            vec![spec_data.early_yield]
        };
        let pos = self.world.gobs.pos[tslot];
        // Remove the crop and restore a decaying tilth entry.
        self.world.crops.remove(&target);
        self.world
            .crop_at
            .remove(&(pos.0.div_euclid(11), pos.1.div_euclid(11)));
        self.world.gobs.kill(target);
        self.broadcast_retract(target);
        self.world.tilth.insert(
            (pos.0.div_euclid(11), pos.1.div_euclid(11)),
            unix_ms() + crate::farm::tilth_decay_ms(),
        );
        let stacks = yields
            .iter()
            .map(|y| {
                let n =
                    farm::count_from_uniform(y.count, self.world.next_ai_rand(1_000_000) as u32);
                crate::nodes::StaticStack {
                    res: y.res.to_string(),
                    count: n.max(1),
                    ql,
                    label: y.label.to_string(),
                }
            })
            .collect();
        info!(gob = target, mature, "guest crop harvested (relay)");
        stacks
    }

    /// Pickup leg: remove the drop, return its exact stack. The stack
    /// crosses as resource NAME (resolves on every node) with count,
    /// quality and the fep.conf display label.
    fn relay_pickup(
        &mut self,
        target: GobId,
        tslot: usize,
    ) -> Option<(Option<crate::nodes::StaticStack>, i32)> {
        let (inv_res, _count, ql, label) = self.world.gobs.kind[tslot].drop_info()?;
        let res_name = self
            .world
            .res
            .name(inv_res)
            .unwrap_or("gfx/invobjs/branch")
            .to_string();
        self.world.gobs.kill(target);
        self.broadcast_retract(target);
        Some((
            Some(crate::nodes::StaticStack {
                res: res_name,
                count: 1,
                ql,
                label: label.to_string(),
            }),
            0,
        ))
    }

    /// Chop leg: one harvest off the tree. Fresh wood drops land on THIS
    /// node (subscribers see them as guests); the frame bump publishes to
    /// viewers so remote trees re-render their harvest state.
    fn relay_chop(
        &mut self,
        target: GobId,
        tslot: usize,
        harvests: u8,
    ) -> Option<(Option<crate::nodes::StaticStack>, i32)> {
        if harvests > 0 {
            self.world.gobs.kind[tslot] = Kind::Tree {
                harvests: harvests - 1,
            };
            self.world.gobs.frame[tslot] += 1;
            let pos = self.world.gobs.pos[tslot];
            self.spawn_drop_near(pos, "gfx/invobjs/wood", 10, "");
            // Frame/publish so every viewer (local and guest) re-renders.
            self.publish(target, GuestEv::Update);
            Some((None, 5))
        } else {
            // Exhausted: remove the tree, leave a stump (same as local).
            let pos = self.world.gobs.pos[tslot];
            self.world.gobs.kill(target);
            self.broadcast_retract(target);
            let stump = self.world.res.intern("gfx/terobjs/trees/log");
            let id = self.world.gobs.spawn(Kind::Stone, pos, stump, 1, 0);
            self.broadcast_spawn(id);
            Some((None, 0))
        }
    }

    /// Mine leg: the stone breaks into a pick-up-able stone drop.
    fn relay_mine(
        &mut self,
        target: GobId,
        tslot: usize,
    ) -> Option<(Option<crate::nodes::StaticStack>, i32)> {
        let pos = self.world.gobs.pos[tslot];
        self.world.gobs.kill(target);
        self.broadcast_retract(target);
        self.spawn_drop_near(pos, "gfx/invobjs/stone", 10, "");
        Some((None, 3))
    }

    /// Authority-side application of one relayed swing (cluster mode):
    /// the attacking player is a guest homed on another node; `chip` /
    /// `dmg` were computed THERE with the same formulas as the local
    /// path (the attacker's str lives on its home node). Applies the
    /// defence chip to the authoritative bar, registers the guest
    /// attacker for retaliation, and answers FightBars so the home
    /// mirror self-heals.
    fn relay_swing(&mut self, attacker: GobId, target: GobId, chip: i32, dmg: i32) {
        let Some(tslot) = self.world.gobs.get(target) else {
            // Died / transferred between the swing and the relay hop; the
            // attacker's node learns the truth from GuestRetract.
            return;
        };
        if !matches!(self.world.gobs.kind[tslot], Kind::Animal { .. }) {
            return;
        }
        tracing::debug!(attacker, target, chip, dmg, "relay swing applied");
        // chip == 0 marks a RANGED relay (archery.rs): arrows bypass the
        // openings economy entirely (the hit roll already happened on
        // the shooter's node), so the damage lands without any defence
        // gate. chip > 0 is the melee swing path below.
        if chip == 0 {
            self.world.guest_attackers.insert(target, attacker);
            self.damage_animal_relayed(target, tslot, dmg);
            return;
        }
        let landed = {
            let af = self.world.animal_fights.entry(target).or_insert_with(|| {
                crate::state::AnimalFight {
                    off: 0,
                    def: crate::fight::BAR_FULL,
                    intensity: 0,
                }
            });
            let breaking = af.def <= crate::fight::OPENING_THRESHOLD;
            af.def = (af.def - chip).max(0);
            let landed = breaking || af.def <= crate::fight::OPENING_THRESHOLD;
            if landed {
                af.def = crate::fight::BAR_FULL;
                af.intensity =
                    (af.intensity + crate::state::INTENSITY_PER_BLOW).min(crate::fight::BAR_FULL);
            }
            landed
        };
        self.world.guest_attackers.insert(target, attacker);
        let def_now = self
            .world
            .animal_fights
            .get(&target)
            .map(|f| f.def)
            .unwrap_or(crate::fight::BAR_FULL);
        if landed {
            self.damage_animal_relayed(target, tslot, dmg);
        }
        // Authoritative bar answer re-syncs the attacker's home mirror.
        if let Some(c) = self.cluster.as_ref() {
            let home = self.node_of_gob(attacker);
            c.mesh.send(
                home,
                crate::nodes::NodeMsg::FightBars {
                    id: target,
                    def: def_now,
                },
            );
        }
    }

    /// Summed equipment armor class (defense, absorption), quality-scaled
    /// per piece. The client computes the same sum from the tooltips
    /// (Equipory.calcAC); the server applies it in combat (armor.rs).
    fn armor_totals(&self, pidx: usize) -> (i32, i32) {
        let mut def = 0;
        let mut abs = 0;
        for slot in &self.world.players[pidx].equip {
            let Some(s) = slot else { continue };
            let name = self.world.res.name(s.res).unwrap_or("");
            if let Some((d, a)) = crate::armor::ac_of(name, i32::from(s.ql)) {
                def += d;
                abs += a;
            }
        }
        (def, abs)
    }

    /// The player's melee swing damage: the FIRST weapon found in the
    /// 16 equipment slots (hand items live at 3/4; slot addressing is
    /// server-side policy, docs/mechanics/items/items-and-quality.md),
    /// else the unarmed strength model (fight.rs). Used by every swing
    /// path - local PvP, animal fights, and the cross-node relays.
    fn melee_dmg(&self, pidx: usize) -> i32 {
        let str_ = *self.world.players[pidx].attrs.get("str").unwrap_or(&10);
        for slot in &self.world.players[pidx].equip {
            let Some(s) = slot else { continue };
            let name = self.world.res.name(s.res).unwrap_or("");
            if let Some(d) = crate::fight::weapon_dmg(name, i32::from(s.ql), str_) {
                return d.max(1);
            }
        }
        crate::fight::unarmed_dmg(str_)
    }

    /// Apply HP damage to a session player after armor absorption.
    /// Returns true when the hit knocked the victim out (the knockout
    /// reset happened inside - session 38 PvP arrows use the flag for
    /// the shooter's chat feedback).
    fn hurt_player(&mut self, pidx: usize, dmg: i32, from: GobId) -> bool {
        let sid = self.world.players[pidx].session;
        let pgob = self.world.players[pidx].gob;
        // Equipment absorption shrinks the damage that reaches HP
        // (armor.rs: dmg * K / (K + abs_total)).
        let (_, abs_total) = self.armor_totals(pidx);
        let dmg = crate::armor::reduce_damage(dmg, abs_total);
        let p = &mut self.world.players[pidx];
        p.hp -= dmg;
        p.stamina = (p.stamina - 2).max(0);
        let hp = p.hp;
        let frame = self.world.tick as u32;
        let quarters = ((hp * 4) / 100).clamp(0, 4) as u8;
        if hp <= 0 {
            p.hp = 50;
            p.energy = (p.energy - 10).max(0);
            p.fight_target = None;
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.fight.rels.clear();
                out.fight.own_def = crate::fight::BAR_FULL;
            }
            self.world.animal_fights.remove(&from);
            // Relay fights: the biter may be a foreign animal (its bars
            // live in the guest mirror); close that mirror too.
            self.world.guest_fights.remove(&from);
            info!(sid, from, "player knocked out by animal");
            return true;
        }
        if let Some(out) = self.sessions.get_mut(&sid) {
            let mut m = MessageBuf::new();
            m.uint8(MSG_OBJDATA)
                .uint8(0)
                .int32(pgob)
                .int32(frame as i32)
                .uint8(OD_HEALTH)
                .uint8(quarters)
                .uint8(OD_END);
            out.send_raw(m.finish());
        }
        false
    }

    /// Criminal-flag duration for a PvP knockout (server policy): 30
    /// real minutes, refreshed by every new knockout while it runs.
    pub const CRIMINAL_MS: u64 = 30 * 60 * 1000;
    /// RMSG_BUFF id of the criminal state. Real buffs start at 1; the
    /// client's pseudo-buffs own the negative ids (-1 crime toggle,
    /// -2 tracking, -3 swim - combat-system.md buff channel).
    pub const CRIMINAL_BUFF_ID: i32 = 1;

    /// The loser's share of a PvP knockout (server policy,
    /// combat-system.md "PvP knockout consequences"): legacy documents
    /// only the DEATH penalties (25-75% through the Tradition/Change
    /// slider); the knockout share - 10% of UNUSED LP, floor zero - is
    /// this server's written policy. Runs on the victim's home node.
    fn knockout_lp_loss(&mut self, loser_pidx: usize) {
        let lost = (self.world.players[loser_pidx].lp / 10).max(0);
        if lost > 0 {
            self.world.players[loser_pidx].lp -= lost;
        }
        let sid = self.world.players[loser_pidx].session;
        if lost > 0 {
            self.chat_line(
                sid,
                &format!("You lost {lost} learning points in the defeat."),
                Some((255, 128, 128)),
            );
        }
    }

    /// The winner's share of a PvP knockout (server policy): flagged
    /// CRIMINAL (assault) for [`CRIMINAL_MS`], icon streamed as a live
    /// buff with a countdown. Runs on the winner's home node (the
    /// relay paths answer there through PvpSwingResult/PvpArrowResult).
    fn flag_criminal(&mut self, winner_pidx: usize) {
        let until = self.world.now_ms + Self::CRIMINAL_MS;
        self.world.players[winner_pidx].criminal_until_ms = Some(until);
        let sid = self.world.players[winner_pidx].session;
        self.chat_line(
            sid,
            "You are flagged criminal for the assault (30 minutes).",
            Some((255, 196, 128)),
        );
        self.stream_criminal_buff(sid);
    }

    /// Stream (or refresh) the criminal buff to one session: countdown
    /// meter in legacy 1/60 s ticks over the remaining wall time. Also
    /// called on world entry so reconnects restore the icon.
    fn stream_criminal_buff(&mut self, sid: SessionId) {
        let Some(&pidx) = self.world.by_session.get(&sid) else {
            return;
        };
        let Some(until) = self.world.players[pidx].criminal_until_ms else {
            return;
        };
        let remaining = until.saturating_sub(self.world.now_ms);
        if remaining == 0 {
            return;
        }
        let cticks = (remaining / 60) as i32;
        const NAME: &str = "gfx/hud/buffs/thorn";
        let gi = self.world.res.intern(NAME);
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        let w = out.res.wire_named(gi, NAME);
        if let Some((n, ver)) = out.res.pending_announce(w) {
            out.send(wdg::resid(w, n, ver));
            out.res.mark_announced(w);
        }
        // RMSG_BUFF rides the RELIABLE session stream (Glob.buffmsg),
        // unlike OBJDATA overlays whose send_raw is datagram-semantics.
        out.send(wdg::buff_set(
            Self::CRIMINAL_BUFF_ID,
            w,
            "Criminal (assault)",
            -1,
            -1,
            100,
            cticks,
            1,
        ));
    }

    /// Criminal-flag expiry sweep (one cheap pass in the world tick):
    /// when the timer runs out the flag clears and the buff icon is
    /// removed with RMSG_BUFF rm.
    fn tick_criminal_expiry(&mut self) {
        let expired: Vec<usize> = self
            .world
            .players
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                matches!(p.criminal_until_ms, Some(u) if u <= self.world.now_ms).then_some(i)
            })
            .collect();
        for i in expired {
            self.world.players[i].criminal_until_ms = None;
            let sid = self.world.players[i].session;
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.send(wdg::buff_rm(Self::CRIMINAL_BUFF_ID));
            }
            self.chat_line(
                sid,
                "Your criminal flag has expired.",
                Some((192, 255, 192)),
            );
        }
    }

    fn tick_vitals(&mut self) {
        let tick = self.world.tick;
        for pidx in 0..self.world.players.len() {
            let p = &mut self.world.players[pidx];
            // Passive LP accrual (skills.rs): the legacy curiosity study
            // system is not implemented yet; the trickle is documented in
            // learning-points-and-curiosity.md server notes.
            crate::skills::accrue(&mut p.lp, &mut p.lp_carry_ms, TICK_MS, self.lp_ms_per_lp);
            // Energy decays ~1 per 30 s; hp regen when energy is high.
            if tick.is_multiple_of(300) {
                p.energy = (p.energy - 1).max(0);
            }
            if tick.is_multiple_of(20) {
                if p.energy > 60 && p.hp < 100 {
                    p.hp += 1;
                }
                if p.stamina < 100 {
                    p.stamina += 1;
                }
                if p.atk_cd > 0 {
                    p.atk_cd -= 1;
                }
            }
            // Starvation damage.
            if p.energy == 0 && tick.is_multiple_of(100) {
                p.hp -= 2;
            }
            if p.hp <= 0 {
                // Knockout + respawn at spawn point with penalty.
                p.hp = 50;
                p.energy = (p.energy - 10).max(0);
                p.fight_target = None;
                let gob = p.gob;
                if let Some(slot) = self.world.gobs.get(gob) {
                    self.world.gobs.mv[slot] = None;
                    self.world.gobs.set_pos(slot, (550, 550));
                    self.world.gobs.hp[slot] = 50;
                    self.world.gobs.frame[slot] += 1;
                }
                info!(player = %p.name, "player down, respawned");
            }
        }
    }

    /// Push updated vitals meters + CATTR to one session.
    fn push_cattr(&mut self, sid: SessionId) {
        let Some(p) = self.world.player(sid) else {
            return;
        };
        let (hp, en, st, lp) = (p.hp, p.energy, p.stamina, p.lp);
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.send(wdg::cattr(&[
            ("pts", lp, lp),
            ("hp", 100, hp),
            ("energy", 100, en),
            ("stamina", 100, st),
        ]));
        // Meter widgets by tag.
        let meters: Vec<(u16, i32)> = out
            .widgets
            .iter()
            .filter(|(_, t)| t.as_str() == "vm")
            .map(|(id, _)| *id)
            .enumerate()
            .map(|(i, id)| (id, [hp, en, st][i.min(2)]))
            .collect();
        for (id, amount) in meters {
            out.send(wdg::wdgmsg(id, "set", &[ListVal::I(amount)]));
        }
    }

    fn push_globlob(&mut self) {
        let (unix, dt, mp, yt) = self.world.astro();
        for out in self.sessions.values() {
            if out.player_gob.is_some() {
                out.send(wdg::globlob(unix, dt, mp, yt, None));
            }
        }
    }

    /// Snapshot a player into the save store (position from the gob slot,
    /// inventory translated from process-local indices to resource names).
    fn persist_player(&mut self, gob: crate::state::GobId) {
        let Some(slot) = self.world.gobs.get(gob) else {
            return;
        };
        let pos = self.world.gobs.pos[slot];
        let Some(pidx) = self.world.players.iter().position(|p| p.gob == gob) else {
            return;
        };
        let p = &self.world.players[pidx];
        let inv_named: Vec<(String, u32, u8)> = p
            .inv
            .iter()
            .map(|s| {
                (
                    self.world
                        .res
                        .name(s.res)
                        .unwrap_or("gfx/invobjs/unknown")
                        .to_owned(),
                    s.count,
                    s.ql,
                )
            })
            .collect();
        let labels: Vec<String> = p.inv.iter().map(|s| s.label.to_owned()).collect();
        let equip_named: Vec<(usize, String, u32, u8, String)> = p
            .equip
            .iter()
            .enumerate()
            .filter_map(|(slot, e)| {
                let s = e.as_ref()?;
                Some((
                    slot,
                    self.world
                        .res
                        .name(s.res)
                        .unwrap_or("gfx/invobjs/unknown")
                        .to_owned(),
                    s.count,
                    s.ql,
                    s.label.to_owned(),
                ))
            })
            .collect();
        self.save.snapshot(p, pos, inv_named, labels, equip_named);
    }

    fn on_session_closed(&mut self, sid: SessionId) {
        // A migration pending on this session dies with it.
        self.pending_joins.remove(&sid);
        if let Some(out) = self.sessions.remove(&sid) {
            // A stack left on the cursor goes back to the inventory so a
            // log-out mid-plant does not eat the item; same-resource stacks
            // merge (InvStack::absorb policy) instead of piling up.
            if let Some(stack) = out.cursor {
                if let Some(p) = self
                    .world
                    .by_session
                    .get(&sid)
                    .copied()
                    .and_then(|idx| self.world.players.get_mut(idx))
                {
                    match p.inv.iter_mut().find(|s| s.res == stack.res) {
                        Some(s) => s.absorb(&stack),
                        None => p.inv.push(stack),
                    }
                }
            }
            if let Some(gob) = out.player_gob {
                // Leaving a party is part of teardown: the roster clears
                // for the remaining members before the player vanishes.
                self.party_leave_gob(gob);
                self.persist_player(gob);
                self.broadcast_retract(gob);
                self.world.gobs.kill(gob);
            }
        }
        if let Some(idx) = self.world.by_session.remove(&sid) {
            self.world.players.remove(idx);
            // Reindex by_session after removal.
            for v in self.world.by_session.values_mut() {
                if *v > idx {
                    *v -= 1;
                }
            }
            // Fix Kind::Player back-references.
            for slot in 0..self.world.gobs.alive.len() {
                if let Kind::Player { player } = self.world.gobs.kind[slot] {
                    if player == usize::MAX {
                        continue;
                    }
                    self.world.gobs.kind[slot] = Kind::Player {
                        player: player.min(self.world.players.len().saturating_sub(1)),
                    };
                }
            }
        }
        info!(sid, "session closed");
    }
}

enum AnimalAction {
    Chase(GobId),
    Flee,
    Wander,
    Idle,
}

/// ResTable stores &'static str; runtime names from items need leaking.
fn leak_static(name: &str) -> &'static str {
    // Memoized leak: repeated calls with the same content return the same
    // pointer. The pose fan-out used to Box::leak a fresh copy per call -
    // an unbounded per-tick allocation leak under the duel-cohort load
    // (the equip layer names are re-derived on every guest pose block).
    static MEMO: std::sync::OnceLock<std::sync::RwLock<HashMap<Box<str>, &'static str>>> =
        std::sync::OnceLock::new();
    let memo = MEMO.get_or_init(|| std::sync::RwLock::new(HashMap::new()));
    if let Some(leaked) = memo.read().map(|m| m.get(name).copied()).ok().flatten() {
        return leaked;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    if let Ok(mut m) = memo.write() {
        m.entry(leaked.into()).or_insert(leaked);
    }
    leaked
}

/// Unix time in milliseconds: the shared clock for crop stage deadlines
/// and tilth decay (survives restarts alongside persisted crops).
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// World-gob resource for a dropped inventory item.
///
/// Inventory item resources (gfx/invobjs/*) carry image + tooltip layers
/// but no `neg` hitbox layer, so the real client cannot render them as
/// world gobs: the sprite init fails with "No negative found" and the
/// drop never appears. The pack mirrors most items under
/// gfx/terobjs/items/<base> with image + neg layers - drops use that
/// world shape. When the pack has no terobj shape for an item, the
/// generic branch shape keeps the drop VISIBLE (a wrong-but-visible
/// shape beats an invisible one); the pickup still restores the original
/// invobj via Kind::Drop::inv_res_idx.
fn drop_world_res(inv_res_name: &str) -> &'static str {
    let base = inv_res_name.rsplit('/').next().unwrap_or(inv_res_name);
    let ter = format!("gfx/terobjs/items/{base}");
    if crate::resources::served(&ter) {
        leak_static(&ter)
    } else {
        "gfx/terobjs/items/branch"
    }
}

impl Kind {
    /// Extract (inv resname_idx, count, ql, display label) from a Drop
    /// kind. The INVENTORY resource, not the gob's render resource: the
    /// render shape is a gfx/terobjs/items world resource (needs a
    /// `neg` layer), while the restored stack must carry the
    /// gfx/invobjs icon resource the inventory widget renders.
    pub fn drop_info(&self) -> Option<(u16, u32, u8, &'static str)> {
        match self {
            Kind::Drop {
                inv_res_idx,
                ql,
                label,
                ..
            } => Some((*inv_res_idx, 1, *ql, label)),
            _ => None,
        }
    }
}

/// Announce one world resource to a session (the resid wire dance) and
/// return its session-local wire id. Callers must intern the resource
/// BEFORE borrowing the session mutably.
fn announce_res(out: &mut SessionOut, gi: u16, name: &'static str) -> i32 {
    let w = out.res.wire_named(gi, name);
    if let Some((n, ver)) = out.res.pending_announce(w) {
        out.send(wdg::resid(w, n, ver));
        out.res.mark_announced(w);
    }
    w as i32
}

// The test module lives in its own file (game/tests.rs) - a #[path]
// child module keeps every private item of `game` visible to the
// tests without pub-super annotations (proj-lib-main-split: testable
// logic; 7.4k lines of tests out of the implementation file).
#[cfg(test)]
#[path = "game/tests.rs"]
mod tests;

// Feature submodules (proj-mod-by-feature): each carries a slice of the
// `impl Game` block. They are children of `game`, so items private to
// this module stay visible here and below; methods another file calls
// are `pub(super)`. The test module (game/tests.rs) is also a child and
// therefore still sees every private item.
mod animals;
mod building;
mod craft;
mod farming;
