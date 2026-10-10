//! Session lifecycle + world entry: registering a newly accepted
//! session (character-list push), the enter_world pipeline (spawn
//! position or persisted restore, bootstrap windows, cattr/globlob
//! sync) and the char-attribute snapshot behind the chr widgets.
//!
//! Pure move out of game.rs (session 70 split).

use super::*;

impl Game {
    // ------------------------------------------------------------------
    // Session lifecycle
    // ------------------------------------------------------------------

    /// Register a newly accepted session; shows the character list.
    /// `tx` is the sink the game task writes outgoing RMSG payloads into;
    /// the session task owns the receiver. `account` is the authenticated
    /// login user (character save keys are account-scoped).
    pub(super) fn session_connected(
        &mut self,
        sid: SessionId,
        account: String,
        tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        raw_tx: tokio::sync::mpsc::Sender<crate::state::BlockBytes>,
    ) {
        let mut out = SessionOut {
            sid,
            account: account.clone(),
            queue: tx,
            raw: raw_tx,
            player_gob: None,
            visible: crate::fxhash::FxHashSet::default(),
            unacked: crate::fxhash::FxHashMap::default(),
            retx_throttle_until: Instant::now(),
            gob_acked: crate::fxhash::FxHashMap::default(),
            next_wid: 100,
            widgets: HashMap::new(),
            mapreqs: HashSet::new(),
            res: crate::resources::ResTable::new(),
            fight: crate::fight::FightState::new(),
            craft_recipe: None,
            craft_window: None,
            item_menu: None,
            item_wids: HashMap::new(),
            crop_menu: None,
            animal_menu: None,
            chat_wid: 0,
            party_wid: 0,
            player_menu: None,
            pending_plow: false,
            pending_build: None,
            station_menu: None,
            trough_menu: None,
            cursor: None,
            cursor_wid: None,
            grids_seen: HashSet::new(),
            vis_cell: None,
            vis_cache: None,
            vis_cache_pos: None,
            last_retract_tick: 0,
            visible_bits: Vec::new(),
            ack_lag_ema_ms: 0,
        };
        // Character selection UI (session-lifecycle.md 3.1).
        let w_bg = out.new_wid("img");
        let w_logo = out.new_wid("img");
        let w_list = out.new_wid("charlist");
        // Avatar layer RESIDs must be announced before the charlist add.
        // The login portrait (Charlist -> Avaview -> AvaRender) flattens
        // `Resource.layers(imgc)` of every listed resource, so the layers
        // must be IMAGE-bearing standing frames - the pose-router
        // resources ("gfx/borka/body" et al) carry no imgc layers and
        // would leave the portrait blank ("no face" bug report). The
        // same frame set layers the in-world avatar, so the login card
        // and the world character match. Octant 1 is the camera-facing
        // front octant: art_dir(1) = sprite 0, the head-on front view the
        // login card expects.
        let portrait_layers = avatar_pose_layers(false, 1);
        let mut layer_ids = Vec::with_capacity(portrait_layers.len());
        for name in portrait_layers {
            let global = self.world.res.intern(name);
            let w = out.res.wire_named(global, name);
            if let Some((n, v)) = out.res.pending_announce(w) {
                out.send(wdg::resid(w, n, v));
                out.res.mark_announced(w);
            }
            layer_ids.push(w);
        }
        info!(layers = ?portrait_layers, "charlist portrait layers announced");
        out.send(wdg::new_wdg(
            w_bg,
            "img",
            0,
            0,
            0,
            &[ListVal::S("gfx/ccscr".into())],
        ));
        out.send(wdg::new_wdg(
            w_logo,
            "img",
            274,
            10,
            0,
            &[ListVal::S("gfx/logo2".into())],
        ));
        out.send(wdg::new_wdg(
            w_list,
            "charlist",
            300,
            200,
            0,
            &[ListVal::I(6)],
        ));
        // One character per account.
        let mut add = MessageBuf::new();
        add.uint8(RMSG_WDGMSG)
            .uint16(w_list)
            .string("add")
            .lstr("Player");
        for id in &layer_ids {
            add.lint(*id as i32);
        }
        add.lend();
        out.send(add.finish());
        self.sessions.insert(sid, out);
    }

