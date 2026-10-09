//! Black-box wire contracts for the compiled hnh-server binary.
//!
//! These tests close the pyramid gap between the in-module unit tests
//! (white-box, in-process Game) and the manual python probes: they boot
//! the real binary on ephemeral ports and speak the real protocol.
//! Each test owns its own ServerGuard (RAII, no shared state).

mod common;

use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};

use common::{ArgVal, ServerGuard, Session, REQUIRED_CATTR};
use hnh_proto::{MSG_SESS, PVER, SESSERR_AUTH};

const TREE_PREFIX: &str = "gfx/terobjs/trees/";
const BOULDER_PREFIX: &str = "gfx/terobjs/bumlings/";
const BRANCH_WORLD: &str = "gfx/terobjs/items/branch";
const STONE_WORLD: &str = "gfx/terobjs/items/stone";
const TROUGH_RES: &str = "gfx/terobjs/trough";
const BOULDER_STONES: u8 = 5;
// Per ground-click hop: 6 tiles, inside the path-check budget (the
// probe_walk.py / test_gather.py cadence).
const HOP: i32 = 66;
const NORTH_HOPS: usize = 40;

/// Full bootstrap into the world: charlist -> play -> mapview -> objdata,
/// plus the inventory window open (shared by the two game-flow tests).
fn enter_world(server: &ServerGuard, username: &str) -> (Session, i32) {
    let mut sess = Session::connect(server, username);
    assert!(
        sess.pump_until(|s| s.widgets_by_name.contains_key("charlist"), 10),
        "no charlist\n{}",
        server.log_tail(2000)
    );
    sess.send_play(username);
    assert!(
        sess.pump_until(|s| s.widgets_by_name.contains_key("mapview"), 15),
        "no mapview\n{}",
        server.log_tail(2000)
    );
    assert!(
        sess.pump_until(|s| s.player_gob.is_some() && s.objdata_datagrams > 0, 15),
        "no player gob / objdata\n{}",
        server.log_tail(2000)
    );
    // Open the inventory the way SlenHud does (the inv button) and wait
    // for the starter stacks to be visible.
    let slen = sess.widgets_by_name["slen"];
    sess.send_wdgmsg(slen, "inv", &[0]);
    let ready = |s: &Session| {
        s.item_by_res("gfx/invobjs/branch").is_some()
            && s.item_by_res("gfx/invobjs/stone").is_some()
    };
    assert!(
        sess.pump_until(ready, 10),
        "starter stacks never appeared\n{}",
        server.log_tail(2000)
    );
    let player = sess.player_gob.expect("player gob");
    (sess, player)
}

/// The count argument of an inventory item widget (the layout the
/// build-flow contract already pins: index 4 carries the stack size).
fn stack_count(sess: &Session, res: &str) -> Option<i32> {
    let wid = sess.item_by_res(res)?;
    sess.items.get(&wid)?.get(4).and_then(ArgVal::as_int)
}

/// `inv take` with an acknowledgment loop: the take is only accepted
/// server-side when the target wid is the LIVE inventory widget. An
/// inventory refresh (every take/sink/drop rebuilds the item widget set,
/// session-61 note) may retire the wid a stale client snapshot still
/// resolves to - the server refuses that take silently and the retry
/// re-resolves the fresh wid. Returns when the named stack IS on the
/// cursor (the cursor widget carries drag=1).
fn take_to_cursor(sess: &mut Session, res: &str, tag: &str) {
    for attempt in 0..5 {
        if let Some(wid) = sess.item_by_res(res) {
            sess.inv_take(wid);
            if sess.pump_until(|s| s.cursor_held(Some(res)), 2) {
                return;
            }
        }
        // Settle the widget rebuild storm, then re-resolve.
        std::thread::sleep(Duration::from_millis(300));
        let _ = attempt;
    }
    panic!(
        "{tag}: {res} never landed on the cursor (items={:?})\nchat={:?}",
        sess.items, sess.chat_lines
    );
}

