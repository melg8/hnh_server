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
    /// Monotonic id for MSG_MAPDATA fragment groups. NEVER reuse the
    /// tick here: several MAPREQs land in the SAME tick (the 3x3
    /// bootstrap), and clients reassemble fragments by pktid, so a
    /// shared pktid mixes the grids' fragments into garbage streams.
    mapdata_seq: u32,
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
    /// Retransmit sweep round-robin scratch (session 68 budgeted sweep):
    /// session-id ring reused per sweep (taken/restored; was an implicit
    /// fixed HashMap iteration order that let the budget starve the tail).
    retx_scratch: Vec<SessionId>,
    /// Ring start for the NEXT sweep: advances by the number of sessions
    /// seen, so budget-starved sessions go first on the next pass.
    retx_cursor: usize,
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
                // Aux slot (session 66): only the crucible's tin bar
                // rides here; restore it only when the label still is
                // the alloy tin charge (the same leak-per-entry policy).
                let aux = saved.aux.as_ref().and_then(|(_res, ql, label)| {
                    let leaked: &'static str = Box::leak(label.clone().into_boxed_str());
                    crate::craft::ALLOY_INPUT_TIN
                        .eq_ignore_ascii_case(label)
                        .then_some((world.res.intern("gfx/invobjs/bar-tin"), *ql, leaked))
                });
                world.stations.insert(
                    gob,
                    crate::build::StationState {
                        spec: saved.spec,
                        fuel: saved.fuel,
                        fuel_ql_sum: saved.fuel_ql_sum,
                        fuel_seen: saved.fuel_seen,
                        input,
                        aux,
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
            mapdata_seq: 0,
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
            retx_scratch: Vec::new(),
            retx_cursor: 0,
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
                        // wmax attribution: freeze this tick's phase and
                        // sub-phase counters so the report names the
                        // spike's owner, not the steady state.
                        let p = &mut self.world.perf;
                        p.wmax_phase_us = p.phase_us;
                        p.wmax_mvbat_scan_us = p.mvbat_scan_us;
                        p.wmax_mvbat_encode_us = p.mvbat_encode_us;
                        p.wmax_mvbat_fanout_us = p.mvbat_fanout_us;
                        p.wmax_retx_sweep_us = p.retx_sweep_us;
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
        self.world.perf.fanout_pairs = 0;
        self.world.perf.fanout_hits = 0;
        self.world.perf.fanout_fin = 0;
        self.world.perf.fanout_msgs = 0;
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
    pub(super) fn tile_at(&mut self, (x, y): (i32, i32)) -> Option<u8> {
        let gc = (x.div_euclid(1100), y.div_euclid(1100));
        let ix = (x.div_euclid(11)).rem_euclid(100) as usize;
        let iy = (y.div_euclid(11)).rem_euclid(100) as usize;
        Some(self.world.grids.grid(gc).tile(ix, iy))
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
/// World-shape aliases for inventory items the 2009 pack renders only in
/// a sibling metal's shape: the pack ships no tin / cast-iron / bronze
/// world sprites, but the nugget and bar families are visually interchangeable
/// (same size and silhouette across metals). Checked after the item's own
/// shape and before the branch fallback.
const DROP_WORLD_ALIASES: &[(&str, &str)] = &[
    ("nugget-tin", "gfx/terobjs/items/nugget-copper"),
    ("bar-tin", "gfx/terobjs/items/bar-copper"),
    ("bar-castiron", "gfx/terobjs/items/bar-iron"),
    ("nugget-castiron", "gfx/terobjs/items/nugget-iron"),
    // The 2009 pack ships no bronze bar sprite either; bronze is a
    // copper alloy and renders through the copper silhouette (session
    // 67: before this alias the crucible's output drops fell back to
    // the branch shape, making the wire probe's drop scan impossible).
    ("bar-bronze", "gfx/terobjs/items/bar-copper"),
    // Session 71 (baking chain): the pack ships flour and grist
    // inventory icons but no world shapes for them; both render through
    // the seed-bag silhouette - the closest held-item shape to a bag of
    // milled grain (same fallback policy as bronze->copper above).
    ("grist-wheat", "gfx/terobjs/items/bag-seed"),
    ("flour", "gfx/terobjs/items/bag-seed"),
];

/// Crate-visible lookup for tests and callers that need to know whether
/// an inventory item resolves its world shape through the alias table
/// (craft.rs tests pin the smelter outputs to own-shape-or-alias).
pub(crate) fn drop_world_alias(base: &str) -> Option<&'static str> {
    DROP_WORLD_ALIASES
        .iter()
        .find(|(b, _)| *b == base)
        .map(|(_, world)| *world)
}

fn drop_world_res(inv_res_name: &str) -> &'static str {
    let base = inv_res_name.rsplit('/').next().unwrap_or(inv_res_name);
    let ter = format!("gfx/terobjs/items/{base}");
    if crate::resources::served(&ter) {
        leak_static(&ter)
    } else if let Some(world) = drop_world_alias(base) {
        world
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

// The test battery lives in its own module file (game/tests.rs) with
// per-theme children in game/tests/ - a child module keeps every
// private item of `game` visible to the tests without pub-super
// annotations (proj-lib-main-split: testable logic; the S76 split
// arranged the 8.4k-line flat battery into per-feature files).
#[cfg(test)]
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
mod entry;
mod farming;
mod interact;
mod items;
mod lifecycle;
mod pose;
mod relay;
mod social;
mod stream;
// Keep `game::move_dir` / `game::art_dir` (doc references in state.rs
// and nodes.rs) resolving after the pose-table move (proj-pub-use-reexport).
// The pub(super) row re-exposes the table accessors and the avatar base
// resource to the sibling stream/interact/cluster files through their
// `use super::*` globs (a glob only pulls items declared in `game` itself).
pub use pose::{art_dir, move_dir};
pub(super) use pose::{
    avatar_doll_layers, avatar_pose_layers, kritter_base, kritter_pose_layer, AVATAR_BASE,
};
