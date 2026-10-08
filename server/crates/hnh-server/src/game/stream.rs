//! Wire-side gob streaming: map requests, the per-session gob
//! block encoder, spawn/retract streaming and the visibility
//! update pass.

use super::*;

impl Game {
    pub(super) fn on_mapreq(&mut self, sid: SessionId, gc: (i32, i32)) {
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
    pub(super) fn encode_gob_block(
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
    pub(super) fn stream_spawn(&mut self, sid: SessionId, id: GobId) {
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
                // Spawn is a critical-loss block: the client never
                // re-requests gob state, so a lost datagram would leave
                // an invisible object forever (the session-63 finding).
                Self::record_unacked(out, id, frame, block, true);
            }
        }
    }

    pub(super) fn stream_retract(&mut self, sid: SessionId, id: GobId) {
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
            // The removal must ride a frame HIGHER than every frame
            // this session ever saw for the gob (spawn, updates, hp
            // ticks): the retransmit sweep skips frames at/below the
            // client's acked high-water mark, and the on-objack retain
            // likewise drops frames an ack covers. max(known frames) +
            // 1 keeps the OD_REM wire-fresh; the client ignores the
            // frame of a removal op.
            let acked_frame = out.gob_acked.get(&id).copied().flatten().unwrap_or(0);
            let pending_max = out
                .unacked
                .get(&id)
                .and_then(|per| per.keys().next_back().copied())
                .unwrap_or(0);
            let rem_frame = frame.max(acked_frame).max(pending_max).saturating_add(1);
            let mut m = MessageBuf::new();
            m.uint8(MSG_OBJDATA)
                .uint8(0)
                .int32(id)
                .int32(rem_frame as i32)
                .uint8(OD_REM)
                .uint8(OD_END);
            // Stale pending blocks of this gob must not survive the
            // retract: a resent spawn frame arriving after OD_REM would
            // resurrect a phantom client-side (the retransmit sweep is
            // in-order, so retire the whole history here).
            out.unacked.remove(&id);
            out.gob_acked.remove(&id);
            // The retract itself is a critical-loss block: a client
            // that misses OD_REM renders a phantom gob forever
            // (nothing re-retracts).
            Self::record_unacked(out, id, rem_frame, m.finish(), true);
        } else {
            out.unacked.remove(&id);
            out.gob_acked.remove(&id);
        }
    }

    /// Per-tick visibility update: spawns, retractions, movement deltas.
    /// Visibility: parallel in-range candidate scan (phase A, read-only
    /// over the SoA columns), then serial spawn/move/retract application
    /// (phase B, mutates session state and streams wire blocks).
    pub(super) fn update_visibility(&mut self) {
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
        //
        // Allocation-free pass (session 52): per-session result lists are
        // appended into ONE flat buffer; `ranges` holds (start, len) per
        // to_scan entry. Each rayon task reuses a single (flat, scratch)
        // pair for its whole partition - the per-session Vec churn this
        // replaces was ~1000 heap allocations per tick at the 1000-
        // session scale.
        let scan_t = Instant::now();
        struct ScanRanges {
            ranges: Vec<(u32, u32)>,
            flat: Vec<GobId>,
        }
        let scanned = if self.workers > 1 && to_scan.len() > 8 {
            let nodes = std::num::NonZeroUsize::new(self.workers).expect("workers >= 1");
            let parts = crate::grid_owner::partition_by_owner(
                |&i| crate::visidx::cell_of(to_scan[i].1 .0, to_scan[i].1 .1),
                (0..to_scan.len()).collect::<Vec<usize>>(),
                nodes,
            );
            // Each task: one reusable buffer pair for its whole partition,
            // emitting (index, start, len) triples plus its flat segment.
            type ScanSegment = (Vec<(usize, u32, u32)>, Vec<GobId>);
            let per_task: Vec<ScanSegment> = parts
                .par_iter()
                .map(|part| {
                    // seg is the per-session working buffer (the scan
                    // functions clear it); flat accumulates the results
                    // of the whole partition and is NEVER cleared.
                    let mut seg: Vec<GobId> = Vec::with_capacity(512);
                    let mut scratch: Vec<GobId> = Vec::new();
                    let mut flat: Vec<GobId> = Vec::with_capacity(part.len() * 512);
                    let mut triples = Vec::with_capacity(part.len());
                    for &i in part {
                        self.scan_for_entry_into(&to_scan[i], &mut seg, &mut scratch);
                        let start = flat.len() as u32;
                        flat.extend_from_slice(&seg);
                        let len = flat.len() as u32 - start;
                        triples.push((i, start, len));
                    }
                    (triples, flat)
                })
                .collect();
            // Merge the task segments: ranges are written BY to_scan
            // INDEX so Phase B's ranges[i] always pairs with to_scan[i].
            // The flat buffer holds the segments in whatever order the
            // tasks finished; each (offset, len) range is self-contained,
            // so no global reordering is needed.
            let total: usize = per_task.iter().map(|(t, f)| t.len() + f.len()).sum();
            let mut merged = ScanRanges {
                ranges: vec![(0, 0); to_scan.len()],
                flat: Vec::with_capacity(total),
            };
            for (triples, flat) in per_task {
                for (i, start, len) in triples {
                    let start = start as usize;
                    let end = start + len as usize;
                    merged.ranges[i] = (merged.flat.len() as u32, len);
                    merged.flat.extend_from_slice(&flat[start..end]);
                }
            }
            merged
        } else {
            let mut seg: Vec<GobId> = Vec::with_capacity(512);
            let mut scratch: Vec<GobId> = Vec::new();
            let mut flat: Vec<GobId> = Vec::with_capacity(to_scan.len() * 512);
            let mut ranges = Vec::with_capacity(to_scan.len());
            for e in &to_scan {
                self.scan_for_entry_into(e, &mut seg, &mut scratch);
                let start = flat.len() as u32;
                flat.extend_from_slice(&seg);
                let len = flat.len() as u32 - start;
                ranges.push((start, len));
            }
            ScanRanges { ranges, flat }
        };
        self.world.perf.vis_gob_scans += scanned.flat.len() as u64;
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
        for (i, (sid, (px, py), cell_moved, _kind)) in to_scan.iter().enumerate() {
            let (start, len) = scanned.ranges[i];
            let cand = &scanned.flat[start as usize..start as usize + len as usize];
            let spawn_t = Instant::now();
            for id in cand {
                // Check-only here: stream_spawn performs the insert and
                // skips already-present ids; inserting before calling it
                // would suppress the spawn block entirely (the avatar
                // bug: the client never received its own gob).
                let is_new = !self.sessions[sid].visible.contains(id);
                if is_new {
                    spawn_count += 1;
                    self.stream_spawn(*sid, *id);
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
            if *cell_moved || self.world.tick.is_multiple_of(8) {
                self.retract_sweep_due(*sid, *px, *py);
            }
            // The result list becomes the session's cache. The previous
            // cache Vec is recycled (take -> clear -> refill) so the
            // steady state allocates nothing here (mem-reuse-collections).
            if let Some(out) = self.sessions.get_mut(sid) {
                let mut cache = out.vis_cache.take().unwrap_or_default();
                cache.clear();
                cache.extend_from_slice(cand);
                out.vis_cache = Some(cache);
                out.vis_cache_pos = Some((*px, *py));
            }
            retract_us += retract_t.elapsed().as_micros();
            self.world.perf.visible_total += self.sessions[sid].visible.len();
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
    /// Allocation-free scan: writes the in-range id list into `out`
    /// (cleared first); `scratch` is a reusable buffer for the patch
    /// path's touched-id candidates. One (out, scratch) pair serves the
    /// whole scan pass per worker (perf-drain-reuse / mem-reuse-collections).
    fn scan_for_entry_into(
        &self,
        e: &(SessionId, (i32, i32), bool, bool),
        out: &mut Vec<GobId>,
        scratch: &mut Vec<GobId>,
    ) {
        // Slot 4: true = Patch (patch the cached result), false = Full.
        // The cached list is BORROWED (read) - no per-tick clone on the
        // hot path; all access here is immutable so rayon shares &self.
        let (sid, (px, py), _moved, patch) = (e.0, e.1, e.2, e.3);
        match self.sessions.get(&sid).and_then(|o| o.vis_cache.as_deref()) {
            Some(cached) if patch => self.patch_vis_cache_into(px, py, cached, out, scratch),
            _ => self.scan_visible_into(px, py, out),
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
    /// Allocation-free variant: the result is written into `out` (cleared
    /// first); `scratch` receives the touched-id candidates.
    fn patch_vis_cache_into(
        &self,
        px: i32,
        py: i32,
        cached: &[GobId],
        out: &mut Vec<GobId>,
        scratch: &mut Vec<GobId>,
    ) {
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
        out.clear();
        out.reserve(cached.len() + 16);
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
        self.world
            .gobs
            .vis
            .touched_in_view_into(px, py, VIEW_RADIUS, scratch);
        for &id in scratch.iter() {
            if in_range(id) && out.binary_search(&id).is_err() {
                out.push(id);
            }
        }
        out.sort_unstable();
        out.dedup();
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
    /// Cluster guests merge in (foreign-authority gobs rendered locally).
    /// Guests live in the SAME cell buckets as local gobs - ingest_guest,
    /// the authority-demote paths and remove_guest keep that invariant -
    /// so the compaction below resolves an id through the SoA columns
    /// first and the guest table second. The pre-session-54 design
    /// dropped guests in the compaction and then re-walked the WHOLE
    /// guest table per rescan: O(node guest population) per session per
    /// scan - fine single-node (the table is empty), a landmine at
    /// multi-node 10k where the table holds every foreign gob any local
    /// session ever subscribed to. The scan cost is now bounded by the
    /// guest population of the VIEW cells only.
    pub(super) fn scan_visible_into(&self, px: i32, py: i32, out: &mut Vec<GobId>) {
        out.clear();
        self.world
            .gobs
            .vis
            .gobs_in_view_into(px, py, VIEW_RADIUS, out);
        // In-place compaction: cell buckets over-cover the exact view
        // square, dead ids vanish between bucket updates. Write-index
        // compaction keeps the buffer allocation-free (coll-seq-choice).
        let mut w = 0usize;
        for r in 0..out.len() {
            let id = out[r];
            let gpos = match self.world.gobs.get(id) {
                Some(slot) => self.world.gobs.pos[slot],
                None => match self.world.guests.get(&id) {
                    Some(g) => g.pos,
                    // Neither table: a dead gob between bucket updates.
                    None => continue,
                },
            };
            if (gpos.0 - px).abs() > VIEW_RADIUS || (gpos.1 - py).abs() > VIEW_RADIUS {
                continue;
            }
            out[w] = id;
            w += 1;
        }
        out.truncate(w);
    }

    pub(super) fn on_objack(&mut self, sid: SessionId, acks: Vec<(GobId, u32)>) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        for (id, frame) in acks {
            // Track the confirmed high-water mark for the retransmit
            // sweep's in-order gate (the client echoes its max decoded
            // frame, so a stale smaller ack never rolls this back).
            let acked = out.gob_acked.entry(id).or_insert(None);
            if frame > acked.unwrap_or(0) {
                *acked = Some(frame);
            }
            if let Some(per_gob) = out.unacked.get_mut(&id) {
                per_gob.retain(|f, _| *f > frame);
                if per_gob.is_empty() {
                    out.unacked.remove(&id);
                    // The mark only gates pending blocks; a gob with
                    // nothing pending drops it (new frames are always
                    // higher - frames grow monotonically).
                    out.gob_acked.remove(&id);
                }
            }
        }
    }

    /// OBJACK-driven retransmission sweep (session 64): every recorded
    /// block still unconfirmed past its schedule delay is presumed
    /// lost and resent. Runs every 3rd tick (~300 ms) as a rare-event
    /// pass: in the steady state the table is near-empty (the client
    /// acks within <= 320 ms), so the sweep costs one map walk.
    ///
    /// In-order guarantee: frames ascend (BTreeMap) and the raw socket
    /// is FIFO, so a sweep sends lost datagrams in their original wire
    /// order. The walk latches `blocked` while the LOWEST unconfirmed
    /// frame of a gob is still inside its delay window - a resent
    /// stale frame must never overtake a newer one (an out-of-order
    /// OD_REM would phantom-delete a fresh spawn client-side).
    pub(super) fn retransmit_unacked(&mut self) {
        // Session 65: the sweep is now attributed in the perf report
        // (retx_* fields) - "verify, do not assume" applies to the
        // rare-event claim too: the load-bot cohort never echoes
        // OBJACK, so its blocks live here until retirement.
        let sweep_started = Instant::now();
        let now = sweep_started;
        let mut pending_n = 0usize;
        let mut resent_n = 0usize;
        let mut full_n = 0usize;
        let mut busy_sessions = 0usize;
        for out in self.sessions.values_mut() {
            if out.unacked.is_empty() {
                continue;
            }
            // Backpressure gate (session 65): a session that refused
            // sends in the last RETRANS_THROTTLE_MS gets no retried
            // blocks - try_send against a full bounded channel measured
            // 85% refusals at the 1000-bot scale, so the honest move is
            // to let the queue drain and come back on a later sweep.
            if out.retx_throttle_until > now {
                continue;
            }
            busy_sessions += 1;
            let raw = out.raw.clone();
            let acked = &out.gob_acked;
            let mut queue_full = false;
            for (id, per) in out.unacked.iter_mut() {
                let acked = acked.get(id).copied().flatten();
                // Ordered walk: `blocked` latches while the lowest
                // unconfirmed frame is still inside its delay window.
                let mut blocked = false;
                for (frame, block) in per.iter_mut() {
                    pending_n += 1;
                    if blocked || acked.is_some_and(|a| *frame <= a) {
                        continue;
                    }
                    if block.expired(now) {
                        // Past the try-count schedule or the hard age
                        // ceiling (session 65): the peer never acks this
                        // one (dead peer / load bot) - the retire pass
                        // drops it; block the rest of the gob so no
                        // later frame escapes through the hole.
                        blocked = true;
                        continue;
                    }
                    if now.duration_since(block.last_sent) < block.delay() {
                        blocked = true;
                        continue;
                    }
                    // A full raw queue (a burst fan-out to a slow
                    // session) must NOT burn the block's attempts: the
                    // next sweep retries while the queue drains. It also
                    // throttles the session for RETRANS_THROTTLE_MS -
                    // firing retries into a saturated channel measured
                    // 85% refusals and a 426 ms sweep at the 1000-bot
                    // scale.
                    if raw.try_send(block.bytes.clone()).is_ok() {
                        block.last_sent = now;
                        block.tries += 1;
                        resent_n += 1;
                    } else {
                        queue_full = true;
                        blocked = true;
                        full_n += 1;
                    }
                }
            }
            if queue_full {
                out.retx_throttle_until = now + Duration::from_millis(RETRANS_THROTTLE_MS);
            }
        }
        // Structured observability (obs-structured-fields): fires only
        // when there was retransmit work - silent in the healthy
        // steady state where the table drains within ~320 ms.
        if resent_n > 0 || full_n > 0 {
            debug!(
                pending = pending_n,
                resent = resent_n,
                queue_full = full_n,
                "objdata retransmit"
            );
        }
        // Retire expired blocks (the ordered walk cannot mutate the
        // map structure mid-iteration). Skipped entirely when nothing
        // is pending: the per-sweep attribution showed the walk itself
        // is the only measurable cost of this pass at 1000 sessions.
        let p = &mut self.world.perf;
        p.retx_sweep_us = sweep_started.elapsed().as_micros() as u64;
        p.retx_pending = pending_n as u64;
        p.retx_resent = resent_n as u64;
        p.retx_queue_full = full_n as u64;
        p.retx_busy_sessions = busy_sessions as u64;
        if pending_n > 0 {
            for out in self.sessions.values_mut() {
                if out.unacked.is_empty() {
                    continue;
                }
                out.unacked.retain(|_, per| {
                    per.retain(|_, block| !block.expired(now));
                    !per.is_empty()
                });
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

    pub(super) fn is_cluster(&self) -> bool {
        self.cluster.is_some()
    }

    /// Owning node of one VisIndex cell (0 in single-node mode).
    pub(super) fn cell_owner(&self, cell: (i32, i32)) -> usize {
        match &self.cluster {
            Some(c) => crate::grid_owner::owner_of(cell, c.nodes),
            None => 0,
        }
    }

    /// Simulation authority for the gob at `slot`. Players in MY gob table
    /// are my sessions' players — homed here by definition. Everything else
    /// follows its cell's owner.
    pub(super) fn is_authority_slot(&self, slot: usize) -> bool {
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
    pub(super) fn node_of_gob(&self, id: GobId) -> usize {
        match &self.cluster {
            None => 0,
            Some(c) => {
                let slot = (id & 0xFFFF) as usize;
                let per = (crate::state::MAX_SLOT + 1) / c.nodes.get();
                (slot / per).min(c.nodes.get() - 1)
            }
        }
    }
}
