//! Save + command-loop plumbing: the 30 s autosave and the shutdown
//! flush (save v7 snapshots), the game-task command dispatch
//! (handle_cmd), the 5 s perf report, the per-player save snapshot
//! and the session teardown (cursor rollback, party leave,
//! by_session reindexing).
//!
//! Pure move out of game.rs (session 70 split).

use super::*;

impl Game {
    /// Snapshot every online player + the world state, then write the
    /// save file on a BLOCKING THREAD (the serialize+write bulk measured
    /// 95 ms per save at 1k players / 519 ms at 10k - the 30 s autosave
    /// must not stall the 100 ms tick budget with it). The live-cadence
    /// entry point.
    pub(super) fn autosave(&mut self) {
        self.save_all();
        self.save.flush_background(self.world.seed);
    }

    /// Snapshot + write SYNCHRONOUSLY. The shutdown-path entry point
    /// (the run loop's final flush): the process is going away, so the
    /// write must complete before it does.
    pub(super) fn save_all_and_flush(&mut self) {
        self.save_all();
        if let Err(e) = self.save.flush(self.world.seed) {
            tracing::warn!(error = %e, "save flush failed");
        }
    }

    /// Snapshot every online player + the world state into the store
    /// WITHOUT writing (see `autosave` for the writing halves).
    pub(super) fn save_all(&mut self) {
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
                aux: station.aux.map(|(r, q, l)| {
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
                aux: None,
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
    }

    pub(super) fn handle_cmd(&mut self, cmd: Cmd) {
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

    pub(super) fn report_perf(&mut self) {
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
            fanout_pairs = self.world.perf.fanout_pairs,
            fanout_hits = self.world.perf.fanout_hits,
            fanout_fin = self.world.perf.fanout_fin,
            fanout_msgs = self.world.perf.fanout_msgs,
            start_blocks = self.world.perf.start_blocks,
            fx_batch_n = self.world.perf.fx_batch_n,
            retx_sweep_us = self.world.perf.retx_sweep_us,
            retx_pending = self.world.perf.retx_pending,
            retx_resent = self.world.perf.retx_resent,
            retx_queue_full = self.world.perf.retx_queue_full,
            retx_busy_sessions = self.world.perf.retx_busy_sessions,
            tail_us = self.world.perf.tail_us,
            startbat_fanout_us = self.world.perf.startbat_fanout_us,
            retx_retire_us = self.world.perf.retx_retire_us,
            retx_retired_gobs = self.world.perf.retx_retired_gobs,
            sweeps_us = self.world.perf.sweeps_us,
            grid_gens = self.world.grids.gen_count,
            grid_hits = self.world.grids.hit_count,
            wmax_mvbat_fanout_us = self.world.perf.wmax_mvbat_fanout_us,
            wmax_retx_sweep_us = self.world.perf.wmax_retx_sweep_us,
            "perf"
        );
        // The window maximum has been delivered to this window's report;
        // the next 5 s window measures from zero.
        self.world.perf.window_max_tick_us = 0;
    }

    /// Snapshot a player into the save store (position from the gob slot,
    /// inventory translated from process-local indices to resource names).
    pub(super) fn persist_player(&mut self, gob: crate::state::GobId) {
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

    pub(super) fn on_session_closed(&mut self, sid: SessionId) {
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
