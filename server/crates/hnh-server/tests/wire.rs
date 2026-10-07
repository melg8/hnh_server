//! Black-box wire contracts for the compiled hnh-server binary.
//!
//! These tests close the pyramid gap between the in-module unit tests
//! (white-box, in-process Game) and the manual python probes: they boot
//! the real binary on ephemeral ports and speak the real protocol.
//! Each test owns its own ServerGuard (RAII, no shared state).

mod common;

use std::net::ToSocketAddrs;

use common::{ArgVal, ServerGuard, Session, REQUIRED_CATTR};
use hnh_proto::{MSG_SESS, PVER, SESSERR_AUTH};

/// Contract: dev TLS auth -> cookie -> MSG_SESS -> full bootstrap
/// (charlist -> play -> mapview + 3x3 MAPREQ -> MAPDATA + OBJDATA) with
/// the complete REQUIRED_CATTR set present BEFORE the `chr` widget
/// creation, and the attack paginae shipped.
#[test]
fn world_entry_bootstrap_contract() {
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

/// Contract: a mapview ground click produces an own-gob OD_LINBEG toward
/// the clicked subtile followed by monotonic OD_LINSTEP progress frames
/// that approach the target without a teleport.
#[test]
fn movement_click_walks_with_linstep_progress() {
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
    // subtiles - the probe_walk.py coordinates.
    sess.click_ground(555 + 220, 555);

    // Assert 1: an own-gob LINBEG appears toward the clicked target.
    let own_linbeg = |s: &Session| {
        s.gobs
            .get(&player_gob)
            .and_then(|g| g.linbeg)
            .filter(|l| l.tx > l.sx) // eastward
    };
    let linbeg = if sess.pump_until(|s| own_linbeg(s).is_some(), 10) {
        own_linbeg(&sess)
    } else {
        None
    };
    assert!(
        linbeg.is_some(),
        "no own-gob LINBEG after ground click (unacked rel dgrams: {})\n{}",
        sess.debug_pending_rel(),
        server.log_tail(2000)
    );
    let linbeg = linbeg.expect("LINBEG asserted above");
    assert!(
        (linbeg.tx - linbeg.sx - 220).abs() <= 22,
        "LINBEG target not ~220 subtiles east: sx={} tx={}",
        linbeg.sx,
        linbeg.tx
    );
    assert!(
        (linbeg.ty - linbeg.sy).abs() <= 22,
        "LINBEG drifts on y: sy={} ty={}",
        linbeg.sy,
        linbeg.ty
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
