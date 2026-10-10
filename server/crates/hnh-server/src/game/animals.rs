//! Tamed-animal AI, the quell/taming service, the production and
//! feeding sweep (session 47/48) and the animal damage/relay paths.

use super::*;

impl Game {
    /// Click on a fully tamed domestic producer (session 47; docs
    /// "Animal products and collection flows"): open the collection
    /// flower menu - Milk on a cow, Shear on a sheep. Returns true when
    /// the click is consumed (menu opened, or a hint chat for an empty
    /// meter); false when the click falls through to the fight path
    /// (wild, mid-taming, or non-producing animal). A fully tamed
    /// producer never opens a fight: tamed livestock cannot be aggroed.
    pub(super) fn open_animal_menu(
        &mut self,
        sid: SessionId,
        target: GobId,
        species: Species,
    ) -> bool {
        if !matches!(species, Species::Cow | Species::Sheep) {
            return false;
        }
        let Some(tame) = self.world.tamed.get(&target) else {
            return false;
        };
        if tame.tameness < crate::state::TAMENESS_FULL {
            return false;
        }
        let (option, hint) = match species {
            Species::Cow => {
                if tame.milk_units >= crate::state::MILK_PER_BUCKET_UNITS {
                    ("Milk", "")
                } else {
                    ("", "The cow has no milk yet.")
                }
            }
            _ => {
                if tame.wool > 0 {
                    ("Shear", "")
                } else {
                    ("", "The sheep has no wool to shear.")
                }
            }
        };
        if option.is_empty() {
            self.system_line(sid, hint);
            return true;
        }
        let Some(out) = self.sessions.get_mut(&sid) else {
            return true;
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
        out.animal_menu = Some((w, target));
        true
    }

    /// Flower-menu choice on a tamed producer (session 47). Milk draws
    /// one bucket: consumes an empty bucket (inventory first, then any
    /// equipment slot - the same any-slot policy as the crafting tool
    /// scan and the taming rope check), drains MILK_PER_BUCKET_UNITS
    /// from the cow and grants a bucket-milk item at the grazing quality
    /// (10). Shear collects the whole stored wool into the inventory.
    /// The meter is re-validated against live state: the menu can sit
    /// open while the meter drains or the animal dies.
    pub(super) fn apply_animal_choice(&mut self, sid: SessionId, wid: u16, choice: i32) {
        let pending = self
            .sessions
            .get(&sid)
            .and_then(|o| o.animal_menu)
            .filter(|(w, _)| *w == wid);
        let Some((_, gob)) = pending else {
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.animal_menu = None;
        out.send(wdg::dst_wdg(wid));
        if choice != 0 {
            out.send(wdg::wdgmsg(wid, "cancel", &[]));
            return;
        }
        out.send(wdg::wdgmsg(wid, "act", &[ListVal::I(0)]));
        // Re-validate (data phase): species, live meters.
        let Some(slot) = self.world.gobs.get(gob) else {
            return;
        };
        let Kind::Animal { species } = self.world.gobs.kind[slot] else {
            return;
        };
        let Some(tame) = self.world.tamed.get(&gob) else {
            return;
        };
        let (milk_units, wool) = (tame.milk_units, tame.wool);
        match species {
            Species::Cow => {
                if milk_units < crate::state::MILK_PER_BUCKET_UNITS {
                    self.system_line(sid, "The cow has no milk yet.");
                    return;
                }
                let Some(pidx) = self.world.by_session.get(&sid).copied() else {
                    return;
                };
                let buckete = self.world.res.intern("gfx/invobjs/buckete");
                let inv_has = self.world.players[pidx]
                    .inv
                    .iter()
                    .any(|s| s.res == buckete && s.count > 0);
                let equip_has = self.world.players[pidx]
                    .equip
                    .iter()
                    .flatten()
                    .any(|s| s.res == buckete && s.count > 0);
                if !inv_has && !equip_has {
                    self.system_line(sid, "You need an empty bucket to milk a cow.");
                    return;
                }
                // Mutate phase: drain the bucket (inventory first), the
                // meter, then grant the filled bucket.
                if inv_has {
                    let inv = &mut self.world.players[pidx].inv;
                    if let Some(s) = inv.iter_mut().find(|s| s.res == buckete && s.count > 0) {
                        s.count -= 1;
                    }
                    self.world.players[pidx].inv.retain(|s| s.count > 0);
                } else if let Some(e) = self.world.players[pidx]
                    .equip
                    .iter_mut()
                    .find(|e| matches!(e, Some(s) if s.res == buckete && s.count > 0))
                {
                    if let Some(s) = e.as_mut() {
                        s.count -= 1;
                        if s.count == 0 {
                            *e = None;
                        }
                    }
                }
                if let Some(tame) = self.world.tamed.get_mut(&gob) {
                    tame.milk_units -= crate::state::MILK_PER_BUCKET_UNITS;
                }
                self.refresh_inventory(sid);
                let milk_res = self.world.res.intern("gfx/invobjs/bucket-milk");
                self.grant_pickup(
                    sid,
                    InvStack {
                        res: milk_res,
                        count: 1,
                        ql: crate::state::GRAZE_PRODUCT_QL,
                        label: "",
                    },
                );
            }
            Species::Sheep => {
                if wool == 0 {
                    self.system_line(sid, "The sheep has no wool to shear.");
                    return;
                }
                if let Some(tame) = self.world.tamed.get_mut(&gob) {
                    let stored = tame.wool;
                    tame.wool = 0;
                    let wool_res = self.world.res.intern("gfx/invobjs/wool");
                    self.grant_pickup(
                        sid,
                        InvStack {
                            res: wool_res,
                            count: u32::from(stored),
                            ql: crate::state::GRAZE_PRODUCT_QL,
                            label: "",
                        },
                    );
                }
            }
            _ => {}
        }
    }

    /// Animal AI: parallel intent pass over the SoA columns (read-only),
    /// then serial application (writes stay on the game task). Intents are
    /// computed per grid-owner partition (grid_owner.rs) so the same pure
    /// function maps to true cross-process grid owners later.
    pub(super) fn tick_animals(&mut self) {
        let tick = self.world.tick;
        // Cluster: only cell-owned animals simulate here (foreign ones are
        // guests or other nodes' authority; transferred out on crossing).
        // Tamed animals skip AI entirely: the client renders the leash
        // (OD_FOLLOW), the beast holds position and never re-aggros while
        // the tame row lives AND stays leashed (session 83). A LOOSE row
        // (timer-based leash break, banked tameness) runs the wild AI
        // again - the beast panics/bites until the next quell re-leashes
        // it.
        let animal_ids: Vec<GobId> = self
            .world
            .animal_gobs
            .iter()
            .copied()
            .filter(|&id| match self.world.gobs.get(id) {
                Some(slot) => {
                    self.is_authority_slot(slot)
                        && !self.world.tamed.get(&id).map(|t| !t.loose).unwrap_or(false)
                }
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
        // Aggro leash (session 83): a chase NEVER outlives
        // AGGRO_GIVEUP subtiles of ground between the aggressor and its
        // home anchor, and a surrendered beast walks back before it may
        // re-aggro. Without this a boar that catches a tamer mid-approach
        // shadows the player forever (its 55 subt/s beats the 50 run
        // gait) and the one-Fightview rule then keeps the forced duel
        // alive for minutes - the live dairy probe never got its own
        // fight window back. The anchor is spawn-time state; old saves
        // and guest transfers default to "where I stand" (never leashed).
        let leashed = if species.aggressive() {
            let home = world.animal_home.get(id).copied().unwrap_or((ax, ay));
            let home_d = (ax - home.0).abs() + (ay - home.1).abs();
            home_d >= AGGRO_GIVEUP || world.animal_surrender.contains(id)
        } else {
            false
        };
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
            // The leash pass wins over a fresh aggro: a player stepping
            // into the surrender march does not restart the chase.
            _ if leashed => {
                let home = world.animal_home.get(id).copied().unwrap_or((ax, ay));
                AnimalAction::Return(home)
            }
            Some((pgob, dist)) if species.aggressive() && dist < aggro => AnimalAction::Chase(pgob),
            Some((pgob, dist)) if !species.aggressive() && dist < 200 => {
                // Directional panic (session 83): hop straight AWAY from
                // the threat. The old hop was a random ±550 scatter that
                // regularly bounded THROUGH the player and kept the
                // fight's swing cadence out of the 33-subtile reach for
                // the whole ~20 s hop - the live dairy probe could never
                // land a quell swing. A beast already IN a fight keeps
                // its hops tiny (the tamer's chase closes within a tick
                // or two); a grazing beast bounds away in longer hops
                // that a run/sprint player (50/66 vs the cow's 30) can
                // still run down.
                let (px, py) = world
                    .gobs
                    .get(pgob)
                    .map(|ps| world.gobs.pos[ps])
                    .unwrap_or((ax, ay));
                let (dx, dy) = (ax - px, ay - py);
                let len = (dx.abs() + dy.abs()).max(1);
                let in_fight = world.animal_fights.contains_key(id);
                let hop = if in_fight { 18 } else { 165 };
                // Deterministic jitter (the same (tick, slot) splitmix32
                // family as the wander branch) keeps the parallel pass
                // race-free; ±8 while fighting, ±40 at large.
                let h =
                    (tick as u32).wrapping_mul(0x9E3779B9) ^ (slot as u32).wrapping_mul(0x85EBCA6B);
                let (span, mid) = if in_fight { (17u32, 8) } else { (81u32, 40) };
                let j = ((h >> 8) % span) as i32 - mid;
                AnimalAction::Flee((dx * hop / len + j, dy * hop / len + j))
            }
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
            AnimalAction::Flee(step) => {
                // The pure intent pass already aimed this hop away from
                // the threat (small while fighting, long while grazing);
                // the serial phase only adds it to the current position.
                (sx + step.0, sy + step.1)
            }
            AnimalAction::Return((hx, hy)) => {
                // Aggro-leash surrender march (session 83). The
                // hysteresis set holds the beast non-aggressive for the
                // whole walk back; a duel it still holds is torn down
                // HERE - server side - so the player is released the
                // moment the chaser gives up (fight_del closes the
                // fightview; the one-Fightview gate then lets the
                // player's own click open their wanted fight again).
                self.world.animal_surrender.insert(id);
                let home_d = (sx - hx).abs() + (sy - hy).abs();
                if home_d <= crate::state::AGGRO_ARRIVED {
                    // Back inside the home circle: stand down, the set
                    // release re-arms normal aggro.
                    self.world.animal_surrender.remove(&id);
                    return;
                }
                let duel_live = self.world.animal_fights.remove(&id).is_some()
                    || self
                        .world
                        .players
                        .iter()
                        .any(|p| p.fight_target == Some(id));
                if duel_live {
                    // O(players) on a rare event; the DISENGAGE teardown
                    // is the same triple (target, row, widget).
                    if let Some(pidx) = self
                        .world
                        .players
                        .iter()
                        .position(|p| p.fight_target == Some(id))
                    {
                        let sid = self.world.players[pidx].session;
                        self.world.players[pidx].fight_target = None;
                        self.fight_del(sid, id);
                        info!(
                            target = id,
                            "aggro leash: the chase is surrendered, the duel torn down"
                        );
                    }
                }
                (hx, hy)
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

    pub(super) fn tick_combat(&mut self) {
        const REACH: i32 = 33; // ~3 tiles
        const DISENGAGE: i32 = 300;
        let tick = self.world.tick;

        // --- once-per-tick lookup indexes (session 43) ---
        // One O(players) pass fills the slot-indexed maps the melee paths
        // below used to replace with per-attacker / per-animal linear
        // scans (O(N^2) aggregate at the 1000-session load scale).
        // `resize` to 0 keeps capacity across ticks; only growth reallocs
        // (mem-reuse-collections).
        let t_ix = Instant::now();
        let nslots = self.world.gobs.alive.len();
        self.combat_ix.player_of_slot.clear();
        self.combat_ix.player_of_slot.resize(nslots, 0);
        self.combat_ix.engaged_of_slot.clear();
        self.combat_ix.engaged_of_slot.resize(nslots, 0);
        for (i, p) in self.world.players.iter().enumerate() {
            let Some(pslot) = self.world.gobs.get(p.gob) else {
                continue;
            };
            self.combat_ix.player_of_slot[pslot] = (i as u32) + 1;
            if let Some(t) = p.fight_target {
                if let Some(tslot) = self.world.gobs.get(t) {
                    // First (lowest) player index wins a shared target,
                    // matching the removed linear `find` semantics.
                    if self.combat_ix.engaged_of_slot[tslot] == 0 {
                        self.combat_ix.engaged_of_slot[tslot] = (i as u32) + 1;
                    }
                }
            }
        }
        let combat_index_us = t_ix.elapsed().as_micros() as u64;

        // --- battle-intensity de-escalation (session 46) ---
        // Jorb's quell prerequisite list (animals-and-husbandry.md) needs
        // "battle intensity reduced to 0": every combat tick without a
        // landed blow cools each fight a little. One O(fights) pass; the
        // per-blow raises live in the two bite/damage paths.
        for af in self.world.animal_fights.values_mut() {
            af.intensity = (af.intensity - crate::state::INTENSITY_DECAY).max(0);
        }

        // --- player side: offence gen, swings, bar streaming ---
        // Event counters split the phase cost inside the loop (the
        // session-43 load run measured an 18 ms player-phase mean; these
        // decide between chase starts, swing bookkeeping and the landed
        // hit tail: hurt + chat + FX + log).
        let mut chase_n = 0u64;
        let mut chase_us = 0u64;
        let mut swing_n = 0u64;
        let mut hit_n = 0u64;
        let mut hit_us = 0u64;
        let t_pl = Instant::now();
        'player: for pidx in 0..self.world.players.len() {
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
                        let t_c = Instant::now();
                        self.start_move(pslot, (tx, ty));
                        chase_us += t_c.elapsed().as_micros() as u64;
                        chase_n += 1;
                    }
                    continue;
                }
                // Every swing relays one (dmg, chip) pair: the owner
                // applies the chip to its authoritative bar and decides on
                // its own opening; landing locally is only UI prediction.
                // Session 83, guest ANIMAL: no selected attack, no swing
                // (the HnH Fightview queue rule; see the local animal
                // path below for the full rationale). Guest PLAYERS keep
                // the legacy bare-fisted auto swing (PvP model).
                let guest_is_player = matches!(guest.kind, crate::nodes::GuestKind::Player { .. });
                if !guest_is_player
                    && self
                        .sessions
                        .get(&sid)
                        .is_some_and(|o| o.fight.atk_cur.is_none())
                {
                    continue;
                }
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
                    // The attacker's equipped melee weapon replaces the
                    // unarmed model on the relay path too (game.rs
                    // melee_dmg: first weapon in the equipment slots).
                    let dmg = self.melee_dmg(pidx);
                    (dmg, def_chip)
                };
                self.world.players[pidx].stamina = (self.world.players[pidx].stamina - 2).max(0);
                if let Some(c) = self.cluster.as_ref() {
                    let (dmg, chip) = relay;
                    // Guest PLAYERS take the PvP melee path (session 39):
                    // the swing ships to the VICTIM'S HOME NODE (armor,
                    // HP and the knockout path live with the session,
                    // same authority split as PvpArrow) - not the cell
                    // owner, which is where a guest ANIMAL's bars live.
                    if guest_is_player {
                        let home = self.node_of_gob(target);
                        c.mesh.send(
                            home,
                            crate::nodes::NodeMsg::PvpSwing {
                                attacker: pgob,
                                victim: target,
                                chip,
                                dmg,
                            },
                        );
                    } else {
                        let authority = self.cell_owner(crate::visidx::cell_of(tx, ty));
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
                    let t_c = Instant::now();
                    let moved = self.start_move(pslot, (tx, ty));
                    chase_us += t_c.elapsed().as_micros() as u64;
                    chase_n += 1;
                    if !moved && self.world.tick.is_multiple_of(10) {
                        debug!(
                            sid,
                            target, px, py, tx, ty, "chase blocked: no clear path to the target"
                        );
                    }
                }
                continue;
            }
            // --- PvP melee (session 39): the target is another session ---
            // --- player. The openings economy runs against the VICTIM's ---
            // session defence bar (`own_def`) instead of an animal_fights
            // row: identical chip arithmetic and opening threshold, and a
            // landed hit goes through hurt_player (armor absorption, HP,
            // knockout) exactly like an animal bite. The victim's
            // automatic relation on the attacker (start_pvp_melee)
            // mirrors the pressure so their fight window shows the duel.
            if matches!(self.world.gobs.kind[tslot], Kind::Player { .. }) {
                // O(1) victim lookup through the slot index (was a linear
                // `players` scan per attacker per tick: O(N^2) aggregate).
                let vpidx = match self.combat_ix.player_of_slot[tslot] {
                    0 => {
                        self.world.players[pidx].fight_target = None;
                        self.fight_del(sid, target);
                        continue;
                    }
                    row => (row - 1) as usize,
                };
                let vsid = self.world.players[vpidx].session;
                // Attacker bar gen + swing decision (the same pacing as
                // the animal path); returns the swing payload when the
                // offence bar covered a swing this tick.
                swing_n += 1;
                let swung = {
                    let Some(out) = self.sessions.get_mut(&sid) else {
                        continue;
                    };
                    out.fight.own_off =
                        (out.fight.own_off + crate::fight::OFF_REGEN).min(crate::fight::BAR_FULL);
                    if out.fight.atkc > 0 {
                        out.fight.atkc -= 1;
                    }
                    if out.fight.own_off < crate::fight::SWING_SPEND || out.fight.atkc > 0 {
                        None
                    } else {
                        out.fight.own_off -= crate::fight::SWING_SPEND;
                        out.fight.atkc = crate::fight::ATKC_TICKS;
                        // Attack weight scales 0.5..2.0 with advantage
                        // (identical to the animal swing path); read the
                        // balance immutably, bump IP mutably, then read
                        // the spent bar - three disjoint borrows.
                        let weight = out
                            .fight
                            .rel(target)
                            .map(|rel| (rel.balance.clamp(-5, 5) as f32) * 0.1 + 1.0)
                            .unwrap_or(1.0);
                        if let Some(rel) = out.fight.rel_mut(target) {
                            rel.ip_self += 1;
                        }
                        let def_chip = (crate::fight::SWING_DEF_DMG as f32 * weight) as i32;
                        Some((def_chip, out.fight.own_off))
                    }
                };
                let Some((def_chip, off_now)) = swung else {
                    continue;
                };
                self.world.players[pidx].stamina = (self.world.players[pidx].stamina - 2).max(0);
                let dmg = self.melee_dmg(pidx);
                // Chip the victim's session defence bar; an opening
                // (below threshold) passes the damage through and resets
                // the bar to full, mirroring the animal-bite policy.
                let (landed, victim_def_after) = {
                    let Some(vout) = self.sessions.get_mut(&vsid) else {
                        continue;
                    };
                    let breaking = vout.fight.own_def <= crate::fight::OPENING_THRESHOLD;
                    vout.fight.own_def = (vout.fight.own_def - def_chip).max(0);
                    let landed = breaking || vout.fight.own_def <= crate::fight::OPENING_THRESHOLD;
                    if landed {
                        vout.fight.own_def = crate::fight::BAR_FULL;
                    }
                    let after = vout.fight.own_def;
                    // Mirror the attacker's pressure into the victim's
                    // relation view (their window shows the duel live).
                    if let Some(rel) = vout.fight.rel_mut(pgob) {
                        rel.ip_other += 1;
                        rel.offence = off_now;
                    }
                    (landed, after)
                };
                // The attacker's own view of the victim's defence
                // (victim_def_after was captured before this mutable
                // re-borrow of the attacker's session).
                let vdef = victim_def_after;
                if let Some(out) = self.sessions.get_mut(&sid) {
                    if let Some(rel) = out.fight.rel_mut(target) {
                        rel.defence = vdef;
                    }
                }
                if landed {
                    hit_n += 1;
                    let t_h = Instant::now();
                    let knocked = self.hurt_player(vpidx, dmg, pgob);
                    let vname = self.world.players[vpidx].name.clone();
                    let aname = self.world.players[pidx].name.clone();
                    // Session 44 hit-tail trim: the per-hit info log was a
                    // measured chunk of the 1.3-3.6 ms hit tail at the
                    // 1000-dueler scale (25-40 hits/s saturate the
                    // tracing pipeline). Per-hit detail drops to debug;
                    // the aggregate below keeps the operational signal.
                    debug!(sid, vsid, target, dmg, knocked, "pvp melee hit");
                    self.chat_line(
                        sid,
                        &format!("You hit {vname} for {dmg} damage."),
                        Some((192, 255, 192)),
                    );
                    self.chat_line(
                        vsid,
                        &format!("{aname} hits you for {dmg} damage."),
                        Some((255, 128, 128)),
                    );
                    self.fx_overlay_broadcast(target, "gfx/fx/hit");
                    if knocked {
                        self.chat_line(
                            sid,
                            "You have defeated your target!",
                            Some((192, 255, 192)),
                        );
                        // PvP knockout consequences (server policy): the
                        // loser forfeits 10% unused LP, the winner takes
                        // the criminal flag (combat-system.md).
                        self.knockout_lp_loss(vpidx);
                        self.flag_criminal(pidx);
                        // Teardown on the attacker's side; hurt_player
                        // already reset the victim (hp, rels, target).
                        self.world.players[pidx].fight_target = None;
                        self.fight_del(sid, target);
                    }
                    hit_us += t_h.elapsed().as_micros() as u64;
                }
                continue;
            }
            // Bar updates and swing decision inside a tight scope, so the
            // session borrow is dropped before any self-facing call.
            // Returns (quell?, damage-swing?); None = no action this tick.
            let action = {
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
                // Session 83: a swing against an ANIMAL needs a selected
                // attack (the HnH Fightview queue rule: no queued attack,
                // no damage swing - the player stands guard). The live
                // taming trace showed why: after a REFUSED quell ("too
                // heated", atk_cur stays None) the cadence happily fired
                // ~50 free auto-swings over the 40 s wait and killed the
                // aurochs long before any quell could resolve. PvP melee
                // keeps the legacy auto-swing (the load duelers and the
                // melee probe both model bare-fisted auto attacks).
                if out.fight.atk_cur.is_none() {
                    continue;
                }
                // Swing: spend offence, chip defence, land damage on an opening.
                out.fight.own_off -= crate::fight::SWING_SPEND;
                out.fight.atkc = crate::fight::ATKC_TICKS;
                // The SELECTED attack resolves: Quell the Beast turns the
                // swing into a taming attempt instead of a damage swing
                // (the IP/adv/rope gates ran at selection time, so the
                // attempt is valid here by construction).
                let quell = out.fight.atk_cur == Some("paginae/atk/quell");
                if quell {
                    debug!(sid, target, "quell swing resolves");
                }
                if quell {
                    Some((true, None))
                } else {
                    let Some(rel) = out.fight.rel_mut(target) else {
                        continue;
                    };
                    rel.ip_self += 1;
                    // Attack weight scales 0.5..2.0 with advantage (balance).
                    let weight = (rel.balance.clamp(-5, 5) as f32) * 0.1 + 1.0;
                    let def_chip = (crate::fight::SWING_DEF_DMG as f32 * weight) as i32;
                    // Chip the animal's defence in the World store (the mirror
                    // source); rel.defence streams it to the client. A landed
                    // swing also heats the battle (quell needs a calm beast).
                    let (_, landed) = {
                        let Some(af) = self.world.animal_fights.get_mut(&target) else {
                            continue;
                        };
                        let breaking = af.def <= crate::fight::OPENING_THRESHOLD;
                        af.def = (af.def - def_chip).max(0);
                        let landed = breaking || af.def <= crate::fight::OPENING_THRESHOLD;
                        if landed {
                            af.def = crate::fight::BAR_FULL;
                            af.intensity = (af.intensity + crate::state::INTENSITY_PER_BLOW)
                                .min(crate::fight::BAR_FULL);
                        }
                        (breaking, landed)
                    };
                    let swing = if landed {
                        Some(self.melee_dmg(pidx))
                    } else {
                        None
                    };
                    Some((false, swing))
                }
            };
            let Some((quell, swing)) = action else {
                continue;
            };
            self.world.players[pidx].stamina = (self.world.players[pidx].stamina - 2).max(0);
            if quell {
                self.apply_quell(pidx, sid, target);
            } else if let Some(dmg) = swing {
                self.damage_animal(pidx, sid, target, tslot, dmg);
            }
        }

        let combat_players_us = t_pl.elapsed().as_micros() as u64;

        // --- animal side: aggressive animals swing back ---
        // Snapshot the engaged animals in `animal_gobs` order (rows are
        // removed mid-loop by knockout deaths, so a copy is required);
        // the old loop cloned the WHOLE animal list and linear-scanned
        // players per animal (O(animals x players) per tick) - the slot
        // index answers the engagement in O(1). A victim knocked out by
        // the player pass above keeps a stale row here: the live
        // `fight_target` re-check below reproduces the removed `find`
        // semantics exactly (perf-coll, mem-reuse-collections).
        let combat_animals_us;
        {
            let t_an = Instant::now();
            let mut engaged_animals = std::mem::take(&mut self.combat_ix.engaged_animals);
            engaged_animals.clear();
            for &id in self.world.animal_gobs.iter() {
                let Some(aslot) = self.world.gobs.get(id) else {
                    continue;
                };
                if self.combat_ix.engaged_of_slot[aslot] != 0 {
                    engaged_animals.push(id);
                }
            }
            for id in engaged_animals.iter().copied() {
                let Some(slot) = self.world.gobs.get(id) else {
                    continue;
                };
                let Kind::Animal { species } = self.world.gobs.kind[slot] else {
                    continue;
                };
                let _ = species;
                // Find the engaged player and re-check reach. The live
                // fight_target comparison keeps the first-engaged-player
                // semantics of the removed linear scan.
                let Some(row) = self.combat_ix.engaged_of_slot[slot].checked_sub(1) else {
                    continue;
                };
                let pidx = row as usize;
                if self.world.players[pidx].fight_target != Some(id) {
                    continue;
                }
                let p_sid = self.world.players[pidx].session;
                let p_gob = self.world.players[pidx].gob;
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
                // The defence bar to write back after this tick (None = no
                // swing): the chip must ACCUMULATE across bites until an
                // opening, not reset implicitly - the local `new_def` used
                // to be dropped, which only worked because fresh sessions
                // started at own_def = 0 (fixed in session 39: FightState::new
                // starts the defence FULL).
                let mut next_def: Option<i32> = None;
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
                        next_def = Some(new_def);
                    }
                }
                if let Some(dmg) = bite {
                    // The beast's landed bite heats the battle too.
                    if let Some(af) = self.world.animal_fights.get_mut(&id) {
                        af.intensity = (af.intensity + crate::state::INTENSITY_PER_BLOW)
                            .min(crate::fight::BAR_FULL);
                    }
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
                    if let Some(nd) = next_def {
                        out.fight.own_def = nd;
                    }
                    if bite.is_some() {
                        out.fight.own_def = crate::fight::BAR_FULL;
                    }
                }
            }
            combat_animals_us = t_an.elapsed().as_micros() as u64;
            self.combat_ix.engaged_animals = engaged_animals;
        }

        // --- relay retaliation: animals strike back at guest players ---
        // The attacker is a session player homed on ANOTHER node (it
        // renders here as a published guest): no session, no defence bar,
        // no armor table locally. The bite therefore just SHIPS to the
        // attacker's home node, where hurt_player applies absorption,
        // HP, stamina and the knockout path. v1 bite uses the default
        // str (same value the local path computes for str 10).
        // Snapshot in scratch (rows mutate mid-loop); same iteration
        // order the collected Vec had (mem-reuse-collections).
        let t_re = Instant::now();
        let mut relay_rows = std::mem::take(&mut self.combat_ix.relay_rows);
        relay_rows.clear();
        relay_rows.extend(self.world.guest_attackers.iter().map(|(&a, &p)| (a, p)));
        for (id, attacker) in relay_rows.iter().copied() {
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
        let combat_relay_us = t_re.elapsed().as_micros() as u64;
        self.combat_ix.relay_rows = relay_rows;

        // --- fast bar streaming: updod per relation + offdef, every 2 ticks ---
        if tick.is_multiple_of(2) {
            let mut sids = std::mem::take(&mut self.combat_ix.bar_sids);
            sids.clear();
            sids.extend(self.sessions.keys().copied());
            for sid in &sids {
                let Some(out) = self.sessions.get_mut(sid) else {
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
            self.combat_ix.bar_sids = sids;
        }

        // Session-43 sub-attribution: combat p95 spikes were the top NEXT
        // target; these counters decide where the next cut goes.
        let perf = &mut self.world.perf;
        perf.combat_index_us = combat_index_us;
        perf.combat_players_us = combat_players_us;
        perf.combat_animals_us = combat_animals_us;
        perf.combat_relay_us = combat_relay_us;
        perf.combat_chase_n = chase_n;
        perf.combat_chase_us = chase_us;
        perf.combat_swing_n = swing_n;
        perf.combat_hit_n = hit_n;
        perf.combat_hit_us = hit_us;
        // Session 44 hit-tail trim: the operational hit signal as a
        // 5-second aggregate (50 ticks) instead of a per-hit info line
        // (the per-hit log was a measured chunk of the hit tail).
        if hit_n > 0 && self.world.tick.is_multiple_of(50) {
            let mean_us = perf.combat_hit_us / hit_n.max(1);
            info!(
                hits = hit_n,
                mean_hit_us = mean_us,
                "pvp hits (5s aggregate)"
            );
        }
    }

    // ------------------------------------------------------------------
    // Taming (session 45; animals-and-husbandry.md taming service)
    // ------------------------------------------------------------------

    /// Quell target gate: local animal, Animal Husbandry skill, rope
    /// equipped, a calm battle (intensity 0), this tamer's rope not
    /// already bound to a partially-tamed beast, and the beast not
    /// already tamed. Returns the refusal reason or None.
    pub(super) fn quell_gate(&mut self, pidx: usize, target: GobId) -> Option<String> {
        let why = self.quell_gate_inner(pidx, target);
        debug!(
            pidx,
            target,
            refused = why.is_some(),
            reason = why.as_deref().unwrap_or(""),
            "quell gate"
        );
        why
    }

    fn quell_gate_inner(&mut self, pidx: usize, target: GobId) -> Option<String> {
        let tslot = self.world.gobs.get(target)?;
        if !matches!(self.world.gobs.kind[tslot], Kind::Animal { .. }) {
            return Some("You can only quell an animal.".to_owned());
        }
        // Docs step 1: the tamer needs the Animal Husbandry skill.
        if !crate::skills::can_quell(&self.world.players[pidx].skills) {
            return Some("You need the Animal Husbandry skill to quell a beast.".to_owned());
        }
        // Cross-node guest animals keep their authority on the owner node:
        // the MVP resolves quells on the local authority only.
        if self.world.guests.contains_key(&target) {
            return Some("That beast is beyond your rope's reach.".to_owned());
        }
        if let Some(tame) = self.world.tamed.get(&target) {
            if !tame.loose {
                return Some("That beast is already quelled.".to_owned());
            }
            // A LOOSE row is banked tameness on a wild-again beast: the
            // docs' five-cycle protocol re-quells the SAME beast ("a
            // quelled beast re-aggros on its own ... quell again"), so
            // the gate passes and apply_quell re-leashes it.
        }
        if !self.rope_equipped(pidx) {
            return Some("You need a rope equipped to quell a beast.".to_owned());
        }
        // Docs step 2 (Jorb's list): battle intensity reduced to 0. A hot
        // fight must cool down first - stop swinging and wait.
        if let Some(af) = self.world.animal_fights.get(&target) {
            if af.intensity > 0 {
                return Some("The battle is too heated for the beast to quell.".to_owned());
            }
        }
        // The rope binds to one animal until it turns hostile again
        // (tameness reaches full = permanently tame, binding ends). A
        // LOOSE beast frees the rope (docs step 4: "until it is tamed or
        // breaks loose") - only on-leash mid-taming rows bind.
        let my_gob = self.world.players[pidx].gob;
        let bound = self
            .world
            .tamed
            .values()
            .any(|t| !t.loose && t.tamer == my_gob && t.tameness < crate::state::TAMENESS_FULL);
        if bound {
            return Some(
                "Your rope is bound to another beast until it is tamed or breaks loose.".to_owned(),
            );
        }
        None
    }

    /// True when a rope (gfx/invobjs/rope) sits in any equipment slot.
    /// The docs name "a Rope equipped as the weapon" - any equip slot
    /// accepts it for now (weapon-slot-only is a documented NEXT check).
    fn rope_equipped(&mut self, pidx: usize) -> bool {
        const ROPE: &str = "gfx/invobjs/rope";
        let rope_gidx = self.world.res.intern(ROPE);
        self.world.players[pidx]
            .equip
            .iter()
            .flatten()
            .any(|s| s.res == rope_gidx)
    }

    /// One successful Quell: +20 tameness, the battle ends, the beast
    /// follows the tamer (leashed), the rope binds, and the 10-minute
    /// leash-break timer starts (docs steps 3-5). At 100 tameness the
    /// animal is permanently tame and never breaks loose again.
    pub(super) fn apply_quell(&mut self, pidx: usize, sid: SessionId, target: GobId) {
        debug!(sid, target, "apply quell");
        let tamer_gob = self.world.players[pidx].gob;
        // End the battle: the beast stops biting (out of animal_fights).
        self.world.animal_fights.remove(&target);
        // Accumulate tameness. A LOOSE row (banked progress after a
        // timer-based leash break) re-leashes in place: the entry survives
        // the break, so the five cycles stack 20 -> 40 -> ... -> 100 on
        // the SAME beast (session 83; docs step 5 "quell again").
        let (tameness, full) = {
            let entry = self
                .world
                .tamed
                .entry(target)
                .or_insert_with(|| crate::state::TameState::new(tamer_gob, 0));
            entry.tamer = tamer_gob;
            entry.loose = false;
            entry.tameness = (entry.tameness + crate::state::TAMENESS_PER_QUELL)
                .min(crate::state::TAMENESS_FULL);
            // The leash-break deadline: ~10 minutes from NOW on every
            // quell below full (docs step 5; game-time based). The
            // HNH_LEASH_TICKS dev knob shrinks the live-verification
            // spacing (see state::leash_break_ticks).
            entry.break_at_tick = self.world.tick + crate::state::leash_break_ticks();
            if entry.tameness >= crate::state::TAMENESS_FULL {
                entry.break_at_tick = 0;
                (entry.tameness, true)
            } else {
                (entry.tameness, false)
            }
        };
        // The leash: client-side following (OCache OD_FOLLOW -> Following).
        self.stream_follow(target, tamer_gob);
        // Clean up the session-side fight (bars + window).
        self.fight_del(sid, target);
        self.world.players[pidx].fight_target = None;
        let label = match self
            .world
            .gobs
            .get(target)
            .map(|s| &self.world.gobs.kind[s])
        {
            Some(Kind::Animal { species }) => format!("{species:?}"),
            _ => "beast".to_owned(),
        };
        self.chat_line(
            sid,
            &format!("You quell the {label}. Tameness: {tameness}/100."),
            Some((255, 255, 128)),
        );
        if full {
            // Docs step 6: at 100 tameness the animal "metamorphoses" in
            // place into its domestic morph, still following the tamer.
            let morphed = self.apply_species_morph(target).is_some();
            self.chat_line(
                sid,
                if morphed {
                    "The beast settles into its domestic form."
                } else {
                    "The beast is fully tamed and stays by your side."
                },
                Some((255, 255, 128)),
            );
        }
    }

    /// Swap one animal's species at full tameness (session 46): rewrite
    /// the Kind, the drawable resource and the vitals, then broadcast
    /// OD_RES so every viewer re-renders the sprite in place (the client
    /// path is OCache.cres -> ResDrawable reset; Session.java OD_RES = 2).
    /// Returns the new species, or None when the species does not morph
    /// (the 2009 pack ships no pig drawable - documented policy).
    fn apply_species_morph(&mut self, id: GobId) -> Option<crate::state::Species> {
        let slot = self.world.gobs.get(id)?;
        let Kind::Animal { species } = self.world.gobs.kind[slot] else {
            return None;
        };
        let new_species = species.morph()?;
        let res_idx = self.world.res.intern(new_species.resname());
        self.world.gobs.kind[slot] = Kind::Animal {
            species: new_species,
        };
        self.world.gobs.res_idx[slot] = res_idx;
        self.world.gobs.max_hp[slot] = new_species.max_hp();
        self.world.gobs.hp[slot] = self.world.gobs.hp[slot].min(new_species.max_hp());
        self.world.gobs.speed[slot] = new_species.speed();
        // OD_RES re-render for every viewer through the packed start
        // batch: one encoded block, the wire id patched per session
        // (same pattern as the FX overlay fan-out).
        let frame = self.world.gobs.frame[slot];
        let (px, py) = self.world.gobs.pos[slot];
        // Headerless block: [fl][id(4)][frame(4)][OD_RES][wire(2) <- patch][OD_END].
        let patch_off = 1 + 4 + 4 + 1;
        let mut m = MessageBuf::new();
        m.uint8(0)
            .int32(id)
            .int32(frame as i32)
            .uint8(OD_RES)
            .uint16(res_idx)
            .uint8(OD_END);
        self.start_scratch.push_patched(
            id,
            frame,
            crate::visidx::cell_of(px, py),
            true,
            Some(crate::move_batch::Patch::One {
                slot: [(res_idx, patch_off)],
            }),
            &m.finish(),
        );
        self.world.perf.fx_batch_n += 1;
        Some(new_species)
    }

    /// Broadcast an OD_FOLLOW block for one gob (leash rendering). Gob
    /// ids are global (not session-relative wire ids), so the encoded
    /// block needs no per-session patching - fan out through the packed
    /// start batch like a one-shot FX (Session 44 batched fan-out).
    fn stream_follow(&mut self, id: GobId, target: GobId) {
        let Some(slot) = self.world.gobs.get(id) else {
            return;
        };
        let frame = self.world.gobs.frame[slot] as i32;
        let (px, py) = self.world.gobs.pos[slot];
        let mut m = MessageBuf::new();
        m.uint8(0)
            .int32(id)
            .int32(frame)
            .uint8(OD_FOLLOW)
            .int32(target)
            .int8(0)
            .int32(0)
            .int32(0)
            .uint8(OD_END);
        self.start_scratch.push(
            id,
            self.world.gobs.frame[slot],
            crate::visidx::cell_of(px, py),
            true,
            &m.finish(),
        );
        self.world.perf.fx_batch_n += 1;
    }

    /// Broadcast the follow REMOVAL (oid = -1: delattr client-side).
    pub(super) fn stream_follow_off(&mut self, id: GobId) {
        let Some(slot) = self.world.gobs.get(id) else {
            return;
        };
        let frame = self.world.gobs.frame[slot] as i32;
        let (px, py) = self.world.gobs.pos[slot];
        let mut m = MessageBuf::new();
        m.uint8(0)
            .int32(id)
            .int32(frame)
            .uint8(OD_FOLLOW)
            .int32(-1)
            .uint8(OD_END);
        self.start_scratch.push(
            id,
            self.world.gobs.frame[slot],
            crate::visidx::cell_of(px, py),
            true,
            &m.finish(),
        );
        self.world.perf.fx_batch_n += 1;
    }

    /// Starvation death of a fully tamed producer (session 48 policy):
    /// the animal despawns without a corpse or loot (the corpse
    /// pipeline is not implemented - documented in the livestock doc),
    /// the tame row drops, and the tamer is chatted when online.
    pub(super) fn starve_kill(&mut self, target: GobId) {
        let Some(tame) = self.world.tamed.remove(&target) else {
            return;
        };
        let label = match self
            .world
            .gobs
            .get(target)
            .map(|slot| self.world.gobs.kind[slot])
        {
            Some(Kind::Animal {
                species: Species::Cow,
            }) => "cow",
            Some(Kind::Animal {
                species: Species::Sheep,
            }) => "sheep",
            _ => "animal",
        };
        self.world.gobs.kill(target);
        self.broadcast_retract(target);
        self.world.animal_gobs.retain(|&g| g != target);
        self.world.animal_fights.remove(&target);
        self.world.animal_home.remove(&target);
        self.world.animal_surrender.remove(&target);
        self.world.guest_attackers.remove(&target);
        if let Some(pidx) = self.world.players.iter().position(|p| p.gob == tame.tamer) {
            let sid = self.world.players[pidx].session;
            self.chat_line(
                sid,
                &format!("Your {label} has starved to death."),
                Some((255, 128, 128)),
            );
        }
        info!(gob = target, label, "animal starved to death");
    }

    /// Break a leash: the beast re-aggros, the follow ends, the rope
    /// frees (docs step 5). Chat the tamer when online.
    pub(super) fn break_leash(&mut self, target: GobId, reason: &str) {
        let Some(tame) = self.world.tamed.remove(&target) else {
            return;
        };
        self.stream_follow_off(target);
        let tamer_pidx = self.world.players.iter().position(|p| p.gob == tame.tamer);
        if let Some(pidx) = tamer_pidx {
            let sid = self.world.players[pidx].session;
            self.chat_line(
                sid,
                &format!("The beast breaks its leash {reason}!"),
                Some((255, 128, 128)),
            );
        }
    }

    /// Timer-based leash break (session 83): the docs model it as "a
    /// tameness-decay timer that re-enables the animal's hostile state
    /// unless quelled again" - the beast goes wild again (wild AI, follow
    /// off, rope free) but the accumulated tameness stays BANKED in the
    /// row. The next quell re-leashes the beast and adds on top; only a
    /// damage-based break (break_leash) wipes the progress. Fully tamed
    /// beasts never reach here (break_at_tick stays 0).
    pub(super) fn leash_expire(&mut self, target: GobId) {
        let (tamer, tameness) = {
            let Some(tame) = self.world.tamed.get_mut(&target) else {
                return;
            };
            if tame.tameness >= crate::state::TAMENESS_FULL {
                return;
            }
            tame.loose = true;
            (tame.tamer, tame.tameness)
        };
        self.stream_follow_off(target);
        let tamer_pidx = self.world.players.iter().position(|p| p.gob == tamer);
        if let Some(pidx) = tamer_pidx {
            let sid = self.world.players[pidx].session;
            debug!(?target, tameness, "leash expired; tameness banked");
            self.chat_line(
                sid,
                "The beast breaks its leash and re-attacks!",
                Some((255, 128, 128)),
            );
        }
    }

    pub(super) fn damage_animal(
        &mut self,
        pidx: usize,
        sid: SessionId,
        target: GobId,
        tslot: usize,
        dmg: i32,
    ) {
        // Docs step 5: hitting or damaging the beast removes the tameness
        // gained (server policy: ALL of it - the beast shakes the leash
        // and re-aggros). Runs before the normal damage path so the row
        // is gone before any death handling.
        if self.world.tamed.contains_key(&target) {
            self.break_leash(target, "as your blow lands");
        }
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
                Self::record_unacked(out, target, frame, b, false);
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
            self.world.animal_home.remove(&target);
            self.world.animal_surrender.remove(&target);
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

    /// HP damage to a relay-fought animal (authority side): streams
    /// OD_HEALTH to local viewers, publishes the guest update (hp rides
    /// the GuestState to the attacker's node), and on death drops loot,
    /// retracts, and credits the attacker's home node with the LP. The
    /// tail of `damage_animal` minus the session-facing fight UI, which
    /// lives on the attacker's node.
    pub(super) fn damage_animal_relayed(&mut self, target: GobId, tslot: usize, dmg: i32) {
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
                Self::record_unacked(out, target, frame, b, false);
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
            self.world.animal_home.remove(&target);
            self.world.animal_surrender.remove(&target);
            self.world.guest_attackers.remove(&target);
            for (res, count, label) in species.loot() {
                for _ in 0..count {
                    self.spawn_drop_near(pos, res, 10, label);
                }
            }
            info!(target, ?species, "relay-killed animal");
        }
    }
}
