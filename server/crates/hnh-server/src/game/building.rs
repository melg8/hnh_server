//! Construction: build pagina placement, plan material sinking,
//! completion, and the finished production stations (oven/smelter)
//! plus the Food Trough fodder store (session 48).

use super::*;

impl Game {
    /// Build pagina activated: drive the client into placement mode. The
    /// mapview `place` uimsg carries (resname, version, on-tile[, radius]);
    /// the ghost plob follows the mouse until the player commits.
    pub(super) fn arm_build_placement(&mut self, sid: SessionId, spec: usize) {
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
    pub(super) fn on_map_place(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
        let mc = args.iter().filter_map(|a| a.as_coord()).next();
        let button = args.iter().filter_map(|a| a.as_int()).next().unwrap_or(1);
        if button != 1 {
            self.cancel_build(sid);
            return;
        }
        let Some((mx, my)) = mc else { return };
        // A lifted Food Trough (session 62) takes precedence over the
        // build ghost: a map click while carrying places the trough on
        // the clicked tile (docs "lift-and-right-click"; the legacy
        // client drives placement through the same mapview place path).
        if self.world.player(sid).map(|p| p.carried_trough.is_some()) == Some(true) {
            self.place_carried_trough(sid, (mx, my));
            return;
        }
        let Some(spec) = self.sessions.get(&sid).and_then(|o| o.pending_build) else {
            debug!(sid, "place without armed build: ignoring");
            return;
        };
        self.commit_build(sid, spec, (mx, my));
    }

    /// Click on a Food Trough (Kind::Structure with the trough spec,
    /// session 62): carrying a trough transfers its fodder "like a
    /// liquid" into the clicked one; otherwise the Lift flower menu
    /// opens. Guest troughs (another node's authority) stay out of
    /// scope - the click is a validated no-op there, like a stump pick.
    pub(super) fn trough_click(&mut self, sid: SessionId, target: GobId) {
        if !self.world.troughs.contains_key(&target) {
            return;
        }
        if self.world.player(sid).map(|p| p.carried_trough.is_some()) == Some(true) {
            self.transfer_trough_fodder(sid, target);
        } else {
            self.open_trough_menu(sid, target);
        }
    }

    /// Open the Lift flower menu on a placed Food Trough.
    fn open_trough_menu(&mut self, sid: SessionId, target: GobId) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        // One flower menu at a time per session (the shared rule).
        if let Some((old, _)) = out.trough_menu {
            out.send(wdg::dst_wdg(old));
            out.trough_menu = None;
        }
        if let Some((old, _, _)) = out.station_menu {
            out.send(wdg::dst_wdg(old));
            out.station_menu = None;
        }
        let w = out.new_wid("sm");
        out.send(wdg::new_wdg(
            w,
            "sm",
            -1,
            -1,
            0,
            &[ListVal::S("Lift".to_owned())],
        ));
        out.trough_menu = Some((w, target));
    }

    /// Lift choice on a trough menu: the trough leaves the world (the
    /// gob is retracted for every viewer) and its fodder store rides
    /// the player. One carried object at a time.
    pub(super) fn apply_trough_choice(&mut self, sid: SessionId, wid: u16, choice: i32) {
        let pending = self
            .sessions
            .get(&sid)
            .and_then(|o| o.trough_menu)
            .filter(|(w, _)| *w == wid);
        let Some((_, gob)) = pending else {
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.trough_menu = None;
        out.send(wdg::dst_wdg(wid));
        if choice != 0 {
            out.send(wdg::wdgmsg(wid, "cancel", &[]));
            return;
        }
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return;
        };
        if self.world.players[pidx].carried_trough.is_some() {
            self.system_line(sid, "You are already carrying something.");
            return;
        }
        let Some(state) = self.world.troughs.remove(&gob) else {
            return;
        };
        // Retract for every viewer (the Drop-pickup removal path) and
        // free the tile so a later placement can reuse it.
        if let Some(slot) = self.world.gobs.get(gob) {
            let pos = self.world.gobs.pos[slot];
            let tile = (pos.0.div_euclid(11), pos.1.div_euclid(11));
            self.world.structure_at.remove(&tile);
        }
        self.world.gobs.kill(gob);
        self.broadcast_retract(gob);
        let units = state.units;
        self.world.players[pidx].carried_trough = Some(state);
        self.system_line(
            sid,
            &format!("You lift the trough ({} fodder units).", units),
        );
        info!(sid, gob, units, "trough lifted");
    }

