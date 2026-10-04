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
}

pub struct Game {
    pub world: World,
    pub sessions: HashMap<SessionId, SessionOut>,
    pub rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    pub net_rx: tokio::sync::mpsc::UnboundedReceiver<crate::net::NetCmd>,
    pub saturated: bool,
    next_sid: SessionId,
    /// Grids already populated with objects/animals.
    populated: HashSet<(i32, i32)>,
    /// Character persistence store (loaded snapshots + live updates).
    pub save: crate::persist::SaveStore,
    /// Parsed etc/needed/fep.conf (food -> FEP vector).
    pub fep: crate::craft::FepTable,
    /// Number of parallel grid-owner workers used by the tick (data-parallel
    /// intent computation over SoA columns; apply stays on the game task).
    pub workers: usize,
    /// Milliseconds of online time per granted LP (skills.rs accrual;
    /// precomputed once from HNH_LP_RATE, u64::MAX = disabled).
    lp_ms_per_lp: u64,
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
            populated: HashSet::new(),
            save,
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
        }
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
                        Some(crate::net::NetCmd::Accept { game_tx, raw_tx, reply }) => {
                            let sid = self.alloc_sid();
                            self.session_connected(sid, game_tx, raw_tx);
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
            vis_gob_scans = self.world.perf.vis_gob_scans,
            vis_skipped = self.world.perf.vis_skipped,
            vis_cells = self.world.perf.vis_cells,
            "perf"
        );
    }

    // ------------------------------------------------------------------
    // Session lifecycle
    // ------------------------------------------------------------------

    /// Register a newly accepted session; shows the character list.
    /// `tx` is the sink the game task writes outgoing RMSG payloads into;
    /// the session task owns the receiver.
    pub fn session_connected(
        &mut self,
        sid: SessionId,
        tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        raw_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    ) {
        let mut out = SessionOut {
            sid,
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
            grids_seen: HashSet::new(),
            vis_cell: None,
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
        // and the world character match.
        let mut layer_ids = Vec::with_capacity(Self::player_layer_names().len());
        for name in Self::player_layer_names() {
            let global = self.world.res.intern(name);
            let w = out.res.wire_named(global, name);
            if let Some((n, v)) = out.res.pending_announce(w) {
                out.send(wdg::resid(w, n, v));
                out.res.mark_announced(w);
            }
            layer_ids.push(w);
        }
        info!(layers = ?Self::player_layer_names(), "charlist portrait layers announced");
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
        match (wtag.as_deref(), name) {
            (Some("charlist"), "play") => {
                let chosen = args
                    .first()
                    .and_then(|a| a.as_str())
                    .unwrap_or("Player")
                    .to_owned();
                self.enter_world(sid, chosen);
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
        // Restore the persisted character when one exists for this name;
        // the saved world position overrides the fresh-spawn search.
        let saved_state = self.save.players.get(&name).map(|saved| {
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
            gob,
            session: sid,
            hp,
            energy,
            stamina,
            lp,
            lp_carry_ms: 0,
            skills: restored_skills,
            attrs,
            inv,
            equip: restored_equip,
            fep: crate::craft::FepState::default(),
            fight_target: None,
            atk_cd: 0,
        });
        self.world.by_session.insert(sid, player_idx);

        // Starter kit for fresh characters (server policy; legacy gave
        // nothing but the dev flow needs craftable ingredients on hand).
        // Labels on food keep the fep.conf identity for the eat flow.
        if self.world.players[player_idx].inv.is_empty() {
            let kit: &[(&str, u32, u8, &'static str)] = &[
                ("gfx/invobjs/branch", 2, 10, ""),
                ("gfx/invobjs/stone", 2, 10, ""),
                ("gfx/invobjs/meat", 1, 10, "Beef"),
                // Farming starter seeds: the plow pagina is pushed to
                // every session, so the full plant-grow-harvest loop is
                // playable out of the box.
                ("gfx/invobjs/seed-wheat", 5, 10, "Wheat Seeds"),
                ("gfx/invobjs/seed-carrot", 5, 10, "Carrot Seeds"),
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
            &[ListVal::I(2), ListVal::I(4)],
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
        // Populate the grid the first time anyone looks at it.
        let mut spawned = Vec::new();
        let first_touch = !self.populated.contains(&gc);
        if first_touch {
            self.populated.insert(gc);
            self.world.populate_grid(gc, &mut spawned);
            let animals = if self.saturated { 40 } else { 4 };
            self.world.populate_animals(gc, animals, &mut spawned);
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
        // Players must NOT be announced via OD_RES: the avatar base resource
        // (gfx/borka/body) carries no neg layer, so ResDrawable's eager
        // ImageSprite creation throws "No negative found" inside the
        // client's session reader thread and kills it - the observed black
        // screen after entering the world. Players render through
        // OD_LAYERS (Layered drawable) only.
        let is_player = matches!(kind, Kind::Player { .. });
        if include_res && !is_player {
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
        // Player avatar layers.
        if let Kind::Player { player } = kind {
            if let Some(p) = self.world.players.get(player) {
                // The fork client has no plalay/plparts router support: the
                // "gfx/borka/{body,head,hair}" pose routers carry a custom
                // layer type the client drops, so any layer resolved from
                // them has no neg and ImageSprite dies with "No negative
                // found" on the first render tick (real-client freeze).
                // The legacy official server resolved poses SERVER-SIDE and
                // layered concrete image-bearing frame resources instead
                // (this fork's own JSBot checks layer names like
                // "gfx/borka/body/sitting/"), so do exactly that: base is
                // the body router (a load gate client-side, never
                // sprite-created) and the layers are standing-pose frames.
                let base = wire_res; // base = gfx/borka/body router
                m.uint8(OD_LAYERS).uint16(base);
                for part in Self::player_layer_names() {
                    let gi = self.world.res.intern(part);
                    let w = out.res.wire_named(gi, part);
                    m.uint16(w);
                }
                m.uint16(65535);
                m.uint8(OD_BUDDY).string(&p.name).uint8(0).uint8(0);
            }
        }
        // Health tint.
        let quarters = ((hp * 4) / max_hp.max(1)).clamp(0, 4) as u8;
        m.uint8(OD_HEALTH).uint8(quarters);
        m.uint8(OD_END);
        Some(m.finish())
    }

    /// Standing-pose frame resources that compose a player avatar. The fork
    /// client has no plalay/plparts router support (both layer types are
    /// dropped on load, leaving factories without a neg), so - like the
    /// legacy official server - the avatar must be layered from concrete
    /// image-bearing frame resources. Also used verbatim as the charlist
    /// portrait layers, so the login card and the world avatar match.
    fn player_layer_names() -> &'static [&'static str] {
        &[
            "gfx/borka/body/standing/legs-0",
            "gfx/borka/body/standing/torso/male-0",
            "gfx/borka/body/standing/head-0",
            "gfx/borka/body/standing/arm/idle/left-0",
            "gfx/borka/body/standing/arm/idle/right-0",
            "gfx/borka/hair-karin/standing/hair-0",
        ]
    }

    /// Stream a spawn (full state) for one gob to one session, announcing
    /// its resource id first if the session has not seen it.
    fn stream_spawn(&mut self, sid: SessionId, id: GobId) {
        let Some(slot) = self.world.gobs.get(id) else {
            return;
        };
        let res_idx = self.world.gobs.res_idx[slot];
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
        // Player avatar layers: announce base + every concrete frame
        // resource the OD_LAYERS block references before the spawn block.
        // The client resolves OD_LAYERS ids through these RESIDs; without
        // them the avatar renders invisible ("no doll").
        if matches!(self.world.gobs.kind[slot], Kind::Player { .. }) {
            for layer_name in
                std::iter::once("gfx/borka/body").chain(Self::player_layer_names().iter().copied())
            {
                let gi = self.world.res.intern(layer_name);
                let w = out.res.wire_named(gi, layer_name);
                if let Some((name, ver)) = out.res.pending_announce(w) {
                    out.send(wdg::resid(w, name, ver));
                    out.res.mark_announced(w);
                }
            }
        }
        // Encode and register the spawn block (separate borrow scope).
        if let Some(block) = self.encode_gob_block(sid, id, true) {
            let frame = self.world.gobs.frame[slot];
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.send_raw(block.clone());
                out.unacked.entry(id).or_default().insert(frame, block);
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
        let mut updates: Vec<(SessionId, Vec<GobId>)> = Vec::new();
        let session_ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        // --- Phase A: skip decision + candidate positions. A session
        // whose own cell did not change and whose retract square (the
        // 2xVIEW_RADIUS bound the retract sweep enforces, expanded by one
        // cell) intersects no dirty cell cannot have anything new to
        // spawn, move, or retract: the whole scan is skipped.
        let candidates: Vec<(SessionId, (i32, i32))> = session_ids
            .iter()
            .filter_map(|sid| {
                let player_gob = self.sessions[sid].player_gob?;
                let pslot = self.world.gobs.get(player_gob)?;
                Some((*sid, self.world.gobs.pos[pslot]))
            })
            .collect();
        let mut to_scan: Vec<(SessionId, (i32, i32))> = Vec::new();
        for (sid, (px, py)) in candidates {
            let cell = crate::visidx::cell_of(px, py);
            let moved = self.sessions[&sid].vis_cell != Some(cell);
            if !moved
                && !self
                    .world
                    .gobs
                    .vis
                    .any_dirty_in_view(px, py, VIEW_RADIUS * 2)
            {
                self.world.perf.vis_skipped += 1;
                continue;
            }
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.vis_cell = Some(cell);
            }
            to_scan.push((sid, (px, py)));
        }
        // --- Phase A2: cell-bucketed candidate scan (parallel when
        // multiple sessions are present). The cell query replaces the
        // O(all gobs) sweep; the exact distance filter is unchanged. ---
        let in_range: Vec<Vec<GobId>> = if self.workers > 1 && to_scan.len() > 8 {
            to_scan
                .par_iter()
                .map(|(_sid, (px, py))| self.scan_visible(*px, *py))
                .collect()
        } else {
            to_scan
                .iter()
                .map(|(_sid, (px, py))| self.scan_visible(*px, *py))
                .collect()
        };
        self.world.perf.vis_gob_scans += in_range.iter().map(|v| v.len() as u64).sum::<u64>();
        self.world.perf.vis_cells = self.world.gobs.vis.cell_count();
        // --- Phase B: serial application per session. ---
        for ((sid, (px, py)), cand) in to_scan.into_iter().zip(in_range) {
            let mut moving: Vec<GobId> = Vec::new();
            for id in cand {
                let Some(slot) = self.world.gobs.get(id) else {
                    continue;
                };
                let mv = self.world.gobs.mv[slot];
                let frame = self.world.gobs.frame[slot];
                let is_new;
                let needs_move;
                {
                    let out = self.sessions.get_mut(&sid).expect("BUG: sid from cand");
                    // Check-only here: stream_spawn performs the insert and
                    // skips already-present ids; inserting before calling it
                    // would suppress the spawn block entirely (the avatar
                    // bug: the client never received its own gob).
                    is_new = !out.visible.contains(&id);
                    needs_move = !is_new
                        && mv.is_some()
                        && out
                            .unacked
                            .get(&id)
                            .map(|m| !m.contains_key(&frame))
                            .unwrap_or(true);
                }
                if is_new {
                    self.stream_spawn(sid, id);
                } else if needs_move {
                    moving.push(id);
                }
            }
            if !moving.is_empty() {
                updates.push((sid, moving));
            }
            // Retractions: visible set minus in-range is too expensive to
            // scan fully every tick; do a cheap sweep only over the visible
            // set (bounded by ~few hundred gobs per session).
            let to_retract: Vec<GobId> = {
                let out = self.sessions.get_mut(&sid).expect("BUG: sid from keys");
                out.visible
                    .iter()
                    .filter(|&&id| {
                        self.world
                            .gobs
                            .get(id)
                            .map(|slot| {
                                let (gx, gy) = self.world.gobs.pos[slot];
                                (gx - px).abs() > VIEW_RADIUS * 2
                                    || (gy - py).abs() > VIEW_RADIUS * 2
                            })
                            .unwrap_or(true) // dead gobs get retracted too
                    })
                    .copied()
                    .collect()
            };
            for id in to_retract {
                self.stream_retract(sid, id);
            }
            self.world.perf.visible_total += self.sessions[&sid].visible.len();
        }
        // Movement deltas: LINSTEP progress frames.
        for (sid, gobs) in updates {
            for id in gobs {
                let Some(slot) = self.world.gobs.get(id) else {
                    continue;
                };
                let Some(lm) = self.world.gobs.mv[slot] else {
                    continue;
                };
                let frame = self.world.gobs.frame[slot];
                let Some(out) = self.sessions.get_mut(&sid) else {
                    continue;
                };
                let mut m = MessageBuf::new();
                m.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(id)
                    .int32(frame as i32)
                    .uint8(OD_LINSTEP)
                    .int32(lm.step)
                    .uint8(OD_END);
                let block = m.finish();
                out.send_raw(block.clone());
                out.unacked.entry(id).or_default().insert(frame, block);
            }
        }
    }

    /// Pure in-range gob scan around a point (no mutation; rayon-friendly).
    /// In-range gob scan around a point: query the dirty-cell index for
    /// the view cells, then apply the exact distance filter (cells are
    /// coarse buckets; the filter preserves the old O(all gobs) result).
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
    // Player commands
    // ------------------------------------------------------------------

    fn on_map_click(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
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
        let Some(slot) = self.world.gobs.get(player_gob) else {
            return;
        };
        let (sx, sy) = self.world.gobs.pos[slot];
        // Clamp path length and validate walkability server-side.
        let dx = (target.0 - sx).clamp(-5000, 5000);
        let dy = (target.1 - sy).clamp(-5000, 5000);
        let (tx, ty) = (sx + dx, sy + dy);
        if !path_clear(&mut self.world, sx, sy, tx, ty) {
            return;
        }
        let dist = (tx - sx).abs() + (ty - sy).abs();
        let speed = self.world.gobs.speed[slot].max(1);
        let ms = (dist * 1000) / speed; // milliseconds at speed subtile/s
        let steps = (ms / (TICK_MS as i32)).clamp(1, 1000);
        self.world.gobs.mv[slot] = Some(LinMove {
            sx,
            sy,
            tx,
            ty,
            steps,
            step: 0,
        });
        self.world.gobs.frame[slot] += 1;
        self.world.gobs.set_pos(slot, (tx, ty)); // logical position = destination
        let frame = self.world.gobs.frame[slot];
        // Send LINBEG to all sessions that see this gob.
        let viewers: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&player_gob))
            .map(|(s, _)| *s)
            .collect();
        for v in viewers {
            if let Some(out) = self.sessions.get_mut(&v) {
                let mut m = MessageBuf::new();
                m.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(player_gob)
                    .int32(frame as i32)
                    .uint8(OD_LINBEG)
                    .coord(sx, sy)
                    .coord(tx, ty)
                    .int32(steps)
                    .uint8(OD_END);
                let block = m.finish();
                out.send_raw(block.clone());
                out.unacked
                    .entry(player_gob)
                    .or_default()
                    .insert(frame, block);
            }
        }
        trace!(sid, sx, sy, tx, ty, steps, "walk");
    }

    fn player_interact(
        &mut self,
        sid: SessionId,
        _player_gob: GobId,
        target: GobId,
        _at: (i32, i32),
    ) {
        let Some(tslot) = self.world.gobs.get(target) else {
            return;
        };
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
                // Pick up: move into inventory.
                let res_idx = self.world.gobs.res_idx[tslot];
                if let Some(drop) = self.world.gobs.kind[tslot].drop_info() {
                    if let Some(p) = self.world.player_mut(sid) {
                        p.inv.push(InvStack {
                            res: res_idx,
                            count: drop.1 as u32,
                            ql: drop.2,
                            label: drop.3,
                        });
                    }
                }
                self.world.gobs.kill(target);
                self.broadcast_retract(target);
                self.refresh_inventory(sid);
            }
            Kind::Animal { species } => {
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
                // Select that opponent; answer with `cur`.
                if let Some(&gob) = ints.first() {
                    if let Some(p) = self.world.player_mut(sid) {
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

    fn spawn_drop_near(
        &mut self,
        at: (i32, i32),
        resname: &'static str,
        ql: u8,
        label: &'static str,
    ) {
        let res_idx = self.world.res.intern(resname);
        let jitter = |w: &mut World| (w.next_ai_rand(7) - 3) * 11;
        let jx = jitter(&mut self.world);
        let jy = jitter(&mut self.world);
        let id = self.world.gobs.spawn(
            Kind::Drop {
                resname_idx: res_idx,
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
    }

    fn broadcast_retract(&mut self, id: GobId) {
        let sids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for sid in sids {
            self.stream_retract(sid, id);
        }
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
            out.send(wdg::new_wdg(w, "inv", 350, 250, 0, &[]));
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

    /// Rebuild inventory items: destroy old item widgets, create new ones.
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
        if let Some(p) = self.world.player_mut(sid) {
            p.inv.push(stack);
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
                    if !s.label.is_empty() {
                        args.push(ListVal::S(s.label.to_owned()));
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
        self.world.players[pidx].equip[ep as usize] = Some(stack);
        self.send_epry_state(sid);
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
                return;
            }
            if self.world.stations.contains_key(&gob) {
                self.station_itemact(sid, gob, cursor);
                return;
            }
            // Fall through to the map-space behaviors below for other
            // gob kinds (legacy iteminteract semantics).
        }
        match farm::spec_by_seed_label(label) {
            Some(spec) => self.plant_seed(sid, spec, Self::tile_coord(mx, my), cursor),
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
            }
        }
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
        if let Some(p) = self.world.player_mut(sid) {
            p.stamina = (p.stamina - 10).max(0);
        }
        self.world.grids.mutate_tile(gc, lx, ly, tile::PLOWED);
        let now = unix_ms();
        self.world
            .tilth
            .insert((tx, ty), now + crate::farm::tilth_decay_ms());
        // Re-send the mutated grid to every client holding it.
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
        info!(sid, tx, ty, "tile plowed");
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
        let Kind::Crop { stage, .. } = self.world.gobs.kind[slot] else {
            return;
        };
        let Some(state) = self.world.crops.get(&target) else {
            return;
        };
        let spec = &farm::CROPS[state.spec as usize];
        let option = if stage >= spec.stages {
            "Harvest"
        } else if stage >= spec.early_stage {
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
            if let Some(p) = self.world.player_mut(sid) {
                p.inv.push(InvStack {
                    res: res_idx,
                    count: n,
                    ql: q,
                    label: y.label,
                });
            }
        }
        self.refresh_inventory(sid);
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
                    out.unacked.entry(gob).or_default().insert(frame, block);
                }
            }
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
        for tile in expired {
            self.world.tilth.remove(&tile);
            debug!(tx = tile.0, ty = tile.1, "tilth decayed");
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
                out.unacked.entry(gob).or_default().insert(frame, block);
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
        info!(sid, gob, label = cursor.label, "station input loaded");
    }

    /// Click on a station gob: open the Light/Extinguish flower menu.
    fn open_station_menu(&mut self, sid: SessionId, target: GobId) {
        let Some(station) = self.world.stations.get(&target).cloned() else {
            return;
        };
        let buildable = &crate::build::BUILDABLES[station.spec as usize];
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
        let option = if station.lit { "Extinguish" } else { "Light" };
        out.send(wdg::new_wdg(
            w,
            "sm",
            -1,
            -1,
            0,
            &[ListVal::S(option.to_owned())],
        ));
        out.station_menu = Some((w, target));
        let _ = buildable;
    }

    /// Flower menu choice on a station: Light starts a job (fuel +
    /// input required), Extinguish cancels the lit state.
    fn apply_station_choice(&mut self, sid: SessionId, wid: u16, choice: i32) {
        let pending = self
            .sessions
            .get(&sid)
            .and_then(|o| o.station_menu)
            .filter(|(w, _)| *w == wid);
        let Some((_, gob)) = pending else {
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
        let mut consumed: Vec<(u8, u32)> = Vec::new(); // (ql, units)
        for (resname, need) in recipe.inputs {
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
                remaining -= take;
            }
        }
        self.world.players[pidx].inv.retain(|s| s.count > 0);
        // Weighted-average output quality (loftar: sum(q*w)/sum(w)).
        let total_w: u32 = consumed.iter().map(|(_, w)| w).sum();
        let qsum: u32 = consumed.iter().map(|(q, w)| *q as u32 * w).sum();
        let mut q = (qsum.checked_div(total_w).unwrap_or(10) as i32).max(1);
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
            self.world.players[pidx].inv.push(InvStack {
                res: gidx,
                count: *count,
                ql: out_q,
                label: "",
            });
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
        self.world.players[pidx].inv.push(InvStack {
            res: self.world.res.intern("gfx/invobjs/meat"),
            count: 1,
            ql: out_ql,
            label: roasted,
        });
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
            .map(|(w, _)| w);
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
        // Per-phase attribution keeps the data-oriented hot loops honest:
        // regressions show up in the phase histogram, not just the total.
        let mut phase_us = [0u128; 5];
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
        self.tick_farming();
        // Production stations: bounded by the live station set.
        self.tick_stations();
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
        for slot in 0..self.world.gobs.alive.len() {
            if !self.world.gobs.alive[slot] {
                continue;
            }
            if let Some(lm) = self.world.gobs.mv[slot] {
                // Active mover: keep its cell dirty so viewers receive
                // LINSTEP progress and boundary exits are caught.
                self.world
                    .gobs
                    .vis
                    .mark_mover(gob_id_from_slot(slot, self.world.gobs.gen[slot]));
                if lm.step < lm.steps {
                    let new_step = (lm.step + 1).min(lm.steps);
                    self.world.gobs.mv[slot] = Some(LinMove {
                        step: new_step,
                        ..lm
                    });
                    if new_step >= lm.steps {
                        // Move finished: position already set at destination;
                        // clear movement and bump the frame so clients that
                        // acked the move get a final static position.
                        self.world.gobs.mv[slot] = None;
                        self.world.gobs.frame[slot] += 1;
                        self.world.gobs.set_pos(slot, (lm.tx, lm.ty));
                    }
                }
            }
        }
    }

    /// Animal AI: parallel intent pass over the SoA columns (read-only),
    /// then serial application (writes stay on the game task). Intents are
    /// computed per grid-region bucket so the same pure function maps to
    /// true cross-process grid owners later.
    fn tick_animals(&mut self) {
        let tick = self.world.tick;
        let animal_ids: Vec<GobId> = self.world.animal_gobs.clone();
        // Phase A (parallel): pure intent computation over immutable SoA
        // state. Randomness derives from (tick, slot) hashes so the pass is
        // deterministic and race-free without a shared RNG.
        let workers = self.workers.max(1);
        let decisions: Vec<(GobId, AnimalAction)> = if workers > 1 && animal_ids.len() > 64 {
            // Chunk ids into worker-sized buckets; rayon runs the pure
            // decision function per bucket.
            let bucket = animal_ids.len().div_ceil(workers);
            animal_ids
                .par_chunks(bucket)
                .map(|chunk| {
                    chunk
                        .iter()
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
        let speed = self.world.gobs.speed[slot].max(1);
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
        if !path_clear(&mut self.world, sx, sy, tx, ty) {
            return;
        }
        let dist = (tx - sx).abs() + (ty - sy).abs();
        let ms = (dist * 1000) / speed;
        let steps = (ms / (TICK_MS as i32)).clamp(1, 600);
        self.world.gobs.mv[slot] = Some(LinMove {
            sx,
            sy,
            tx,
            ty,
            steps,
            step: 0,
        });
        self.world.gobs.frame[slot] += 1;
        self.world.gobs.set_pos(slot, (tx, ty));
        let frame = self.world.gobs.frame[slot];
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
                out.unacked.entry(id).or_default().insert(frame, block);
            }
        }
    }

    fn tick_combat(&mut self) {
        const REACH: i32 = 33; // ~3 tiles
        const DISENGAGE: i32 = 300;
        let tick = self.world.tick;

        // --- player side: offence gen, swings, bar streaming ---
        let players: Vec<usize> = (0..self.world.players.len()).collect();
        for pidx in players {
            let (target, sid, pgob) = {
                let p = &self.world.players[pidx];
                (p.fight_target, p.session, p.gob)
            };
            let Some(target) = target else { continue };
            let Some(pslot) = self.world.gobs.get(pgob) else {
                continue;
            };
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
                    let dist = (tx - px).abs() + (ty - py).abs();
                    let speed = self.world.gobs.speed[pslot].max(1);
                    let ms = (dist * 1000) / speed;
                    let steps = (ms / (TICK_MS as i32)).clamp(1, 600);
                    let (sx, sy) = (px, py);
                    self.world.gobs.mv[pslot] = Some(LinMove {
                        sx,
                        sy,
                        tx,
                        ty,
                        steps,
                        step: 0,
                    });
                    self.world.gobs.frame[pslot] += 1;
                    self.world.gobs.set_pos(pslot, (tx, ty));
                    let frame = self.world.gobs.frame[pslot];
                    let viewers: Vec<SessionId> = self
                        .sessions
                        .iter()
                        .filter(|(_, o)| o.visible.contains(&pgob))
                        .map(|(s, _)| *s)
                        .collect();
                    for v in viewers {
                        if let Some(out) = self.sessions.get_mut(&v) {
                            let mut m = MessageBuf::new();
                            m.uint8(MSG_OBJDATA)
                                .uint8(0)
                                .int32(pgob)
                                .int32(frame as i32)
                                .uint8(OD_LINBEG)
                                .coord(sx, sy)
                                .coord(tx, ty)
                                .int32(steps)
                                .uint8(OD_END);
                            let b = m.finish();
                            out.send_raw(b.clone());
                            out.unacked.entry(pgob).or_default().insert(frame, b);
                        }
                    }
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
                    let new_def = (own_def - crate::fight::SWING_DEF_DMG).max(0);
                    if new_def <= crate::fight::OPENING_THRESHOLD {
                        bite = Some(dmg);
                    }
                }
            }
            if let Some(dmg) = bite {
                self.hurt_player(pidx, dmg, id);
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

    /// Apply animal damage to a player (health quarters stream too).
    fn hurt_player(&mut self, pidx: usize, dmg: i32, from: GobId) {
        let sid = self.world.players[pidx].session;
        let pgob = self.world.players[pidx].gob;
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
        if let Some(out) = self.sessions.remove(&sid) {
            // A stack left on the cursor goes back to the inventory so a
            // log-out mid-plant does not eat the item.
            if let Some(stack) = out.cursor {
                if let Some(p) = self
                    .world
                    .by_session
                    .get(&sid)
                    .copied()
                    .and_then(|idx| self.world.players.get_mut(idx))
                {
                    p.inv.push(stack);
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

impl Kind {
    /// Extract (resname_idx, count, ql, display label) from a Drop kind.
    pub fn drop_info(&self) -> Option<(u16, u8, u8, &'static str)> {
        match self {
            Kind::Drop {
                resname_idx,
                ql,
                label,
            } => Some((*resname_idx, 1, *ql, label)),
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
        tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
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
        let (raw_tx, raw_rx) = tokio::sync::mpsc::unbounded_channel();
        g.session_connected(1, tx, raw_tx);
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
        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::unbounded_channel();
        g.session_connected(1, tx, raw_tx);
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
                    OD_LAYERS => {
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
        let (raw_tx, _raw_rx) = tokio::sync::mpsc::unbounded_channel();
        g.session_connected(1, tx, raw_tx);
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
        let (raw_tx, _raw_rx) = tokio::sync::mpsc::unbounded_channel();
        g.session_connected(1, tx, raw_tx);
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
        let (raw_tx, _raw_rx) = tokio::sync::mpsc::unbounded_channel();
        g.session_connected(1, tx, raw_tx);
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
        let (raw_tx, _raw_rx) = tokio::sync::mpsc::unbounded_channel();
        g.session_connected(1, tx, raw_tx);
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
}
