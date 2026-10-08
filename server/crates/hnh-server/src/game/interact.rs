//! Player interaction and movement: the map-click dispatch, the
//! walk/interact routing, the movement tick with its packed fan-out
//! (LINSTEP cadence, pose streaming) and the shared `start_move`
//! entry every mover (players, animal AI, combat chase) goes through.
//!
//! Pure move out of game.rs (session 49 split continued).

use super::*;

impl Game {
    // ------------------------------------------------------------------
    // Map
    // ------------------------------------------------------------------

    pub(super) fn on_map_click(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
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

    pub(super) fn player_walk(&mut self, sid: SessionId, player_gob: GobId, target: (i32, i32)) {
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

    // ------------------------------------------------------------------
    // World gathering (docs/mechanics/crafting-and-building.md, "World
    // gathering"): the shared pick legs behind BOTH the local click path
    // (player_interact above) and the cross-node relay act (relay_static
    // in game.rs). Keeping one implementation prevents the local and
    // relay outcomes from drifting.
    // ------------------------------------------------------------------

    /// One branch pick off a tree: a branch drop lands next to the tree,
    /// the frame bump re-renders the harvest state for every viewer, and
    /// an exhausted tree leaves a decorative stump. Returns the LP the
    /// act grants (0 once exhausted).
    pub(super) fn harvest_tree(&mut self, target: GobId, tslot: usize) -> i32 {
        let Kind::Tree { harvests } = self.world.gobs.kind[tslot] else {
            return 0;
        };
        let pos = self.world.gobs.pos[tslot];
        debug!(target, harvests, "tree pick");
        if harvests > 0 {
            self.world.gobs.kind[tslot] = Kind::Tree {
                harvests: harvests - 1,
            };
            self.world.gobs.frame[tslot] += 1;
            self.spawn_drop_near(pos, "gfx/invobjs/branch", crate::state::GATHER_QL, "");
            // Re-publish so guest copies on subscriber nodes re-render.
            self.publish(target, GuestEv::Update);
            crate::state::TREE_PICK_LP
        } else {
            // Exhausted: remove the tree, leave a stump.
            self.world.gobs.kill(target);
            self.broadcast_retract(target);
            let stump = self.world.res.intern("gfx/terobjs/trees/log");
            let id = self.world.gobs.spawn(Kind::Stump, pos, stump, 1, 0);
            self.broadcast_spawn(id);
            0
        }
    }

    /// One stone pick off a boulder: a stone drop lands next to it; the
    /// depleted boulder disappears. Returns the LP the act grants.
    pub(super) fn harvest_boulder(&mut self, target: GobId, tslot: usize) -> i32 {
        let Kind::Boulder { left } = self.world.gobs.kind[tslot] else {
            return 0;
        };
        let pos = self.world.gobs.pos[tslot];
        debug!(target, left, "boulder pick");
        self.spawn_drop_near(pos, "gfx/invobjs/stone", crate::state::GATHER_QL, "");
        if left > 1 {
            self.world.gobs.kind[tslot] = Kind::Boulder { left: left - 1 };
            self.world.gobs.frame[tslot] += 1;
            self.publish(target, GuestEv::Update);
        } else {
            self.world.gobs.kill(target);
            self.broadcast_retract(target);
        }
        crate::state::STONE_PICK_LP
    }

    pub(super) fn player_interact(
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
            Kind::Tree { .. } => {
                let lp = self.harvest_tree(target, tslot);
                if lp > 0 {
                    if let Some(p) = self.world.player_mut(sid) {
                        p.lp += lp;
                    }
                    self.push_cattr(sid);
                    // Refresh the char sheet LP balance if it is open.
                    self.push_lp_msgs(sid);
                }
            }
            Kind::Boulder { .. } => {
                let lp = self.harvest_boulder(target, tslot);
                if let Some(p) = self.world.player_mut(sid) {
                    p.lp += lp;
                }
                self.push_cattr(sid);
                // Refresh the char sheet LP balance if it is open.
                self.push_lp_msgs(sid);
            }
            Kind::Stump => {
                // Decorative remnant: nothing to pick (docs "World
                // gathering").
                trace!(sid, target, "stump pick: nothing to yield");
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
                if buildable.id == "trough" {
                    // Food Trough (session 62): carrying -> the fodder
                    // transfer, otherwise the Lift flower menu.
                    self.trough_click(sid, target);
                    return;
                }
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
    // Movement tick + packed fan-out (move_batch.rs; sessions 41/44)
    // ------------------------------------------------------------------

    pub(super) fn tick_movement(&mut self) {
        let now = self.world.now_ms;
        // Encoded OD blocks for this tick's progress and finalization go
        // into the packed cell-indexed batch (see `move_batch` and
        // `broadcast_batch`): blocks are encoded once, then each session
        // probes only the non-empty cells - the O(sessions x movers)
        // per-session scan this replaces dominated the tick budget at the
        // 400+ mover scale (68 ms of movement phase measured at 426
        // players) and re-appeared at the duel-cohort load scale.
        //
        // Session 57 split the single loop into a SCAN pass (position
        // advance + dirty marks + candidate collection, zero wire work)
        // and an ENCODE pass (wire blocks off the collected candidates):
        // the mvbat_* perf counters attribute the two honestly, and the
        // scratch Vecs + the block encoder are taken/restored so the 10 Hz
        // hot path stays allocation-free (a fresh Vec::new() per tick and
        // a 256 B MessageBuf::new() + finish + drop per encoded block were
        // the loop's remaining allocator churn).
        let mut batch = std::mem::take(&mut self.move_scratch);
        batch.clear();
        let mut finished = std::mem::take(&mut self.mv_finished_scratch);
        finished.clear();
        let mut progress = std::mem::take(&mut self.mv_progress_scratch);
        progress.clear();
        let mut encode = std::mem::take(&mut self.mv_encode_scratch);
        let t_scan = Instant::now();
        let mut movers = 0usize;
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
            movers += 1;
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
                        progress.push((id, frame, l, cx, cy));
                    }
                }
            }
        }
        self.world.perf.mvbat_scan_us = t_scan.elapsed().as_micros() as u64;
        let t_enc = Instant::now();
        // Encode pass: block layout [fl][id i32][frame i32][ops..OD_END] -
        // NO per-block MSG header. The fan-out datagram carries ONE
        // MSG_OBJDATA type byte followed by consecutive blocks
        // (Session.getobjdata loops exactly this shape). Candidates iterate
        // in slot order - the old single loop pushed progress blocks inside
        // the scan and finalizers after it, so the packed batch order is
        // unchanged.
        for (id, frame, l, cx, cy) in progress.drain(..) {
            encode.clear();
            encode
                .uint8(0)
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
                encode.as_slice(),
            );
        }
        for (slot, tx, ty) in finished.drain(..) {
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
            encode.clear();
            encode
                .uint8(0)
                .int32(id)
                .int32(frame as i32)
                .uint8(OD_MOVE)
                .coord(tx, ty)
                .uint8(OD_LINSTEP)
                .int32(steps)
                .uint8(OD_END);
            batch.push(
                id,
                frame,
                crate::visidx::cell_of(tx, ty),
                true,
                encode.as_slice(),
            );
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
        self.world.perf.mvbat_encode_us = t_enc.elapsed().as_micros() as u64;
        self.world.perf.mvbat_movers = movers as u64;
        self.broadcast_batch(&mut batch);
        self.move_scratch = batch;
        self.mv_finished_scratch = finished;
        self.mv_progress_scratch = progress;
        self.mv_encode_scratch = encode;
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
    pub(super) fn broadcast_batch(&mut self, batch: &mut crate::move_batch::MoveBatch) {
        if batch.is_empty() {
            return;
        }
        self.world.perf.move_blocks += batch.len() as u64;
        self.world.perf.move_cells += batch.cell_count() as u64;
        let t_fan = Instant::now();
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
            // Dense sorted scan (session 57): the groups are ordered by
            // (y, x), so this session's y-cell range is ONE contiguous
            // segment - binary-search its start, break at its end, and
            // x-test inside. Strictly sequential memory (a few KiB in
            // L1/L2) instead of the old per-session HashMap walk.
            batch.ensure_groups();
            let (groups, order) = batch.groups();
            let cy0 = crate::move_batch::axis_cell_lo(*py, FANOUT_SPAN);
            let cy1 = crate::move_batch::axis_cell_hi(*py, FANOUT_SPAN);
            let cx0 = crate::move_batch::axis_cell_lo(*px, FANOUT_SPAN);
            let cx1 = crate::move_batch::axis_cell_hi(*px, FANOUT_SPAN);
            let first = groups.partition_point(|g| g.y < cy0);
            for g in &groups[first..] {
                if g.y > cy1 {
                    break;
                }
                if g.x < cx0 || g.x > cx1 {
                    continue;
                }
                for &i in &order[g.off as usize..(g.off + g.len) as usize] {
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
                                Self::record_unacked(out, id, frame, patched, false);
                            }
                        }
                        None => {
                            m.bytes(bytes);
                            if fin {
                                Self::record_unacked(out, id, frame, bytes.to_vec(), false);
                            }
                        }
                    }
                }
            }
            if let Some(m) = m {
                out.send_raw(m.finish());
            }
        }
        // `+=`: the movement batch calls this mid-tick, the start/FX batch
        // at tick end - one attribution total per fan-out.
        self.world.perf.mvbat_fanout_us += t_fan.elapsed().as_micros() as u64;
        self.fan_scratch = sids_pos;
    }

    /// Record an OBJDATA block for per-gob retransmission. The per-gob
    /// map is CAPPED at the last 4 frames: sessions that never OBJACK
    /// (load bots, slow clients mid-lag) would otherwise grow it without
    /// bound - measured OOM driver at the 1000-session scale (~40 MB/s of
    /// finalizer blocks before the cap).
    pub(super) fn record_unacked(
        out: &mut SessionOut,
        id: GobId,
        frame: u32,
        block: Vec<u8>,
        critical: bool,
    ) {
        const UNACKED_CAP: usize = 4;
        let per = out.unacked.entry(id).or_default();
        per.insert(
            frame,
            crate::state::UnackedBlock {
                bytes: block,
                last_sent: Instant::now(),
                tries: 0,
                critical,
            },
        );
        while per.len() > UNACKED_CAP {
            // BTreeMap: the first key IS the min frame - no O(n) scan
            // the HashMap version paid.
            let Some(min) = per.keys().copied().next() else {
                break;
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
    pub(super) fn fx_overlay_broadcast(&mut self, id: GobId, res_name: &'static str) {
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
    pub(super) fn stream_pose(&mut self, slot: usize) {
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
    pub(super) fn stream_avatar(&mut self, slot: usize) {
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
    pub(super) fn interpolated_pos(&self, slot: usize) -> (i32, i32) {
        match self.world.gobs.mv[slot] {
            Some(lm) => lm.pos_at(self.world.now_ms),
            None => self.world.gobs.pos[slot],
        }
    }

    /// Begin (or retarget) a linear move for the gob at `slot` toward
    /// `target`. Shared by player clicks, animal AI, and combat chase so
    /// every mover uses one timing model (`LinMove::client_steps`) and
    /// one retarget rule: when already moving, the new move starts from
    /// the CURRENTLY INTERPOLATED position - never from the old
    /// destination, which is what teleported the avatar on rapid clicks.
    pub(super) fn start_move(&mut self, slot: usize, target: (i32, i32)) -> bool {
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
}
