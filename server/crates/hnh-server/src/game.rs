//! The game task: owns the world simulation, streams state to sessions.
//!
//! One task owns the whole `World` (single-writer, SoA layout) and runs a
//! fixed 10 Hz tick. Session tasks feed it commands via an mpsc channel and
//! receive encoded `RMSG` payloads through per-session queues. This split
//! keeps the hot loop allocation-light and the network tasks wait-free.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tracing::{debug, info, trace, warn};

use hnh_proto::consts::*;
use hnh_proto::MessageBuf;

use crate::resources::wdg::{self, ListVal};
use crate::state::*;

// Despawn horizon for dropped items; consumed by the item-despawn pass
// landing together with persistence.
#[allow(dead_code)]
pub const ITEM_DROP_LIFETIME_TICKS: u64 = 20 * 300; // 5 minutes

/// Commands session tasks send into the game task.
pub enum Cmd {
    Wdgmsg {
        sid: SessionId,
        wid: u16,
        name: String,
        args: Vec<hnh_proto::ListArg>,
    },
    MapReq {
        sid: SessionId,
        gc: (i32, i32),
    },
    ObjAck {
        sid: SessionId,
        acks: Vec<(GobId, u32)>,
    },
    SessionClosed {
        sid: SessionId,
    },
    ReportPerf {},
    /// Graceful stop: flush persistence and exit the loop.
    Shutdown {},
}

pub struct Game {
    pub world: World,
    pub sessions: HashMap<SessionId, SessionOut>,
    pub rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    pub net_rx: tokio::sync::mpsc::UnboundedReceiver<crate::net::NetCmd>,
    pub saturated: bool,
    next_sid: SessionId,
    /// Grids already populated with objects/animals.
    populated: HashSet<(i32, i32)>,
    /// Character persistence store (loaded snapshots + live updates).
    pub save: crate::persist::SaveStore,
}

