//! Guest-node relay application: the authority-side legs that apply
//! interactions relayed by a peer node (plant, plow, station act /
//! item, static act, crop harvest, pickup, melee swing). The wire
//! contracts live in game/cluster.rs; this file owns the validated
//! world mutations.
//!
//! Pure move out of game.rs (session 70 split).

use super::*;

impl Game {
    /// Authority-side application of a relayed planting act (session 31).
    /// The seed physically lives on the home node's cursor (its quality
    /// rides the act); the TILE state (tilth, occupancy) is authoritative
    /// HERE. Same validation order as the local plant_seed path, minus
    /// the skill gate (session state stays on the home node) - reach
    /// parity note: the local path also has no explicit reach check, the
    /// client can only aim inside its own view. Refusals stay silent:
    /// the home node's cursor keeps the seed, matching a local refusal.
    pub(super) fn relay_plant(&mut self, player: GobId, tx: i32, ty: i32, spec: u8, seed_ql: u8) {
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
    pub(super) fn relay_plow(&mut self, player: GobId, tx: i32, ty: i32) {
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
    pub(super) fn answer_plow(&mut self, player: GobId, ok: bool) {
        if let Some(c) = self.cluster.as_ref() {
            let home = self.node_of_gob(player);
            c.mesh
                .send(home, crate::nodes::NodeMsg::PlowAck { player, ok });
        }
    }

    /// Unicast StationAck to the acting player's home node (session 33;
    /// no-op without a cluster - the local menu path never relays).
    pub(super) fn answer_station(&mut self, player: GobId, result: crate::nodes::StationResult) {
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
    pub(super) fn relay_station_act(
        &mut self,
        player: GobId,
        target: GobId,
        act: crate::nodes::StationAct,
    ) {
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
                // The crucible needs BOTH slots (copper + tin) to light.
                if crate::build::BUILDABLES[station.spec as usize]
                    .station
                    .as_ref()
                    .is_some_and(|s| s.kind == crate::build::StationKind::Alloyer)
                    && station.aux.is_none()
                {
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
    pub(super) fn relay_station_item(
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
        // check first, then the lit/input gates for the input slot).
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
        // The crucible's tin delivery lands in the aux slot, so the
        // single-input gate below must not fire for it.
        let aux_target = station_spec.kind == crate::build::StationKind::Alloyer
            && stack
                .label
                .eq_ignore_ascii_case(crate::craft::ALLOY_INPUT_TIN);
        if !aux_target && station.input.is_some() {
            self.answer_station_item(player, StationItemResult::InputFull);
            return;
        }
        // Station input dispatch mirrors station_itemact: the oven roasts
        // meat labels and bakes dough labels (session 71), the smelter
        // melts ore labels, the crucible takes copper (input) and tin
        // (aux) (session 66), the kiln takes the Clay label (session 69).
        let accepts = match station_spec.kind {
            crate::build::StationKind::Oven => {
                crate::craft::roast_result(leak_static(stack.label.as_str())).is_some()
                    || crate::craft::bake_result(leak_static(stack.label.as_str())).is_some()
            }
            crate::build::StationKind::Smelter => {
                crate::craft::smelt_result(leak_static(stack.label.as_str())).is_some()
            }
            crate::build::StationKind::Alloyer => {
                stack
                    .label
                    .eq_ignore_ascii_case(crate::craft::ALLOY_INPUT_COPPER)
                    || stack
                        .label
                        .eq_ignore_ascii_case(crate::craft::ALLOY_INPUT_TIN)
            }
            crate::build::StationKind::Kiln => {
                crate::craft::kiln_result(leak_static(stack.label.as_str())).is_some()
            }
            // Session 71: the quern grinds Grist of Wheat.
            crate::build::StationKind::Quern => {
                crate::craft::grind_result(leak_static(stack.label.as_str())).is_some()
            }
        };
        if !accepts {
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
        if aux_target {
            if st.aux.is_some() {
                self.answer_station_item(player, StationItemResult::InputFull);
                return;
            }
            st.aux = Some((res_idx, stack.ql, leak_static(stack.label.as_str())));
        } else {
            st.input = Some((res_idx, stack.ql, leak_static(stack.label.as_str())));
        }
        self.publish(target, GuestEv::Update);
        self.answer_station_item(player, StationItemResult::InputLoaded);
        info!(target, label = stack.label, "relay station input loaded");
    }

    /// Unicast StationItemAck to the acting player's home node.
    pub(super) fn answer_station_item(
        &mut self,
        player: GobId,
        result: crate::nodes::StationItemResult,
    ) {
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
    pub(super) fn relay_static(
        &mut self,
        player: GobId,
        target: GobId,
        act: crate::nodes::StaticAct,
    ) {
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
                (Kind::OreDeposit { .. }, StaticAct::Mine) => {
                    Some((vec![None], self.harvest_ore_deposit(target, tslot)))
                }
                // Session 69: shore clay deposits relay the Mine act
                // exactly like ore deposits (the Stone class tag).
                (Kind::ClayDeposit { .. }, StaticAct::Mine) => {
                    Some((vec![None], self.harvest_clay_deposit(target, tslot)))
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
    pub(super) fn relay_crop_harvest(
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
    pub(super) fn relay_pickup(
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
    pub(super) fn relay_swing(&mut self, attacker: GobId, target: GobId, chip: i32, dmg: i32) {
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
}