    /// Map-click placement of a carried Food Trough: the same tile
    /// validations as a build commit (reach, walkable terrain, no crop
    /// / plan / structure on the tile), then the trough re-enters the
    /// world with the fodder store it left with.
    fn place_carried_trough(&mut self, sid: SessionId, (mx, my): (i32, i32)) {
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return;
        };
        let Some(spec) = crate::build::BUILDABLES
            .iter()
            .position(|b| b.id == "trough")
        else {
            return;
        };
        let buildable = &crate::build::BUILDABLES[spec];
        let tile = Self::tile_coord(mx, my);
        // Reach: identical 5-tile policy to a build commit.
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
            self.system_line(sid, "Too far away to place that there.");
            return;
        }
        // Terrain + occupancy: passable and free of crops/plans/structures.
        let gc = (tile.0.div_euclid(100), tile.1.div_euclid(100));
        let (lx, ly) = (
            tile.0.rem_euclid(100) as usize,
            tile.1.rem_euclid(100) as usize,
        );
        let t = self.world.grids.grid(gc).tile(lx, ly);
        if crate::state::tile_speed(t).is_none()
            || self.world.crop_at.contains_key(&tile)
            || self.world.plan_at.contains_key(&tile)
            || self.world.structure_at.contains_key(&tile)
        {
            self.system_line(sid, "You cannot place the trough there.");
            return;
        }
        let Some(carried) = self.world.players[pidx].carried_trough.take() else {
            return;
        };
        let res_idx = self.world.res.intern(buildable.res);
        let pos = (tile.0 * 11 + 5, tile.1 * 11 + 5);
        let gob = self.world.gobs.spawn(
            Kind::Structure { spec: spec as u8 },
            pos,
            res_idx,
            buildable.hp,
            0,
        );
        self.world.troughs.insert(gob, carried);
        self.world.structure_at.insert(tile, gob);
        self.broadcast_spawn(gob);
        self.system_line(
            sid,
            &format!("You place the trough ({} fodder units).", carried.units),
        );
        info!(sid, gob, units = carried.units, ?tile, "trough placed");
    }

    /// Transfer the carried fodder into a clicked placed trough "like a
    /// liquid" (docs animals-and-husbandry.md "Feeding: troughs and
    /// grazing"): units move up to the destination's capacity; the
    /// quality average mixes by the same running-average arithmetic the
    /// load path uses (the moved units carry the source's average).
    fn transfer_trough_fodder(&mut self, sid: SessionId, target: GobId) {
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return;
        };
        let Some(dest) = self.world.troughs.get_mut(&target) else {
            return;
        };
        let Some(carried) = self.world.players[pidx].carried_trough else {
            return;
        };
        let free = crate::state::TROUGH_CAP_UNITS - dest.units;
        let moved = carried.units.min(free);
        if moved == 0 {
            self.system_line(sid, "The trough is full.");
            return;
        }
        // The moved units leave the source at the source's average. The
        // source keeps its FULL quality history (the session-48 rule
        // "consumption drains units but NOT the quality history" - a
        // transfer consumes the source's units), so its average is
        // invariant under the move and an emptied trough keeps its q.
        let src_avg = carried
            .ql_sum
            .checked_div(carried.ql_seen)
            .unwrap_or(10) // an empty store rides at the grazing baseline
            .min(255) as u8;
        dest.units += moved;
        dest.ql_seen += moved as u64;
        dest.ql_sum += u64::from(src_avg) * moved as u64;
        let remaining = carried.units - moved;
        self.world.players[pidx].carried_trough = Some(TroughState {
            units: remaining,
            ql_sum: carried.ql_sum,
            ql_seen: carried.ql_seen,
        });
        self.system_line(sid, &format!("Transferred {} fodder units.", moved));
        info!(sid, target, moved, remaining, "trough fodder transferred");
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
    pub(super) fn sink_material(&mut self, sid: SessionId, gob: GobId, mut cursor: InvStack) {
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
    pub(super) fn complete_plan(&mut self, gob: GobId) {
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
            // The Food Trough is a plain structure with a fodder store:
            // open an empty one the moment it completes (itemact fills
            // it; the production sweep drains it).
            if buildable.id == "trough" {
                self.world
                    .troughs
                    .insert(gob, crate::state::TroughState::default());
            }
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
                // Build-transition re-render: a lost one leaves a
                // permanently stale sprite client-side - critical.
                Self::record_unacked(out, gob, frame, block, true);
            }
        }
    }

    /// itemact on a finished Food Trough (session 48; animals-and-
    /// husbandry.md "Feeding: troughs and grazing"): a fodder item on
    /// the cursor tops up the store, one item per click (the oven-fuel
    /// accounting policy keeps the quality average exact). Refusals
    /// chat and destroy nothing. The doc's lift-and-right-click
    /// trough-to-trough transfer stays out of scope until a lift
    /// mechanic exists (Open questions).
    pub(super) fn trough_itemact(&mut self, sid: SessionId, gob: GobId, mut cursor: InvStack) {
        let resname = match self.world.res.name(cursor.res) {
            Some(n) => n,
            None => return,
        };
        let Some(per_item) = crate::state::fodder_units(resname) else {
            self.system_line(sid, "The trough does not accept that as fodder.");
            return;
        };
        let Some(trough) = self.world.troughs.get_mut(&gob) else {
            return;
        };
        let free = crate::state::TROUGH_CAP_UNITS - trough.units;
        if free == 0 {
            self.system_line(sid, "The trough is full.");
            return;
        }
        let take = per_item.min(free);
        trough.units += take;
        trough.ql_sum += u64::from(cursor.ql) * u64::from(take);
        trough.ql_seen += u64::from(take);
        let avg = trough.avg_ql();
        cursor.count -= 1;
        if let Some(out) = self.sessions.get_mut(&sid) {
            out.cursor = if cursor.count == 0 {
                None
            } else {
                Some(cursor)
            };
        }
        self.refresh_inventory(sid);
        self.system_line(sid, "Fodder added to the trough.");
        info!(sid, gob, taken = take, avg, "trough loaded");
    }

    /// itemact on a finished station: fuel deliveries fill the fuel
    /// store; the roast input fills the single input slot (unlit only).
    pub(super) fn station_itemact(&mut self, sid: SessionId, gob: GobId, mut cursor: InvStack) {
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
    pub(super) fn open_station_menu(&mut self, sid: SessionId, target: GobId) {
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
    pub(super) fn show_station_menu(&mut self, sid: SessionId, target: GobId, lit: bool) {
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
    pub(super) fn apply_station_choice(&mut self, sid: SessionId, wid: u16, choice: i32) {
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
    pub(super) fn set_station_lit(&mut self, gob: GobId, lit: bool) {
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
    pub(super) fn tick_stations(&mut self) {
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
}