impl Game {
    pub fn new(
        seed: u64,
        rx: tokio::sync::mpsc::UnboundedReceiver<Cmd>,
        net_rx: tokio::sync::mpsc::UnboundedReceiver<crate::net::NetCmd>,
        saturated: bool,
    ) -> Self {
        // Default save location keeps the one-command dev flow; tests pass
        // through this path and simply never touch the store.
        let save_path = std::env::var("HNH_SAVE_FILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("../save/world.json"));
        let save = crate::persist::SaveStore::load(&save_path, seed);
        Game {
            world: World::new(seed),
            sessions: HashMap::new(),
            rx,
            net_rx,
            saturated,
            next_sid: 1,
            populated: HashSet::new(),
            save,
        }
    }

    pub fn alloc_sid(&mut self) -> SessionId {
        let sid = self.next_sid;
        self.next_sid = self.next_sid.wrapping_add(1).max(1);
        sid
    }

    pub async fn run(mut self) {
        let mut tick_timer = tokio::time::interval(Duration::from_millis(TICK_MS));
        tick_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_glob = 0u64;
        let mut autosave = tokio::time::interval(Duration::from_secs(30));
        autosave.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        info!(hz = TICK_HZ, "game loop started");
        loop {
            tokio::select! {
                _ = autosave.tick() => {
                    self.autosave();
                }
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(Cmd::Shutdown {}) => break,
                        Some(c) => self.handle_cmd(c),
                        None => break,
                    }
                }
                ncmd = self.net_rx.recv() => {
                    match ncmd {
                        Some(crate::net::NetCmd::Accept { game_tx, raw_tx, reply }) => {
                            let sid = self.alloc_sid();
                            self.session_connected(sid, game_tx, raw_tx);
                            let _ = reply.send(sid);
                        }
                        Some(other) => {
                            let cmd: Cmd = other.into();
                            self.handle_cmd(cmd);
                        }
                        None => break,
                    }
                }
                _ = tick_timer.tick() => {
                    let t0 = Instant::now();
                    self.tick();
                    let us = t0.elapsed().as_micros();
                    self.world.perf.last_tick_us = us;
                    if us > self.world.perf.max_tick_us {
                        self.world.perf.max_tick_us = us;
                    }
                    last_glob += 1;
                    if last_glob >= TICK_HZ * 5 {
                        last_glob = 0;
                        self.push_globlob();
                    }
                }
            }
        }
        info!("game loop stopping");
        self.save_all_and_flush();
    }

    /// Snapshot every online player, then write the save file. Called on the
    /// 30 s autosave cadence and at shutdown.
    fn autosave(&mut self) {
        self.save_all_and_flush();
    }

    fn save_all_and_flush(&mut self) {
        let seed = self.world.seed;
        for p in &self.world.players {
            if let Some(slot) = self.world.gobs.get(p.gob) {
                let pos = self.world.gobs.pos[slot];
                let inv_named: Vec<(String, u32, u8)> = p
                    .inv
                    .iter()
                    .map(|(idx, count, ql)| {
                        (
                            self.world
                                .res
                                .name(*idx)
                                .unwrap_or("gfx/invobjs/unknown")
                                .to_owned(),
                            *count,
                            *ql,
                        )
                    })
                    .collect();
                self.save.snapshot(p, pos, inv_named);
            }
        }
        if let Err(e) = self.save.flush(seed) {
            tracing::warn!(error = %e, "autosave failed");
        }
    }

    fn handle_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Wdgmsg {
                sid,
                wid,
                name,
                args,
            } => self.on_wdgmsg(sid, wid, &name, args),
            Cmd::MapReq { sid, gc } => self.on_mapreq(sid, gc),
            Cmd::ObjAck { sid, acks } => self.on_objack(sid, acks),
            Cmd::SessionClosed { sid } => self.on_session_closed(sid),
            Cmd::ReportPerf {} => self.report_perf(),
            // Handled in the run loop; reaching handle_cmd means no loop is
            // running (e.g. during tests), so this is a no-op.
            Cmd::Shutdown {} => {}
        }
    }

    fn report_perf(&self) {
        info!(
            players = self.world.players.len(),
            animals = self.world.animal_gobs.len(),
            tick_us = self.world.perf.last_tick_us,
            max_tick_us = self.world.perf.max_tick_us,
            sessions = self.world.perf.active_sessions,
            gobs = self.world.gobs.alive.iter().filter(|a| **a).count(),
            spawned = self.world.perf.spawned_objects,
            "perf"
        );
    }

    // ------------------------------------------------------------------
    // Session lifecycle
    // ------------------------------------------------------------------

    /// Register a newly accepted session; shows the character list.
    /// `tx` is the sink the game task writes outgoing RMSG payloads into;
    /// the session task owns the receiver.
    pub fn session_connected(
        &mut self,
        sid: SessionId,
        tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        raw_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    ) {
        let mut out = SessionOut {
            sid,
            queue: tx,
            raw: raw_tx,
            player_gob: None,
            visible: HashSet::new(),
            unacked: HashMap::new(),
            next_wid: 100,
            widgets: HashMap::new(),
            mapreqs: HashSet::new(),
            res: crate::resources::ResTable::new(),
            fight: crate::fight::FightState::default(),
        };
        // Character selection UI (session-lifecycle.md 3.1).
        let w_bg = out.new_wid("img");
        let w_logo = out.new_wid("img");
        let w_list = out.new_wid("charlist");
        // Avatar layer RESIDs must be announced before the charlist add.
        let body = out
            .res
            .wire_named(self.world.res.intern("gfx/borka/body"), "gfx/borka/body");
        let head = out
            .res
            .wire_named(self.world.res.intern("gfx/borka/head"), "gfx/borka/head");
        let hair = out
            .res
            .wire_named(self.world.res.intern("gfx/borka/hair"), "gfx/borka/hair");
        for w in [body, head, hair] {
            if let Some((n, v)) = out.res.pending_announce(w) {
                out.send(wdg::resid(w, n, v));
                out.res.mark_announced(w);
            }
        }
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
            .lstr("Player")
            .lint(body as i32)
            .lint(head as i32)
            .lint(hair as i32)
            .lend();
        out.send(add.finish());
        self.sessions.insert(sid, out);
    }

    fn on_wdgmsg(&mut self, sid: SessionId, wid: u16, name: &str, args: Vec<hnh_proto::ListArg>) {
        let Some(out) = self.sessions.get(&sid) else {
            return;
        };
        let wtag = out.widgets.get(&wid).cloned();
        match (wtag.as_deref(), name) {
            (Some("charlist"), "play") => {
                let chosen = args
                    .first()
                    .and_then(|a| a.as_str())
                    .unwrap_or("Player")
                    .to_owned();
                self.enter_world(sid, chosen);
            }
            (Some("slen"), "inv") => {
                self.open_inventory(sid);
            }
            (Some("slen"), "equ") => {
                // Equipment window: static empty container for now.
                if let Some(out) = self.sessions.get_mut(&sid) {
                    let w = out.new_wid("inv");
                    out.send(wdg::new_wdg(w, "inv", 400, 200, 0, &[]));
                }
            }
            (Some("slen"), _) | (None, "bud") | (None, "chr") => {}
            (Some("inv"), "drop") => self.inv_drop(sid, wid, &args),
            (Some("mapview"), "click") => self.on_map_click(sid, &args),
            (Some("mapview"), "place") => {
                debug!(sid, "placement confirmed (stub)");
            }
            (Some("scm"), "act") => {
                let action: Vec<String> = args
                    .iter()
                    .filter_map(|a| a.as_str().map(str::to_owned))
                    .collect();
                self.on_menu_action(sid, &action);
            }
            (Some("frv"), "click") | (Some("frv"), "give") => {
                self.on_frv_msg(sid, name, &args);
            }
            (Some("sm"), "cl") => {
                let choice = args.first().and_then(|a| a.as_int()).unwrap_or(-1);
                debug!(sid, choice, "flower menu choice");
            }
            _ => {
                trace!(sid, wid, name, "unhandled wdgmsg");
            }
        }
    }

    /// Phase 3.2 (session-lifecycle.md): enter the world after `play`.
    fn enter_world(&mut self, sid: SessionId, name: String) {
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
        // Restore the persisted character when one exists for this name;
        // the saved world position overrides the fresh-spawn search.
        let saved_state = self.save.players.get(&name).map(|saved| {
            let mut restored_inv = Vec::with_capacity(saved.inv.len());
            for (resname, count, ql) in &saved.inv {
                let idx = self.world.res.intern(leak_static(resname));
                restored_inv.push((idx, *count, *ql));
            }
            (
                saved.pos,
                saved.hp,
                saved.energy,
                saved.stamina,
                saved.lp,
                saved.attrs.clone(),
                restored_inv,
            )
        });
        let (spawn_pos, hp, energy, stamina, lp, attrs, inv) = match &saved_state {
            Some((pos, hp, energy, stamina, lp, attrs, inv)) => {
                info!(sid, %name, "restoring persisted character");
                (
                    *pos,
                    *hp,
                    *energy,
                    *stamina,
                    *lp,
                    attrs.clone(),
                    inv.clone(),
                )
            }
            None => {
                let mut fresh = HashMap::new();
                fresh.insert("str".to_owned(), 10);
                fresh.insert("agi".to_owned(), 10);
                fresh.insert("int".to_owned(), 10);
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
        self.world.players.push(Player {
            name: name.clone(),
            gob,
            session: sid,
            hp,
            energy,
            stamina,
            lp,
            attrs,
            inv,
            fight_target: None,
            atk_cd: 0,
        });
        self.world.by_session.insert(sid, player_idx);

        // --- HUD + world bootstrap (order matters; lifecycle doc 3.2) ---
        let player_gob = gob;
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
        // Tilesets before first MAPDATA.
        for (id, name, ver) in hnh_world::TILESETS {
            out.send(wdg::tiles(*id, name, *ver));
        }
        // HUD widgets.
        let w_slen = out.new_wid("slen");
        let w_scm = out.new_wid("scm");
        let w_speed = out.new_wid("speedget");
        let w_buffs = out.new_wid("buffs");
        let w_mv = out.new_wid("mapview");
        out.send(wdg::new_wdg(w_slen, "slen", 0, 0, 0, &[]));
        out.send(wdg::new_wdg(w_scm, "scm", 0, 0, 0, &[]));
        out.send(wdg::new_wdg(
            w_speed,
            "speedget",
            0,
            0,
            0,
            &[ListVal::I(2), ListVal::I(4)],
        ));
        out.send(wdg::new_wdg(w_buffs, "buffs", 0, 0, 0, &[]));
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
        // Global state.
        let (unix, dt, mp, yt) = self.world.astro();
        out.send(wdg::globlob(unix, dt, mp, yt, Some((255, 255, 255, 255))));
        out.send(wdg::cattr(&[
            ("pts", 100, 100),
            ("hp", 100, 100),
            ("str", 10, 10),
            ("agi", 10, 10),
            ("int", 10, 10),
        ]));
        out.send(wdg::paginae_add(&["paginae/act/add", "paginae/add/study"]));
        info!(sid, %name, gob, "player entered world");
    }

    fn find_spawn_position(&mut self) -> (i32, i32) {
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

    // ------------------------------------------------------------------
    // Map
    // ------------------------------------------------------------------

    fn on_mapreq(&mut self, sid: SessionId, gc: (i32, i32)) {
        // Populate the grid the first time anyone looks at it.
        let mut spawned = Vec::new();
        let first_touch = !self.populated.contains(&gc);
        if first_touch {
            self.populated.insert(gc);
            self.world.populate_grid(gc, &mut spawned);
            let animals = if self.saturated { 40 } else { 4 };
            self.world.populate_animals(gc, animals, &mut spawned);
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
    fn encode_gob_block(
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
        let res_name = self
            .world
            .res
            .name(res_idx)
            .unwrap_or("gfx/terobjs/bumlings/stone1");
        let out = self.sessions.get_mut(&sid)?;
        let wire_res = out.res.wire_named(res_idx, res_name);
        let mut m = MessageBuf::new();
        m.uint8(MSG_OBJDATA);
        m.uint8(0); // flags
        m.int32(id);
        m.int32(frame as i32);
        if include_res {
            // OD_RES with the resource; sprite dynamic data for plants.
            m.uint8(OD_RES).uint16(wire_res | 0x8000);
            let sdt = match kind {
                Kind::Tree { harvests } => vec![harvests],
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
        // Player avatar layers.
        if let Kind::Player { player } = kind {
            if let Some(p) = self.world.players.get(player) {
                let head = out
                    .res
                    .wire_named(self.world.res.intern("gfx/borka/head"), "gfx/borka/head");
                let hair = out
                    .res
                    .wire_named(self.world.res.intern("gfx/borka/hair"), "gfx/borka/hair");
                m.uint8(OD_LAYERS).uint16(wire_res); // base = body
                m.uint16(head);
                m.uint16(hair);
                m.uint16(65535);
                m.uint8(OD_BUDDY).string(&p.name).uint8(0).uint8(0);
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
    fn stream_spawn(&mut self, sid: SessionId, id: GobId) {
        let Some(slot) = self.world.gobs.get(id) else {
            return;
        };
        let res_idx = self.world.gobs.res_idx[slot];
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
            .unwrap_or("gfx/terobjs/bumlings/stone1");
        let wire = out.res.wire_named(res_idx, res_name);
        if let Some((name, ver)) = out.res.pending_announce(wire) {
            let msg = wdg::resid(wire, name, ver);
            out.send(msg);
            out.res.mark_announced(wire);
        }
        // Encode and register the spawn block (separate borrow scope).
        if let Some(block) = self.encode_gob_block(sid, id, true) {
            let frame = self.world.gobs.frame[slot];
            if let Some(out) = self.sessions.get_mut(&sid) {
                out.send_raw(block.clone());
                out.unacked.entry(id).or_default().insert(frame, block);
            }
        }
    }

    fn stream_retract(&mut self, sid: SessionId, id: GobId) {
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
            let mut m = MessageBuf::new();
            m.uint8(MSG_OBJDATA)
                .uint8(0)
                .int32(id)
                .int32(frame as i32)
                .uint8(OD_REM)
                .uint8(OD_END);
            out.send_raw(m.finish());
        }
        out.unacked.remove(&id);
    }

    /// Per-tick visibility update: spawns, retractions, movement deltas.
    fn update_visibility(&mut self) {
        // Gather positions first (avoid double borrow).
        let mut updates: Vec<(SessionId, Vec<GobId>)> = Vec::new();
        let session_ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for sid in session_ids {
            let Some(player_gob) = self.sessions[&sid].player_gob else {
                continue;
            };
            let Some(pslot) = self.world.gobs.get(player_gob) else {
                continue;
            };
            let (px, py) = self.world.gobs.pos[pslot];
            // Everything within VIEW_RADIUS of the player.
            let mut moving: Vec<GobId> = Vec::new();
            for slot in 0..self.world.gobs.alive.len() {
                if !self.world.gobs.alive[slot] {
                    continue;
                }
                let (gx, gy) = self.world.gobs.pos[slot];
                if (gx - px).abs() > VIEW_RADIUS || (gy - py).abs() > VIEW_RADIUS {
                    continue;
                }
                let id = gob_id_from_slot(slot, self.world.gobs.gen[slot]);
                let mv = self.world.gobs.mv[slot];
                let frame = self.world.gobs.frame[slot];
                let is_new;
                let needs_move;
                {
                    let out = self.sessions.get_mut(&sid).expect("BUG: sid from keys");
                    is_new = out.visible.insert(id);
                    needs_move = !is_new
                        && mv.is_some()
                        && out
                            .unacked
                            .get(&id)
                            .map(|m| !m.contains_key(&frame))
                            .unwrap_or(true);
                }
                if is_new {
                    self.stream_spawn(sid, id);
                } else if needs_move {
                    moving.push(id);
                }
            }
            if !moving.is_empty() {
                updates.push((sid, moving));
            }
            // Retractions: visible set minus in-range is too expensive to
            // scan fully every tick; do a cheap sweep only over the visible
            // set (bounded by ~few hundred gobs per session).
            let to_retract: Vec<GobId> = {
                let out = self.sessions.get_mut(&sid).expect("BUG: sid from keys");
                out.visible
                    .iter()
                    .filter(|&&id| {
                        self.world
                            .gobs
                            .get(id)
                            .map(|slot| {
                                let (gx, gy) = self.world.gobs.pos[slot];
                                (gx - px).abs() > VIEW_RADIUS * 2
                                    || (gy - py).abs() > VIEW_RADIUS * 2
                            })
                            .unwrap_or(true) // dead gobs get retracted too
                    })
                    .copied()
                    .collect()
            };
            for id in to_retract {
                self.stream_retract(sid, id);
            }
            self.world.perf.visible_total += self.sessions[&sid].visible.len();
        }
        // Movement deltas: LINSTEP progress frames.
        for (sid, gobs) in updates {
            for id in gobs {
                let Some(slot) = self.world.gobs.get(id) else {
                    continue;
                };
                let Some(lm) = self.world.gobs.mv[slot] else {
                    continue;
                };
                let frame = self.world.gobs.frame[slot];
                let Some(out) = self.sessions.get_mut(&sid) else {
                    continue;
                };
                let mut m = MessageBuf::new();
                m.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(id)
                    .int32(frame as i32)
                    .uint8(OD_LINSTEP)
                    .int32(lm.step)
                    .uint8(OD_END);
                let block = m.finish();
                out.send_raw(block.clone());
                out.unacked.entry(id).or_default().insert(frame, block);
            }
        }
    }

    fn on_objack(&mut self, sid: SessionId, acks: Vec<(GobId, u32)>) {
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        for (id, frame) in acks {
            if let Some(per_gob) = out.unacked.get_mut(&id) {
                per_gob.retain(|f, _| *f > frame);
                if per_gob.is_empty() {
                    out.unacked.remove(&id);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Player commands
    // ------------------------------------------------------------------

    fn on_map_click(&mut self, sid: SessionId, args: &[hnh_proto::ListArg]) {
        // click(c0, mc, button, modflags[, gobid, gobrc])
        let mc = args.iter().find_map(|a| a.as_coord());
        let button = args.iter().filter_map(|a| a.as_int()).nth(1).unwrap_or(0);
        let gobid = args.get(4).and_then(|a| a.as_int());
        let Some((_x, y)) = mc else { return };
        let (mx, my) = mc.expect("BUG: mc checked above");
        let Some(player_gob) = self.sessions.get(&sid).and_then(|o| o.player_gob) else {
            return;
        };
        if button == 1 {
            if let Some(target) = gobid {
                self.player_interact(sid, player_gob, target, (mx, my));
            } else {
                self.player_walk(sid, player_gob, (mx, my));
            }
        }
        let _ = y;
    }

    fn player_walk(&mut self, sid: SessionId, player_gob: GobId, target: (i32, i32)) {
        let Some(slot) = self.world.gobs.get(player_gob) else {
            return;
        };
        let (sx, sy) = self.world.gobs.pos[slot];
        // Clamp path length and validate walkability server-side.
        let dx = (target.0 - sx).clamp(-5000, 5000);
        let dy = (target.1 - sy).clamp(-5000, 5000);
        let (tx, ty) = (sx + dx, sy + dy);
        if !path_clear(&mut self.world, sx, sy, tx, ty) {
            return;
        }
        let dist = (tx - sx).abs() + (ty - sy).abs();
        let speed = self.world.gobs.speed[slot].max(1);
        let ms = (dist * 1000) / speed; // milliseconds at speed subtile/s
        let steps = (ms / (TICK_MS as i32)).clamp(1, 1000);
        self.world.gobs.mv[slot] = Some(LinMove {
            sx,
            sy,
            tx,
            ty,
            steps,
            step: 0,
        });
        self.world.gobs.frame[slot] += 1;
        self.world.gobs.pos[slot] = (tx, ty); // logical position = destination
        let frame = self.world.gobs.frame[slot];
        // Send LINBEG to all sessions that see this gob.
        let viewers: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&player_gob))
            .map(|(s, _)| *s)
            .collect();
        for v in viewers {
            if let Some(out) = self.sessions.get_mut(&v) {
                let mut m = MessageBuf::new();
                m.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(player_gob)
                    .int32(frame as i32)
                    .uint8(OD_LINBEG)
                    .coord(sx, sy)
                    .coord(tx, ty)
                    .int32(steps)
                    .uint8(OD_END);
                let block = m.finish();
                out.send_raw(block.clone());
                out.unacked
                    .entry(player_gob)
                    .or_default()
                    .insert(frame, block);
            }
        }
        trace!(sid, sx, sy, tx, ty, steps, "walk");
    }

    fn player_interact(
        &mut self,
        sid: SessionId,
        _player_gob: GobId,
        target: GobId,
        _at: (i32, i32),
    ) {
        let Some(tslot) = self.world.gobs.get(target) else {
            return;
        };
        match self.world.gobs.kind[tslot] {
            Kind::Tree { harvests } => {
                if harvests > 0 {
                    self.world.gobs.kind[tslot] = Kind::Tree {
                        harvests: harvests - 1,
                    };
                    self.world.gobs.frame[tslot] += 1;
                    let pos = self.world.gobs.pos[tslot];
                    self.spawn_drop_near(pos, "gfx/invobjs/log", 1, 10);
                    if let Some(p) = self.world.player_mut(sid) {
                        p.lp += 5;
                    }
                    self.push_cattr(sid);
                } else {
                    // Tree exhausted: remove and leave a stump.
                    let pos = self.world.gobs.pos[tslot];
                    self.world.gobs.kill(target);
                    let stump = self.world.res.intern("gfx/terobjs/trees/log");
                    let id = self.world.gobs.spawn(Kind::Stone, pos, stump, 1, 0);
                    self.broadcast_spawn(id);
                }
            }
            Kind::Stone => {
                let pos = self.world.gobs.pos[tslot];
                self.world.gobs.kill(target);
                self.spawn_drop_near(pos, "gfx/invobjs/stone", 1, 10);
                if let Some(p) = self.world.player_mut(sid) {
                    p.lp += 3;
                }
                self.push_cattr(sid);
            }
            Kind::Drop { .. } => {
                // Pick up: move into inventory.
                let res_idx = self.world.gobs.res_idx[tslot];
                if let Some(drop) = self.world.gobs.kind[tslot].drop_info() {
                    if let Some(p) = self.world.player_mut(sid) {
                        p.inv.push((res_idx, drop.1 as u32, drop.2));
                    }
                }
                self.world.gobs.kill(target);
                self.broadcast_retract(target);
                self.refresh_inventory(sid);
            }
            Kind::Animal { species } => {
                self.start_fight(sid, target, species);
            }
            Kind::Player { .. } => {
                warn!(sid, "pvp interactions are not enabled yet");
            }
        }
    }

    fn start_fight(&mut self, sid: SessionId, target: GobId, species: Species) {
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
            });
        info!(sid, target, ?species, "fight started");
    }

    // ------------------------------------------------------------------
    // Fightview (frv) widget protocol
    // ------------------------------------------------------------------

    /// Send one frv uimsg to the session (no-op without a fight widget).
    fn fight_uimsg(&mut self, sid: SessionId, name: &str, args: &[i32]) {
        if let Some(out) = self.sessions.get_mut(&sid) {
            if let Some(w) = out.fight.widget {
                let b = crate::fight::uimsg(w, name, args);
                out.send(b);
            }
        }
    }

    /// Open (or reuse) the fight window and add a relation for `target`.
    fn fight_open(&mut self, sid: SessionId, target: GobId) {
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
    fn fight_del(&mut self, sid: SessionId, gob: GobId) {
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
        }
    }

    /// Handle client->server frv wdgmsg (click / give).
    fn on_frv_msg(&mut self, sid: SessionId, name: &str, args: &[hnh_proto::ListArg]) {
        let ints: Vec<i32> = args.iter().filter_map(|a| a.as_int()).collect();
        match name {
            "click" => {
                // Select that opponent; answer with `cur`.
                if let Some(&gob) = ints.first() {
                    if let Some(p) = self.world.player_mut(sid) {
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

    fn spawn_drop_near(&mut self, at: (i32, i32), resname: &'static str, _count: u8, _ql: u8) {
        let res_idx = self.world.res.intern(resname);
        let jitter = |w: &mut World| (w.next_ai_rand(7) - 3) * 11;
        let jx = jitter(&mut self.world);
        let jy = jitter(&mut self.world);
        let id = self.world.gobs.spawn(
            Kind::Drop {
                resname_idx: res_idx,
                ql: 10,
            },
            (at.0 + jx, at.1 + jy),
            res_idx,
            1,
            0,
        );
        self.broadcast_spawn(id);
    }

    fn broadcast_spawn(&mut self, id: GobId) {
        let sids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for sid in sids {
            if self.sessions[&sid].visible.contains(&id) || self.session_in_range(sid, id) {
                self.stream_spawn(sid, id);
            }
        }
    }

    fn broadcast_retract(&mut self, id: GobId) {
        let sids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for sid in sids {
            self.stream_retract(sid, id);
        }
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

    fn open_inventory(&mut self, sid: SessionId) {
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
            out.send(wdg::new_wdg(w, "inv", 350, 250, 0, &[]));
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

    /// Rebuild inventory items: destroy old item widgets, create new ones.
    fn refresh_inventory(&mut self, sid: SessionId) {
        let Some(inv_wid) = self.inv_window(sid) else {
            return;
        };
        let items: Vec<(u16, u32, u8)> = self
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
            .collect();
        for id in old {
            out.send(wdg::dst_wdg(id));
        }
        for (n, (res_idx, count, ql)) in items.iter().enumerate() {
            let res_name = self.world.res.name(*res_idx).unwrap_or("gfx/invobjs/stone");
            let wire = out.res.wire_named(*res_idx, res_name);
            if let Some((name, ver)) = out.res.pending_announce(wire) {
                out.send(wdg::resid(wire, name, ver));
                out.res.mark_announced(wire);
            }
            let w = out.new_wid("item");
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
                    ListVal::I(*ql as i32),
                    ListVal::I(0),
                    ListVal::S(String::new()),
                    ListVal::I(*count as i32),
                ],
            ));
        }
    }

    fn inv_drop(&mut self, sid: SessionId, _wid: u16, _args: &[hnh_proto::ListArg]) {
        // Drop the last stack onto the ground at the player's feet.
        let (gob, stack) = match self.world.player_mut(sid) {
            Some(p) => match p.inv.pop() {
                Some(s) => (p.gob, s),
                None => return,
            },
            None => return,
        };
        let Some(slot) = self.world.gobs.get(gob) else {
            return;
        };
        let pos = self.world.gobs.pos[slot];
        let name = self.world.res.name(stack.0).unwrap_or("gfx/invobjs/stone");
        self.spawn_drop_near(pos, leak_static(name), stack.1 as u8, stack.2);
        self.refresh_inventory(sid);
    }

    fn on_menu_action(&mut self, sid: SessionId, action: &[String]) {
        debug!(sid, ?action, "menu action");
    }

    // ------------------------------------------------------------------
    // Simulation tick
    // ------------------------------------------------------------------

    fn tick(&mut self) {
        self.world.tick += 1;
        self.tick_movement();
        self.tick_animals();
        self.tick_combat();
        self.tick_vitals();
        self.update_visibility();
        self.world.perf.active_sessions = self.sessions.len();
    }

    fn tick_movement(&mut self) {
        for slot in 0..self.world.gobs.alive.len() {
            if !self.world.gobs.alive[slot] {
                continue;
            }
            if let Some(lm) = self.world.gobs.mv[slot] {
                if lm.step < lm.steps {
                    let new_step = (lm.step + 1).min(lm.steps);
                    self.world.gobs.mv[slot] = Some(LinMove {
                        step: new_step,
                        ..lm
                    });
                    if new_step >= lm.steps {
                        // Move finished: position already set at destination;
                        // clear movement and bump the frame so clients that
                        // acked the move get a final static position.
                        self.world.gobs.mv[slot] = None;
                        self.world.gobs.frame[slot] += 1;
                        self.world.gobs.pos[slot] = (lm.tx, lm.ty);
                    }
                }
            }
        }
    }

    fn tick_animals(&mut self) {
        let tick = self.world.tick;
        let mut decisions: Vec<(GobId, AnimalAction)> = Vec::new();
        let animal_ids: Vec<GobId> = self.world.animal_gobs.clone();
        for &id in &animal_ids {
            let Some(slot) = self.world.gobs.get(id) else {
                continue;
            };
            let Kind::Animal { species } = self.world.gobs.kind[slot] else {
                continue;
            };
            if self.world.gobs.mv[slot].is_some() {
                continue;
            }
            // Find nearest player within perception. Saturated worlds widen
            // the aggro radius so predators converge on the bot cohorts.
            let perception = if self.saturated { 1500 } else { 400 };
            let aggro = if self.saturated { 900 } else { 300 };
            let (ax, ay) = self.world.gobs.pos[slot];
            let mut nearest: Option<(GobId, i32)> = None;
            for p in &self.world.players {
                if let Some(pslot) = self.world.gobs.get(p.gob) {
                    let (px, py) = self.world.gobs.pos[pslot];
                    let d = ((px - ax).abs() + (py - ay).abs()).min(i32::MAX - 1);
                    if d < perception && nearest.map(|(_, nd)| d < nd).unwrap_or(true) {
                        nearest = Some((p.gob, d));
                    }
                }
            }
            let action = match nearest {
                Some((pgob, dist)) if species.aggressive() && dist < aggro => {
                    if self.saturated && tick.is_multiple_of(200) {
                        info!(?species, id, ?pgob, dist, "predator chasing");
                    }
                    AnimalAction::Chase(pgob)
                }
                Some((_pgob, dist)) if !species.aggressive() && dist < 200 => AnimalAction::Flee,
                _ if tick % 20 == (slot as u64) % 20 => {
                    let r = self.world.next_ai_rand(4);
                    if r == 0 {
                        AnimalAction::Wander
                    } else {
                        AnimalAction::Idle
                    }
                }
                _ => AnimalAction::Idle,
            };
            decisions.push((id, action));
        }
        for (id, action) in decisions {
            self.apply_animal_action(id, action);
        }
    }

    fn apply_animal_action(&mut self, id: GobId, action: AnimalAction) {
        let Some(slot) = self.world.gobs.get(id) else {
            return;
        };
        let (sx, sy) = self.world.gobs.pos[slot];
        let speed = self.world.gobs.speed[slot].max(1);
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
            AnimalAction::Flee => {
                // Run away from the nearest player (approximately: random
                // opposite direction).
                let jx = (self.world.next_ai_rand(21) - 10) * 55;
                let jy = (self.world.next_ai_rand(21) - 10) * 55;
                (sx + jx, sy + jy)
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
        if !path_clear(&mut self.world, sx, sy, tx, ty) {
            return;
        }
        let dist = (tx - sx).abs() + (ty - sy).abs();
        let ms = (dist * 1000) / speed;
        let steps = (ms / (TICK_MS as i32)).clamp(1, 600);
        self.world.gobs.mv[slot] = Some(LinMove {
            sx,
            sy,
            tx,
            ty,
            steps,
            step: 0,
        });
        self.world.gobs.frame[slot] += 1;
        self.world.gobs.pos[slot] = (tx, ty);
        let frame = self.world.gobs.frame[slot];
        let viewers: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, o)| o.visible.contains(&id))
            .map(|(s, _)| *s)
            .collect();
        for v in viewers {
            if let Some(out) = self.sessions.get_mut(&v) {
                let mut m = MessageBuf::new();
                m.uint8(MSG_OBJDATA)
                    .uint8(0)
                    .int32(id)
                    .int32(frame as i32)
                    .uint8(OD_LINBEG)
                    .coord(sx, sy)
                    .coord(tx, ty)
                    .int32(steps)
                    .uint8(OD_END);
                let block = m.finish();
                out.send_raw(block.clone());
                out.unacked.entry(id).or_default().insert(frame, block);
            }
        }
    }

    fn tick_combat(&mut self) {
        const REACH: i32 = 33; // ~3 tiles
        const DISENGAGE: i32 = 300;
        let tick = self.world.tick;

        // --- player side: offence gen, swings, bar streaming ---
        let players: Vec<usize> = (0..self.world.players.len()).collect();
        for pidx in players {
            let (target, sid, pgob) = {
                let p = &self.world.players[pidx];
                (p.fight_target, p.session, p.gob)
            };
            let Some(target) = target else { continue };
            let Some(pslot) = self.world.gobs.get(pgob) else {
                continue;
            };
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
                    let dist = (tx - px).abs() + (ty - py).abs();
                    let speed = self.world.gobs.speed[pslot].max(1);
                    let ms = (dist * 1000) / speed;
                    let steps = (ms / (TICK_MS as i32)).clamp(1, 600);
                    let (sx, sy) = (px, py);
                    self.world.gobs.mv[pslot] = Some(LinMove {
                        sx,
                        sy,
                        tx,
                        ty,
                        steps,
                        step: 0,
                    });
                    self.world.gobs.frame[pslot] += 1;
                    self.world.gobs.pos[pslot] = (tx, ty);
                    let frame = self.world.gobs.frame[pslot];
                    let viewers: Vec<SessionId> = self
                        .sessions
                        .iter()
                        .filter(|(_, o)| o.visible.contains(&pgob))
                        .map(|(s, _)| *s)
                        .collect();
                    for v in viewers {
                        if let Some(out) = self.sessions.get_mut(&v) {
                            let mut m = MessageBuf::new();
                            m.uint8(MSG_OBJDATA)
                                .uint8(0)
                                .int32(pgob)
                                .int32(frame as i32)
                                .uint8(OD_LINBEG)
                                .coord(sx, sy)
                                .coord(tx, ty)
                                .int32(steps)
                                .uint8(OD_END);
                            let b = m.finish();
                            out.send_raw(b.clone());
                            out.unacked.entry(pgob).or_default().insert(frame, b);
                        }
                    }
                }
                continue;
            }
            // Bar updates and swing decision inside a tight scope, so the
            // session borrow is dropped before any self-facing call.
            let mut swing = None;
            {
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
                // Swing: spend offence, chip defence, land damage on an opening.
                out.fight.own_off -= crate::fight::SWING_SPEND;
                out.fight.atkc = crate::fight::ATKC_TICKS;
                let Some(rel) = out.fight.rel_mut(target) else {
                    continue;
                };
                rel.ip_self += 1;
                // Attack weight scales 0.5..2.0 with advantage (balance).
                let weight = (rel.balance.clamp(-5, 5) as f32) * 0.1 + 1.0;
                let def_chip = (crate::fight::SWING_DEF_DMG as f32 * weight) as i32;
                let breaking = rel.defence <= crate::fight::OPENING_THRESHOLD;
                rel.defence = (rel.defence - def_chip).max(0);
                let landed = breaking || rel.defence <= crate::fight::OPENING_THRESHOLD;
                if landed {
                    rel.defence = crate::fight::BAR_FULL;
                    let str = *self.world.players[pidx].attrs.get("str").unwrap_or(&10);
                    swing = Some((5 * str / 10).max(1));
                }
            }
            self.world.players[pidx].stamina = (self.world.players[pidx].stamina - 2).max(0);
            if let Some(dmg) = swing {
                self.damage_animal(pidx, sid, target, tslot, dmg);
            }
        }

        // --- animal side: aggressive animals swing back ---
        let animals: Vec<GobId> = self.world.animal_gobs.clone();
        for id in animals {
            let Some(slot) = self.world.gobs.get(id) else {
                continue;
            };
            let Kind::Animal { species } = self.world.gobs.kind[slot] else {
                continue;
            };
            let _ = species;
            // Find the engaged player and re-check reach.
            let Some(engaged) = self
                .world
                .players
                .iter()
                .enumerate()
                .find(|(_, q)| q.fight_target == Some(id))
                .map(|(i, q)| (i, q.session, q.gob))
            else {
                continue;
            };
            let (pidx, p_sid, p_gob) = engaged;
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
            // Animal offence builds; swing chips the player's defence.
            let mut bite = None;
            {
                let Some(af) = self.world.animal_fights.get_mut(&id) else {
                    continue;
                };
                if animal_off >= crate::fight::SWING_SPEND {
                    af.off -= crate::fight::SWING_SPEND;
                    let str = *self.world.players[pidx].attrs.get("str").unwrap_or(&10);
                    // Animal bites are lighter than player swings.
                    let dmg = (5 * str / 10).max(1) / 2;
                    let new_def = (own_def - crate::fight::SWING_DEF_DMG).max(0);
                    if new_def <= crate::fight::OPENING_THRESHOLD {
                        bite = Some(dmg);
                    }
                }
            }
            if let Some(dmg) = bite {
                self.hurt_player(pidx, dmg, id);
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
                if bite.is_some() {
                    out.fight.own_def = crate::fight::BAR_FULL;
                }
            }
        }

        // --- fast bar streaming: updod per relation + offdef, every 2 ticks ---
        if tick.is_multiple_of(2) {
            let sids: Vec<SessionId> = self.sessions.keys().copied().collect();
            for sid in sids {
                let Some(out) = self.sessions.get_mut(&sid) else {
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
        }
    }

    /// Apply player damage to an animal, handling death + loot.
    fn damage_animal(
        &mut self,
        pidx: usize,
        sid: SessionId,
        target: GobId,
        tslot: usize,
        dmg: i32,
    ) {
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
                out.unacked.entry(target).or_default().insert(frame, b);
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
            for (res, count) in species.loot() {
                for _ in 0..count {
                    self.spawn_drop_near(pos, res, 1, 10);
                }
            }
            self.world.players[pidx].lp += 10;
            self.push_cattr(sid);
            self.world.players[pidx].fight_target = None;
            self.fight_del(sid, target);
            info!(target, ?species, "animal killed");
        }
    }

    /// Apply animal damage to a player (health quarters stream too).
    fn hurt_player(&mut self, pidx: usize, dmg: i32, from: GobId) {
        let sid = self.world.players[pidx].session;
        let pgob = self.world.players[pidx].gob;
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
            info!(sid, from, "player knocked out by animal");
            return;
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
    }

    fn tick_vitals(&mut self) {
        let tick = self.world.tick;
        for pidx in 0..self.world.players.len() {
            let p = &mut self.world.players[pidx];
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
                    self.world.gobs.pos[slot] = (550, 550);
                    self.world.gobs.hp[slot] = 50;
                    self.world.gobs.frame[slot] += 1;
                }
                info!(player = %p.name, "player down, respawned");
            }
        }
    }

    /// Push updated vitals meters + CATTR to one session.
    fn push_cattr(&mut self, sid: SessionId) {
        let Some(p) = self.world.player(sid) else {
            return;
        };
        let (hp, en, st, lp) = (p.hp, p.energy, p.stamina, p.lp);
        let Some(out) = self.sessions.get_mut(&sid) else {
            return;
        };
        out.send(wdg::cattr(&[
            ("pts", lp, lp),
            ("hp", 100, hp),
            ("energy", 100, en),
            ("stamina", 100, st),
        ]));
        // Meter widgets by tag.
        let meters: Vec<(u16, i32)> = out
            .widgets
            .iter()
            .filter(|(_, t)| t.as_str() == "vm")
            .map(|(id, _)| *id)
            .enumerate()
            .map(|(i, id)| (id, [hp, en, st][i.min(2)]))
            .collect();
        for (id, amount) in meters {
            out.send(wdg::wdgmsg(id, "set", &[ListVal::I(amount)]));
        }
    }

    fn push_globlob(&mut self) {
        let (unix, dt, mp, yt) = self.world.astro();
        for out in self.sessions.values() {
            if out.player_gob.is_some() {
                out.send(wdg::globlob(unix, dt, mp, yt, None));
            }
        }
    }

    /// Snapshot a player into the save store (position from the gob slot,
    /// inventory translated from process-local indices to resource names).
    fn persist_player(&mut self, gob: crate::state::GobId) {
        let Some(slot) = self.world.gobs.get(gob) else {
            return;
        };
        let pos = self.world.gobs.pos[slot];
        let Some(pidx) = self.world.players.iter().position(|p| p.gob == gob) else {
            return;
        };
        let p = &self.world.players[pidx];
        let inv_named: Vec<(String, u32, u8)> = p
            .inv
            .iter()
            .map(|(idx, count, ql)| {
                (
                    self.world
                        .res
                        .name(*idx)
                        .unwrap_or("gfx/invobjs/unknown")
                        .to_owned(),
                    *count,
                    *ql,
                )
            })
            .collect();
        self.save.snapshot(p, pos, inv_named);
    }

    fn on_session_closed(&mut self, sid: SessionId) {
        if let Some(out) = self.sessions.remove(&sid) {
            if let Some(gob) = out.player_gob {
                self.persist_player(gob);
                self.broadcast_retract(gob);
                self.world.gobs.kill(gob);
            }
        }
        if let Some(idx) = self.world.by_session.remove(&sid) {
            self.world.players.remove(idx);
            // Reindex by_session after removal.
            for v in self.world.by_session.values_mut() {
                if *v > idx {
                    *v -= 1;
                }
            }
            // Fix Kind::Player back-references.
            for slot in 0..self.world.gobs.alive.len() {
                if let Kind::Player { player } = self.world.gobs.kind[slot] {
                    if player == usize::MAX {
                        continue;
                    }
                    self.world.gobs.kind[slot] = Kind::Player {
                        player: player.min(self.world.players.len().saturating_sub(1)),
                    };
                }
            }
        }
        info!(sid, "session closed");
    }
}

enum AnimalAction {
    Chase(GobId),
    Flee,
    Wander,
    Idle,
}

/// ResTable stores &'static str; runtime names from items need leaking.
fn leak_static(name: &str) -> &'static str {
    Box::leak(name.to_owned().into_boxed_str())
}

impl Kind {
    /// Extract (resname_idx, count, ql) from a Drop kind.
    pub fn drop_info(&self) -> Option<(u16, u8, u8)> {
        match self {
            Kind::Drop { resname_idx, ql } => Some((*resname_idx, 1, *ql)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Predators in a saturated world must engage the player: chase, open
    /// the Fightview window and start swinging back.
    #[tokio::test]
    async fn predator_engages_player_in_reach() {
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = Game::new(42, cmd_rx, net_rx, true);
        let (tx, mut _rx) = tokio::sync::mpsc::unbounded_channel();
        let (raw_tx, _raw_rx) = tokio::sync::mpsc::unbounded_channel();
        g.session_connected(1, tx, raw_tx);
        // Select the character through the normal widget path.
        let wid = g
            .sessions
            .get(&1)
            .unwrap()
            .widgets
            .iter()
            .find(|(_, t)| t.as_str() == "charlist")
            .map(|(k, _)| *k)
            .expect("charlist widget");
        g.on_wdgmsg(
            1,
            wid,
            "play",
            vec![hnh_proto::ListArg::Str("hunter".to_owned())],
        );
        // Populate the player's grid with saturated wildlife.
        g.on_mapreq(1, (0, 0));
        // Find a wolf or boar and teleport the player into melee reach.
        let predator = (0..g.world.animal_gobs.len())
            .map(|i| g.world.animal_gobs[i])
            .find(|&id| {
                let slot = g.world.gobs.get(id).unwrap();
                matches!(g.world.gobs.kind[slot], Kind::Animal { species } if species.aggressive())
            })
            .expect("saturated grid spawns predators");
        let pslot = predator_slot(&g);
        let (ax, ay) = g.world.gobs.pos[pslot];
        let pgob = g.world.players[0].gob;
        let pslot2 = g.world.gobs.get(pgob).unwrap();
        g.world.gobs.pos[pslot2] = (ax + 5, ay);
        info!(?ax, ?ay, "teleported player next to predator");
        // Run ticks until the engagement opens.
        let mut fought = false;
        for _ in 0..300 {
            g.tick();
            if !g.world.animal_fights.is_empty() {
                fought = true;
                break;
            }
        }
        assert!(fought, "predator must engage a player in reach");
    }

    fn predator_slot(g: &Game) -> usize {
        for &id in &g.world.animal_gobs {
            if let Some(slot) = g.world.gobs.get(id) {
                let aggro = matches!(
                    g.world.gobs.kind[slot],
                    Kind::Animal { species } if species.aggressive()
                );
                if aggro {
                    return slot;
                }
            }
        }
        panic!("BUG: no predator");
    }

    /// The bootstrap stream must announce avatar RESIDs before the
    /// charlist add (session-lifecycle.md 3.1).
    #[tokio::test]
    async fn bootstrap_announces_resids_first() {
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_net_tx, net_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = Game::new(42, cmd_rx, net_rx, false);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (raw_tx, _raw_rx) = tokio::sync::mpsc::unbounded_channel();
        g.session_connected(1, tx, raw_tx);
        // Inspect the wire table state after registration.
        {
            let out = g.sessions.get(&1).unwrap();
            eprintln!(
                "WIRE: {:?}",
                (0..out.res.wire_count())
                    .map(|w| out.res.pending_announce(w as u16))
                    .collect::<Vec<_>>()
            );
        }
        drop(g); // close channels to end the recv loop
        let mut types = Vec::new();
        while let Ok(p) = rx.try_recv() {
            types.push(p[0]);
        }
        {
            eprintln!("TYPES: {:?}", types);
            assert_eq!(&types[..3], &[RMSG_RESID, RMSG_RESID, RMSG_RESID]);
        }
        assert!(types.contains(&RMSG_NEWWDG));
    }
}
