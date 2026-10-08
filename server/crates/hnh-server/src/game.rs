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
    pub sessions: crate::fxhash::FxHashMap<SessionId, SessionOut>,
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
    /// Scratch for tick_movement's finished-mover list (taken/restored;
    /// was a fresh `Vec::new()` per tick).
    mv_finished_scratch: Vec<(usize, i32, i32)>,
    /// Scratch for tick_movement's cadence-tick progress candidates
    /// `(id, frame, step, cx, cy)` collected by the scan pass and encoded
    /// by the encode pass (taken/restored).
    mv_progress_scratch: Vec<(GobId, u32, i32, i32, i32)>,
    /// Scratch block encoder for tick_movement (taken/restored; was a
    /// fresh 256 B `MessageBuf::new()` + finish + drop per encoded block
    /// - the last per-mover allocator churn on the 10 Hz hot path).
    mv_encode_scratch: MessageBuf,
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
    pub player_abroad: crate::fxhash::FxHashMap<GobId, usize>,
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
            sessions: crate::fxhash::FxHashMap::default(),
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
            mv_finished_scratch: Vec::new(),
            mv_progress_scratch: Vec::new(),
            mv_encode_scratch: MessageBuf::new(),
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
            player_abroad: crate::fxhash::FxHashMap::default(),
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
                    // Per-window maximum: reset by report_perf every 5 s so
                    // spikes attribute to their window (the lifetime max
                    // above never resets and stops being informative after
                    // the first ramp-up spike).
                    if us > self.world.perf.window_max_tick_us {
                        self.world.perf.window_max_tick_us = us;
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

    fn report_perf(&mut self) {
        let ph = self.world.perf.phase_us;
        info!(
            players = self.world.players.len(),
            animals = self.world.animal_gobs.len(),
            tick_us = self.world.perf.last_tick_us,
            mean_tick_us = self.world.perf.mean_tick_us,
            max_tick_us = self.world.perf.max_tick_us,
            wmax_tick_us = self.world.perf.window_max_tick_us as u64,
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
            mvbat_scan_us = self.world.perf.mvbat_scan_us,
            mvbat_encode_us = self.world.perf.mvbat_encode_us,
            mvbat_fanout_us = self.world.perf.mvbat_fanout_us,
            mvbat_movers = self.world.perf.mvbat_movers,
            ix_cand_n = self.world.perf.ix_cand_n,
            move_blocks = self.world.perf.move_blocks,
            move_cells = self.world.perf.move_cells,
            start_blocks = self.world.perf.start_blocks,
            fx_batch_n = self.world.perf.fx_batch_n,
            grid_gens = self.world.grids.gen_count,
            grid_hits = self.world.grids.hit_count,
            "perf"
        );
        // The window maximum has been delivered to this window's report;
        // the next 5 s window measures from zero.
        self.world.perf.window_max_tick_us = 0;
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
            visible: crate::fxhash::FxHashSet::default(),
            unacked: crate::fxhash::FxHashMap::default(),
            gob_acked: crate::fxhash::FxHashMap::default(),
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
            trough_menu: None,
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
            carried_trough: None,
        });
        // A lifted Food Trough (session 62) rides the character across
        // sessions: restore the fodder store before anything else can
        // interact with the fresh player row.
        if let Some(saved) = self.save.players.get(&key) {
            if let Some(t) = saved.carried_trough {
                self.world.players[player_idx].carried_trough = Some(t.into());
            }
        }
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
                // immediately. Session 58 tops it up (10/6) so the
                // stone-tool batch (saw/pickaxe/scythe/sprucecap) is also
                // reachable without first harvesting; world gathering
                // (bough/stone picking) remains future work.
                ("gfx/invobjs/branch", 10, 10, ""),
                ("gfx/invobjs/stone", 6, 10, ""),
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
        self.world.perf.mvbat_scan_us = 0;
        self.world.perf.mvbat_encode_us = 0;
        self.world.perf.mvbat_fanout_us = 0;
        self.world.perf.mvbat_movers = 0;
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
        // OBJDATA retransmission sweep (session 64): every 3rd tick the
        // unacked table is walked in frame order and unconfirmed blocks
        // past their schedule delay are resent (stream::retransmit_unacked).
        // Rare-event shape: near-empty in the steady state, O(pending).
        if self.world.tick.is_multiple_of(3) {
            self.retransmit_unacked();
        }
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
            self.broadcast_batch(&mut batch);
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

    /// World tile at a subtile coordinate (None outside the generated
    /// area; grid loading is deterministic, see GridStore).
    fn tile_at(&mut self, (x, y): (i32, i32)) -> Option<u8> {
        let gc = (x.div_euclid(1100), y.div_euclid(1100));
        let ix = (x.div_euclid(11)).rem_euclid(100) as usize;
        let iy = (y.div_euclid(11)).rem_euclid(100) as usize;
        Some(self.world.grids.grid(gc).tile(ix, iy))
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
                // The pick legs are SHARED with the local click path
                // (game/interact.rs harvest_tree/harvest_boulder): one
                // implementation, no drift between local and relay.
                (Kind::Tree { .. }, StaticAct::Chop) => {
                    Some((vec![None], self.harvest_tree(target, tslot)))
                }
                (Kind::Boulder { .. }, StaticAct::Mine) => {
                    Some((vec![None], self.harvest_boulder(target, tslot)))
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
mod combat;
mod craft;
mod farming;
mod interact;
mod items;
mod stream;
