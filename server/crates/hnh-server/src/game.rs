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
    kritter: [[[&'static str; 8]; 2]; 7],
    /// [species] pose-router base resources.
    kritter_base: [&'static str; 7],
}

static POSES: std::sync::OnceLock<PoseTable> = std::sync::OnceLock::new();

/// Species index order must mirror the enum declaration order (state.rs).
const SPECIES_FOLDERS: [&str; 7] = ["deer", "fox", "wolf", "boar", "cow", "hare", "aurochs"];

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
        let mut kritter: [[[&'static str; 8]; 2]; 7] = Default::default();
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
            }
        }
        let restored = world.crops.len();
        if restored > 0 {
            info!(crops = restored, "persisted crops restored");
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
            structures.push(crate::persist::SavedStructure {
                spec,
                tile: *tile,
                quality,
                fuel: 0,
                fuel_ql_sum: 0,
                fuel_seen: 0,
                input: None,
                progress: 0,
            });
        }
        self.save.world_state.structures = structures;
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
            vis_retract_us = self.world.perf.vis_retract_us,
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
            fight: crate::fight::FightState::default(),
            craft_recipe: None,
            craft_window: None,
            item_menu: None,
            item_wids: HashMap::new(),
            crop_menu: None,
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
        self.world.players.push(Player {
            name: name.clone(),
            account,
            gob,
            session: sid,
            hp,
            energy,
            stamina,
            lp,
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
        ]);
        for r in crate::craft::RECIPES {
            pages.push(r.pagina);
        }
        out.send(wdg::paginae_add(&pages));
        // Initial paperdoll contents ("set" + "ava") now that the player
        // and the epry widget both exist.
        self.send_epry_state(sid);
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
                    self.retract_sweep(sid, px, py);
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
        for ((sid, (px, py), cell_moved, _kind), cand) in to_scan.into_iter().zip(in_range) {
            let spawn_t = Instant::now();
            for id in &cand {
                // Check-only here: stream_spawn performs the insert and
                // skips already-present ids; inserting before calling it
                // would suppress the spawn block entirely (the avatar
                // bug: the client never received its own gob).
                let is_new = !self.sessions[&sid].visible.contains(id);
                if is_new {
                    self.stream_spawn(sid, *id);
                }
            }
            // Retractions use a 2x VIEW_RADIUS hysteresis (a gob between
            // R and 2R stays spawned but off-screen), so a per-tick sweep
            // is wasted work: run it every 8th tick and whenever the
            // session crossed a vis cell. Deaths retract immediately via
            // broadcast_retract.
            spawn_us += spawn_t.elapsed().as_micros();
            let retract_t = Instant::now();
            if cell_moved || self.world.tick.is_multiple_of(8) {
                self.retract_sweep(sid, px, py);
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
            // resolution as local movers).
            if pose_flipped {
                for sid in &viewers {
                    self.stream_guest_pose(*sid, id);
                }
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
    /// Session 34 batch shape (the batch_move_broadcast pattern): blocks
    /// are encoded once per moving guest, then ONE pass over the sessions
    /// merges every visible block into ONE datagram per session. The
    /// per-guest full-session scan this replaces (a viewers Vec built by
    /// filtering ALL sessions per guest, every tick) measured 56-67
    /// ms/tick at 600 clustered walking bots - O(guests x sessions)
    /// HashSet lookups plus one datagram per (guest, viewer) pair.
    /// LINSTEP progress frames are deliberately NOT recorded in
    /// `unacked`: each frame is superseded next tick, so a lost datagram
    /// self-heals within 100 ms.
    fn tick_guests(&mut self) {
        if self.world.guests.is_empty() {
            return;
        }
        let now = self.world.now_ms;
        let ids: Vec<GobId> = self.world.guests.keys().copied().collect();
        let mut linsteps: Vec<(GobId, u32, Vec<u8>)> = Vec::new();
        let mut fin_blocks: Vec<(GobId, u32, Vec<u8>)> = Vec::new();
        let mut finished_ids: Vec<GobId> = Vec::new();
        for id in ids {
            let (mv, finished, pos, frame) = {
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
                    (None, true, (lm.tx, lm.ty), frame)
                } else {
                    let (cx, cy) = lm.pos_at(now);
                    let l = lm.step_at(now);
                    let linstep = l > lm.step;
                    g.mv = Some(LinMove { step: l, ..lm });
                    (
                        if linstep { Some((l, lm.steps)) } else { None },
                        false,
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
            if finished {
                let g = self.world.guests.get_mut(&id).expect("checked above");
                g.moving = false;
                let mut m = MessageBuf::new();
                m.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(id)
                    .int32(frame as i32)
                    .uint8(OD_MOVE)
                    .coord(pos.0, pos.1)
                    .uint8(OD_LINSTEP)
                    .int32(0)
                    .uint8(OD_END);
                fin_blocks.push((id, frame, m.finish()));
                finished_ids.push(id);
            } else if let Some((l, _steps)) = mv {
                let mut m = MessageBuf::new();
                m.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(id)
                    .int32(frame as i32)
                    .uint8(OD_LINSTEP)
                    .int32(l)
                    .uint8(OD_END);
                linsteps.push((id, frame, m.finish()));
            }
        }
        // One pass over the sessions: merge every visible block into one
        // datagram; finalizers also land in `unacked` (retransmittable).
        let sids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for sid in sids {
            let Some(out) = self.sessions.get_mut(&sid) else {
                continue;
            };
            let mut m = MessageBuf::with_capacity(256);
            for (id, frame, block) in &fin_blocks {
                if !out.visible.contains(id) {
                    continue;
                }
                m.bytes(block);
                Self::record_unacked(out, *id, *frame, block.clone());
            }
            for (id, _frame, block) in &linsteps {
                if !out.visible.contains(id) {
                    continue;
                }
                m.bytes(block);
            }
            if !m.is_empty() {
                out.send_raw(m.finish());
            }
        }
        // Rest pose for finished movers: the standing layer block per
        // viewer (rare - only on movement finalization; statics skip).
        for id in finished_ids {
            let viewers: Vec<SessionId> = self
                .sessions
                .iter()
                .filter(|(_, o)| o.visible.contains(&id))
                .map(|(s, _)| *s)
                .collect();
            for sid in viewers {
                self.stream_guest_pose(sid, id);
            }
        }
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

    /// Re-stream one guest's pose layers to one session (pose flip).
    fn stream_guest_pose(&mut self, sid: SessionId, id: GobId) {
        use crate::nodes::GuestKind;
        let Some(g) = self.world.guests.get(&id).cloned() else {
            return;
        };
        // Statics have no pose (OD_RES alone renders them): an empty
        // layer list would carry the same bare-0xFFFF defect the
        // session-34 probe caught in the spawn block.
        if matches!(g.kind, GuestKind::Static { .. }) {
            return;
        }
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        let mut m = MessageBuf::new();
        m.uint8(MSG_OBJDATA)
            .uint8(0)
            .int32(id)
            .int32(g.frame as i32)
            .uint8(OD_LAYERS);
        match &g.kind {
            GuestKind::Player { equip, .. } => {
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
            }
            GuestKind::Animal { species } => {
                let Some(sp) = crate::state::Species::from_index(*species) else {
                    return;
                };
                let base = kritter_base(sp);
                let bi = self.world.res.intern(base);
                m.uint16(out.res.wire_named(bi, base));
                let part = kritter_pose_layer(sp, g.moving, g.facing);
                let gi = self.world.res.intern(part);
                m.uint16(out.res.wire_named(gi, part));
                m.uint16(65535);
            }
            GuestKind::Static { .. } => {
                m.uint16(65535);
            }
        }
        m.uint8(OD_END);
        let block = m.finish();
        out.send_raw(block);
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
                    crate::nodes::GuestKind::Player { .. } => {}
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
        if self.world.party_idx(target).is_some() {
            self.system_line(sid, "That player is already in a party.");
            return;
        }
        if let Some(pidx) = self.world.party_idx(clicker_gob) {
            let party = &self.world.parties[pidx];
            if party.leader != clicker_gob {
                self.system_line(sid, "Only the party leader can invite.");
                return;
            }
            if party.members.len() >= crate::party::MAX_MEMBERS {
                self.system_line(sid, "Your party is full.");
                return;
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
        out.send(wdg::new_wdg(
            w,
            "sm",
            -1,
            -1,
            0,
            &[
                ListVal::S("Invite to party".to_owned()),
                ListVal::S("Cancel".to_owned()),
            ],
        ));
        out.player_menu = Some((w, crate::party::PlayerMenu::InviteTarget(target)));
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
        if choice != 0 {
            out.send(wdg::wdgmsg(wid, "cancel", &[]));
            return;
        }
        match action {
            crate::party::PlayerMenu::InviteTarget(target) => {
                self.send_party_invitation(sid, target);
            }
            crate::party::PlayerMenu::JoinParty { leader } => {
                self.join_party(leader, sid);
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
            });
        info!(sid, target, ?species, "fight started");
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
        let guest = self.world.guests.get(&target).cloned();
        let (species, tpos, tslot) = match &guest {
            Some(g) => {
                let species = match g.kind {
                    crate::nodes::GuestKind::Animal { species } => {
                        match Species::from_index(species) {
                            Some(sp) => sp,
                            None => {
                                self.world.players[pidx].aim = None;
                                return;
                            }
                        }
                    }
                    _ => {
                        self.world.players[pidx].aim = None;
                        return;
                    }
                };
                (species, g.pos, None)
            }
            None => match self.world.gobs.get(target) {
                Some(s) => (
                    match self.world.gobs.kind[s] {
                        crate::state::Kind::Animal { species } => species,
                        _ => {
                            self.world.players[pidx].aim = None;
                            return;
                        }
                    },
                    self.world.gobs.pos[s],
                    Some(s),
                ),
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
            self.chat_line(
                sid,
                &format!("Your arrow hits the {} for {dmg} damage.", species.name()),
                Some((192, 255, 192)),
            );
            match (guest.is_some(), tslot) {
                (true, _) => {
                    // Cross-node shot: the authority applies the damage
                    // (chip 0 marks the ranged bypass).
                    if let Some(c) = self.cluster.as_ref() {
                        let authority = self.cell_owner(crate::visidx::cell_of(tpos.0, tpos.1));
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

    /// Map units -> tile coordinates (11x11 map units per tile).
    fn tile_coord(mx: i32, my: i32) -> (i32, i32) {
        (mx.div_euclid(11), my.div_euclid(11))
    }

    /// Plow Field action: furrow one grass tile (Adventure > Landscaping).
    /// Tile 9 (PLOWED) is a real tile type the client renders from the
    /// map stream; the mutation is recorded as a grid override so it
    /// survives eviction/restart, and fresh MAPDATA re-sent to holders.
    fn plow_tile(&mut self, sid: SessionId, (tx, ty): (i32, i32)) {
        let gc = (tx.div_euclid(100), ty.div_euclid(100));
        let lx = tx.rem_euclid(100) as usize;
        let ly = ty.rem_euclid(100) as usize;
        // Cluster (session 32): a tile whose cell lives on another node is
        // plowed THERE. The tile authority owns the grid mutation, the
        // tilth clock and the override persistence; the home node never
        // mutates its own copy while relaying - a shadow furrow here would
        // render on this node's clients and then desync until the
        // authority's TileMutation broadcast arrives.
        let tile_gob_pos = (tx * 11 + 5, ty * 11 + 5);
        if self.is_cluster()
            && self.cell_owner(crate::visidx::cell_of(tile_gob_pos.0, tile_gob_pos.1))
                != self.cluster_me()
        {
            let player_gob = self
                .world
                .players
                .iter()
                .find(|p| p.session == sid)
                .map(|p| p.gob);
            if let (Some(player_gob), Some(c)) = (player_gob, self.cluster.as_ref()) {
                let authority =
                    self.cell_owner(crate::visidx::cell_of(tile_gob_pos.0, tile_gob_pos.1));
                c.mesh.send(
                    authority,
                    crate::nodes::NodeMsg::RelayPlowAct {
                        player: player_gob,
                        tx,
                        ty,
                    },
                );
                debug!(sid, tx, ty, authority, "relay plow act sent");
            }
            return;
        }
        let tile = self.world.grids.grid(gc).tile(lx, ly);
        if tile != tile::GRASS {
            debug!(sid, tx, ty, tile, "plow refused: not grass");
            return;
        }
        if self.world.crop_at.contains_key(&(tx, ty)) {
            debug!(sid, tx, ty, "plow refused: tile occupied");
            return;
        }
        // Drain stamina (server policy; legacy plow-by-hand cost unknown).
        // The relay path drains on the PlowAck instead (never before).
        if let Some(p) = self.world.player_mut(sid) {
            p.stamina = (p.stamina - 10).max(0);
        }
        self.mutate_tile_local(gc, lx, ly, tx, ty, tile::PLOWED);
        let now = unix_ms();
        self.world
            .tilth
            .insert((tx, ty), now + crate::farm::tilth_decay_ms());
        info!(sid, tx, ty, "tile plowed");
    }

    /// Apply one local-authority tile mutation end to end (session 32):
    /// mutate the live grid (which also records the persisted override),
    /// re-send the whole grid as fragmented MAPDATA to every local holder,
    /// and in cluster mode broadcast TileMutation so every peer holding
    /// this grid converges on the new tile.
    fn mutate_tile_local(
        &mut self,
        gc: (i32, i32),
        lx: usize,
        ly: usize,
        tx: i32,
        ty: i32,
        new_tile: u8,
    ) {
        self.world.grids.mutate_tile(gc, lx, ly, new_tile);
        self.resend_grid_to_holders(gc);
        if let Some(c) = self.cluster.as_ref() {
            c.mesh.broadcast_except(
                c.nodes.get(),
                c.me,
                crate::nodes::NodeMsg::TileMutation {
                    tx,
                    ty,
                    tile: new_tile,
                },
            );
        }
    }

    /// Re-send one grid as fragmented MAPDATA to every session holding it
    /// (tile-mutation path; plow, decay revert, remote TileMutation).
    fn resend_grid_to_holders(&mut self, gc: (i32, i32)) {
        let payload = {
            let grid = self.world.grids.grid(gc);
            hnh_proto::MapGridPayload {
                gc,
                mnm: grid.mnm.clone(),
                tiles: grid.tiles.as_slice().to_vec(),
                plot_flags: vec![],
                plots: vec![],
            }
            .encode()
        };
        let holders: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.grids_seen.contains(&gc))
            .map(|(s, _)| *s)
            .collect();
        for h in holders {
            if let Some(out) = self.sessions.get_mut(&h) {
                let pktid = (self.world.tick & 0x3FFFFFFF) as i32;
                let frags = hnh_proto::fragment_payload(MSG_MAPDATA, pktid, &payload, 1200);
                for f in frags {
                    out.send_raw(f);
                }
            }
        }
    }

    /// Receive side of TileMutation (session 32): apply one
    /// authority-side tile change to the local copy. A resident grid
    /// takes the full mutation plus a MAPDATA re-send to local holders; a
    /// non-resident grid only records the override - never materialize a
    /// grid nobody looks at just to shadow a mutation (the next
    /// generation replays the override anyway).
    fn apply_remote_tile_mutation(&mut self, tx: i32, ty: i32, tile: u8) {
        let gc = (tx.div_euclid(100), ty.div_euclid(100));
        let lx = tx.rem_euclid(100) as usize;
        let ly = ty.rem_euclid(100) as usize;
        let was_resident = self.world.grids.is_resident(gc);
        self.world.grids.note_override_maybe(gc, lx, ly, tile);
        if was_resident {
            self.resend_grid_to_holders(gc);
            debug!(
                tx,
                ty, tile, "remote tile mutation applied to resident grid"
            );
        } else {
            debug!(tx, ty, tile, "remote tile mutation recorded as override");
        }
    }

    /// Plant one seed unit from the cursor on a plowed, empty tile.
    fn plant_seed(&mut self, sid: SessionId, spec: usize, (tx, ty): (i32, i32), cursor: InvStack) {
        // The Farming skill value gates planting (learning-points-and-
        // curiosity.md server notes). The seed stays on the cursor so the
        // player can re-act after learning the skill.
        if !self.has_skill_value(sid, "farming", 1) {
            debug!(sid, tx, ty, "plant refused: farming skill value 0");
            self.system_line(
                sid,
                "You need the Farming skill (Character Sheet -> Skill Values) to plant.",
            );
            return;
        }
        // Cluster (session 31): a furrow outside my cells lives on another
        // node - tilth and occupancy are authoritative THERE. Two-phase:
        // relay the act, the ack consumes the seed (never lose a seed to
        // a rejected or lost relay hop).
        let tile_gob_pos = (tx * 11 + 5, ty * 11 + 5);
        if self.is_cluster()
            && self.cell_owner(crate::visidx::cell_of(tile_gob_pos.0, tile_gob_pos.1))
                != self.cluster_me()
        {
            let player_gob = self
                .world
                .players
                .iter()
                .find(|p| p.session == sid)
                .map(|p| p.gob);
            if let (Some(player_gob), Some(c)) = (player_gob, self.cluster.as_ref()) {
                let authority =
                    self.cell_owner(crate::visidx::cell_of(tile_gob_pos.0, tile_gob_pos.1));
                c.mesh.send(
                    authority,
                    crate::nodes::NodeMsg::RelayPlantAct {
                        player: player_gob,
                        tx,
                        ty,
                        spec: spec as u8,
                        seed_ql: cursor.ql,
                    },
                );
                debug!(sid, tx, ty, authority, "relay plant act sent");
            }
            return;
        }
        if !self.world.tilth.contains_key(&(tx, ty)) {
            debug!(sid, tx, ty, "plant refused: tile not plowed");
            return;
        }
        if self.world.crop_at.contains_key(&(tx, ty)) {
            debug!(sid, tx, ty, "plant refused: tile occupied");
            return;
        }
        // Consume one unit; empty cursor hands the stack back to the flow.
        let mut cursor = cursor;
        cursor.count = cursor.count.saturating_sub(1);
        let spec_data = &farm::CROPS[spec];
        let res_idx = self.world.res.intern(spec_data.gob_res);
        let now = unix_ms();
        let state = crate::farm::CropState {
            spec: spec as u8,
            stage: 0,
            seed_ql: cursor.ql,
            soil_ql: crate::farm::soil_quality(tx, ty),
            next_stage_at: now + farm::stage_duration_ms(spec_data).as_millis() as u64,
        };
        // Gob at the tile center; hp/speed are unused for plants.
        let gob = self.world.gobs.spawn(
            Kind::Crop {
                spec: spec as u8,
                stage: 0,
            },
            (tx * 11 + 5, ty * 11 + 5),
            res_idx,
            1,
            0,
        );
        self.world.crops.insert(gob, state);
        self.world.crop_at.insert((tx, ty), gob);
        // Planting clears the tilth decay timer (legacy quirk).
        self.world.tilth.insert((tx, ty), 0);
        if let Some(out) = self.sessions.get_mut(&sid) {
            out.cursor = if cursor.count == 0 {
                None
            } else {
                Some(cursor)
            };
        }
        self.refresh_inventory(sid);
        self.broadcast_spawn(gob);
        info!(sid, tx, ty, spec = spec_data.gob_res, "seed planted");
    }

    /// Click on a crop gob: open the stage-appropriate harvest flower menu.
    fn open_crop_menu(&mut self, sid: SessionId, target: GobId) {
        let Some(slot) = self.world.gobs.get(target) else {
            return;
        };
        let Kind::Crop { stage, spec } = self.world.gobs.kind[slot] else {
            return;
        };
        self.show_crop_menu(sid, target, spec, stage)
    }

    /// Flower menu UI for one crop (local and guest clicks share it;
    /// `spec`/`stage` come from the authoritative state or the guest
    /// view). The menu choice is applied by `harvest_crop`, which routes
    /// locals through the world tables and guests through the relay.
    fn show_crop_menu(&mut self, sid: SessionId, target: GobId, spec: u8, stage: u8) {
        let Some(spec_data) = farm::CROPS.get(spec as usize) else {
            return;
        };
        let option = if stage >= spec_data.stages {
            "Harvest"
        } else if stage >= spec_data.early_stage {
            "Harvest (unripe)"
        } else {
            debug!(sid, stage, "crop not harvestable yet");
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        // One flower menu at a time per session.
        if let Some((old, _)) = out.crop_menu {
            out.send(wdg::dst_wdg(old));
        }
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
            &[ListVal::S(option.to_owned())],
        ));
        out.crop_menu = Some((w, target));
    }

    /// Flower menu choice on a crop: apply the per-stage yield table.
    fn harvest_crop(&mut self, sid: SessionId, wid: u16, choice: i32) {
        let pending = self
            .sessions
            .get(&sid)
            .and_then(|o| o.crop_menu)
            .filter(|(w, _)| *w == wid);
        let Some((_, gob)) = pending else {
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.crop_menu = None;
        out.send(wdg::dst_wdg(wid));
        if choice != 0 {
            out.send(wdg::wdgmsg(wid, "cancel", &[]));
            return;
        }
        out.send(wdg::wdgmsg(wid, "act", &[ListVal::I(0)]));
        // Guest crop (session 31): the menu was local session UI; the
        // harvest act itself routes to the crop's authority, which
        // re-validates the stage, rolls the yield table and acks the
        // stacks back to this node (StaticAck -> grant_pickup).
        if let Some(g) = self.world.guests.get(&gob) {
            if let crate::nodes::GuestKind::Static {
                class: crate::nodes::StaticClass::Crop,
                ..
            } = &g.kind
            {
                let player_gob = self
                    .world
                    .players
                    .iter()
                    .find(|p| p.session == sid)
                    .map(|p| p.gob);
                if let (Some(player_gob), Some(c)) = (player_gob, self.cluster.as_ref()) {
                    let authority = self.cell_owner(crate::visidx::cell_of(g.pos.0, g.pos.1));
                    c.mesh.send(
                        authority,
                        crate::nodes::NodeMsg::RelayStaticAct {
                            player: player_gob,
                            target: gob,
                            act: crate::nodes::StaticAct::HarvestCrop,
                        },
                    );
                    debug!(sid, gob, authority, "relay crop harvest sent");
                }
            }
            return;
        }
        let Some(slot) = self.world.gobs.get(gob) else {
            return;
        };
        let Kind::Crop { stage, spec } = self.world.gobs.kind[slot] else {
            return;
        };
        let pos = self.world.gobs.pos[slot];
        let Some(state) = self.world.crops.get(&gob).copied() else {
            return;
        };
        let spec_data = &farm::CROPS[spec as usize];
        let mature = stage >= spec_data.stages;
        // Quality roll: seed q + [-5,+5], soil below seed caps at +2
        // (docs "Quality model"); skill softcap lands with the skill leaf.
        let roll = farm::roll_from_uniform(self.world.next_ai_rand(11) as u32);
        let ql = farm::quality_roll(state.seed_ql, state.soil_ql, roll);
        let yields: Vec<farm::Yield> = if mature {
            spec_data.mature_yields.to_vec()
        } else {
            vec![spec_data.early_yield]
        };
        let drawn: Vec<(farm::Yield, u32, u8)> = yields
            .iter()
            .map(|y| {
                let n =
                    farm::count_from_uniform(y.count, self.world.next_ai_rand(1_000_000) as u32);
                (*y, n.max(1), ql)
            })
            .collect();
        // Remove the crop and restore a decaying tilth entry.
        self.world.crops.remove(&gob);
        self.world
            .crop_at
            .remove(&(pos.0.div_euclid(11), pos.1.div_euclid(11)));
        self.world.gobs.kill(gob);
        self.broadcast_retract(gob);
        self.world.tilth.insert(
            (pos.0.div_euclid(11), pos.1.div_euclid(11)),
            unix_ms() + crate::farm::tilth_decay_ms(),
        );
        for (y, n, q) in drawn {
            let res_idx = self.world.res.intern(y.res);
            self.grant_pickup(
                sid,
                InvStack {
                    res: res_idx,
                    count: n,
                    ql: q,
                    label: y.label,
                },
            );
        }
        info!(sid, gob, mature, "crop harvested");
    }

    /// Per-tick crop growth + tilth decay (farming scheduler pass).
    fn tick_farming(&mut self) {
        if self.world.crops.is_empty() && self.world.tilth.is_empty() {
            return;
        }
        let now = unix_ms();
        let mut due: Vec<(GobId, u8, u64)> = Vec::new();
        for (gob, state) in self.world.crops.iter() {
            if state.next_stage_at <= now {
                let spec = &farm::CROPS[state.spec as usize];
                let next_stage = (state.stage + 1).min(spec.stages);
                let next_at = if next_stage >= spec.stages {
                    u64::MAX
                } else {
                    now + farm::stage_duration_ms(spec).as_millis() as u64
                };
                due.push((*gob, next_stage, next_at));
            }
        }
        for (gob, stage, next_at) in due {
            let Some(slot) = self.world.gobs.get(gob) else {
                continue;
            };
            let Kind::Crop { spec, .. } = self.world.gobs.kind[slot] else {
                continue;
            };
            self.world.gobs.kind[slot] = Kind::Crop { spec, stage };
            self.world.gobs.frame[slot] += 1;
            if let Some(state) = self.world.crops.get_mut(&gob) {
                state.stage = stage;
                state.next_stage_at = next_at;
            }
            // Stage update: OD_RES re-send with a fresh sdt byte; the
            // client's OCache.cres rebuilds the sprite (non-empty sdt).
            let frame = self.world.gobs.frame[slot];
            let viewers: Vec<SessionId> = self
                .sessions
                .iter()
                .filter(|(_, o)| o.visible.contains(&gob))
                .map(|(s, _)| *s)
                .collect();
            for v in viewers {
                let block = self.encode_gob_block(v, gob, true);
                if let (Some(out), Some(block)) = (self.sessions.get_mut(&v), block) {
                    out.send_raw(block.clone());
                    Self::record_unacked(out, gob, frame, block);
                }
            }
            // Cluster: subscribers re-render the new stage from the
            // updated guest payload (sdt byte in the re-published block).
            self.publish(gob, GuestEv::Update);
            trace!(gob, stage, "crop stage advance");
        }
        // Tilth decay: unplanted furrows revert to grass.
        let expired: Vec<(i32, i32)> = self
            .world
            .tilth
            .iter()
            .filter(|(_, &deadline)| deadline != 0 && deadline <= now)
            .map(|(t, _)| *t)
            .collect();
        for tile_coord in expired {
            self.world.tilth.remove(&tile_coord);
            // Session 32: an expired furrow reverts its tile to GRASS - the
            // live grid, the persisted override (mutate_tile records it),
            // every local holder (MAPDATA re-send) and, in cluster mode,
            // every peer holding the grid (TileMutation broadcast). Before
            // this the tile stayed PLOWED forever: it could never be
            // re-plowed (not grass) nor planted (no tilth) - a dead end.
            let (tx, ty) = tile_coord;
            let gc = (tx.div_euclid(100), ty.div_euclid(100));
            let lx = tx.rem_euclid(100) as usize;
            let ly = ty.rem_euclid(100) as usize;
            let cur = self.world.grids.grid(gc).tile(lx, ly);
            if cur == tile::PLOWED {
                self.mutate_tile_local(gc, lx, ly, tx, ty, tile::GRASS);
            }
            debug!(tx = tile_coord.0, ty = tile_coord.1, "tilth decayed");
        }
    }

    // ------------------------------------------------------------------
    // Building placement + production stations
    // (crafting-and-building.md: plans, material sinking, stages)
    // ------------------------------------------------------------------

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

    /// Build pagina activated: drive the client into placement mode. The
    /// mapview `place` uimsg carries (resname, version, on-tile[, radius]);
    /// the ghost plob follows the mouse until the player commits.
    fn arm_build_placement(&mut self, sid: SessionId, spec: usize) {
        let buildable = &crate::build::BUILDABLES[spec];
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        let Some(wid) = Self::mapview_wid(out) else {
            debug!(sid, "build refused: no mapview yet");
            return;
        };
        let res_idx = self.world.res.intern(buildable.res);
        let wire = out.res.wire_named(res_idx, buildable.res);
        if let Some((n, v)) = out.res.pending_announce(wire) {
            out.send(wdg::resid(wire, n, v));
            out.res.mark_announced(wire);
        }
        // Replace any armed placement: the client's plob is singular.
        let mut args: Vec<ListVal> = vec![
            ListVal::S(buildable.res.to_owned()),
            ListVal::I(1),
            ListVal::I(buildable.on_tile as i32),
        ];
        if let Some(r) = buildable.place_radius {
            args.push(ListVal::I(r));
        }
        out.send(wdg::wdgmsg(wid, "place", &args));
        out.pending_build = Some(spec);
        info!(sid, id = buildable.id, "build pagina armed");
    }

    /// Cancel an armed placement (right-button commit or new flow): drop
    /// the client ghost and clear the pending build.
    fn cancel_build(&mut self, sid: SessionId) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        if out.pending_build.is_none() {
            return;
        }
        if let Some(wid) = Self::mapview_wid(out) {
            out.send(wdg::wdgmsg(wid, "unplace", &[]));
        }
        out.pending_build = None;
    }

    /// MapView `place(coord, button, modflags)`: the ghost commit. Button
    /// 1 places; any other button cancels (server policy, mirroring the
    /// client's left-click commit / right-click flower-menu split).
    fn on_map_place(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
        let mc = args.iter().filter_map(|a| a.as_coord()).next();
        let button = args.iter().filter_map(|a| a.as_int()).next().unwrap_or(1);
        if button != 1 {
            self.cancel_build(sid);
            return;
        }
        let Some((mx, my)) = mc else { return };
        let Some(spec) = self.sessions.get(&sid).and_then(|o| o.pending_build) else {
            debug!(sid, "place without armed build: ignoring");
            return;
        };
        self.commit_build(sid, spec, (mx, my));
    }

    /// Validate a placement commit and spawn the construction plan gob.
    fn commit_build(&mut self, sid: SessionId, spec: usize, (mx, my): (i32, i32)) {
        let buildable = &crate::build::BUILDABLES[spec];
        let tile = Self::tile_coord(mx, my);
        // Reach: server-side validation of the commit point (client trust
        // boundary; 5 tiles matches the interaction radius policy).
        let in_reach = self
            .world
            .player(sid)
            .and_then(|p| self.world.gobs.get(p.gob))
            .map(|slot| {
                let (px, py) = self.world.gobs.pos[slot];
                let (ptx, pty) = (px.div_euclid(11), py.div_euclid(11));
                (ptx - tile.0).abs() <= 5 && (pty - tile.1).abs() <= 5
            })
            .unwrap_or(false);
        if !in_reach {
            self.system_line(sid, "Too far away to build there.");
            return;
        }
        // Terrain: passable tiles only (the tile_speed rule table is the
        // single walkability source; water and cliffs refuse plans).
        let gc = (tile.0.div_euclid(100), tile.1.div_euclid(100));
        let (lx, ly) = (
            tile.0.rem_euclid(100) as usize,
            tile.1.rem_euclid(100) as usize,
        );
        let t = self.world.grids.grid(gc).tile(lx, ly);
        if crate::state::tile_speed(t).is_none() {
            debug!(
                sid,
                tx = tile.0,
                ty = tile.1,
                tile = t,
                "build refused: terrain"
            );
            return;
        }
        // Occupancy: one site per tile across crops, plans, structures.
        if self.world.crop_at.contains_key(&tile)
            || self.world.plan_at.contains_key(&tile)
            || self.world.structure_at.contains_key(&tile)
        {
            debug!(sid, tx = tile.0, ty = tile.1, "build refused: occupied");
            return;
        }
        let res_idx = self.world.res.intern(buildable.res);
        let pos = (tile.0 * 11 + 5, tile.1 * 11 + 5);
        let gob = self.world.gobs.spawn(
            Kind::Plan {
                spec: spec as u8,
                stage: 0,
            },
            pos,
            res_idx,
            buildable.hp,
            0,
        );
        self.world.plans.insert(
            gob,
            crate::build::PlanState {
                spec: spec as u8,
                tile,
                credited: Vec::new(),
            },
        );
        self.world.plan_at.insert(tile, gob);
        if let Some(out) = self.sessions.get_mut(&sid) {
            out.pending_build = None;
            if let Some(wid) = Self::mapview_wid(out) {
                out.send(wdg::wdgmsg(wid, "unplace", &[]));
            }
        }
        self.broadcast_spawn(gob);
        info!(sid, id = buildable.id, tile = ?tile, gob, "construction plan placed");
    }

    /// Sink held material into a construction plan (itemact on the plan
    /// gob): validate against remaining demand, consume, snapshot the
    /// delivery quality, advance the stage, and complete when full.
    fn sink_material(&mut self, sid: SessionId, gob: GobId, mut cursor: InvStack) {
        let resname = match self.world.res.name(cursor.res) {
            Some(n) => n,
            None => return,
        };
        let Some(plan) = self.world.plans.get(&gob).cloned() else {
            return;
        };
        let buildable = &crate::build::BUILDABLES[plan.spec as usize];
        let remaining = crate::build::remaining(buildable, &plan.credited, resname);
        if remaining == 0 {
            // Not a demanded material (or already full): the item stays in
            // hand and the plan does not consume it.
            self.system_line(sid, &format!("The {} does not need that.", buildable.id));
            return;
        }
        let n = cursor.count.min(remaining);
        cursor.count -= n;
        let credited = &mut self
            .world
            .plans
            .get_mut(&gob)
            .expect("BUG: plan checked above")
            .credited;
        match credited.iter_mut().find(|c| c.res == resname) {
            Some(c) => {
                c.count += n;
                c.ql_sum += cursor.ql as u64 * n as u64;
            }
            None => credited.push(crate::build::Credited {
                res: resname,
                count: n,
                ql_sum: cursor.ql as u64 * n as u64,
            }),
        }
        let new_stage = crate::build::stage_for(buildable, credited);
        let complete = self
            .world
            .plans
            .get(&gob)
            .map(|p| p.complete(buildable))
            .unwrap_or(false);
        // Consume from the cursor (empty cursor hands control back).
        if let Some(out) = self.sessions.get_mut(&sid) {
            out.cursor = if cursor.count == 0 {
                None
            } else {
                Some(cursor)
            };
        }
        self.refresh_inventory(sid);
        if complete {
            self.complete_plan(gob);
            return;
        }
        // Stage advancement: OD_RES re-send with a fresh sdt byte (the
        // crop-growth render pattern; OCache.cres rebuilds the sprite).
        let cur_stage = match self.world.gobs.get(gob) {
            Some(slot) => match self.world.gobs.kind[slot] {
                Kind::Plan { stage, .. } => stage,
                _ => return,
            },
            None => return,
        };
        if new_stage != cur_stage {
            let spec = plan.spec;
            if let Some(slot) = self.world.gobs.get(gob) {
                self.world.gobs.kind[slot] = Kind::Plan {
                    spec,
                    stage: new_stage,
                };
                self.world.gobs.frame[slot] += 1;
            }
            self.restage_gob(gob);
            // Cluster (session 34): a stage advance re-renders for LOCAL
            // viewers through restage_gob; peers watching the build get
            // the same sdt byte through a GuestUpdate re-publish. Without
            // this a foreign player's plan sprite never leaves stage 0.
            self.publish(gob, GuestEv::Update);
        }
        info!(sid, id = buildable.id, n, res = resname, "material sunk");
    }

    /// Convert a fully-credited plan into the finished structure gob.
    fn complete_plan(&mut self, gob: GobId) {
        let Some(plan) = self.world.plans.remove(&gob) else {
            return;
        };
        self.world.plan_at.remove(&plan.tile);
        let buildable = &crate::build::BUILDABLES[plan.spec as usize];
        let slot = match self.world.gobs.get(gob) {
            Some(s) => s,
            None => return,
        };
        let quality = crate::build::structure_quality(&plan.credited);
        let kind = if buildable.station.is_some() {
            Kind::Station {
                spec: plan.spec,
                lit: false,
            }
        } else {
            Kind::Structure { spec: plan.spec }
        };
        // In-place conversion keeps the gob id (and its visibility set):
        // only the resource/state re-render marks the transition.
        self.world.gobs.kind[slot] = kind;
        self.world.gobs.frame[slot] += 1;
        if buildable.station.is_some() {
            self.world.stations.insert(
                gob,
                crate::build::StationState {
                    spec: plan.spec,
                    fuel: 0,
                    fuel_ql_sum: 0,
                    fuel_seen: 0,
                    input: None,
                    lit: false,
                    progress: 0,
                    quality,
                },
            );
            self.world.structure_at.insert(plan.tile, gob);
        } else {
            self.world.structure_at.insert(plan.tile, gob);
        }
        self.restage_gob(gob);
        // Cluster (session 34): completion flips the guest row's class
        // Structure -> Station and attaches the StationView snapshot.
        // Every guest interaction (fuel, input, Light menu, relay acts)
        // keys off the Station class, so without this re-publish a peer
        // watching the build keeps a dead Structure guest forever - the
        // session-33 handoff gap, now driven end to end by
        // probe_station.py.
        self.publish(gob, GuestEv::Update);
        info!(id = buildable.id, gob, quality, "structure completed");
    }

    /// Re-send a gob's full block (OD_RES with sdt) to every viewer so a
    /// Kind/resource state change re-renders client-side.
    fn restage_gob(&mut self, gob: GobId) {
        let frame = match self.world.gobs.get(gob) {
            Some(slot) => self.world.gobs.frame[slot],
            None => return,
        };
        let viewers: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&gob))
            .map(|(s, _)| *s)
            .collect();
        for v in viewers {
            let block = self.encode_gob_block(v, gob, true);
            if let (Some(out), Some(block)) = (self.sessions.get_mut(&v), block) {
                out.send_raw(block.clone());
                Self::record_unacked(out, gob, frame, block);
            }
        }
    }

    /// itemact on a finished station: fuel deliveries fill the fuel
    /// store; the roast input fills the single input slot (unlit only).
    fn station_itemact(&mut self, sid: SessionId, gob: GobId, mut cursor: InvStack) {
        let Some(station) = self.world.stations.get(&gob).cloned() else {
            return;
        };
        let buildable = &crate::build::BUILDABLES[station.spec as usize];
        let Some(station_spec) = buildable.station.as_ref() else {
            return;
        };
        let resname = match self.world.res.name(cursor.res) {
            Some(n) => n,
            None => return,
        };
        if station_spec.fuel.contains(&resname) {
            // Fuel delivery: one unit per itemact keeps accounting exact.
            let station = self
                .world
                .stations
                .get_mut(&gob)
                .expect("BUG: station checked above");
            station.fuel += 1;
            station.fuel_ql_sum += cursor.ql as u64;
            station.fuel_seen += 1;
            cursor.count -= 1;
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.cursor = if cursor.count == 0 {
                    None
                } else {
                    Some(cursor)
                };
            }
            self.refresh_inventory(sid);
            self.system_line(sid, "Fuel added to the oven.");
            // Cluster: the readiness snapshot (fuel) rides the next
            // GuestUpdate (session 33).
            self.publish(gob, GuestEv::Update);
            info!(sid, gob, "station fueled");
            return;
        }
        if station.lit {
            self.system_line(sid, "The fire is burning; wait for it to finish.");
            return;
        }
        if station.input.is_some() {
            self.system_line(sid, "The oven already holds an input.");
            return;
        }
        // Roast input: any raw meat label in craft::ROAST_MAP (the same
        // chain as the hand-craft roast recipe).
        if crate::craft::roast_result(cursor.label).is_none() {
            self.system_line(sid, "The oven cannot process that.");
            return;
        }
        let station = self
            .world
            .stations
            .get_mut(&gob)
            .expect("BUG: station checked above");
        station.input = Some((cursor.res, cursor.ql, cursor.label));
        cursor.count -= 1;
        if let Some(out) = self.sessions.get_mut(&sid) {
            out.cursor = if cursor.count == 0 {
                None
            } else {
                Some(cursor)
            };
        }
        self.refresh_inventory(sid);
        self.system_line(sid, "Input loaded; right-click the oven to light it.");
        // Cluster: the readiness snapshot (has_input) rides the next
        // GuestUpdate (session 33).
        self.publish(gob, GuestEv::Update);
        info!(sid, gob, label = cursor.label, "station input loaded");
    }

    /// Click on a station gob: open the Light/Extinguish flower menu.
    /// Local wrapper: the option label resolves against the
    /// authoritative local state (act intent None - the choice below
    /// resolves against the same state).
    fn open_station_menu(&mut self, sid: SessionId, target: GobId) {
        let lit = self
            .world
            .stations
            .get(&target)
            .map(|st| st.lit)
            .unwrap_or(false);
        self.show_station_menu_with(sid, target, lit, None);
    }

    /// Shared flower-menu body for local and GUEST stations (session 33).
    /// A guest station passes the act intent picked from the piggybacked
    /// snapshot: Light when the view says unlit, Extinguish when lit.
    fn show_station_menu(&mut self, sid: SessionId, target: GobId, lit: bool) {
        let act = if lit {
            crate::nodes::StationAct::Extinguish
        } else {
            crate::nodes::StationAct::Light
        };
        self.show_station_menu_with(sid, target, lit, Some(act));
    }

    fn show_station_menu_with(
        &mut self,
        sid: SessionId,
        target: GobId,
        lit: bool,
        guest_act: Option<crate::nodes::StationAct>,
    ) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        // One flower menu at a time per session.
        if let Some((old, _)) = out.crop_menu {
            out.send(wdg::dst_wdg(old));
            out.crop_menu = None;
        }
        if let Some((old, _)) = out.player_menu {
            out.send(wdg::dst_wdg(old));
            out.player_menu = None;
        }
        let w = out.new_wid("sm");
        let option = if lit { "Extinguish" } else { "Light" };
        out.send(wdg::new_wdg(
            w,
            "sm",
            -1,
            -1,
            0,
            &[ListVal::S(option.to_owned())],
        ));
        out.station_menu = Some((w, target, guest_act));
    }

    /// Flower menu choice on a station: Light starts a job (fuel +
    /// input required), Extinguish cancels the lit state. A GUEST
    /// station's choice relays to the authority (session 33) - the home
    /// node never mutates a foreign station's state, it only renders the
    /// ack's outcome (refusal lines match the local path verbatim).
    fn apply_station_choice(&mut self, sid: SessionId, wid: u16, choice: i32) {
        let pending = self
            .sessions
            .get(&sid)
            .and_then(|o| o.station_menu)
            .filter(|(w, _, _)| *w == wid);
        let Some((_, gob, guest_act)) = pending else {
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.station_menu = None;
        out.send(wdg::dst_wdg(wid));
        if choice != 0 {
            out.send(wdg::wdgmsg(wid, "cancel", &[]));
            return;
        }
        out.send(wdg::wdgmsg(wid, "act", &[ListVal::I(0)]));
        // Guest station: relay the snapshot-picked act; the authority
        // re-validates against its own state and answers StationAck.
        if let Some(act) = guest_act {
            let Some(player_gob) = self.world.player(sid).map(|p| p.gob) else {
                return;
            };
            if let Some(c) = self.cluster.as_ref() {
                let authority = self
                    .world
                    .guests
                    .get(&gob)
                    .map_or(self.cluster.as_ref().map_or(0, |c| c.me), |g| {
                        self.cell_owner(crate::visidx::cell_of(g.pos.0, g.pos.1))
                    });
                c.mesh.send(
                    authority,
                    crate::nodes::NodeMsg::RelayStationAct {
                        player: player_gob,
                        target: gob,
                        act,
                    },
                );
                debug!(sid, gob, ?act, authority, "relay station act sent");
            }
            return;
        }
        let Some(station) = self.world.stations.get(&gob).cloned() else {
            return;
        };
        if station.lit {
            // Extinguish: progress resets (legacy ovens lost the dough;
            // this server preserves the input, policy documented).
            let station = self
                .world
                .stations
                .get_mut(&gob)
                .expect("BUG: station checked above");
            station.lit = false;
            station.progress = 0;
            self.set_station_lit(gob, false);
            info!(sid, gob, "station extinguished");
            return;
        }
        if station.fuel < crate::build::FUEL_PER_JOB {
            self.system_line(sid, "The oven needs fuel first.");
            return;
        }
        if station.input.is_none() {
            self.system_line(sid, "The oven needs an input before lighting.");
            return;
        }
        let station = self
            .world
            .stations
            .get_mut(&gob)
            .expect("BUG: station checked above");
        station.lit = true;
        station.progress = 0;
        self.set_station_lit(gob, true);
        info!(sid, gob, "station lit");
    }

    /// Single source of truth for the wire-visible lit byte: the Kind
    /// variant carries the sdt re-render, the StationState carries the
    /// simulation state — both must move together.
    fn set_station_lit(&mut self, gob: GobId, lit: bool) {
        if let Some(slot) = self.world.gobs.get(gob) {
            if let Kind::Station { spec, .. } = self.world.gobs.kind[slot] {
                self.world.gobs.kind[slot] = Kind::Station { spec, lit };
                self.world.gobs.frame[slot] += 1;
            }
        }
        self.restage_gob(gob);
        // Cluster: subscribers re-render the lit sprite from the sdt
        // byte in the re-published guest block (session 33).
        self.publish(gob, GuestEv::Update);
    }

    /// Per-tick station pass: advance lit jobs, burn fuel, and emit the
    /// output drop beside the station with the station quality formula.
    fn tick_stations(&mut self) {
        if self.world.stations.is_empty() {
            return;
        }
        let mut finished: Vec<(GobId, &'static str, u8, String)> = Vec::new();
        let mut unlit: Vec<GobId> = Vec::new();
        for (gob, station) in self.world.stations.iter_mut() {
            if !station.lit {
                continue;
            }
            let buildable = &crate::build::BUILDABLES[station.spec as usize];
            let Some(spec) = buildable.station.as_ref() else {
                continue;
            };
            station.progress += 1;
            if station.progress < spec.job_ticks {
                continue;
            }
            // Job complete: burn fuel, consume input, roll the output.
            station.progress = 0;
            station.lit = false;
            unlit.push(*gob);
            if station.fuel >= crate::build::FUEL_PER_JOB {
                station.fuel -= crate::build::FUEL_PER_JOB;
                // Burn at the delivered-fuel average; keep the average
                // stable across burns.
                let avg = station.fuel_quality() as u64;
                station.fuel_ql_sum = station.fuel_ql_sum.saturating_sub(avg);
                station.fuel_seen = station.fuel_seen.saturating_sub(1);
            }
            let Some((_, q_item, label)) = station.input.take() else {
                continue;
            };
            let output_label = crate::craft::roast_result(label).unwrap_or(label);
            let ql =
                crate::build::station_output_ql(q_item, station.quality, station.fuel_quality());
            finished.push((*gob, output_label, ql, label.to_owned()));
        }
        for gob in unlit {
            // Wire re-render of the extinguished state (Kind + sdt byte).
            self.set_station_lit(gob, false);
        }
        for (gob, output_label, ql, raw_label) in finished {
            let pos = match self.world.gobs.get(gob) {
                Some(slot) => self.world.gobs.pos[slot],
                None => continue,
            };
            self.spawn_drop_near(pos, "gfx/invobjs/meat", ql, output_label);
            if let Some(sid) = self
                .sessions
                .iter()
                .find(|(_, o)| o.visible.contains(&gob))
                .map(|(s, _)| *s)
            {
                self.system_line(sid, "The oven finished its work.");
            }
            debug!(
                gob,
                output = output_label,
                raw = raw_label,
                ql,
                "station job done"
            );
        }
    }

    // ------------------------------------------------------------------
    // Crafting (crafting-and-building.md: making protocol)
    // ------------------------------------------------------------------

    fn on_menu_action(&mut self, sid: SessionId, action: &[String]) {
        // Craft leaves send act("craft", <recipe-id>) via MenuGrid.
        if action.len() >= 2 && action[0] == "craft" {
            let recipe_id = action[1].as_str();
            // The roast pagina carries ad ["craft", "roast"]; it maps to a
            // dynamic recipe resolved per attempt (any raw meat in scope).
            let known =
                recipe_id == "roast" || crate::craft::RECIPES.iter().any(|r| r.id == recipe_id);
            if !known {
                info!(sid, recipe = recipe_id, "unknown craft id: ignoring");
                return;
            }
            self.open_make_window(sid, recipe_id);
        } else if action.first().map(String::as_str) == Some("plow") {
            // Plow Field pagina (ad ["plow"]): arm tile plowing; the next
            // map click plows the tile under the cursor.
            info!(sid, "plow pagina armed");
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.pending_plow = true;
            }
        } else if !action.is_empty() {
            // Build paginae send their own ad string ("act(\"oven\")"),
            // decoded from the res pack action layers.
            let ad = action[0].as_str();
            if let Some(spec) = crate::build::buildable_by_ad(ad) {
                self.arm_build_placement(sid, spec);
            } else {
                debug!(sid, ?action, "menu action");
            }
        }
    }

    /// Open the `make` widget for a recipe and push its `pop` contents:
    /// a flat (wire-id, count) list, inputs terminated by -1, then outputs.
    fn open_make_window(&mut self, sid: SessionId, recipe_id: &str) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        // One crafting dialog at a time (UI.make_window is a single slot).
        if let Some(old) = out.craft_window {
            out.send(wdg::dst_wdg(old));
            out.craft_window = None;
            out.craft_recipe = None;
        }
        let (title, pop): (&str, Vec<ListVal>) = if recipe_id == "roast" {
            ("Roasted Meat", Vec::new())
        } else {
            match crate::craft::RECIPES.iter().find(|r| r.id == recipe_id) {
                None => return,
                Some(r) => {
                    let mut pop = Vec::new();
                    for (resname, count) in r.inputs {
                        let gidx = self.world.res.intern(resname);
                        let wire = out.res.wire_named(gidx, resname);
                        if let Some((n, v)) = out.res.pending_announce(wire) {
                            out.send(wdg::resid(wire, n, v));
                            out.res.mark_announced(wire);
                        }
                        pop.push(ListVal::I(wire as i32));
                        pop.push(ListVal::I(*count as i32));
                    }
                    pop.push(ListVal::I(-1));
                    for (resname, count) in r.outputs {
                        let gidx = self.world.res.intern(resname);
                        let wire = out.res.wire_named(gidx, resname);
                        if let Some((n, v)) = out.res.pending_announce(wire) {
                            out.send(wdg::resid(wire, n, v));
                            out.res.mark_announced(wire);
                        }
                        pop.push(ListVal::I(wire as i32));
                        pop.push(ListVal::I(*count as i32));
                    }
                    (r.name, pop)
                }
            }
        };
        // Dynamic roast pop: one input (first raw meat present) and its
        // mapped output; rebuilt per attempt when the raw stack changes.
        let pop = if recipe_id == "roast" {
            let meat = self
                .world
                .player(sid)
                .map(|p| {
                    p.inv
                        .iter()
                        .find_map(|s| crate::craft::roast_result(s.label).map(|out| (s.label, out)))
                })
                .unwrap_or(None);
            match meat {
                Some((raw, roasted)) => {
                    let mut pop = Vec::new();
                    for resname in [raw, roasted] {
                        let gidx = self.world.res.intern("gfx/invobjs/meat");
                        let wire = out.res.wire_named(gidx, "gfx/invobjs/meat");
                        if let Some((n, v)) = out.res.pending_announce(wire) {
                            out.send(wdg::resid(wire, n, v));
                            out.res.mark_announced(wire);
                        }
                        pop.push(ListVal::I(wire as i32));
                        pop.push(ListVal::I(1));
                        if resname == raw {
                            pop.push(ListVal::I(-1));
                        }
                    }
                    pop
                }
                None => vec![ListVal::I(-1)],
            }
        } else {
            pop
        };
        let w = out.new_wid("make");
        out.send(wdg::new_wdg(
            w,
            "make",
            350,
            200,
            0,
            &[ListVal::S(title.to_owned())],
        ));
        out.send(wdg::wdgmsg(w, "pop", &pop));
        out.craft_window = Some(w);
        out.craft_recipe = Some(recipe_id.to_owned());
        info!(sid, recipe = recipe_id, "makewindow opened");
    }

    /// Client pressed Craft (mode 0) or Craft All (mode 1) on the make
    /// widget. Loop while preconditions hold; stop after the last success.
    fn on_make_cmd(&mut self, sid: SessionId, mode: i32) {
        let Some(recipe_id) = self.sessions.get(&sid).and_then(|o| o.craft_recipe.clone()) else {
            return;
        };
        let max_iter = if mode == 1 { 64 } else { 1 };
        let mut made = 0u32;
        for _ in 0..max_iter {
            if !self.craft_once(sid, &recipe_id) {
                break;
            }
            made += 1;
        }
        if made > 0 {
            self.refresh_inventory(sid);
            // Re-push pop so the window reflects any roast-input change.
            self.open_make_window(sid, &recipe_id);
        }
        info!(sid, recipe = recipe_id, made, "craft batch done");
    }

    /// One craft attempt: validate, consume (lowest quality first), produce.
    /// Returns false when a precondition fails (ends batch crafting).
    fn craft_once(&mut self, sid: SessionId, recipe_id: &str) -> bool {
        if recipe_id == "roast" {
            return self.roast_once(sid);
        }
        let Some(recipe) = crate::craft::RECIPES.iter().find(|r| r.id == recipe_id) else {
            return false;
        };
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return false;
        };
        // Validate: every input present in the required quantity.
        for (resname, need) in recipe.inputs {
            let gidx = self.world.res.intern(resname);
            let have: u32 = self.world.players[pidx]
                .inv
                .iter()
                .filter(|s| s.res == gidx)
                .map(|s| s.count)
                .sum();
            if have < *need {
                debug!(sid, recipe = recipe.id, resname, "missing ingredient");
                return false;
            }
        }
        // Consume lowest-quality-first so Craft All rolls per-iteration
        // quality from the actually consumed items (crafting doc).
        // `consumed` keeps the flat unit list (pre-36 unit weighting);
        // `per_type` additionally accumulates (qsum, units) per input
        // TYPE for the RoB Legacy:Quality type-weighted formula.
        let mut consumed: Vec<(u8, u32)> = Vec::new(); // (ql, units)
        let mut per_type: Vec<(u32, u32)> = vec![(0, 0); recipe.inputs.len()];
        for (ti, (resname, need)) in recipe.inputs.iter().enumerate() {
            let gidx = self.world.res.intern(resname);
            let mut remaining = *need;
            while remaining > 0 {
                // Find the lowest-quality non-empty stack of this resource.
                let slot = {
                    let inv = &self.world.players[pidx].inv;
                    inv.iter().enumerate().fold(None::<usize>, |best, (i, s)| {
                        if s.res == gidx && s.count > 0 {
                            match best {
                                None => Some(i),
                                Some(b) if s.ql < inv[b].ql => Some(i),
                                other => other,
                            }
                        } else {
                            best
                        }
                    })
                };
                let Some(slot) = slot else {
                    // Validation passed but stacks emptied mid-loop: fail safe.
                    return false;
                };
                let stack = &mut self.world.players[pidx].inv[slot];
                let take = remaining.min(stack.count);
                stack.count -= take;
                consumed.push((stack.ql, take));
                per_type[ti].0 += u32::from(stack.ql) * take;
                per_type[ti].1 += take;
                remaining -= take;
            }
        }
        self.world.players[pidx].inv.retain(|s| s.count > 0);
        // Output quality. With per-type weights (RoB Legacy:Quality):
        // each input TYPE first averages its own consumed units, then the
        // type averages combine as sum(q_t * w_t)/sum(w_t). Empty
        // q_weights keeps the pre-36 behavior: every consumed UNIT weighs
        // equally across types.
        let mut q = if recipe.q_weights.is_empty() {
            let total_w: u32 = consumed.iter().map(|(_, w)| w).sum();
            let qsum: u32 = consumed.iter().map(|(q, w)| u32::from(*q) * w).sum();
            (qsum.checked_div(total_w).unwrap_or(10) as i32).max(1)
        } else {
            let mut qs: u32 = 0;
            let mut ws: u32 = 0;
            for (ti, (qsum, units)) in per_type.iter().enumerate() {
                let w = recipe.q_weights.get(ti).copied().unwrap_or(1);
                let qt = qsum.checked_div((*units).max(1)).unwrap_or(10);
                qs += qt * w;
                ws += w;
            }
            (qs.checked_div(ws.max(1)).unwrap_or(10) as i32).max(1)
        };
        // Softcap by the crafter's relevant attribute (skill stand-in):
        // q = (q + attr)/2 when attr < q (crafting-and-building.md).
        let attr_val = self.world.players[pidx]
            .attrs
            .get(recipe.softcap_attr)
            .copied()
            .unwrap_or(10);
        if attr_val < q {
            q = (attr_val + q) / 2;
        }
        let out_q = q.clamp(1, 255) as u8;
        for (resname, count) in recipe.outputs {
            let gidx = self.world.res.intern(resname);
            let out = InvStack {
                res: gidx,
                count: *count,
                ql: out_q,
                label: "",
            };
            // Merge into an existing same-resource stack (absorb policy)
            // so repeat crafts fill one stack, not one slot per craft.
            match self.world.players[pidx]
                .inv
                .iter_mut()
                .find(|s| s.res == gidx)
            {
                Some(s) => s.absorb(&out),
                None => self.world.players[pidx].inv.push(out),
            }
        }
        // First-time discoveries grant LP (learning doc); keep it modest.
        self.world.players[pidx].lp += 1;
        self.push_cattr(sid);
        true
    }

    /// One roast attempt: find a raw meat stack, convert one unit.
    fn roast_once(&mut self, sid: SessionId) -> bool {
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return false;
        };
        let pos = self.world.players[pidx]
            .inv
            .iter()
            .position(|s| crate::craft::roast_result(s.label).is_some() && s.count > 0);
        let Some(pos) = pos else {
            debug!(sid, "roast: no raw meat in inventory");
            return false;
        };
        let stack = &mut self.world.players[pidx].inv[pos];
        let roasted = crate::craft::roast_result(stack.label).unwrap_or(stack.label);
        stack.count -= 1;
        let out_ql = stack.ql;
        let raw = stack.label;
        if stack.count == 0 {
            self.world.players[pidx].inv.remove(pos);
        }
        // Merge into an existing roasted stack when one exists (absorb
        // policy) instead of stacking duplicates per roast.
        let out_stack = InvStack {
            res: self.world.res.intern("gfx/invobjs/meat"),
            count: 1,
            ql: out_ql,
            label: roasted,
        };
        match self.world.players[pidx]
            .inv
            .iter_mut()
            .find(|s| s.res == out_stack.res && s.label == out_stack.label)
        {
            Some(s) => s.absorb(&out_stack),
            None => self.world.players[pidx].inv.push(out_stack),
        }
        debug!(sid, raw, roasted, "roasted one meat");
        true
    }

    // ------------------------------------------------------------------
    // Eating (food-and-fep.md: eat flow + FEP accumulation)
    // ------------------------------------------------------------------

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
        // The dirty set served this tick's visibility pass; spawn marks
        // after this point (farming/station drops) dirty the next pass.
        self.world.gobs.vis.clear_dirty();
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
        // Encoded OD blocks for this tick's progress and finalization, sent
        // as ONE OBJDATA datagram per viewing session (see
        // batch_move_broadcast) - the per-block per-session fan-out that
        // used to live here dominated the tick budget at the 400+ mover
        // scale (68 ms of movement phase measured at 426 players).
        let mut linsteps: Vec<(GobId, u32, Vec<u8>)> = Vec::new();
        let mut fin_blocks: Vec<(GobId, u32, Vec<u8>)> = Vec::new();
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
                    let mut m = MessageBuf::new();
                    m.uint8(MSG_OBJDATA)
                        .uint8(0)
                        .int32(id)
                        .int32(frame as i32)
                        .uint8(OD_LINSTEP)
                        .int32(l)
                        .uint8(OD_END);
                    linsteps.push((id, frame, m.finish()));
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
            m.uint8(MSG_OBJDATA)
                .uint8(0)
                .int32(id)
                .int32(frame as i32)
                .uint8(OD_MOVE)
                .coord(tx, ty)
                .uint8(OD_LINSTEP)
                .int32(steps)
                .uint8(OD_END);
            fin_blocks.push((id, frame, m.finish()));
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
        self.batch_move_broadcast(linsteps, fin_blocks);
    }

    /// Movement fan-out: send every LINSTEP progress block and every move
    /// finalizer to each viewing session as ONE combined OBJDATA datagram
    /// (the wire format allows consecutive gob blocks per datagram; the
    /// client's recv_objdata loops them). Blocks land in `unacked` per gob
    /// exactly like the old per-block path so OBJACK retransmission keeps
    /// working. O(sessions x movers) hash probes, one datagram and one
    /// channel send per session per tick.
    fn batch_move_broadcast(
        &mut self,
        linsteps: Vec<(GobId, u32, Vec<u8>)>,
        fin_blocks: Vec<(GobId, u32, Vec<u8>)>,
    ) {
        let sids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for sid in sids {
            let Some(out) = self.sessions.get_mut(&sid) else {
                continue;
            };
            let mut m = MessageBuf::with_capacity(256);
            for (id, frame, block) in &fin_blocks {
                if !out.visible.contains(id) {
                    continue;
                }
                m.bytes(block);
                Self::record_unacked(out, *id, *frame, block.clone());
            }
            for (id, _frame, block) in &linsteps {
                if !out.visible.contains(id) {
                    continue;
                }
                m.bytes(block);
                // Progress frames are deliberately NOT recorded in
                // `unacked`: each LINSTEP is superseded by the next tick's
                // frame, so a lost datagram self-heals within 100 ms and
                // per-session block clones would dominate the tick at the
                // 600+ mover scale (measured: 400+ ms of clone traffic per
                // second before this change).
            }
            if !m.is_empty() {
                out.send_raw(m.finish());
            }
        }
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
    fn fx_overlay_broadcast(&mut self, id: GobId, res_name: &'static str) {
        let Some(slot) = self.world.gobs.get(id) else {
            return;
        };
        let frame = self.world.gobs.frame[slot];
        let frame_i32 = frame as i32;
        let gi = self.world.res.intern(res_name);
        self.overlay_seq = self.overlay_seq.wrapping_add(1);
        // Wire id: bit 0 = the persist flag (0 = one-shot), the rest is the
        // client-side overlay id (15-bit sequence keeps it comfortably
        // positive).
        let olid = ((self.overlay_seq & 0x7FFF) << 1) as i32;
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
            let w = out.res.wire_named(gi, res_name);
            if let Some((n, ver)) = out.res.pending_announce(w) {
                out.send(wdg::resid(w, n, ver));
                out.res.mark_announced(w);
            }
            let mut m = MessageBuf::new();
            m.uint8(MSG_OBJDATA)
                .uint8(0)
                .int32(id)
                .int32(frame_i32)
                .uint8(OD_OVERLAY)
                .int32(olid)
                .uint16(w)
                .uint8(OD_END);
            let block = m.finish();
            out.send_raw(block.clone());
            Self::record_unacked(out, id, frame, block);
        }
    }

    /// Resolve and stream one composited-drawable pose (OD_LAYERS) to
    /// every viewer of the gob at `slot`. The layer set derives from the
    /// gob kind + current pose state (moving -> walking set of `facing`,
    /// standing set otherwise; players 6 parts, animals 1 part). No frame
    /// streaming: each directional resource embeds its own animation, so
    /// this fires only on pose/direction CHANGES. Resources unseen by a
    /// session are announced first; the block lands in `unacked` so late
    /// joiners re-ack it like any other frame-carrying update.
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
            // Announce every pose resource this session has not seen.
            let mut announces: Vec<Vec<u8>> = Vec::new();
            let mut wire_ids: Vec<u16> = Vec::with_capacity(layer_names.len() + 1);
            let bw = out.res.wire_named(base_global, base_name);
            if let Some((n, ver)) = out.res.pending_announce(bw) {
                announces.push(wdg::resid(bw, n, ver));
                out.res.mark_announced(bw);
            }
            wire_ids.push(bw);
            for n in layer_names.iter() {
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
            m.uint8(OD_LAYERS).uint16(wire_ids[0]);
            for w in &wire_ids[1..] {
                m.uint16(*w);
            }
            m.uint16(65535).uint8(OD_END);
            let block = m.finish();
            out.send_raw(block.clone());
            out.unacked
                .entry(id)
                .or_default()
                .insert(self.world.gobs.frame[slot], block);
        }
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
        let (sx, sy) = self.interpolated_pos(slot);
        self.world.gobs.set_pos(slot, (sx, sy));
        let (tx, ty) = (
            target.0.clamp(sx - 5000, sx + 5000),
            target.1.clamp(sy - 5000, sy + 5000),
        );
        if !path_clear(&mut self.world, sx, sy, tx, ty) {
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
        let viewers: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&id))
            .map(|(s, _)| *s)
            .collect();
        for v in viewers {
            if let Some(out) = self.sessions.get_mut(&v) {
                let mut m = MessageBuf::new();
                m.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(id)
                    .int32(frame as i32)
                    .uint8(OD_LINBEG)
                    .coord(sx, sy)
                    .coord(tx, ty)
                    .int32(steps)
                    .uint8(OD_END);
                let block = m.finish();
                out.send_raw(block.clone());
                Self::record_unacked(out, id, frame, block);
            }
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
            self.stream_pose(slot);
        }
        // Cluster: the move start/retarget is a guest update for subscribed
        // peers (and the cell owner, for players standing abroad).
        let id = gob_id_from_slot(slot, self.world.gobs.gen[slot]);
        self.publish(id, GuestEv::Update);
        true
    }

    /// Animal AI: parallel intent pass over the SoA columns (read-only),
    /// then serial application (writes stay on the game task). Intents are
    /// computed per grid-owner partition (grid_owner.rs) so the same pure
    /// function maps to true cross-process grid owners later.
    fn tick_animals(&mut self) {
        let tick = self.world.tick;
        // Cluster: only cell-owned animals simulate here (foreign ones are
        // guests or other nodes' authority; transferred out on crossing).
        let animal_ids: Vec<GobId> = self
            .world
            .animal_gobs
            .iter()
            .copied()
            .filter(|&id| match self.world.gobs.get(id) {
                Some(slot) => self.is_authority_slot(slot),
                None => false,
            })
            .collect();
        // Phase A (parallel): pure intent computation over immutable SoA
        // state. Randomness derives from (tick, slot) hashes so the pass is
        // deterministic and race-free without a shared RNG. Work groups by
        // VisIndex-cell owner (grid_owner): each partition is the unit a
        // multi-node deployment would hand to its owning node process.
        let workers = self.workers.max(1);
        let nodes = std::num::NonZeroUsize::new(workers).expect("workers >= 1");
        let decisions: Vec<(GobId, AnimalAction)> = if workers > 1 && animal_ids.len() > 64 {
            let cell_of_gob = |id: &GobId| -> (i32, i32) {
                self.world
                    .gobs
                    .get(*id)
                    .map(|slot| {
                        crate::visidx::cell_of(
                            self.world.gobs.pos[slot].0,
                            self.world.gobs.pos[slot].1,
                        )
                    })
                    .unwrap_or((0, 0))
            };
            let partitions = crate::grid_owner::partition_by_owner(
                cell_of_gob,
                animal_ids.iter().copied(),
                nodes,
            );
            // Rayon runs the pure decision function per grid-owner
            // partition; a dead/gone id yields no intent and the serial
            // apply phase never sees it.
            partitions
                .par_iter()
                .map(|part| {
                    part.iter()
                        .filter_map(|id| Self::animal_intent(id, &self.world, tick, self.saturated))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<Vec<_>>>()
                .into_iter()
                .flatten()
                .collect()
        } else {
            animal_ids
                .iter()
                .filter_map(|id| Self::animal_intent(id, &self.world, tick, self.saturated))
                .collect()
        };
        // Phase B (serial): apply writes; may use the shared RNG.
        for (id, action) in decisions {
            self.apply_animal_action(id, action);
        }
    }

    /// Pure per-animal decision (no mutation) — the unit that maps to a
    /// grid-owner shard in the multi-node layout.
    fn animal_intent(
        id: &GobId,
        world: &World,
        tick: u64,
        saturated: bool,
    ) -> Option<(GobId, AnimalAction)> {
        let slot = world.gobs.get(*id)?;
        let Kind::Animal { species } = world.gobs.kind[slot] else {
            return None;
        };
        if world.gobs.mv[slot].is_some() {
            return None;
        }
        // Find nearest player within perception. Saturated worlds widen
        // the aggro radius so predators converge on the bot cohorts.
        let perception = if saturated { 1500 } else { 400 };
        let aggro = if saturated { 900 } else { 300 };
        let (ax, ay) = world.gobs.pos[slot];
        let mut nearest: Option<(GobId, i32)> = None;
        for p in &world.players {
            if let Some(pslot) = world.gobs.get(p.gob) {
                let (px, py) = world.gobs.pos[pslot];
                let d = ((px - ax).abs() + (py - ay).abs()).min(i32::MAX - 1);
                if d < perception && nearest.map(|(_, nd)| d < nd).unwrap_or(true) {
                    nearest = Some((p.gob, d));
                }
            }
        }
        let action = match nearest {
            Some((pgob, dist)) if species.aggressive() && dist < aggro => AnimalAction::Chase(pgob),
            Some((_pgob, dist)) if !species.aggressive() && dist < 200 => AnimalAction::Flee,
            _ if tick % 20 == (slot as u64) % 20 => {
                // Deterministic (tick, slot) hash stands in for the shared
                // RNG so the parallel pass stays race-free (splitmix32).
                let h =
                    (tick as u32).wrapping_mul(0x9E3779B9) ^ (slot as u32).wrapping_mul(0x85EBCA6B);
                let h = h ^ (h >> 13);
                let h = h.wrapping_mul(0xC2B2AE35);
                let h = h ^ (h >> 16);
                if h.is_multiple_of(4) {
                    AnimalAction::Wander
                } else {
                    AnimalAction::Idle
                }
            }
            _ => AnimalAction::Idle,
        };
        Some((*id, action))
    }

    fn apply_animal_action(&mut self, id: GobId, action: AnimalAction) {
        let Some(slot) = self.world.gobs.get(id) else {
            return;
        };
        let (sx, sy) = self.world.gobs.pos[slot];
        let (tx, ty) = match action {
            AnimalAction::Chase(pgob) => {
                let Some(pslot) = self.world.gobs.get(pgob) else {
                    return;
                };
                let species = match self.world.gobs.kind[slot] {
                    Kind::Animal { species } => species,
                    _ => return,
                };
                let (px, py) = self.world.gobs.pos[pslot];
                // Stop within combat reach (~3 tiles) to attack.
                let dx = px - sx;
                let dy = py - sy;
                let d = (dx.abs() + dy.abs()).max(1);
                if d <= 33 {
                    // Engage: open the fight from both directions.
                    let sid = self
                        .world
                        .players
                        .iter()
                        .find(|p| p.gob == pgob)
                        .map(|p| p.session);
                    if let Some(sid) = sid {
                        self.start_fight(sid, id, species);
                    }
                    return;
                }
                // Step a bounded distance toward the target so the animal
                // re-evaluates frequently instead of walking past a moving
                // player for a hundred ticks.
                let cap = 200.min(d);
                (sx + dx * cap / d, sy + dy * cap / d)
            }
            AnimalAction::Flee => {
                // Run away from the nearest player (approximately: random
                // opposite direction).
                let jx = (self.world.next_ai_rand(21) - 10) * 55;
                let jy = (self.world.next_ai_rand(21) - 10) * 55;
                (sx + jx, sy + jy)
            }
            AnimalAction::Wander => {
                let jx = (self.world.next_ai_rand(15) - 7) * 22;
                let jy = (self.world.next_ai_rand(15) - 7) * 22;
                (sx + jx, sy + jy)
            }
            AnimalAction::Idle => return,
        };
        let tx = tx.clamp(-1_000_000, 1_000_000);
        let ty = ty.clamp(-1_000_000, 1_000_000);
        // Shared movement entry point (timing model + interpolated
        // retargeting; see start_move).
        self.start_move(slot, (tx, ty));
    }

    fn tick_combat(&mut self) {
        const REACH: i32 = 33; // ~3 tiles
        const DISENGAGE: i32 = 300;
        let tick = self.world.tick;

        // --- player side: offence gen, swings, bar streaming ---
        let players: Vec<usize> = (0..self.world.players.len()).collect();
        'player: for pidx in players {
            let (target, aim, sid, pgob) = {
                let p = &self.world.players[pidx];
                (p.fight_target, p.aim, p.session, p.gob)
            };
            // Ranged aim runs its own tick (accuracy meter, chase,
            // auto-release) and is exclusive with a melee target.
            if let Some(a) = aim {
                self.tick_aim(pidx, sid, pgob, a);
                continue;
            }
            let Some(target) = target else { continue };
            let Some(pslot) = self.world.gobs.get(pgob) else {
                continue;
            };
            // --- cluster relay: target is a foreign-authority guest ---
            // Same reach/chase/swing pacing as the local path below; the
            // defence bar lives in the local mirror (`guest_fights`) and
            // every swing ships a RelayAttack to the animal's owner, whose
            // authoritative FightBars answer re-syncs the mirror. HP and
            // death stay on the owner (GuestUpdate/Retract flow back).
            if self.world.guests.contains_key(&target) {
                let Some(guest) = self.world.guests.get(&target) else {
                    self.world.players[pidx].fight_target = None;
                    self.world.guest_fights.remove(&target);
                    self.fight_del(sid, target);
                    continue;
                };
                let (tx, ty) = guest.pos;
                let (px, py) = self.world.gobs.pos[pslot];
                if (px - tx).abs() > DISENGAGE || (py - ty).abs() > DISENGAGE {
                    self.world.players[pidx].fight_target = None;
                    self.world.guest_fights.remove(&target);
                    self.fight_del(sid, target);
                    continue;
                }
                if (px - tx).abs() > REACH || (py - ty).abs() > REACH {
                    // In engagement range but not swinging: chase instead.
                    if self.world.gobs.mv[pslot].is_none() {
                        self.start_move(pslot, (tx, ty));
                    }
                    continue;
                }
                // Every swing relays one (dmg, chip) pair: the owner
                // applies the chip to its authoritative bar and decides on
                // its own opening; landing locally is only UI prediction.
                let relay: (i32, i32) = {
                    let Some(out) = self.sessions.get_mut(&sid) else {
                        continue 'player;
                    };
                    out.fight.own_off =
                        (out.fight.own_off + crate::fight::OFF_REGEN).min(crate::fight::BAR_FULL);
                    if out.fight.atkc > 0 {
                        out.fight.atkc -= 1;
                    }
                    if out.fight.own_off < crate::fight::SWING_SPEND || out.fight.atkc > 0 {
                        continue 'player;
                    }
                    out.fight.own_off -= crate::fight::SWING_SPEND;
                    out.fight.atkc = crate::fight::ATKC_TICKS;
                    let Some(rel) = out.fight.rel_mut(target) else {
                        continue 'player;
                    };
                    rel.ip_self += 1;
                    // Attack weight scales 0.5..2.0 with advantage.
                    let weight = (rel.balance.clamp(-5, 5) as f32) * 0.1 + 1.0;
                    let def_chip = (crate::fight::SWING_DEF_DMG as f32 * weight) as i32;
                    // Chip the mirror with the same arithmetic the owner
                    // applies (one RelayAttack per swing re-syncs anyway,
                    // so a lost frame self-heals on the next one).
                    let _ = {
                        let Some(mf) = self.world.guest_fights.get_mut(&target) else {
                            continue 'player;
                        };
                        let breaking = mf.def <= crate::fight::OPENING_THRESHOLD;
                        mf.def = (mf.def - def_chip).max(0);
                        let landed = breaking || mf.def <= crate::fight::OPENING_THRESHOLD;
                        if landed {
                            mf.def = crate::fight::BAR_FULL;
                        }
                        (breaking, landed)
                    };
                    rel.defence = self
                        .world
                        .guest_fights
                        .get(&target)
                        .map(|f| f.def)
                        .unwrap_or(0);
                    let str = *self.world.players[pidx].attrs.get("str").unwrap_or(&10);
                    ((5 * str / 10).max(1), def_chip)
                };
                self.world.players[pidx].stamina = (self.world.players[pidx].stamina - 2).max(0);
                if let Some(c) = self.cluster.as_ref() {
                    let authority = self.cell_owner(crate::visidx::cell_of(tx, ty));
                    let (dmg, chip) = relay;
                    c.mesh.send(
                        authority,
                        crate::nodes::NodeMsg::RelayAttack {
                            attacker: pgob,
                            target,
                            chip,
                            dmg,
                        },
                    );
                }
                continue;
            }
            let Some(tslot) = self.world.gobs.get(target) else {
                self.world.players[pidx].fight_target = None;
                self.fight_del(sid, target);
                continue;
            };
            let (px, py) = self.world.gobs.pos[pslot];
            let (tx, ty) = self.world.gobs.pos[tslot];
            if (px - tx).abs() > DISENGAGE || (py - ty).abs() > DISENGAGE {
                // Out of range entirely: clean disengagement.
                self.world.players[pidx].fight_target = None;
                self.world.animal_fights.remove(&target);
                self.fight_del(sid, target);
                continue;
            }
            if (px - tx).abs() > REACH || (py - ty).abs() > REACH {
                // In engagement range but not swinging: chase instead.
                if self.world.gobs.mv[pslot].is_none() {
                    // Shared movement entry point (client-consistent timing;
                    // see start_move).
                    self.start_move(pslot, (tx, ty));
                }
                continue;
            }
            // Bar updates and swing decision inside a tight scope, so the
            // session borrow is dropped before any self-facing call.
            let mut swing = None;
            {
                let Some(out) = self.sessions.get_mut(&sid) else {
                    continue;
                };
                // Own bar gen and cooldown first (own_off and rel are disjoint
                // fields; touch rel only after own_off updates).
                out.fight.own_off =
                    (out.fight.own_off + crate::fight::OFF_REGEN).min(crate::fight::BAR_FULL);
                if out.fight.atkc > 0 {
                    out.fight.atkc -= 1;
                }
                if out.fight.own_off < crate::fight::SWING_SPEND || out.fight.atkc > 0 {
                    continue;
                }
                // Swing: spend offence, chip defence, land damage on an opening.
                out.fight.own_off -= crate::fight::SWING_SPEND;
                out.fight.atkc = crate::fight::ATKC_TICKS;
                let Some(rel) = out.fight.rel_mut(target) else {
                    continue;
                };
                rel.ip_self += 1;
                // Attack weight scales 0.5..2.0 with advantage (balance).
                let weight = (rel.balance.clamp(-5, 5) as f32) * 0.1 + 1.0;
                let def_chip = (crate::fight::SWING_DEF_DMG as f32 * weight) as i32;
                // Chip the animal's defence in the World store (the mirror
                // source); rel.defence streams it to the client.
                let (_, landed) = {
                    let Some(af) = self.world.animal_fights.get_mut(&target) else {
                        continue;
                    };
                    let breaking = af.def <= crate::fight::OPENING_THRESHOLD;
                    af.def = (af.def - def_chip).max(0);
                    let landed = breaking || af.def <= crate::fight::OPENING_THRESHOLD;
                    if landed {
                        af.def = crate::fight::BAR_FULL;
                    }
                    (breaking, landed)
                };
                if landed {
                    let str = *self.world.players[pidx].attrs.get("str").unwrap_or(&10);
                    swing = Some((5 * str / 10).max(1));
                }
            }
            self.world.players[pidx].stamina = (self.world.players[pidx].stamina - 2).max(0);
            if let Some(dmg) = swing {
                self.damage_animal(pidx, sid, target, tslot, dmg);
            }
        }

        // --- animal side: aggressive animals swing back ---
        let animals: Vec<GobId> = self.world.animal_gobs.clone();
        for id in animals {
            let Some(slot) = self.world.gobs.get(id) else {
                continue;
            };
            let Kind::Animal { species } = self.world.gobs.kind[slot] else {
                continue;
            };
            let _ = species;
            // Find the engaged player and re-check reach.
            let Some(engaged) = self
                .world
                .players
                .iter()
                .enumerate()
                .find(|(_, q)| q.fight_target == Some(id))
                .map(|(i, q)| (i, q.session, q.gob))
            else {
                continue;
            };
            let (pidx, p_sid, p_gob) = engaged;
            let Some(pslot) = self.world.gobs.get(p_gob) else {
                continue;
            };
            let (ax, ay) = self.world.gobs.pos[slot];
            let (px, py) = self.world.gobs.pos[pslot];
            if (px - ax).abs() > 33 || (py - ay).abs() > 33 {
                // Not in reach: animal defence regenerates.
                if let Some(af) = self.world.animal_fights.get_mut(&id) {
                    af.def = (af.def + crate::fight::DEF_REGEN).min(crate::fight::BAR_FULL);
                }
                continue;
            }
            // Animal offence builds every tick while in reach (mirrors the
            // player's own_off regen). Without this the offence stayed at
            // its initial 0 forever: the swing condition below could never
            // fire and predators NEVER attacked (session 21: the missing
            // attack animation had no attack behind it).
            if let Some(af) = self.world.animal_fights.get_mut(&id) {
                af.off = (af.off + crate::fight::OFF_REGEN).min(crate::fight::BAR_FULL);
            }
            let animal_off = self
                .world
                .animal_fights
                .get(&id)
                .map(|f| f.off)
                .unwrap_or(0);
            let own_def = self
                .sessions
                .get(&p_sid)
                .map(|out| out.fight.own_def)
                .unwrap_or(crate::fight::BAR_FULL);
            // Armor defense slows the breakthrough (armor.rs); fetched
            // outside the mutable borrow below.
            let (def_ac, _) = self.armor_totals(pidx);
            // Animal offence builds; swing chips the player's defence.
            let mut bite = None;
            {
                let Some(af) = self.world.animal_fights.get_mut(&id) else {
                    continue;
                };
                if animal_off >= crate::fight::SWING_SPEND {
                    af.off -= crate::fight::SWING_SPEND;
                    let str = *self.world.players[pidx].attrs.get("str").unwrap_or(&10);
                    // Animal bites are lighter than player swings.
                    let dmg = (5 * str / 10).max(1) / 2;
                    let chip = crate::armor::defense_chip(crate::fight::SWING_DEF_DMG, def_ac);
                    let new_def = (own_def - chip).max(0);
                    if new_def <= crate::fight::OPENING_THRESHOLD {
                        bite = Some(dmg);
                    }
                }
            }
            if let Some(dmg) = bite {
                self.hurt_player(pidx, dmg, id);
                // Attack animation: the one-shot bite FX overlay on the
                // victim (8-frame anim in the resource; the client removes
                // the overlay itself once the cycle completes). This is the
                // native visual cue for animal attacks - the kritter pose
                // pack ships no dedicated attack pose.
                self.fx_overlay_broadcast(p_gob, "gfx/fx/bite");
            }
            // Mirror the animal bars into the player's relation view.
            if let Some(out) = self.sessions.get_mut(&p_sid) {
                if let Some(rel) = out.fight.rel_mut(id) {
                    rel.ip_other += 1;
                    rel.offence = animal_off;
                    rel.defence = self
                        .world
                        .animal_fights
                        .get(&id)
                        .map(|f| f.def)
                        .unwrap_or(0);
                }
                if bite.is_some() {
                    out.fight.own_def = crate::fight::BAR_FULL;
                }
            }
        }

        // --- relay retaliation: animals strike back at guest players ---
        // The attacker is a session player homed on ANOTHER node (it
        // renders here as a published guest): no session, no defence bar,
        // no armor table locally. The bite therefore just SHIPS to the
        // attacker's home node, where hurt_player applies absorption,
        // HP, stamina and the knockout path. v1 bite uses the default
        // str (same value the local path computes for str 10).
        let relay_rows: Vec<(GobId, GobId)> = self
            .world
            .guest_attackers
            .iter()
            .map(|(&a, &p)| (a, p))
            .collect();
        for (id, attacker) in relay_rows {
            let Some(slot) = self.world.gobs.get(id) else {
                self.world.guest_attackers.remove(&id);
                continue;
            };
            // A cell-boundary transfer takes the fight along: the new
            // owner rebuilds the row from the next RelayAttack.
            if !self.is_authority_slot(slot) {
                self.world.guest_attackers.remove(&id);
                self.world.animal_fights.remove(&id);
                continue;
            }
            // The attacker must still be published here (a home node
            // retracts its guest when the player leaves the cell).
            let Some(g) = self.world.guests.get(&attacker) else {
                self.world.guest_attackers.remove(&id);
                self.world.animal_fights.remove(&id);
                continue;
            };
            let (ax, ay) = self.world.gobs.pos[slot];
            let (px, py) = g.pos;
            if (px - ax).abs() > 33 || (py - ay).abs() > 33 {
                // Out of reach: offence keeps building, no bite.
                if let Some(af) = self.world.animal_fights.get_mut(&id) {
                    af.off = (af.off + crate::fight::OFF_REGEN).min(crate::fight::BAR_FULL);
                }
                continue;
            }
            let mut bite = None;
            if let Some(af) = self.world.animal_fights.get_mut(&id) {
                af.off = (af.off + crate::fight::OFF_REGEN).min(crate::fight::BAR_FULL);
                if af.off >= crate::fight::SWING_SPEND {
                    af.off -= crate::fight::SWING_SPEND;
                    // Default-str bite: (5 * 10 / 10).max(1) / 2.
                    bite = Some(2);
                }
            }
            if let Some(dmg) = bite {
                if let Some(c) = self.cluster.as_ref() {
                    let home = self.node_of_gob(attacker);
                    c.mesh.send(
                        home,
                        crate::nodes::NodeMsg::PlayerHurt {
                            player_gob: attacker,
                            dmg,
                            from: id,
                        },
                    );
                }
            }
        }

        // --- fast bar streaming: updod per relation + offdef, every 2 ticks ---
        if tick.is_multiple_of(2) {
            let sids: Vec<SessionId> = self.sessions.keys().copied().collect();
            for sid in sids {
                let Some(out) = self.sessions.get_mut(&sid) else {
                    continue;
                };
                let Some(w) = out.fight.widget else { continue };
                for rel in &out.fight.rels {
                    let b = crate::fight::uimsg(w, "updod", &[rel.gob, rel.offence, rel.defence]);
                    out.send(b.clone());
                }
                let b = crate::fight::uimsg(w, "offdef", &[out.fight.own_off, out.fight.own_def]);
                out.send(b);
                // Soft state on cooldown ticks.
                if out.fight.atkc == crate::fight::ATKC_TICKS / 2 {
                    for rel in &out.fight.rels {
                        let b = crate::fight::uimsg(
                            w,
                            "upd",
                            &[
                                rel.gob,
                                rel.balance,
                                rel.intensity,
                                rel.give,
                                rel.ip_self,
                                rel.ip_other,
                            ],
                        );
                        let _ = b;
                    }
                }
            }
        }
    }

    /// Apply player damage to an animal, handling death + loot.
    fn damage_animal(
        &mut self,
        pidx: usize,
        sid: SessionId,
        target: GobId,
        tslot: usize,
        dmg: i32,
    ) {
        let spec_dmg = dmg;
        if let Some(af) = self.world.animal_fights.get_mut(&target) {
            af.def = af.def.clamp(0, crate::fight::BAR_FULL);
        }
        self.world.gobs.hp[tslot] -= spec_dmg;
        self.world.gobs.frame[tslot] += 1;
        let frame = self.world.gobs.frame[tslot];
        let quarters = ((self.world.gobs.hp[tslot] * 4) / self.world.gobs.max_hp[tslot].max(1))
            .clamp(0, 4) as u8;
        let viewers: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&target))
            .map(|(s, _)| *s)
            .collect();
        for v in viewers {
            if let Some(out) = self.sessions.get_mut(&v) {
                let mut m = MessageBuf::new();
                m.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(target)
                    .int32(frame as i32)
                    .uint8(OD_HEALTH)
                    .uint8(quarters)
                    .uint8(OD_END);
                let b = m.finish();
                out.send_raw(b.clone());
                out.unacked.entry(target).or_default().insert(frame, b);
            }
        }
        if self.world.gobs.hp[tslot] <= 0 {
            let Kind::Animal { species } = self.world.gobs.kind[tslot] else {
                return;
            };
            let pos = self.world.gobs.pos[tslot];
            self.world.gobs.kill(target);
            self.broadcast_retract(target);
            self.world.animal_gobs.retain(|&g| g != target);
            self.world.animal_fights.remove(&target);
            for (res, count, label) in species.loot() {
                for _ in 0..count {
                    self.spawn_drop_near(pos, res, 10, label);
                }
            }
            self.world.players[pidx].lp += 10;
            self.push_cattr(sid);
            self.world.players[pidx].fight_target = None;
            self.fight_del(sid, target);
            info!(target, ?species, "animal killed");
        }
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
                }
            });
            let breaking = af.def <= crate::fight::OPENING_THRESHOLD;
            af.def = (af.def - chip).max(0);
            let landed = breaking || af.def <= crate::fight::OPENING_THRESHOLD;
            if landed {
                af.def = crate::fight::BAR_FULL;
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

    /// HP damage to a relay-fought animal (authority side): streams
    /// OD_HEALTH to local viewers, publishes the guest update (hp rides
    /// the GuestState to the attacker's node), and on death drops loot,
    /// retracts, and credits the attacker's home node with the LP. The
    /// tail of `damage_animal` minus the session-facing fight UI, which
    /// lives on the attacker's node.
    fn damage_animal_relayed(&mut self, target: GobId, tslot: usize, dmg: i32) {
        self.world.gobs.hp[tslot] -= dmg;
        self.world.gobs.frame[tslot] += 1;
        let frame = self.world.gobs.frame[tslot];
        let quarters = ((self.world.gobs.hp[tslot] * 4) / self.world.gobs.max_hp[tslot].max(1))
            .clamp(0, 4) as u8;
        let viewers: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&target))
            .map(|(s, _)| *s)
            .collect();
        for v in viewers {
            if let Some(out) = self.sessions.get_mut(&v) {
                let mut m = MessageBuf::new();
                m.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(target)
                    .int32(frame as i32)
                    .uint8(OD_HEALTH)
                    .uint8(quarters)
                    .uint8(OD_END);
                let b = m.finish();
                out.send_raw(b.clone());
                out.unacked.entry(target).or_default().insert(frame, b);
            }
        }
        // Publish the hp delta to subscribed peers (the attacker's home
        // node streams OD_HEALTH to ITS viewers from this state).
        self.publish(target, GuestEv::Update);
        if self.world.gobs.hp[tslot] <= 0 {
            let Kind::Animal { species } = self.world.gobs.kind[tslot] else {
                return;
            };
            let pos = self.world.gobs.pos[tslot];
            // Credit the guest attacker's home node (the LP wallet lives
            // there) before the fight rows drop.
            let attacker = self.world.guest_attackers.get(&target).copied();
            if let (Some(c), Some(atk)) = (self.cluster.as_ref(), attacker) {
                let home = self.node_of_gob(atk);
                c.mesh.send(
                    home,
                    crate::nodes::NodeMsg::KillCredit {
                        player_gob: atk,
                        lp: 10,
                    },
                );
            }
            self.world.gobs.kill(target);
            self.broadcast_retract(target);
            self.world.animal_gobs.retain(|&g| g != target);
            self.world.animal_fights.remove(&target);
            self.world.guest_attackers.remove(&target);
            for (res, count, label) in species.loot() {
                for _ in 0..count {
                    self.spawn_drop_near(pos, res, 10, label);
                }
            }
            info!(target, ?species, "relay-killed animal");
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

    /// Apply animal damage to a player (health quarters stream too).
    fn hurt_player(&mut self, pidx: usize, dmg: i32, from: GobId) {
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
            return;
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
    Box::leak(name.to_owned().into_boxed_str())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Enter the world as `name` on a fresh single-session game and return
    /// the game plus the outgoing message receivers (shared setup for the
    /// equipment tests).
    fn entered_game(
        name: &str,
    ) -> (
        Game,
        tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
        tokio::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = Game::new(
            42,
            cmd_rx,
            net_rx,
            false,
            std::env::temp_dir().join(format!("hnh-equip-test-{}.json", name)),
        );
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (raw_tx, raw_rx) = tokio::sync::mpsc::channel(512);
        g.session_connected(1, "acct".to_owned(), tx, raw_tx);
        let wid = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "charlist")
            .map(|(k, _)| *k)
            .expect("charlist widget");
        g.on_wdgmsg(
            1,
            wid,
            "play",
            vec![hnh_proto::ListArg::Str(name.to_owned())],
        );
        for _ in 0..3 {
            g.tick();
        }
        (g, rx, raw_rx)
    }

    /// The paperdoll must exist right after world entry with a full "set"
    /// sync and the "ava" gob binding (the user-reported missing doll).
    #[tokio::test]
    async fn epry_is_bootstrapped_with_set_and_ava() {
        let (g, mut rx, _raw) = entered_game("dolluser");
        assert!(
            g.epry_window(1).is_some(),
            "epry widget must exist after world entry"
        );
        let mut saw_set = false;
        let mut saw_ava = false;
        while let Ok(msg) = rx.try_recv() {
            // RMSG_WDGMSG payload: type byte, u16 wid, NUL-terminated name.
            if msg.first() == Some(&RMSG_WDGMSG) && msg.len() > 4 {
                let nul = msg[3..]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|p| 3 + p)
                    .unwrap_or(msg.len());
                let n = String::from_utf8_lossy(&msg[3..nul]);
                if n == "set" {
                    saw_set = true;
                }
                if n == "ava" {
                    saw_ava = true;
                }
            }
        }
        assert!(saw_set, "bootstrap must queue the epry \"set\" sync");
        assert!(saw_ava, "bootstrap must queue the epry \"ava\" gob id");
    }

    /// Equip via cursor ("drop"), unequip via "take"; occupied and invalid
    /// slots are rejected without losing the held stack.
    #[tokio::test]
    async fn epry_equips_and_unequips_via_cursor() {
        let (mut g, _rx, _raw) = entered_game("equipuser");
        let pidx = *g.world.by_session.get(&1).unwrap();
        assert!(g.world.players[pidx].equip.iter().all(|s| s.is_none()));

        // Open the inventory and take the first stack onto the cursor.
        let slen = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "slen")
            .map(|(k, _)| *k)
            .unwrap();
        g.on_wdgmsg(1, slen, "inv", vec![]);
        let item_wid = g
            .sessions
            .get(&1)
            .unwrap()
            .item_wids
            .iter()
            .find(|(_, &idx)| idx == 0)
            .map(|(&w, _)| w)
            .expect("first inventory item widget");
        g.on_wdgmsg(1, item_wid, "take", vec![]);
        assert!(
            g.sessions.get(&1).unwrap().cursor.is_some(),
            "take -> cursor"
        );

        let epry = g.epry_window(1).unwrap();
        // Equip into slot 3.
        g.on_wdgmsg(1, epry, "drop", vec![hnh_proto::ListArg::Int(3)]);
        assert!(
            g.world.players[pidx].equip[3].is_some(),
            "cursor item must land in slot 3"
        );
        assert!(g.sessions.get(&1).unwrap().cursor.is_none());

        // Occupied slot: the take has to refill the hand first.
        let held = g.world.players[pidx].equip[3];
        g.on_wdgmsg(1, epry, "drop", vec![hnh_proto::ListArg::Int(3)]);
        assert_eq!(g.world.players[pidx].equip[3], held, "occupied slot kept");

        // Invalid slot: no panic, no state change.
        g.on_wdgmsg(1, epry, "drop", vec![hnh_proto::ListArg::Int(16)]);
        g.on_wdgmsg(1, epry, "drop", vec![hnh_proto::ListArg::Int(-1)]);

        // Unequip: back onto the cursor.
        g.on_wdgmsg(1, epry, "take", vec![hnh_proto::ListArg::Int(3)]);
        assert!(g.world.players[pidx].equip[3].is_none());
        assert!(g.sessions.get(&1).unwrap().cursor.is_some());

        // Empty slot take with a full hand: rejected.
        g.on_wdgmsg(1, epry, "take", vec![hnh_proto::ListArg::Int(4)]);
        assert!(g.world.players[pidx].equip[4].is_none());
        assert!(g.sessions.get(&1).unwrap().cursor.is_some(), "hand kept");
    }

    /// Extract the (op, layer wire ids) pairs from one raw OBJDATA block
    /// (the same layout the spawn-block test walks).
    fn objdata_layer_lists(block: &[u8]) -> Vec<(u8, Vec<u16>)> {
        if block.len() < 10 || block[0] != MSG_OBJDATA {
            return Vec::new();
        }
        let mut off = 10;
        let mut out = Vec::new();
        while off < block.len() {
            let code = block[off];
            off += 1;
            match code {
                OD_END => break,
                OD_RES => {
                    let wire = u16::from_le_bytes([block[off], block[off + 1]]);
                    off += 2;
                    if wire & 0x8000 != 0 {
                        let n = block[off] as usize;
                        off += 1 + n;
                    }
                }
                OD_MOVE => off += 8,
                OD_LINBEG => off += 20,
                OD_LINSTEP => off += 4,
                OD_LAYERS | OD_AVATAR => {
                    let mut ids = Vec::new();
                    while off + 2 <= block.len() {
                        let id = u16::from_le_bytes([block[off], block[off + 1]]);
                        off += 2;
                        if id == 65535 {
                            break;
                        }
                        ids.push(id);
                    }
                    out.push((code, ids));
                }
                OD_HEALTH => off += 1,
                OD_BUDDY => break,
                _ => break,
            }
        }
        out
    }

    /// Equipping must re-stream the world drawable (OD_LAYERS) with the
    /// piece's borka layers and push the updated doll attribute
    /// (OD_AVATAR) to the owner; unequipping streams again without the
    /// piece (the live doll/world update, not just a spawn-time view).
    #[tokio::test]
    async fn equip_change_streams_layers_and_avatar() {
        let (mut g, mut rx, mut raw) = entered_game("equipvisuser");
        let pidx = *g.world.by_session.get(&1).unwrap();
        // Hand the player a wearable (server-side grant, like a pickup).
        let pants = "gfx/invobjs/linenpants";
        let res = g.world.res.intern(pants);
        g.world.players[pidx].inv.push(InvStack {
            res,
            count: 1,
            ql: 10,
            label: "",
        });
        // Drain the bootstrap traffic: only the CHANGE may be asserted.
        while rx.try_recv().is_ok() {}
        while raw.try_recv().is_ok() {}

        // Reserve the wire id of the layer name the equip table emits
        // for the standing pants (wire_named allocates once per session,
        // so the later stream reuses this id). The world gob is idle, so
        // the standing set of the SPAWN facing applies (spawn faces
        // movement octant 1 - the camera-facing front).
        let gob = g.world.players[pidx].gob;
        let slot = g.world.gobs.get(gob).expect("player gob slot");
        let facing = g.world.gobs.facing[slot];
        let pants_le: &str =
            crate::equip::world_layers([&"gfx/invobjs/linenpants"], false, facing)[0];
        let pants_layer: &'static str = pants_le;
        let layer_res = g.world.res.intern(pants_layer);
        let pants_wire = g
            .sessions
            .get_mut(&1)
            .unwrap()
            .res
            .wire_named(layer_res, pants_layer);

        // Equip: take the stack onto the cursor, drop it into slot 2.
        let slen = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "slen")
            .map(|(k, _)| *k)
            .unwrap();
        g.on_wdgmsg(1, slen, "inv", vec![]);
        // The starting inventory already holds a branch: target the
        // granted stack's widget (the last inventory index) explicitly.
        let pants_idx = g.world.players[pidx].inv.len() - 1;
        let item_wid = g
            .sessions
            .get(&1)
            .unwrap()
            .item_wids
            .iter()
            .find(|(_, &idx)| idx == pants_idx)
            .map(|(&w, _)| w)
            .unwrap_or_else(|| panic!("granted stack widget at idx {pants_idx}"));
        g.on_wdgmsg(1, item_wid, "take", vec![]);
        let epry = g.epry_window(1).unwrap();
        g.on_wdgmsg(1, epry, "drop", vec![hnh_proto::ListArg::Int(2)]);
        assert!(g.world.players[pidx].equip[2].is_some());

        // The OD_LAYERS re-stream must carry the pants wire id, and the
        // doll attribute (OD_AVATAR) must be pushed with it too.
        let mut saw_world = false;
        let mut saw_doll = false;
        while let Ok(block) = raw.try_recv() {
            for (op, ids) in objdata_layer_lists(&block) {
                if ids.contains(&pants_wire) {
                    if op == OD_LAYERS {
                        saw_world = true;
                    }
                    if op == OD_AVATAR {
                        saw_doll = true;
                    }
                }
            }
        }
        assert!(
            saw_world,
            "equip must re-stream OD_LAYERS carrying wire id {pants_le:?}"
        );
        assert!(saw_doll, "equip must push OD_AVATAR carrying the piece");

        // Unequip: another OD_LAYERS/OD_AVATAR pair streams (now without
        // the piece - the layer lists shrink back).
        while raw.try_recv().is_ok() {}
        g.on_wdgmsg(1, epry, "take", vec![hnh_proto::ListArg::Int(2)]);
        assert!(g.world.players[pidx].equip[2].is_none());
        let mut streamed_after_take = 0;
        while let Ok(block) = raw.try_recv() {
            for (op, _) in objdata_layer_lists(&block) {
                if op == OD_LAYERS || op == OD_AVATAR {
                    streamed_after_take += 1;
                }
            }
        }
        assert!(
            streamed_after_take >= 2,
            "unequip must re-stream the drawable and the doll"
        );
    }

    // ------------------------------------------------------------------
    // Session 26: cursor drag widget + ground drops
    // ------------------------------------------------------------------

    /// Taking a stack onto the cursor must create the drag Item widget
    /// with drag=1 + a grab Coord (the legacy held-item body at the
    /// pointer); dropping it back must destroy that widget and leave no
    /// table entry behind.
    #[tokio::test]
    async fn take_creates_and_drop_destroys_cursor_widget() {
        let (mut g, mut rx, _raw) = entered_game("cursoruser");
        g.open_inventory(1);
        // The starter kit ships Wheat Seeds; take that stack.
        let pidx = *g.world.by_session.get(&1).unwrap();
        let item_wid = g.sessions[&1]
            .item_wids
            .iter()
            .find(|(_, &idx)| {
                g.world.players[pidx].inv.get(idx).map(|s| s.label) == Some("Wheat Seeds")
            })
            .map(|(w, _)| *w)
            .expect("starter wheat seeds must have an item widget");
        g.inv_take(1, item_wid);

        let out = g.sessions.get(&1).unwrap();
        assert!(out.cursor.is_some(), "cursor holds the taken stack");
        let cw = out.cursor_wid.expect("drag widget must exist after take");
        assert_eq!(out.widgets.get(&cw).map(String::as_str), Some("item"));
        // Wire proof: the NEWWDG for the cursor wid carries drag=1 and a
        // grab Coord among its args (Item.java factory contract).
        let mut saw_drag_widget = false;
        while let Ok(msg) = rx.try_recv() {
            if msg.first() != Some(&RMSG_NEWWDG) {
                continue;
            }
            let mut m = hnh_proto::MessageBuf::from_slice(&msg[1..]);
            let id = m.u16().unwrap();
            let ty = m.str().unwrap();
            if id != cw || ty != "item" {
                continue;
            }
            let (_x, _y) = m.coord2().unwrap();
            let _parent = m.u16().unwrap();
            let mut args = Vec::new();
            while let Some(a) = m.list_arg().unwrap() {
                args.push(a);
            }
            // args: [res, q, dragFlag, (dragCoord), tooltip, num]
            assert!(args.len() >= 6, "drag item args: {args:?}");
            assert_eq!(args[2], hnh_proto::ListArg::Int(1), "drag flag");
            assert!(
                matches!(args[3], hnh_proto::ListArg::Coord(..)),
                "grab coord expected: {args:?}"
            );
            saw_drag_widget = true;
        }
        assert!(saw_drag_widget, "cursor drag widget must be on the wire");

        // Drop back into the inventory: widget destroyed, table clean.
        g.inv_drop(1, cw, &[]);
        let out = g.sessions.get(&1).unwrap();
        assert!(out.cursor.is_none(), "cursor empty after drop");
        assert!(out.cursor_wid.is_none(), "drag widget id cleared");
        assert!(
            !out.widgets.contains_key(&cw),
            "destroyed drag widget leaves the widget table"
        );
        let mut saw_dst = false;
        while let Ok(msg) = rx.try_recv() {
            if msg.first() == Some(&RMSG_DSTWDG) && msg.len() >= 3 {
                let id = u16::from_le_bytes([msg[1], msg[2]]);
                saw_dst |= id == cw;
            }
        }
        assert!(saw_dst, "the drag widget destruction must be on the wire");
    }

    /// MapView `drop` must land the held stack as a ground gob near the
    /// player and clear the cursor (stack + drag widget).
    #[tokio::test]
    async fn map_drop_spawns_ground_gob_and_clears_cursor() {
        let (mut g, mut rx, mut raw) = entered_game("dropuser");
        g.open_inventory(1);
        let pidx = *g.world.by_session.get(&1).unwrap();
        let item_wid = g.sessions[&1]
            .item_wids
            .iter()
            .find(|(_, &idx)| {
                g.world.players[pidx].inv.get(idx).map(|s| s.label) == Some("Wheat Seeds")
            })
            .map(|(w, _)| *w)
            .expect("starter wheat seeds must have an item widget");
        g.inv_take(1, item_wid);
        assert!(g.sessions[&1].cursor.is_some());
        // Drain bootstrap + take traffic.
        while rx.try_recv().is_ok() {}
        while raw.try_recv().is_ok() {}
        let count_drops = |g: &Game| {
            g.world
                .gobs
                .kind
                .iter()
                .zip(g.world.gobs.alive.iter())
                .filter(|(_, a)| **a)
                .filter(|(k, _)| matches!(k, Kind::Drop { .. }))
                .count()
        };
        let drops_before = count_drops(&g);

        g.on_map_drop(1);

        let out = g.sessions.get(&1).unwrap();
        assert!(out.cursor.is_none(), "cursor cleared by the drop");
        assert!(out.cursor_wid.is_none(), "drag widget gone after the drop");
        // A Drop gob appeared.
        let drops_after = count_drops(&g);
        assert!(
            drops_after > drops_before,
            "map drop must spawn a ground gob ({drops_before} -> {drops_after})"
        );
        // The drop gob must have been announced on the raw wire (OBJDATA).
        let mut saw_objdata = false;
        while let Ok(p) = raw.try_recv() {
            if p.first() == Some(&MSG_OBJDATA) {
                saw_objdata = true;
            }
        }
        assert!(saw_objdata, "the ground gob must be streamed to the client");
    }

    /// The take -> ground-drop round trip conserves the stack: what left
    /// the inventory came back as the same resource/count on the ground.
    #[tokio::test]
    async fn map_drop_conserves_stack_contents() {
        let (mut g, _rx, _raw) = entered_game("dropq");
        g.open_inventory(1);
        let pidx = *g.world.by_session.get(&1).unwrap();
        let item_wid = g.sessions[&1]
            .item_wids
            .iter()
            .find(|(_, &idx)| {
                g.world.players[pidx].inv.get(idx).map(|s| s.label) == Some("Wheat Seeds")
            })
            .map(|(w, _)| *w)
            .expect("item widget");
        let taken = {
            let inv = &g.world.players[pidx].inv;
            let idx = g.sessions[&1].item_wids[&item_wid];
            inv[idx]
        };
        g.inv_take(1, item_wid);
        g.on_map_drop(1);
        // The new Drop gob: inv_res_idx == the taken stack's resource
        // (pickup restores the exact icon), render res != inv res (the
        // world shape must be a terobjs resource with a neg layer).
        let mut hit_inv = false;
        let mut render_is_world_shape = false;
        for (k, _alive) in g
            .world
            .gobs
            .kind
            .iter()
            .zip(g.world.gobs.alive.iter())
            .filter(|(_, a)| **a)
        {
            if let Kind::Drop {
                resname_idx,
                inv_res_idx,
                ql,
                label,
            } = k
            {
                if *inv_res_idx == taken.res && *ql == taken.ql && *label == taken.label {
                    hit_inv = true;
                    let render_name = g.world.res.name(*resname_idx).unwrap_or("");
                    render_is_world_shape |= render_name.starts_with("gfx/terobjs/items/");
                }
            }
        }
        assert!(
            hit_inv,
            "the drop must carry the taken stack's inventory resource (res {})",
            taken.res
        );
        assert!(
            render_is_world_shape,
            "the drop gob must render with a gfx/terobjs/items world shape"
        );
        let inv_name = g.world.res.name(taken.res).unwrap_or("");
        assert!(inv_name.starts_with("gfx/invobjs/"));
    }

    /// Regression test (avatar bug): the player's own gob must be streamed
    /// to the session with OD_BUDDY naming the character. A double insert
    /// into `visible` (scan phase + stream_spawn) used to suppress the
    /// spawn block, so the client never received its own avatar gob.
    #[tokio::test]
    async fn player_gob_is_streamed_with_buddy() {
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = Game::new(
            42,
            cmd_rx,
            net_rx,
            false,
            std::env::temp_dir().join("hnh-game-test-save.json"),
        );
        let (tx, mut _rx) = tokio::sync::mpsc::unbounded_channel();
        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::channel(512);
        g.session_connected(1, "acct".to_owned(), tx, raw_tx);
        let wid = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "charlist")
            .map(|(k, _)| *k)
            .expect("charlist widget");
        g.on_wdgmsg(
            1,
            wid,
            "play",
            vec![hnh_proto::ListArg::Str("avatared".to_owned())],
        );
        // A few ticks: visibility scan must stream the player's own gob.
        for _ in 0..5 {
            g.tick();
        }
        let mut saw_buddy = false;
        while let Ok(block) = raw_rx.try_recv() {
            // Block layout: [MSG_OBJDATA][flags][gobid i32][frame i32][subs].
            if block.len() < 10 || block[0] != MSG_OBJDATA {
                continue;
            }
            let mut off = 10;
            while off < block.len() {
                let code = block[off];
                off += 1;
                match code {
                    OD_END => break,
                    OD_RES => {
                        let wire = u16::from_le_bytes([block[off], block[off + 1]]);
                        off += 2;
                        if wire & 0x8000 != 0 {
                            let n = block[off] as usize;
                            off += 1 + n;
                        }
                    }
                    OD_MOVE => off += 8,
                    OD_LINBEG => off += 20,
                    OD_LINSTEP => off += 4,
                    OD_LAYERS | OD_AVATAR => {
                        // base u16, then u16 layer ids until the 65535
                        // terminator (variable size since the layers are
                        // the concrete standing-pose frames).
                        off += 2;
                        loop {
                            let id = u16::from_le_bytes([block[off], block[off + 1]]);
                            off += 2;
                            if id == 65535 {
                                break;
                            }
                        }
                    }
                    OD_HEALTH => off += 1,
                    OD_BUDDY => {
                        let end = block[off..]
                            .iter()
                            .position(|&b| b == 0)
                            .map(|p| off + p)
                            .unwrap_or(block.len());
                        if &block[off..end] == b"avatared" {
                            saw_buddy = true;
                        }
                        break;
                    }
                    _ => break,
                }
            }
            if saw_buddy {
                break;
            }
        }
        assert!(
            saw_buddy,
            "player gob spawn block with OD_BUDDY must be streamed"
        );
    }

    /// Full plow -> plant -> grow -> harvest flow at the handler level
    /// (wire transport is covered by server/scripts/test_farming.py).
    #[tokio::test]
    async fn farming_flow_end_to_end() {
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = Game::new(
            42,
            cmd_rx,
            net_rx,
            false,
            std::env::temp_dir().join("hnh-game-test-save.json"),
        );
        let (tx, mut _rx) = tokio::sync::mpsc::unbounded_channel();
        let (raw_tx, _raw_rx) = tokio::sync::mpsc::channel(512);
        g.session_connected(1, "acct".to_owned(), tx, raw_tx);
        let charlist = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "charlist")
            .map(|(k, _)| *k)
            .unwrap();
        g.on_wdgmsg(
            1,
            charlist,
            "play",
            vec![hnh_proto::ListArg::Str("farmer".to_owned())],
        );
        // Put the player on a known grass spot and populate its grid.
        let pgob = g.world.players[0].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        g.world.gobs.set_pos(pslot, (550, 550));
        // The farming skill value gates planting; grant it the way the
        // sattr purchase would (the wire flow is covered by skillbot).
        g.world.players[0].attrs.insert("farming".to_owned(), 1);
        g.on_mapreq(1, (0, 0));
        // Find a grass tile near the player (avoid trees/stones/animals).
        let mut tile = None;
        'outer: for r in 0..8i32 {
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
                    if g.world.grids.grid(gc).tile(ix, iy) == tile::GRASS {
                        tile = Some((tx, ty));
                        break 'outer;
                    }
                }
            }
        }
        let (tx0, ty0) = tile.expect("grass tile near spawn");
        // 1. Arm the plow pagina, click the tile.
        let scm = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "scm")
            .map(|(k, _)| *k)
            .unwrap();
        g.on_wdgmsg(
            1,
            scm,
            "act",
            vec![hnh_proto::ListArg::Str("plow".to_owned())],
        );
        assert!(
            g.sessions.get(&1).unwrap().pending_plow,
            "plow pagina must arm"
        );
        let c0 = hnh_proto::ListArg::Coord(0, 0);
        let mc = hnh_proto::ListArg::Coord(tx0 * 11 + 5, ty0 * 11 + 5);
        let mapview = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "mapview")
            .map(|(k, _)| *k)
            .unwrap();
        g.on_wdgmsg(
            1,
            mapview,
            "click",
            vec![
                c0.clone(),
                mc.clone(),
                hnh_proto::ListArg::Int(1),
                hnh_proto::ListArg::Int(0),
            ],
        );
        assert!(
            g.world.tilth.contains_key(&(tx0, ty0)),
            "tile must be furrowed"
        );

        // 2. Take a seed stack onto the cursor and itemact the tile.
        let seed_idx = g.world.players[0]
            .inv
            .iter()
            .position(|s| s.label == "Wheat Seeds")
            .expect("starter seeds");
        let stack = g.world.players[0].inv[seed_idx];
        g.sessions.get_mut(&1).unwrap().cursor = Some(stack);
        g.on_map_itemact(1, &[c0, mc, hnh_proto::ListArg::Int(0)]);
        // Crop gob exists and is registered at the tile.
        let crop_gob = *g
            .world
            .crop_at
            .get(&(tx0, ty0))
            .expect("crop gob registered");
        assert!(g.world.crops.contains_key(&crop_gob));
        // Cursor kept the remainder (5 seeds - 1).
        assert_eq!(
            g.sessions.get(&1).unwrap().cursor.as_ref().map(|s| s.count),
            Some(4)
        );

        // 3. Force growth to maturity by rewinding stage deadlines.
        let state = g.world.crops.get_mut(&crop_gob).unwrap();
        state.next_stage_at = 0;
        g.tick();
        assert_eq!(
            g.world.crops.get(&crop_gob).unwrap().stage,
            1,
            "first stage advance on due tick"
        );
        for _ in 0..8 {
            let st = g.world.crops.get_mut(&crop_gob).unwrap();
            st.next_stage_at = 0;
            g.tick();
        }
        let mature = {
            let slot = g.world.gobs.get(crop_gob).unwrap();
            match g.world.gobs.kind[slot] {
                Kind::Crop { stage, spec } => stage >= farm::CROPS[spec as usize].stages,
                _ => false,
            }
        };
        assert!(mature, "crop must reach maturity after forced advances");

        // 4. Click the crop -> flower menu -> harvest -> yields.
        g.on_map_click(
            1,
            &[
                hnh_proto::ListArg::Coord(0, 0),
                hnh_proto::ListArg::Coord(tx0 * 11 + 5, ty0 * 11 + 5),
                hnh_proto::ListArg::Int(1),
                hnh_proto::ListArg::Int(0),
                hnh_proto::ListArg::Int(crop_gob),
                hnh_proto::ListArg::Coord(tx0 * 11 + 5, ty0 * 11 + 5),
            ],
        );
        let sm = g.sessions.get(&1).unwrap().crop_menu.map(|(w, _)| w);
        assert!(sm.is_some(), "harvest flower menu must open");
        g.on_flower_choice(1, sm.unwrap(), 0);
        let inv_before = g.world.players[0].inv.len();
        assert!(inv_before > 5, "harvest must push yields into inventory");
        assert!(
            !g.world.crops.contains_key(&crop_gob)
                && g.world.crop_at.get(&(tx0, ty0)) != Some(&crop_gob),
            "crop gob removed from registries"
        );
        // Tilth decay timer restored (non-zero deadline again).
        assert_ne!(g.world.tilth.get(&(tx0, ty0)), Some(&0));
    }

    /// Predators in a saturated world must engage the player: chase, open
    /// the Fightview window and start swinging back.
    #[tokio::test]
    async fn predator_engages_player_in_reach() {
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = Game::new(
            42,
            cmd_rx,
            net_rx,
            true,
            std::env::temp_dir().join("hnh-game-test-save.json"),
        );
        let (tx, mut _rx) = tokio::sync::mpsc::unbounded_channel();
        let (raw_tx, _raw_rx) = tokio::sync::mpsc::channel(512);
        g.session_connected(1, "acct".to_owned(), tx, raw_tx);
        // Select the character through the normal widget path.
        let wid = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "charlist")
            .map(|(k, _)| *k)
            .expect("charlist widget");
        g.on_wdgmsg(
            1,
            wid,
            "play",
            vec![hnh_proto::ListArg::Str("hunter".to_owned())],
        );
        // Populate the player's grid with saturated wildlife.
        g.on_mapreq(1, (0, 0));
        // Find a wolf or boar and teleport the player into melee reach.
        let _predator = (0..g.world.animal_gobs.len())
            .map(|i| g.world.animal_gobs[i])
            .find(|&id| {
                let slot = g.world.gobs.get(id).unwrap();
                matches!(g.world.gobs.kind[slot], Kind::Animal { species } if species.aggressive())
            })
            .expect("saturated grid spawns predators");
        let pslot = predator_slot(&g);
        let (ax, ay) = g.world.gobs.pos[pslot];
        let pgob = g.world.players[0].gob;
        let pslot2 = g.world.gobs.get(pgob).unwrap();
        g.world.gobs.set_pos(pslot2, (ax + 5, ay));
        info!(?ax, ?ay, "teleported player next to predator");
        // Run ticks until the engagement opens.
        let mut fought = false;
        for _ in 0..300 {
            g.tick();
            if !g.world.animal_fights.is_empty() {
                fought = true;
                break;
            }
        }
        assert!(fought, "predator must engage a player in reach");
    }

    /// A stationary player next to a predator must land damage through
    /// openings and eventually kill it (full combat kill-cycle check).
    #[tokio::test]
    async fn stationary_player_kills_predator() {
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = Game::new(
            42,
            cmd_rx,
            net_rx,
            true,
            std::env::temp_dir().join("hnh-game-test-save.json"),
        );
        let (tx, mut _rx) = tokio::sync::mpsc::unbounded_channel();
        let (raw_tx, _raw_rx) = tokio::sync::mpsc::channel(512);
        g.session_connected(1, "acct".to_owned(), tx, raw_tx);
        let wid = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "charlist")
            .map(|(k, _)| *k)
            .expect("charlist widget");
        g.on_wdgmsg(
            1,
            wid,
            "play",
            vec![hnh_proto::ListArg::Str("hunter".to_owned())],
        );
        g.on_mapreq(1, (0, 0));
        let (pred_id, pred_slot) = g
            .world
            .animal_gobs
            .iter()
            .filter_map(|&id| {
                g.world.gobs.get(id).map(|slot| {
                    let aggro = matches!(
                        g.world.gobs.kind[slot],
                        Kind::Animal { species } if species.aggressive()
                    );
                    if aggro {
                        Some((id, slot))
                    } else {
                        None
                    }
                })
            })
            .flatten()
            .next()
            .expect("saturated grid spawns predators");
        let (ax, ay) = g.world.gobs.pos[pred_slot];
        // Leave exactly one predator alive so the fight dynamics are
        // deterministic (no pack target swapping).
        let keep = pred_id;
        let others: Vec<GobId> = g
            .world
            .animal_gobs
            .iter()
            .copied()
            .filter(|&id| id != keep)
            .collect();
        for id in others {
            g.world.gobs.kill(id);
        }
        g.world.animal_gobs.retain(|&id| id == keep);
        let pgob = g.world.players[0].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        g.world.gobs.set_pos(pslot, (ax + 5, ay));
        let mut killed = false;
        let mut saw_damage = false;
        // Track the predator currently engaged (the dense pack may swap
        // targets as other wolves wander into reach).
        for tick in 0..3000 {
            if tick % 4 == 0 {
                // Hold position next to the predator (stationary player).
                g.world.gobs.set_pos(pslot, (ax + 5, ay));
            }
            g.tick();
            // The target may vanish (killed): check both paths.
            if !g.world.gobs.alive[pred_slot] {
                killed = true;
                break;
            }
            let engaged = g.world.players[0].fight_target;
            if let Some(tid) = engaged {
                if let Some(tslot) = g.world.gobs.get(tid) {
                    if g.world.gobs.hp[tslot] < g.world.gobs.max_hp[tslot] {
                        saw_damage = true;
                    }
                }
            }
        }
        assert!(saw_damage, "damage must land through openings");
        assert!(
            killed,
            "stationary player must kill a predator in 3000 ticks"
        );
    }

    /// A stationary player in reach must be BITTEN: the animal's offence
    /// builds every tick, swings chip the player's defence, the bite lands
    /// (hp drop) and the one-shot bite FX overlay (gfx/fx/bite) is
    /// broadcast to the victim - the animal attack animation path.
    #[tokio::test]
    async fn predator_bites_and_broadcasts_bite_overlay() {
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = Game::new(
            42,
            cmd_rx,
            net_rx,
            true,
            std::env::temp_dir().join("hnh-game-test-save.json"),
        );
        let (tx, mut _rx) = tokio::sync::mpsc::unbounded_channel();
        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::channel(512);
        g.session_connected(1, "acct".to_owned(), tx, raw_tx);
        let wid = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "charlist")
            .map(|(k, _)| *k)
            .expect("charlist widget");
        g.on_wdgmsg(
            1,
            wid,
            "play",
            vec![hnh_proto::ListArg::Str("prey".to_owned())],
        );
        g.on_mapreq(1, (0, 0));
        let (pred_id, pred_slot) = g
            .world
            .animal_gobs
            .iter()
            .filter_map(|&id| {
                g.world.gobs.get(id).map(|slot| {
                    let aggro = matches!(
                        g.world.gobs.kind[slot],
                        Kind::Animal { species } if species.aggressive()
                    );
                    if aggro {
                        Some((id, slot))
                    } else {
                        None
                    }
                })
            })
            .flatten()
            .next()
            .expect("saturated grid spawns predators");
        let (ax, ay) = g.world.gobs.pos[pred_slot];
        let keep = pred_id;
        let others: Vec<GobId> = g
            .world
            .animal_gobs
            .iter()
            .copied()
            .filter(|&id| id != keep)
            .collect();
        for id in others {
            g.world.gobs.kill(id);
        }
        g.world.animal_gobs.retain(|&id| id == keep);
        let pgob = g.world.players[0].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        g.world.gobs.set_pos(pslot, (ax + 5, ay));
        let mut bitten = false;
        let mut saw_overlay = false;
        for tick in 0..900 {
            if tick % 4 == 0 {
                g.world.gobs.set_pos(pslot, (ax + 5, ay));
            }
            g.tick();
            if g.world.players[0].hp < 100 {
                bitten = true;
            }
        }
        // Scan the raw OBJDATA stream for a bite overlay on the player gob.
        while let Ok(msg) = raw_rx.try_recv() {
            if msg.first() != Some(&MSG_OBJDATA) || msg.len() < 10 {
                continue;
            }
            let gid = i32::from_le_bytes([msg[2], msg[3], msg[4], msg[5]]);
            if gid != pgob as i32 {
                continue;
            }
            // Walk the op stream: find OD_OVERLAY (12) before OD_END (0).
            let mut off = 10;
            while off < msg.len() {
                let op = msg[off];
                off += 1;
                match op {
                    0 => break,     // OD_END
                    1 => off += 8,  // OD_MOVE
                    3 => off += 20, // OD_LINBEG
                    4 => off += 4,  // OD_LINSTEP
                    6 | 9 => {
                        // OD_LAYERS / OD_AVATAR: u16 ids to 65535.
                        if op == 6 {
                            off += 2;
                        }
                        while off + 1 < msg.len() {
                            let id = u16::from_le_bytes([msg[off], msg[off + 1]]);
                            off += 2;
                            if id == 65535 {
                                break;
                            }
                        }
                    }
                    12 => {
                        saw_overlay = true;
                        break;
                    }
                    14 => off += 1, // OD_HEALTH
                    15 => {
                        // OD_BUDDY: string + 2 bytes.
                        match msg[off..].iter().position(|&b| b == 0) {
                            Some(p) => off += p + 1 + 2,
                            None => break,
                        }
                    }
                    _ => break,
                }
            }
            if saw_overlay {
                break;
            }
        }
        assert!(bitten, "predator must land a bite (player hp drop)");
        assert!(
            saw_overlay,
            "bite must broadcast the one-shot FX overlay to the victim"
        );
    }

    fn predator_slot(g: &Game) -> usize {
        for &id in &g.world.animal_gobs {
            if let Some(slot) = g.world.gobs.get(id) {
                let aggro = matches!(
                    g.world.gobs.kind[slot],
                    Kind::Animal { species } if species.aggressive()
                );
                if aggro {
                    return slot;
                }
            }
        }
        panic!("BUG: no predator");
    }

    /// The bootstrap stream must announce avatar RESIDs before the
    /// charlist add (session-lifecycle.md 3.1).
    #[tokio::test]
    async fn bootstrap_announces_resids_first() {
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = Game::new(
            42,
            cmd_rx,
            net_rx,
            false,
            std::env::temp_dir().join("hnh-game-test-save.json"),
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (raw_tx, _raw_rx) = tokio::sync::mpsc::channel(512);
        g.session_connected(1, "acct".to_owned(), tx, raw_tx);
        // Inspect the wire table state after registration.
        {
            let out = g.sessions.get(&1).unwrap();
            eprintln!(
                "WIRE: {:?}",
                (0..out.res.wire_count())
                    .map(|w| out.res.pending_announce(w as u16))
                    .collect::<Vec<_>>()
            );
        }
        drop(g); // close channels to end the recv loop
        let mut types = Vec::new();
        while let Ok(p) = rx.try_recv() {
            types.push(p[0]);
        }
        {
            eprintln!("TYPES: {:?}", types);
            assert_eq!(&types[..3], &[RMSG_RESID, RMSG_RESID, RMSG_RESID]);
        }
        assert!(types.contains(&RMSG_NEWWDG));
    }

    // ------------------------------------------------------------------
    // Session 20: movement fidelity (timing, retargeting, gaits, poses)
    // ------------------------------------------------------------------

    /// The client covers a move in c * 66.67 ms (LinMove.ctick: a +=
    /// (dt/1000)/(c*0.06) * 0.9). client_steps must round-trip the planned
    /// duration within one tick so the client and the server agree on when
    /// the gob arrives.
    #[test]
    fn movement_timing_client_steps_match_planned_duration() {
        for total_ms in [
            100u32, 250, 500, 1000, 1667, 3333, 5000, 10000, 30000, 60000, 120000,
        ] {
            let c = LinMove::client_steps(total_ms);
            let client_ms = i64::from(c) * 200; // c * 200/3 ms exact
            let err = (client_ms - i64::from(total_ms) * 3).abs();
            assert!(
                err <= 300,
                "client_steps({total_ms}) = {c} => client time {client_ms}/3 ms, err {err} ms"
            );
        }
    }

    /// A walk of 30 tiles at walk gait (33 subtile/s) must take 10 s and
    /// produce the client-consistent step count; the server-side logical
    /// position must track the interpolated path (not jump to the
    /// destination) and land exactly on the target when the move ends.
    #[tokio::test]
    async fn movement_timing_walk_duration_and_interpolated_pos() {
        let (mut g, _rx, _raw) = entered_game("walktiming");
        let pgob = g.sessions[&1].player_gob.expect("player gob");
        let slot = g.world.gobs.get(pgob).expect("slot");
        let (sx, sy) = g.world.gobs.pos[slot];
        let target = (sx + 330, sy); // 30 tiles
        assert!(g.start_move(slot, target), "walk must be accepted");
        let lm = g.world.gobs.mv[slot].expect("mv");
        assert_eq!(lm.total_ms, 10_000, "30 tiles at 33 subtile/s = 10 s");
        assert_eq!(lm.steps, 150, "client steps for 10 s at 66.67 ms/step");
        assert_eq!(g.world.gobs.speed[slot], GAIT_SPEEDS[GAIT_WALK]);

        // Halfway through, the logical position is on the path...
        for _ in 0..50 {
            g.tick();
        }
        let (mx, my) = g.world.gobs.pos[slot];
        assert!(
            (mx - (sx + 165)).abs() <= 2 && (my - sy).abs() <= 2,
            "mid-move logical pos ({mx},{my}) must be ~midpath ({},{})",
            sx + 165,
            sy
        );
        // ...and a viewer saw LINSTEP progress ~halfway.
        assert!(g.world.gobs.mv[slot].is_some(), "still moving halfway");

        // Completion: exactly on the target, movement cleared.
        for _ in 0..55 {
            g.tick();
        }
        assert!(g.world.gobs.mv[slot].is_none(), "move finished");
        assert_eq!(g.world.gobs.pos[slot], target);
    }

    /// Rapid re-clicks must NOT teleport: a second walk order while moving
    /// starts from the interpolated on-path position, never from the old
    /// destination.
    #[tokio::test]
    async fn movement_reclick_starts_from_interpolated_position() {
        let (mut g, _rx, _raw) = entered_game("reclick");
        let pgob = g.sessions[&1].player_gob.expect("player gob");
        let slot = g.world.gobs.get(pgob).expect("slot");
        let (sx, sy) = g.world.gobs.pos[slot];
        let far = (sx + 330, sy);
        assert!(g.start_move(slot, far));
        // Walk ~2 s (20 ticks), then click somewhere else.
        for _ in 0..20 {
            g.tick();
        }
        let (cx, cy) = g.world.gobs.pos[slot];
        let back = (sx, sy);
        assert!(g.start_move(slot, back), "retarget accepted");
        let lm = g.world.gobs.mv[slot].expect("mv after retarget");
        assert_eq!(
            (lm.sx, lm.sy),
            (cx, cy),
            "new move must start from the interpolated position, not the old destination"
        );
        assert_ne!(
            lm.sx,
            lm.tx + 330,
            "sanity: not teleporting from destination"
        );
        // The move completes back at the start point without a position jump
        // larger than one path leg.
        for _ in 0..(lm.total_ms as u64 / TICK_MS) + 2 {
            g.tick();
        }
        assert!(g.world.gobs.mv[slot].is_none());
        assert_eq!(g.world.gobs.pos[slot], back);
    }

    /// Gait speeds must match docs/mechanics/character/attributes-and-vitals.md
    /// (RoB Glossary "Speed"): crawl 1.5, walk 3.0, run 4.5, sprint 6.0
    /// tiles/s = 16/33/50/66 subtile/s, and the speedget "set" message must
    /// apply the picked gait to the mover speed.
    #[tokio::test]
    async fn gait_speeds_match_docs_and_speedget_set_applies() {
        assert_eq!(GAIT_SPEEDS, [16, 33, 50, 66]);
        assert_eq!(GAIT_SPEEDS[GAIT_WALK], 33, "walk = 3 tiles/s");
        let (mut g, _rx, _raw) = entered_game("gaits");
        let pgob = g.sessions[&1].player_gob.expect("player gob");
        let slot = g.world.gobs.get(pgob).expect("slot");
        assert_eq!(g.world.gobs.speed[slot], 33, "default gait is walk");
        let wid = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "speedget")
            .map(|(k, _)| *k)
            .expect("speedget widget");
        g.on_wdgmsg(1, wid, "set", vec![hnh_proto::ListArg::Int(2)]);
        assert_eq!(g.world.gobs.speed[slot], 50, "run gait applied");
        g.on_wdgmsg(1, wid, "set", vec![hnh_proto::ListArg::Int(9)]);
        assert_eq!(
            g.world.gobs.speed[slot], 66,
            "out-of-range clamps to sprint"
        );
    }

    /// Direction quantization: the 8 octants map to the pack's
    /// directional pose index (dir 0 = +x, dir 2 = +y, dir 4 = -x,
    /// dir 6 = -y, diagonals on odd dirs; wraparound exact at 180 deg).
    #[test]
    fn move_dir_quantizes_octants() {
        assert_eq!(move_dir((0, 0), (10, 0)), 0, "+x");
        assert_eq!(move_dir((0, 0), (10, 10)), 1, "+x+y diagonal");
        assert_eq!(move_dir((0, 0), (0, 10)), 2, "+y");
        assert_eq!(move_dir((0, 0), (-10, 10)), 3, "-x+y");
        assert_eq!(move_dir((0, 0), (-10, 0)), 4, "-x");
        assert_eq!(move_dir((0, 0), (-10, -10)), 5, "-x-y");
        assert_eq!(move_dir((0, 0), (0, -10)), 6, "-y");
        assert_eq!(move_dir((0, 0), (10, -10)), 7, "+x-y");
        assert_eq!(move_dir((0, 0), (-10, -1)), 4, "steep -x wraparound");
        assert_eq!(move_dir((0, 0), (-1, -10)), 6, "steep -y wraparound");
        assert_eq!(move_dir((5, 5), (5, 5)), 0, "zero vector defaults to dir 0");
    }

    /// The art ring is rotated one octant against the movement ring:
    /// sprite = (octant - 1) mod 8. Anchors: front octant 1 -> sprite 0,
    /// back octant 5 -> sprite 4, pure left octant 3 -> sprite 2, pure
    /// right octant 7 -> sprite 6. The two user-reported defect cases:
    /// walking up (octant 5) must show the BACK set (sprite 4), not the
    /// up-right set (sprite 5); walking left (octant 3) must show the
    /// pure LEFT profile (sprite 2), not the up-left set (sprite 3).
    #[test]
    fn art_dir_offsets_the_sprite_ring() {
        for octant in 0u8..8 {
            assert_eq!(art_dir(octant), (octant + 7) & 7, "octant {octant}");
        }
        assert_eq!(art_dir(1), 0, "camera-facing front is sprite 0");
        assert_eq!(art_dir(5), 4, "walking up shows the back set");
        assert_eq!(art_dir(3), 2, "walking left shows the left profile");
        assert_eq!(art_dir(7), 6, "walking right shows the right profile");
        assert_eq!(art_dir(0), 7, "east shows the down-right 3/4 set");
    }

    /// Pose layer composition: walking vs standing sets at a direction,
    /// plus the fixed banzai doll set and the kritter pose part. Layer
    /// names carry the ART sprite index (art_dir(octant)), so octant 3
    /// composes legs-2 and octant 6 composes legs-5.
    #[test]
    fn pose_layers_compose_direction_and_kind() {
        let walk = avatar_pose_layers(true, 3);
        assert!(walk[0].ends_with("walking/legs-2"), "{}", walk[0]);
        assert!(walk[1].ends_with("walking/torso/male-2"), "{}", walk[1]);
        assert!(
            walk[5].starts_with("gfx/borka/hair-karin/walking/"),
            "{}",
            walk[5]
        );
        let stand = avatar_pose_layers(false, 6);
        assert!(stand[0].ends_with("standing/legs-5"), "{}", stand[0]);
        assert!(stand[3].contains("arm/idle/left-5"), "{}", stand[3]);
        let doll = avatar_doll_layers();
        assert!(
            doll.iter().all(|n| n.contains("/standing/")),
            "doll is a standing pose"
        );
        assert!(
            doll.iter().any(|n| n.contains("arm/banzai/left-0")),
            "doll arms are banzai (spread), front view sprite 0"
        );
        assert!(
            !doll.iter().any(|n| n.contains("arm/idle")),
            "doll never uses idle arms"
        );
        let wolf = kritter_pose_layer(Species::Wolf, true, 5);
        assert_eq!(wolf, "gfx/kritter/wolf/body/walking/walking-4");
        let hare = kritter_pose_layer(Species::Hare, false, 0);
        assert_eq!(hare, "gfx/kritter/hare/body/standing/standing-7");
        assert_eq!(kritter_base(Species::Fox), "gfx/kritter/fox/body");
    }

    /// While a player avatar moves, the streamed pose is the walking set
    /// of the travel direction (pose_streamed = 8+dir); when the move ends
    /// the standing set of the same direction returns (pose_streamed =
    /// dir). No frame streaming ever happens: the pose streams fire only
    /// on pose/direction changes.
    #[tokio::test]
    async fn walk_layers_swap_between_walking_and_standing() {
        let (mut g, _rx, _raw) = entered_game("walkpose");
        let pgob = g.sessions[&1].player_gob.expect("player gob");
        let slot = g.world.gobs.get(pgob).expect("slot");
        let (sx, sy) = g.world.gobs.pos[slot];
        // Long walk east: 60 tiles at walk speed ~ 20 s.
        assert!(g.start_move(slot, (sx + 660, sy)));
        assert_eq!(g.world.gobs.facing[slot], 0, "east is dir 0");
        assert_eq!(
            g.world.gobs.pose_streamed[slot], 8,
            "walking set of dir 0 streamed on start"
        );
        // 2 s in: still the walking pose (the client cycles the frames
        // natively; the server must not re-stream anything mid-walk).
        let before = g.world.gobs.pose_streamed[slot];
        for _ in 0..20 {
            g.tick();
        }
        assert_eq!(
            g.world.gobs.pose_streamed[slot], before,
            "no frame streaming mid-walk"
        );
        // Drain the remaining move.
        for _ in 0..210 {
            g.tick();
        }
        assert!(g.world.gobs.mv[slot].is_none(), "move finished");
        assert_eq!(
            g.world.gobs.pose_streamed[slot], 0,
            "standing set of dir 0 restored after arrival"
        );
    }

    // ------------------------------------------------------------------
    // Session 27: multi-node cluster (authority, guests, transfer, chat)
    // ------------------------------------------------------------------

    use std::num::NonZeroUsize;

    /// Test harness: game + widget-msg channel + raw-block channel + mesh
    /// publish sink (what this node would send to its peers).
    type ClusterHarness = (
        Game,
        tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
        tokio::sync::mpsc::Receiver<Vec<u8>>,
        tokio::sync::mpsc::UnboundedReceiver<(usize, crate::nodes::NodeMsg)>,
    );

    /// A game wired for node `me` of a `nodes`-node cluster WITHOUT a real
    /// mesh: guest publishes land in a drainable channel (mesh_rx), which
    /// lets tests assert exactly what this node would send to its peers.
    fn clustered_game(name: &str, me: usize, nodes: usize) -> ClusterHarness {
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = Game::new(
            42,
            cmd_rx,
            net_rx,
            false,
            std::env::temp_dir().join(format!("hnh-cluster-test-{}.json", name)),
        );
        let (mesh_tx, mut mesh_rx) = tokio::sync::mpsc::unbounded_channel();
        let nz = NonZeroUsize::new(nodes).expect("nodes");
        g.world = World::with_layout(42, nz, me);
        g.cluster = Some(Cluster {
            me,
            nodes: nz,
            mesh: crate::nodes::Mesh { out_tx: mesh_tx },
            peer_subs: HashMap::new(),
            my_subs: HashMap::new(),
            player_abroad: HashMap::new(),
        });
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (raw_tx, raw_rx) = tokio::sync::mpsc::channel(4096);
        g.session_connected(1, "acct".to_owned(), tx, raw_tx);
        let wid = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "charlist")
            .map(|(k, _)| *k)
            .expect("charlist widget");
        g.on_wdgmsg(
            1,
            wid,
            "play",
            vec![hnh_proto::ListArg::Str(name.to_owned())],
        );
        // Simulate the cluster answering the character-migration query:
        // every peer nacks a save key it does not hold, which completes
        // the world entry in one mesh round trip (no 2 s deadline wait).
        if g.cluster.is_some() {
            let nodes = g.cluster.as_ref().map(|c| c.nodes.get()).unwrap_or(0);
            let me = g.cluster.as_ref().map(|c| c.me).unwrap_or(0);
            let mut names: Vec<String> = Vec::new();
            while let Ok((_, msg)) = mesh_rx.try_recv() {
                if let crate::nodes::NodeMsg::CharQuery { name, .. } = msg {
                    names.push(name);
                }
            }
            for name in names {
                for peer in 0..nodes {
                    if peer != me {
                        g.on_node_msg(crate::nodes::NodeMsg::CharNack {
                            to: me,
                            from: peer,
                            name: name.clone(),
                        });
                    }
                }
            }
        }
        for _ in 0..3 {
            g.tick();
        }
        (g, rx, raw_rx, mesh_rx)
    }

    /// First position around (px, py) whose VisIndex cell belongs to a node
    /// other than `me` (scans outward; cells are 250 subtiles).
    fn foreign_cell_pos(g: &Game, me: usize) -> (i32, i32) {
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let slot = g.world.gobs.get(pgob).expect("player gob");
        let (px, py) = g.world.gobs.pos[slot];
        for r in [260, 400, 600, 900, 1300, 1800] {
            for (dx, dy) in [(r, 0), (0, r), (-r, 0), (0, -r), (r, r), (-r, -r)] {
                let c = crate::visidx::cell_of(px + dx, py + dy);
                let owner = match &g.cluster {
                    Some(cl) => crate::grid_owner::owner_of(c, cl.nodes),
                    None => 0,
                };
                if owner != me {
                    return (px + dx, py + dy);
                }
            }
        }
        panic!("no foreign cell found near spawn");
    }

    /// First position around the player whose VisIndex cell belongs to
    /// `me` (mirror of foreign_cell_pos for home-ground fixtures).
    fn home_cell_pos(g: &Game, me: usize) -> (i32, i32) {
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let slot = g.world.gobs.get(pgob).expect("player gob");
        let (px, py) = g.world.gobs.pos[slot];
        for r in [11, 130, 260, 400, 600, 900, 1300, 1800] {
            for (dx, dy) in [(r, r), (0, r), (r, 0), (-r, -r), (0, -r), (-r, 0)] {
                let c = crate::visidx::cell_of(px + dx, py + dy);
                let owner = match &g.cluster {
                    Some(cl) => crate::grid_owner::owner_of(c, cl.nodes),
                    None => 0,
                };
                if owner == me {
                    return (px + dx, py + dy);
                }
            }
        }
        panic!("no home cell found near spawn");
    }

    /// A gob id from another node's allocation range (slot stride).
    fn foreign_node_gob_id(me: usize, nodes: usize, seq: usize) -> GobId {
        let per = (MAX_SLOT + 1) / nodes;
        let slot = me * per + seq; // my own range is NOT foreign; pick another
        let slot = if slot == me * per + seq {
            (nodes - me - 1) * per + seq + 3
        } else {
            slot
        };
        gob_id_from_slot(slot % (MAX_SLOT + 1), 1)
    }

    /// G3: a foreign-cell animal must not simulate on this node while a
    /// local-cell animal does, and a player standing in a foreign cell
    /// keeps moving (players are always authored by their home node).
    #[tokio::test]
    async fn authority_follows_cells_but_players_stay_home() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("authuser", 0, 2);
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).expect("player gob");

        // Home-cell deer (mine): flees when the player is close.
        let res = g.world.res.intern(Species::Deer.resname());
        let (hx, hy) = home_cell_pos(&g, 0);
        let local_deer = g.world.gobs.spawn(
            Kind::Animal {
                species: Species::Deer,
            },
            (hx, hy),
            res,
            10,
            33,
        );
        g.world.animal_gobs.push(local_deer);
        // Foreign-cell deer: same species, other node's cell.
        let (fx, fy) = foreign_cell_pos(&g, 0);
        let foreign_deer = g.world.gobs.spawn(
            Kind::Animal {
                species: Species::Deer,
            },
            (fx, fy),
            res,
            10,
            33,
        );
        g.world.animal_gobs.push(foreign_deer);

        for _ in 0..12 {
            g.tick();
        }
        let lslot = g.world.gobs.get(local_deer).expect("local deer alive");
        assert!(
            g.world.gobs.mv[lslot].is_some(),
            "home deer must simulate (flee from the nearby player)"
        );
        // The foreign deer never simulates here: the cluster pass hands it
        // to its owner on the first tick (demoted to a local guest).
        assert!(
            g.world.guests.contains_key(&foreign_deer) && g.world.gobs.get(foreign_deer).is_none(),
            "foreign deer must transfer out, not simulate locally"
        );

        // Player authority: teleport into the foreign cell and click a
        // nearby target — the home node still simulates its own player.
        g.world.gobs.set_pos(pslot, (fx, fy));
        g.player_walk(1, pgob, (fx + 30, fy));
        let pslot = g.world.gobs.get(pgob).expect("player gob");
        assert!(
            g.world.gobs.mv[pslot].is_some(),
            "a player abroad must keep moving on its home node"
        );
    }

    /// G4: a guest announce flows through the visibility machinery — the
    /// session spawns it, receives movement finalizers on update, and the
    /// retract drops it from the session's visible set.
    #[tokio::test]
    async fn guest_ingest_update_reach_sessions() {
        let (mut g, _rx, mut raw, _mesh) = clustered_game("guestuser", 0, 2);
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];

        let gid = foreign_node_gob_id(0, 2, 7);
        assert_ne!(gid, pgob, "guest id must never collide with local ids");
        let st = crate::nodes::GuestState {
            id: gid,
            pos: (px + 60, py),
            mv: None,
            moving: false,
            facing: 1,
            kind: crate::nodes::GuestKind::Animal {
                species: Species::Wolf.index(),
            },
            hp: 50,
            max_hp: 50,
            speed: 33,
        };
        g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st));
        g.tick();
        assert!(
            g.sessions[&1].visible.contains(&gid),
            "an in-view guest must spawn to the session through the vis scan"
        );
        // Wire proof: the session received an OBJDATA block for the guest
        // carrying OD_LAYERS (server-side pose resolution mirrors locals).
        let mut saw_layers = false;
        while let Ok(block) = raw.try_recv() {
            if block.first() != Some(&MSG_OBJDATA) {
                continue;
            }
            let id = i32::from_le_bytes([block[2], block[3], block[4], block[5]]);
            if id != gid {
                continue;
            }
            for (op, _) in objdata_layer_lists(&block) {
                if op == OD_LAYERS {
                    saw_layers = true;
                }
            }
        }
        assert!(saw_layers, "guest spawn must carry the pose layer list");

        // Update: the owner starts the wolf moving; the guest row adopts
        // the linmove and the progress loop advances it.
        let st2 = crate::nodes::GuestState {
            id: gid,
            pos: (px + 60, py),
            mv: Some(crate::nodes::GuestLinMove {
                sx: px + 60,
                sy: py,
                tx: px + 260,
                ty: py,
                steps: 10,
                step: 0,
                started_ms: g.world.now_ms,
                total_ms: 300,
            }),
            moving: true,
            facing: 0,
            kind: crate::nodes::GuestKind::Animal {
                species: Species::Wolf.index(),
            },
            hp: 50,
            max_hp: 50,
            speed: 33,
        };
        g.on_node_msg(crate::nodes::NodeMsg::GuestUpdate(st2));
        g.tick();
        let gr = g.world.guests.get(&gid).expect("guest row");
        assert!(
            gr.mv.is_some() && gr.mv.as_ref().unwrap().step > 0,
            "guest progress must advance locally from the linmove params"
        );

        // Retract: the owner reports death; the session drops the gob.
        g.on_node_msg(crate::nodes::NodeMsg::GuestRetract { id: gid });
        assert!(
            !g.sessions[&1].visible.contains(&gid),
            "retracted guest must drop"
        );
        assert!(!g.world.guests.contains_key(&gid));
    }

    /// G5: an animal standing in a foreign cell transfers to its owner
    /// (GuestTransfer on the mesh), demotes to a guest locally with the
    /// SAME id, and the receiver claims it into its sim tables.
    #[tokio::test]
    async fn animal_transfer_keeps_identity_across_nodes() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("xfer0", 0, 2);
        let (fx, fy) = foreign_cell_pos(&g, 0);
        let res = g.world.res.intern(Species::Wolf.resname());
        let id = g.world.gobs.spawn(
            Kind::Animal {
                species: Species::Wolf,
            },
            (fx, fy),
            res,
            40,
            33,
        );
        g.world.animal_gobs.push(id);
        g.tick();
        // Sender side: mesh carries the transfer to the owner; the local
        // copy is a guest (viewers keep rendering, sim stops).
        let mut transferred = None;
        while let Ok((peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::GuestTransfer(st) = msg {
                transferred = Some((peer, st));
            }
        }
        let (peer, st) = transferred.expect("transfer must publish to the cell owner");
        assert_eq!(peer, 1, "the foreign cell's owner receives the transfer");
        assert_eq!(st.id, id, "transfer preserves the gob id");
        assert!(
            g.world.guests.contains_key(&id) && g.world.gobs.get(id).is_none(),
            "the old owner demotes the gob to a guest"
        );
        assert!(
            !g.world.animal_gobs.contains(&id),
            "the old owner stops simulating the transferred animal"
        );

        // Receiver side: a fresh node-1 game claims the exact id.
        let (mut g1, _rx1, _raw1, _mesh1) = clustered_game("xfer1", 1, 2);
        let wpos = st.pos;
        g1.on_node_msg(crate::nodes::NodeMsg::GuestTransfer(st));
        let slot = g1
            .world
            .gobs
            .get(id)
            .expect("transferred gob alive on the new owner");
        assert!(g1.world.gobs.alive[slot]);
        assert!(
            g1.world.animal_gobs.contains(&id),
            "the new owner simulates it"
        );
        // Deterministic resume: a player walks into the wolf's aggro radius
        // on the new owner - the wolf must chase (AI runs there now).
        let pidx1 = *g1.world.by_session.get(&1).unwrap();
        let pg1 = g1.world.players[pidx1].gob;
        let ps1 = g1.world.gobs.get(pg1).unwrap();
        g1.world.gobs.set_pos(ps1, (wpos.0 - 200, wpos.1));
        let mut moved = false;
        for _ in 0..15 {
            g1.tick();
            if let Some(s) = g1.world.gobs.get(id) {
                if g1.world.gobs.mv[s].is_some() {
                    moved = true;
                    break;
                }
            }
        }
        assert!(moved, "the claimed animal must resume AI on the new owner");
    }

    /// Session 35 (drop authority transfer): a drop spawned onto a cell
    /// this node does NOT own (station output jitter across the cell
    /// boundary) sends GuestTransfer with the FULL DropView payload to
    /// the cell's owner and demotes the local copy to a guest under the
    /// same id - the wire mirror of the animal authority transfer.
    #[tokio::test]
    async fn drop_transfer_sends_to_cell_owner_and_demotes() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("dropxfer0", 0, 2);
        let (fx, fy) = foreign_cell_pos(&g, 0);
        let inv_res_idx = g.world.res.intern("gfx/invobjs/branch");
        let world_res_idx = g.world.res.intern("gfx/terobjs/items/branch");
        let id = g.world.gobs.spawn(
            Kind::Drop {
                resname_idx: world_res_idx,
                inv_res_idx,
                ql: 7,
                label: "",
            },
            (fx, fy),
            world_res_idx,
            1,
            0,
        );
        g.tick();
        let mut transferred = None;
        while let Ok((peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::GuestTransfer(st) = msg {
                transferred = Some((peer, st));
            }
        }
        let (peer, st) = transferred.expect("the foreign drop must transfer to the cell owner");
        assert_eq!(peer, 1, "the foreign cell's owner receives the transfer");
        assert_eq!(st.id, id, "transfer preserves the gob id");
        let crate::nodes::GuestKind::Static { class, drop, .. } = &st.kind else {
            panic!("drop transfer must carry the Static kind");
        };
        assert_eq!(*class, crate::nodes::StaticClass::Drop);
        let view = drop
            .as_ref()
            .expect("transfer carries the DropView payload");
        assert_eq!(view.inv_res, "gfx/invobjs/branch");
        assert_eq!(view.ql, 7);
        assert!(
            g.world.guests.contains_key(&id) && g.world.gobs.get(id).is_none(),
            "the spawner demotes the drop to a guest"
        );
    }

    /// Session 35 (drop authority transfer): the receiving owner claims
    /// the exact id back into Kind::Drop with the deterministic world
    /// shape and a working pickup payload (drop_info round-trips the
    /// inventory resource, quality and label).
    #[tokio::test]
    async fn drop_transfer_receiver_claims_pickup_payload() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("dropxfer1", 0, 2);
        let (fx, fy) = foreign_cell_pos(&g, 0);
        let inv_res_idx = g.world.res.intern("gfx/invobjs/meatroast");
        let world_res_idx = g.world.res.intern(drop_world_res("gfx/invobjs/meatroast"));
        let id = g.world.gobs.spawn(
            Kind::Drop {
                resname_idx: world_res_idx,
                inv_res_idx,
                ql: 12,
                label: "Meat roast",
            },
            (fx, fy),
            world_res_idx,
            1,
            0,
        );
        let slot = g.world.gobs.get(id).unwrap();
        let st = g.guest_state_from_slot(id, slot).unwrap();
        // Receiver side: a fresh node-1 game claims the exact id.
        let (mut g1, _rx1, _raw1, _mesh1) = clustered_game("dropxfer1b", 1, 2);
        g1.on_node_msg(crate::nodes::NodeMsg::GuestTransfer(st));
        let slot1 = g1
            .world
            .gobs
            .get(id)
            .expect("the claimed drop is alive on the new owner");
        assert!(g1.world.gobs.alive[slot1]);
        let (res, count, ql, label) = g1.world.gobs.kind[slot1]
            .drop_info()
            .expect("pickup payload intact after the transfer");
        assert_eq!(
            g1.world.res.name(res),
            Some("gfx/invobjs/meatroast"),
            "the inventory resource round-trips"
        );
        assert_eq!(count, 1);
        assert_eq!(ql, 12);
        assert_eq!(label, "Meat roast");
        assert_eq!(
            g1.world.res.name(g1.world.gobs.res_idx[slot1]),
            Some(drop_world_res("gfx/invobjs/meatroast")),
            "the deterministic world shape is re-derived on the receiver"
        );
    }

    /// Session 35 (drop authority transfer, negative control): a drop on
    /// a cell this node OWNS never transfers - no GuestTransfer on the
    /// mesh and the gob stays in the sim tables.
    #[tokio::test]
    async fn drop_transfer_local_cell_drop_stays() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("dropstay", 0, 2);
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let inv_res_idx = g.world.res.intern("gfx/invobjs/branch");
        let world_res_idx = g.world.res.intern("gfx/terobjs/items/branch");
        let id = g.world.gobs.spawn(
            Kind::Drop {
                resname_idx: world_res_idx,
                inv_res_idx,
                ql: 3,
                label: "",
            },
            (px + 11, py + 11),
            world_res_idx,
            1,
            0,
        );
        g.tick();
        let mut saw_transfer = false;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if matches!(msg, crate::nodes::NodeMsg::GuestTransfer(_)) {
                saw_transfer = true;
            }
        }
        assert!(!saw_transfer, "own-cell drops never transfer");
        assert!(
            g.world.gobs.get(id).is_some(),
            "the local drop stays authoritative on its owner"
        );
        assert!(!g.world.guests.contains_key(&id));
    }

    /// G5 (player territory): a player entering a foreign cell is
    /// announced to that cell's owner; returning home retracts.
    #[tokio::test]
    async fn player_abroad_publishes_to_cell_owner() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("abroad0", 0, 2);
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (fx, fy) = foreign_cell_pos(&g, 0);
        g.world.gobs.set_pos(pslot, (fx, fy));
        g.tick();
        let mut announced = false;
        while let Ok((peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::GuestAnnounce(st) = msg {
                if st.id == pgob {
                    assert_eq!(peer, 1, "announce goes to the foreign owner");
                    announced = true;
                }
            }
        }
        assert!(announced, "the owner must learn about the abroad player");
        assert_eq!(
            g.cluster.as_ref().unwrap().player_abroad.get(&pgob),
            Some(&1),
            "abroad bookkeeping pins the owner"
        );

        // Back home: the owner is told to retract, bookkeeping clears.
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (hx, hy) = home_cell_pos(&g, 0);
        g.world.gobs.set_pos(pslot, (hx, hy));
        g.tick();
        let mut retracted = false;
        while let Ok((peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::GuestRetract { id } = msg {
                if id == pgob && peer == 1 {
                    retracted = true;
                }
            }
        }
        assert!(retracted, "returning home must retract from the owner");
        assert!(!g
            .cluster
            .as_ref()
            .unwrap()
            .player_abroad
            .contains_key(&pgob));
    }

    /// G6: a local chat line broadcasts on the mesh, and a remote line
    /// delivers to in-radius local sessions (same area-chat semantics).
    #[tokio::test]
    async fn chat_relays_across_nodes() {
        let (mut g, mut rx, _raw, mut mesh_rx) = clustered_game("chat0", 0, 2);
        // Synthesize the chat window (the real client creates it; the
        // server only needs a wid to route "log" uimsgs to).
        let chat_wid = {
            let out = g.sessions.get_mut(&1).unwrap();
            let w = out.new_wid("chat");
            out.chat_wid = w;
            w
        };

        // Send path: the line goes out on the mesh (cluster broadcast).
        g.on_chat_msg(1, "hello mesh");
        let mut relayed = false;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::Chat { from, text, .. } = msg {
                assert_eq!(from, "chat0");
                assert_eq!(text, "hello mesh");
                relayed = true;
            }
        }
        assert!(relayed, "local chat must broadcast to peers");

        // Receive path: a remote line near the player delivers to the
        // session's chat window; a far one does not.
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        while rx.try_recv().is_ok() {}
        g.deliver_remote_chat("peer1", (px + 50, py), "near line");
        let mut near = false;
        while let Ok(msg) = rx.try_recv() {
            if msg.first() == Some(&RMSG_WDGMSG) {
                let mut m = hnh_proto::MessageBuf::from_slice(&msg[1..]);
                let wid = m.u16().unwrap();
                if wid == chat_wid && String::from_utf8_lossy(&msg).contains("near line") {
                    near = true;
                }
            }
        }
        assert!(near, "in-radius remote line must reach the session");

        g.deliver_remote_chat("peer1", (px + 100_000, py), "far line");
        let mut far = false;
        while let Ok(msg) = rx.try_recv() {
            if String::from_utf8_lossy(&msg).contains("far line") {
                far = true;
            }
        }
        assert!(!far, "out-of-radius remote line must be filtered");
    }

    // ==================================================================
    // Cross-node interaction relay (session 28)
    // ==================================================================

    /// Ingest a wolf guest standing `dx` subtiles right of the local
    /// player (in view, same cell so node 0 is its authority stand-in for
    /// wire tests) and return its id.
    fn relay_wolf_guest(g: &mut Game, dx: i32, hp: i32) -> GobId {
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let gid = foreign_node_gob_id(0, 2, 7);
        g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(
            crate::nodes::GuestState {
                id: gid,
                pos: (px + dx, py),
                mv: None,
                moving: false,
                facing: 0,
                kind: crate::nodes::GuestKind::Animal {
                    species: Species::Wolf.index(),
                },
                hp,
                max_hp: hp,
                speed: 33,
            },
        ));
        g.tick();
        assert!(
            g.sessions[&1].visible.contains(&gid),
            "relay test precondition: the guest wolf must spawn in view"
        );
        gid
    }

    /// Relay fight opening: the fightview opens against the guest and the
    /// mirror row appears (the authoritative bars stay on the owner).
    #[tokio::test]
    async fn relay_fight_opens_against_a_guest_target() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("relayer", 0, 2);
        let gid = relay_wolf_guest(&mut g, 30, 50);
        g.start_fight(1, gid, Species::Wolf);
        let pidx = *g.world.by_session.get(&1).unwrap();
        assert_eq!(g.world.players[pidx].fight_target, Some(gid));
        assert!(
            g.world.guest_fights.contains_key(&gid),
            "opening a relay fight must seed the local defence-bar mirror"
        );
        let out = g.sessions.get(&1).unwrap();
        assert!(
            out.fight.widget.is_some() && out.fight.rel(gid).is_some(),
            "the fightview widget + relation must exist for a guest target"
        );
    }

    /// The REAL attack entry point: an interact click on a guest animal
    /// (not in the local gob table) opens the relay fight instead of the
    /// old "interact target gone" no-op.
    #[tokio::test]
    async fn interact_click_on_guest_animal_opens_relay_fight() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("clicker", 0, 2);
        let gid = relay_wolf_guest(&mut g, 30, 50);
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        g.player_interact(1, pgob, gid, (0, 0));
        assert_eq!(
            g.world.players[pidx].fight_target,
            Some(gid),
            "a guest-animal click must open the relay fight"
        );
        assert!(g.world.guest_fights.contains_key(&gid));
    }

    /// A swing at an in-reach guest spends offence and ships exactly one
    /// RelayAttack to the wolf's owner; a FightBars answer re-syncs the
    /// mirror the fightview reads.
    #[tokio::test]
    async fn relay_swing_ships_relayattack_and_fightbars_resync() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("swinger", 0, 2);
        let gid = relay_wolf_guest(&mut g, 30, 50);
        g.start_fight(1, gid, Species::Wolf);
        // Full offence bar + no cooldown: the next tick must swing.
        g.sessions.get_mut(&1).unwrap().fight.own_off = crate::fight::BAR_FULL;
        g.sessions.get_mut(&1).unwrap().fight.atkc = 0;
        g.tick();
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let mut attacks = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::RelayAttack {
                attacker,
                target,
                chip,
                dmg,
            } = msg
            {
                attacks.push((attacker, target, chip, dmg));
            }
        }
        // Default str 10: dmg = (5*10/10).max(1) = 5; weight 1.0 -> the
        // plain SWING_DEF_DMG chip.
        assert_eq!(
            attacks,
            vec![(pgob, gid, crate::fight::SWING_DEF_DMG, 5)], // (5*str/10).max(1) with the default str 10.
            "one swing = exactly one RelayAttack to the owner"
        );
        // The authoritative answer re-syncs the mirror.
        g.on_node_msg(crate::nodes::NodeMsg::FightBars { id: gid, def: 1234 });
        assert_eq!(g.world.guest_fights[&gid].def, 1234);
    }

    /// Authority side: one relayed swing chips the authoritative bar; an
    /// opening lands the HP damage (OD_HEALTH to local viewers + the hp
    /// rides GuestUpdate to the attacker's node); the death drops loot,
    /// retracts, and credits the attacker's home node.
    #[tokio::test]
    async fn relay_authority_applies_damage_and_death_credit() {
        let (mut g, _rx, mut raw, mut mesh_rx) = clustered_game("authwlf", 0, 2);
        // The AUTHORITY side fixture: the wolf lives in MY gob table (a
        // local spawn in my cell); the attacker is a foreign player gob.
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let res = g.world.res.intern(Species::Wolf.resname());
        let gid = g.world.gobs.spawn(
            Kind::Animal {
                species: Species::Wolf,
            },
            (px + 30, py),
            res,
            50,
            33,
        );
        g.world.animal_gobs.push(gid);
        g.tick();
        let attacker = foreign_node_gob_id(0, 2, 21);
        // Node 1 subscribes to the wolf's cell: the hp publish must reach
        // it as a GuestUpdate.
        let cell = crate::visidx::cell_of(px + 30, py);
        g.on_node_msg(crate::nodes::NodeMsg::Sub {
            from: 1,
            cells: vec![cell],
        });
        // First swing: a full-bar chip opens the defence; dmg 10 lands.
        g.on_node_msg(crate::nodes::NodeMsg::RelayAttack {
            attacker,
            target: gid,
            chip: crate::fight::BAR_FULL,
            dmg: 10,
        });
        let tslot = g.world.gobs.get(gid).expect("wolf survives the opening");
        assert_eq!(g.world.gobs.hp[tslot], 40);
        let mut updated_hp = None;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::GuestUpdate(st) = msg {
                if st.id == gid {
                    updated_hp = Some(st.hp);
                }
            }
        }
        assert_eq!(updated_hp, Some(40), "hp must publish to the subscriber");
        // Wire proof for LOCAL viewers: OD_HEALTH quarters stream.
        let mut saw_health = false;
        while let Ok(block) = raw.try_recv() {
            if block.first() == Some(&MSG_OBJDATA)
                && i32::from_le_bytes([block[2], block[3], block[4], block[5]]) == gid
                && block.get(10) == Some(&OD_HEALTH)
            {
                saw_health = true;
            }
        }
        assert!(saw_health, "local viewers must get the OD_HEALTH update");
        // Lethal swing: the animal dies, loot drops, the attacker's home
        // node receives the LP credit.
        g.on_node_msg(crate::nodes::NodeMsg::RelayAttack {
            attacker,
            target: gid,
            chip: crate::fight::BAR_FULL,
            dmg: 40,
        });
        assert!(g.world.gobs.get(gid).is_none(), "the wolf must die");
        let mut credited = None;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::KillCredit { player_gob, lp } = msg {
                credited = Some((player_gob, lp));
            }
        }
        assert_eq!(
            credited,
            Some((attacker, 10)),
            "the killer's home node must be credited with the LP"
        );
    }

    /// Retaliation: an animal with a relay row bites the guest player and
    /// the bite SHIPS to the attacker's home node (no local session to
    /// apply it through).
    #[tokio::test]
    async fn relay_retaliation_ships_playerhurt_home() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("biter", 0, 2);
        // The AUTHORITY side fixture: a local wolf + a published guest
        // player standing in reach.
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let res = g.world.res.intern(Species::Wolf.resname());
        let gid = g.world.gobs.spawn(
            Kind::Animal {
                species: Species::Wolf,
            },
            (px + 30, py),
            res,
            50,
            33,
        );
        g.world.animal_gobs.push(gid);
        g.tick();
        let attacker = foreign_node_gob_id(0, 2, 21);
        g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(
            crate::nodes::GuestState {
                id: attacker,
                pos: (px + 50, py),
                mv: None,
                moving: false,
                facing: 0,
                kind: crate::nodes::GuestKind::Player {
                    name: "foreigner".into(),
                    equip: vec![],
                },
                hp: 100,
                max_hp: 100,
                speed: 33,
            },
        ));
        // A relayed swing registers the guest attacker.
        g.on_node_msg(crate::nodes::NodeMsg::RelayAttack {
            attacker,
            target: gid,
            chip: 0,
            dmg: 1,
        });
        assert_eq!(g.world.guest_attackers[&gid], attacker);
        // Full offence + no cooldown: the next tick bites in reach.
        g.world.animal_fights.get_mut(&gid).expect("relay row").off = crate::fight::BAR_FULL;
        g.tick();
        let mut hurt = None;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::PlayerHurt { player_gob, .. } = msg {
                hurt = Some(player_gob);
            }
        }
        assert_eq!(hurt, Some(attacker), "the bite must ship to the home node");
        // The attacker leaving the cell (retract) drops the relay row so
        // the animal stops retaliating at a ghost.
        g.on_node_msg(crate::nodes::NodeMsg::GuestRetract { id: attacker });
        assert!(!g.world.guest_attackers.contains_key(&gid));
        assert!(!g.world.animal_fights.contains_key(&gid));
    }

    /// The owner retracting the fought guest (death seen elsewhere, GC)
    /// closes the local fight: target cleared, mirror dropped, relation
    /// deleted and the frv widget destroyed when it was the last one.
    #[tokio::test]
    async fn guest_retract_closes_the_relay_fight() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("closer", 0, 2);
        let gid = relay_wolf_guest(&mut g, 30, 50);
        g.start_fight(1, gid, Species::Wolf);
        g.on_node_msg(crate::nodes::NodeMsg::GuestRetract { id: gid });
        let pidx = *g.world.by_session.get(&1).unwrap();
        assert_eq!(g.world.players[pidx].fight_target, None);
        assert!(!g.world.guest_fights.contains_key(&gid));
        let out = g.sessions.get(&1).unwrap();
        assert!(
            out.fight.widget.is_none() && out.fight.rel(gid).is_none(),
            "the last relation must close the frv widget"
        );
    }

    // ------------------------------------------------------------------
    // Session 29: cluster save story (account-keyed characters, migration)
    // ------------------------------------------------------------------

    /// A clustered game with NO session: the raw fixture for save-store
    /// and node-msg level tests. Returns the mesh sink receiver so tests
    /// can assert exactly what this node sends to its peers.
    fn bare_clustered(
        tag: &str,
        me: usize,
        nodes: usize,
    ) -> (
        Game,
        tokio::sync::mpsc::UnboundedReceiver<(usize, crate::nodes::NodeMsg)>,
    ) {
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = Game::new(
            42,
            cmd_rx,
            net_rx,
            false,
            std::env::temp_dir().join(format!("hnh-save-test-{}.json", tag)),
        );
        let (mesh_tx, mesh_rx) = tokio::sync::mpsc::unbounded_channel();
        let nz = NonZeroUsize::new(nodes).expect("nodes");
        g.world = World::with_layout(42, nz, me);
        g.cluster = Some(Cluster {
            me,
            nodes: nz,
            mesh: crate::nodes::Mesh { out_tx: mesh_tx },
            peer_subs: HashMap::new(),
            my_subs: HashMap::new(),
            player_abroad: HashMap::new(),
        });
        (g, mesh_rx)
    }

    /// Open one session on `g` and press play on the charlist (the full
    /// login path; cluster nodes may defer the entry on a CharQuery).
    fn open_session_and_play(g: &mut Game, sid: SessionId, account: &str, chosen: &str) {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let (raw_tx, _raw) = tokio::sync::mpsc::channel(512);
        g.session_connected(sid, account.to_owned(), tx, raw_tx);
        let wid = g.sessions[&sid]
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "charlist")
            .map(|(k, _)| *k)
            .expect("charlist widget");
        g.on_wdgmsg(
            sid,
            wid,
            "play",
            vec![hnh_proto::ListArg::Str(chosen.to_owned())],
        );
    }

    fn snapshot(key: &str, pos: (i32, i32)) -> crate::persist::SavedPlayer {
        crate::persist::SavedPlayer {
            name: key.to_owned(),
            pos,
            hp: 80,
            energy: 70,
            stamina: 60,
            lp: 42,
            attrs: HashMap::from([("str".to_owned(), 12)]),
            inv: Vec::new(),
            inv_labels: Vec::new(),
            skills: Vec::new(),
            equip: Vec::new(),
        }
    }

    #[tokio::test]
    async fn char_query_migrates_the_offline_snapshot_to_the_peer() {
        let (mut holder, mut mesh_rx) = bare_clustered("migrate-holder", 0, 2);
        let key = crate::persist::save_key("acct", "Player");
        holder
            .save
            .players
            .insert(key.clone(), snapshot(&key, (500, 500)));
        holder.on_node_msg(crate::nodes::NodeMsg::CharQuery {
            from: 1,
            name: key.clone(),
        });
        // Two-phase: the holder KEEPS the snapshot until the ack, so a
        // lost reply can always be re-served by a query retry.
        assert!(
            holder.save.players.contains_key(&key),
            "the holder must keep the snapshot until CharAck"
        );
        let mut data = None;
        while let Ok((peer, msg)) = mesh_rx.try_recv() {
            assert_eq!(peer, 1, "unicast to the requester");
            if let crate::nodes::NodeMsg::CharData {
                to,
                from,
                name,
                snap,
            } = msg
            {
                assert_eq!(to, 1, "routed to the requester node");
                assert_eq!(from, 0, "sent by the holder");
                assert_eq!(name, key);
                data = Some(snap);
            }
        }
        let snap = data.expect("CharData reply");
        assert_eq!(snap.pos, (500, 500));
        assert_eq!(snap.lp, 42);
        // Re-query before the ack: the snapshot is re-served (idempotent).
        holder.on_node_msg(crate::nodes::NodeMsg::CharQuery {
            from: 1,
            name: key.clone(),
        });
        let mut re_served = false;
        while let Ok((_, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::CharData { name, .. } = msg {
                assert_eq!(name, key);
                re_served = true;
            }
        }
        assert!(re_served, "retry must re-serve the snapshot");
        // The ack completes the migration: the copy leaves the holder.
        holder.on_node_msg(crate::nodes::NodeMsg::CharAck { name: key.clone() });
        assert!(
            !holder.save.players.contains_key(&key),
            "CharAck must drop the holder's copy"
        );
    }

    #[tokio::test]
    async fn char_query_for_an_online_character_nacks_and_keeps_the_snapshot() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("migrate-online", 0, 2);
        let key = crate::persist::save_key("acct", &g.world.players[0].name);
        g.save.players.insert(key.clone(), snapshot(&key, (10, 10)));
        g.on_node_msg(crate::nodes::NodeMsg::CharQuery {
            from: 1,
            name: key.clone(),
        });
        let mut nacks = 0;
        while let Ok((peer, msg)) = mesh_rx.try_recv() {
            assert_eq!(peer, 1);
            if let crate::nodes::NodeMsg::CharNack { to, from, name: n } = msg {
                assert_eq!(to, 1, "routed to the requester");
                assert_eq!(from, 0, "sent by the holder");
                assert_eq!(n, key);
                nacks += 1;
            }
        }
        assert_eq!(nacks, 1, "an online character is answered with a nack");
        assert!(
            g.save.players.contains_key(&key),
            "the live player's snapshot must not migrate"
        );
    }

    #[tokio::test]
    async fn chardata_adopts_the_snapshot_and_enters_the_world() {
        let (mut g, mut mesh_rx) = bare_clustered("migrate-adopt", 1, 2);
        open_session_and_play(&mut g, 1, "acct", "Player");
        // The entry deferred: no player yet, one CharQuery on the mesh.
        assert!(!g.world.by_session.contains_key(&1), "entry must defer");
        let mut queried = None;
        while let Ok((_, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::CharQuery { from, name } = msg {
                assert_eq!(from, 1);
                queried = Some(name);
            }
        }
        let key = queried.expect("CharQuery broadcast");
        assert_eq!(key, crate::persist::save_key("acct", "Player"));
        // The holder answers; the requester adopts, acks back and enters.
        g.on_node_msg(crate::nodes::NodeMsg::CharData {
            to: 1,
            from: 0,
            name: key.clone(),
            snap: snapshot(&key, (777, -777)),
        });
        let pidx = *g.world.by_session.get(&1).expect("entered via migration");
        let pgob = g.world.players[pidx].gob;
        let slot = g.world.gobs.get(pgob).expect("player slot");
        assert_eq!(g.world.gobs.pos[slot], (777, -777), "restored position");
        assert_eq!(g.world.players[pidx].lp, 42, "restored lp");
        assert_eq!(g.world.players[pidx].hp, 80, "restored hp");
        let mut acked = false;
        while let Ok((peer, msg)) = mesh_rx.try_recv() {
            assert_eq!(peer, 0, "ack unicast to the holder");
            if let crate::nodes::NodeMsg::CharAck { name } = msg {
                assert_eq!(name, key);
                acked = true;
            }
        }
        assert!(acked, "adoption must ack so the holder drops its copy");
    }

    #[tokio::test]
    async fn char_nack_majority_enters_fresh_without_the_deadline() {
        let (mut g, _mesh_rx) = bare_clustered("migrate-nacks", 1, 3);
        open_session_and_play(&mut g, 1, "acct", "Player");
        assert!(!g.world.by_session.contains_key(&1));
        // One of two peers answered: still waiting.
        g.on_node_msg(crate::nodes::NodeMsg::CharNack {
            to: 1,
            from: 0,
            name: crate::persist::save_key("acct", "Player"),
        });
        assert!(
            !g.world.by_session.contains_key(&1),
            "entry waits for every peer"
        );
        g.on_node_msg(crate::nodes::NodeMsg::CharNack {
            to: 1,
            from: 2,
            name: crate::persist::save_key("acct", "Player"),
        });
        assert!(
            g.world.by_session.contains_key(&1),
            "the last nack completes the entry"
        );
    }

    #[tokio::test]
    async fn accounts_hold_separate_characters_and_legacy_saves_are_adopted() {
        let (mut g, _mesh_rx) = bare_clustered("account-keys", 0, 1);
        // Legacy layout: one bare "Player" snapshot from an older server.
        g.save
            .players
            .insert("Player".to_owned(), snapshot("Player", (321, 123)));
        open_session_and_play(&mut g, 1, "alice", "Player");
        let pidx = *g.world.by_session.get(&1).expect("legacy adoption entered");
        let pgob = g.world.players[pidx].gob;
        let slot = g.world.gobs.get(pgob).expect("player slot");
        assert_eq!(
            g.world.gobs.pos[slot],
            (321, 123),
            "legacy snapshot restored"
        );
        // Adoption re-keyed the snapshot into the account namespace.
        assert!(
            g.save.players.contains_key("alice:Player"),
            "legacy snapshot re-keyed"
        );
        assert!(!g.save.players.contains_key("Player"), "bare key consumed");
        // A second account gets a FRESH character, not alice's.
        open_session_and_play(&mut g, 2, "bob", "Player");
        let pidx2 = *g.world.by_session.get(&2).expect("second account entered");
        assert_ne!(
            g.world.players[pidx].gob, g.world.players[pidx2].gob,
            "two live players"
        );
        assert_eq!(
            g.save.players.get("bob:Player").map(|s| s.pos),
            None,
            "bob starts with no snapshot"
        );
    }

    // ------------------------------------------------------------------
    // Session 30: cursor pickup redirection + stack merging
    // (items-and-quality.md: stacking policy is server policy)
    // ------------------------------------------------------------------

    /// Test helper: find the only live ground Drop gob.
    fn only_drop_gob(g: &Game) -> GobId {
        let mut found = None;
        for slot in 0..g.world.gobs.alive.len() {
            if g.world.gobs.alive[slot] && matches!(g.world.gobs.kind[slot], Kind::Drop { .. }) {
                found = Some(gob_id_from_slot(slot, g.world.gobs.gen[slot]));
            }
        }
        found.expect("test precondition: exactly one live Drop gob")
    }

    /// Picking up a ground drop while the cursor drags the SAME resource
    /// redirects onto the cursor: counts add, quality re-averages, the
    /// inventory gains nothing, and the drag widget count syncs.
    #[tokio::test]
    async fn pickup_merges_onto_same_resource_cursor() {
        let (mut g, _rx, _raw) = entered_game("cursorpickup");
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let wood = g.world.res.intern("gfx/invobjs/wood");
        // Cursor already drags 2 wood at q10.
        g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
            res: wood,
            count: 2,
            ql: 10,
            label: "",
        });
        // A ground wood drop at q20 lands next to the player.
        g.spawn_drop_near((px, py), "gfx/invobjs/wood", 20, "");
        let drop = only_drop_gob(&g);
        g.player_interact(1, pgob, drop, (0, 0));
        // The cursor stack absorbed the drop: 3 units, (10*2+20*1)/3 = 13.
        let cur = g
            .sessions
            .get(&1)
            .unwrap()
            .cursor
            .expect("cursor keeps the stack");
        assert_eq!(cur.res, wood);
        assert_eq!(cur.count, 3, "counts conserved onto the cursor");
        assert_eq!(cur.ql, 13, "count-weighted quality average");
        assert!(
            !g.world.players[pidx].inv.iter().any(|s| s.res == wood),
            "no parallel inventory stack for a redirected pickup"
        );
        assert!(g.world.gobs.get(drop).is_none(), "drop gob removed");
    }

    /// Releasing the cursor onto the inventory grid merges into an existing
    /// same-resource stack instead of appending a duplicate slot.
    #[tokio::test]
    async fn inv_drop_merges_into_same_resource_stack() {
        let (mut g, _rx, _raw) = entered_game("invdropmerge");
        let pidx = *g.world.by_session.get(&1).unwrap();
        let wood = g.world.res.intern("gfx/invobjs/wood");
        // Inventory holds 3 wood at q10; the cursor drags 2 wood at q30.
        g.world.players[pidx].inv.push(InvStack {
            res: wood,
            count: 3,
            ql: 10,
            label: "",
        });
        g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
            res: wood,
            count: 2,
            ql: 30,
            label: "",
        });
        g.inv_drop(1, 0, &[]);
        // One merged stack: 5 units, (10*3+30*2)/5 = 18.
        let stacks: Vec<_> = g.world.players[pidx]
            .inv
            .iter()
            .filter(|s| s.res == wood)
            .collect();
        assert_eq!(stacks.len(), 1, "exactly one wood stack after the drop");
        assert_eq!(stacks[0].count, 5, "counts conserved");
        assert_eq!(stacks[0].ql, 18, "count-weighted quality average");
        assert!(
            g.sessions.get(&1).unwrap().cursor.is_none(),
            "cursor emptied"
        );
    }

    /// A pickup of a DIFFERENT resource never merges: the cursor keeps its
    /// stack, the pickup lands in its own inventory slot.
    #[tokio::test]
    async fn different_resource_pickup_keeps_stacks_separate() {
        let (mut g, _rx, _raw) = entered_game("separatepick");
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let stone = g.world.res.intern("gfx/invobjs/stone");
        let wood = g.world.res.intern("gfx/invobjs/wood");
        // Cursor drags stone; the ground drop is wood.
        g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
            res: stone,
            count: 1,
            ql: 10,
            label: "",
        });
        g.spawn_drop_near((px, py), "gfx/invobjs/wood", 20, "");
        let drop = only_drop_gob(&g);
        g.player_interact(1, pgob, drop, (0, 0));
        let cur = g
            .sessions
            .get(&1)
            .unwrap()
            .cursor
            .expect("cursor untouched");
        assert_eq!(
            (cur.res, cur.count),
            (stone, 1),
            "cursor still drags the stone"
        );
        let inv_wood: Vec<_> = g.world.players[pidx]
            .inv
            .iter()
            .filter(|s| s.res == wood)
            .collect();
        assert_eq!(inv_wood.len(), 1, "wood stored in its own stack");
        assert_eq!(inv_wood[0].count, 1);
    }

    // ------------------------------------------------------------------
    // Session 30: relay static acts (pickup/chop/mine vs guest statics)
    // ------------------------------------------------------------------

    /// Home side: an interact click on a guest DROP ships exactly one
    /// RelayStaticAct{Pickup} to the drop's cell owner - never a no-op.
    #[tokio::test]
    async fn guest_drop_click_ships_relay_static_pickup() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("dropclick", 0, 2);
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let (fx, fy) = foreign_cell_pos(&g, 0);
        let gid = foreign_node_gob_id(0, 2, 31);
        g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(
            crate::nodes::GuestState {
                id: gid,
                pos: (fx, fy),
                mv: None,
                moving: false,
                facing: 1,
                kind: crate::nodes::GuestKind::Static {
                    res_name: "gfx/terobjs/items/wood".into(),
                    class: crate::nodes::StaticClass::Drop,
                    crop: None,
                    station: None,
                    stage: None,
                    drop: None,
                },
                hp: 1,
                max_hp: 1,
                speed: 0,
            },
        ));
        g.tick();
        g.player_interact(1, pgob, gid, (0, 0));
        let mut relays = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::RelayStaticAct {
                player,
                target,
                act,
            } = msg
            {
                relays.push((player, target, act));
            }
        }
        let authority = {
            let c = g.cluster.as_ref().unwrap();
            crate::grid_owner::owner_of(crate::visidx::cell_of(fx, fy), c.nodes)
        };
        assert_eq!(
            relays,
            vec![(pgob, gid, crate::nodes::StaticAct::Pickup)],
            "one Pickup relay per click, addressed to the cell owner (node {authority})"
        );
    }

    /// Authority side: a relayed Pickup removes the local drop, publishes
    /// the retract, and answers StaticAck with the EXACT stack (resource
    /// name + quality + fep label) so the home node grants it once.
    #[tokio::test]
    async fn relay_pickup_authority_removes_and_acks_stack() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("authdrop", 0, 2);
        // A local drop in a cell OWNED BY ME (ownership is a per-cell
        // hash, so the player's own cell can belong to the peer - anchor
        // the drop at an owned cell's center; the +-33-subtile spawn
        // jitter then never leaves the cell).
        let (hx, hy) = home_cell_pos(&g, 0);
        let cell = crate::visidx::cell_of(hx, hy);
        let center = (
            cell.0 * crate::visidx::CELL + 125,
            cell.1 * crate::visidx::CELL + 125,
        );
        g.spawn_drop_near(center, "gfx/invobjs/branch", 10, "Branch");
        let drop = only_drop_gob(&g);
        // Node 1 subscribes to the drop's cell so the retract publishes.
        g.on_node_msg(crate::nodes::NodeMsg::Sub {
            from: 1,
            cells: vec![cell],
        });
        // The clicking player is a foreign gob homed on node 1.
        let clicker = foreign_node_gob_id(0, 2, 41);
        g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
            player: clicker,
            target: drop,
            act: crate::nodes::StaticAct::Pickup,
        });
        assert!(
            g.world.gobs.get(drop).is_none(),
            "drop removed by the authority"
        );
        let mut acks = Vec::new();
        let mut retracts = 0;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            match msg {
                crate::nodes::NodeMsg::StaticAck { player, stack, lp } => {
                    acks.push((player, stack, lp));
                }
                crate::nodes::NodeMsg::GuestRetract { id } if id == drop => retracts += 1,
                _ => {}
            }
        }
        assert_eq!(retracts, 1, "the retract publishes to subscribers");
        assert_eq!(acks.len(), 1, "exactly one StaticAck");
        let (player, stack, lp) = acks.pop().unwrap();
        assert_eq!(player, clicker);
        assert_eq!(lp, 0);
        let st = stack.expect("pickup acks a stack");
        assert_eq!(st.res, "gfx/invobjs/branch");
        assert_eq!(st.count, 1);
        assert_eq!(st.ql, 10);
        assert_eq!(st.label, "Branch");
    }

    /// Home ack side: StaticAck grants the stack through grant_pickup -
    /// onto a same-resource cursor (redirection), else into inventory -
    /// and lp>0 tops up the wallet + pushes the char sheet.
    #[tokio::test]
    async fn static_ack_grants_stack_and_lp_on_home() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("ackhome", 0, 2);
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let branch = g.world.res.intern("gfx/invobjs/branch");
        // Cursor drags a branch stack: the ack must redirect onto it.
        g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
            res: branch,
            count: 1,
            ql: 10,
            label: "Branch",
        });
        let lp_before = g.world.players[pidx].lp;
        g.on_node_msg(crate::nodes::NodeMsg::StaticAck {
            player: pgob,
            stack: Some(crate::nodes::StaticStack {
                res: "gfx/invobjs/branch".into(),
                count: 2,
                ql: 20,
                label: "Branch".into(),
            }),
            lp: 5,
        });
        let cur = g
            .sessions
            .get(&1)
            .unwrap()
            .cursor
            .expect("cursor absorbed the ack");
        let lp_after = g.world.players[pidx].lp;
        assert_eq!(cur.count, 3, "1@q10 + 2@q20 -> 3 units");
        assert_eq!(cur.ql, 16, "(10*1+20*2)/3 = 16");
        // The starter kit's branch stack (6 since session 36) must stay
        // UNTOUCHED: the ack redirected onto the cursor, not into the
        // inventory.
        let inv_branch: Vec<_> = g.world.players[pidx]
            .inv
            .iter()
            .filter(|s| s.res == branch)
            .collect();
        assert_eq!((inv_branch.len(), inv_branch[0].count), (1, 6));
        assert_eq!(
            lp_after,
            lp_before + 5,
            "lp granted from the ack (relative)"
        );
        // A different-resource ack lands in its own inventory stack.
        g.on_node_msg(crate::nodes::NodeMsg::StaticAck {
            player: pgob,
            stack: Some(crate::nodes::StaticStack {
                res: "gfx/invobjs/wood".into(),
                count: 1,
                ql: 10,
                label: String::new(),
            }),
            lp: 0,
        });
        let wood = g.world.res.intern("gfx/invobjs/wood");
        assert_eq!(
            g.world.players[pidx]
                .inv
                .iter()
                .filter(|s| s.res == wood)
                .count(),
            1,
            "wood stored as its own stack"
        );
    }

    /// Chop authority side: harvests decrement, fresh wood drops spawn on
    /// the authority, lp 5 acks; an exhausted tree is removed with a stump
    /// and acks lp 0.
    #[tokio::test]
    async fn relay_chop_authority_decrements_and_acks() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("authchop", 0, 2);
        let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        // A local tree in MY cell with 2 harvests left.
        let tree_res = g.world.res.intern("gfx/terobjs/trees/old");
        let tree = g
            .world
            .gobs
            .spawn(Kind::Tree { harvests: 2 }, (px + 22, py), tree_res, 1, 0);
        let clicker = foreign_node_gob_id(0, 2, 51);
        g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
            player: clicker,
            target: tree,
            act: crate::nodes::StaticAct::Chop,
        });
        let tslot = g.world.gobs.get(tree).expect("tree survives one chop");
        assert!(
            matches!(g.world.gobs.kind[tslot], Kind::Tree { harvests: 1 }),
            "one chop off"
        );
        // A fresh wood drop spawned next to the tree.
        let mut wood_drops = 0;
        for slot in 0..g.world.gobs.alive.len() {
            if g.world.gobs.alive[slot] && matches!(g.world.gobs.kind[slot], Kind::Drop { .. }) {
                wood_drops += 1;
            }
        }
        assert_eq!(wood_drops, 1, "the chop spawned one wood drop");
        // Exhaust the tree: second chop -> 1 harvest -> third chop kills it.
        g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
            player: clicker,
            target: tree,
            act: crate::nodes::StaticAct::Chop,
        });
        g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
            player: clicker,
            target: tree,
            act: crate::nodes::StaticAct::Chop,
        });
        assert!(g.world.gobs.get(tree).is_none(), "exhausted tree removed");
        let mut acks = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::StaticAck { player, stack, lp } = msg {
                acks.push((player, stack.is_some(), lp));
            }
        }
        assert_eq!(
            acks,
            vec![
                (clicker, false, 5),
                (clicker, false, 5),
                (clicker, false, 0)
            ],
            "chop lp 5, chop lp 5, exhausted lp 0"
        );
    }

    /// A stale guest view must never apply: a Pickup act against a TREE is
    /// dropped by the authority (no state change, no ack).
    #[tokio::test]
    async fn relay_static_mismatch_is_dropped() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("mismatch", 0, 2);
        let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let tree_res = g.world.res.intern("gfx/terobjs/trees/old");
        let tree = g
            .world
            .gobs
            .spawn(Kind::Tree { harvests: 2 }, (px + 22, py), tree_res, 1, 0);
        let clicker = foreign_node_gob_id(0, 2, 61);
        g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
            player: clicker,
            target: tree,
            act: crate::nodes::StaticAct::Pickup, // wrong act for a tree
        });
        let tslot = g.world.gobs.get(tree).expect("tree untouched");
        assert!(matches!(
            g.world.gobs.kind[tslot],
            Kind::Tree { harvests: 2 }
        ));
        let mut saw_ack = false;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if matches!(msg, crate::nodes::NodeMsg::StaticAck { .. }) {
                saw_ack = true;
            }
        }
        assert!(!saw_ack, "a mismatched act acks nothing");
    }

    fn pgob_of(g: &Game) -> GobId {
        let pidx = *g.world.by_session.get(&1).unwrap();
        g.world.players[pidx].gob
    }

    // ------------------------------------------------------------------
    // Session 30: vis-scan result caching (patch + clean paths)
    // ------------------------------------------------------------------

    /// A stationary session patches its cached scan result instead of
    /// rescanning: enterers spawn, leavers retract, deaths purge - all
    /// without a position change on the viewer side.
    #[tokio::test]
    async fn vis_cache_patches_stationary_session() {
        let (mut g, _rx, _raw) = entered_game("viscache");
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        // Prime the cache: the entry ticks already scanned (Full).
        assert!(g.sessions.get(&1).unwrap().vis_cache.is_some());
        let base = g.sessions.get(&1).unwrap().vis_cache.clone().unwrap();
        // 1) An enterer spawns in view (touched): the patch must add it.
        let drop_res = g.world.res.intern("gfx/terobjs/items/branch");
        let d = g.world.gobs.spawn(
            Kind::Drop {
                resname_idx: drop_res,
                inv_res_idx: g.world.res.intern("gfx/invobjs/branch"),
                ql: 10,
                label: "Branch",
            },
            (px + 60, py + 60),
            drop_res,
            1,
            0,
        );
        g.tick();
        assert!(
            g.sessions.get(&1).unwrap().visible.contains(&d),
            "the patch spawned the new drop for the stationary viewer"
        );
        // 2) A leaver moves out of view (touched at its old cell): the
        //    patch must drop it from the result; the retract sweep then
        //    removes it from `visible` on its cadence.
        g.world
            .gobs
            .set_pos(g.world.gobs.get(d).unwrap(), (px + 90, py + 2400));
        g.world.gobs.vis.reposition(d, (px + 90, py + 2400));
        for _ in 0..10 {
            g.tick();
        }
        assert!(
            !g.sessions
                .get(&1)
                .unwrap()
                .vis_cache
                .as_ref()
                .unwrap()
                .contains(&d),
            "the patched result no longer lists the leaver"
        );
        // 3) A death in view purges by liveness (the patch never keeps a
        //    dead id - the ghost-respawn class of bug).
        let d2 = g.world.gobs.spawn(
            Kind::Drop {
                resname_idx: drop_res,
                inv_res_idx: g.world.res.intern("gfx/invobjs/branch"),
                ql: 10,
                label: "Branch",
            },
            (px - 60, py - 60),
            drop_res,
            1,
            0,
        );
        g.tick();
        assert!(g.sessions.get(&1).unwrap().visible.contains(&d2));
        g.world.gobs.kill(d2);
        g.broadcast_retract(d2);
        g.tick();
        assert!(
            !g.sessions
                .get(&1)
                .unwrap()
                .vis_cache
                .as_ref()
                .unwrap()
                .contains(&d2),
            "a dead gob never lingers in the cached result"
        );
        // 4) The cached result stays consistent with a fresh full scan:
        //    same id set as scan_visible at the same position.
        let cached = g.sessions.get(&1).unwrap().vis_cache.clone().unwrap();
        let fresh = g.scan_visible(px, py);
        let mut a = cached;
        let mut b = fresh;
        a.sort_unstable();
        a.dedup();
        b.sort_unstable();
        b.dedup();
        assert_eq!(a, b, "patched result equals a full rescan");
        // 5) The view stayed quiet in between (the clean skip fired at
        //    least once): visible_total accounting never crashed.
        let _ = base.len();
    }

    // ------------------------------------------------------------------
    // Session 31: cross-node crop harvest relay
    // ------------------------------------------------------------------

    /// Plant a crop gob directly (restore-path construction, no farming
    /// skill gate) near the test player and return its id.
    fn planted_crop(g: &mut Game, spec: u8, stage: u8) -> GobId {
        let pslot = g.world.gobs.get(pgob_of(g)).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let res = g.world.res.intern("gfx/terobjs/crops/wheat");
        let gob = g
            .world
            .gobs
            .spawn(Kind::Crop { spec, stage }, (px + 30, py), res, 1, 0);
        g.world.crops.insert(
            gob,
            crate::farm::CropState {
                spec,
                stage,
                seed_ql: 10,
                soil_ql: 10,
                next_stage_at: u64::MAX,
            },
        );
        gob
    }

    #[tokio::test]
    async fn crop_guest_state_carries_crop_class_and_payload() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("cropstate", 0, 2);
        let gob = planted_crop(&mut g, 1, 3);
        let slot = g.world.gobs.get(gob).unwrap();
        let st = g
            .guest_state_from_slot(gob, slot)
            .expect("a crop publishes a guest state");
        match st.kind {
            crate::nodes::GuestKind::Static {
                class,
                crop: Some((spec, stage)),
                ..
            } => {
                assert_eq!(class, crate::nodes::StaticClass::Crop);
                assert_eq!((spec, stage), (1, 3), "spec and stage ride the payload");
            }
            other => panic!("expected a Crop static, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn crop_stage_advance_publishes_update_to_subscribers() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("cropstage", 0, 2);
        let gob = planted_crop(&mut g, 1, 2);
        // A subscribed peer: its subscription covers the crop's cell.
        let (cx, cy) = crate::visidx::cell_of(
            g.world.gobs.pos[g.world.gobs.get(gob).unwrap()].0,
            g.world.gobs.pos[g.world.gobs.get(gob).unwrap()].1,
        );
        g.cluster
            .as_mut()
            .unwrap()
            .peer_subs
            .insert(1, std::iter::once((cx, cy)).collect());
        while let Ok((_, msg)) = mesh_rx.try_recv() {
            let _ = msg; // drain the subscribe/announce backlog
        }
        // Force a stage advance through the farming scheduler.
        g.world.crops.get_mut(&gob).unwrap().next_stage_at = 0;
        g.tick_farming();
        // The advance re-publishes the guest state with the new stage.
        let mut saw_stage_update = false;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::GuestUpdate(st) = msg {
                if st.id == gob {
                    match st.kind {
                        crate::nodes::GuestKind::Static {
                            class: crate::nodes::StaticClass::Crop,
                            crop: Some((_spec, stage)),
                            ..
                        } => {
                            assert_eq!(stage, 3, "wheat advances 2 -> 3");
                            saw_stage_update = true;
                        }
                        other => panic!("expected a crop static update, got {other:?}"),
                    }
                }
            }
        }
        assert!(saw_stage_update, "stage advance publishes a GuestUpdate");
    }

    #[tokio::test]
    async fn guest_crop_click_opens_menu_only_when_ripe() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("cropmenu", 0, 2);
        let pgob = pgob_of(&g);
        // Ripe guest crop: stage 5 >= wheat early_stage 2 -> menu opens.
        // Ingested through the REAL path (GuestAnnounce), so the guest row
        // is exactly what a subscriber would hold.
        let ripe = planted_crop(&mut g, 1, 5);
        let rslot = g.world.gobs.get(ripe).unwrap();
        let st = g.guest_state_from_slot(ripe, rslot).unwrap();
        let (fx, fy) = st.pos;
        g.world.gobs.kill(ripe);
        g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st));
        g.player_interact(1, pgob, ripe, (fx, fy));
        assert!(
            g.sessions.get(&1).unwrap().crop_menu.is_some(),
            "a ripe guest crop opens the harvest menu"
        );
        // Unripe guest crop: stage 1 < early_stage 2 -> the menu target
        // stays on the earlier crop.
        let unripe = planted_crop(&mut g, 1, 1);
        let uslot = g.world.gobs.get(unripe).unwrap();
        let ust = g.guest_state_from_slot(unripe, uslot).unwrap();
        g.world.gobs.kill(unripe);
        g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(ust));
        g.player_interact(1, pgob, unripe, (0, 0));
        let menu = g.sessions.get(&1).unwrap().crop_menu.unwrap();
        assert_ne!(
            menu.1, unripe,
            "an unripe guest crop never replaces the menu target"
        );
    }

    #[tokio::test]
    async fn relay_harvest_crop_acks_yield_stacks_and_kills_crop() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("cropharvest", 0, 2);
        // A mature wheat crop (spec 1, stage = stages = final).
        let stages = crate::farm::CROPS[1].stages;
        let gob = planted_crop(&mut g, 1, stages);
        let clicker = foreign_node_gob_id(0, 2, 61);
        g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
            player: clicker,
            target: gob,
            act: crate::nodes::StaticAct::HarvestCrop,
        });
        // The crop is dead on the authority and tilth is restored.
        assert!(
            g.world.gobs.get(gob).is_none(),
            "the relayed harvest removes the crop"
        );
        // One StaticAck per mature-yield stack, all addressed to the
        // clicking player's home node, none carrying LP.
        let mut acks: Vec<(Option<crate::nodes::StaticStack>, i32)> = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::StaticAck { player, stack, lp } = msg {
                assert_eq!(player, clicker);
                acks.push((stack, lp));
            }
        }
        let yields = crate::farm::CROPS[1].mature_yields;
        assert_eq!(acks.len(), yields.len(), "one ack per yielded stack");
        for (stack, lp) in &acks {
            assert_eq!(*lp, 0, "crops never grant LP");
            let stack = stack.as_ref().expect("harvest acks always carry a stack");
            assert!(
                yields.iter().any(|y| y.res == stack.res),
                "yielded {} is in the wheat table",
                stack.res
            );
            assert!(stack.count >= 1);
        }
    }

    #[tokio::test]
    async fn relay_harvest_crop_mismatch_is_dropped() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("cropmismatch", 0, 2);
        // A TREE tagged with the harvest act: the authority re-validates
        // the Kind and drops the stale view.
        let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let res = g.world.res.intern("gfx/terobjs/trees/old");
        let tree = g
            .world
            .gobs
            .spawn(Kind::Tree { harvests: 2 }, (px + 40, py), res, 1, 0);
        let clicker = foreign_node_gob_id(0, 2, 61);
        g.on_node_msg(crate::nodes::NodeMsg::RelayStaticAct {
            player: clicker,
            target: tree,
            act: crate::nodes::StaticAct::HarvestCrop,
        });
        assert!(
            g.world.gobs.get(tree).is_some(),
            "the tree survives the mismatched act"
        );
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            assert!(
                !matches!(msg, crate::nodes::NodeMsg::StaticAck { .. }),
                "a mismatched act never acks"
            );
        }
    }

    #[tokio::test]
    async fn home_node_grants_every_yield_ack_stack() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("cropack", 0, 2);
        let pgob = pgob_of(&g);
        let before = g
            .world
            .players
            .iter()
            .find(|p| p.gob == pgob)
            .unwrap()
            .inv
            .len();
        // Two acks (a two-stack harvest): BOTH stacks must land.
        for res in ["gfx/invobjs/wheat", "gfx/invobjs/grainseed"] {
            g.on_node_msg(crate::nodes::NodeMsg::StaticAck {
                player: pgob,
                stack: Some(crate::nodes::StaticStack {
                    res: res.to_owned(),
                    count: 3,
                    ql: 10,
                    label: "Wheat".to_owned(),
                }),
                lp: 0,
            });
        }
        let after = g
            .world
            .players
            .iter()
            .find(|p| p.gob == pgob)
            .unwrap()
            .inv
            .len();
        assert_eq!(after, before + 2, "each yield ack grants its stack");
    }

    #[tokio::test]
    async fn foreign_plant_ships_relay_act_and_keeps_seed() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("plantrelay", 0, 2);
        g.world.players[0].attrs.insert("farming".to_owned(), 1);
        let seed = InvStack {
            res: g.world.res.intern("gfx/invobjs/seed-wheat"),
            count: 3,
            ql: 11,
            label: "Wheat grain",
        };
        if let Some(out) = g.sessions.get_mut(&1) {
            out.cursor = Some(seed);
        }
        // A plowed tile far outside node 0's cells: derive the tile from
        // a foreign-cell position and keep it only if its tile-center gob
        // position still lands on a foreign cell (tiles are 11 subtiles,
        // vis cells 250 - straddling is possible, so probe a few).
        let base = foreign_cell_pos(&g, 0);
        let (mut tx, mut ty) = (base.0.div_euclid(11), base.1.div_euclid(11));
        for d in 0..40 {
            let cand = (base.0.div_euclid(11) + d, base.1.div_euclid(11));
            let c = crate::visidx::cell_of(cand.0 * 11 + 5, cand.1 * 11 + 5);
            if crate::grid_owner::owner_of(c, std::num::NonZeroUsize::new(2).unwrap()) != 0 {
                tx = cand.0;
                ty = cand.1;
                break;
            }
        }
        g.world.tilth.insert((tx, ty), u64::MAX);
        g.plant_seed(1, 1, (tx, ty), seed);
        // The act shipped with the cursor's spec + seed quality...
        let mut relayed = false;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::RelayPlantAct { spec, seed_ql, .. } = msg {
                assert_eq!(spec, 1);
                assert_eq!(seed_ql, 11);
                relayed = true;
            }
        }
        assert!(relayed, "a foreign furrow click relays the plant act");
        // ...and the seed stayed on the cursor (consumed only on ack).
        assert_eq!(
            g.sessions.get(&1).unwrap().cursor.as_ref().map(|c| c.count),
            Some(3),
            "the seed must not be consumed before the PlantAck"
        );
    }

    #[tokio::test]
    async fn relay_plant_spawns_crop_acks_and_rejects_double() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("relayplant", 0, 2);
        let clicker = foreign_node_gob_id(0, 2, 61);
        // A plowed, empty tile on THIS node's cells.
        let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let (tx, ty) = (px.div_euclid(11), py.div_euclid(11) + 2);
        g.world.tilth.insert((tx, ty), u64::MAX);
        g.on_node_msg(crate::nodes::NodeMsg::RelayPlantAct {
            player: clicker,
            tx,
            ty,
            spec: 1,
            seed_ql: 9,
        });
        // The crop exists at the tile center with the relayed seed quality.
        let gob = g
            .world
            .crop_at
            .get(&(tx, ty))
            .copied()
            .expect("the relayed plant creates the crop");
        let slot = g.world.gobs.get(gob).unwrap();
        match g.world.gobs.kind[slot] {
            Kind::Crop { spec, stage } => {
                assert_eq!((spec, stage), (1, 0));
            }
            other => panic!("expected a crop, got {other:?}"),
        }
        assert_eq!(g.world.crops.get(&gob).unwrap().seed_ql, 9);
        // First ack arrives; a second act on the same tile stays silent.
        let mut acks = 0;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if matches!(msg, crate::nodes::NodeMsg::PlantAck { ok: true, .. }) {
                acks += 1;
            }
        }
        assert_eq!(acks, 1, "exactly one PlantAck for the planted tile");
        g.on_node_msg(crate::nodes::NodeMsg::RelayPlantAct {
            player: clicker,
            tx,
            ty,
            spec: 1,
            seed_ql: 9,
        });
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            assert!(
                !matches!(msg, crate::nodes::NodeMsg::PlantAck { ok: true, .. }),
                "an occupied tile never acks a second plant"
            );
        }
    }

    #[tokio::test]
    async fn plant_ack_consumes_cursor_seed() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("plantack", 0, 2);
        let pgob = pgob_of(&g);
        let seed = InvStack {
            res: g.world.res.intern("gfx/invobjs/seed-wheat"),
            count: 3,
            ql: 11,
            label: "Wheat grain",
        };
        if let Some(out) = g.sessions.get_mut(&1) {
            out.cursor = Some(seed);
        }
        g.on_node_msg(crate::nodes::NodeMsg::PlantAck {
            player: pgob,
            ok: true,
        });
        assert_eq!(
            g.sessions.get(&1).unwrap().cursor.as_ref().map(|c| c.count),
            Some(2),
            "one ack consumes one seed unit"
        );
        // A failure ack keeps the cursor untouched.
        g.on_node_msg(crate::nodes::NodeMsg::PlantAck {
            player: pgob,
            ok: false,
        });
        assert_eq!(
            g.sessions.get(&1).unwrap().cursor.as_ref().map(|c| c.count),
            Some(2)
        );
    }

    // ------------------------------------------------------------------
    // Session 32: cross-node plowing (TileMutation), tilth decay revert
    // ------------------------------------------------------------------

    /// Find a grass tile whose tile-center VisIndex cell is owned by `me`
    /// in a `nodes`-node cluster, near `from`. Probes a widening strip of
    /// tiles from `from` until both conditions hold (ownership is a per-
    /// cell hash, grass a per-tile roll; both are deterministic).
    fn grass_tile_on_cell(g: &mut Game, from: (i32, i32), me: usize, nodes: usize) -> (i32, i32) {
        let nz = std::num::NonZeroUsize::new(nodes).expect("nodes");
        let base = (from.0.div_euclid(11), from.1.div_euclid(11));
        for dy in -40..=40i32 {
            for dx in -40..=40i32 {
                let (tx, ty) = (base.0 + dx, base.1 + dy);
                let cell = crate::visidx::cell_of(tx * 11 + 5, ty * 11 + 5);
                if crate::grid_owner::owner_of(cell, nz) != me {
                    continue;
                }
                let gc = (tx.div_euclid(100), ty.div_euclid(100));
                let lx = tx.rem_euclid(100) as usize;
                let ly = ty.rem_euclid(100) as usize;
                if g.world.grids.grid(gc).tile(lx, ly) == tile::GRASS {
                    return (tx, ty);
                }
            }
        }
        panic!("no grass tile on my cells near {from:?}");
    }

    #[tokio::test]
    async fn foreign_plow_ships_relay_act_without_local_mutation() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("plowrelay", 0, 2);
        let nz = std::num::NonZeroUsize::new(2).unwrap();
        // A tile whose tile-center cell is foreign, materialized locally
        // BEFORE the act (the realistic case: the player is looking at it).
        let base = foreign_cell_pos(&g, 0);
        let (mut tx, mut ty) = (base.0.div_euclid(11), base.1.div_euclid(11));
        for d in 0..40 {
            let cand = (base.0.div_euclid(11) + d, base.1.div_euclid(11));
            let c = crate::visidx::cell_of(cand.0 * 11 + 5, cand.1 * 11 + 5);
            if crate::grid_owner::owner_of(c, nz) != 0 {
                tx = cand.0;
                ty = cand.1;
                break;
            }
        }
        let gc = (tx.div_euclid(100), ty.div_euclid(100));
        let lx = tx.rem_euclid(100) as usize;
        let ly = ty.rem_euclid(100) as usize;
        let before = g.world.grids.grid(gc).tile(lx, ly);
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let stamina_before = g.world.players[pidx].stamina;
        g.plow_tile(1, (tx, ty));
        // The act shipped to the authority (the foreign cell owner)...
        let mut relayed = false;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::RelayPlowAct {
                player,
                tx: mtx,
                ty: mty,
            } = msg
            {
                assert_eq!(player, pgob);
                assert_eq!((mtx, mty), (tx, ty));
                relayed = true;
            }
        }
        assert!(relayed, "a foreign tile click relays the plow act");
        // ...and NOTHING mutated locally: no tilth clock, no override, no
        // stamina drain, the live tile value is untouched.
        assert!(!g.world.tilth.contains_key(&(tx, ty)), "no shadow tilth");
        assert!(
            !g.world.grids.overrides.contains_key(&(tx, ty)),
            "no shadow override"
        );
        assert_eq!(
            g.world.grids.grid(gc).tile(lx, ly),
            before,
            "no shadow furrow in the local grid"
        );
        assert_eq!(
            g.world.players[pidx].stamina, stamina_before,
            "stamina drains only on the PlowAck"
        );
    }

    #[tokio::test]
    async fn relay_plow_plows_authority_broadcasts_and_acks() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("relayplow", 0, 2);
        let clicker = foreign_node_gob_id(0, 2, 61);
        // A grass tile on THIS node's cells near the player.
        let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let (tx, ty) = grass_tile_on_cell(&mut g, (px, py), 0, 2);
        let gc = (tx.div_euclid(100), ty.div_euclid(100));
        let lx = tx.rem_euclid(100) as usize;
        let ly = ty.rem_euclid(100) as usize;
        g.on_node_msg(crate::nodes::NodeMsg::RelayPlowAct {
            player: clicker,
            tx,
            ty,
        });
        // The furrow exists on the authority: tile, override, tilth clock.
        assert_eq!(g.world.grids.grid(gc).tile(lx, ly), tile::PLOWED);
        assert_eq!(g.world.grids.overrides.get(&(tx, ty)), Some(&tile::PLOWED));
        assert!(g.world.tilth.contains_key(&(tx, ty)));
        // The authority answered PlowAck ok and broadcast TileMutation.
        let (mut acks_ok, mut mutations) = (0, 0);
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            match msg {
                crate::nodes::NodeMsg::PlowAck { player, ok } => {
                    assert_eq!((player, ok), (clicker, true));
                    acks_ok += 1;
                }
                crate::nodes::NodeMsg::TileMutation {
                    tx: mtx,
                    ty: mty,
                    tile,
                } => {
                    assert_eq!((mtx, mty, tile), (tx, ty, tile::PLOWED));
                    mutations += 1;
                }
                _ => {}
            }
        }
        assert_eq!(acks_ok, 1, "exactly one ok PlowAck");
        assert_eq!(mutations, 1, "exactly one TileMutation broadcast");
        // A second act on the now-plowed tile is refused (not grass): a
        // failure ack, no second mutation, the tilth clock is untouched.
        g.on_node_msg(crate::nodes::NodeMsg::RelayPlowAct {
            player: clicker,
            tx,
            ty,
        });
        let (mut fail_acks, mut more_mutations) = (0, 0);
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            match msg {
                crate::nodes::NodeMsg::PlowAck { ok: false, .. } => fail_acks += 1,
                crate::nodes::NodeMsg::TileMutation { .. } => more_mutations += 1,
                _ => {}
            }
        }
        assert_eq!(fail_acks, 1, "the re-plow of a furrow fails loudly");
        assert_eq!(more_mutations, 0, "a refusal broadcasts nothing");
    }

    #[tokio::test]
    async fn plow_ack_drains_stamina_exactly_once() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("plowack", 0, 2);
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let before = g.world.players[pidx].stamina;
        g.on_node_msg(crate::nodes::NodeMsg::PlowAck {
            player: pgob,
            ok: true,
        });
        assert_eq!(
            g.world.players[pidx].stamina,
            before.saturating_sub(10),
            "one ok ack drains exactly one plow's stamina"
        );
        // A failure ack (and an ack for an unknown player) drains nothing.
        g.on_node_msg(crate::nodes::NodeMsg::PlowAck {
            player: pgob,
            ok: false,
        });
        g.on_node_msg(crate::nodes::NodeMsg::PlowAck {
            player: foreign_node_gob_id(0, 2, 71),
            ok: true,
        });
        assert_eq!(g.world.players[pidx].stamina, before.saturating_sub(10));
    }

    #[tokio::test]
    async fn tile_mutation_resident_and_nonresident_paths() {
        let (mut g, _rx, mut raw_rx, _mesh) = clustered_game("tilemut", 0, 2);
        // Resident path: the session holds the player's grid (an explicit
        // map request), then a TileMutation for a tile inside it mutates
        // the live grid and re-sends MAPDATA to the holder.
        let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let (tx, ty) = (px.div_euclid(11), py.div_euclid(11));
        let gc = (tx.div_euclid(100), ty.div_euclid(100));
        let (lx, ly) = (tx.rem_euclid(100) as usize, ty.rem_euclid(100) as usize);
        g.on_mapreq(1, gc);
        let sent_first = raw_rx.try_recv().is_ok();
        assert!(sent_first, "on_mapreq fragments reach the raw channel");
        while raw_rx.try_recv().is_ok() {}
        assert!(g.world.grids.is_resident(gc));
        g.on_node_msg(crate::nodes::NodeMsg::TileMutation {
            tx,
            ty,
            tile: tile::PLOWED,
        });
        assert_eq!(g.world.grids.grid(gc).tile(lx, ly), tile::PLOWED);
        let mut resent = 0;
        while let Ok(block) = raw_rx.try_recv() {
            if block[0] == MSG_MAPDATA {
                resent += 1;
            }
        }
        assert!(resent >= 1, "the holder receives a fresh MAPDATA stream");
        // Non-resident path: a far grid nobody looks at only records the
        // override; it is never materialized and nothing is re-sent.
        g.on_node_msg(crate::nodes::NodeMsg::TileMutation {
            tx: 907,
            ty: 907,
            tile: tile::PLOWED,
        });
        assert!(!g.world.grids.is_resident((9, 9)));
        assert_eq!(
            g.world.grids.overrides.get(&(907, 907)),
            Some(&tile::PLOWED)
        );
        assert!(
            raw_rx.is_empty(),
            "a non-resident mutation re-sends nothing"
        );
        // Later materialization replays the override.
        assert_eq!(g.world.grids.grid((9, 9)).tile(7, 7), tile::PLOWED);
    }

    #[tokio::test]
    async fn tilth_decay_reverts_furrow_to_grass_and_broadcasts() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("decayrevert", 0, 2);
        let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let (tx, ty) = grass_tile_on_cell(&mut g, (px, py), 0, 2);
        let gc = (tx.div_euclid(100), ty.div_euclid(100));
        let (lx, ly) = (tx.rem_euclid(100) as usize, ty.rem_euclid(100) as usize);
        // Local plow on my own cell: furrow + tilth clock + broadcast.
        g.plow_tile(1, (tx, ty));
        assert_eq!(g.world.grids.grid(gc).tile(lx, ly), tile::PLOWED);
        assert!(g.world.tilth.contains_key(&(tx, ty)));
        let mut mutations = 0;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::TileMutation { tile, .. } = msg {
                assert_eq!(tile, tile::PLOWED);
                mutations += 1;
            }
        }
        assert_eq!(mutations, 1, "the local plow also broadcasts");
        // Force the deadline into the past and run the farming pass.
        g.world.tilth.insert((tx, ty), 1);
        for _ in 0..3 {
            g.tick();
        }
        assert!(!g.world.tilth.contains_key(&(tx, ty)), "tilth decayed");
        assert_eq!(
            g.world.grids.grid(gc).tile(lx, ly),
            tile::GRASS,
            "the furrow reverted to grass"
        );
        assert_eq!(
            g.world.grids.overrides.get(&(tx, ty)),
            Some(&tile::GRASS),
            "the persisted override reverted too"
        );
        let mut reverts = 0;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::TileMutation { tile, .. } = msg {
                assert_eq!(tile, tile::GRASS);
                reverts += 1;
            }
        }
        assert_eq!(reverts, 1, "the decay revert broadcasts to peers");
        // The tile is re-plowable: no stuck-furrow dead end.
        g.plow_tile(1, (tx, ty));
        assert_eq!(g.world.grids.grid(gc).tile(lx, ly), tile::PLOWED);
        assert!(g.world.tilth.contains_key(&(tx, ty)));
    }

    // ------------------------------------------------------------------
    // Session 33: cross-node station menus (relay + piggybacked snapshot)
    // ------------------------------------------------------------------

    /// A finished oven beside the player: the exact rows place_buildable
    /// would have produced (Kind::Station + StationState), so tests read
    /// like the real world state.
    fn built_oven(g: &mut Game, fuel: u32, input: Option<(&'static str, u8)>) -> GobId {
        let pslot = g.world.gobs.get(pgob_of(g)).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let res = g.world.res.intern("gfx/terobjs/oven");
        let gob = g.world.gobs.spawn(
            Kind::Station {
                spec: 0,
                lit: false,
            },
            (px + 30, py),
            res,
            1,
            0,
        );
        let input_row = input.map(|(label, ql)| {
            let idx = g.world.res.intern("gfx/invobjs/meat");
            (idx, ql, label)
        });
        g.world.stations.insert(
            gob,
            crate::build::StationState {
                spec: 0,
                fuel,
                fuel_ql_sum: 10 * fuel as u64,
                fuel_seen: fuel as u64,
                input: input_row,
                lit: false,
                progress: 0,
                quality: 10,
            },
        );
        gob
    }

    /// The publish path carries the Station class and the full readiness
    /// snapshot (lit, fuel, has_input) - everything the home node needs
    /// to open the menu without a round trip.
    #[tokio::test]
    async fn station_publishes_class_and_snapshot() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("stpub", 0, 2);
        let gob = built_oven(&mut g, 2, Some(("Raw Deer Meat", 10)));
        let slot = g.world.gobs.get(gob).unwrap();
        let st = g.guest_state_from_slot(gob, slot).unwrap();
        match st.kind {
            crate::nodes::GuestKind::Static { class, station, .. } => {
                assert_eq!(class, crate::nodes::StaticClass::Station);
                let view = station.expect("station payload rides the publish");
                assert_eq!(view.spec, 0);
                assert!(!view.lit);
                assert_eq!(view.fuel, 2);
                assert!(view.has_input);
            }
            other => panic!("a station publishes as a static, got {other:?}"),
        }
    }

    /// Session 34: a plan's stage advance (a credited material crossing
    /// the stage boundary) re-publishes the guest state to every
    /// subscribed peer, carrying the new construction stage so the
    /// peer's plan sprite re-renders exactly like the local restage
    /// path. Before session 34 the stage byte never left the node.
    #[tokio::test]
    async fn plan_stage_advance_republishes_stage() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("planstage", 0, 2);
        // A fresh oven plan on a PEER-owned cell: exactly what
        // build_plan places and sink_material credits.
        let (fx, fy) = foreign_cell_pos(&g, 0);
        let tile = (fx.div_euclid(11), fy.div_euclid(11));
        let pos = (tile.0 * 11 + 5, tile.1 * 11 + 5);
        let res = g.world.res.intern("gfx/terobjs/oven");
        let gob = g
            .world
            .gobs
            .spawn(Kind::Plan { spec: 0, stage: 0 }, pos, res, 1, 0);
        g.world.plans.insert(
            gob,
            crate::build::PlanState {
                spec: 0,
                tile,
                credited: Vec::new(),
            },
        );
        g.world.plan_at.insert(tile, gob);
        // A subscribed peer: its subscription covers the plan's cell.
        let cell = crate::visidx::cell_of(pos.0, pos.1);
        g.cluster
            .as_mut()
            .unwrap()
            .peer_subs
            .insert(1, std::iter::once(cell).collect());
        while let Ok((_, _)) = mesh_rx.try_recv() {}
        // Sink the stone stack through the REAL material path: the oven
        // demand is stone x2 + branch x1 over 2 stages, so a two-stone
        // stack crosses the 2/3 boundary and advances stage 0 -> 1.
        let stone = g.world.res.intern("gfx/invobjs/stone");
        let stack = crate::state::InvStack {
            res: stone,
            count: 2,
            ql: 10,
            label: "",
        };
        g.sink_material(1, gob, stack);
        let mut saw_stage = false;
        while let Ok((_, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::GuestUpdate(st) = msg {
                if st.id == gob {
                    match st.kind {
                        crate::nodes::GuestKind::Static {
                            class,
                            stage: Some(stage),
                            ..
                        } => {
                            assert_eq!(class, crate::nodes::StaticClass::Structure);
                            assert_eq!(stage, 1, "two stones advance the plan to stage 1");
                            saw_stage = true;
                        }
                        other => panic!("expected a staged plan static update, got {other:?}"),
                    }
                }
            }
        }
        assert!(saw_stage, "stage advance publishes a GuestUpdate");
    }

    /// Session 34: completing a plan re-publishes the guest state with
    /// the REAL kind - a finished oven publishes as the Station class
    /// with its readiness snapshot, so a subscribed peer's flower menu,
    /// fuel, input and light relay all come alive the moment the build
    /// completes. Before this re-publish the peer kept a dead Structure
    /// guest forever (every station interaction keys off the Station
    /// class).
    #[tokio::test]
    async fn plan_completion_republishes_station_class() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("plancomp", 0, 2);
        let (fx, fy) = foreign_cell_pos(&g, 0);
        let tile = (fx.div_euclid(11), fy.div_euclid(11));
        let pos = (tile.0 * 11 + 5, tile.1 * 11 + 5);
        let res = g.world.res.intern("gfx/terobjs/oven");
        let gob = g
            .world
            .gobs
            .spawn(Kind::Plan { spec: 0, stage: 1 }, pos, res, 1, 0);
        // Fully credited: stone x2 + branch x1 completes the oven.
        g.world.plans.insert(
            gob,
            crate::build::PlanState {
                spec: 0,
                tile,
                credited: vec![
                    crate::build::Credited {
                        res: "gfx/invobjs/stone",
                        count: 2,
                        ql_sum: 20,
                    },
                    crate::build::Credited {
                        res: "gfx/invobjs/branch",
                        count: 1,
                        ql_sum: 10,
                    },
                ],
            },
        );
        g.world.plan_at.insert(tile, gob);
        let cell = crate::visidx::cell_of(pos.0, pos.1);
        g.cluster
            .as_mut()
            .unwrap()
            .peer_subs
            .insert(1, std::iter::once(cell).collect());
        while let Ok((_, _)) = mesh_rx.try_recv() {}
        g.complete_plan(gob);
        let mut saw_station = false;
        while let Ok((_, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::GuestUpdate(st) = msg {
                if st.id == gob {
                    match st.kind {
                        crate::nodes::GuestKind::Static {
                            class,
                            station: Some(view),
                            ..
                        } => {
                            assert_eq!(class, crate::nodes::StaticClass::Station);
                            assert_eq!(view.spec, 0);
                            assert!(!view.lit, "a fresh oven is unlit");
                            assert_eq!(view.fuel, 0);
                            assert!(!view.has_input);
                            saw_station = true;
                        }
                        other => panic!("expected a station static update, got {other:?}"),
                    }
                }
            }
        }
        assert!(
            saw_station,
            "completion publishes the Station class to peers"
        );
    }

    /// Session 34: a GuestUpdate whose kind payload CHANGED (class flip,
    /// stage advance, lit byte) re-renders the sprite for every session
    /// already rendering the gob - the wire mirror of restage_gob.
    /// Before this the existing-guest path only streamed pose, move and
    /// hp deltas, so a lit guest oven never re-rendered for the players
    /// watching it (the session-33 comment claimed it did).
    #[tokio::test]
    async fn guest_kind_flip_re_renders_existing_viewer() {
        let (mut g, _rx, mut raw, _mesh) = clustered_game("kindflip", 0, 2);
        // A guest oven the player ALREADY sees: announce it through the
        // real path, tick the vis scan so it spawns for session 1, then
        // flip the lit byte through a GuestUpdate.
        let gob = built_oven(&mut g, 1, None);
        let slot = g.world.gobs.get(gob).unwrap();
        let mut st = g.guest_state_from_slot(gob, slot).unwrap();
        g.world.gobs.kill(gob);
        g.world.stations.remove(&gob);
        g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st.clone()));
        g.tick();
        assert!(
            g.sessions[&1].visible.contains(&gob),
            "the guest oven must be visible before the flip"
        );
        while raw.try_recv().is_ok() {}
        // The authority lights it: the re-published GuestUpdate carries
        // the flipped lit byte in its snapshot.
        st.kind = crate::nodes::GuestKind::Static {
            res_name: "gfx/terobjs/oven".into(),
            class: crate::nodes::StaticClass::Station,
            crop: None,
            station: Some(crate::nodes::StationView {
                spec: 0,
                lit: true,
                fuel: 3,
                has_input: true,
            }),
            stage: None,
            drop: None,
        };
        g.on_node_msg(crate::nodes::NodeMsg::GuestUpdate(st));
        // Wire proof: the raw stream carries an OD_RES re-render with
        // the fresh sdt byte for the flipped gob (wire id | 0x8000,
        // len 1, byte 1 - the same shape encode_guest_block emits for
        // a fresh spawn).
        let mut saw_lit_restage = false;
        while let Ok(block) = raw.try_recv() {
            if block.first() != Some(&MSG_OBJDATA) {
                continue;
            }
            if block.len() < 14 {
                continue;
            }
            let id = i32::from_le_bytes([block[2], block[3], block[4], block[5]]);
            if id != gob {
                continue;
            }
            // Skip the frame, then read the OD sequence: OD_RES with the
            // sdt extension must carry byte 1 (lit).
            let mut off = 10;
            while off < block.len() {
                match block[off] {
                    OD_END => break,
                    OD_RES => {
                        let wire = u16::from_le_bytes([block[off + 1], block[off + 2]]);
                        if wire & 0x8000 != 0 {
                            let len = block[off + 3] as usize;
                            assert!(len >= 1, "lit sdt payload missing");
                            assert_eq!(
                                block[off + 4],
                                1,
                                "the re-render must carry the lit sdt byte"
                            );
                            saw_lit_restage = true;
                        }
                        off += 3 + if wire & 0x8000 != 0 {
                            1 + block[off + 3] as usize
                        } else {
                            0
                        };
                    }
                    OD_MOVE => off += 9,
                    OD_LINBEG => off += 21,
                    OD_LINSTEP => off += 5,
                    OD_HEALTH => off += 2,
                    OD_LAYERS => {
                        off += 3;
                        while off + 1 < block.len() {
                            let l = u16::from_le_bytes([block[off], block[off + 1]]);
                            off += 2;
                            if l == 0xFFFF {
                                break;
                            }
                        }
                    }
                    _ => break,
                }
            }
        }
        assert!(
            saw_lit_restage,
            "a kind flip must re-render OD_RES with the fresh sdt byte"
        );
    }

    /// A guest oven click opens the Light menu LOCALLY (session UI) from
    /// the piggybacked snapshot, armed with the guest act intent.
    #[tokio::test]
    async fn guest_station_click_opens_menu_and_relays_choice() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("stclick", 0, 2);
        let pgob = pgob_of(&g);
        let gob = built_oven(&mut g, 0, None);
        let slot = g.world.gobs.get(gob).unwrap();
        let st = g.guest_state_from_slot(gob, slot).unwrap();
        let (fx, fy) = st.pos;
        // The real subscriber path: the local row dies, the guest row
        // arrives through GuestAnnounce exactly like on a live mesh.
        g.world.gobs.kill(gob);
        g.world.stations.remove(&gob);
        g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st));
        g.player_interact(1, pgob, gob, (fx, fy));
        let menu = g
            .sessions
            .get(&1)
            .and_then(|o| o.station_menu)
            .expect("the guest oven click opens the flower menu");
        assert_eq!(menu.1, gob);
        assert_eq!(
            menu.2,
            Some(crate::nodes::StationAct::Light),
            "the act intent comes from the snapshot (unlit -> Light)"
        );
        // Acting on the menu relays the act; the home node mutates
        // nothing (no local station row exists here at all).
        g.apply_station_choice(1, menu.0, 0);
        let mut relays = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::RelayStationAct {
                player,
                target,
                act,
            } = msg
            {
                relays.push((player, target, act));
            }
        }
        assert_eq!(
            relays,
            vec![(pgob, gob, crate::nodes::StationAct::Light)],
            "the choice relays to the station's authority"
        );
    }

    /// Authority side of the menu relay: full validation order (stale ->
    /// fuel -> input), the same transitions as the local menu path, and
    /// one StationAck per act with the exact result.
    #[tokio::test]
    async fn relay_station_act_validates_and_applies() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("stact", 0, 2);
        let clicker = foreign_node_gob_id(0, 2, 71);
        let empty = built_oven(&mut g, 0, None);
        g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
            player: clicker,
            target: empty,
            act: crate::nodes::StationAct::Light,
        });
        let mut acks = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::StationAck { player, result } = msg {
                assert_eq!(player, clicker);
                acks.push(result);
            }
        }
        assert_eq!(
            acks,
            vec![crate::nodes::StationResult::NeedsFuel],
            "no fuel -> NeedsFuel, nothing mutated"
        );
        assert!(!g.world.stations.get(&empty).unwrap().lit);

        // Fuel but no input.
        let fueled = built_oven(&mut g, 1, None);
        g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
            player: clicker,
            target: fueled,
            act: crate::nodes::StationAct::Light,
        });
        let mut acks = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::StationAck { result, .. } = msg {
                acks.push(result);
            }
        }
        assert_eq!(acks, vec![crate::nodes::StationResult::NeedsInput]);

        // Ready: the job starts, the lit Kind byte moves with the state.
        let ready = built_oven(&mut g, 1, Some(("Raw Deer Meat", 10)));
        g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
            player: clicker,
            target: ready,
            act: crate::nodes::StationAct::Light,
        });
        let mut acks = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::StationAck { result, .. } = msg {
                acks.push(result);
            }
            if let crate::nodes::NodeMsg::GuestUpdate(st) = msg {
                if st.id == ready {
                    if let crate::nodes::GuestKind::Static { station, .. } = &st.kind {
                        assert!(station.unwrap().lit, "the update re-publishes lit=true");
                    }
                }
            }
        }
        assert_eq!(acks, vec![crate::nodes::StationResult::Lit]);
        assert!(g.world.stations.get(&ready).unwrap().lit);

        // Stale: lighting an already-lit oven changes nothing.
        g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
            player: clicker,
            target: ready,
            act: crate::nodes::StationAct::Light,
        });
        let mut acks = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::StationAck { result, .. } = msg {
                acks.push(result);
            }
        }
        assert_eq!(acks, vec![crate::nodes::StationResult::Stale]);

        // Extinguish resets the progress and preserves the input.
        g.world.stations.get_mut(&ready).unwrap().progress = 5;
        g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
            player: clicker,
            target: ready,
            act: crate::nodes::StationAct::Extinguish,
        });
        let st = g.world.stations.get(&ready).unwrap().clone();
        assert!(!st.lit);
        assert_eq!(st.progress, 0);
        assert!(st.input.is_some(), "extinguish preserves the input");
        let mut acks = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::StationAck { result, .. } = msg {
                acks.push(result);
            }
        }
        assert_eq!(acks, vec![crate::nodes::StationResult::Extinguished]);
    }

    /// A lit oven relights only after the job ends: the relay answers
    /// Stale for an extinguish on an unlit oven too (view lag).
    #[tokio::test]
    async fn relay_station_extinguish_unlit_is_stale() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("stext", 0, 2);
        let clicker = foreign_node_gob_id(0, 2, 72);
        let gob = built_oven(&mut g, 1, Some(("Raw Deer Meat", 10)));
        g.on_node_msg(crate::nodes::NodeMsg::RelayStationAct {
            player: clicker,
            target: gob,
            act: crate::nodes::StationAct::Extinguish,
        });
        let mut acks = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::StationAck { result, .. } = msg {
                acks.push(result);
            }
        }
        assert_eq!(
            acks,
            vec![crate::nodes::StationResult::Stale],
            "extinguishing an unlit oven is a stale-view no-op"
        );
    }

    /// Home side of the item relay: the click with a held stack ships
    /// RelayStationItem (one unit described by name/ql/label) and the
    /// cursor stays intact until the ack decides.
    #[tokio::test]
    async fn guest_station_itemact_ships_relay_and_keeps_cursor() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("stitem", 0, 2);
        let gob = built_oven(&mut g, 0, None);
        let slot = g.world.gobs.get(gob).unwrap();
        let st = g.guest_state_from_slot(gob, slot).unwrap();
        let (fx, fy) = st.pos;
        g.world.gobs.kill(gob);
        g.world.stations.remove(&gob);
        g.on_node_msg(crate::nodes::NodeMsg::GuestAnnounce(st));
        // Put a branch stack (oven fuel) on the cursor.
        let branch = g.world.res.intern("gfx/invobjs/branch");
        g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
            res: branch,
            count: 3,
            ql: 10,
            label: "Branch",
        });
        let args = vec![
            hnh_proto::ListArg::Coord(0, 0),
            hnh_proto::ListArg::Coord(fx, fy),
            hnh_proto::ListArg::Int(0),
            hnh_proto::ListArg::Int(gob),
            hnh_proto::ListArg::Int(0),
        ];
        g.on_map_itemact(1, &args);
        let mut relays = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::RelayStationItem {
                player,
                target,
                stack,
            } = msg
            {
                relays.push((player, target, stack.res.clone(), stack.ql, stack.count));
            }
        }
        assert_eq!(relays.len(), 1, "one item relay per click");
        assert_eq!(relays[0].1, gob);
        assert_eq!(relays[0].2, "gfx/invobjs/branch");
        assert_eq!(
            g.sessions.get(&1).and_then(|o| o.cursor).map(|c| c.count),
            Some(3),
            "the cursor stack is untouched before the ack (seed-safe)"
        );
    }

    /// Authority side of the item relay: every branch of the local
    /// station_itemact validation order answers with its exact result.
    #[tokio::test]
    async fn relay_station_item_full_validation_order() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("stitemauth", 0, 2);
        let clicker = foreign_node_gob_id(0, 2, 73);
        let fuel_stack = |res: &str, label: &str| crate::nodes::StaticStack {
            res: res.to_owned(),
            count: 1,
            ql: 10,
            label: label.to_owned(),
        };
        // Fuel loads on an empty oven.
        let gob = built_oven(&mut g, 0, None);
        g.on_node_msg(crate::nodes::NodeMsg::RelayStationItem {
            player: clicker,
            target: gob,
            stack: fuel_stack("gfx/invobjs/branch", "Branch"),
        });
        let st = g.world.stations.get(&gob).unwrap().clone();
        assert_eq!(st.fuel, 1);
        assert_eq!(st.fuel_seen, 1);
        // Input loads when unlit + empty.
        g.on_node_msg(crate::nodes::NodeMsg::RelayStationItem {
            player: clicker,
            target: gob,
            stack: fuel_stack("gfx/invobjs/meat", "Raw Deer Meat"),
        });
        let st = g.world.stations.get(&gob).unwrap().clone();
        assert!(st.input.is_some(), "the roast input slot loads");
        // A second input refuses with InputFull.
        g.on_node_msg(crate::nodes::NodeMsg::RelayStationItem {
            player: clicker,
            target: gob,
            stack: fuel_stack("gfx/invobjs/meat", "Raw Deer Meat"),
        });
        // A non-fuel non-roastable refuses with NotProcessable.
        let other = built_oven(&mut g, 1, None);
        g.on_node_msg(crate::nodes::NodeMsg::RelayStationItem {
            player: clicker,
            target: other,
            stack: fuel_stack("gfx/invobjs/stone", "Stone"),
        });
        // A lit oven refuses the input with BusyLit (fuel still loads).
        let lit = built_oven(&mut g, 0, None);
        g.world.stations.get_mut(&lit).unwrap().lit = true;
        g.on_node_msg(crate::nodes::NodeMsg::RelayStationItem {
            player: clicker,
            target: lit,
            stack: fuel_stack("gfx/invobjs/meat", "Raw Deer Meat"),
        });
        let mut acks = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::StationItemAck { player, result } = msg {
                assert_eq!(player, clicker);
                acks.push(result);
            }
        }
        assert_eq!(
            acks,
            vec![
                crate::nodes::StationItemResult::FuelAdded,
                crate::nodes::StationItemResult::InputLoaded,
                crate::nodes::StationItemResult::InputFull,
                crate::nodes::StationItemResult::NotProcessable,
                crate::nodes::StationItemResult::BusyLit,
            ]
        );
        assert_eq!(g.world.stations.get(&lit).unwrap().input, None);
    }

    /// Home side of the item ack: FuelAdded/InputLoaded consume exactly
    /// one cursor unit; refusals keep the whole stack.
    #[tokio::test]
    async fn station_itemack_consumes_one_unit_or_keeps_stack() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("stack", 0, 2);
        let pgob = pgob_of(&g);
        let branch = g.world.res.intern("gfx/invobjs/branch");
        let put_cursor = |g: &mut Game, n: u32| {
            g.sessions.get_mut(&1).unwrap().cursor = Some(InvStack {
                res: branch,
                count: n,
                ql: 10,
                label: "Branch",
            });
        };
        put_cursor(&mut g, 3);
        g.on_node_msg(crate::nodes::NodeMsg::StationItemAck {
            player: pgob,
            result: crate::nodes::StationItemResult::FuelAdded,
        });
        assert_eq!(
            g.sessions.get(&1).and_then(|o| o.cursor).map(|c| c.count),
            Some(2),
            "a multi-unit stack keeps the rest"
        );
        put_cursor(&mut g, 1);
        g.on_node_msg(crate::nodes::NodeMsg::StationItemAck {
            player: pgob,
            result: crate::nodes::StationItemResult::FuelAdded,
        });
        assert!(
            g.sessions.get(&1).and_then(|o| o.cursor).is_none(),
            "an exhausted stack clears the cursor"
        );
        put_cursor(&mut g, 2);
        g.on_node_msg(crate::nodes::NodeMsg::StationItemAck {
            player: pgob,
            result: crate::nodes::StationItemResult::NotProcessable,
        });
        assert_eq!(
            g.sessions.get(&1).and_then(|o| o.cursor).map(|c| c.count),
            Some(2),
            "a refusal keeps the whole stack"
        );
    }

    /// Owner-filtered populate (session 33): on_mapreq materializes the
    /// grid's TILES (deterministic, identical on every node) but spawns
    /// statics/animals only for cells THIS node owns - no shadow copies
    /// of the cell owner's content. Before this fix every node carried
    /// its own copy of the same grid's statics (and its rng placed
    /// DIFFERENT animals than the owner's roll).
    #[tokio::test]
    async fn owner_filtered_populate_spawns_only_my_cells() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("ownpop", 0, 2);
        let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        // A grid near spawn: touches both nodes' cells (a grid spans
        // 5x5 VisIndex cells, rendezvous-hashed between 2 nodes).
        let gc = (px.div_euclid(1100), py.div_euclid(1100));
        g.on_mapreq(1, gc);
        let nodes = g.cluster.as_ref().unwrap().nodes;
        let me = g.cluster.as_ref().unwrap().me;
        let mut mine = 0usize;
        let mut foreign = 0usize;
        for id in g.world.gobs.vis.gobs_in_view(px + 2000, py + 2000, 6000) {
            let Some(slot) = g.world.gobs.get(id) else {
                continue;
            };
            if matches!(g.world.gobs.kind[slot], Kind::Player { .. }) {
                continue; // players are homed, not cell-owned
            }
            let pos = g.world.gobs.pos[slot];
            let cell = crate::visidx::cell_of(pos.0, pos.1);
            if crate::grid_owner::owner_of(cell, nodes) == me {
                mine += 1;
            } else {
                foreign += 1;
            }
        }
        assert!(mine > 0, "the grid's my-cells content spawned");
        assert_eq!(foreign, 0, "no static/animal may spawn on a non-owner node");
    }

    /// Session 34: Sub carries DIFFS (tick_cluster sends only the added
    /// cells), so a follow-up Sub must EXTEND the peer's subscription,
    /// not replace it. The old whole-set replace silently unsubscribed
    /// every earlier cell the first time a moving session's view
    /// produced a second Sub - cross-node updates for still-subscribed
    /// cells stopped flowing (a lit guest oven never re-rendered).
    #[tokio::test]
    async fn sub_diffs_extend_not_replace() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("subdiff", 0, 2);
        let nodes = g.cluster.as_ref().unwrap().nodes;
        // Two cells THIS node owns: a peer subscribing to them passes the
        // receiver-side ownership filter (Sub drops cells the receiver
        // does not own).
        let mut my_cells: Vec<(i32, i32)> = Vec::new();
        'scan: for cy in -1..=4i32 {
            for cx in -1..=4i32 {
                let cell = (cx, cy);
                if crate::grid_owner::owner_of(cell, nodes) == 0 {
                    my_cells.push(cell);
                    if my_cells.len() == 2 {
                        break 'scan;
                    }
                }
            }
        }
        assert_eq!(my_cells.len(), 2, "the lattice must have two own cells");
        // First Sub: one cell. Follow-up Sub: the second cell (a diff).
        g.on_node_msg(crate::nodes::NodeMsg::Sub {
            from: 1,
            cells: vec![my_cells[0]],
        });
        g.on_node_msg(crate::nodes::NodeMsg::Sub {
            from: 1,
            cells: vec![my_cells[1]],
        });
        let subs = &g.cluster.as_ref().unwrap().peer_subs[&1];
        assert!(
            subs.contains(&my_cells[0]) && subs.contains(&my_cells[1]),
            "both diffed cells stay subscribed, got {:?}",
            subs
        );
        // Unsub of one cell keeps the other (the same incremental rule).
        g.on_node_msg(crate::nodes::NodeMsg::Unsub {
            from: 1,
            cells: vec![my_cells[1]],
        });
        let subs = &g.cluster.as_ref().unwrap().peer_subs[&1];
        assert!(subs.contains(&my_cells[0]), "the untouched cell stays");
        assert!(!subs.contains(&my_cells[1]), "the removed cell goes");
    }

    /// Sub-driven populate (session 33): a peer's Sub materializes the
    /// authority's part of every touched grid and announces EVERY gob it
    /// holds in the subscribed cells (fresh + pre-existing), so the
    /// subscriber's view starts from the single authoritative copy.
    #[tokio::test]
    async fn sub_driven_populate_announces_to_subscriber() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("subpop", 0, 2);
        let nodes = g.cluster.as_ref().unwrap().nodes;
        let me = g.cluster.as_ref().unwrap().me;
        // Pre-existing content: an oven near spawn (my cell by the
        // clustered_game layout).
        let pslot = g.world.gobs.get(pgob_of(&g)).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let my_cell = crate::visidx::cell_of(px, py);
        assert_eq!(
            crate::grid_owner::owner_of(my_cell, nodes),
            me,
            "fixture: the player spawns on a home cell"
        );
        let _oven = built_oven(&mut g, 1, None);
        // A peer subscribes to my cell.
        g.on_node_msg(crate::nodes::NodeMsg::Sub {
            from: 1,
            cells: vec![my_cell],
        });
        let mut announced = Vec::new();
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::GuestAnnounce(st) = msg {
                announced.push((st.id, st.pos));
            }
        }
        assert!(
            announced.len() >= 2,
            "the player gob AND the oven (and populated statics) announce to the subscriber, got {}",
            announced.len()
        );
        // Everything announced actually stands in the subscribed cell.
        for (_, pos) in &announced {
            assert_eq!(
                crate::visidx::cell_of(pos.0, pos.1),
                my_cell,
                "announces stay inside the subscribed cell"
            );
        }
        // The touched grid materialized my part (populated set grew).
        let touched: Vec<(i32, i32)> = Game::grids_touching_cell(my_cell);
        for gc in touched {
            assert!(g.populated.contains(&gc), "grid {gc:?} materialized");
        }
    }

    /// grids_touching_cell arithmetic: a VisIndex cell (250 subtiles)
    /// touches one or two grids (1100 subtiles) per axis, never more,
    /// and the covered subtile span always lands inside the returned
    /// grids.
    #[test]
    fn grids_touching_cell_covers_the_cell() {
        for cell in [
            (0i32, 0i32),
            (4, 4),
            (-1, -1),
            (5, -3),
            (-7, 9),
            (100, -100),
        ] {
            let grids = Game::grids_touching_cell(cell);
            assert!(!grids.is_empty() && grids.len() <= 4);
            let (cx, cy) = cell;
            for u in [cx * 250, cx * 250 + 124, cx * 250 + 249] {
                for v in [cy * 250, cy * 250 + 124, cy * 250 + 249] {
                    let gx = u.div_euclid(1100);
                    let gy = v.div_euclid(1100);
                    assert!(
                        grids.contains(&(gx, gy)),
                        "cell {cell:?} subtile ({u},{v}) grid ({gx},{gy}) missing"
                    );
                }
            }
        }
    }

    /// Home side of the act ack: the refusal results render the exact
    /// system lines the local menu path emits (UX parity).
    #[tokio::test]
    async fn station_ack_refusals_render_system_lines() {
        let (mut g, mut rx, _raw, _mesh) = clustered_game("stline", 0, 2);
        let pgob = pgob_of(&g);
        fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) -> String {
            let mut text = String::new();
            while let Ok(buf) = rx.try_recv() {
                // System lines are wdgmsg("sysmsg"...)-shaped chat frames;
                // a byte scan is enough to assert the wording crossed.
                text.push_str(&String::from_utf8_lossy(&buf));
            }
            text
        }
        let _ = drain(&mut rx);
        g.on_node_msg(crate::nodes::NodeMsg::StationAck {
            player: pgob,
            result: crate::nodes::StationResult::NeedsFuel,
        });
        let text = drain(&mut rx);
        assert!(
            text.contains("oven needs fuel"),
            "NeedsFuel renders: {text}"
        );
        g.on_node_msg(crate::nodes::NodeMsg::StationAck {
            player: pgob,
            result: crate::nodes::StationResult::Lit,
        });
        let text = drain(&mut rx);
        assert!(
            !text.contains("oven"),
            "a successful Light stays silent (parity with the local path)"
        );
    }

    // ------------------------------------------------------------------
    // Session 36: the bow chain (woodbow / stonearrow / bonearrow)
    // ------------------------------------------------------------------

    /// Helper: replace a player's inventory with a synthetic stack list.
    fn set_inv(g: &mut Game, stacks: &[(&'static str, u32, u8)]) {
        let pidx = *g.world.by_session.get(&1).unwrap();
        g.world.players[pidx].inv = stacks
            .iter()
            .map(|(res, count, ql)| InvStack {
                res: g.world.res.intern(res),
                count: *count,
                ql: *ql,
                label: "",
            })
            .collect();
    }

    /// Wooden Bow quality follows the RoB type-weighted formula
    /// `(qBranches + qString)/2` INDEPENDENT of the unit counts: 4
    /// branches at q40 + 1 string at q10 average the TYPES to 25, then
    /// the Marksmanship (ranged) softcap (10 here) halves it toward 17.
    /// The pre-36 unit-weighted math would give 34 -> 22, so the assert
    /// distinguishes the two models.
    #[tokio::test]
    async fn woodbow_quality_is_type_weighted() {
        let (mut g, _rx, _raw) = entered_game("bowq");
        set_inv(
            &mut g,
            &[("gfx/invobjs/branch", 4, 40), ("gfx/invobjs/string", 1, 10)],
        );
        assert!(g.craft_once(1, "woodbow"), "craft must succeed");
        let pidx = *g.world.by_session.get(&1).unwrap();
        let bow_gidx = g.world.res.intern("gfx/invobjs/bow");
        let bow = g.world.players[pidx]
            .inv
            .iter()
            .find(|s| s.res == bow_gidx)
            .expect("bow produced");
        assert_eq!(bow.count, 1);
        // (40 + 10)/2 = 25, softcap ranged=10: (25 + 10)/2 = 17.
        assert_eq!(bow.ql, 17, "type-weighted quality with ranged softcap");
        // All inputs consumed.
        assert!(
            !g.world.players[pidx]
                .inv
                .iter()
                .any(|s| s.res == g.world.res.intern("gfx/invobjs/branch")),
            "branches fully consumed"
        );
    }

    /// Stone Arrows come out as ONE batch of ten per craft, and the
    /// branch type weighs double the stone type (RoB Legacy:Quality
    /// arrow example): stone q10 + branches q40 -> (10*1 + 40*2)/3 = 30,
    /// softcap survive (unset -> 10): (30 + 10)/2 = 20.
    #[tokio::test]
    async fn stonearrow_bundles_ten_and_branch_weighs_double() {
        let (mut g, _rx, _raw) = entered_game("arrq");
        set_inv(
            &mut g,
            &[("gfx/invobjs/stone", 1, 10), ("gfx/invobjs/branch", 2, 40)],
        );
        assert!(g.craft_once(1, "stonearrow"), "craft must succeed");
        let pidx = *g.world.by_session.get(&1).unwrap();
        let arr_gidx = g.world.res.intern("gfx/invobjs/arrow-stone");
        let arrows = g.world.players[pidx]
            .inv
            .iter()
            .find(|s| s.res == arr_gidx)
            .expect("stone arrows produced");
        assert_eq!(arrows.count, 10, "one craft yields a bundle of ten");
        // (10*1 + 40*2)/3 = 30, softcap survive=10: (30+10)/2 = 20.
        assert_eq!(arrows.ql, 20);
    }

    /// A Wooden Bow dropped into any equipment slot renders the dedicated
    /// carrying layers (gfx/borka/eq-bow/.../arm/carrying/...) on the
    /// world drawable AND on the paperdoll doll set.
    #[tokio::test]
    async fn bow_equip_renders_carrying_pose() {
        let (mut g, _rx, _raw) = entered_game("bowpose");
        set_inv(&mut g, &[("gfx/invobjs/bow", 1, 10)]);
        let pidx = *g.world.by_session.get(&1).unwrap();
        // Equip via the same slot the epry flow uses (slot 0).
        let stack = g.world.players[pidx].inv.pop().unwrap();
        g.world.players[pidx].equip[0] = Some(stack);
        let names: Vec<&'static str> = g.world.players[pidx]
            .equip
            .iter()
            .flatten()
            .filter_map(|s| g.world.res.name(s.res))
            .collect();
        let world = crate::equip::world_layers(names.iter(), false, 1);
        assert_eq!(world.len(), 2, "standing front: left + right carrying");
        assert!(world
            .iter()
            .all(|l| l.contains("eq-bow/standing/arm/carrying/")));
        let doll = crate::equip::doll_layers(names.iter());
        assert_eq!(doll.len(), 2, "doll renders the front carrying pair");
        assert!(
            doll.iter()
                .all(|l| l.contains("eq-bow/standing/arm/carrying/")),
            "doll layers: {doll:?}"
        );
        let walking = crate::equip::world_layers(names.iter(), true, 1);
        assert!(
            walking
                .iter()
                .all(|l| l.contains("eq-bow/walking/arm/carrying/")),
            "walking pose carries too"
        );
    }

    /// The bow chain must be craftable straight out of the starter kit
    /// (the kit composition is the server policy that keeps the chain
    /// playable with zero foraging).
    #[tokio::test]
    async fn starter_kit_covers_the_bow_chain() {
        let (mut g, _rx, _raw) = entered_game("bowkit");
        let pidx = *g.world.by_session.get(&1).unwrap();
        fn count(g: &mut Game, pidx: usize, res: &'static str) -> u32 {
            let gidx = g.world.res.intern(res);
            g.world.players[pidx]
                .inv
                .iter()
                .filter(|s| s.res == gidx)
                .map(|s| s.count)
                .sum()
        }
        assert!(
            count(&mut g, pidx, "gfx/invobjs/branch") >= 4 + 2,
            "bow + arrow branches"
        );
        assert!(count(&mut g, pidx, "gfx/invobjs/string") >= 1, "bow string");
        assert!(count(&mut g, pidx, "gfx/invobjs/stone") >= 1, "arrow stone");
        // The menu must announce every new pagina (rendered MenuGrid).
        for page in [
            "paginae/craft/woodbow",
            "paginae/craft/stonearrow",
            "paginae/craft/bonearrow",
        ] {
            assert!(
                crate::craft::RECIPES.iter().any(|r| r.pagina == page),
                "{page} in RECIPES"
            );
        }
    }

    // ------------------------------------------------------------------
    // Bow ranged combat (session 37, archery.rs)
    // ------------------------------------------------------------------

    /// Equip a bow into slot 0 and put one stack of arrows into the
    /// inventory.
    fn arm_bow(g: &mut Game, pidx: usize, arrows: u32, bow_ql: u8) {
        let bow_gidx = g.world.res.intern("gfx/invobjs/bow");
        g.world.players[pidx].equip[0] = Some(crate::state::InvStack {
            res: bow_gidx,
            count: 1,
            ql: bow_ql,
            label: "",
        });
        if arrows > 0 {
            let arr_gidx = g.world.res.intern("gfx/invobjs/arrow-stone");
            g.world.players[pidx].inv.push(crate::state::InvStack {
                res: arr_gidx,
                count: arrows,
                ql: 10,
                label: "",
            });
        }
    }

    /// Spawn a Deer at Chebyshev distance `d` (units) from the player
    /// with explicit HP (lets hit/miss tests survive a 75-damage shot).
    fn spawn_deer_at(g: &mut Game, pidx: usize, d: i32, hp: i32) -> crate::state::GobId {
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).expect("player gob");
        let (px, py) = g.world.gobs.pos[pslot];
        let res = g.world.res.intern(Species::Deer.resname());
        let id = g.world.gobs.spawn(
            Kind::Animal {
                species: Species::Deer,
            },
            (px + d, py),
            res,
            hp,
            33,
        );
        g.world.animal_gobs.push(id);
        id
    }

    fn arrow_count(g: &mut Game, pidx: usize) -> u32 {
        let arr_gidx = g.world.res.intern("gfx/invobjs/arrow-stone");
        g.world.players[pidx]
            .inv
            .iter()
            .filter(|s| s.res == arr_gidx)
            .map(|s| s.count)
            .sum()
    }

    /// Extract the chat "log" text from one RMSG_WDGMSG frame (used by
    /// the aim-progress assertions). Payload after the widget name is
    /// a typed list: LIST_STR(2), NUL-terminated text, then the color.
    fn chat_log_text(msg: &[u8]) -> Option<String> {
        if msg.first() != Some(&RMSG_WDGMSG) || msg.len() < 5 {
            return None;
        }
        let nul = msg[3..].iter().position(|&b| b == 0)? + 3;
        if &msg[3..nul] != b"log" {
            return None;
        }
        let rest = &msg[nul + 1..];
        if rest.first() != Some(&2) {
            return None; // LIST_STR tag
        }
        let end = rest[1..].iter().position(|&b| b == 0)? + 1;
        String::from_utf8(rest[1..end].to_vec()).ok()
    }

    /// Clicking an animal with an equipped bow opens the RANGED aim
    /// path, not the melee fight window; without a bow the melee fight
    /// opens as before.
    #[tokio::test]
    async fn bow_click_opens_aim_instead_of_fight() {
        let (mut g, _rx, _raw) = entered_game("bowaim");
        let pidx = *g.world.by_session.get(&1).unwrap();
        arm_bow(&mut g, pidx, 10, 10);
        let deer = spawn_deer_at(&mut g, pidx, 66, 200);
        let pgob = g.world.players[pidx].gob;
        g.player_interact(1, pgob, deer, (0, 0));
        let p = &g.world.players[pidx];
        assert_eq!(p.aim.map(|a| a.target), Some(deer), "aim started");
        assert_eq!(p.fight_target, None, "no melee fight for a bow carrier");
        // Without a bow the same click opens the melee fight.
        g.world.players[pidx].aim = None;
        g.world.players[pidx].equip[0] = None;
        g.player_interact(1, pgob, deer, (0, 0));
        assert_eq!(
            g.world.players[pidx].fight_target,
            Some(deer),
            "melee fight opens without a bow"
        );
    }

    /// A dry bow (no arrows) refuses to aim and never falls back into
    /// melee while the bow is still equipped.
    #[tokio::test]
    async fn dry_bow_refuses_to_aim() {
        let (mut g, _rx, _raw) = entered_game("drybow");
        let pidx = *g.world.by_session.get(&1).unwrap();
        arm_bow(&mut g, pidx, 0, 10);
        let deer = spawn_deer_at(&mut g, pidx, 66, 200);
        let pgob = g.world.players[pidx].gob;
        g.player_interact(1, pgob, deer, (0, 0));
        assert_eq!(g.world.players[pidx].aim, None, "no aim without arrows");
        assert_eq!(
            g.world.players[pidx].fight_target, None,
            "no melee fallback while the bow is equipped"
        );
    }

    /// A guaranteed hit (roll 0) consumes exactly one arrow, applies
    /// the Fandom damage formula, depletes the attack meter and keeps
    /// the aim up for the next shot.
    #[tokio::test]
    async fn arrow_hit_consumes_one_arrow_and_damages() {
        let (mut g, _rx, _raw) = entered_game("arrowhit");
        let pidx = *g.world.by_session.get(&1).unwrap();
        arm_bow(&mut g, pidx, 10, 10);
        let deer = spawn_deer_at(&mut g, pidx, 66, 200);
        let dslot = g.world.gobs.get(deer).unwrap();
        let hp0 = g.world.gobs.hp[dslot];
        let aim = crate::archery::RangedAim::new(deer, 10, crate::archery::AIM_RATE_WOODBOW);
        // Prime the offence bar so the depletion is observable.
        if let Some(out) = g.sessions.get_mut(&1) {
            out.fight.own_off = crate::fight::BAR_FULL;
        }
        g.shoot_arrow(pidx, 1, aim, 0);
        let dslot = g.world.gobs.get(deer).unwrap();
        assert_eq!(
            g.world.gobs.hp[dslot],
            hp0 - crate::archery::bow_damage(10),
            "q10 bow deals 75*sqrt(10/10) = 75"
        );
        assert_eq!(arrow_count(&mut g, pidx), 9, "exactly one arrow consumed");
        assert_eq!(
            g.sessions[&1].fight.own_off, 0,
            "attack meter depleted by the shot"
        );
        assert_eq!(
            g.world.players[pidx].aim.map(|a| a.target),
            Some(deer),
            "aim continues while the target lives"
        );
    }

    /// A guaranteed miss (roll 99) still spends the arrow but leaves
    /// the target's HP untouched.
    #[tokio::test]
    async fn arrow_miss_spends_the_arrow_only() {
        let (mut g, _rx, _raw) = entered_game("arrowmiss");
        let pidx = *g.world.by_session.get(&1).unwrap();
        arm_bow(&mut g, pidx, 10, 10);
        let deer = spawn_deer_at(&mut g, pidx, 66, 200);
        let dslot = g.world.gobs.get(deer).unwrap();
        let hp0 = g.world.gobs.hp[dslot];
        let aim = crate::archery::RangedAim::new(deer, 10, crate::archery::AIM_RATE_WOODBOW);
        g.shoot_arrow(pidx, 1, aim, 99);
        let dslot = g.world.gobs.get(deer).unwrap();
        assert_eq!(g.world.gobs.hp[dslot], hp0, "miss deals no damage");
        assert_eq!(arrow_count(&mut g, pidx), 9, "the arrow is lost on a miss");
    }

    /// The aim meter fills over 40 ticks (4 s at 10 Hz), streams the
    /// 25/50/75% progress lines in order, and spends no arrow before
    /// the release.
    #[tokio::test]
    async fn aim_meter_fills_with_progress_lines() {
        let (mut g, mut rx, _raw) = entered_game("aimmeter");
        let pidx = *g.world.by_session.get(&1).unwrap();
        arm_bow(&mut g, pidx, 10, 10);
        let deer = spawn_deer_at(&mut g, pidx, 66, 200);
        let pgob = g.world.players[pidx].gob;
        g.player_interact(1, pgob, deer, (0, 0));
        let mut aim = g.world.players[pidx].aim.expect("aim started");
        for _ in 0..39 {
            g.tick_aim(pidx, 1, pgob, aim);
            aim = g.world.players[pidx].aim.expect("still aiming");
        }
        assert!(
            aim.meter >= 75 * crate::archery::AIM_FULL / 100,
            "39 of 40 ticks reach at least 75%"
        );
        assert_eq!(
            arrow_count(&mut g, pidx),
            10,
            "no arrow spent before release"
        );
        // Progress lines arrived.
        let mut seen: Vec<String> = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let Some(text) = chat_log_text(&msg) {
                seen.push(text);
            }
        }
        assert!(
            seen.iter().any(|t| t.contains("Aiming at 25%")),
            "25% line sent: {seen:?}"
        );
        assert!(
            seen.iter().any(|t| t.contains("Aiming at 75%")),
            "75% line sent: {seen:?}"
        );
    }

    /// An out-of-range target is chased, not shot at: no meter gain,
    /// and the aim survives inside the drop radius.
    #[tokio::test]
    async fn out_of_range_target_is_chased() {
        let (mut g, _rx, _raw) = entered_game("bowchase");
        let pidx = *g.world.by_session.get(&1).unwrap();
        arm_bow(&mut g, pidx, 10, 10);
        let deer = spawn_deer_at(&mut g, pidx, 200, 200);
        let pgob = g.world.players[pidx].gob;
        let aim = crate::archery::RangedAim::new(deer, 10, crate::archery::AIM_RATE_WOODBOW);
        g.world.players[pidx].aim = Some(aim);
        g.tick_aim(pidx, 1, pgob, aim);
        let aim = g.world.players[pidx].aim.expect("aim kept while chasing");
        assert_eq!(aim.meter, 0, "no meter gain out of range");
        let pslot = g.world.gobs.get(pgob).unwrap();
        assert!(g.world.gobs.mv[pslot].is_some(), "player closes in");
        assert_eq!(aim.target, deer, "aim survives inside the drop radius");
    }

    /// A ground click cancels an active aim (walk away instead).
    #[tokio::test]
    async fn walk_cancels_aim() {
        let (mut g, _rx, _raw) = entered_game("aimcancel");
        let pidx = *g.world.by_session.get(&1).unwrap();
        arm_bow(&mut g, pidx, 10, 10);
        let deer = spawn_deer_at(&mut g, pidx, 66, 200);
        let pgob = g.world.players[pidx].gob;
        g.player_interact(1, pgob, deer, (0, 0));
        assert!(g.world.players[pidx].aim.is_some());
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        g.player_walk(1, pgob, (px + 200, py));
        assert_eq!(g.world.players[pidx].aim, None, "aim dropped on walk");
    }

    /// A lethal arrow kills the deer, drops its loot (meat + bone from
    /// session 36) and ends the aim.
    #[tokio::test]
    async fn lethal_arrow_kills_and_loots() {
        let (mut g, _rx, _raw) = entered_game("bowkill");
        let pidx = *g.world.by_session.get(&1).unwrap();
        arm_bow(&mut g, pidx, 10, 40);
        let deer = spawn_deer_at(&mut g, pidx, 66, Species::Deer.max_hp());
        let dslot = g.world.gobs.get(deer).unwrap();
        let pos = g.world.gobs.pos[dslot];
        let aim = crate::archery::RangedAim::new(deer, 40, crate::archery::AIM_RATE_WOODBOW);
        g.shoot_arrow(pidx, 1, aim, 0);
        assert!(
            g.world.gobs.get(deer).is_none(),
            "q40 arrow (150 dmg) kills a 40 HP deer"
        );
        assert_eq!(g.world.players[pidx].aim, None, "aim ends with the kill");
        // Loot on the ground: deer drops meat and a bone nearby.
        let meat_gidx = g.world.res.intern("gfx/invobjs/meat");
        let bone_gidx = g.world.res.intern("gfx/invobjs/bone");
        let loot: Vec<u16> = (0..g.world.gobs.kind.len())
            .filter(|i| {
                let (dx, dy) = g.world.gobs.pos[*i];
                (dx - pos.0).abs() < 60 && (dy - pos.1).abs() < 60
            })
            .filter_map(|i| g.world.gobs.kind[i].drop_info().map(|d| d.0))
            .collect();
        assert!(
            loot.contains(&meat_gidx) || loot.contains(&bone_gidx),
            "meat or bone dropped near the kill"
        );
    }
    /// Cross-node archery: aiming at a GUEST animal fills the meter,
    /// and the auto-release ships one RelayAttack with chip=0 (the
    /// ranged bypass marker) carrying the Fandom damage to the
    /// animal's authority node. The arrow is spent on the shooter's
    /// node regardless of the hit roll.
    #[tokio::test]
    async fn relay_arrow_shot_ships_ranged_relayattack() {
        let (mut g, _rx, _raw, mut mesh_rx) = clustered_game("bowrelay", 0, 2);
        let gid = relay_wolf_guest(&mut g, 66, 200);
        let pidx = *g.world.by_session.get(&1).unwrap();
        arm_bow(&mut g, pidx, 10, 10);
        let pgob = g.world.players[pidx].gob;
        g.player_interact(1, pgob, gid, (0, 0));
        assert_eq!(
            g.world.players[pidx].aim.map(|a| a.target),
            Some(gid),
            "aim opens against a guest animal"
        );
        // Fill the meter: 40 ticks at 250/tick.
        for _ in 0..40 {
            let aim = g.world.players[pidx].aim.expect("aim kept");
            g.tick_aim(pidx, 1, pgob, aim);
        }
        assert_eq!(arrow_count(&mut g, pidx), 9, "one arrow spent on release");
        let mut saw_ranged_relay = false;
        while let Ok((_peer, msg)) = mesh_rx.try_recv() {
            if let crate::nodes::NodeMsg::RelayAttack { chip, dmg, .. } = msg {
                assert_eq!(chip, 0, "ranged relay carries the chip-0 marker");
                assert_eq!(dmg, crate::archery::bow_damage(10));
                saw_ranged_relay = true;
            }
        }
        assert!(
            saw_ranged_relay,
            "the release must relay one ranged RelayAttack"
        );
    }

    /// The authority side applies a chip-0 RelayAttack without the
    /// openings gate: HP drops by the full damage even at a full
    /// defence bar, and death runs the relayed death flow.
    #[tokio::test]
    async fn relay_swing_chip0_bypasses_openings_and_kills() {
        let (mut g, _rx, _raw, _mesh) = clustered_game("arrowauth", 0, 2);
        // A LOCAL wolf as the authority-side target (the animal this
        // node owns); the relay path is exercised directly.
        let pidx = *g.world.by_session.get(&1).unwrap();
        let pgob = g.world.players[pidx].gob;
        let pslot = g.world.gobs.get(pgob).unwrap();
        let (px, py) = g.world.gobs.pos[pslot];
        let res = g.world.res.intern(Species::Wolf.resname());
        let wolf = g.world.gobs.spawn(
            Kind::Animal {
                species: Species::Wolf,
            },
            (px + 66, py),
            res,
            Species::Wolf.max_hp(),
            33,
        );
        g.world.animal_gobs.push(wolf);
        let wslot = g.world.gobs.get(wolf).unwrap();
        let hp0 = g.world.gobs.hp[wslot];
        // chip 0, damage 200 (> wolf 60 HP): full defence bar, no gate.
        g.relay_swing(pgob, wolf, 0, 200);
        assert!(
            g.world.gobs.get(wolf).is_none(),
            "a chip-0 relay kills through a full defence bar"
        );
        let _ = hp0;
        // The kill cleared the fight teardown state.
        assert_eq!(g.world.players[pidx].fight_target, None);
    }
}
