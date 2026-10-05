//! In-process load-test bots.
//!
//! Bots drive the real UDP session path (loopback): handshake, widget
//! bootstrap, character select, then a behavior loop that exercises the
//! master-prompt gameplay triangle — walking around, fighting the wildlife,
//! harvesting trees/stones, and picking up the drops. Sessions run as tokio
//! tasks on a single runtime worker each, so thousands of bots fit far below
//! the process/thread ulimit. This gives an end-to-end load and correctness
//! signal identical to real clients (same socket, same reliability layer,
//! same widget protocol).
//!
//! Interaction targeting works at the wire level: each session receives
//! RMSG_RESID resource-name announcements and MSG_OBJDATA gob blocks, builds
//! a small view of the surrounding gobs, classifies them by resource name,
//! and clicks concrete gob ids exactly like a real client's MapView does.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tracing::info;

use hnh_proto::consts::*;
use hnh_proto::{RelReceiver, RelSender};

// Cohort-wide counters; run() logs the final totals as the load verdict.
static STAT_FIGHTS: AtomicU64 = AtomicU64::new(0);
static STAT_HARVESTS: AtomicU64 = AtomicU64::new(0);
static STAT_PICKUPS: AtomicU64 = AtomicU64::new(0);
static STAT_WALKS: AtomicU64 = AtomicU64::new(0);
static STAT_BITES: AtomicU64 = AtomicU64::new(0);
static STAT_GOB_OBS: AtomicU64 = AtomicU64::new(0);
static STAT_RES_NAMES: AtomicU64 = AtomicU64::new(0);
static STAT_CLS_ANIMAL: AtomicU64 = AtomicU64::new(0);
static STAT_CLS_TREE: AtomicU64 = AtomicU64::new(0);
static STAT_CLS_STONE: AtomicU64 = AtomicU64::new(0);
static STAT_CLS_DROP: AtomicU64 = AtomicU64::new(0);
static STAT_CLS_PLAYER: AtomicU64 = AtomicU64::new(0);
static STAT_CLS_OTHER: AtomicU64 = AtomicU64::new(0);

pub async fn run(count: usize, secs: u64) {
    info!(count, secs, "spawning load-test bots");
    let start = Instant::now();
    let mut handles = Vec::with_capacity(count);
    for i in 0..count {
        // Stagger logins to mimic a realistic ramp without starving the
        // server's bootstrap path at the 1000-session scale.
        tokio::time::sleep(Duration::from_millis(30)).await;
        handles.push(tokio::spawn(bot_session(i, secs)));
    }
    let mut ok = 0usize;
    for h in handles {
        if h.await.unwrap_or(false) {
            ok += 1;
        }
    }
    info!(
        connected = ok,
        total = count,
        fights = STAT_FIGHTS.load(Ordering::Relaxed),
        harvests = STAT_HARVESTS.load(Ordering::Relaxed),
        pickups = STAT_PICKUPS.load(Ordering::Relaxed),
        bites_taken = STAT_BITES.load(Ordering::Relaxed),
        walks = STAT_WALKS.load(Ordering::Relaxed),
        gobs_seen = STAT_GOB_OBS.load(Ordering::Relaxed),
        res_names = STAT_RES_NAMES.load(Ordering::Relaxed),
        cls_animals = STAT_CLS_ANIMAL.load(Ordering::Relaxed),
        cls_trees = STAT_CLS_TREE.load(Ordering::Relaxed),
        cls_stones = STAT_CLS_STONE.load(Ordering::Relaxed),
        cls_drops = STAT_CLS_DROP.load(Ordering::Relaxed),
        cls_players = STAT_CLS_PLAYER.load(Ordering::Relaxed),
        cls_other = STAT_CLS_OTHER.load(Ordering::Relaxed),
        elapsed_secs = start.elapsed().as_secs(),
        "bot cohort finished"
    );
}

/// Gameplay class of a gob, resolved from its wire resource name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GobClass {
    Animal,
    Tree,
    Stone,
    Drop,
    Player,
    Other,
}

/// Classify a gob by its announced resource name. The kritter check must
/// precede the body check: kritter pose bases are named `.../<sp>/body`.
pub fn classify(res_name: &str) -> GobClass {
    if res_name.contains("/kritter/") {
        GobClass::Animal
    } else if res_name.contains("/borka") {
        GobClass::Player
    } else if res_name.contains("/trees/") {
        GobClass::Tree
    } else if res_name.contains("/bumlings/") {
        GobClass::Stone
    } else if res_name.contains("/invobjs/") {
        GobClass::Drop
    } else {
        GobClass::Other
    }
}

