//! Combat: the Fightview openings duel (animal fights + PvP melee),
//! bow archery (aim meter, arrow release), the frv widget protocol
//! and the PvP consequences (armor totals, damage, knockout LP
//! loss, criminal flag) - combat-system.md.
//!
//! Pure move out of game.rs (session 49 split continued).

use super::*;

impl Game {
    pub(super) fn start_fight(&mut self, sid: SessionId, target: GobId, species: Species) {
        // One Fightview per player (docs combat-system.md): an engaged
        // player never opens ANOTHER duel - not on a repeat click on the
        // same beast, and not when an aggressive chaser (boar/wolf)
        // reaches swing reach mid-taming. Session 83 live trace: an
        // aggroed boar re-entered start_fight on EVERY combat tick,
        // re-opening the frv widget ten times a second and stealing
        // fight_target away from the beast being quelled, so the taming
        // protocol never had a stable fight to talk to ("the fight
        // never stayed open"). The fight lives until DISENGAGE, the
        // target's death or the player's exit - all of which already
        // reset fight_target.
        if self
            .world
            .player(sid)
            .is_some_and(|p| p.fight_target.is_some())
        {
            return;
        }
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
                    intensity: 0,
                    idle_ticks: 0,
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
                intensity: 0,
                idle_ticks: 0,
            });
        info!(sid, target, ?species, "fight started");
    }

    /// Open the unarmed melee duel on another PLAYER (session 39 PvP).
    /// The attacker's offence bar and the fight UI stay LOCAL (session
    /// state, exactly like the animal fight); the victim's defence bar
    /// is the victim session's `own_def` when both players are local,
    /// or lives on the victim's home node when the target is a guest
    /// (each swing relays a PvpSwing there and the PvpSwingResult
    /// answer re-syncs the local mirror).
    pub(super) fn start_pvp_melee(&mut self, sid: SessionId, target: GobId) {
        // Self-clicks never engage (the menu guard already refuses them,
        // this is the belt-and-braces path for direct callers).
        if self.world.player(sid).map(|p| p.gob) == Some(target) {
            return;
        }
        // Melee and ranged are exclusive player state; drop any live aim.
        if let Some(p) = self.world.player_mut(sid) {
            p.aim = None;
        }
        // Cross-node guest player: mirror the animal relay-fight setup.
        if self.world.guests.contains_key(&target) {
            let vname = self
                .world
                .guests
                .get(&target)
                .and_then(|g| g.kind.player_name())
                .unwrap_or("someone")
                .to_owned();
            if let Some(p) = self.world.player_mut(sid) {
                p.fight_target = Some(target);
                p.atk_cd = 0;
            }
            self.fight_open(sid, target);
            self.world
                .guest_fights
                .entry(target)
                .or_insert_with(|| crate::state::AnimalFight {
                    off: 0,
                    def: crate::fight::BAR_FULL,
                    intensity: 0,
                    idle_ticks: 0,
                });
            self.chat_line(sid, &format!("You attack {vname}!"), Some((255, 200, 128)));
            info!(sid, target, "pvp relay duel started");
            return;
        }
        // Local player target: open the duel on BOTH sides - the victim
        // gets a relation on the attacker immediately (legacy Fightview
        // opens both ways) so they can select the attacker in the fight
        // window and answer without hunting for the flower menu first.
        let Some(vpidx) = self.world.players.iter().position(|p| p.gob == target) else {
            return;
        };
        let vsid = self.world.players[vpidx].session;
        let vname = self.world.players[vpidx].name.clone();
        let agob = self.world.player(sid).map(|p| p.gob);
        let aname = self.world.player(sid).map(|p| p.name.clone());
        let Some(agob) = agob else { return };
        if let Some(p) = self.world.player_mut(sid) {
            p.fight_target = Some(target);
            p.atk_cd = 0;
        }
        self.fight_open(sid, target);
        self.fight_open(vsid, agob);
        self.chat_line(sid, &format!("You attack {vname}!"), Some((255, 200, 128)));
        if let Some(aname) = aname {
            self.chat_line(
                vsid,
                &format!("{aname} attacks you!"),
                Some((255, 128, 128)),
            );
        }
        info!(sid, target, vsid, "pvp melee duel started");
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
    pub(super) fn start_aim(&mut self, sid: SessionId, target: GobId) -> bool {
        // PvP guard (session 38): never aim at yourself - the click
        // falls through to the caller's default path (the party menu).
        if self.world.player(sid).map(|p| p.gob) == Some(target) {
            return false;
        }
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
    pub(super) fn tick_aim(
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
    pub(super) fn shoot_arrow(
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
        // Session 38: player targets (local Kind::Player, guest
        // GuestKind::Player) resolve to the PvP path - the victim's
        // armor/HP/knockout live on ITS home node (hurt_player), so a
        // cross-node hit rides a PvpArrow there.
        enum ShotTarget {
            Animal {
                species: Species,
                slot: Option<usize>,
            },
            Player {
                name: String,
                pidx: Option<usize>,
            },
        }
        let guest = self.world.guests.get(&target).cloned();
        let (tgt, tpos) = match &guest {
            Some(g) => match &g.kind {
                crate::nodes::GuestKind::Animal { species } => {
                    match Species::from_index(*species) {
                        Some(sp) => (
                            ShotTarget::Animal {
                                species: sp,
                                slot: None,
                            },
                            g.pos,
                        ),
                        None => {
                            self.world.players[pidx].aim = None;
                            return;
                        }
                    }
                }
                crate::nodes::GuestKind::Player { name, .. } => (
                    ShotTarget::Player {
                        name: name.clone(),
                        pidx: None,
                    },
                    g.pos,
                ),
                _ => {
                    self.world.players[pidx].aim = None;
                    return;
                }
            },
            None => match self.world.gobs.get(target) {
                Some(s) => match self.world.gobs.kind[s] {
                    crate::state::Kind::Animal { species } => (
                        ShotTarget::Animal {
                            species,
                            slot: Some(s),
                        },
                        self.world.gobs.pos[s],
                    ),
                    crate::state::Kind::Player { player } => (
                        ShotTarget::Player {
                            name: self
                                .world
                                .players
                                .get(player)
                                .map(|p| p.name.clone())
                                .unwrap_or_default(),
                            pidx: Some(player),
                        },
                        self.world.gobs.pos[s],
                    ),
                    _ => {
                        self.world.players[pidx].aim = None;
                        return;
                    }
                },
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
            // Destructure the target into plain data first: the
            // damage paths below need &mut self.
            let (is_player, tname, vidx) = match &tgt {
                ShotTarget::Animal { species, .. } => (false, species.name().to_string(), None),
                ShotTarget::Player { name, pidx, .. } => (true, name.clone(), *pidx),
            };
            match vidx {
                // LOCAL PvP shot: armor absorption, HP and the
                // knockout path all live on this node.
                Some(vidx) => {
                    let vsid = self.world.players[vidx].session;
                    let knocked = self.hurt_player(vidx, dmg, self.world.players[pidx].gob);
                    self.fx_overlay_broadcast(target, "gfx/fx/hit");
                    self.chat_line(
                        vsid,
                        &format!("An arrow hits you for {dmg} damage."),
                        Some((255, 128, 128)),
                    );
                    self.chat_line(
                        sid,
                        &format!("Your arrow hits {tname} for {dmg} damage."),
                        Some((192, 255, 192)),
                    );
                    if knocked {
                        self.chat_line(
                            sid,
                            &format!("You have defeated {tname}!"),
                            Some((192, 255, 192)),
                        );
                        // PvP knockout consequences (server policy):
                        // LP loss on the loser, criminal flag on the
                        // winner (combat-system.md).
                        self.knockout_lp_loss(vidx);
                        self.flag_criminal(pidx);
                    }
                }
                None if is_player => {
                    // CROSS-NODE PvP shot: the hit roll already happened
                    // here; the victim's armor/HP/knockout live on ITS
                    // home node (node_of_gob from the gob id).
                    self.chat_line(
                        sid,
                        &format!("Your arrow hits {tname} for {dmg} damage."),
                        Some((192, 255, 192)),
                    );
                    if let Some(c) = self.cluster.as_ref() {
                        let home = self.node_of_gob(target);
                        c.mesh.send(
                            home,
                            crate::nodes::NodeMsg::PvpArrow {
                                victim: target,
                                attacker: self.world.players[pidx].gob,
                                dmg,
                            },
                        );
                    }
                }
                _ => {
                    // Animal target. Cross-node shot: the authority
                    // applies the damage (chip 0 marks the ranged
                    // bypass).
                    self.chat_line(
                        sid,
                        &format!("Your arrow hits the {tname} for {dmg} damage."),
                        Some((192, 255, 192)),
                    );
                    let tslot = match &tgt {
                        ShotTarget::Animal { slot, .. } => *slot,
                        ShotTarget::Player { .. } => None,
                    };
                    match (guest.is_some(), tslot) {
                        (true, _) => {
                            if let Some(c) = self.cluster.as_ref() {
                                let authority =
                                    self.cell_owner(crate::visidx::cell_of(tpos.0, tpos.1));
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
                }
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
    pub(super) fn fight_uimsg(&mut self, sid: SessionId, name: &str, args: &[i32]) {
        if let Some(out) = self.sessions.get_mut(&sid) {
            if let Some(w) = out.fight.widget {
                let b = crate::fight::uimsg(w, name, args);
                out.send(b);
            }
        }
    }

    /// Open (or reuse) the fight window and add a relation for `target`.
    pub(super) fn fight_open(&mut self, sid: SessionId, target: GobId) {
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
    pub(super) fn fight_del(&mut self, sid: SessionId, gob: GobId) {
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
            // The fight window is gone: drop the SELECTED attack too
            // (session 83). atk_cur used to survive the window close and
            // leak into the NEXT fight - a quell selected in round one
            // then resolved 0.9 s into round two with advantage 0,
            // skipping every gate ("the gates ran at selection time"
            // only holds within ONE fight). A fresh fight starts with a
            // fresh selection.
            out.fight.atk_cur = None;
            out.fight.atk_next = None;
            out.fight.blk = None;
        }
    }

    /// Handle client->server frv wdgmsg (click / give).
    /// One fight-window maneuver: `act("atk", id)` from the MenuGrid
    /// (the paginae/atk/* buttons). Validates the IP/advantage
    /// requirements against the CURRENT relation, applies the
    /// costs/gains, mirrors the opponent-side IP delta to a local
    /// victim, and streams the frv updates (`upd`, `atk`, `blk`).
    /// Guest targets keep their authoritative IP on their home node
    /// (documented server policy in combat-system.md).
    pub(super) fn on_maneuver(&mut self, sid: SessionId, id: &str) {
        let Some(m) = crate::fight::maneuver(id) else {
            debug!(sid, id, "unknown maneuver id");
            return;
        };
        let Some(&pidx) = self.world.by_session.get(&sid) else {
            debug!(sid, id, "maneuver: no player index");
            return;
        };
        debug!(
            sid,
            id,
            target = self.world.players[pidx].fight_target,
            ip = self
                .sessions
                .get(&sid)
                .and_then(|o| o
                    .fight
                    .rel(self.world.players[pidx].fight_target.unwrap_or_default()))
                .map(|r| r.ip_self),
            adv = self
                .sessions
                .get(&sid)
                .and_then(|o| o
                    .fight
                    .rel(self.world.players[pidx].fight_target.unwrap_or_default()))
                .map(|r| r.adv),
            "maneuver in"
        );
        let pgob = self.world.players[pidx].gob;
        let Some(target) = self.world.players[pidx].fight_target else {
            self.chat_line(sid, "You are not fighting anyone.", Some((255, 128, 128)));
            return;
        };
        // Requirements and costs first (refusals never mutate state).
        let refuse: Option<String> = {
            let Some(out) = self.sessions.get(&sid) else {
                debug!(sid, id, "maneuver: session gone");
                return;
            };
            let Some(rel) = out.fight.rel(target) else {
                debug!(
                    sid,
                    id,
                    target,
                    nrels = out.fight.rels.len(),
                    widget = out.fight.widget.is_some(),
                    "maneuver: no relation for the live fight_target"
                );
                return;
            };
            if rel.ip_self < m.req_ip {
                Some(format!(
                    "You need at least {} initiative points for that.",
                    m.req_ip
                ))
            } else if rel.adv < m.req_adv {
                Some("You need more advantage for that.".to_owned())
            } else if rel.ip_self < m.ip_cost {
                Some("Not enough initiative points.".to_owned())
            } else {
                None
            }
        };
        if let Some(why) = refuse {
            self.chat_line(sid, &why, Some((255, 128, 128)));
            return;
        }
        // Quell the Beast (session 45 taming): target-specific gates on
        // top of the static IP/advantage requirements (animals-and-
        // husbandry.md taming service). Refusals chat and mutate nothing.
        if id == "quell" {
            let why = self.quell_gate(pidx, target);
            if let Some(why) = why {
                self.chat_line(sid, &why, Some((255, 128, 128)));
                return;
            }
        }
        // Opponent-side IP delta FIRST: a LOCAL victim's own pool changes
        // (their rel(pgob).ip_self) and their window re-streams; a
        // GUEST's authoritative pool lives on their home node (the
        // attacker's mirror applies the prediction below).
        let mut new_opp_ip: Option<i32> = None;
        if m.ip_opp != 0 {
            let vsid = self
                .world
                .players
                .iter()
                .find(|p| p.gob == target)
                .map(|p| p.session);
            if let Some(vsid) = vsid {
                let vupd = self.sessions.get_mut(&vsid).and_then(|vout| {
                    let rel = vout.fight.rel_mut(pgob)?;
                    rel.ip_self = (rel.ip_self + m.ip_opp).max(0);
                    Some((
                        rel.ip_self,
                        vec![
                            rel.gob,
                            rel.balance,
                            rel.intensity,
                            rel.give,
                            rel.ip_self,
                            rel.ip_other,
                        ],
                    ))
                });
                if let Some((pool, vupd)) = vupd {
                    new_opp_ip = Some(pool);
                    if let Some(vout) = self.sessions.get_mut(&vsid) {
                        if let Some(w) = vout.fight.widget {
                            let b = crate::fight::uimsg(w, "upd", &vupd);
                            vout.send(b);
                        }
                    }
                }
            } else if let Some(c) = self.cluster.as_ref() {
                // The victim is a foreign session player (a guest gob
                // here): her IP pool is authoritative on her home node.
                // Relay the opponent-pool delta so her fight window stays
                // truthful (session 42 ManeuverDelta; the attacker's own
                // window below applies the mirror prediction).
                let home = self.node_of_gob(target);
                if home != c.me {
                    c.mesh.send(
                        home,
                        crate::nodes::NodeMsg::ManeuverDelta {
                            attacker: pgob,
                            victim: target,
                            ip_opp: m.ip_opp,
                        },
                    );
                }
            }
        }
        // Apply the user's side: IP economy, advantage, attack queue.
        let upd = {
            let Some(out) = self.sessions.get_mut(&sid) else {
                return;
            };
            // Attack queue / stance FIRST (disjoint fields from the
            // relation list; rel_mut borrows the whole fight state).
            match m.kind {
                crate::fight::ManeuverKind::Attack => {
                    // The two-slot queue: the current attack slides into
                    // `next`, the selection becomes `current` (Fightview
                    // renders atk [cur, next]).
                    let old = out.fight.atk_cur;
                    out.fight.atk_cur = Some(m.res);
                    out.fight.atk_next = old;
                }
                crate::fight::ManeuverKind::Block => {
                    out.fight.blk = Some(m.res);
                }
                crate::fight::ManeuverKind::Boost => {}
            }
            let Some(rel) = out.fight.rel_mut(target) else {
                return;
            };
            rel.ip_self = (rel.ip_self - m.ip_cost + m.ip_gain).max(0);
            rel.adv = (rel.adv + m.adv).clamp(-50, 50);
            rel.sync_balance();
            // The opponent pool view: the victim's fresh value when the
            // delta applied locally, else the mirror prediction.
            rel.ip_other = new_opp_ip.unwrap_or((rel.ip_other + m.ip_opp).max(0));
            vec![
                rel.gob,
                rel.balance,
                rel.intensity,
                rel.give,
                rel.ip_self,
                rel.ip_other,
            ]
        };
        // Stream the user's own window: the relation update plus the
        // attack-queue / stance slot. Intern the pagina resources
        // BEFORE the mutable session borrow (res.intern borrows the
        // world store).
        let (cur, next, blk) = {
            let Some(out) = self.sessions.get(&sid) else {
                return;
            };
            (out.fight.atk_cur, out.fight.atk_next, out.fight.blk)
        };
        let cur_gi = cur.map(|n| (n, self.world.res.intern(n)));
        let next_gi = next.map(|n| (n, self.world.res.intern(n)));
        let blk_gi = blk.map(|n| (n, self.world.res.intern(n)));
        if let Some(out) = self.sessions.get_mut(&sid) {
            if let Some(w) = out.fight.widget {
                let b = crate::fight::uimsg(w, "upd", &upd);
                out.send(b);
                match m.kind {
                    crate::fight::ManeuverKind::Attack => {
                        let wc = cur_gi.map(|(n, gi)| announce_res(out, gi, n)).unwrap_or(-1);
                        let wn = next_gi
                            .map(|(n, gi)| announce_res(out, gi, n))
                            .unwrap_or(-1);
                        let b = crate::fight::uimsg(w, "atk", &[wc, wn]);
                        out.send(b);
                    }
                    crate::fight::ManeuverKind::Block => {
                        let wb = blk_gi.map(|(n, gi)| announce_res(out, gi, n)).unwrap_or(-1);
                        let b = crate::fight::uimsg(w, "blk", &[wb]);
                        out.send(b);
                    }
                    crate::fight::ManeuverKind::Boost => {}
                }
            }
        }
    }

    /// Handle client->server frv wdgmsg (click / give).
    pub(super) fn on_frv_msg(&mut self, sid: SessionId, name: &str, args: &[hnh_proto::ListArg]) {
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

    // ------------------------------------------------------------------
    // PvP consequences: armor, damage, knockout, criminal flag
    // ------------------------------------------------------------------

    /// Summed equipment armor class (defense, absorption), quality-scaled
    /// per piece. The client computes the same sum from the tooltips
    /// (Equipory.calcAC); the server applies it in combat (armor.rs).
    pub(super) fn armor_totals(&self, pidx: usize) -> (i32, i32) {
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

    /// The player's melee swing damage: the FIRST weapon found in the
    /// 16 equipment slots (hand items live at 3/4; slot addressing is
    /// server-side policy, docs/mechanics/items/items-and-quality.md),
    /// else the unarmed strength model (fight.rs). Used by every swing
    /// path - local PvP, animal fights, and the cross-node relays.
    pub(super) fn melee_dmg(&self, pidx: usize) -> i32 {
        let str_ = *self.world.players[pidx].attrs.get("str").unwrap_or(&10);
        for slot in &self.world.players[pidx].equip {
            let Some(s) = slot else { continue };
            let name = self.world.res.name(s.res).unwrap_or("");
            if let Some(d) = crate::fight::weapon_dmg(name, i32::from(s.ql), str_) {
                return d.max(1);
            }
        }
        crate::fight::unarmed_dmg(str_)
    }

    /// Apply HP damage to a session player after armor absorption.
    /// Returns true when the hit knocked the victim out (the knockout
    /// reset happened inside - session 38 PvP arrows use the flag for
    /// the shooter's chat feedback).
    pub(super) fn hurt_player(&mut self, pidx: usize, dmg: i32, from: GobId) -> bool {
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
            return true;
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
        false
    }

    /// Criminal-flag duration for a PvP knockout (server policy): 30
    /// real minutes, refreshed by every new knockout while it runs.
    pub const CRIMINAL_MS: u64 = 30 * 60 * 1000;
    /// RMSG_BUFF id of the criminal state. Real buffs start at 1; the
    /// client's pseudo-buffs own the negative ids (-1 crime toggle,
    /// -2 tracking, -3 swim - combat-system.md buff channel).
    pub const CRIMINAL_BUFF_ID: i32 = 1;

    /// The loser's share of a PvP knockout (server policy,
    /// combat-system.md "PvP knockout consequences"): legacy documents
    /// only the DEATH penalties (25-75% through the Tradition/Change
    /// slider); the knockout share - 10% of UNUSED LP, floor zero - is
    /// this server's written policy. Runs on the victim's home node.
    pub(super) fn knockout_lp_loss(&mut self, loser_pidx: usize) {
        let lost = (self.world.players[loser_pidx].lp / 10).max(0);
        if lost > 0 {
            self.world.players[loser_pidx].lp -= lost;
        }
        let sid = self.world.players[loser_pidx].session;
        if lost > 0 {
            self.chat_line(
                sid,
                &format!("You lost {lost} learning points in the defeat."),
                Some((255, 128, 128)),
            );
        }
    }

    /// The winner's share of a PvP knockout (server policy): flagged
    /// CRIMINAL (assault) for [`CRIMINAL_MS`], icon streamed as a live
    /// buff with a countdown. Runs on the winner's home node (the
    /// relay paths answer there through PvpSwingResult/PvpArrowResult).
    pub(super) fn flag_criminal(&mut self, winner_pidx: usize) {
        let until = self.world.now_ms + Self::CRIMINAL_MS;
        self.world.players[winner_pidx].criminal_until_ms = Some(until);
        let sid = self.world.players[winner_pidx].session;
        self.chat_line(
            sid,
            "You are flagged criminal for the assault (30 minutes).",
            Some((255, 196, 128)),
        );
        self.stream_criminal_buff(sid);
    }

    /// Stream (or refresh) the criminal buff to one session: countdown
    /// meter in legacy 1/60 s ticks over the remaining wall time. Also
    /// called on world entry so reconnects restore the icon.
    pub(super) fn stream_criminal_buff(&mut self, sid: SessionId) {
        let Some(&pidx) = self.world.by_session.get(&sid) else {
            return;
        };
        let Some(until) = self.world.players[pidx].criminal_until_ms else {
            return;
        };
        let remaining = until.saturating_sub(self.world.now_ms);
        if remaining == 0 {
            return;
        }
        let cticks = (remaining / 60) as i32;
        const NAME: &str = "gfx/hud/buffs/thorn";
        let gi = self.world.res.intern(NAME);
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        let w = out.res.wire_named(gi, NAME);
        if let Some((n, ver)) = out.res.pending_announce(w) {
            out.send(wdg::resid(w, n, ver));
            out.res.mark_announced(w);
        }
        // RMSG_BUFF rides the RELIABLE session stream (Glob.buffmsg),
        // unlike OBJDATA overlays whose send_raw is datagram-semantics.
        out.send(wdg::buff_set(
            Self::CRIMINAL_BUFF_ID,
            w,
            "Criminal (assault)",
            -1,
            -1,
            100,
            cticks,
            1,
        ));
    }

    /// Criminal-flag expiry sweep (one cheap pass in the world tick):
    /// when the timer runs out the flag clears and the buff icon is
    /// removed with RMSG_BUFF rm.
    pub(super) fn tick_criminal_expiry(&mut self) {
        let expired: Vec<usize> = self
            .world
            .players
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                matches!(p.criminal_until_ms, Some(u) if u <= self.world.now_ms).then_some(i)
            })
            .collect();
        for i in expired {
            self.world.players[i].criminal_until_ms = None;
            let sid = self.world.players[i].session;
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.send(wdg::buff_rm(Self::CRIMINAL_BUFF_ID));
            }
            self.chat_line(
                sid,
                "Your criminal flag has expired.",
                Some((192, 255, 192)),
            );
        }
    }

    pub(super) fn tick_vitals(&mut self) {
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
}
