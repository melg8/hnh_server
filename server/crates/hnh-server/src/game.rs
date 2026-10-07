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
                // Session 36: 6 branches + 4 stones + 2 string let a fresh
                // character craft one Wooden Bow (4 branch + 1 string) and
                // one batch of Stone Arrows (1 stone + 2 branch) out of the
                // box, with oven-building headroom (stone x2 + branch x1 of
                // the demand) on top - the whole bow chain is playable
                // immediately.
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
mod cluster;
mod craft;
mod farming;
mod items;
mod stream;
