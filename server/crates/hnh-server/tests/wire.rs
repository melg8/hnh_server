//! Black-box wire contracts for the compiled hnh-server binary.
//!
//! These tests close the pyramid gap between the in-module unit tests
//! (white-box, in-process Game) and the manual python probes: they boot
//! the real binary on ephemeral ports and speak the real protocol.
//! Each test owns its own ServerGuard (RAII, no shared state).

mod common;

use std::net::ToSocketAddrs;

use common::{ServerGuard, Session, REQUIRED_CATTR};
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
    // datagram (fragments only add more).
    assert!(
        sess.mapdata_datagrams >= 9,
        "mapdata datagrams {} < 9",
        sess.mapdata_datagrams
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
    let linbeg = linbeg.expect("no own-gob LINBEG after ground click");
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
