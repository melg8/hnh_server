//! Farming: tile plowing and mutation, seed planting, crop menus
//! and the harvest flow, and the crop growth tick.

use super::*;

impl Game {
    /// Map units -> tile coordinates (11x11 map units per tile).
    pub(super) fn tile_coord(mx: i32, my: i32) -> (i32, i32) {
        (mx.div_euclid(11), my.div_euclid(11))
    }

    /// Plow Field action: furrow one grass tile (Adventure > Landscaping).
    /// Tile 9 (PLOWED) is a real tile type the client renders from the
    /// map stream; the mutation is recorded as a grid override so it
    /// survives eviction/restart, and fresh MAPDATA re-sent to holders.
    pub(super) fn plow_tile(&mut self, sid: SessionId, (tx, ty): (i32, i32)) {
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
    pub(super) fn mutate_tile_local(
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
    pub(super) fn apply_remote_tile_mutation(&mut self, tx: i32, ty: i32, tile: u8) {
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
    pub(super) fn plant_seed(
        &mut self,
        sid: SessionId,
        spec: usize,
        (tx, ty): (i32, i32),
        cursor: InvStack,
    ) {
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
    pub(super) fn open_crop_menu(&mut self, sid: SessionId, target: GobId) {
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
    pub(super) fn show_crop_menu(&mut self, sid: SessionId, target: GobId, spec: u8, stage: u8) {
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
    pub(super) fn harvest_crop(&mut self, sid: SessionId, wid: u16, choice: i32) {
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
    pub(super) fn tick_farming(&mut self) {
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
}