    /// Snapshot of the CAttr entries the client's CharWnd constructor
    /// requires. Names must match CharWnd.baseval/skillval/Belief exactly:
    /// `CharWnd$Attr.<init>` does `glob.cattr.get(nm)` and dereferences the
    /// result without a null check, so any missing name NPEs the client the
    /// moment SlenHud.binded() requests the char sheet after entering.
    /// Internal short keys (agi/int/con/per/cha/dex) map to the client's
    /// long names (agil/intel/cons/perc/csm/dxt); skills and beliefs are
    /// synthesized (no progression system behind them yet) and expmod
    /// defaults to 100 (learning ability percent).
    pub(super) fn char_attr_snapshot(&self, sid: SessionId) -> Vec<(&'static str, i32, i32)> {
        let Some(p) = self.world.player(sid) else {
            return Vec::new();
        };
        let base = |k: &str| p.attrs.get(k).copied().unwrap_or(10);
        let zero = |k: &str| p.attrs.get(k).copied().unwrap_or(0);
        let mut v: Vec<(&'static str, i32, i32)> = vec![
            ("pts", p.lp, p.lp),
            ("hp", 100, p.hp),
            ("energy", 100, p.energy),
            ("stamina", 100, p.stamina),
            ("str", base("str"), base("str")),
            ("agil", base("agi"), base("agi")),
            ("intel", base("int"), base("int")),
            ("cons", base("con"), base("con")),
            ("perc", base("per"), base("per")),
            ("csm", base("cha"), base("cha")),
            ("dxt", base("dex"), base("dex")),
            ("psy", base("psy"), base("psy")),
        ];
        v.push(("expmod", base("expmod"), base("expmod")));
        for s in [
            "unarmed",
            "melee",
            "ranged",
            "explore",
            "stealth",
            "sewing",
            "smithing",
            "carpentry",
            "cooking",
            "farming",
            "survive",
        ] {
            v.push((s, zero(s), zero(s)));
        }
        for b in ["life", "night", "civil", "nature", "martial", "change"] {
            v.push((b, zero(b), zero(b)));
        }
        v
    }

    /// Phase 3.2 (session-lifecycle.md): enter the world after `play`.
    pub(super) fn enter_world(&mut self, sid: SessionId, name: String) {
        self.enter_world_inner(sid, name, true);
    }

