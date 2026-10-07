//! The cluster mesh: node-message handling, guest gob mirroring and
//! republishing, the per-cell subscription model, authority transfer
//! and the relay paths every feature module publishes through.

use super::*;

impl Game {
    /// Node-link message dispatch (cluster mode only).
    pub(super) fn on_node_msg(&mut self, msg: crate::nodes::NodeMsg) {
        use crate::nodes::NodeMsg;
        match msg {
            NodeMsg::Ping => {}
            NodeMsg::Hello { .. } => {} // handshake handled by the mesh
            NodeMsg::Sub { from, cells } => {
                let owned: Vec<(i32, i32)> = {
                    let Some(c) = self.cluster.as_mut() else {
                        return;
                    };
                    let owned: Vec<(i32, i32)> = cells
                        .into_iter()
                        .filter(|&cell| crate::grid_owner::owner_of(cell, c.nodes) == c.me)
                        .collect();
                    tracing::debug!(from, cells = owned.len(), "peer subscribed");
                    // The sender ships DIFFS (tick_cluster sends only the
                    // added cells; Unsub removes), so apply incrementally.
                    // Session 34: the old whole-set replace silently
                    // dropped every previously subscribed cell the first
                    // time a moving session's view produced a second Sub
                    // - cross-node updates stopped flowing for cells the
                    // peer still subscribes to (a lit guest oven never
                    // re-rendered).
                    c.peer_subs
                        .entry(from)
                        .or_default()
                        .extend(owned.iter().copied());
                    owned
                };
                // Session 33 (Sub-driven populate): the subscriber's own
                // MAPREQ never materialized MY content for these cells
                // (owner-filtered populate) - materialize it now and
                // announce everything I hold there, so the subscriber
                // renders one authoritative copy per gob.
                self.populate_for_subscriber(from, owned);
            }
            NodeMsg::Unsub { from, cells } => {
                let Some(c) = self.cluster.as_mut() else {
                    return;
                };
                if let Some(subs) = c.peer_subs.get_mut(&from) {
                    for cell in cells {
                        subs.remove(&cell);
                    }
                    if subs.is_empty() {
                        c.peer_subs.remove(&from);
                    }
                }
            }
            NodeMsg::Chat { from, at, text } => self.deliver_remote_chat(&from, at, &text),
            NodeMsg::GuestAnnounce(st) => self.ingest_guest(st),
            NodeMsg::GuestUpdate(st) => self.ingest_guest(st),
            NodeMsg::GuestRetract { id } => self.remove_guest(id),
            NodeMsg::GuestTransfer(st) => self.promote_transfer(st),
            NodeMsg::RelayAttack {
                attacker,
                target,
                chip,
                dmg,
            } => self.relay_swing(attacker, target, chip, dmg),
            NodeMsg::FightBars { id, def } => {
                // Authoritative defence bar from the animal's owner; the
                // mirror self-heals from it (the fightview reads this).
                if let Some(af) = self.world.guest_fights.get_mut(&id) {
                    af.def = def.clamp(0, crate::fight::BAR_FULL);
                    tracing::debug!(id, def, "relay fight bars synced");
                }
            }
            NodeMsg::PlayerHurt {
                player_gob,
                dmg,
                from,
            } => {
                // Animal retaliation against one of MY session players;
                // armor absorption and the knockout path live here.
                if let Some(pidx) = self.world.players.iter().position(|p| p.gob == player_gob) {
                    tracing::debug!(player_gob, dmg, from, "relay bite applied");
                    self.hurt_player(pidx, dmg, from);
                    // Native bite visual on the victim (the owner node's
                    // own overlay covers only ITS local viewers).
                    self.fx_overlay_broadcast(player_gob, "gfx/fx/bite");
                }
            }
            NodeMsg::PvpArrow {
                victim,
                attacker,
                dmg,
            } => {
                // Cross-node PvP arrow (session 38): one of MY session
                // players was hit by a foreign archer. Armor absorption,
                // HP and the knockout path live here (the same
                // hurt_player authority split as PlayerHurt); the
                // shooter's node gets the outcome back so its chat can
                // report the defeat.
                if let Some(pidx) = self.world.players.iter().position(|p| p.gob == victim) {
                    tracing::debug!(victim, attacker, dmg, "pvp arrow applied");
                    let vsid = self.world.players[pidx].session;
                    let knocked = self.hurt_player(pidx, dmg, attacker);
                    self.chat_line(
                        vsid,
                        &format!("An arrow hits you for {dmg} damage."),
                        Some((255, 128, 128)),
                    );
                    self.fx_overlay_broadcast(victim, "gfx/fx/hit");
                    if knocked {
                        // Authority side of the knockout consequences: the
                        // victim's LP share lives here (home node); the
                        // shooter's criminal flag is applied when the
                        // PvpArrowResult answer reaches ITS node.
                        self.knockout_lp_loss(pidx);
                    }
                    if let Some(c) = self.cluster.as_ref() {
                        let home = self.node_of_gob(attacker);
                        c.mesh.send(
                            home,
                            crate::nodes::NodeMsg::PvpArrowResult {
                                shooter: attacker,
                                killed: knocked,
                            },
                        );
                    }
                }
            }
            NodeMsg::PvpArrowResult { shooter, killed } => {
                // The victim's home node answered my shot.
                if let Some(p) = self.world.players.iter().find(|p| p.gob == shooter) {
                    if killed {
                        let sid = p.session;
                        self.chat_line(
                            sid,
                            "You have defeated your target!",
                            Some((192, 255, 192)),
                        );
                        // Winner's share of the knockout consequences
                        // (server policy): the criminal flag lives on
                        // the shooter's home node - here.
                        if let Some(widx) = self.world.players.iter().position(|p| p.gob == shooter)
                        {
                            self.flag_criminal(widx);
                        }
                    }
                }
            }
            NodeMsg::PvpSwing {
                attacker,
                victim,
                chip,
                dmg,
            } => {
                // Cross-node melee PvP (session 39): one of MY session
                // players is being swung at by a foreign attacker. The
                // victim's defence bar, armor, HP and the knockout path
                // are all authoritative here; the applied outcome goes
                // back as PvpSwingResult so the attacker's mirror and
                // chat stay truthful.
                if let Some(pidx) = self.world.players.iter().position(|p| p.gob == victim) {
                    tracing::debug!(attacker, victim, chip, dmg, "pvp swing applied");
                    let vsid = self.world.players[pidx].session;
                    let (landed, def_after) = {
                        let Some(vout) = self.sessions.get_mut(&vsid) else {
                            return;
                        };
                        let breaking = vout.fight.own_def <= crate::fight::OPENING_THRESHOLD;
                        vout.fight.own_def = (vout.fight.own_def - chip).max(0);
                        let landed =
                            breaking || vout.fight.own_def <= crate::fight::OPENING_THRESHOLD;
                        if landed {
                            vout.fight.own_def = crate::fight::BAR_FULL;
                        }
                        (landed, vout.fight.own_def)
                    };
                    let def_now = def_after;
                    let mut killed = false;
                    if landed {
                        killed = self.hurt_player(pidx, dmg, attacker);
                        // The attacker is published here as a guest while
                        // both players see each other; fall back to an
                        // anonymous line if the view already dropped.
                        let aname = self
                            .world
                            .guests
                            .get(&attacker)
                            .and_then(|g| g.kind.player_name())
                            .unwrap_or("Someone")
                            .to_owned();
                        self.chat_line(
                            vsid,
                            &format!("{aname} hits you for {dmg} damage."),
                            Some((255, 128, 128)),
                        );
                        self.fx_overlay_broadcast(victim, "gfx/fx/hit");
                        if killed {
                            // Authority side of the knockout consequences:
                            // the victim's LP share lives here (home
                            // node); the attacker's criminal flag is
                            // applied from the PvpSwingResult answer.
                            self.knockout_lp_loss(pidx);
                        }
                    }
                    if let Some(c) = self.cluster.as_ref() {
                        let home = self.node_of_gob(attacker);
                        c.mesh.send(
                            home,
                            crate::nodes::NodeMsg::PvpSwingResult {
                                attacker,
                                victim,
                                def: def_now,
                                landed,
                                killed,
                            },
                        );
                    }
                }
            }
            NodeMsg::ManeuverDelta {
                attacker,
                victim,
                ip_opp,
            } => {
                // The foreign attacker's node relayed a maneuver's
                // opponent-pool delta (session 42): my session player's
                // IP pool is authoritative here, keyed by the attacker's
                // guest gob. Fold and re-stream the victim's window; the
                // attacker's own window already applied the mirror
                // prediction.
                if ip_opp == 0 {
                    return;
                }
                let Some(pidx) = self.world.players.iter().position(|p| p.gob == victim) else {
                    return;
                };
                let vsid = self.world.players[pidx].session;
                let vupd = self.sessions.get_mut(&vsid).and_then(|vout| {
                    let rel = vout.fight.rel_mut(attacker)?;
                    rel.ip_self = (rel.ip_self + ip_opp).max(0);
                    Some(vec![
                        rel.gob,
                        rel.balance,
                        rel.intensity,
                        rel.give,
                        rel.ip_self,
                        rel.ip_other,
                    ])
                });
                if let Some(vupd) = vupd {
                    if let Some(vout) = self.sessions.get_mut(&vsid) {
                        if let Some(w) = vout.fight.widget {
                            let b = crate::fight::uimsg(w, "upd", &vupd);
                            vout.send(b);
                        }
                    }
                }
            }
            NodeMsg::PvpSwingResult {
                attacker,
                victim,
                def,
                landed,
                killed,
            } => {
                // The victim's home node answered my swing: re-sync the
                // local mirror (the fightview reads it) and close the
                // narrative on a landed hit / knockout.
                if let Some(p) = self.world.players.iter().find(|p| p.gob == attacker) {
                    let sid = p.session;
                    if let Some(mf) = self.world.guest_fights.get_mut(&victim) {
                        mf.def = def.clamp(0, crate::fight::BAR_FULL);
                    }
                    if let Some(out) = self.sessions.get_mut(&sid) {
                        if let Some(rel) = out.fight.rel_mut(victim) {
                            rel.defence = def.clamp(0, crate::fight::BAR_FULL);
                        }
                    }
                    if landed {
                        let vname = self
                            .world
                            .guests
                            .get(&victim)
                            .and_then(|g| g.kind.player_name())
                            .unwrap_or("your target");
                        self.chat_line(
                            sid,
                            &format!("You hit {vname} for damage."),
                            Some((192, 255, 192)),
                        );
                        if killed {
                            self.chat_line(
                                sid,
                                "You have defeated your target!",
                                Some((192, 255, 192)),
                            );
                            let pidx = self.world.players.iter().position(|p| p.gob == attacker);
                            if let Some(pidx) = pidx {
                                self.world.players[pidx].fight_target = None;
                                // Winner's share of the knockout
                                // consequences (server policy): the
                                // criminal flag lives on the attacker's
                                // home node - here.
                                self.flag_criminal(pidx);
                            }
                            self.world.guest_fights.remove(&victim);
                            self.fight_del(sid, victim);
                        }
                    }
                }
            }
            NodeMsg::KillCredit { player_gob, lp } => {
                if let Some(p) = self.world.players.iter_mut().find(|p| p.gob == player_gob) {
                    p.lp += lp;
                    let sid = p.session;
                    self.push_cattr(sid);
                }
            }
            NodeMsg::RelayStaticAct {
                player,
                target,
                act,
            } => {
                self.relay_static(player, target, act);
            }
            NodeMsg::StaticAck { player, stack, lp } => {
                // Authority applied a relayed static act for MY session
                // player: grant the stack/lp exactly like the local path.
                if let Some(pidx) = self.world.players.iter().position(|p| p.gob == player) {
                    let sid = self.world.players[pidx].session;
                    if let Some(s) = stack {
                        let res_name = leak_static(s.res.as_str());
                        let gidx = self.world.res.intern(res_name);
                        self.grant_pickup(
                            sid,
                            InvStack {
                                res: gidx,
                                count: s.count,
                                ql: s.ql,
                                label: leak_static(s.label.as_str()),
                            },
                        );
                    }
                    if lp > 0 {
                        self.world.players[pidx].lp += lp;
                        self.push_cattr(sid);
                        self.push_lp_msgs(sid);
                    }
                }
            }
            NodeMsg::RelayPlantAct {
                player,
                tx,
                ty,
                spec,
                seed_ql,
            } => {
                self.relay_plant(player, tx, ty, spec, seed_ql);
            }
            NodeMsg::PlantAck { player, ok } => {
                // The authority planted the crop: NOW the seed leaves the
                // cursor. A failure ack (or silence from a downed peer)
                // keeps the seed - the player can retry.
                if !ok {
                    return;
                }
                if let Some(pidx) = self.world.players.iter().position(|p| p.gob == player) {
                    let sid = self.world.players[pidx].session;
                    if let Some(cursor) = self.sessions.get(&sid).and_then(|o| o.cursor) {
                        let mut cursor = cursor;
                        cursor.count = cursor.count.saturating_sub(1);
                        if let Some(out) = self.sessions.get_mut(&sid) {
                            out.cursor = if cursor.count == 0 {
                                None
                            } else {
                                Some(cursor)
                            };
                        }
                        self.refresh_inventory(sid);
                        self.sync_cursor_widget(sid);
                    }
                }
            }
            NodeMsg::RelayPlowAct { player, tx, ty } => {
                self.relay_plow(player, tx, ty);
            }
            NodeMsg::PlowAck { player, ok } => {
                // The authority plowed the furrow: NOW the stamina leaves
                // the player (never before the ack - a refused or lost
                // relay costs nothing, exactly like a local refusal).
                if !ok {
                    return;
                }
                if let Some(p) = self.world.players.iter_mut().find(|p| p.gob == player) {
                    p.stamina = (p.stamina - 10).max(0);
                }
            }
            NodeMsg::TileMutation { tx, ty, tile } => {
                self.apply_remote_tile_mutation(tx, ty, tile);
            }
            NodeMsg::RelayStationAct {
                player,
                target,
                act,
            } => {
                self.relay_station_act(player, target, act);
            }
            NodeMsg::StationAck { player, result } => {
                // Authority outcome for MY session player's station menu
                // choice. Refusals render the SAME system lines the local
                // path emits; success and stale views stay silent.
                let Some(pidx) = self.world.players.iter().position(|p| p.gob == player) else {
                    return;
                };
                let sid = self.world.players[pidx].session;
                match result {
                    crate::nodes::StationResult::Lit
                    | crate::nodes::StationResult::Extinguished
                    | crate::nodes::StationResult::Stale => {}
                    crate::nodes::StationResult::NeedsFuel => {
                        self.system_line(sid, "The oven needs fuel first.");
                    }
                    crate::nodes::StationResult::NeedsInput => {
                        self.system_line(sid, "The oven needs an input before lighting.");
                    }
                }
            }
            NodeMsg::RelayStationItem {
                player,
                target,
                stack,
            } => {
                self.relay_station_item(player, target, stack);
            }
            NodeMsg::StationItemAck { player, result } => {
                // Authority outcome for MY session player's fuel/input
                // delivery. FuelAdded/InputLoaded consume ONE cursor unit
                // NOW (never before the ack - a refused or lost relay
                // must not destroy the item, the seed-safe pattern).
                use crate::nodes::StationItemResult;
                let Some(pidx) = self.world.players.iter().position(|p| p.gob == player) else {
                    return;
                };
                let sid = self.world.players[pidx].session;
                match result {
                    StationItemResult::FuelAdded => {
                        self.consume_cursor_unit(sid);
                        self.system_line(sid, "Fuel added to the oven.");
                    }
                    StationItemResult::InputLoaded => {
                        self.consume_cursor_unit(sid);
                        self.system_line(sid, "Input loaded; right-click the oven to light it.");
                    }
                    StationItemResult::BusyLit => {
                        self.system_line(sid, "The fire is burning; wait for it to finish.");
                    }
                    StationItemResult::InputFull => {
                        self.system_line(sid, "The oven already holds an input.");
                    }
                    StationItemResult::NotProcessable => {
                        self.system_line(sid, "The oven cannot process that.");
                    }
                    StationItemResult::Gone => {}
                }
            }
            NodeMsg::CharQuery { from, name } => {
                // Cluster character migration, two-phase (query -> data ->
                // ack). A peer holding the snapshot OFFLINE re-serves it on
                // every query until the CharAck confirms adoption; a peer
                // without the key (or with the character ONLINE - a live
                // player keeps its home) answers CharNack.
                if self.cluster.is_none() || from == self.cluster_me() {
                    return;
                }
                let online = self
                    .world
                    .players
                    .iter()
                    .any(|p| crate::persist::save_key(&p.account, &p.name) == name);
                if online || !self.save.players.contains_key(&name) {
                    debug!(from, %name, "char query: nack (missing or online)");
                    let me = self.cluster_me();
                    self.cluster_mesh().send(
                        from,
                        NodeMsg::CharNack {
                            to: from,
                            from: me,
                            name,
                        },
                    );
                    return;
                }
                let Some(snap) = self.save.players.get(&name).cloned() else {
                    return;
                };
                info!(from, %name, "char query: serving snapshot to peer");
                let me = self.cluster_me();
                self.cluster_mesh().send(
                    from,
                    NodeMsg::CharData {
                        to: from,
                        from: me,
                        name,
                        snap,
                    },
                );
            }
            NodeMsg::CharData {
                to,
                from,
                name,
                snap,
            } => {
                if self.cluster.is_none() || to != self.cluster_me() {
                    return;
                }
                // Mark the peer answered even if the join is gone: a
                // duplicate CharData after a retry needs no further nacks.
                if let Some((_, join)) = self
                    .pending_joins
                    .iter_mut()
                    .find(|(_, j)| crate::persist::save_key(&j.account, &j.chosen) == name)
                {
                    join.answered.insert(from);
                }
                let Some((sid, chosen)) = self
                    .pending_joins
                    .iter()
                    .find(|(_, j)| crate::persist::save_key(&j.account, &j.chosen) == name)
                    .map(|(s, j)| (*s, j.chosen.clone()))
                else {
                    debug!(%name, "late CharData with no pending join: dropped");
                    return;
                };
                self.pending_joins.remove(&sid);
                info!(sid, %name, pos = ?snap.pos, "char migration received: entering world");
                self.save.players.insert(name.clone(), snap);
                // Confirm adoption so the holder drops its copy.
                self.cluster_mesh().send(from, NodeMsg::CharAck { name });
                self.enter_world_inner(sid, chosen, false);
            }
            NodeMsg::CharAck { name } => {
                // The requester adopted the snapshot: the migration is
                // durable. Drop the local copy and persist the removal.
                if self.cluster.is_none() || !self.save.players.contains_key(&name) {
                    return;
                }
                info!(%name, "char ack: dropping migrated snapshot");
                self.save.players.remove(&name);
                if let Err(e) = self.save.flush(self.world.seed) {
                    tracing::warn!(error = %e, "char migration flush failed");
                }
            }
            NodeMsg::CharNack { to, from, name } => {
                if self.cluster.is_none() || to != self.cluster_me() || from == self.cluster_me() {
                    return;
                }
                let nodes = self.cluster_nodes();
                let Some((sid, all_answered, chosen)) = self
                    .pending_joins
                    .iter_mut()
                    .find(|(_, j)| crate::persist::save_key(&j.account, &j.chosen) == name)
                    .map(|(s, j)| {
                        j.answered.insert(from);
                        let done = j.peers_answered(nodes);
                        (*s, done, if done { Some(j.chosen.clone()) } else { None })
                    })
                else {
                    debug!(%name, "late CharNack with no pending join: dropped");
                    return;
                };
                if !all_answered {
                    return;
                }
                self.pending_joins.remove(&sid);
                if let Some(chosen) = chosen {
                    info!(sid, %name, "char query: every peer answered, entering fresh");
                    self.enter_world_inner(sid, chosen, false);
                }
            }
        }
    }