/// One decoded MSG_OBJDATA operation (the subset bots act on).
#[derive(Debug, Clone, PartialEq)]
pub enum ObjOp {
    /// Gob removed (id).
    Remove(i32),
    /// Gob position (id, x, y).
    Move(i32, i32, i32),
    /// Movement target (id, sx, sy, tx, ty) — recorded as the target pos.
    Lin(i32, i32, i32, i32, i32),
    /// Flat resource spawn (id, wire res id).
    Res(i32, u16),
    /// Layered spawn base (id, wire base res id).
    Layers(i32, u16),
    /// Bite overlay landed on the gob (id, overlay wire res).
    Overlay(i32, u16),
    /// Player name plate (id, name) — carries the bot's own gob identity.
    Buddy(i32, String),
}

/// Parse one MSG_OBJDATA datagram payload into ops. Unknown ops abort the
/// remaining block the way the client's reader would fail: the datagram is
/// dropped whole rather than misparsed (the server only emits ops bots know,
/// so this never happens against this server).
pub fn parse_objdata(payload: &[u8]) -> Vec<ObjOp> {
    let mut out = Vec::new();
    let mut m = hnh_proto::MessageBuf::from_slice(payload);
    'blocks: while !m.eom() {
        // One block: uint8 flags, int32 id, int32 frame, then ops.
        let (Ok(fl), Ok(id), Ok(_frame)) = (m.u8(), m.i32(), m.i32()) else {
            break;
        };
        if fl & 1 != 0 {
            out.push(ObjOp::Remove(id));
            continue;
        }
        loop {
            let Ok(op) = m.u8() else { break 'blocks };
            match op {
                OD_REM => out.push(ObjOp::Remove(id)),
                OD_MOVE => {
                    let (Ok(x), Ok(y)) = (m.i32(), m.i32()) else {
                        break 'blocks;
                    };
                    out.push(ObjOp::Move(id, x, y));
                }
                OD_RES => {
                    let Ok(mut resid) = m.u16() else {
                        break 'blocks;
                    };
                    if resid & 0x8000 != 0 {
                        resid &= !0x8000;
                        let Ok(sdt_len) = m.u8() else { break 'blocks };
                        if m.skip(sdt_len as usize).is_err() {
                            break 'blocks;
                        }
                    }
                    out.push(ObjOp::Res(id, resid));
                }
                OD_LINBEG => {
                    let (Ok(sx), Ok(sy), Ok(tx), Ok(ty), Ok(_st)) =
                        (m.i32(), m.i32(), m.i32(), m.i32(), m.i32())
                    else {
                        break 'blocks;
                    };
                    out.push(ObjOp::Lin(id, sx, sy, tx, ty));
                }
                OD_LINSTEP => {
                    if m.i32().is_err() {
                        break 'blocks;
                    }
                }
                OD_SPEECH => {
                    if m.i32().and_then(|_| m.i32()).and_then(|_| m.str()).is_err() {
                        break 'blocks;
                    }
                }
                OD_LAYERS => {
                    let Ok(base) = m.u16() else { break 'blocks };
                    loop {
                        let Ok(layer) = m.u16() else { break 'blocks };
                        if layer == 65535 {
                            break;
                        }
                    }
                    out.push(ObjOp::Layers(id, base));
                }
                OD_DRAWOFF => {
                    if m.i32().and_then(|_| m.i32()).is_err() {
                        break 'blocks;
                    }
                }
                OD_LUMIN => {
                    if m.i32()
                        .and_then(|_| m.i32())
                        .and_then(|_| m.u16())
                        .and_then(|_| m.u8())
                        .is_err()
                    {
                        break 'blocks;
                    }
                }
                OD_FOLLOW | OD_HOMING => {
                    // Server does not emit these; skip the gob block whole.
                    break 'blocks;
                }
                OD_OVERLAY => {
                    let (Ok(_olid), Ok(raw)) = (m.i32(), m.u16()) else {
                        break 'blocks;
                    };
                    if raw == 65535 {
                        // Overlay removal carries no sdt (client Session.java
                        // checks resid == 65535 before the flag branch).
                        continue;
                    }
                    let mut resid = raw;
                    if resid & 0x8000 != 0 {
                        resid &= !0x8000;
                        let Ok(sdt_len) = m.u8() else { break 'blocks };
                        if m.skip(sdt_len as usize).is_err() {
                            break 'blocks;
                        }
                    }
                    out.push(ObjOp::Overlay(id, resid));
                }
                OD_HEALTH => {
                    if m.u8().is_err() {
                        break 'blocks;
                    }
                }
                OD_BUDDY => {
                    let name = match m.str() {
                        Ok(n) => n,
                        Err(_) => break 'blocks,
                    };
                    if m.u8().and_then(|_| m.u8()).is_err() {
                        break 'blocks;
                    }
                    out.push(ObjOp::Buddy(id, name));
                }
                OD_END => break,
                _ => {
                    // Unknown op: stop parsing this datagram like the client
                    // reader would fail, rather than desync the byte stream.
                    break 'blocks;
                }
            }
        }
    }
    out
}

/// Per-session view of the world: resource names and nearby gobs.
#[derive(Default)]
struct BotView {
    /// This bot's player name (resolves the own gob via OD_BUDDY).
    name: String,
    /// Own gob id once the BUDDY plate lands on the spawn block.
    self_gob: Option<i32>,
    /// Last known own position (subtiles) from MOVE/LINBEG of the own gob.
    self_pos: Option<(i32, i32)>,
    /// Session wire id -> announced resource name.
    res_names: HashMap<u16, String>,
    /// Gob id -> (wire res id, pos x, pos y).
    gobs: HashMap<i32, (u16, i32, i32)>,
    /// Gobs whose wire id was not announced yet (reclassify on RESID).
    unnamed: Vec<i32>,
}

impl BotView {
    fn apply(&mut self, op: ObjOp, bites: bool) {
        match op {
            ObjOp::Remove(id) => {
                self.gobs.remove(&id);
            }
            ObjOp::Move(id, x, y) => {
                // Movement ops precede RES/LAYERS in a spawn block; insert a
                // placeholder so the position survives until the wire lands.
                let g = self.gobs.entry(id).or_insert((0, x, y));
                g.1 = x;
                g.2 = y;
                if Some(id) == self.self_gob {
                    self.self_pos = Some((x, y));
                }
            }
            ObjOp::Lin(id, _sx, _sy, tx, ty) => {
                // Track the movement target: close enough for targeting.
                let g = self.gobs.entry(id).or_insert((0, tx, ty));
                g.1 = tx;
                g.2 = ty;
                if Some(id) == self.self_gob {
                    self.self_pos = Some((tx, ty));
                }
            }
            ObjOp::Res(id, wire) | ObjOp::Layers(id, wire) => {
                // Keep a position already received in this block (the server
                // sends MOVE before RES/LAYERS); a (0,0) default marks the
                // gob position-unknown until the first MOVE/LIN lands.
                let e = self.gobs.entry(id).or_insert((wire, 0, 0));
                e.0 = wire;
                if !self.res_names.contains_key(&wire) {
                    self.unnamed.push(id);
                }
            }
            ObjOp::Overlay(_id, _wire) => {
                if bites {
                    STAT_BITES.fetch_add(1, Ordering::Relaxed);
                }
            }
            ObjOp::Buddy(id, name) => {
                if name == self.name {
                    self.self_gob = Some(id);
                }
            }
        }
    }

    /// A RESID announcement arrived: remember the name and reclassify any
    /// gobs that were waiting for this wire id.
    fn on_resid(&mut self, wire: u16, name: String) {
        self.res_names.insert(wire, name);
        if !self.unnamed.is_empty() {
            self.unnamed.retain(|id| {
                if let Some(g) = self.gobs.get(id) {
                    if g.0 == wire {
                        // Position may still be unknown (op order); leave the
                        // entry — targets without a position are skipped.
                        return false;
                    }
                }
                true
            });
        }
    }

    fn class_of(&self, wire: u16) -> GobClass {
        match self.res_names.get(&wire) {
            Some(n) => classify(n),
            None => GobClass::Other,
        }
    }

    /// Nearest gob of a class within `radius` subtiles of (x, y).
    fn nearest(&self, cls: GobClass, x: i32, y: i32, radius: i64) -> Option<(i32, i32, i32)> {
        let mut best: Option<(i32, i32, i32)> = None;
        let mut best_d2 = radius * radius;
        for (&id, &(wire, gx, gy)) in &self.gobs {
            if self.class_of(wire) != cls || (gx == 0 && gy == 0) {
                continue;
            }
            let dx = (gx - x) as i64;
            let dy = (gy - y) as i64;
            let d2 = dx * dx + dy * dy;
            if d2 <= best_d2 {
                best_d2 = d2;
                best = Some((id, gx, gy));
            }
        }
        best
    }
}

/// A chosen interaction target.
struct Target {
    gob: i32,
    at: (i32, i32),
    stat: fn(),
}

/// Pick the next action for a bot at (x, y). A weighted roll spreads the
/// cohort across the master-prompt triangle: ~50% fights, ~30% harvest,
/// ~20% loot, each branch falling through to the next class so a bot never
/// idles while any target exists in range.
fn pick_target(view: &BotView, x: i32, y: i32, roll: u32) -> Option<Target> {
    // Drops land near the harvested target, which the bot may have clicked
    // from up to ~8 tiles away; the pickup click has no server-side reach
    // check, so use a radius that covers the whole harvest zone.
    let drop = view.nearest(GobClass::Drop, x, y, 15 * 11);
    let animal = view.nearest(GobClass::Animal, x, y, 10 * 11);
    let harvest = view
        .nearest(GobClass::Tree, x, y, 8 * 11)
        .or_else(|| view.nearest(GobClass::Stone, x, y, 8 * 11));
    let stat_of = |cls: GobClass| -> fn() {
        match cls {
            GobClass::Animal => || {
                STAT_FIGHTS.fetch_add(1, Ordering::Relaxed);
            },
            GobClass::Tree | GobClass::Stone => || {
                STAT_HARVESTS.fetch_add(1, Ordering::Relaxed);
            },
            _ => || {
                STAT_PICKUPS.fetch_add(1, Ordering::Relaxed);
            },
        }
    };
    let pick = |t: Option<(i32, i32, i32)>, cls: GobClass| {
        t.map(|(gob, gx, gy)| Target {
            gob,
            at: (gx, gy),
            stat: stat_of(cls),
        })
    };
    // Ordered candidate chains per roll bucket.
    type Candidate = (GobClass, Option<(i32, i32, i32)>);
    let chain: &[Candidate] = &[
        (GobClass::Drop, drop),
        (GobClass::Animal, animal),
        (GobClass::Tree, harvest),
    ];
    let order: &[GobClass] = if roll < 5 {
        &[GobClass::Animal, GobClass::Tree, GobClass::Drop]
    } else if roll < 8 {
        &[GobClass::Tree, GobClass::Animal, GobClass::Drop]
    } else {
        &[GobClass::Drop, GobClass::Animal, GobClass::Tree]
    };
    let by_cls = |cls: GobClass| chain.iter().find(|(c, _)| *c == cls);
    order
        .iter()
        .filter_map(|cls| by_cls(*cls))
        .find_map(|&(cls, t)| pick(t, cls))
}

/// Outcome of the bootstrap phase.
struct Boot {
    mapview: Option<u16>,
}

/// One bot session: async socket, fixed behavior loop.
async fn bot_session(idx: usize, secs: u64) -> bool {
    let Ok(sock) = UdpSocket::bind("127.0.0.1:0").await else {
        return false;
    };
    let server: SocketAddr = "127.0.0.1:1870".parse().expect("BUG: literal");
    let mut rel_tx = RelSender::new();
    let mut rel_rx = RelReceiver::new();
    let mut rng = hnh_world::JavaRandom::new(idx as i64 ^ 0xB075);
    let name = format!("bot{idx:05}");
    let cookie = crate::auth().issue_cookie(&name);
    let mut view = BotView {
        name: name.clone(),
        ..BotView::default()
    };

    // --- handshake ---
    let mut sess = hnh_proto::MessageBuf::new();
    sess.uint8(MSG_SESS)
        .uint16(1)
        .string("Haven")
        .uint16(PVER)
        .string(&name)
        .bytes(&cookie);
    let sess = sess.finish();
    let start = Instant::now();
    let mut accepted = false;
    while start.elapsed() < Duration::from_secs(30) {
        let _ = sock.send_to(&sess, server).await;
        let mut buf = [0u8; 1500];
        if tokio::time::timeout(Duration::from_millis(500), sock.recv_from(&mut buf))
            .await
            .is_ok()
            && buf[0] == MSG_SESS
        {
            if buf.len() > 1 && buf[1] == 0 {
                accepted = true;
                break;
            }
            return false;
        }
    }
    if !accepted {
        return false;
    }

    // --- bootstrap: collect widgets until mapview appears, then play ---
    let boot = bootstrap(&sock, &mut rel_tx, &mut rel_rx, server, &name).await;
    let Some(mapview) = boot.mapview else {
        return false;
    };

    // Each bot owns a home tile area so the cohort spreads across distinct
    // grids (realistic MAPREQ streaming, per-grid population, and fights
    // with the local wildlife population). home_* are tile coordinates; the
    // grids covering them are requested like a real client (raw MAPREQ
    // datagrams) and clicks target subtiles within the same area.
    let home_tx = 60 + (rng.next_bounded(21) - 10) * 3;
    let home_ty = 60 + (rng.next_bounded(21) - 10) * 3;
    let home_gx = home_tx.div_euclid(100);
    let home_gy = home_ty.div_euclid(100);
    for gx in -1..=1 {
        for gy in -1..=1 {
            let mut req = hnh_proto::MessageBuf::new();
            req.uint8(MSG_MAPREQ)
                .int32(home_gx + gx)
                .int32(home_gy + gy);
            let _ = sock.send_to(&req.finish(), server).await;
        }
    }

    // --- behavior loop: walk / fight / harvest / loot for `secs` ---
    let behavior_end = Instant::now() + Duration::from_secs(secs);
    let mut next_action = Instant::now() + Duration::from_millis(500);
    let mut next_beat = Instant::now() + Duration::from_secs(5);
    let mut next_flush = Instant::now() + Duration::from_millis(20);
    let mut alive = true;
    while alive && Instant::now() < behavior_end {
        let mut buf = [0u8; 65536];
        let wait = next_flush
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(5));
        let got = tokio::time::timeout(wait, sock.recv_from(&mut buf)).await;
        if let Ok(Ok((n, _))) = got {
            match buf[0] {
                MSG_REL => {
                    for (ty, payload) in rel_rx.on_rel(&buf[1..n]) {
                        // on_rel payloads carry the rmsg type byte first.
                        if ty == RMSG_RESID {
                            parse_resid(&payload[1..], &mut view);
                        }
                    }
                }
                MSG_OBJDATA => {
                    // Track bites only on the bot's own neighborhood overlays
                    // (all critter bites carry the same fx resource; counting
                    // every one across the cohort measures combat activity).
                    for op in parse_objdata(&buf[1..n]) {
                        view.apply(op, true);
                    }
                }
                MSG_CLOSE => alive = false,
                _ => {}
            }
        }
        let now = Instant::now();
        if now >= next_action {
            next_action = now + Duration::from_millis(400 + rng.next_bounded(800) as u64);
            // Act from the own gob's streamed position; fall back to the
            // home tile center before the first own spawn block lands.
            let (px, py) = view.self_pos.unwrap_or((home_tx * 11, home_ty * 11));
            if let Some(t) = pick_target(&view, px, py, rng.next_bounded(10) as u32) {
                queue_click(&mut rel_tx, mapview, t.at, Some(t.gob));
                (t.stat)();
            } else {
                let jx = home_tx * 11 + rng.next_bounded(600) - 300;
                let jy = home_ty * 11 + rng.next_bounded(600) - 300;
                queue_click(&mut rel_tx, mapview, (jx, jy), None);
                STAT_WALKS.fetch_add(1, Ordering::Relaxed);
            }
        }
        if now >= next_beat {
            next_beat = now + Duration::from_secs(5);
            let _ = sock.send_to(&[MSG_BEAT], server).await;
        }
        if now >= next_flush {
            next_flush = now + Duration::from_millis(20);
            send_rel(&sock, &mut rel_tx, server).await;
        }
    }
    let _ = sock.send_to(&[MSG_CLOSE], server).await;
    STAT_GOB_OBS.fetch_add(view.gobs.len() as u64, Ordering::Relaxed);
    STAT_RES_NAMES.fetch_add(view.res_names.len() as u64, Ordering::Relaxed);
    for &(wire, x, y) in view.gobs.values() {
        let stat = match view.class_of(wire) {
            GobClass::Animal => &STAT_CLS_ANIMAL,
            GobClass::Tree => &STAT_CLS_TREE,
            GobClass::Stone => &STAT_CLS_STONE,
            GobClass::Drop => &STAT_CLS_DROP,
            GobClass::Player => &STAT_CLS_PLAYER,
            GobClass::Other => &STAT_CLS_OTHER,
        };
        let _ = (x, y);
        stat.fetch_add(1, Ordering::Relaxed);
    }
    alive
}

/// Decode one RMSG_RESID payload (uint16 wire, string name, uint16 ver).
fn parse_resid(payload: &[u8], view: &mut BotView) {
    let mut m = hnh_proto::MessageBuf::from_slice(payload);
    if let (Ok(wire), Ok(name)) = (m.u16(), m.str()) {
        view.on_resid(wire, name);
    }
}

/// Queue a mapview click. `gob = Some(id)` is an interaction click (the
/// server resolves fight/harvest/pickup by the target's kind), `None` walks.
fn queue_click(rel_tx: &mut RelSender, mapview: u16, at: (i32, i32), gob: Option<i32>) {
    // Wire shape of a real MapView click (MapView.java:738/747):
    // click(c0, mc, button, modflags[, gobid]); the server reads mc as the
    // SECOND coordinate in the argument list.
    let mut click = hnh_proto::MessageBuf::new();
    click
        .uint8(RMSG_WDGMSG)
        .uint16(mapview)
        .string("click")
        .lcoord(0, 0)
        .lcoord(at.0, at.1)
        .lint(1)
        .lint(0);
    if let Some(g) = gob {
        click.lint(g);
    }
    click.lend();
    rel_tx.queue(&click.finish());
}

async fn bootstrap(
    sock: &UdpSocket,
    rel_tx: &mut RelSender,
    rel_rx: &mut RelReceiver,
    server: SocketAddr,
    name: &str,
) -> Boot {
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut charlist: Option<u16> = None;
    let mut mapview: Option<u16> = None;
    let mut played = false;
    let mut next_flush = Instant::now();
    while Instant::now() < deadline {
        let mut buf = [0u8; 65536];
        let wait = next_flush
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(5));
        let got = tokio::time::timeout(wait, sock.recv_from(&mut buf)).await;
        if let Ok(Ok((n, _))) = got {
            match buf[0] {
                MSG_CLOSE => break,
                MSG_REL => {
                    for (ty, payload) in rel_rx.on_rel(&buf[1..n]) {
                        if ty == RMSG_NEWWDG {
                            let mut m = hnh_proto::MessageBuf::from_slice(&payload[1..]);
                            if let (Ok(wid), Ok(t)) = (m.u16(), m.str()) {
                                match t.as_str() {
                                    "charlist" => charlist = Some(wid),
                                    "mapview" => mapview = Some(wid),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Fire `play` as soon as we see the charlist.
        if let (Some(wid), false) = (charlist, played) {
            played = true;
            let mut play = hnh_proto::MessageBuf::new();
            play.uint8(RMSG_WDGMSG)
                .uint16(wid)
                .string("play")
                .lstr(name)
                .lend();
            rel_tx.queue(&play.finish());
        }
        if Instant::now() >= next_flush {
            next_flush = Instant::now() + Duration::from_millis(10);
            send_rel(sock, rel_tx, server).await;
        }
        if mapview.is_some() && played {
            return Boot { mapview };
        }
    }
    Boot { mapview }
}

async fn send_rel(sock: &UdpSocket, rel: &mut RelSender, server: SocketAddr) {
    for d in rel.poll_transmit(Instant::now(), 1200) {
        let _ = sock.send_to(&d, server).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server's own OD_RES encoding of a stone (no sdt): header + RES.
    fn stone_datagram() -> Vec<u8> {
        let mut m = hnh_proto::MessageBuf::new();
        m.uint8(MSG_OBJDATA)
            .uint8(0)
            .int32(777)
            .int32(0)
            .uint8(OD_RES)
            .uint16(3) // wire id 3, announced as a bumling
            .uint8(OD_MOVE)
            .coord(1010, 2020)
            .uint8(OD_END);
        m.finish()
    }

    #[test]
    fn class_resolves_by_res_name() {
        assert_eq!(classify("gfx/kritter/fox/body"), GobClass::Animal);
        assert_eq!(classify("gfx/borka/body"), GobClass::Player);
        assert_eq!(classify("gfx/terobjs/trees/fir"), GobClass::Tree);
        assert_eq!(classify("gfx/terobjs/bumlings/01"), GobClass::Stone);
        assert_eq!(classify("gfx/invobjs/wood"), GobClass::Drop);
        assert_eq!(classify("gfx/terobjs/stove"), GobClass::Other);
    }

    #[test]
    fn objdata_parses_res_move_and_end() {
        let ops = parse_objdata(&stone_datagram()[1..]);
        assert!(ops.contains(&ObjOp::Res(777, 3)));
        assert!(ops.contains(&ObjOp::Move(777, 1010, 2020)));
    }

    #[test]
    fn objdata_tracks_layers_and_removal() {
        let mut m = hnh_proto::MessageBuf::new();
        m.uint8(0)
            .int32(9)
            .int32(0)
            .uint8(OD_LAYERS)
            .uint16(11)
            .uint16(12)
            .uint16(13)
            .uint16(65535)
            .uint8(OD_HEALTH)
            .uint8(4)
            .uint8(OD_END)
            .uint8(1) // remove flag block
            .int32(9)
            .int32(1)
            .uint8(OD_END);
        let ops = parse_objdata(m.finish().as_slice());
        assert!(ops.contains(&ObjOp::Layers(9, 11)));
        assert!(ops.contains(&ObjOp::Remove(9)));
    }

    #[test]
    fn objdata_skips_overlay_removals_and_counts_adds() {
        let mut m = hnh_proto::MessageBuf::new();
        m.uint8(0)
            .int32(5)
            .int32(0)
            .uint8(OD_OVERLAY)
            .int32(-1)
            .uint16(65535) // overlay removal: not a bite
            .uint8(OD_OVERLAY)
            .int32(-3)
            .uint16(42) // bite fx add
            .uint8(OD_END);
        let ops = parse_objdata(m.finish().as_slice());
        assert!(ops.contains(&ObjOp::Overlay(5, 42)));
        assert!(!ops.iter().any(|o| matches!(o, ObjOp::Overlay(_, 65535))));
    }

    #[test]
    fn view_targets_the_nearest_drop_first() {
        let mut v = BotView::default();
        v.on_resid(1, "gfx/invobjs/wood".into());
        v.on_resid(2, "gfx/kritter/boar/body".into());
        v.on_resid(3, "gfx/terobjs/trees/fir".into());
        // Server wire order: RES/LAYERS first, then MOVE.
        v.apply(ObjOp::Res(100, 1), false);
        v.apply(ObjOp::Move(100, 1000, 1000), false);
        v.apply(ObjOp::Layers(101, 2), false);
        v.apply(ObjOp::Move(101, 1100, 1000), false);
        v.apply(ObjOp::Res(102, 3), false);
        v.apply(ObjOp::Move(102, 1400, 1000), false);
        // Roll 9 = loot bucket: the drop is picked first.
        let t = pick_target(&v, 1000, 1000, 9).expect("a target exists");
        assert_eq!(t.gob, 100, "loot bucket prefers the drop");
        assert_eq!(t.at, (1000, 1000));
    }

    #[test]
    fn view_fights_animal_when_no_drop() {
        let mut v = BotView::default();
        v.on_resid(2, "gfx/kritter/boar/body".into());
        v.on_resid(3, "gfx/terobjs/trees/fir".into());
        v.apply(ObjOp::Layers(101, 2), false);
        v.apply(ObjOp::Move(101, 1100, 1000), false);
        v.apply(ObjOp::Res(102, 3), false);
        v.apply(ObjOp::Move(102, 1400, 1000), false);
        // Roll 0 = fight bucket: the animal is picked over the tree.
        let t = pick_target(&v, 1000, 1000, 0).expect("a target exists");
        assert_eq!(t.gob, 101, "fight bucket prefers the animal");
    }

    #[test]
    fn resid_late_arrival_reclassifies() {
        let mut v = BotView::default();
        // Spawn arrives before the RESID announcement (wire id 7 unknown).
        v.apply(ObjOp::Res(33, 7), false);
        assert_eq!(v.class_of(7), GobClass::Other);
        v.on_resid(7, "gfx/terobjs/bumlings/02".into());
        assert_eq!(v.class_of(7), GobClass::Stone);
    }

    #[test]
    fn parse_resid_reads_wire_and_name() {
        let mut m = hnh_proto::MessageBuf::new();
        m.uint16(7).string("gfx/invobjs/stone").uint16(1);
        let mut v = BotView::default();
        parse_resid(m.finish().as_slice(), &mut v);
        assert_eq!(v.class_of(7), GobClass::Drop);
    }
}