    /// World entry. `allow_defer` gates the cluster migration wait: a
    /// session whose save key lives on a peer defers once (CharQuery
    /// broadcast); the reply or the deadline re-enters with `false`, which
    /// proceeds with whatever state is locally available.
    pub(super) fn enter_world_inner(&mut self, sid: SessionId, chosen: String, allow_defer: bool) {
        let account = self
            .sessions
            .get(&sid)
            .map(|o| o.account.clone())
            .unwrap_or_default();
        let key = crate::persist::save_key(&account, &chosen);
        // Legacy saves (pre-account keying) stored characters under the
        // bare display name; adopt the snapshot into this account's
        // namespace so an existing single-node world survives the upgrade.
        if !self.save.players.contains_key(&key) && self.save.players.contains_key(&chosen) {
            if let Some(mut snap) = self.save.players.remove(&chosen) {
                info!(%chosen, %account, "adopting legacy save key");
                snap.name = key.clone();
                self.save.players.insert(key.clone(), snap);
            }
        }
        // Cluster: the key may live on a peer's shard. Ask before spawning
        // fresh - deferring keeps the charlist widgets up and the client
        // waits out a LAN round trip (bounded by the pending deadline).
        // The query re-broadcasts every CHAR_QUERY_RETRY_MS from the tick
        // drain: a link that is still negotiating buffers the retry and
        // answers as soon as the mesh converges.
        if allow_defer
            && self.cluster_nodes() > 1
            && !self.save.players.contains_key(&key)
            && self.pending_joins.iter().all(|(_, j)| j.account != account)
        {
            if let Some(c) = self.cluster.as_ref() {
                info!(sid, %key, "save key not local: querying cluster peers");
                self.pending_joins.insert(
                    sid,
                    PendingJoin {
                        account,
                        chosen,
                        deadline: std::time::Instant::now()
                            + std::time::Duration::from_millis(CHAR_QUERY_DEADLINE_MS),
                        answered: HashSet::new(),
                        // First retry one cadence after the initial query.
                        next_retry: CHAR_QUERY_RETRY_TICKS,
                    },
                );
                c.mesh.broadcast_except(
                    c.nodes.get(),
                    c.me,
                    crate::nodes::NodeMsg::CharQuery {
                        from: c.me,
                        name: key,
                    },
                );
                return;
            }
        }
        let name = chosen;
        // Destroy selection widgets.
        let widget_ids: Vec<u16> = {
            let Some(out) = self.sessions.get(&sid) else {
                return;
            };
            out.widgets
                .iter()
                .filter(|(_, t)| t.as_str() == "img" || t.as_str() == "charlist")
                .map(|(id, _)| *id)
                .collect()
        };
        if let Some(out) = self.sessions.get(&sid) {
            for id in widget_ids {
                out.send(wdg::dst_wdg(id));
            }
        }
        // Restore the persisted character when one exists for this save
        // key; the saved world position overrides the fresh-spawn search.
        let saved_state = self.save.players.get(&key).map(|saved| {
            let mut restored_inv = Vec::with_capacity(saved.inv.len());
            for (n, (resname, count, ql)) in saved.inv.iter().enumerate() {
                let idx = self.world.res.intern(leak_static(resname));
                let label = saved
                    .inv_labels
                    .get(n)
                    .map(|s| leak_static(s))
                    .unwrap_or("");
                restored_inv.push(InvStack {
                    res: idx,
                    count: *count,
                    ql: *ql,
                    label,
                });
            }
            let restored_skills: HashSet<&'static str> = saved
                .skills
                .iter()
                .filter_map(|s| crate::skills::catalog_get(s).map(|d| d.name))
                .collect();
            let mut restored_equip: Vec<Option<InvStack>> = vec![None; 16];
            for (slot, resname, count, ql, label) in &saved.equip {
                let idx = self.world.res.intern(leak_static(resname));
                let s = (*slot).min(15);
                restored_equip[s] = Some(InvStack {
                    res: idx,
                    count: *count,
                    ql: *ql,
                    label: leak_static(label),
                });
            }
            (
                saved.pos,
                saved.hp,
                saved.energy,
                saved.stamina,
                saved.lp,
                saved.attrs.clone(),
                restored_inv,
                restored_skills,
                restored_equip,
            )
        });
        let (spawn_pos, hp, energy, stamina, lp, attrs, inv, restored_skills, restored_equip) =
            match &saved_state {
                Some((pos, hp, energy, stamina, lp, attrs, inv, skills, equip)) => {
                    info!(sid, %name, "restoring persisted character");
                    (
                        *pos,
                        *hp,
                        *energy,
                        *stamina,
                        *lp,
                        attrs.clone(),
                        inv.clone(),
                        skills.clone(),
                        equip.clone(),
                    )
                }
                None => {
                    let mut fresh = HashMap::new();
                    // All eight base attributes (CharWnd lists str..psy; the
                    // FEP requirement is the highest of them).
                    for k in ["str", "agi", "int", "con", "per", "cha", "dex", "psy"] {
                        fresh.insert(k.to_owned(), 10);
                    }
                    fresh.insert("hp".to_owned(), 100);
                    fresh.insert("energy".to_owned(), 100);
                    fresh.insert("lp".to_owned(), 0);
                    (
                        self.find_spawn_position(),
                        100,
                        100,
                        100,
                        100,
                        fresh,
                        Vec::new(),
                        HashSet::new(),
                        vec![None; 16],
                    )
                }
            };
        let res_body = self.world.res.intern("gfx/borka/body");
        let _res_head = self.world.res.intern("gfx/borka/head");
        let _res_hair = self.world.res.intern("gfx/borka/hair");
        let gob = self.world.gobs.spawn(
            Kind::Player { player: usize::MAX },
            spawn_pos,
            res_body,
            hp.max(1),
            BASE_SPEED,
        );
        let player_idx = self.world.players.len();
        if let Some(slot) = self.world.gobs.get(gob) {
            self.world.gobs.kind[slot] = Kind::Player { player: player_idx };
        }
        // Restore the criminal flag from the save (None on fresh
        // characters or older saves; the buff re-streams on world entry).
        let criminal_until_ms = self
            .save
            .players
            .get(&key)
            .and_then(|s| s.criminal_until_ms);
        self.world.players.push(Player {
            name: name.clone(),
            account,
            gob,
            session: sid,
            hp,
            energy,
            stamina,
            lp,
            criminal_until_ms,
            lp_carry_ms: 0,
            gait: GAIT_WALK as u8,
            skills: restored_skills,
            attrs,
            inv,
            equip: restored_equip,
            fep: crate::craft::FepState::default(),
            fight_target: None,
            atk_cd: 0,
            aim: None,
            carried_trough: None,
        });
        // A lifted Food Trough (session 62) rides the character across
        // sessions: restore the fodder store before anything else can
        // interact with the fresh player row.
        if let Some(saved) = self.save.players.get(&key) {
            if let Some(t) = saved.carried_trough {
                self.world.players[player_idx].carried_trough = Some(t.into());
            }
        }
        self.world.by_session.insert(sid, player_idx);

        // Session 90: the herd re-binds to its owner on login. Restored
        // tamed rows carry tamer gob 0 plus the owner account key in the
        // tamed_owner sidecar (no player gobs exist at restore time);
        // this character's key claims every row it owns - the beasts
        // follow again (the follow walk walks them over, the OD_FOLLOW
        // on the next spawn block renders the rope), the flower menus
        // open, the milk flows. Rows whose key never matches stay
        // parked (a fresh quell re-binds those the old way).
        let owned: Vec<GobId> = self
            .world
            .tamed_owner
            .iter()
            .filter(|(_, k)| k.as_str() == key)
            .map(|(id, _)| *id)
            .collect();
        let mut rebound = 0usize;
        for id in owned {
            self.world.tamed_owner.remove(&id);
            if let Some(tame) = self.world.tamed.get_mut(&id) {
                tame.tamer = gob;
                rebound += 1;
                // Session 90: the follow owns the position now - cancel
                // any in-flight move, then arm the leash (the OD_FOLLOW
                // replaces the client's LinMove attr, the per-tick
                // follow step walks the server side silently).
                if let Some(slot) = self.world.gobs.get(id) {
                    self.world.gobs.mv[slot] = None;
                }
                self.stream_follow(id, gob);
            }
        }
        if rebound > 0 {
            info!(sid, count = rebound, "herd re-bound to its owner on login");
        }

        // Starter kit for fresh characters (server policy; legacy gave
        // nothing but the dev flow needs craftable ingredients on hand).
        // Labels on food keep the fep.conf identity for the eat flow.
        if self.world.players[player_idx].inv.is_empty() {
            let kit: &[(&str, u32, u8, &'static str)] = &[
                // Session 36: 6 branches + 4 stones + 2 string let a fresh
                // character craft one Wooden Bow (4 branch + 1 string) and
                // one batch of Stone Arrows (1 stone + 2 branch) out of the
                // box, with oven-building headroom (stone x2 + branch x1 of
                // the demand) on top - the whole bow chain is playable
                // immediately. Session 58 tops it up (10/6) so the
                // stone-tool batch (saw/pickaxe/scythe/sprucecap) is also
                // reachable without first harvesting; world gathering
                // (bough/stone picking) remains future work.
                // Session 83: the third string lets a fresh character
                // spin a Rope (string x3) - the taming gate's equipped
                // weapon - so the cow -> milk -> butter dairy chain is
                // playable out of the box too.
                ("gfx/invobjs/branch", 10, 10, ""),
                ("gfx/invobjs/stone", 6, 10, ""),
                ("gfx/invobjs/string", 3, 10, ""),
                ("gfx/invobjs/meat", 1, 10, "Beef"),
                // Farming starter seeds: the plow pagina is pushed to
                // every session, so the full plant-grow-harvest loop is
                // playable out of the box.
                ("gfx/invobjs/seed-wheat", 5, 10, "Wheat Seeds"),
                ("gfx/invobjs/seed-carrot", 5, 10, "Carrot Seeds"),
                // Starter clothing: wearable pieces render on the avatar
                // (equip.rs) and give the Equipment doll something to
                // show right away.
                ("gfx/invobjs/linenpants", 1, 10, "Linen Pants"),
                ("gfx/invobjs/linenshirt", 1, 10, "Linen Shirt"),
            ];
            for (resname, count, ql, label) in kit {
                let gidx = self.world.res.intern(resname);
                self.world.players[player_idx].inv.push(InvStack {
                    res: gidx,
                    count: *count,
                    ql: *ql,
                    label,
                });
            }
        }

        // --- HUD + world bootstrap (order matters; lifecycle doc 3.2) ---
        let player_gob = gob;
        // Snapshot CharWnd attributes before the session out-queue is
        // borrowed: they must reach the client before the `chr` widget is
        // created (SlenHud.binded requests it immediately).
        let attr_entries = self.char_attr_snapshot(sid);
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.player_gob = Some(player_gob);
        // RESID announcements for everything referenced so far.
        let pending: Vec<(u16, &'static str, u16)> = (0..out.res.wire_count() as u16)
            .filter_map(|w| out.res.pending_announce(w).map(|(n, v)| (w, n, v)))
            .collect();
        for (w, n, v) in pending {
            out.send(wdg::resid(w, n, v));
        }
        // Tilesets before first MAPDATA; the version must be the real file
        // version - the client hard-rejects mismatched announces.
        for (id, name, _ver) in hnh_world::TILESETS {
            out.send(wdg::tiles(*id, name, crate::resources::file_version(name)));
        }
        // HUD widgets. The mapview MUST be created before the slen HUD:
        // this fork's SlenHud constructor builds the MinimapPanel, which
        // captures `ui.mapview` at creation time - with the old order
        // (slen first) the minimap held a null MapView and the first
        // real render tick died with an NPE in MiniMap.draw, freezing
        // the client right after entering the world (the render-only
        // path no headless probe ever exercised).
        let w_mv = out.new_wid("mapview");
        let w_slen = out.new_wid("slen");
        let w_scm = out.new_wid("scm");
        let w_speed = out.new_wid("speedget");
        let w_buffs = out.new_wid("buffs");
        out.send(wdg::new_wdg(
            w_mv,
            "mapview",
            0,
            0,
            0,
            &[
                ListVal::I(0),
                ListVal::C(spawn_pos.0, spawn_pos.1),
                ListVal::I(player_gob),
            ],
        ));
        out.send(wdg::new_wdg(w_slen, "slen", 0, 0, 0, &[]));
        out.send(wdg::new_wdg(w_scm, "scm", 0, 0, 0, &[]));
        out.send(wdg::new_wdg(
            w_speed,
            "speedget",
            0,
            0,
            0,
            // cur = walk (docs: RoB Glossary "Speed"), max = sprint index.
            &[ListVal::I(GAIT_WALK as i32), ListVal::I(3)],
        ));
        out.send(wdg::new_wdg(w_buffs, "buffs", 0, 0, 0, &[]));
        // Area Chat window (ChatHW factory): title "Area Chat" hides the
        // client close button; closable = 0 keeps the window permanent.
        let w_chat = out.new_wid("slenchat");
        out.send(wdg::new_wdg(
            w_chat,
            "slenchat",
            0,
            0,
            0,
            &[ListVal::S("Area Chat".to_owned()), ListVal::I(0)],
        ));
        out.chat_wid = w_chat;
        // Vitals meters parented to slen: hp (red), energy (yellow),
        // stamina (green).
        let w_hp = out.new_wid("vm");
        let w_en = out.new_wid("vm");
        let w_st = out.new_wid("vm");
        out.send(wdg::new_wdg(
            w_hp,
            "vm",
            90,
            10,
            w_slen,
            &[
                ListVal::I(100),
                ListVal::I(255),
                ListVal::I(0),
                ListVal::I(0),
            ],
        ));
        out.send(wdg::new_wdg(
            w_en,
            "vm",
            109,
            10,
            w_slen,
            &[
                ListVal::I(100),
                ListVal::I(255),
                ListVal::I(255),
                ListVal::I(0),
            ],
        ));
        out.send(wdg::new_wdg(
            w_st,
            "vm",
            128,
            10,
            w_slen,
            &[
                ListVal::I(100),
                ListVal::I(0),
                ListVal::I(255),
                ListVal::I(0),
            ],
        ));
        // Equipment paperdoll (Equipory, widget type "epry"): the
        // user-reported missing doll. Created during bootstrap with a full
        // "set" sync and the "ava" avatar gob binding.
        let w_epry = out.new_wid("epry");
        out.send(wdg::new_wdg(w_epry, "epry", 0, 0, 0, &[]));
        // Global state.
        let (unix, dt, mp, yt) = self.world.astro();
        out.send(wdg::globlob(unix, dt, mp, yt, Some((255, 255, 255, 255))));
        // Full CharWnd attribute set (client-name mapping) before any
        // chance of the `chr` widget being created.
        out.send(wdg::cattr(&attr_entries));
        // Menu paginae: base actions plus every implemented craft recipe
        // and the build tree (RMSG_PAGINAE; parents resolve from the
        // served resource pack: paginae/act/build -> paginae/build/cons
        // -> paginae/build/<id>; ad strings are the Buildable ids).
        let mut pages: Vec<&'static str> =
            vec!["paginae/act/add", "paginae/add/study", "paginae/act/plow"];
        pages.push("paginae/craft/roastmeat");
        pages.extend([
            "paginae/act/build",
            "paginae/build/cons",
            "paginae/build/oven",
            "paginae/build/smelter",
            "paginae/build/trough",
            // Session 66 follow-up: the alloying crucible page was built
            // but never pushed (the wire probes sent act() directly, so
            // the gap was invisible to them); session 69 adds the kiln
            // page - both menugrid leaves are live from this push on.
            "paginae/build/alloyer",
            "paginae/build/kiln",
            // Session 71: the quern (the baking chain's mill) build
            // page - the MenuGrid leaf next to the kiln's.
            "paginae/build/quern",
        ]);
        for r in crate::craft::RECIPES {
            pages.push(r.pagina);
        }
        // Fight-window maneuver buttons (paginae/atk/*): the root page
        // plus every implemented maneuver of the fight.rs table. Server
        // policy: all buttons are visible from the start (skill gating
        // is a future Open question in combat-system.md).
        pages.push("paginae/atk/atk");
        for m in crate::fight::MANEUVERS {
            pages.push(m.res);
        }
        pages.push("paginae/atk/blk");
        // One PAGINAE frame per entry: the 30-entry announce crosses the
        // reliability-layer fragmentation path, and the real client's
        // in-frame loop desynced mid-frame in the probe environment
        // (pagina read at a shifted offset picked up a garbage version -
        // "Wrong res version (1 != 28484)"). Per-entry frames carry the
        // same entries with no in-frame cursor to desync.
        for p in &pages {
            out.send(wdg::paginae_add(std::slice::from_ref(p)));
        }
        // Initial paperdoll contents ("set" + "ava") now that the player
        // and the epry widget both exist.
        self.send_epry_state(sid);
        // Restore the criminal-state buff icon on reconnects (the Glob
        // is rebuilt client-side on every world entry).
        self.stream_criminal_buff(sid);
        info!(sid, %name, gob, "player entered world");
    }

    pub(super) fn find_spawn_position(&mut self) -> (i32, i32) {
        // Scan outward from (550, 550) for a walkable tile center.
        for r in 0..40i32 {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let tx = 50 + dx;
                    let ty = 50 + dy;
                    let gc = (tx.div_euclid(100), ty.div_euclid(100));
                    let ix = tx.rem_euclid(100) as usize;
                    let iy = ty.rem_euclid(100) as usize;
                    let g = self.world.grids.grid(gc);
                    if tile_speed(g.tile(ix, iy)).is_some() {
                        return (tx * 11 + 5, ty * 11 + 5);
                    }
                }
            }
        }
        (550, 550)
    }
}