    /// My node index (single-node mode: 0).
    pub(super) fn cluster_me(&self) -> usize {
        self.cluster.as_ref().map(|c| c.me).unwrap_or(0)
    }

    /// Peer count of the cluster (single-node mode: 0).
    pub(super) fn cluster_nodes(&self) -> usize {
        self.cluster.as_ref().map(|c| c.nodes.get()).unwrap_or(0)
    }

    /// Mesh handle; single-node mode has none, so callers must only use
    /// this after an `is_cluster()` check.
    fn cluster_mesh(&self) -> &crate::nodes::Mesh {
        &self
            .cluster
            .as_ref()
            .expect("BUG: cluster_mesh called outside cluster mode")
            .mesh
    }

    /// Render state of the local gob at `slot` as a wire guest state.
    /// The render resource NAME of a local gob (its world shape, not the
    /// inventory icon) as carried by GuestKind::Static. Leaks into the
    /// interned-name arena like every other cross-node string.
    fn static_res_name(&self, slot: usize) -> String {
        self.world
            .res
            .name(self.world.gobs.res_idx[slot])
            .unwrap_or("gfx/terobjs/items/branch")
            .to_string()
    }

    pub(super) fn guest_state_from_slot(
        &self,
        id: GobId,
        slot: usize,
    ) -> Option<crate::nodes::GuestState> {
        use crate::nodes::{GuestKind, GuestLinMove, GuestState};
        let kind = match self.world.gobs.kind[slot] {
            Kind::Animal { species } => GuestKind::Animal {
                species: species.index(),
            },
            Kind::Player { player } => GuestKind::Player {
                name: self.world.players.get(player)?.name.clone(),
                equip: self
                    .player_equip_names(player)
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect(),
            },
            // Statics publish too (session 30): drops near a cell boundary
            // must be visible AND clickable across nodes. The render name
            // is the gob's world shape (res_idx); the class tag is stable
            // for the gob's lifetime, so subscribers never need an update
            // to pick the right relay act.
            // Session 35: the Drop arm carries the FULL drop payload
            // (DropView) so an authority transfer can rebuild Kind::Drop on
            // the receiving node; plain guest publishes leave it None
            // (subscribers render from res_name and relay Pickup acts).
            Kind::Drop {
                inv_res_idx,
                ql,
                label,
                ..
            } => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Drop,
                crop: None,
                station: None,
                stage: None,
                drop: Some(crate::nodes::DropView {
                    inv_res: self.world.res.name(inv_res_idx).unwrap_or("").to_owned(),
                    ql,
                    label: label.to_owned(),
                }),
            },
            Kind::Tree { .. } => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Tree,
                crop: None,
                station: None,
                stage: None,
                drop: None,
            },
            Kind::Stone => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Stone,
                crop: None,
                station: None,
                stage: None,
                drop: None,
            },
            // Plans/structures: renderable but no relay act today (their
            // menus are session UI on the authority side).
            // Stations (session 33): publish with the Station class + a
            // state snapshot (spec, lit, fuel, has_input) piggybacked on
            // the payload - the home node opens the Light/Extinguish
            // menu locally from it, and every state change re-publishes
            // so subscribers re-render the lit sprite from the sdt byte.
            // Crops (session 31): publish with the Crop class + the
            // (spec, stage) payload - the home node opens the harvest
            // menu locally and the stage re-renders on the subscriber
            // through the sdt byte in the guest spawn/update blocks.
            Kind::Station { spec, lit } => {
                let view = self
                    .world
                    .stations
                    .get(&id)
                    .map(|st| crate::nodes::StationView {
                        spec: st.spec,
                        lit: st.lit,
                        fuel: st.fuel,
                        has_input: st.input.is_some(),
                    });
                GuestKind::Static {
                    res_name: self.static_res_name(slot),
                    class: crate::nodes::StaticClass::Station,
                    crop: None,
                    station: view.or(Some(crate::nodes::StationView {
                        spec,
                        lit,
                        fuel: 0,
                        has_input: false,
                    })),
                    stage: None,
                    drop: None,
                }
            }
            // Plans publish their construction stage (session 34) so a
            // peer watching a build re-renders the plan sprite on every
            // credited material. Structures carry no stage: their final
            // form is the plain sprite. Both publish the Structure class
            // - no relay act - until completion re-publishes the real
            // kind (a finished oven becomes a Station with its snapshot).
            Kind::Plan { stage, .. } => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Structure,
                crop: None,
                station: None,
                stage: Some(stage),
                drop: None,
            },
            Kind::Structure { .. } => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Structure,
                crop: None,
                station: None,
                stage: None,
                drop: None,
            },
            Kind::Crop { spec, stage } => GuestKind::Static {
                res_name: self.static_res_name(slot),
                class: crate::nodes::StaticClass::Crop,
                crop: Some((spec, stage)),
                station: None,
                stage: None,
                drop: None,
            },
        };
        Some(GuestState {
            id,
            pos: self.world.gobs.pos[slot],
            mv: self.world.gobs.mv[slot].map(|lm| GuestLinMove {
                sx: lm.sx,
                sy: lm.sy,
                tx: lm.tx,
                ty: lm.ty,
                steps: lm.steps,
                step: lm.step,
                started_ms: lm.started_ms,
                total_ms: lm.total_ms,
            }),
            moving: self.world.gobs.mv[slot].is_some(),
            facing: self.world.gobs.facing[slot],
            kind,
            hp: self.world.gobs.hp[slot],
            max_hp: self.world.gobs.max_hp[slot],
            speed: self.world.gobs.speed[slot],
        })
    }

    /// Recipient peers for a local gob's guest stream: every peer
    /// subscribed to the gob's cell, plus (for abroad players) the owner
    /// of the cell the player stands in.
    fn publish_targets(&self, id: GobId, slot: usize) -> Vec<usize> {
        let Some(c) = &self.cluster else {
            return Vec::new();
        };
        let cell = crate::visidx::cell_of(self.world.gobs.pos[slot].0, self.world.gobs.pos[slot].1);
        let mut targets: Vec<usize> = c
            .peer_subs
            .iter()
            .filter(|(_, cells)| cells.contains(&cell))
            .map(|(p, _)| *p)
            .collect();
        if matches!(self.world.gobs.kind[slot], Kind::Player { .. }) {
            if let Some(&owner) = c.player_abroad.get(&id) {
                if !targets.contains(&owner) {
                    targets.push(owner);
                }
            }
        }
        targets
    }

    /// The grids a VisIndex cell touches (session 33). A cell spans 250
    /// subtiles, a grid 1100 (100 tiles x 11), so one cell touches one
    /// or two grids per axis - at most four grids total.
    pub(super) fn grids_touching_cell(cell: (i32, i32)) -> Vec<(i32, i32)> {
        let (cx, cy) = cell;
        let gx0 = (cx * 250).div_euclid(1100);
        let gx1 = (cx * 250 + 249).div_euclid(1100);
        let gy0 = (cy * 250).div_euclid(1100);
        let gy1 = (cy * 250 + 249).div_euclid(1100);
        let mut out = Vec::with_capacity(4);
        for gx in gx0..=gx1 {
            for gy in gy0..=gy1 {
                out.push((gx, gy));
            }
        }
        out
    }

    /// Sub-driven populate on the authority (session 33). The subscriber
    /// materialized the TILES of the grids it looks at (deterministic,
    /// identical on every node) but spawned no content for MY cells
    /// (owner-filtered populate). Here I materialize my part of every
    /// touched grid (idempotent per grid) and announce every gob I hold
    /// in the subscribed cells - freshly spawned AND pre-existing - so
    /// the subscriber's view starts from the one authoritative copy.
    fn populate_for_subscriber(&mut self, from: usize, cells: Vec<(i32, i32)>) {
        if cells.is_empty() {
            return;
        }
        // (a) Materialize my part of every grid the cells touch.
        let filter = self.cluster.as_ref().map(|c| (c.me, c.nodes));
        let mut fresh: Vec<GobId> = Vec::new();
        let mut touched: HashSet<(i32, i32)> = HashSet::new();
        for cell in &cells {
            for gc in Self::grids_touching_cell(*cell) {
                if touched.insert(gc) && !self.populated.contains(&gc) {
                    self.populated.insert(gc);
                    self.world.populate_grid(gc, filter, &mut fresh);
                    let animals = if self.saturated { 40 } else { 4 };
                    self.world.populate_animals(gc, filter, animals, &mut fresh);
                }
            }
        }
        // (b) Announce everything I hold in the subscribed cells: the
        // freshly spawned content plus anything that existed earlier
        // (stations, structures, previously populated statics). The cell
        // center +/- 124 subtiles spans exactly one VisIndex cell.
        let mut announce: Vec<GobId> = fresh;
        for &(cx, cy) in &cells {
            let ids = self
                .world
                .gobs
                .vis
                .gobs_in_view(cx * 250 + 125, cy * 250 + 125, 124);
            for id in ids {
                if !announce.contains(&id) {
                    announce.push(id);
                }
            }
        }
        let count = announce.len();
        for id in announce {
            self.publish(id, GuestEv::Announce);
        }
        tracing::debug!(from, gobs = count, "subscriber populate announced");
    }

    /// Publish one local gob event to interested peers. Announce = full
    /// state (new viewer/owner), Update = movement/pose delta, Retract =
    /// death/removal.
    pub(super) fn publish(&mut self, id: GobId, ev: GuestEv) {
        use crate::nodes::NodeMsg;
        if !self.is_cluster() {
            return;
        }
        // A Retract may target an ALREADY-KILLED gob (every death path
        // kills first, then retracts): resolve through the split id with
        // a generation check, so a reused slot never retracts a stranger.
        // Before session 30 this early-returned on dead gobs, so remote
        // retracts never fired at all - subscribers had to wait for their
        // own GC sweep. Announce/Update of a dead gob stays a no-op.
        let slot = match self.world.gobs.get(id) {
            Some(slot) => slot,
            None => {
                let (slot, gen) = crate::state::split_gob_id(id);
                if slot >= self.world.gobs.alive.len() {
                    return;
                }
                if self.world.gobs.gen[slot] != gen {
                    return; // slot was reused: the id is ancient history
                }
                if !matches!(ev, GuestEv::Retract) {
                    return;
                }
                slot
            }
        };
        match ev {
            GuestEv::Retract => {
                for peer in self.publish_targets(id, slot) {
                    self.cluster
                        .as_ref()
                        .expect("checked above")
                        .mesh
                        .send(peer, NodeMsg::GuestRetract { id });
                    self.world.perf.guest_pub += 1;
                }
            }
            GuestEv::Announce | GuestEv::Update => {
                let Some(st) = self.guest_state_from_slot(id, slot) else {
                    return;
                };
                for peer in self.publish_targets(id, slot) {
                    let msg = match ev {
                        GuestEv::Announce => NodeMsg::GuestAnnounce(st.clone()),
                        GuestEv::Update => NodeMsg::GuestUpdate(st.clone()),
                        GuestEv::Retract => unreachable!("routed above"),
                    };
                    self.cluster
                        .as_ref()
                        .expect("checked above")
                        .mesh
                        .send(peer, msg);
                    self.world.perf.guest_pub += 1;
                }
            }
        }
    }

    /// Per-tick cluster maintenance: subscription diffs, player territory
    /// publishing, animal authority transfer on cell crossing, guest GC.
    pub(super) fn tick_cluster(&mut self) {
        use crate::nodes::NodeMsg;
        if !self.is_cluster() {
            return;
        }
        let me = self.cluster.as_ref().expect("cluster").me;

        // --- Subscription maintenance (every 10 ticks): my sessions' view
        // cells unioned, filtered per peer to the cells that peer owns,
        // diffed against the current subscription set.
        if self.world.tick.is_multiple_of(10) {
            let wanted = self.wanted_view_cells();
            let nodes = self.cluster.as_ref().expect("cluster").nodes;
            let n = nodes.get();
            let mut sends: Vec<(usize, NodeMsg)> = Vec::new();
            {
                let c = self.cluster.as_mut().expect("cluster");
                for peer in 0..n {
                    if peer == c.me {
                        continue;
                    }
                    let want: HashSet<(i32, i32)> = wanted
                        .iter()
                        .copied()
                        .filter(|&cell| crate::grid_owner::owner_of(cell, nodes) == peer)
                        .collect();
                    let cur = c.my_subs.entry(peer).or_default();
                    let added: Vec<(i32, i32)> = want.difference(cur).copied().collect();
                    let removed: Vec<(i32, i32)> = cur.difference(&want).copied().collect();
                    if !added.is_empty() {
                        sends.push((
                            peer,
                            NodeMsg::Sub {
                                from: me,
                                cells: added,
                            },
                        ));
                    }
                    if !removed.is_empty() {
                        sends.push((
                            peer,
                            NodeMsg::Unsub {
                                from: me,
                                cells: removed,
                            },
                        ));
                    }
                    *cur = want;
                }
            }
            for (peer, msg) in sends {
                self.cluster.as_ref().expect("cluster").mesh.send(peer, msg);
            }
        }

        // --- Player territory publishing: a local player standing in a
        // foreign cell is announced to that cell's owner (the only node
        // whose sessions can possibly see the player); back home, the
        // foreign owner is told to retract.
        let player_cells: Vec<(GobId, usize)> = self
            .world
            .players
            .iter()
            .filter_map(|p| {
                let slot = self.world.gobs.get(p.gob)?;
                let cell = crate::visidx::cell_of(
                    self.world.gobs.pos[slot].0,
                    self.world.gobs.pos[slot].1,
                );
                Some((p.gob, self.cell_owner(cell)))
            })
            .collect();
        for (pgob, owner) in player_cells {
            let abroad = self
                .cluster
                .as_ref()
                .expect("cluster")
                .player_abroad
                .clone();
            let prev = abroad.get(&pgob).copied();
            if Some(owner) == prev {
                continue;
            }
            if owner == me {
                // Back on home ground: retract from the previous owner.
                if let Some(old) = prev {
                    self.cluster
                        .as_ref()
                        .expect("cluster")
                        .mesh
                        .send(old, NodeMsg::GuestRetract { id: pgob });
                    self.cluster
                        .as_mut()
                        .expect("cluster")
                        .player_abroad
                        .remove(&pgob);
                }
            } else {
                // Retract from the OLD owner if the player switched foreign
                // cells owned by different nodes, then announce to the new.
                if let Some(old) = prev.filter(|&o| o != owner) {
                    self.cluster
                        .as_ref()
                        .expect("cluster")
                        .mesh
                        .send(old, NodeMsg::GuestRetract { id: pgob });
                }
                self.cluster
                    .as_mut()
                    .expect("cluster")
                    .player_abroad
                    .insert(pgob, owner);
                self.publish(pgob, GuestEv::Announce);
            }
        }

        // --- Animal authority transfer: an animal standing in a foreign
        // cell moves to its cell's owner (full state, SAME id), and the
        // local copy demotes to a guest so local viewers never flicker.
        let animal_ids = self.world.animal_gobs.clone();
        for id in animal_ids {
            let Some(slot) = self.world.gobs.get(id) else {
                continue;
            };
            let cell =
                crate::visidx::cell_of(self.world.gobs.pos[slot].0, self.world.gobs.pos[slot].1);
            let owner = self.cell_owner(cell);
            if owner == me {
                continue;
            }
            let Some(st) = self.guest_state_from_slot(id, slot) else {
                continue;
            };
            self.cluster
                .as_ref()
                .expect("cluster")
                .mesh
                .send(owner, NodeMsg::GuestTransfer(st.clone()));
            // Demote: copy into the guest table, drop from every sim table,
            // kill the gob row, and re-index the id as a guest. The wire
            // frame counter carries over so emitted finalizers stay ahead
            // of what viewers already applied.
            let frame = self.world.gobs.frame[slot];
            let res_idx = self.world.gobs.res_idx[slot];
            let mv = st.mv.map(|g| LinMove {
                sx: g.sx,
                sy: g.sy,
                tx: g.tx,
                ty: g.ty,
                steps: g.steps,
                step: g.step,
                started_ms: g.started_ms,
                total_ms: g.total_ms,
            });
            let cell = crate::visidx::cell_of(st.pos.0, st.pos.1);
            self.world.animal_fights.remove(&id);
            self.world.guests.insert(
                id,
                crate::state::GuestGob {
                    pos: st.pos,
                    mv,
                    frame,
                    moving: st.moving,
                    facing: st.facing,
                    kind: st.kind,
                    res_idx,
                    hp: st.hp,
                    max_hp: st.max_hp,
                    cell,
                    territory: false,
                    last_seen_tick: self.world.tick,
                },
            );
            self.world.gobs.kill(id);
            self.world.gobs.vis.insert(id, st.pos);
            self.world.animal_gobs.retain(|&a| a != id);
            // Tame rows never outlive local authority (the follow render
            // and AI skip are node-local; cross-node leashes are an open
            // MVP limitation recorded in the docs).
            if self.world.tamed.remove(&id).is_some() {
                self.stream_follow_off(id);
            }
            tracing::debug!(id, owner, "animal authority transferred");
        }

        // --- Drop authority transfer (session 35): a drop spawned by
        // THIS node onto a cell it does not own (a station output drop
        // whose spawn jitter crossed the cell boundary, stone rubble,
        // loot) is invisible to every player homed on the owner - peers
        // only subscribe to OUR cells, never their own. Mirror the
        // animal path: hand the full drop state (GuestTransfer with the
        // DropView payload) to the cell's owner and demote the local
        // copy to a guest. The owner claims it via promote_transfer and
        // publishes it back to everyone subscribed to that cell - so
        // both sides' players see and can pick up the same drop, with
        // ONE authority deciding the pickup race.
        let mut foreign_drops: Vec<GobId> = Vec::new();
        for slot in 0..self.world.gobs.kind.len() {
            if !self.world.gobs.alive[slot]
                || !matches!(self.world.gobs.kind[slot], Kind::Drop { .. })
            {
                continue;
            }
            let id = crate::state::gob_id_from_slot(slot, self.world.gobs.gen[slot]);
            let cell =
                crate::visidx::cell_of(self.world.gobs.pos[slot].0, self.world.gobs.pos[slot].1);
            if self.cell_owner(cell) != me {
                foreign_drops.push(id);
            }
        }
        for id in foreign_drops {
            let Some(slot) = self.world.gobs.get(id) else {
                continue;
            };
            let Some(st) = self.guest_state_from_slot(id, slot) else {
                continue;
            };
            let owner = crate::grid_owner::owner_of(
                crate::visidx::cell_of(st.pos.0, st.pos.1),
                self.cluster.as_ref().expect("cluster").nodes,
            );
            // The drop is static: no mv, no walking pose, one hit point.
            let frame = self.world.gobs.frame[slot];
            let res_idx = self.world.gobs.res_idx[slot];
            let cell = crate::visidx::cell_of(st.pos.0, st.pos.1);
            self.cluster
                .as_ref()
                .expect("cluster")
                .mesh
                .send(owner, crate::nodes::NodeMsg::GuestTransfer(st.clone()));
            self.world.guests.insert(
                id,
                crate::state::GuestGob {
                    pos: st.pos,
                    mv: None,
                    frame,
                    moving: false,
                    facing: st.facing,
                    kind: st.kind,
                    res_idx,
                    hp: 1,
                    max_hp: 1,
                    cell,
                    territory: false,
                    last_seen_tick: self.world.tick,
                },
            );
            self.world.gobs.kill(id);
            self.world.gobs.vis.insert(id, st.pos);
            tracing::debug!(id, owner, "drop authority transferred");
        }

        // --- Guest GC (every 50 ticks): a guest nobody renders and
        // nobody subscribes can never come back on its own (its owner
        // only streams to subscribed cells) — retract and drop it.
        if self.world.tick.is_multiple_of(50) {
            let subscribed: HashSet<(i32, i32)> = self
                .cluster
                .as_ref()
                .expect("cluster")
                .my_subs
                .values()
                .flatten()
                .copied()
                .collect();
            let any_visible =
                |g: &Self, id: GobId| g.sessions.values().any(|o| o.visible.contains(&id));
            let stale: Vec<GobId> = self
                .world
                .guests
                .iter()
                .filter(|(id, g)| {
                    !g.territory && !subscribed.contains(&g.cell) && !any_visible(self, **id)
                })
                .map(|(id, _)| *id)
                .collect();
            for id in stale {
                self.remove_guest(id);
            }
        }
    }

    /// Union of the view cells of all local sessions (subscription basis).
    fn wanted_view_cells(&self) -> HashSet<(i32, i32)> {
        let mut out = HashSet::new();
        for out_session in self.sessions.values() {
            let Some(pg) = out_session
                .player_gob
                .and_then(|id| self.world.gobs.get(id))
            else {
                continue;
            };
            let (px, py) = self.world.gobs.pos[pg];
            let span = VIEW_RADIUS;
            let (cx0, cx1) = (
                (px - span).div_euclid(crate::visidx::CELL),
                (px + span).div_euclid(crate::visidx::CELL),
            );
            let (cy0, cy1) = (
                (py - span).div_euclid(crate::visidx::CELL),
                (py + span).div_euclid(crate::visidx::CELL),
            );
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    out.insert((cx, cy));
                }
            }
        }
        out
    }

    /// Ingest a foreign-authority gob (announce or update): store/replace
    /// the guest row, keep the dirty-cell index honest so the vis scan
    /// spawns/retracts it for local sessions, and emit wire finalizers to
    /// sessions already rendering it when the movement state changes.
    fn ingest_guest(&mut self, st: crate::nodes::GuestState) {
        use crate::nodes::GuestKind;
        let cell = crate::visidx::cell_of(st.pos.0, st.pos.1);
        let (moving, facing, pos, mv) = (st.moving, st.facing, st.pos, st.mv);
        let kind = st.kind.clone();
        let id = st.id;
        let existed = self.world.guests.contains_key(&id);
        // Resolve render resource + inventory of layers locally.
        let res_idx = match &kind {
            GuestKind::Animal { species } => {
                let sp = match crate::state::Species::from_index(*species) {
                    Some(sp) => sp,
                    None => {
                        tracing::warn!(id, species, "guest animal species out of range");
                        return;
                    }
                };
                self.world.res.intern(sp.resname())
            }
            GuestKind::Player { .. } => self.world.res.intern("gfx/borka/body"),
            GuestKind::Static { res_name, .. } => {
                self.world.res.intern(leak_static(res_name.as_str()))
            }
        };
        let mv_lin = mv.map(|g| LinMove {
            sx: g.sx,
            sy: g.sy,
            tx: g.tx,
            ty: g.ty,
            steps: g.steps,
            step: g.step,
            started_ms: g.started_ms,
            total_ms: g.total_ms,
        });
        // Determine which local sessions already render this gob and what
        // changed, BEFORE mutating (wire finalizers mirror local movement:
        // LINBEG on new move, OD_MOVE on finish, OD_LAYERS on pose flip).
        let viewers: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&id))
            .map(|(s, _)| *s)
            .collect();
        let (pose_flipped, move_changed) = match self.world.guests.get(&id) {
            Some(old) => (
                old.moving != moving || old.facing != facing,
                old.mv.is_some() != mv_lin.is_some(),
            ),
            None => (false, false),
        };
        // HP change on an EXISTING guest (relay fight damage landed on the
        // owner): stream OD_HEALTH so local viewers see the health bar
        // move without waiting for the owner's own broadcast (which only
        // covers ITS local sessions).
        let hp_changed = self
            .world
            .guests
            .get(&id)
            .map(|g| g.hp != st.hp)
            .unwrap_or(false);
        // Kind payload flip (session 34): Structure -> Station on plan
        // completion, a construction stage advance, a station's lit byte,
        // a crop's stage. The row is replaced wholesale below, so compare
        // the OLD row against the incoming kind BEFORE the insert. A flip
        // re-renders every viewer's sprite (OD_RES + fresh sdt byte) -
        // the wire mirror of the owner's local restage_gob path. Before
        // session 34 the existing-guest path only streamed pose, move and
        // hp deltas, so a guest oven's lit byte and a guest plan's stage
        // NEVER re-rendered for players already watching the gob.
        let kind_changed = self
            .world
            .guests
            .get(&id)
            .map(|g| g.kind != kind)
            .unwrap_or(false);
        self.world.guests.insert(
            id,
            crate::state::GuestGob {
                pos,
                mv: mv_lin,
                frame: self.world.guests.get(&id).map(|g| g.frame).unwrap_or(0),
                moving,
                facing,
                kind,
                res_idx,
                hp: st.hp,
                max_hp: st.max_hp,
                cell,
                territory: false,
                last_seen_tick: self.world.tick,
            },
        );
        if !existed {
            // New guest: dirty-cell insert so the vis scan spawns it.
            self.world.gobs.vis.insert(id, pos);
            self.world.perf.guest_ingests += 1;
            tracing::debug!(id, ?moving, "guest ingested");
            return;
        }
        // Existing guest: reposition the index (both cells dirty) and
        // stream the same finalizers the owner's viewers got.
        self.world.gobs.vis.reposition(id, pos);
        if pose_flipped || move_changed {
            self.world.guests.get_mut(&id).expect("just inserted").frame += 1;
            let frame = self.world.guests.get(&id).expect("just inserted").frame;
            let mut m = MessageBuf::new();
            m.uint8(MSG_OBJDATA).uint8(0).int32(id).int32(frame as i32);
            match &mv_lin {
                Some(lm) => {
                    m.uint8(OD_LINBEG)
                        .coord(lm.sx, lm.sy)
                        .coord(lm.tx, lm.ty)
                        .int32(lm.steps);
                }
                None => {
                    m.uint8(OD_MOVE).coord(pos.0, pos.1);
                }
            }
            m.uint8(OD_LINSTEP)
                .int32(mv_lin.map(|g| g.steps).unwrap_or(0));
            m.uint8(OD_END);
            let block = m.finish();
            for sid in &viewers {
                if let Some(out) = self.sessions.get_mut(sid) {
                    out.send_raw(block.clone());
                    Self::record_unacked(out, id, frame, block.clone());
                }
            }
            // Pose flip streams the new layer set (same server-side pose
            // resolution as local movers), batched into one datagram per
            // viewer session.
            if pose_flipped {
                self.stream_guest_poses_batched(viewers.iter().map(|sid| (*sid, id)).collect());
            }
        }
        if hp_changed {
            let g = self.world.guests.get_mut(&id).expect("existing guest");
            g.frame += 1;
            let frame = g.frame;
            let quarters = ((g.hp * 4) / g.max_hp.max(1)).clamp(0, 4) as u8;
            let mut m = MessageBuf::new();
            m.uint8(MSG_OBJDATA)
                .uint8(0)
                .int32(id)
                .int32(frame as i32)
                .uint8(OD_HEALTH)
                .uint8(quarters)
                .uint8(OD_END);
            let block = m.finish();
            for sid in &viewers {
                if let Some(out) = self.sessions.get_mut(sid) {
                    out.send_raw(block.clone());
                    Self::record_unacked(out, id, frame, block.clone());
                }
            }
        }
        if kind_changed {
            // Full-block re-render (OD_RES with the fresh sdt byte, pose,
            // layers, health): the same wire shape as a fresh guest
            // spawn, so OCache rebuilds the sprite exactly like the
            // owner's local restage_gob path does for its own viewers.
            // Every viewer here is by construction already rendering the
            // gob (the visible set was snapshotted above).
            if let Some(g) = self.world.guests.get_mut(&id) {
                g.frame += 1;
            }
            let frame = self.world.guests.get(&id).map(|g| g.frame).unwrap_or(0);
            for sid in &viewers {
                if let Some(block) = self.encode_guest_block(*sid, id, true) {
                    if let Some(out) = self.sessions.get_mut(sid) {
                        out.send_raw(block.clone());
                        Self::record_unacked(out, id, frame, block);
                    }
                }
            }
        }
    }

    /// Remove a guest entirely (owner retract or GC): drop the row, clean
    /// the vis index, and retract it from every session rendering it.
    fn remove_guest(&mut self, id: GobId) {
        if self.world.guests.remove(&id).is_none() {
            return;
        }
        self.world.gobs.vis.remove(id);
        let sids: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&id))
            .map(|(s, _)| *s)
            .collect();
        for sid in sids {
            self.stream_retract(sid, id);
        }
        // Relay bookkeeping (session 28): a retracted ANIMAL closes the
        // fight of any local player engaged with it (its authoritative HP
        // is gone — death retract or out-of-cell GC); a retracted PLAYER
        // guest drops its relay rows so the animal stops retaliating.
        if self.world.guest_fights.remove(&id).is_some() {
            for pidx in 0..self.world.players.len() {
                if self.world.players[pidx].fight_target == Some(id) {
                    let sid = self.world.players[pidx].session;
                    self.world.players[pidx].fight_target = None;
                    self.fight_del(sid, id);
                }
            }
        }
        if self.world.guest_attackers.remove(&id).is_some() {
            // `id` was an animal with a relay fight row here.
            self.world.animal_fights.remove(&id);
        }
        let attacking_mine: Vec<GobId> = self
            .world
            .guest_attackers
            .iter()
            .filter(|(_, &p)| p == id)
            .map(|(&a, _)| a)
            .collect();
        for a in attacking_mine {
            self.world.guest_attackers.remove(&a);
            self.world.animal_fights.remove(&a);
        }
    }

    /// Authority handoff inbound: materialize the transferred gob under
    /// its EXACT id and take over simulation.
    fn promote_transfer(&mut self, st: crate::nodes::GuestState) {
        use crate::nodes::GuestKind;
        let crate::nodes::GuestState {
            id,
            pos,
            mv,
            moving,
            facing,
            kind,
            hp,
            max_hp,
            speed,
        } = st;
        // Only animals and drops transfer (players stay homed; trees and
        // stones never spawn on a foreign cell - the world generator
        // places statics inside their own cell). Anything else arriving
        // here is a peer bug — reject rather than corrupt local tables.
        let (spawn_kind, res_idx) = match kind {
            GuestKind::Animal { species } => match crate::state::Species::from_index(species) {
                Some(sp) => (
                    Kind::Animal { species: sp },
                    self.world.res.intern(sp.resname()),
                ),
                None => {
                    tracing::warn!(id, species, "transfer species out of range");
                    return;
                }
            },
            // Session 35 drop transfer: the cell's owner claims a drop
            // spawned by a peer (station output jitter across the cell
            // boundary, stone rubble, loot). The world render shape is
            // re-derived from the inventory resource name with the SAME
            // deterministic function the spawner used (drop_world_res),
            // so both nodes agree on the sprite without carrying it.
            GuestKind::Static {
                class: crate::nodes::StaticClass::Drop,
                drop: Some(view),
                ..
            } => {
                let inv_res_idx = self.world.res.intern(leak_static(&view.inv_res));
                let world_res = drop_world_res(&view.inv_res);
                let res_idx = self.world.res.intern(world_res);
                let label = leak_static(&view.label);
                (
                    Kind::Drop {
                        resname_idx: res_idx,
                        inv_res_idx,
                        ql: view.ql,
                        label,
                    },
                    res_idx,
                )
            }
            other => {
                tracing::warn!(?other, id, "transfer of a non-transferable guest rejected");
                return;
            }
        };
        let was_guest = self.world.guests.remove(&id).is_some();
        self.world
            .gobs
            .spawn_with_id(id, spawn_kind, pos, res_idx, Vitals { hp, max_hp, speed });
        if let Some(slot) = self.world.gobs.get(id) {
            self.world.gobs.facing[slot] = facing;
            self.world.gobs.pose_streamed[slot] = if moving { 8 + facing } else { facing };
            if let Some(g) = mv {
                self.world.gobs.mv[slot] = Some(LinMove {
                    sx: g.sx,
                    sy: g.sy,
                    tx: g.tx,
                    ty: g.ty,
                    steps: g.steps,
                    step: g.step,
                    started_ms: g.started_ms,
                    total_ms: g.total_ms,
                });
            }
        }
        match spawn_kind {
            Kind::Animal { .. } => {
                if !self.world.animal_gobs.contains(&id) {
                    self.world.animal_gobs.push(id);
                }
                tracing::debug!(id, "animal authority claimed");
            }
            Kind::Drop { .. } => tracing::debug!(id, "drop authority claimed"),
            _ => unreachable!("the match above only yields Animal or Drop"),
        }
        let _ = was_guest;
        // My subscribers may already render this gob (border viewers):
        // announce so their sessions re-acquire it if it left their view
        // while it was a guest elsewhere.
        self.publish(id, GuestEv::Announce);
    }

    /// Area chat from a remote node: same radius filter, but against the
    /// SENDER's position carried on the message.
    pub(super) fn deliver_remote_chat(&mut self, from: &str, at: (i32, i32), text: &str) {
        let line = format!("{from}: {text}");
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
                crate::chat::within_radius(
                    self.world.gobs.pos[slot],
                    at,
                    crate::chat::AREA_CHAT_RADIUS,
                )
            })
            .map(|(s, _)| *s)
            .collect();
        for r in recipients {
            self.chat_line(r, &line, None);
        }
    }

    /// Guest movement progress: identical timing model to local movers
    /// (the owner authored the linmove params; progress math is pure), so
    /// no per-tick streaming from the owner is needed — each subscriber
    /// derives LINSTEP locally for its viewing sessions.
    ///
    /// Session 34 batch shape: blocks are encoded once per moving guest,
    /// then ONE pass over the sessions merges every visible block into
    /// ONE datagram per session. Session 41 packs the blocks into one
    /// shared buffer indexed by vis cell (`move_batch`): the per-session
    /// fan-out iterates only the non-empty cells (a few dozen for a
    /// moving crowd) and rejects whole cells with one rectangle test —
    /// the O(sessions x movers) hash-probe fan-out this replaces was the
    /// dominant guests-phase cost in the 2x300 duel-cohort cluster
    /// (p95 7.9 ms at 200 sessions, linear in the session count).
    /// LINSTEP progress frames are deliberately NOT recorded in
    /// `unacked`: each frame is superseded next tick, so a lost datagram
    /// self-heals within 100 ms.
    pub(super) fn tick_guests(&mut self) {
        if self.world.guests.is_empty() {
            return;
        }
        let now = self.world.now_ms;
        let encode_t = Instant::now();
        let ids: Vec<GobId> = self.world.guests.keys().copied().collect();
        let mut batch = std::mem::take(&mut self.move_scratch);
        batch.clear();
        let mut finished_ids: Vec<GobId> = Vec::new();
        // Progress outcome for one guest this tick.
        enum GuestMove {
            /// Move finished this tick (finalizer block).
            Finish,
            /// Still moving, LINSTEP counter advanced to `i32`.
            Step(i32),
            /// Still moving within the same LINSTEP index: no block.
            Quiet,
        }
        for id in ids {
            let (mv, pos, frame) = {
                let Some(g) = self.world.guests.get_mut(&id) else {
                    continue;
                };
                let Some(lm) = g.mv else {
                    continue;
                };
                g.frame += 1;
                let frame = g.frame;
                let elapsed = now.saturating_sub(lm.started_ms);
                if elapsed >= u64::from(lm.total_ms) {
                    (GuestMove::Finish, (lm.tx, lm.ty), frame)
                } else {
                    let (cx, cy) = lm.pos_at(now);
                    let l = lm.step_at(now);
                    let advanced = l > lm.step;
                    g.mv = Some(LinMove { step: l, ..lm });
                    // Progress frames ship on the LINSTEP_EVERY_TICKS
                    // cadence only; the server-side counter still advances
                    // every tick.
                    let ship = self.world.tick.is_multiple_of(LINSTEP_EVERY_TICKS);
                    (
                        if advanced && ship {
                            GuestMove::Step(l)
                        } else {
                            GuestMove::Quiet
                        },
                        (cx, cy),
                        frame,
                    )
                }
            };
            // Apply: position + dirty-cell index.
            {
                let g = self.world.guests.get_mut(&id).expect("checked above");
                g.pos = pos;
            }
            self.world.gobs.vis.reposition(id, pos);
            match mv {
                GuestMove::Finish => {
                    let g = self.world.guests.get_mut(&id).expect("checked above");
                    g.moving = false;
                    // Headerless block (see broadcast_batch).
                    let mut m = MessageBuf::new();
                    m.uint8(0)
                        .int32(id)
                        .int32(frame as i32)
                        .uint8(OD_MOVE)
                        .coord(pos.0, pos.1)
                        .uint8(OD_LINSTEP)
                        .int32(0)
                        .uint8(OD_END);
                    batch.push(
                        id,
                        frame,
                        crate::visidx::cell_of(pos.0, pos.1),
                        true,
                        &m.finish(),
                    );
                    finished_ids.push(id);
                }
                GuestMove::Step(l) => {
                    let mut m = MessageBuf::new();
                    m.uint8(0)
                        .int32(id)
                        .int32(frame as i32)
                        .uint8(OD_LINSTEP)
                        .int32(l)
                        .uint8(OD_END);
                    batch.push(
                        id,
                        frame,
                        crate::visidx::cell_of(pos.0, pos.1),
                        false,
                        &m.finish(),
                    );
                }
                GuestMove::Quiet => {}
            }
        }
        self.world.perf.guests_encode_us = encode_t.elapsed().as_micros() as u64;
        // Fan-out: each session merges its visible blocks into one
        // datagram; finalizers also land in `unacked` (retransmittable).
        let fanout_t = Instant::now();
        self.broadcast_batch(&batch);
        self.move_scratch = batch;
        self.world.perf.guests_fanout_us = fanout_t.elapsed().as_micros() as u64;
        // Rest pose for finished movers: the standing layer block per
        // viewer, batched into one datagram per session (rare - only on
        // movement finalization; statics skip).
        let pose_t = Instant::now();
        let mut pose_jobs: Vec<(SessionId, GobId)> = Vec::new();
        for id in finished_ids {
            for (sid, out) in &self.sessions {
                if out.visible.contains(&id) {
                    pose_jobs.push((*sid, id));
                }
            }
        }
        self.stream_guest_poses_batched(pose_jobs);
        self.world.perf.guests_pose_us = pose_t.elapsed().as_micros() as u64;
    }

    /// Spawn block for a guest (mirrors `encode_gob_block`'s player/animal
    /// branches reading the GuestGob row instead of the SoA columns).
    fn encode_guest_block(&mut self, sid: SessionId, id: GobId, restage: bool) -> Option<Vec<u8>> {
        use crate::nodes::GuestKind;
        let g = self.world.guests.get(&id)?.clone();
        let out = self.sessions.get_mut(&sid)?;
        // A restage block re-renders a gob the session ALREADY sees (kind
        // flip / sdt byte change); a fresh spawn only fires once.
        if !restage && !out.visible.insert(id) {
            return None;
        }
        let mut m = MessageBuf::new();
        m.uint8(MSG_OBJDATA)
            .uint8(0)
            .int32(id)
            .int32(g.frame as i32);
        if let GuestKind::Static {
            res_name,
            crop,
            station,
            stage,
            ..
        } = &g.kind
        {
            let name = leak_static(self.world.res.name(g.res_idx).unwrap_or(res_name.as_str()));
            let w = out.res.wire_named(g.res_idx, name);
            // Crops carry their growth stage as the sprite sdt byte -
            // the same wire shape the local path emits for plants
            // (wire id | 0x8000, then len + bytes; OCache rebuilds the
            // sprite on a stage change). Stations (session 33) carry
            // their lit byte the same way, so a lit oven re-renders on
            // every re-published GuestUpdate without a new OD kind.
            // Construction plans (session 34) carry their build stage
            // the same way, so a peer watching a build sees the same
            // stage sprite the local restage path emits.
            if let Some(view) = station {
                m.uint8(OD_RES).uint16(w | 0x8000);
                m.uint8(1).uint8(view.lit as u8);
            } else {
                match (crop, stage) {
                    (Some((_spec, cstage)), _) => {
                        m.uint8(OD_RES).uint16(w | 0x8000);
                        m.uint8(1).uint8(*cstage);
                    }
                    (None, Some(pstage)) => {
                        m.uint8(OD_RES).uint16(w | 0x8000);
                        m.uint8(1).uint8(*pstage);
                    }
                    (None, None) => {
                        m.uint8(OD_RES).uint16(w);
                    }
                }
            }
        }
        match &g.mv {
            Some(lm) => {
                m.uint8(OD_LINBEG)
                    .coord(lm.sx, lm.sy)
                    .coord(lm.tx, lm.ty)
                    .int32(lm.steps);
                m.uint8(OD_LINSTEP).int32(lm.step);
            }
            None => {
                m.uint8(OD_MOVE).coord(g.pos.0, g.pos.1);
            }
        }
        // Composited drawables (players + animals only): server-side
        // pose resolution, the mirror of the local encode_gob_block
        // branch. Statics render from OD_RES alone and carry NO
        // OD_LAYERS - the local path never writes one for them, and a
        // bare 0xFFFF terminator without a base u16 breaks every
        // strict OD sequence parser (the session-34 probe caught the
        // test-build client crashing on guest static blocks).
        match &g.kind {
            GuestKind::Player { name, equip } => {
                m.uint8(OD_LAYERS);
                let base = "gfx/borka/body";
                let bi = self.world.res.intern(base);
                m.uint16(out.res.wire_named(bi, base));
                let equip_static: Vec<&'static str> =
                    equip.iter().map(|s| leak_static(s)).collect();
                for part in avatar_pose_layers(g.moving, g.facing) {
                    let gi = self.world.res.intern(part);
                    m.uint16(out.res.wire_named(gi, part));
                }
                for part in crate::equip::world_layers(&equip_static, g.moving, g.facing) {
                    let gi = self.world.res.intern(part);
                    m.uint16(out.res.wire_named(gi, part));
                }
                m.uint16(65535);
                // Non-own viewer: standing doll set (the own viewer's gob
                // is always local-homed, never a guest).
                let doll: Vec<&'static str> = avatar_pose_layers(false, g.facing)
                    .iter()
                    .copied()
                    .chain(
                        crate::equip::world_layers(&equip_static, false, g.facing)
                            .iter()
                            .copied(),
                    )
                    .collect();
                m.uint8(OD_AVATAR);
                for part in doll {
                    let gi = self.world.res.intern(part);
                    m.uint16(out.res.wire_named(gi, part));
                }
                m.uint16(65535);
                m.uint8(OD_BUDDY).string(name).uint8(0).uint8(0);
            }
            GuestKind::Animal { species } => {
                m.uint8(OD_LAYERS);
                let sp = crate::state::Species::from_index(*species)?;
                let base = kritter_base(sp);
                let bi = self.world.res.intern(base);
                m.uint16(out.res.wire_named(bi, base));
                let part = kritter_pose_layer(sp, g.moving, g.facing);
                let gi = self.world.res.intern(part);
                m.uint16(out.res.wire_named(gi, part));
                m.uint16(65535);
            }
            GuestKind::Static { .. } => {
                // Statics render from OD_RES alone; no OD_LAYERS (see
                // the comment above the match).
            }
        }
        let quarters = ((g.hp * 4) / g.max_hp.max(1)).clamp(0, 4) as u8;
        m.uint8(OD_HEALTH).uint8(quarters);
        m.uint8(OD_END);
        Some(m.finish())
    }

    /// Stream a guest spawn to one session: RESID announcements first
    /// (mirror stream_spawn's announce logic), then the encoded block.
    pub(super) fn stream_guest_spawn(&mut self, sid: SessionId, id: GobId) {
        use crate::nodes::GuestKind;
        let Some(g) = self.world.guests.get(&id).cloned() else {
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        // Announce the render resource(s) this session has not seen yet.
        let mut layers: Vec<&'static str> = Vec::new();
        match &g.kind {
            GuestKind::Animal { species } => {
                if let Some(sp) = crate::state::Species::from_index(*species) {
                    layers.push(kritter_base(sp));
                    layers.push(kritter_pose_layer(sp, g.moving, g.facing));
                }
            }
            GuestKind::Player { equip, .. } => {
                let equip_static: Vec<&'static str> =
                    equip.iter().map(|s| leak_static(s)).collect();
                layers.push("gfx/borka/body");
                layers.extend(avatar_pose_layers(g.moving, g.facing).iter().copied());
                layers.extend(crate::equip::world_layers(
                    &equip_static,
                    g.moving,
                    g.facing,
                ));
                layers.extend(avatar_doll_layers().iter().copied());
                layers.extend(crate::equip::doll_layers(&equip_static));
            }
            GuestKind::Static { .. } => {}
        }
        let static_res = matches!(g.kind, GuestKind::Static { .. });
        for layer_name in layers {
            let gi = self.world.res.intern(layer_name);
            let w = out.res.wire_named(gi, layer_name);
            if let Some((name, ver)) = out.res.pending_announce(w) {
                let msg = wdg::resid(w, name, ver);
                out.send(msg);
                out.res.mark_announced(w);
            }
        }
        if static_res {
            let name = self
                .world
                .res
                .name(g.res_idx)
                .unwrap_or("gfx/terobjs/items/branch");
            let w = out.res.wire_named(g.res_idx, name);
            if let Some((rname, ver)) = out.res.pending_announce(w) {
                let msg = wdg::resid(w, rname, ver);
                out.send(msg);
                out.res.mark_announced(w);
            }
        }
        if let Some(block) = self.encode_guest_block(sid, id, false) {
            let frame = self.world.guests.get(&id).map(|g| g.frame).unwrap_or(0);
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.send_raw(block.clone());
                Self::record_unacked(out, id, frame, block);
            }
        }
    }

    /// Stream the rest-pose blocks for finished guest movers, batched:
    /// ONE OBJDATA datagram per session carries ALL of that session's
    /// finished guests' layer blocks. The duel-cohort crowd finalizes
    /// dozens of guests per tick with ~300 viewers each - the per-(guest,
    /// viewer) datagram and the per-call GuestGob clone were the dominant
    /// pose-phase cost (p50 11 ms at 300 sessions). Guest rows are read
    /// in place (no clone); wire ids resolve through the session table.
    fn stream_guest_poses_batched(&mut self, mut jobs: Vec<(SessionId, GobId)>) {
        use crate::nodes::GuestKind;
        if jobs.is_empty() {
            return;
        }
        jobs.sort_unstable();
        let mut i = 0;
        while i < jobs.len() {
            let sid = jobs[i].0;
            let mut j = i;
            while j < jobs.len() && jobs[j].0 == sid {
                j += 1;
            }
            let Some(out) = self.sessions.get_mut(&sid) else {
                i = j;
                continue;
            };
            // Datagram materializes lazily: sessions whose guests all
            // vanished allocate nothing.
            let mut m: Option<MessageBuf> = None;
            for (_, id) in &jobs[i..j] {
                let Some(g) = self.world.guests.get(id) else {
                    continue;
                };
                // Statics have no pose (OD_RES alone renders them): an
                // empty layer list would carry the same bare-0xFFFF defect
                // the session-34 probe caught in the spawn block.
                if matches!(g.kind, GuestKind::Static { .. }) {
                    continue;
                }
                let frame = g.frame;
                let moving = g.moving;
                let facing = g.facing;
                let kind = &g.kind;
                let mm = m.get_or_insert_with(|| MessageBuf::with_capacity(256));
                mm.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(*id)
                    .int32(frame as i32)
                    .uint8(OD_LAYERS);
                match kind {
                    GuestKind::Player { equip, .. } => {
                        let base = "gfx/borka/body";
                        let bi = self.world.res.intern(base);
                        mm.uint16(out.res.wire_named(bi, base));
                        for part in avatar_pose_layers(moving, facing) {
                            let gi = self.world.res.intern(part);
                            mm.uint16(out.res.wire_named(gi, part));
                        }
                        // equip names are leaked already; no GuestGob clone.
                        let equip_static: Vec<&'static str> =
                            equip.iter().map(|s| leak_static(s)).collect();
                        for part in crate::equip::world_layers(&equip_static, moving, facing) {
                            let gi = self.world.res.intern(part);
                            mm.uint16(out.res.wire_named(gi, part));
                        }
                        mm.uint16(65535);
                    }
                    GuestKind::Animal { species } => {
                        let Some(sp) = crate::state::Species::from_index(*species) else {
                            continue;
                        };
                        let base = kritter_base(sp);
                        let bi = self.world.res.intern(base);
                        mm.uint16(out.res.wire_named(bi, base));
                        let part = kritter_pose_layer(sp, moving, facing);
                        let gi = self.world.res.intern(part);
                        mm.uint16(out.res.wire_named(gi, part));
                        mm.uint16(65535);
                    }
                    GuestKind::Static { .. } => {}
                }
                mm.uint8(OD_END);
            }
            if let Some(m) = m {
                out.send_raw(m.finish());
            }
            i = j;
        }
    }

    // Player commands
    // ------------------------------------------------------------------
}