/// Release whatever the cursor holds back into the inventory (the
/// session-51 cursor-return contract) and wait for the empty cursor.
fn return_cursor(sess: &mut Session, tag: &str) {
    if sess.cursor_held(None) {
        sess.inv_drop();
        let empty = sess.pump_until(|s| !s.cursor_held(None), 5);
        assert!(
            empty,
            "{tag}: cursor never emptied after the inventory drop\nchat={:?}",
            sess.chat_lines
        );
        // Settle the post-drop refresh so later wid resolutions are live.
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Contract: dev TLS auth -> cookie -> MSG_SESS -> full bootstrap
/// (charlist -> play -> mapview + 3x3 MAPREQ -> MAPDATA + OBJDATA) with
/// the complete REQUIRED_CATTR set present BEFORE the `chr` widget
/// creation, and the attack paginae shipped.
#[test]
fn world_entry_bootstrap_contract() {
    let _slot = common::acquire_test_slot();
    // Arrange
    let server = ServerGuard::boot("world_entry");
    let mut sess = Session::connect(&server, "boottest");
    assert_eq!(
        sess.widgets_by_name.get("charlist"),
        None,
        "pristine session"
    );

    // Act: the legacy bootstrap flow.
    assert!(
        sess.pump_until(|s| s.widgets_by_name.contains_key("charlist"), 10),
        "no charlist widget within 10s\n{}",
        server.log_tail(2000)
    );
    sess.send_play("boottest");
    assert!(
        sess.pump_until(|s| s.widgets_by_name.contains_key("mapview"), 15),
        "no mapview widget after play\n{}",
        server.log_tail(2000)
    );
    let entered = sess.pump_until(
        |s| s.mapdata_datagrams > 0 && s.objdata_datagrams > 0 && s.cattr_at_chr.is_some(),
        15,
    );

    // Assert
    let stats = format!(
        "widgets={:?} mapdata={} objdata={} cattr={} chr={:?} rel_held={}\n",
        sess.widgets,
        sess.mapdata_datagrams,
        sess.objdata_datagrams,
        sess.cattr_names.len(),
        sess.cattr_at_chr.as_ref().map(|s| s.len()),
        sess.debug_held_len(),
    );
    assert!(
        entered,
        "bootstrap incomplete\n{stats}\n{}",
        server.log_tail(2000)
    );
    let at_chr = sess.cattr_at_chr.clone().expect("chr seen");
    let missing: Vec<_> = REQUIRED_CATTR
        .iter()
        .filter(|n| !at_chr.contains(**n))
        .copied()
        .collect();
    assert!(
        missing.is_empty(),
        "cattr missing at chr creation: {missing:?}"
    );
    // 3x3 grids requested; every grid answers with at least one MAPDATA
    // datagram (fragments only add more). Wait rather than assert: raw
    // MAPDATA rides lossy UDP and the harness re-requests grids on the
    // legacy client cadence, so under parallel-test CPU load the nine
    // answers may land seconds apart.
    let nine = sess.pump_until(|s| s.mapdata_datagrams >= 9, 20);
    assert!(
        nine,
        "mapdata datagrams {} < 9\n{}",
        sess.mapdata_datagrams,
        server.log_tail(2000)
    );
    assert!(sess.objdata_datagrams > 0, "no objdata stream");
    assert!(
        sess.player_gob.is_some(),
        "mapview args did not carry the player gob id"
    );
    assert!(
        sess.paginae_atk.contains("paginae/atk/atk"),
        "attack paginae missing from the login push: {:?}",
        sess.paginae_atk
    );
}

/// Contract: a MSG_SESS with a cookie that never left the auth server is
/// rejected with SESSERR_AUTH and no session is created.
#[test]
fn session_rejects_bogus_cookie() {
    let _slot = common::acquire_test_slot();
    // Arrange
    let server = ServerGuard::boot("badcookie");
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    sock.set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .expect("timeout");
    let addr = ("127.0.0.1", server.game_port)
        .to_socket_addrs()
        .expect("addr")
        .next()
        .expect("addr");

    // Act: handshake with garbage cookie (legacy retransmit cadence).
    let mut msg = vec![MSG_SESS];
    msg.extend_from_slice(&1u16.to_le_bytes());
    msg.extend_from_slice(b"Haven\0");
    msg.extend_from_slice(&PVER.to_le_bytes());
    msg.extend_from_slice(b"ghost\0");
    msg.extend_from_slice(&[0xABu8; 32]);
    let mut reply = None;
    for _ in 0..5 {
        sock.send_to(&msg, addr).expect("send");
        let mut buf = [0u8; 64];
        match sock.recv_from(&mut buf) {
            Ok((n, _)) if n >= 2 && buf[0] == MSG_SESS => {
                reply = Some((buf[0], buf[1]));
                break;
            }
            Ok(_) => continue,
            Err(_) => continue,
        }
    }

    // Assert
    assert_eq!(
        reply,
        Some((MSG_SESS, SESSERR_AUTH)),
        "bogus cookie must answer MSG_SESS/SESSERR_AUTH"
    );
}

/// Session 64: a lost statics spawn wave is RECOVERED by the server's
/// OBJACK-driven retransmission sweep. The harness drops every inbound
/// OBJDATA datagram for 1.2 s (the whole statics burst plus the first
/// fast-schedule retransmits land inside the window) - exactly like a
/// UDP loss window on a loaded link. Gob state has no client-side
/// re-request (the session-63 finding that made lost spawns
/// unrecoverable), so the only path a LIVE in-view static can arrive
/// through is the server-side sweep. Far statics (the seed-42 forest
/// stands outside the spawn meadow) ride spawn+retract pairs: the
/// retract supersedes the spawn in the unacked table, so only their
/// OD_REM retransmits and the client never sees the spawn - by design.
/// The deterministic tail holds OBJACKs and demands a duplicate
/// (id, frame) on the wire - the sweep's in-order walk resending an
/// unconfirmed block.
#[test]
fn lost_static_spawn_wave_is_retransmitted() {
    let _slot = common::acquire_test_slot();
    let server = ServerGuard::boot("retrans");
    let mut sess = Session::connect(&server, "retransuser");
    assert!(
        sess.pump_until(|s| s.widgets_by_name.contains_key("charlist"), 10),
        "no charlist\n{}",
        server.log_tail(2000)
    );
    sess.send_play("retransuser");
    assert!(
        sess.pump_until(|s| s.widgets_by_name.contains_key("mapview"), 15),
        "no mapview\n{}",
        server.log_tail(2000)
    );
    assert!(
        sess.pump_until(|s| s.objdata_datagrams > 0, 15),
        "no objdata\n{}",
        server.log_tail(2000)
    );
    // Arm the loss window BEFORE the statics burst leaves (world entry
    // streams it right after the player's own spawn block).
    sess.drop_objdata_until = Some(Instant::now() + Duration::from_millis(1200));
    let drained = sess.pump_until(
        |s| s.dropped_objdata > 0 && s.drop_objdata_until.is_none(),
        15,
    );
    assert!(
        drained && sess.dropped_objdata > 10,
        "drop window never consumed the spawn wave (dropped={})\n{}",
        sess.dropped_objdata,
        server.log_tail(2000)
    );
    // Recovery: a LIVE in-view static (a boulder) must arrive through
    // the retransmission sweep - no other mechanism can deliver it.
    // The !removed filter is the same one the gathering contract uses.
    let got_static = sess.pump_until(
        |s| {
            s.gobs
                .iter()
                .any(|(gid, g)| !g.removed && s_res_prefix(s, *gid, BOULDER_PREFIX))
        },
        20,
    );
    assert!(
        got_static,
        "no in-view static arrived after the drop window (dropped={}, gobs={})\n{}",
        sess.dropped_objdata,
        sess.gobs.len(),
        server.log_tail(2000)
    );
    // Deterministic sweep proof: stop echoing OBJACKs; the sweep must
    // resend an already-decoded (id, frame) block within its schedule.
    sess.hold_objacks = true;
    let reseen = sess.pump_until(|s| s.saw_retransmitted_spawn, 12);
    sess.hold_objacks = false;
    assert!(
        reseen,
        "no duplicate (id, frame) block while OBJACKs were held\n{}",
        server.log_tail(2000)
    );
}

/// Contract: a mapview ground click produces an own-gob OD_LINBEG toward
/// the clicked subtile followed by monotonic OD_LINSTEP progress frames
/// that approach the target without a teleport.
#[test]
fn movement_click_walks_with_linstep_progress() {
    let _slot = common::acquire_test_slot();
    // Arrange: full bootstrap (same flow as world_entry_bootstrap_contract).
    let server = ServerGuard::boot("movement");
    let mut sess = Session::connect(&server, "walker");
    assert!(
        sess.pump_until(|s| s.widgets_by_name.contains_key("charlist"), 10),
        "no charlist\n{}",
        server.log_tail(2000)
    );
    sess.send_play("walker");
    assert!(
        sess.pump_until(|s| s.widgets_by_name.contains_key("mapview"), 15),
        "no mapview\n{}",
        server.log_tail(2000)
    );
    assert!(
        sess.pump_until(|s| s.objdata_datagrams > 0, 15),
        "no objdata\n{}",
        server.log_tail(2000)
    );
    let player_gob = sess.player_gob.expect("player gob id in mapview args");

    // Act: ground click 20 tiles east of the spawn center (555, 555)
    // subtiles - the probe_walk.py coordinates. The re-click mirrors the
    // real client: the mv-phase LINBEG batch rides RAW UDP, a lost frame
    // is invisible to the server, and a player whose walk did not start
    // clicks again (each click retargets; the +/-22 tolerance below
    // absorbs the interpolation offset).
    let own_linbeg = |s: &Session| {
        s.gobs
            .get(&player_gob)
            .and_then(|g| g.linbeg)
            .filter(|l| l.tx > l.sx) // eastward
    };
    let mut linbeg = None;
    for _ in 0..4 {
        sess.click_ground(555 + 220, 555);
        if sess.pump_until(|s| own_linbeg(s).is_some(), 5) {
            linbeg = own_linbeg(&sess);
            break;
        }
    }
    assert!(
        linbeg.is_some(),
        "no own-gob LINBEG after ground click (unacked rel dgrams: {})\n{}",
        sess.debug_pending_rel(),
        server.log_tail(2000)
    );
    let linbeg = linbeg.expect("LINBEG asserted above");
    // The clicked MAP POINT is the target (tx ~= 775, ty ~= 555): the
    // re-click retargets from the interpolated position, so the segment
    // LENGTH (tx - sx) is whatever remains of the walk, not 220.
    assert!(
        (linbeg.tx - (555 + 220)).abs() <= 22,
        "LINBEG target is not the clicked point: tx={}",
        linbeg.tx
    );
    assert!(
        (linbeg.ty - 555).abs() <= 22,
        "LINBEG drifts on y: ty={}",
        linbeg.ty
    );
    assert!(
        linbeg.tx > linbeg.sx,
        "LINBEG must head east: sx={} tx={}",
        linbeg.sx,
        linbeg.tx
    );

    // Assert 2: LINSTEP step indices ascend to the LINBEG's step count,
    // and the arrival OD_MOVE snaps the gob exactly onto the clicked
    // target tile (the server's movement-interpolation contract: no
    // teleport, no overshoot, exact arrival).
    let reached = sess.pump_until(
        |s| {
            s.gobs
                .get(&player_gob)
                .map(|g| {
                    let c = g.linbeg.map(|l| l.c).unwrap_or(0);
                    g.linsteps.last().copied().unwrap_or(0) >= c
                })
                .unwrap_or(false)
        },
        30,
    );
    let own = &sess.gobs[&player_gob];
    let steps = &own.linsteps;
    let linbeg = own.linbeg.expect("own-gob LINBEG already asserted");
    assert!(
        !steps.is_empty(),
        "no LINSTEP progress frames\n{}",
        server.log_tail(2000)
    );
    let dump = format!(
        "own gob {player_gob}: linbegs={:?} last_moves={:?} step_tail={:?}\n",
        own.linbegs,
        own.moves.iter().rev().take(3).collect::<Vec<_>>(),
        steps.iter().rev().take(10).collect::<Vec<_>>(),
    );
    assert!(
        reached,
        "LINSTEP never reached the step count c={} (last={})\n{dump}\n{}",
        linbeg.c,
        steps.last().copied().unwrap_or(0),
        server.log_tail(2000)
    );
    // Monotonic per-frame progress (interpolation never rewinds).
    for pair in steps.windows(2) {
        assert!(pair[1] >= pair[0], "LINSTEP rewound: {steps:?}");
    }
    // No teleport: the tick-cadence step advance is bounded (observed
    // ~3 steps/frame at the 5 Hz LINSTEP cadence; 25 leaves a wide
    // margin, a teleport would jump most of the walk in one frame).
    let max_step = steps.windows(2).map(|p| p[1] - p[0]).max().unwrap_or(0);
    assert!(
        max_step <= 25,
        "LINSTEP jump of {max_step} steps exceeds the walk-cadence bound"
    );
    // Arrival: the last MOVE op must be the exact clicked target.
    let last_move = own.moves.last().copied().expect("arrival MOVE op");
    assert_eq!(
        last_move,
        (linbeg.tx, linbeg.ty),
        "gob did not arrive exactly at the clicked target"
    );
}

/// Contract: the oven build pipeline over the real wire, including the
/// cursor-remainder rule the session-51 probe fix pinned. The plan sink
/// caps at the demand line; the undelivered remainder of the held stack
/// STAYS on the drag cursor (legacy behavior) and the inventory `drop`
/// wdgmsg returns it. Without the return, the next inv take is refused
/// (one cursor item at a time) and the plan stalls at stage 1 - the
/// regression that hid between the session-36 kit bump and session 50.
#[test]
fn build_flow_sinks_partial_stack_then_completes_after_cursor_return() {
    let _slot = common::acquire_test_slot();
    // Arrange: full bootstrap + the inventory window (the slen button).
    let server = ServerGuard::boot("buildflow");
    let mut sess = Session::connect(&server, "builder");
    assert!(
        sess.pump_until(|s| s.widgets_by_name.contains_key("charlist"), 10),
        "no charlist\n{}",
        server.log_tail(2000)
    );
    sess.send_play("builder");
    assert!(
        sess.pump_until(|s| s.widgets_by_name.contains_key("mapview"), 15),
        "no mapview\n{}",
        server.log_tail(2000)
    );
    assert!(
        sess.pump_until(|s| s.player_gob.is_some() && s.objdata_datagrams > 0, 15),
        "no player gob / objdata\n{}",
        server.log_tail(2000)
    );
    // Open the inventory the way SlenHud does, then wait until the
    // starter stacks (and their RESID announcements) are visible.
    let slen = sess.widgets_by_name["slen"];
    sess.send_wdgmsg(slen, "inv", &[0]);
    let items_ready = |s: &Session| {
        s.item_by_res("gfx/invobjs/stone").is_some()
            && s.item_by_res("gfx/invobjs/branch").is_some()
    };
    assert!(
        sess.pump_until(items_ready, 10),
        "starter stone/branch widgets never appeared\n{}",
        server.log_tail(2000)
    );

    // Act 1: arm the oven build pagina; the mapview must answer with the
    // ghost-drive uimsg naming the resource and the on-tile flag.
    sess.menu_act("oven");
    assert!(
        sess.pump_until(|s| s.last_wdgmsg("place").is_some(), 10),
        "no mapview place uimsg\n{}",
        server.log_tail(2000)
    );
    let place = sess.last_wdgmsg("place").expect("place seen").clone();
    assert_eq!(
        place.first().and_then(|a| a.as_str()),
        Some("gfx/terobjs/oven"),
        "place uimsg names the wrong resource: {place:?}"
    );
    assert_eq!(
        place.get(2).and_then(|a| a.as_int()),
        Some(1),
        "on-tile flag missing from the place uimsg: {place:?}"
    );

    // Act 2: commit the ghost one tile east of the player.
    let ppos = sess
        .gob_pos(sess.player_gob.expect("player gob"))
        .expect("player MOVE op");
    let (ptx, pty) = (ppos.0.div_euclid(11), ppos.1.div_euclid(11));
    let mc = ((ptx + 1) * 11 + 5, pty * 11 + 5);
    sess.send_place(mc.0, mc.1, 1, 0);
    let oven_wire = sess
        .res_names
        .iter()
        .find(|(_, n)| n.as_str() == "gfx/terobjs/oven")
        .map(|(w, _)| *w)
        .expect("oven resource announced");
    let plan_seen = |s: &Session| {
        s.gobs
            .values()
            .any(|g| g.res == Some(oven_wire) && g.sdt.as_deref() == Some(&[0u8][..]))
    };
    assert!(
        sess.pump_until(plan_seen, 10),
        "plan gob never spawned\n{}",
        server.log_tail(2000)
    );
    let plan_gob = sess
        .gobs
        .iter()
        .find(|(_, g)| g.res == Some(oven_wire) && g.sdt.as_deref() == Some(&[0u8][..]))
        .map(|(gid, _)| *gid)
        .expect("plan gob id");

    // Count helper hoisted here: the starter kit size is not a contract
    // of this test, only (starter - demand) is.
    let stone_stack_count = |s: &Session| -> Option<i32> {
        let wid = s.item_by_res("gfx/invobjs/stone")?;
        s.items.get(&wid)?.get(4).and_then(ArgVal::as_int)
    };

    // Act 3: sink the stone stack. Demand is stone x2; the take removes
    // the WHOLE starter stack, the plan credits 2, and the rest rides
    // back on the cursor.
    let stone_wid = sess.item_by_res("gfx/invobjs/stone").expect("stone wid");
    let starter_stones = stone_stack_count(&sess).expect("starter stone count");
    sess.inv_take(stone_wid);
    sess.map_itemact(mc.0, mc.1, plan_gob);
    assert!(
        sess.pump_until(
            |s| s.gobs.get(&plan_gob).and_then(|g| g.sdt.clone()).as_deref() == Some(&[1u8][..]),
            10
        ),
        "stage never advanced after the stone sink\n{}",
        server.log_tail(2000)
    );

    // The cursor now holds the remainder (2 stones, drag flag set).
    let cursor_held = |s: &Session| {
        s.items
            .values()
            .any(|a| a.get(2).and_then(ArgVal::as_int) == Some(1))
    };
    assert!(
        sess.pump_until(cursor_held, 5),
        "stone remainder never appeared on the cursor\n{}",
        server.log_tail(2000)
    );

    // Act 4: the KEY regression contract - return the remainder to the
    // inventory, then take the branch and finish the plan. Without the
    // drop the take below would be silently refused and the plan would
    // stall at stage 1.
    sess.inv_drop();
    assert!(
        sess.pump_until(|s| !cursor_held(s), 5),
        "cursor never emptied after the inventory drop\n{}",
        server.log_tail(2000)
    );
    // The stone remainder is back in the inventory as its own stack:
    // the take removed the whole stack, the plan credited 2, so the
    // drop returns exactly starter - 2. (Checked on the stone stack
    // itself - other stacks, e.g. string, also carry count 2.)
    let merged = sess.pump_until(|s| stone_stack_count(s) == Some(starter_stones - 2), 5);
    let items_dump: Vec<_> = sess
        .items
        .iter()
        .map(|(wid, a)| {
            (
                *wid,
                a.iter()
                    .map(|v| match v {
                        ArgVal::Int(v) => v.to_string(),
                        ArgVal::Str(s) => format!("{s:?}"),
                        ArgVal::Coord(x, y) => format!("({x},{y})"),
                        ArgVal::Color(r, g, b, c) => format!("col({r},{g},{b},{c})"),
                    })
                    .collect::<Vec<_>>()
                    .join(","),
            )
        })
        .collect();
    let stone_seen = stone_stack_count(&sess);
    assert!(
        merged,
        "stone remainder never returned to the inventory (stone stack count = {stone_seen:?}); items={items_dump:?}\n{}",
        server.log_tail(2000)
    );

    let branch_wid = sess
        .item_by_res("gfx/invobjs/branch")
        .expect("branch wid after cursor return");
    sess.inv_take(branch_wid);
    sess.map_itemact(mc.0, mc.1, plan_gob);

    // Assert: the last demand line fills -> the plan completes in place
    // (station conversion re-renders with the unlit sdt byte 0).
    assert!(
        sess.pump_until(
            |s| s.gobs.get(&plan_gob).and_then(|g| g.sdt.clone()).as_deref() == Some(&[0u8][..]),
            10
        ),
        "plan never completed after the branch sink\n{}",
        server.log_tail(2000)
    );
    let plan = &sess.gobs[&plan_gob];
    assert_eq!(
        plan.res,
        Some(oven_wire),
        "completion must convert the plan in place (same gob id and resource)"
    );
}

/// Walk the character toward `target` until within `stop` subtiles
/// (Chebyshev) - the approach the real client performs before an act;
/// the drop spawns next to the object and streams inside VIEW_RADIUS
/// (300), so acting from <=220 guarantees the drop is visible. Hops are
/// 6-tile ground clicks (inside the path-check budget, the probe_walk
/// cadence); rapid clicks retarget from the interpolated position.
/// Returns false when the walk budget is exhausted (unreachable object:
/// water, rocks - the caller picks another candidate).
fn approach_opt(sess: &mut Session, player: i32, target: (i32, i32), stop: i32) -> bool {
    for _ in 0..60 {
        let pos = sess.gob_pos(player).expect("player pos");
        let (dx, dy) = (target.0 - pos.0, target.1 - pos.1);
        if dx.abs().max(dy.abs()) <= stop {
            return true;
        }
        // One axis per hop (the larger distance first): diagonal clicks
        // can hit terrain the axis-aligned path avoids, and the server
        // speed budget is Manhattan (~2.3s per 66-subtile axis hop).
        let step = |v: i32| v.clamp(-HOP, HOP);
        let steps_before = sess
            .gobs
            .get(&player)
            .map(|g| g.linsteps.len())
            .unwrap_or(0);
        if dx.abs() >= dy.abs() {
            sess.click_ground(pos.0 + step(dx), pos.1);
        } else {
            sess.click_ground(pos.0, pos.1 + step(dy));
        }
        // The walk STARTS only if the path check passed: a refused walk
        // emits no own-gob LINSTEP at all (load-independent signal). A
        // started hop streams LINSTEP frames on the 5 Hz cadence while
        // the arrival MOVE op updates the position - under parallel-test
        // CPU load the arrival can lag several seconds behind the
        // nominal 2.3s, hence the generous windows.
        let started = sess.pump_until(
            |s| {
                s.gobs
                    .get(&player)
                    .map(|g| g.linsteps.len() > steps_before)
                    .unwrap_or(false)
            },
            12,
        );
        if !started {
            return false; // path refused (water): the caller retries elsewhere
        }
        let arrived = sess.pump_until(
            |s| {
                let p = s.gob_pos(player).unwrap_or(pos);
                if p != pos {
                    return true;
                }
                (target.0 - p.0).abs().max((target.1 - p.1).abs()) <= stop
            },
            20,
        );
        if !arrived {
            return false; // stalled mid-hop (severe load)
        }
    }
    false
}

/// Contract (docs/mechanics/crafting-and-building.md "World gathering",
/// session 60, over the real wire): a boulder pick spawns a stone item
/// drop next to it, the BOULDER_STONES-th pick retracts the boulder, a
/// tree pick spawns a branch drop and the tree survives, and a clicked
/// drop lands in the inventory as its gfx/invobjs stack. The spawn area
/// on seed 42 is open grass ringed by terrain that blocks some walks,
/// so both phases walk to the first REACHABLE candidate (the approach
/// the real client performs) and act from inside VIEW_RADIUS.
///
/// Session 64: the session-56 loss half of this #[ignore] is FIXED
/// server-side (lost OBJDATA spawn frames ride the OBJACK-driven
/// retransmission sweep - see lost_static_spawn_wave_is_retransmitted).
/// The scenario stays out of the default gate for a DIFFERENT,
/// documented reason: it is a 40-90s multi-hop walking scenario whose
/// approach timeouts are wall-clock-bound, so two concurrent boots on
/// the 2-core sandbox slow the 5 Hz LINSTEP cadence enough to starve
/// the walk legs (observed once in the full parallel suite). Run it
/// explicitly on a quiet machine: `cargo test --test wire -- --ignored`.
#[test]
#[ignore = "long walking scenario: run explicitly (cargo test -- --ignored), see doc"]
fn world_gathering_picks_yield_drops_exhaust_and_land_in_inventory() {
    let _slot = common::acquire_test_slot();
    // Arrange
    let server = ServerGuard::boot("gathering");
    let (mut sess, player) = enter_world(&server, "gatherer");
    // The candidate scans below sort by distance from the PLAYER; under
    // parallel-test load the player's spawn MOVE op may land after the
    // first statics batch - wait for it or the sort falls back to (0,0)
    // and the candidates point behind the water.
    assert!(
        sess.pump_until(|s| s.gob_pos(player).is_some(), 15),
        "player spawn MOVE never landed\n{}",
        server.log_tail(2000)
    );
    let starter_branch = stack_count(&sess, "gfx/invobjs/branch").expect("starter branch");
    let starter_stone = stack_count(&sess, "gfx/invobjs/stone").expect("starter stone");

    // Act 1: BOULDER_STONES picks on a reachable boulder near spawn
    // (the drop spawns next to the object with +-33 subtiles of jitter
    // and streams only inside VIEW_RADIUS - the real client walks to
    // the object before acting, the test walks to the first candidate
    // the terrain allows; some seed-42 boulders sit behind water).
    let collect_boulders = |s: &Session| -> Vec<(i32, (i32, i32))> {
        let mut out: Vec<(i32, (i32, i32))> = s
            .gobs
            .iter()
            .filter(|(gid, g)| !g.removed && s_res_prefix(s, **gid, BOULDER_PREFIX))
            .map(|(gid, _)| *gid)
            .filter_map(|gid| s.gob_pos(gid).map(|p| (gid, p)))
            .collect();
        let pp = s.gob_pos(player).unwrap_or((0, 0));
        out.sort_by_key(|(_, p)| (p.0 - pp.0).abs().max((p.1 - pp.1).abs()));
        out
    };
    let boulder_candidates = collect_boulders(&sess);
    let streamed = sess.pump_until(|s| !collect_boulders(s).is_empty(), 15);
    let boulder_candidates = if boulder_candidates.is_empty() && streamed {
        collect_boulders(&sess)
    } else {
        boulder_candidates
    };
    assert!(
        !boulder_candidates.is_empty(),
        "no boulder streamed in view at spawn\n{}",
        server.log_tail(2000)
    );
    let mut boulder_pos = None;
    for (b, bpos) in boulder_candidates.iter().take(8) {
        if approach_opt(&mut sess, player, *bpos, 200) {
            boulder_pos = Some((*b, *bpos));
            break;
        }
    }
    let (boulder, bpos) = boulder_pos.expect("no reachable boulder among the candidates");
    let mut stones_seen: Vec<i32> = Vec::new();
    for pick in 1..=BOULDER_STONES {
        assert!(
            !sess.gobs[&boulder].removed,
            "boulder retracted before pick {pick}"
        );
        sess.click_gob(boulder, bpos);
        let got = sess.pump_until(
            |s| {
                s.gobs.iter().any(|(gid, g)| {
                    !stones_seen.contains(gid)
                        && !g.removed
                        && s.gob_res_name(*gid) == Some(STONE_WORLD)
                })
            },
            8,
        );
        assert!(
            got,
            "pick {pick}: no stone drop spawned (seen={stones_seen:?})\n{}",
            server.log_tail(2000)
        );
        let drop = sess
            .gobs
            .iter()
            .find(|(gid, g)| {
                !stones_seen.contains(gid) && !g.removed && s_res_is(&sess, **gid, STONE_WORLD)
            })
            .map(|(gid, _)| *gid)
            .expect("fresh stone drop");
        stones_seen.push(drop);
    }
    assert_eq!(
        stones_seen.len(),
        BOULDER_STONES as usize,
        "one stone drop per pick"
    );
    let depleted = sess.pump_until(|s| s.gobs[&boulder].removed, 8);
    assert!(
        depleted,
        "depleted boulder must be retracted\n{}",
        server.log_tail(2000)
    );

    // Act 2: a clicked stone drop lands in the inventory.
    let first_drop = stones_seen[0];
    let dpos = sess.gob_pos(first_drop).expect("drop pos");
    sess.click_gob(first_drop, dpos);
    let picked = sess.pump_until(
        |s| stack_count(s, "gfx/invobjs/stone") == Some(starter_stone + 1),
        8,
    );
    assert!(
        picked,
        "stone drop never landed in the inventory ({} -> ?)\n{}",
        starter_stone,
        server.log_tail(2000)
    );

    // Act 3: pick a tree; the tree must SURVIVE. The spawn area on
    // seed 42 is open grass - the nearest trees sit in the grid (0, -1)
    // forest, ~1200 subtiles north. The pick act itself has no reach
    // check, so: click the nearest streamed tree NOW, walk to it (one
    // axis per hop), and the branch drop streams into view on approach.
    let collect_trees = |s: &Session| -> Vec<(i32, (i32, i32))> {
        let mut out: Vec<(i32, (i32, i32))> = s
            .gobs
            .iter()
            .filter(|(gid, g)| !g.removed && s_res_prefix(s, **gid, TREE_PREFIX))
            .map(|(gid, _)| *gid)
            .filter_map(|gid| s.gob_pos(gid).map(|p| (gid, p)))
            .collect();
        let pp = s.gob_pos(player).unwrap_or((0, 0));
        out.sort_by_key(|(_, p)| (p.0 - pp.0).abs() + (p.1 - pp.1).abs());
        out
    };
    let mut tree_candidates = collect_trees(&sess);
    for _ in 0..NORTH_HOPS {
        if !tree_candidates.is_empty() {
            break;
        }
        let pos = sess.gob_pos(player).expect("player pos");
        sess.click_ground(pos.0, pos.1 - HOP);
        sess.pump_until(|s| !collect_trees(s).is_empty(), 2);
        tree_candidates = collect_trees(&sess);
    }
    assert!(
        !tree_candidates.is_empty(),
        "no tree streamed among the loaded grids"
    );
    let mut tree = None;
    'trees: for (candidate, tpos) in tree_candidates.iter().take(6) {
        // Walk FIRST, then exactly ONE pick: every click on a tree burns
        // one of its TREE_HARVESTS, so the retry policy may never re-click
        // the same tree. From the 250-subtile stop radius the drop
        // (spawned +-33 subtiles next to the tree) streams immediately.
        if !approach_opt(&mut sess, player, *tpos, 250) {
            continue;
        }
        sess.click_gob(*candidate, *tpos);
        let got = sess.pump_until(
            |s| {
                s.gobs
                    .iter()
                    .any(|(gid, g)| !g.removed && s.gob_res_name(*gid) == Some(BRANCH_WORLD))
            },
            10,
        );
        if got {
            tree = Some(*candidate);
            break 'trees;
        }
    }
    let tree = tree.expect("no reachable tree yielded a branch drop");
    assert!(
        !sess.gobs[&tree].removed,
        "the tree must survive a pick (stump only after TREE_HARVESTS)"
    );

    // Act 4: the branch drop lands in the inventory.
    let branch_drop = sess
        .gobs
        .iter()
        .find(|(gid, g)| !g.removed && s_res_is(&sess, **gid, BRANCH_WORLD))
        .map(|(gid, _)| *gid)
        .expect("branch drop gob");
    let bpos = sess.gob_pos(branch_drop).expect("branch drop pos");
    sess.click_gob(branch_drop, bpos);
    let branch_picked = sess.pump_until(
        |s| stack_count(s, "gfx/invobjs/branch") == Some(starter_branch + 1),
        8,
    );
    assert!(
        branch_picked,
        "branch drop never landed in the inventory ({} -> ?)\n{}",
        starter_branch,
        server.log_tail(2000)
    );
}

/// Resource-name lookup outside a Session closure borrow (the gobs
/// iteration above borrows sess.gobs, so the name check goes through a
/// free function).
fn s_res_is(sess: &Session, gid: i32, name: &str) -> bool {
    sess.gob_res_name(gid) == Some(name)
}

/// Prefix variant of s_res_is (tree/boulder candidate scans).
fn s_res_prefix(sess: &Session, gid: i32, prefix: &str) -> bool {
    sess.gob_res_name(gid)
        .map(|n| n.starts_with(prefix))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Food Trough lift / place-back / fodder transfer (session 62)
// ---------------------------------------------------------------------------

/// The site search and sink choreography of the python buildbot
/// (test_feeding.py build_trough): arm the pagina, try up to four tiles
/// east of the player, commit the ghost, sink the branch demand from the
/// starter stack, return the cursor remainder. Completion is asserted by
/// the caller through a fodder delivery ("Fodder added to the trough."),
/// the only completion signal a 1-stage build exposes on the wire.
fn build_trough(sess: &mut Session, dx: i32, tag: &str) -> (i32, (i32, i32)) {
    let ppos = sess
        .gob_pos(sess.player_gob.expect("player gob"))
        .expect("player pos");
    let ptile = (ppos.0.div_euclid(11), ppos.1.div_euclid(11));
    for dy in 0..4i32 {
        let cand = (ptile.0 + dx, ptile.1 + dy);
        let mc = (cand.0 * 11 + 5, cand.1 * 11 + 5);
        sess.menu_act("trough");
        let armed = sess.pump_until(|s| s.last_wdgmsg("place").is_some(), 10);
        assert!(
            armed,
            "{tag}: no mapview place uimsg after arming the trough pagina\nchat={:?}",
            sess.chat_lines
        );
        sess.send_place(mc.0, mc.1, 1, 0);
        sess.pump_until(|s| s.gob_with_res_at(TROUGH_RES, mc).is_some(), 6);
        if let Some(plan) = sess.gob_with_res_at(TROUGH_RES, mc) {
            // Sink the branch demand (x4): the take removes the WHOLE
            // starter stack, the plan credits 4, the remainder rides the
            // cursor back via the inv drop (the session-51 contract).
            take_to_cursor(sess, "gfx/invobjs/branch", tag);
            sess.map_itemact(mc.0, mc.1, plan);
            return_cursor(sess, tag);
            return (plan, mc);
        }
        // Site refused (terrain/occupancy): the search moves on.
    }
    panic!(
        "{tag}: no candidate site accepted a trough plan\nchat={:?}",
        sess.chat_lines
    );
}

/// Load `units` fodder units one itemact at a time from the named seed
/// stack (one unit per click, the trough_itemact contract). `chat_base`
/// is the "Fodder added" count to expect before the first click.
fn load_fodder(
    sess: &mut Session,
    mc: (i32, i32),
    trough: i32,
    seed_res: &str,
    units: usize,
    chat_base: usize,
    tag: &str,
) {
    for i in 0..units {
        take_to_cursor(sess, seed_res, tag);
        sess.map_itemact(mc.0, mc.1, trough);
        let want = chat_base + i + 1;
        let ok = sess.pump_until(|s| s.chat_count("Fodder added to the trough.") == want, 8);
        assert!(
            ok,
            "{tag}: fodder click {i} never acknowledged (want {want})\nchat={:?}",
            sess.chat_lines
        );
        // Settle the post-itemact refresh before re-resolving wids.
        std::thread::sleep(Duration::from_millis(300));
    }
    // Return whatever is left on the cursor (a partially drained seed
    // stack) so later takes are not refused.
    return_cursor(sess, tag);
}

/// Click the trough, choose the "Lift" petal, and wait for the gob
/// retraction + the carry system line.
fn lift_trough(sess: &mut Session, gob: i32, pos: (i32, i32), units: usize, tag: &str) {
    sess.click_gob(gob, pos);
    let menu = sess.pump_until(
        |s| {
            s.sm_menus
                .values()
                .any(|a| a.iter().any(|v| v.as_str() == Some("Lift")))
        },
        8,
    );
    assert!(
        menu,
        "{tag}: the trough menu never offered Lift\nmenus={:?}\nchat={:?}",
        sess.sm_menus, sess.chat_lines
    );
    let wid = *sess
        .sm_menus
        .iter()
        .find(|(_, a)| a.iter().any(|v| v.as_str() == Some("Lift")))
        .map(|(w, _)| w)
        .expect("sm wid asserted above");
    sess.flower_choice(wid, 0);
    let lift_line = format!("You lift the trough ({units} fodder units).");
    let lifted = sess.pump_until(
        |s| s.gobs.get(&gob).map(|g| g.removed).unwrap_or(false) && s.chat_count(&lift_line) >= 1,
        8,
    );
    assert!(
        lifted,
        "{tag}: lift never confirmed (gob retracted + system line)\nchat={:?}",
        sess.chat_lines
    );
}

/// Contract (docs/mechanics/livestock/animals-and-husbandry.md
/// "Feeding: troughs and grazing", session 62, over the real wire):
/// the completed trough opens the one-petal "Lift" menu; the lift
/// retracts the gob and starts the carry with the store's units; a map
/// click places the carried trough back down with the same store;
/// clicking a placed trough while carrying transfers fodder "like a
/// liquid" (the moved units carry the source's average, the system
/// lines name every outcome).
#[test]
fn trough_lift_place_back_and_fodder_transfer_contract() {
    let _slot = common::acquire_test_slot();
    // Arrange
    let server = ServerGuard::boot("troughlift");
    let (mut sess, _player) = enter_world(&server, "trougher");

    // Act 1: build trough 1 east of the player and verify completion
    // through a fodder delivery (a 1-stage build keeps sdt at 0, so the
    // delivery is the visible completion signal).
    let (t1, mc1) = build_trough(&mut sess, 1, "trough1");
    load_fodder(
        &mut sess,
        mc1,
        t1,
        "gfx/invobjs/seed-wheat",
        5,
        0,
        "trough1",
    );

    // Act 2: lift trough 1 - the gob retracts, the carry starts.
    let t1_pos = sess.gob_pos(t1).expect("trough 1 pos");
    lift_trough(&mut sess, t1, t1_pos, 5, "trough1");

    // Act 3: place the carried trough back down at a fresh tile.
    let ppos = sess
        .gob_pos(sess.player_gob.expect("player gob"))
        .expect("player pos");
    let ptile = (ppos.0.div_euclid(11), ppos.1.div_euclid(11));
    let site = (ptile.0 + 1, ptile.1 + 2);
    let mc_back = (site.0 * 11 + 5, site.1 * 11 + 5);
    sess.send_place(mc_back.0, mc_back.1, 1, 0);
    let placed = sess.pump_until(
        |s| {
            s.gob_with_res_at(TROUGH_RES, mc_back).is_some()
                && s.chat_count("You place the trough (5 fodder units).") >= 1
        },
        8,
    );
    assert!(
        placed,
        "the carried trough was never placed back\nchat={:?}",
        sess.chat_lines
    );
    let t1b = sess
        .gob_with_res_at(TROUGH_RES, mc_back)
        .expect("placed trough gob");

    // Act 4: build + load + lift trough 2 (2 carrot units carried).
    let (t2, mc2) = build_trough(&mut sess, 2, "trough2");
    load_fodder(
        &mut sess,
        mc2,
        t2,
        "gfx/invobjs/seed-carrot",
        2,
        5,
        "trough2",
    );
    let t2_pos = sess.gob_pos(t2).expect("trough 2 pos");
    lift_trough(&mut sess, t2, t2_pos, 2, "trough2");

    // Act 5: click the placed trough while carrying - the liquid
    // transfer moves the carried 2 units into trough 1.
    let t1b_pos = sess.gob_pos(t1b).expect("placed trough pos");
    sess.click_gob(t1b, t1b_pos);
    let transferred = sess.pump_until(|s| s.chat_count("Transferred 2 fodder units.") >= 1, 8);
    assert!(
        transferred,
        "no transfer system line\nchat={:?}",
        sess.chat_lines
    );
}

// ---------------------------------------------------------------------------
// Session 72: the craft-flow wire contract
// ---------------------------------------------------------------------------

/// One make-window cycle: arm the craft pagina (act("craft", id)),
/// wait for the make widget, press Craft once.
fn craft_once(sess: &mut Session, recipe_id: &str, tag: &str) {
    sess.menu_act_words(&["craft", recipe_id]);
    assert!(
        sess.pump_until(|s| s.widgets_by_name.contains_key("make"), 8),
        "{tag}: no make widget for {recipe_id}\nchat={:?}",
        sess.chat_lines
    );
    sess.press_make(0);
}

/// Contract: the make widget consumes the real inventory stacks, the
/// tool-gated recipe refuses with the exact system line BEFORE the saw
/// exists, and the crafted PRIMARY output carries the recipe display
/// name - the label every station input gate matches on (session 71's
/// live finding, now pinned on the wire). The saw/bucket pair walks
/// the whole shape from the starter kit alone.
#[test]
fn craft_flow_carries_the_recipe_label_and_enforces_the_tool_gate() {
    let _slot = common::acquire_test_slot();
    let server = ServerGuard::boot("craftflow");
    let (mut sess, _player) = enter_world(&server, "craftsman");

    let branch_count = |s: &Session| stack_count(s, "gfx/invobjs/branch");
    let stone_count = |s: &Session| stack_count(s, "gfx/invobjs/stone");
    let starter_branch = branch_count(&sess).expect("starter branch");
    let starter_stone = stone_count(&sess).expect("starter stone");

    // Act 1: the bucket pagina WITHOUT the saw - the tool gate refuses
    // with the exact line and nothing is consumed or produced.
    craft_once(&mut sess, "bucket", "toolgate");
    let refused = sess.pump_until(
        |s| s.chat_lines.iter().any(|l| l.contains("You need the Saw")),
        8,
    );
    assert!(
        refused,
        "no tool-gate refusal line\nchat={:?}",
        sess.chat_lines
    );
    assert!(
        sess.item_by_res("gfx/invobjs/buckete").is_none(),
        "the refused craft must not produce a bucket"
    );
    assert_eq!(
        branch_count(&sess),
        Some(starter_branch),
        "the refused craft must not consume ingredients"
    );

    // Act 2: the saw craft - the primary output carries the recipe
    // display name "Saw" (the station-gate label contract) and the
    // inputs (branch x2 + stone x1) leave the inventory.
    craft_once(&mut sess, "saw", "saw");
    let saw_ready = |s: &Session| stack_count(s, "gfx/invobjs/saw") == Some(1);
    assert!(
        sess.pump_until(saw_ready, 8),
        "the saw never appeared\nchat={:?}",
        sess.chat_lines
    );
    assert_eq!(
        sess.item_label("gfx/invobjs/saw"),
        Some("Saw".to_owned()),
        "the crafted primary output must carry the recipe display name"
    );
    let saw_ready2 = |s: &Session| {
        branch_count(s) == Some(starter_branch - 2) && stone_count(s) == Some(starter_stone - 1)
    };
    assert!(
        sess.pump_until(saw_ready2, 8),
        "the saw inputs were not consumed (branch {:?}, stone {:?})",
        branch_count(&sess),
        stone_count(&sess)
    );

    // Act 3: with the saw in the inventory the bucket craft passes the
    // tool gate and yields the labeled "Bucket" from branch x3.
    craft_once(&mut sess, "bucket", "bucket");
    let bucket_ready = |s: &Session| stack_count(s, "gfx/invobjs/buckete") == Some(1);
    assert!(
        sess.pump_until(bucket_ready, 8),
        "the bucket never appeared\nchat={:?}",
        sess.chat_lines
    );
    assert_eq!(
        sess.item_label("gfx/invobjs/buckete"),
        Some("Bucket".to_owned()),
        "the bucket output must carry its recipe display name"
    );
    let branch_spent = |s: &Session| branch_count(s) == Some(starter_branch - 5);
    assert!(
        sess.pump_until(branch_spent, 8),
        "the bucket inputs were not consumed (branch {:?})",
        branch_count(&sess)
    );
}
