//! Inventory and item flows: ground drops, the inventory window,
//! the drag cursor, equipment, itemact on map/gobs, the food
//! flower menu and eating.

use super::*;

impl Game {
    /// Spawn an item drop gob near `at`.
    ///
    /// Two resources per drop: the gob RENDERS with a gfx/terobjs/items
    /// world shape (inventory item resources have no `neg` layer, so the
    /// real client fails their sprite with "No negative found" and the
    /// drop is invisible - measured on the GL client, session 26), while
    /// Kind::Drop::inv_res_idx keeps the gfx/invobjs icon resource so
    /// picking up restores the exact original stack.
    pub(super) fn spawn_drop_near(
        &mut self,
        at: (i32, i32),
        resname: &'static str,
        ql: u8,
        label: &'static str,
    ) {
        let inv_res_idx = self.world.res.intern(resname);
        let world_res = drop_world_res(resname);
        let res_idx = self.world.res.intern(world_res);
        let jitter = |w: &mut World| (w.next_ai_rand(7) - 3) * 11;
        let jx = jitter(&mut self.world);
        let jy = jitter(&mut self.world);
        let id = self.world.gobs.spawn(
            Kind::Drop {
                resname_idx: res_idx,
                inv_res_idx,
                ql,
                label,
            },
            (at.0 + jx, at.1 + jy),
            res_idx,
            1,
            0,
        );
        self.broadcast_spawn(id);
    }

    pub(super) fn broadcast_spawn(&mut self, id: GobId) {
        let sids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for sid in sids {
            if self.sessions[&sid].visible.contains(&id) || self.session_in_range(sid, id) {
                self.stream_spawn(sid, id);
            }
        }
        // Cluster: tell subscribed peers about the new gob (they render it
        // as a guest for their sessions).
        self.publish(id, GuestEv::Announce);
    }

    pub(super) fn broadcast_retract(&mut self, id: GobId) {
        let sids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for sid in sids {
            self.stream_retract(sid, id);
        }
        // Cluster: subscribers drop their guest copy.
        self.publish(id, GuestEv::Retract);
    }

    fn session_in_range(&self, sid: SessionId, id: GobId) -> bool {
        let Some(pslot) = self.world.gobs.get(id) else {
            return false;
        };
        let Some(out) = self.sessions.get(&sid) else {
            return false;
        };
        let Some(pg) = out.player_gob else {
            return false;
        };
        let Some(pp) = self.world.gobs.get(pg) else {
            return false;
        };
        let (px, py) = self.world.gobs.pos[pp];
        let (gx, gy) = self.world.gobs.pos[pslot];
        (gx - px).abs() <= VIEW_RADIUS && (gy - py).abs() <= VIEW_RADIUS
    }

    // ------------------------------------------------------------------
    // Inventory
    // ------------------------------------------------------------------

