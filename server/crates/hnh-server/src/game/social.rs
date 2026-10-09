//! Chat + party + character-sheet UI: the area-chat relay, party
//! invitations / joins / leaves + roster sync, the LP skill shop
//! (buy + attribute spend) and the chr / mapview / speedget widget
//! builders - communication.md, character.md.
//!
//! Pure move out of game.rs (session 70 split).

use super::*;

impl Game {
    // ------------------------------------------------------------------
    // Chat + party (docs/mechanics/network/communication.md)
    // ------------------------------------------------------------------

    /// Relay one area-chat line from a player to every session in radius.
    pub(super) fn on_chat_msg(&mut self, sid: SessionId, raw: &str) {
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
        // Cluster: relay the line to every peer (each node re-filters by
        // the sender's position for its own sessions).
        if self.is_cluster() {
            let c = self.cluster.as_ref().expect("cluster");
            c.mesh.broadcast_except(
                c.nodes.get(),
                c.me,
                crate::nodes::NodeMsg::Chat {
                    from: sender_name,
                    at: sender_pos,
                    text: text.to_string(),
                },
            );
        }
    }

    /// Push one "log" line to a session's Area Chat window. The chat
    /// uimsg arg list is (text[, color[, urgent]]); `None` renders the
    /// client default color.
    pub(super) fn chat_line(&mut self, sid: SessionId, text: &str, color: Option<(u8, u8, u8)>) {
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
    pub(super) fn system_line(&mut self, sid: SessionId, text: &str) {
        let (r, g, b) = crate::chat::SYSTEM_COLOR;
        self.chat_line(sid, text, Some((r, g, b)));
    }

    /// Click on another player: open the clicker's invite flower menu.
    pub(super) fn open_party_invite_menu(&mut self, sid: SessionId, target: GobId) {
        let clicker_gob = match self.sessions.get(&sid).and_then(|o| o.player_gob) {
            Some(g) => g,
            None => return,
        };
        if target == clicker_gob {
            return;
        }
        // Party-invite applicability does NOT gate the Fight option
        // (session 39): an already-partied or full-party target can
        // still be dueled, so the refusals below only drop the invite
        // petal from the menu instead of blocking it entirely.
        let mut invite_ok = self.world.party_idx(target).is_none();
        if invite_ok {
            if let Some(pidx) = self.world.party_idx(clicker_gob) {
                let party = &self.world.parties[pidx];
                if party.leader != clicker_gob || party.members.len() >= crate::party::MAX_MEMBERS {
                    invite_ok = false;
                }
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
        let petals: Vec<ListVal> = if invite_ok {
            vec![
                ListVal::S("Invite to party".to_owned()),
                ListVal::S("Fight".to_owned()),
                ListVal::S("Cancel".to_owned()),
            ]
        } else {
            vec![
                ListVal::S("Fight".to_owned()),
                ListVal::S("Cancel".to_owned()),
            ]
        };
        out.send(wdg::new_wdg(w, "sm", -1, -1, 0, &petals));
        out.player_menu = Some((w, crate::party::PlayerMenu::InviteTarget(target)));
    }

    /// Melee duel offer on a CROSS-NODE guest player (session 39): the
    /// same flower menu, minus the party petal - party membership has no
    /// cross-node relay, so Fight is the only offer available.
    pub(super) fn open_guest_fight_menu(&mut self, sid: SessionId, target: GobId) {
        let clicker_gob = match self.sessions.get(&sid).and_then(|o| o.player_gob) {
            Some(g) => g,
            None => return,
        };
        if target == clicker_gob {
            return;
        }
        let Some(out) = self.sessions.get_mut(&sid) else {
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
                ListVal::S("Fight".to_owned()),
                ListVal::S("Cancel".to_owned()),
            ],
        ));
        out.player_menu = Some((w, crate::party::PlayerMenu::FightTarget(target)));
    }

    /// Flower menu petal on a party menu: confirm/cancel the clicker's
    /// invite or the invitee's join.
    pub(super) fn on_party_menu_choice(&mut self, sid: SessionId, wid: u16, choice: i32) {
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
        // NOTE: the petal index is NOT validated here - each action below
        // knows its own menu layout (the Fight petal is index 1 on the
        // invite menu, 0 on the guest menu) and cancels on anything else.
        match action {
            crate::party::PlayerMenu::InviteTarget(target) => {
                // Petal 0 invites; petal 1 opens the melee duel (session
                // 39); anything else cancels.
                if choice == 0 {
                    self.send_party_invitation(sid, target);
                } else if choice == 1 {
                    self.start_pvp_melee(sid, target);
                } else {
                    let out = self.sessions.get_mut(&sid);
                    if let Some(out) = out {
                        out.send(wdg::wdgmsg(wid, "cancel", &[]));
                    }
                }
            }
            crate::party::PlayerMenu::JoinParty { leader } => {
                if choice == 0 {
                    self.join_party(leader, sid);
                } else {
                    let out = self.sessions.get_mut(&sid);
                    if let Some(out) = out {
                        out.send(wdg::wdgmsg(wid, "cancel", &[]));
                    }
                }
            }
            crate::party::PlayerMenu::FightTarget(target) => {
                if choice == 0 {
                    self.start_pvp_melee(sid, target);
                } else {
                    let out = self.sessions.get_mut(&sid);
                    if let Some(out) = out {
                        out.send(wdg::wdgmsg(wid, "cancel", &[]));
                    }
                }
            }
        }
    }

    /// The clicker confirmed: offer membership to the target player.
    pub(super) fn send_party_invitation(&mut self, inviter_sid: SessionId, target: GobId) {
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
    pub(super) fn join_party(&mut self, leader_gob: GobId, joiner_sid: SessionId) {
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
    pub(super) fn sync_party(&mut self, pidx: usize) {
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
    pub(super) fn party_leave(&mut self, sid: SessionId) {
        let Some(gob) = self.sessions.get(&sid).and_then(|o| o.player_gob) else {
            return;
        };
        self.party_leave_gob(gob);
    }

    /// Remove a gob from its party, transfer leadership or disband, and
    /// clear the client-side roster state of everyone involved.
    pub(super) fn party_leave_gob(&mut self, gob: GobId) {
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
    pub(super) fn chr_window(&self, sid: SessionId) -> Option<u16> {
        self.sessions.get(&sid)?.chr_window()
    }

    /// Push the LP balance + skill lists to an open character sheet
    /// (CharWnd `exp`/`nsk`/`psk` uimsgs). Only catalog names whose pack
    /// resource exists are pushed; `nsk` carries (name, cost) pairs of
    /// everything the character does not own yet.
    pub(super) fn push_lp_msgs(&mut self, sid: SessionId) {
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
    pub(super) fn on_skill_buy(&mut self, sid: SessionId, name: &str) {
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
            Err(crate::skills::BuyError::Prerequisite) => {
                let def = crate::skills::catalog_get(name);
                let prereq = def
                    .and_then(|d| d.prereq)
                    .and_then(crate::skills::catalog_get);
                let label = prereq.map(|d| d.label).unwrap_or("another skill");
                self.system_line(sid, &format!("You need to know {label} first."));
            }
        }
        self.push_lp_msgs(sid);
    }

    /// chr "sattr": raise incrementable skill values. The client sends
    /// EVERY SAttr as (name, targetBaseValue) pairs on each Buy click —
    /// untouched ones carry their current value and are skipped here.
    /// The batch is priced first and applied all-or-nothing (the client
    /// prediction is advisory; the server is authoritative).
    pub(super) fn on_skill_attrs(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
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
    pub(super) fn has_skill_value(&self, sid: SessionId, name: &str, min: i32) -> bool {
        self.world
            .player(sid)
            .map(|p| p.attrs.get(name).copied().unwrap_or(0) >= min)
            .unwrap_or(false)
    }

    /// Widget id of this session's mapview, if created.
    pub(super) fn mapview_wid(out: &SessionOut) -> Option<u16> {
        out.widgets
            .iter()
            .find(|(_, t)| t.as_str() == "mapview")
            .map(|(id, _)| *id)
    }

    /// Widget id of this session's speedget, if created.
    pub(super) fn speedget_wid(&self, sid: SessionId) -> Option<u16> {
        self.sessions
            .get(&sid)?
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "speedget")
            .map(|(id, _)| *id)
    }

    /// Open the character sheet window (`chr`) and feed its FEP bar.
    pub(super) fn open_char_sheet(&mut self, sid: SessionId) {
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
}
