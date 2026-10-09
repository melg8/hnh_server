//! The craft menu flow: the make widget, recipe resolution and the
//! roast chain.

use super::*;

impl Game {
    pub(super) fn on_menu_action(&mut self, sid: SessionId, action: &[String]) {
        // Craft leaves send act("craft", <recipe-id>) via MenuGrid.
        if action.len() >= 2 && action[0] == "craft" {
            let recipe_id = action[1].as_str();
            // The roast pagina carries ad ["craft", "roast"]; it maps to a
            // dynamic recipe resolved per attempt (any raw meat in scope).
            let known =
                recipe_id == "roast" || crate::craft::RECIPES.iter().any(|r| r.id == recipe_id);
            if !known {
                info!(sid, recipe = recipe_id, "unknown craft id: ignoring");
                return;
            }
            self.open_make_window(sid, recipe_id);
        } else if action.len() >= 2 && action[0] == "atk" {
            // Fight-window maneuvers: paginae/atk/* buttons send
            // act("atk", <maneuver-id>) through MenuGrid.
            self.on_maneuver(sid, action[1].as_str());
        } else if action.first().map(String::as_str) == Some("plow") {
            // Plow Field pagina (ad ["plow"]): arm tile plowing; the next
            // map click plows the tile under the cursor.
            info!(sid, "plow pagina armed");
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.pending_plow = true;
            }
        } else if !action.is_empty() {
            // Build paginae send their own ad string ("act(\"oven\")"),
            // decoded from the res pack action layers.
            let ad = action[0].as_str();
            if let Some(spec) = crate::build::buildable_by_ad(ad) {
                self.arm_build_placement(sid, spec);
            } else {
                debug!(sid, ?action, "menu action");
            }
        }
    }

    /// Open the `make` widget for a recipe and push its `pop` contents:
    /// a flat (wire-id, count) list, inputs terminated by -1, then outputs.
    fn open_make_window(&mut self, sid: SessionId, recipe_id: &str) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        // One crafting dialog at a time (UI.make_window is a single slot).
        if let Some(old) = out.craft_window {
            out.send(wdg::dst_wdg(old));
            out.craft_window = None;
            out.craft_recipe = None;
        }
        let (title, pop): (&str, Vec<ListVal>) = if recipe_id == "roast" {
            ("Roasted Meat", Vec::new())
        } else {
            match crate::craft::RECIPES.iter().find(|r| r.id == recipe_id) {
                None => return,
                Some(r) => {
                    let mut pop = Vec::new();
                    for (resname, count) in r.inputs {
                        let gidx = self.world.res.intern(resname);
                        let wire = out.res.wire_named(gidx, resname);
                        if let Some((n, v)) = out.res.pending_announce(wire) {
                            out.send(wdg::resid(wire, n, v));
                            out.res.mark_announced(wire);
                        }
                        pop.push(ListVal::I(wire as i32));
                        pop.push(ListVal::I(*count as i32));
                    }
                    pop.push(ListVal::I(-1));
                    for (resname, count) in r.outputs {
                        let gidx = self.world.res.intern(resname);
                        let wire = out.res.wire_named(gidx, resname);
                        if let Some((n, v)) = out.res.pending_announce(wire) {
                            out.send(wdg::resid(wire, n, v));
                            out.res.mark_announced(wire);
                        }
                        pop.push(ListVal::I(wire as i32));
                        pop.push(ListVal::I(*count as i32));
                    }
                    (r.name, pop)
                }
            }
        };
        // Dynamic roast pop: one input (first raw meat present) and its
        // mapped output; rebuilt per attempt when the raw stack changes.
        let pop = if recipe_id == "roast" {
            let meat = self
                .world
                .player(sid)
                .map(|p| {
                    p.inv
                        .iter()
                        .find_map(|s| crate::craft::roast_result(s.label).map(|out| (s.label, out)))
                })
                .unwrap_or(None);
            match meat {
                Some((raw, roasted)) => {
                    let mut pop = Vec::new();
                    for resname in [raw, roasted] {
                        let gidx = self.world.res.intern("gfx/invobjs/meat");
                        let wire = out.res.wire_named(gidx, "gfx/invobjs/meat");
                        if let Some((n, v)) = out.res.pending_announce(wire) {
                            out.send(wdg::resid(wire, n, v));
                            out.res.mark_announced(wire);
                        }
                        pop.push(ListVal::I(wire as i32));
                        pop.push(ListVal::I(1));
                        if resname == raw {
                            pop.push(ListVal::I(-1));
                        }
                    }
                    pop
                }
                None => vec![ListVal::I(-1)],
            }
        } else {
            pop
        };
        let w = out.new_wid("make");
        out.send(wdg::new_wdg(
            w,
            "make",
            350,
            200,
            0,
            &[ListVal::S(title.to_owned())],
        ));
        out.send(wdg::wdgmsg(w, "pop", &pop));
        out.craft_window = Some(w);
        out.craft_recipe = Some(recipe_id.to_owned());
        info!(sid, recipe = recipe_id, "makewindow opened");
    }

    /// Client pressed Craft (mode 0) or Craft All (mode 1) on the make
    /// widget. Loop while preconditions hold; stop after the last success.
    pub(super) fn on_make_cmd(&mut self, sid: SessionId, mode: i32) {
        let Some(recipe_id) = self.sessions.get(&sid).and_then(|o| o.craft_recipe.clone()) else {
            return;
        };
        let max_iter = if mode == 1 { 64 } else { 1 };
        let mut made = 0u32;
        for _ in 0..max_iter {
            if !self.craft_once(sid, &recipe_id) {
                break;
            }
            made += 1;
        }
        if made > 0 {
            self.refresh_inventory(sid);
            // Re-push pop so the window reflects any roast-input change.
            self.open_make_window(sid, &recipe_id);
        }
        info!(sid, recipe = recipe_id, made, "craft batch done");
    }

    /// One craft attempt: validate, consume (lowest quality first), produce.
    /// Returns false when a precondition fails (ends batch crafting).
    pub(super) fn craft_once(&mut self, sid: SessionId, recipe_id: &str) -> bool {
        if recipe_id == "roast" {
            return self.roast_once(sid);
        }
        let Some(recipe) = crate::craft::RECIPES.iter().find(|r| r.id == recipe_id) else {
            return false;
        };
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return false;
        };
        // Tool requirement (session 46): the recipe's tool resource must
        // sit in the inventory or any equipment slot. Checked before the
        // consume pass so a refusal never destroys ingredients.
        if let Some(tool) = recipe.tool {
            let tool_gidx = self.world.res.intern(tool);
            let inv_has = self.world.players[pidx]
                .inv
                .iter()
                .any(|s| s.res == tool_gidx);
            let equip_has = self.world.players[pidx]
                .equip
                .iter()
                .flatten()
                .any(|s| s.res == tool_gidx);
            if !inv_has && !equip_has {
                let tool_name = self.world.res.name(tool_gidx).unwrap_or(tool);
                let msg = format!("You need the {} to make that.", tool_name);
                self.chat_line(sid, &msg, Some((255, 128, 128)));
                return false;
            }
        }
        // Validate: every input present in the required quantity.
        for (resname, need) in recipe.inputs {
            let gidx = self.world.res.intern(resname);
            let have: u32 = self.world.players[pidx]
                .inv
                .iter()
                .filter(|s| s.res == gidx)
                .map(|s| s.count)
                .sum();
            if have < *need {
                debug!(sid, recipe = recipe.id, resname, "missing ingredient");
                return false;
            }
        }
        // Consume lowest-quality-first so Craft All rolls per-iteration
        // quality from the actually consumed items (crafting doc).
        // `consumed` keeps the flat unit list (pre-36 unit weighting);
        // `per_type` additionally accumulates (qsum, units) per input
        // TYPE for the RoB Legacy:Quality type-weighted formula.
        let mut consumed: Vec<(u8, u32)> = Vec::new(); // (ql, units)
        let mut per_type: Vec<(u32, u32)> = vec![(0, 0); recipe.inputs.len()];
        for (ti, (resname, need)) in recipe.inputs.iter().enumerate() {
            let gidx = self.world.res.intern(resname);
            let mut remaining = *need;
            while remaining > 0 {
                // Find the lowest-quality non-empty stack of this resource.
                let slot = {
                    let inv = &self.world.players[pidx].inv;
                    inv.iter().enumerate().fold(None::<usize>, |best, (i, s)| {
                        if s.res == gidx && s.count > 0 {
                            match best {
                                None => Some(i),
                                Some(b) if s.ql < inv[b].ql => Some(i),
                                other => other,
                            }
                        } else {
                            best
                        }
                    })
                };
                let Some(slot) = slot else {
                    // Validation passed but stacks emptied mid-loop: fail safe.
                    return false;
                };
                let stack = &mut self.world.players[pidx].inv[slot];
                let take = remaining.min(stack.count);
                stack.count -= take;
                consumed.push((stack.ql, take));
                per_type[ti].0 += u32::from(stack.ql) * take;
                per_type[ti].1 += take;
                remaining -= take;
            }
        }
        self.world.players[pidx].inv.retain(|s| s.count > 0);
        // Output quality. With per-type weights (RoB Legacy:Quality):
        // each input TYPE first averages its own consumed units, then the
        // type averages combine as sum(q_t * w_t)/sum(w_t). Empty
        // q_weights keeps the pre-36 behavior: every consumed UNIT weighs
        // equally across types.
        let mut q = if recipe.q_weights.is_empty() {
            let total_w: u32 = consumed.iter().map(|(_, w)| w).sum();
            let qsum: u32 = consumed.iter().map(|(q, w)| u32::from(*q) * w).sum();
            (qsum.checked_div(total_w).unwrap_or(10) as i32).max(1)
        } else {
            let mut qs: u32 = 0;
            let mut ws: u32 = 0;
            for (ti, (qsum, units)) in per_type.iter().enumerate() {
                let w = recipe.q_weights.get(ti).copied().unwrap_or(1);
                let qt = qsum.checked_div((*units).max(1)).unwrap_or(10);
                qs += qt * w;
                ws += w;
            }
            (qs.checked_div(ws.max(1)).unwrap_or(10) as i32).max(1)
        };
        // Softcap by the crafter's relevant attribute (skill stand-in):
        // q = (q + attr)/2 when attr < q (crafting-and-building.md).
        let attr_val = self.world.players[pidx]
            .attrs
            .get(recipe.softcap_attr)
            .copied()
            .unwrap_or(10);
        if attr_val < q {
            q = (attr_val + q) / 2;
        }
        let out_q = q.clamp(1, 255) as u8;
        for (oi, (resname, count)) in recipe.outputs.iter().enumerate() {
            let gidx = self.world.res.intern(resname);
            // Session 71 (bake chain): crafted stacks carry the recipe's
            // display name as the stack label. The station input gates
            // match on the display label (craft::BAKE_MAP /
            // GRIND_MAP / SMELT_MAP / KILN_MAP keys are display names),
            // and a label-less crafted stack made every hand-crafted
            // station input - the Bread Dough leg - unprocessable
            // ("The station cannot process that."). Multi-output
            // recipes label only the PRIMARY output (the byproducts -
            // the dough recipe's returned bucket - keep the empty
            // label, the client falls back to the resource name).
            let out = InvStack {
                res: gidx,
                count: *count,
                ql: out_q,
                label: if oi == 0 { recipe.name } else { "" },
            };
            // Merge into an existing same-resource stack (absorb policy)
            // so repeat crafts fill one stack, not one slot per craft.
            match self.world.players[pidx]
                .inv
                .iter_mut()
                .find(|s| s.res == gidx)
            {
                Some(s) => s.absorb(&out),
                None => self.world.players[pidx].inv.push(out),
            }
        }
        // First-time discoveries grant LP (learning doc); keep it modest.
        self.world.players[pidx].lp += 1;
        self.push_cattr(sid);
        true
    }

    /// One roast attempt: find a raw meat stack, convert one unit.
    fn roast_once(&mut self, sid: SessionId) -> bool {
        let Some(pidx) = self.world.by_session.get(&sid).copied() else {
            return false;
        };
        let pos = self.world.players[pidx]
            .inv
            .iter()
            .position(|s| crate::craft::roast_result(s.label).is_some() && s.count > 0);
        let Some(pos) = pos else {
            debug!(sid, "roast: no raw meat in inventory");
            return false;
        };
        let stack = &mut self.world.players[pidx].inv[pos];
        let roasted = crate::craft::roast_result(stack.label).unwrap_or(stack.label);
        stack.count -= 1;
        let out_ql = stack.ql;
        let raw = stack.label;
        if stack.count == 0 {
            self.world.players[pidx].inv.remove(pos);
        }
        // Merge into an existing roasted stack when one exists (absorb
        // policy) instead of stacking duplicates per roast.
        let out_stack = InvStack {
            res: self.world.res.intern("gfx/invobjs/meat"),
            count: 1,
            ql: out_ql,
            label: roasted,
        };
        match self.world.players[pidx]
            .inv
            .iter_mut()
            .find(|s| s.res == out_stack.res && s.label == out_stack.label)
        {
            Some(s) => s.absorb(&out_stack),
            None => self.world.players[pidx].inv.push(out_stack),
        }
        debug!(sid, raw, roasted, "roasted one meat");
        true
    }

    // ------------------------------------------------------------------
    // Eating (food-and-fep.md: eat flow + FEP accumulation)
    // ------------------------------------------------------------------
}