    pub(super) fn open_inventory(&mut self, sid: SessionId) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        let existing = out
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "invwnd")
            .map(|(id, _)| *id);
        if existing.is_none() {
            let w = out.new_wid("invwnd");
            // The client's Inventory factory requires the grid size
            // (Coord isz, cells); an empty arg list crashes its create()
            // (ArrayIndexOutOfBounds) and kills the whole UI thread.
            // 4 columns x 8 rows matches the refresh_inventory layout.
            out.send(wdg::new_wdg(w, "inv", 350, 250, 0, &[ListVal::C(4, 8)]));
            self.refresh_inventory(sid);
        }
    }

    fn inv_window(&self, sid: SessionId) -> Option<u16> {
        self.sessions
            .get(&sid)?
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "invwnd")
            .map(|(id, _)| *id)
    }

    // ------------------------------------------------------------------
    // Cursor drag widget (session 26): the held stack rendered at the
    // pointer. Legacy flow: Item.mousedown sends `take`; the server
    // replies with a drag Item widget (drag=1 + grab offset) parented to
    // the root; the client's Item constructor grabs the mouse and the
    // widget follows it (Item.java drag constructor). Without it the
    // held stack is invisible until dropped.
    // ------------------------------------------------------------------

    /// Keep the drag Item widget in sync with the cursor stack: create
    /// it when a stack is picked up, refresh `num` when the count
    /// changes, destroy it when the cursor empties. Call after any flow
    /// that mutates `out.cursor` (take, drop, itemact consumption).
    pub(super) fn sync_cursor_widget(&mut self, sid: SessionId) {
        let has_cursor = self.sessions.get(&sid).and_then(|o| o.cursor).is_some();
        let has_widget = self.sessions.get(&sid).and_then(|o| o.cursor_wid).is_some();
        match (has_cursor, has_widget) {
            (false, _) => self.hide_cursor_widget(sid),
            (true, false) => self.create_cursor_widget(sid),
            (true, true) => {
                let Some(stack) = self.sessions.get(&sid).and_then(|o| o.cursor) else {
                    return;
                };
                let Some(out) = self.sessions.get_mut(&sid) else {
                    return;
                };
                let w = out.cursor_wid.expect("BUG: has_widget checked");
                out.send(wdg::wdgmsg(w, "num", &[ListVal::I(stack.count as i32)]));
            }
        }
    }

    /// Destroy the drag Item widget (cursor emptied). The cursor stack
    /// itself is untouched.
    fn hide_cursor_widget(&mut self, sid: SessionId) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        if let Some(w) = out.cursor_wid.take() {
            out.send(wdg::dst_wdg(w));
            out.widgets.remove(&w);
        }
    }

    /// Create the drag Item widget for the current cursor stack.
    fn create_cursor_widget(&mut self, sid: SessionId) {
        let Some(stack) = self.sessions.get(&sid).and_then(|o| o.cursor) else {
            return;
        };
        let res_name = self
            .world
            .res
            .name(stack.res)
            .unwrap_or("gfx/invobjs/stone");
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        if out.cursor_wid.is_some() {
            return; // one drag widget at a time
        }
        let wire = out.res.wire_named(stack.res, res_name);
        if let Some((name, ver)) = out.res.pending_announce(wire) {
            out.send(wdg::resid(wire, name, ver));
            out.res.mark_announced(wire);
        }
        let w = out.new_wid("item");
        out.cursor_wid = Some(w);
        // Item factory args (Item.java): res, q, drag flag, drag Coord
        // (grab offset), tooltip, num. Parent 0 = root: the drag item
        // floats over every window.
        out.send(wdg::new_wdg(
            w,
            "item",
            0,
            0,
            0,
            &[
                ListVal::I(wire as i32),
                ListVal::I(stack.ql as i32),
                ListVal::I(1),
                ListVal::C(0, 0),
                // Server tooltip = display name; food-and-fep.md Item.name()
                // precedence makes this the fep.conf lookup key for food.
                ListVal::S(stack.label.to_owned()),
                ListVal::I(stack.count as i32),
            ],
        ));
    }

    /// Rebuild inventory items: destroy old item widgets, create new ones.
    /// The cursor drag widget is not touched (it is not an inventory
    /// item); refresh_inventory runs on every cursor flow, so a stale
    /// dst here would kill the drag widget the sync helper just made.
    pub(super) fn refresh_inventory(&mut self, sid: SessionId) {
        let Some(inv_wid) = self.inv_window(sid) else {
            return;
        };
        let items: Vec<InvStack> = self
            .world
            .player(sid)
            .map(|p| p.inv.clone())
            .unwrap_or_default();
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        let old: Vec<u16> = out
            .widgets
            .iter()
            .filter(|(_, t)| t.as_str() == "item")
            .map(|(id, _)| *id)
            .filter(|id| Some(*id) != out.cursor_wid)
            .collect();
        for id in old {
            out.send(wdg::dst_wdg(id));
        }
        out.item_wids.clear();
        for (n, stack) in items.iter().enumerate() {
            let res_name = self
                .world
                .res
                .name(stack.res)
                .unwrap_or("gfx/invobjs/stone");
            let wire = out.res.wire_named(stack.res, res_name);
            if let Some((name, ver)) = out.res.pending_announce(wire) {
                out.send(wdg::resid(wire, name, ver));
                out.res.mark_announced(wire);
            }
            let w = out.new_wid("item");
            out.item_wids.insert(w, n);
            let x = 15 + (n as i32 % 4) * 40;
            let y = 15 + (n as i32 / 4) * 40;
            out.send(wdg::new_wdg(
                w,
                "item",
                x,
                y,
                inv_wid,
                &[
                    ListVal::I(wire as i32),
                    ListVal::I(stack.ql as i32),
                    ListVal::I(0),
                    // Server tooltip = display name; food-and-fep.md Item.name()
                    // precedence makes this the fep.conf lookup key for food.
                    ListVal::S(stack.label.to_owned()),
                    ListVal::I(stack.count as i32),
                ],
            ));
        }
    }

    pub(super) fn inv_drop(&mut self, sid: SessionId, _wid: u16, _args: &[hnh_proto::ListArg]) {
        // Inventory "drop": the client sends it when the held (cursor) item
        // is released onto an inventory grid; the stack returns to storage.
        // Ground drops ride the mapview `drop` wdgmsg instead.
        let stack = {
            let Some(out) = self.sessions.get_mut(&sid) else {
                return;
            };
            out.cursor.take()
        };
        let Some(stack) = stack else {
            return;
        };
        self.hide_cursor_widget(sid);
        self.grant_pickup(sid, stack);
    }

    /// Place a picked-up stack (ground-drop click, cursor release onto the
    /// inventory grid, or a relayed cross-node pickup ack): when the cursor
    /// already drags the SAME resource, the counts merge onto the cursor
    /// (redirection - no failed pickup, one drag stack); otherwise the
    /// stack stores into the inventory, merging into an existing
    /// same-resource stack when one exists (items-and-quality.md leaves
    /// stacking policy to the server; count-weighted quality average, see
    /// InvStack::absorb).
    pub(super) fn grant_pickup(&mut self, sid: SessionId, stack: InvStack) {
        // Cursor redirection: same resource on the cursor absorbs the new
        // stack; the drag widget's count syncs through sync_cursor_widget.
        let cursor_same = self
            .sessions
            .get(&sid)
            .and_then(|o| o.cursor.as_ref())
            .is_some_and(|c| c.res == stack.res);
        if cursor_same {
            if let Some(out) = self.sessions.get_mut(&sid) {
                if let Some(c) = out.cursor.as_mut() {
                    c.absorb(&stack);
                }
            }
            self.sync_cursor_widget(sid);
            return;
        }
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            // No session player (should not happen for a pickup click):
            // dropping the stack would lose items; keep it safe instead.
            debug!(sid, "grant_pickup without session player");
            return;
        };
        // Inventory merge: the first same-resource stack absorbs; a new
        // resource creates its own stack.
        let existing = self.world.players[pidx]
            .inv
            .iter_mut()
            .find(|s| s.res == stack.res);
        match existing {
            Some(s) => s.absorb(&stack),
            None => self.world.players[pidx].inv.push(stack),
        }
        self.refresh_inventory(sid);
    }

    // ------------------------------------------------------------------
    // Equipment (the Equipory paperdoll, docs/mechanics/items/
    // items-and-quality.md): widget type "epry", 16 wire-indexed slots,
    // full-state "set" sync + "ava" avatar gob binding. Equipping rides
    // the cursor item: "drop" onto a slot stores it, "take" retrieves it.
    // ------------------------------------------------------------------

    pub(super) fn epry_window(&self, sid: SessionId) -> Option<u16> {
        self.sessions
            .get(&sid)?
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "epry")
            .map(|(id, _)| *id)
    }

    /// The equipped pieces' inventory resource names for one player, in
    /// slot order. Only resources the equip::table knows how to render
    /// pass the filter (the equip module drops non-wearables itself; the
    /// empty-name fallback guards a stale resource index after a pack
    /// change).
    pub(super) fn player_equip_names(&self, player: usize) -> Vec<&'static str> {
        self.world.players[player]
            .equip
            .iter()
            .flatten()
            .map(|s| self.world.res.name(s.res).unwrap_or(""))
            .filter(|n| !n.is_empty())
            .collect()
    }

    /// Create the paperdoll window if absent, then resync its contents.
    pub(super) fn open_epry(&mut self, sid: SessionId) {
        if self.epry_window(sid).is_none() {
            let Some(out) = self.sessions.get_mut(&sid) else {
                return;
            };
            let w = out.new_wid("epry");
            out.send(wdg::new_wdg(w, "epry", 0, 0, 0, &[]));
        }
        self.send_epry_state(sid);
    }

    /// Full paperdoll resync: RMSG_WDGMSG "set" (for each of the 16 slots
    /// in order: -1, or wire resid + quality + optional tooltip) followed
    /// by "ava" (the avatar gob the window previews).
    pub(super) fn send_epry_state(&mut self, sid: SessionId) {
        let Some(w) = self.epry_window(sid) else {
            return;
        };
        let Some(&pidx) = self.world.by_session.get(&sid) else {
            return;
        };
        // Phase 1 (world borrow): snapshot the equipped stacks together
        // with their resource names.
        let equipped: Vec<Option<(InvStack, &'static str)>> = self.world.players[pidx]
            .equip
            .iter()
            .map(|slot| {
                slot.map(|s| {
                    let name = self.world.res.name(s.res).unwrap_or("gfx/invobjs/unknown");
                    (s, name)
                })
            })
            .collect();
        let gob = self.world.players[pidx].gob;
        // Phase 2 (session borrow): wire ids, announcements, uimsgs.
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        let mut args: Vec<ListVal> = Vec::with_capacity(48);
        for slot in &equipped {
            match slot {
                Some((s, name)) => {
                    let wire = out.res.wire_named(s.res, name);
                    if let Some((n, v)) = out.res.pending_announce(wire) {
                        out.send(wdg::resid(wire, n, v));
                        out.res.mark_announced(wire);
                    }
                    args.push(ListVal::I(wire as i32));
                    args.push(ListVal::I(s.ql as i32));
                    // Armor pieces append the "Armor class: D/A" tooltip
                    // line; Equipory.calcAC parses and sums it per slot
                    // (combat-system.md: the server owns the numbers).
                    let tt = match crate::armor::ac_line(name, i32::from(s.ql)) {
                        Some(line) if !s.label.is_empty() => {
                            format!("{}\n{}", s.label, line)
                        }
                        Some(line) => line,
                        None => s.label.to_owned(),
                    };
                    if !tt.is_empty() {
                        args.push(ListVal::S(tt));
                    }
                }
                None => args.push(ListVal::I(-1)),
            }
        }
        out.send(wdg::wdgmsg(w, "set", &args));
        out.send(wdg::wdgmsg(w, "ava", &[ListVal::I(gob)]));
    }

    /// epry "drop" (slot): store the held cursor item into slot `ep`.
    /// Slot -1 (window background) is a deliberate no-op.
    pub(super) fn epry_drop(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
        let ep = args.first().and_then(|a| a.as_int()).unwrap_or(-1);
        if !(0..16).contains(&ep) {
            return;
        }
        let Some(&pidx) = self.world.by_session.get(&sid) else {
            return;
        };
        if self.world.players[pidx].equip[ep as usize].is_some() {
            return; // slot occupied
        }
        let stack = {
            let Some(out) = self.sessions.get_mut(&sid) else {
                return;
            };
            out.cursor.take()
        };
        let Some(stack) = stack else {
            return; // empty hand
        };
        self.hide_cursor_widget(sid);
        self.world.players[pidx].equip[ep as usize] = Some(stack);
        self.send_epry_state(sid);
        self.stream_equipment_change(pidx);
    }

    /// epry "take" (slot): pick the equipped item back onto the cursor.
    pub(super) fn epry_take(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
        let ep = args.first().and_then(|a| a.as_int()).unwrap_or(-1);
        if !(0..16).contains(&ep) {
            return;
        }
        let Some(&pidx) = self.world.by_session.get(&sid) else {
            return;
        };
        if self
            .sessions
            .get(&sid)
            .map(|o| o.cursor.is_some())
            .unwrap_or(true)
        {
            return; // hand already full
        }
        let Some(stack) = self.world.players[pidx].equip[ep as usize].take() else {
            return; // empty slot
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.cursor = Some(stack);
        self.send_epry_state(sid);
        self.stream_equipment_change(pidx);
        self.sync_cursor_widget(sid);
    }

    /// Broadcast one player's equipment change to every viewer: the
    /// world drawable (OD_LAYERS) re-streams with the piece layers and
    /// the doll attribute (OD_AVATAR) recomposites on the owner.
    fn stream_equipment_change(&mut self, pidx: usize) {
        let gob = self.world.players[pidx].gob;
        let Some(slot) = self.world.gobs.get(gob) else {
            return;
        };
        self.stream_pose(slot);
        self.stream_avatar(slot);
    }

    // ------------------------------------------------------------------
    // Crop farming (docs/mechanics/livestock/farming-and-plants.md)
    // ------------------------------------------------------------------

    /// Inventory "take": move one stack onto the cursor. The client then
    /// aims with the mouse; a map click arrives as mapview `itemact`.
    pub(super) fn inv_take(&mut self, sid: SessionId, wid: u16) {
        let stack_idx = self
            .sessions
            .get(&sid)
            .and_then(|o| o.item_wids.get(&wid).copied());
        let Some(stack_idx) = stack_idx else {
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        if out.cursor.is_some() {
            return; // one cursor item at a time
        }
        let Some(stack) = self
            .world
            .player(sid)
            .and_then(|p| p.inv.get(stack_idx).copied())
        else {
            return;
        };
        if let Some(p) = self.world.player_mut(sid) {
            p.inv.remove(stack_idx);
        }
        out.cursor = Some(stack);
        self.refresh_inventory(sid);
        self.sync_cursor_widget(sid);
    }

    /// MapView `itemact(cc0, mc, modflags[, gobid, gobrc])`: the player
    /// clicked the map with an item on the cursor. cc0 is a screen
    /// coordinate; the world-space target is the second coord (mc).
    pub(super) fn on_map_itemact(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
        let mc = args.iter().filter_map(|a| a.as_coord()).nth(1);
        let Some((mx, my)) = mc else { return };
        let Some(cursor) = self.sessions.get(&sid).and_then(|o| o.cursor) else {
            return;
        };
        let label = cursor.label;
        // Gob-targeted itemact (client sends [cc, mc, modflags, gobid,
        // gobrc] when the click lands on a gob; MapView.iteminteract):
        // plans sink the held material, stations take fuel or input.
        if let Some(gob) = args.get(3).and_then(|a| a.as_int()) {
            if self.world.plans.contains_key(&gob) {
                self.sink_material(sid, gob, cursor);
                self.sync_cursor_widget(sid);
                return;
            }
            if self.world.stations.contains_key(&gob) {
                self.station_itemact(sid, gob, cursor);
                self.sync_cursor_widget(sid);
                return;
            }
            // Food Trough (session 48): fodder deliveries top up the
            // store. Cross-node troughs are an open policy (like guest
            // quell) - the local authority owns its own troughs.
            if self.world.troughs.contains_key(&gob) {
                self.trough_itemact(sid, gob, cursor);
                self.sync_cursor_widget(sid);
                return;
            }
            // Guest station (session 33): fuel/input delivery relays the
            // held stack to the station's authority. The cursor stack is
            // NOT consumed before the ack (seed-safe): the authority
            // answers StationItemAck and the home node consumes exactly
            // one unit on FuelAdded/InputLoaded, keeping the whole stack
            // on every refusal - parity with the local refusal paths.
            if let Some(crate::nodes::GuestKind::Static {
                class: crate::nodes::StaticClass::Station,
                ..
            }) = self.world.guests.get(&gob).map(|g| &g.kind)
            {
                let res_name = self
                    .world
                    .res
                    .name(cursor.res)
                    .unwrap_or("gfx/invobjs/stone")
                    .to_owned();
                let player_gob = self.world.player(sid).map(|p| p.gob);
                let pos = self.world.guests.get(&gob).map(|g| g.pos);
                if let (Some(player_gob), Some(pos), Some(c)) =
                    (player_gob, pos, self.cluster.as_ref())
                {
                    let authority = self.cell_owner(crate::visidx::cell_of(pos.0, pos.1));
                    c.mesh.send(
                        authority,
                        crate::nodes::NodeMsg::RelayStationItem {
                            player: player_gob,
                            target: gob,
                            stack: crate::nodes::StaticStack {
                                res: res_name,
                                count: 1,
                                ql: cursor.ql,
                                label: cursor.label.to_owned(),
                            },
                        },
                    );
                    debug!(sid, gob, authority, "relay station item sent");
                }
                return;
            }
            // Fall through to the map-space behaviors below for other
            // gob kinds (legacy iteminteract semantics).
        }
        match farm::spec_by_seed_label(label) {
            Some(spec) => {
                self.plant_seed(sid, spec, Self::tile_coord(mx, my), cursor);
                self.sync_cursor_widget(sid);
            }
            None => {
                // Not a seed: legacy map click with a cursor item drops it.
                let pos = self
                    .world
                    .player(sid)
                    .and_then(|p| self.world.gobs.get(p.gob))
                    .map(|slot| self.world.gobs.pos[slot]);
                if let Some(pos) = pos {
                    let name = self
                        .world
                        .res
                        .name(cursor.res)
                        .unwrap_or("gfx/invobjs/stone")
                        .to_owned();
                    self.spawn_drop_near(pos, leak_static(&name), cursor.ql, cursor.label);
                    if let Some(out) = self.sessions.get_mut(&sid) {
                        out.cursor = None;
                    }
                }
                self.sync_cursor_widget(sid);
            }
        }
    }

    /// MapView `drop(modflags)`: release the held stack onto the ground
    /// near the player (Item.java drag release onto the map target;
    /// legacy spawns a ground gob). The drag widget is destroyed and the
    /// cursor cleared; the dropped gob follows the normal pickup/despawn
    /// path (`Kind::Drop`).
    pub(super) fn on_map_drop(&mut self, sid: SessionId) {
        let Some(cursor) = self.take_cursor_stack(sid) else {
            return;
        };
        self.hide_cursor_widget(sid);
        let pos = self
            .world
            .player(sid)
            .and_then(|p| self.world.gobs.get(p.gob))
            .map(|slot| self.world.gobs.pos[slot]);
        if let Some(pos) = pos {
            let name = self
                .world
                .res
                .name(cursor.res)
                .unwrap_or("gfx/invobjs/stone")
                .to_owned();
            self.spawn_drop_near(pos, leak_static(&name), cursor.ql, cursor.label);
            info!(
                sid,
                label = cursor.label,
                count = cursor.count,
                "ground drop"
            );
        }
        self.refresh_inventory(sid);
    }

    /// Remove the cursor stack from the session (widget not touched;
    /// callers own the drag-widget sync).
    fn take_cursor_stack(&mut self, sid: SessionId) -> Option<InvStack> {
        self.sessions.get_mut(&sid).and_then(|o| o.cursor.take())
    }

    /// Consume ONE unit from the cursor stack (relay ack path): a
    /// multi-unit stack keeps the rest, an exhausted stack clears the
    /// cursor. Inventory refresh + drag-widget sync match the local
    /// itemact path so the client UI never sees a stale cursor.
    pub(super) fn consume_cursor_unit(&mut self, sid: SessionId) {
        if let Some(cursor) = self.sessions.get_mut(&sid).and_then(|o| o.cursor.as_mut()) {
            cursor.count = cursor.count.saturating_sub(1);
            let rest = cursor.count;
            if rest == 0 {
                self.sessions.get_mut(&sid).expect("cursor above").cursor = None;
            }
        }
        self.refresh_inventory(sid);
        self.sync_cursor_widget(sid);
    }

    /// Item right-click (`iact`): open a flower menu for foods.
    pub(super) fn on_item_iact(&mut self, sid: SessionId, wid: u16) {
        let stack_idx = self
            .sessions
            .get(&sid)
            .and_then(|o| o.item_wids.get(&wid).copied());
        let Some(stack_idx) = stack_idx else {
            return;
        };
        let label = self
            .world
            .player(sid)
            .and_then(|p| p.inv.get(stack_idx))
            .map(|s| s.label)
            .unwrap_or("");
        if label.is_empty() || self.fep.get(label).is_none() {
            debug!(sid, label, "iact on non-food item: no menu");
            return;
        }
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        // One flower menu at a time per session.
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
            &[ListVal::S("Eat".to_owned())],
        ));
        out.item_menu = Some((w, stack_idx));
    }

    /// Flower menu petal click: `cl <i>`; petal 0 confirms.
    pub(super) fn on_flower_choice(&mut self, sid: SessionId, wid: u16, choice: i32) {
        // Player party menus take precedence (opened last, one menu at a
        // time per session; the openers close any earlier menu).
        let player_menu = self
            .sessions
            .get(&sid)
            .and_then(|o| o.player_menu)
            .map(|(w, _)| w);
        if player_menu == Some(wid) {
            self.on_party_menu_choice(sid, wid, choice);
            return;
        }
        // Crop harvest menus take precedence over the item eat menu.
        let crop_menu = self
            .sessions
            .get(&sid)
            .and_then(|o| o.crop_menu)
            .map(|(w, _)| w);
        if crop_menu == Some(wid) {
            self.harvest_crop(sid, wid, choice);
            return;
        }
        // Tamed-animal production menus (session 47): Milk / Shear.
        let animal_menu = self
            .sessions
            .get(&sid)
            .and_then(|o| o.animal_menu)
            .map(|(w, _)| w);
        if animal_menu == Some(wid) {
            self.apply_animal_choice(sid, wid, choice);
            return;
        }
        // Station Light/Extinguish menus.
        let station_menu = self
            .sessions
            .get(&sid)
            .and_then(|o| o.station_menu)
            .map(|(w, _, _)| w);
        if station_menu == Some(wid) {
            self.apply_station_choice(sid, wid, choice);
            return;
        }
        let pending = self
            .sessions
            .get(&sid)
            .and_then(|o| o.item_menu)
            .filter(|(w, _)| *w == wid);
        let Some((_, stack_idx)) = pending else {
            return;
        };
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.item_menu = None;
        out.send(wdg::dst_wdg(wid));
        if choice != 0 {
            // Confirm the cancel client-side (FlowerMenu.uimsg "cancel").
            out.send(wdg::wdgmsg(wid, "cancel", &[]));
            return;
        }
        out.send(wdg::wdgmsg(wid, "act", &[ListVal::I(0)]));
        self.eat_item(sid, stack_idx);
    }

    /// Apply one unit of food: energy fill, FEP grant, HHP healing,
    /// attribute gain on reaching the requirement (fandom FEP loop).
    fn eat_item(&mut self, sid: SessionId, stack_idx: usize) {
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return;
        };
        let Some(stack) = self.world.players[pidx].inv.get(stack_idx).copied() else {
            return;
        };
        let Some(feps) = self.fep.get(stack.label) else {
            debug!(sid, label = stack.label, "eat: no fep entry");
            return;
        };
        let total_fep: f32 = feps
            .iter()
            .filter(|(a, _)| *a != FepAttr::Hhp)
            .map(|(_, v)| v)
            .sum();
        let hhp: f32 = feps
            .iter()
            .find(|(a, _)| *a == FepAttr::Hhp)
            .map(|(_, v)| *v)
            .unwrap_or(0.0);
        // Consume one unit, then roll the attribute draw (single world
        // borrow at a time; the RNG lives on World).
        let roll = self.world.next_ai_rand(1_000_000) as u32;
        {
            let inv = &mut self.world.players[pidx].inv;
            inv[stack_idx].count -= 1;
            if inv[stack_idx].count == 0 {
                inv.remove(stack_idx);
            }
        }
        let p = &mut self.world.players[pidx];
        // Energy fill: server policy (legacy per-food fill unknown,
        // food-and-fep.md open question 1) scaled by the food's FEP total.
        let fill = (10.0f32 + total_fep * 1.5).min(60.0) as i32;
        p.energy = (p.energy + fill).min(100);
        // HHP heals the hard pool directly (fep.conf semantics, doc note 10).
        if hhp > 0.0 {
            p.hp = (p.hp + hhp.round() as i32).min(100);
        }
        // Grant FEPs (tenths), then check the attribute requirement.
        p.fep.grant(feps, stack.ql);
        let cap = p
            .attrs
            .iter()
            .filter(|(k, _)| crate::craft::FepAttr::from_key(&k.to_uppercase()).is_some())
            .map(|(_, v)| *v)
            .max()
            .unwrap_or(10);
        // Pre-rolled weighted draw (single call in pick_gain).
        let mut rng = || roll;
        if p.fep.total() >= cap * 10 {
            if let Some(gain) = p.fep.pick_gain(&mut rng) {
                *p.attrs.entry(gain.to_owned()).or_insert(10) += 1;
                info!(sid, attr = gain, "attribute raised by food");
            }
            p.fep.reset();
        }
        self.refresh_inventory(sid);
        self.push_food_msg(sid);
        self.push_cattr(sid);
        info!(sid, label = stack.label, fill, "ate food");
    }

    /// Push the `food` uimsg on the chr widget: cap in tenths, then
    /// (id, tenths, color) triples (CharWnd.FoodMeter.update contract).
    pub(super) fn push_food_msg(&mut self, sid: SessionId) {
        let chr_wid = match self.sessions.get(&sid).and_then(|o| o.chr_window()) {
            Some(w) => w,
            None => return,
        };
        let Some(p) = self.world.player(sid) else {
            return;
        };
        let cap = p
            .attrs
            .iter()
            .filter(|(k, _)| crate::craft::FepAttr::from_key(&k.to_uppercase()).is_some())
            .map(|(_, v)| *v)
            .max()
            .unwrap_or(10)
            * 10;
        let mut entries: Vec<(&'static str, i32)> =
            p.fep.acc.iter().map(|(k, v)| (*k, *v)).collect();
        entries.sort_unstable_by_key(|(k, _)| *k);
        let mut args: Vec<ListVal> = vec![ListVal::I(cap)];
        for (id, tenths) in entries {
            let attr = FepAttr::from_key(&id.to_uppercase()).unwrap_or(FepAttr::Str);
            let (r, g, b, a) = attr.color();
            args.push(ListVal::S(id.to_owned()));
            args.push(ListVal::I(tenths));
            args.push(ListVal::Col(r, g, b, a));
        }
        if let Some(out) = self.sessions.get_mut(&sid) {
            out.send(wdg::wdgmsg(chr_wid, "food", &args));
        }
    }
}
