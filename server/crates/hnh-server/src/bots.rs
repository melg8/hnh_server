//! In-process load-test bots.
//!
//! Bots drive the real UDP session path (loopback): handshake, widget
//! bootstrap, character select, then a behavior loop of walking around.
//! This gives an end-to-end load and correctness signal identical to real
//! clients (same socket, same reliability layer, same widget protocol).

use std::net::UdpSocket as StdUdp;
use std::time::{Duration, Instant};

use tracing::info;

use hnh_proto::consts::*;
use hnh_proto::{RelReceiver, RelSender};

pub async fn run(count: usize) {
    info!(count, "spawning load-test bots");
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    for i in 0..count {
        let tx = tx.clone();
        // Stagger logins 20 ms apart to mimic a realistic ramp.
        tokio::time::sleep(Duration::from_millis(20)).await;
        std::thread::spawn(move || {
            let ok = bot_session(i);
            let _ = tx.send(ok);
        });
    }
    drop(tx);
    tokio::task::spawn_blocking(move || {
        let mut ok = 0usize;
        let start = Instant::now();
        for v in rx {
            if v {
                ok += 1;
            }
        }
        info!(
            connected = ok,
            total = count,
            elapsed_secs = start.elapsed().as_secs(),
            "bot cohort finished"
        );
    });
}

/// Outcome of the bootstrap phase.
struct Boot {
    mapview: Option<u16>,
}

/// One bot session: blocking sockets, fixed behavior loop.
fn bot_session(idx: usize) -> bool {
    let sock = match StdUdp::bind("127.0.0.1:0") {
        Ok(s) => s,
        Err(_) => return false,
    };
    sock.set_read_timeout(Some(Duration::from_millis(30))).ok();
    let server: std::net::SocketAddr = "127.0.0.1:1870".parse().expect("BUG: literal");
    let mut rel_tx = RelSender::new();
    let mut rel_rx = RelReceiver::new();
    let mut rng = hnh_world::JavaRandom::new(idx as i64 ^ 0xB075);
    let name = format!("bot{idx:05}");
    let cookie = crate::auth().issue_cookie(&name);

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
    while start.elapsed() < Duration::from_secs(10) {
        let _ = sock.send_to(&sess, server);
        let mut buf = [0u8; 1500];
        if let Ok((_n, _)) = sock.recv_from(&mut buf) {
            if buf[0] == MSG_SESS {
                if buf.len() > 1 && buf[1] == 0 {
                    accepted = true;
                    break;
                }
                return false;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if !accepted {
        return false;
    }

    // --- bootstrap: collect widgets until mapview appears, then play ---
    let boot = bootstrap(&sock, &mut rel_tx, &mut rel_rx, server, &name);
    let Some(mapview) = boot.mapview else {
        return false;
    };

    // --- behavior loop: random walks for up to 10 minutes ---
    let behavior_end = Instant::now() + Duration::from_secs(600);
    let mut next_action = Instant::now() + Duration::from_millis(500);
    let mut next_beat = Instant::now() + Duration::from_secs(5);
    let mut alive = true;
    while alive && Instant::now() < behavior_end {
        let now = Instant::now();
        drain_inbound(&sock, &mut rel_rx, &mut alive);
        if now >= next_action {
            next_action = now + Duration::from_millis(400 + rng.next_bounded(800) as u64);
            let jx = 555 * 11 + rng.next_bounded(600) - 300;
            let jy = 555 * 11 + rng.next_bounded(600) - 300;
            let mut click = hnh_proto::MessageBuf::new();
            click
                .uint8(RMSG_WDGMSG)
                .uint16(mapview)
                .string("click")
                .lint(0)
                .lint(jx)
                .lint(jy)
                .lint(1)
                .lint(0)
                .lend();
            rel_tx.queue(&click.finish());
        }
        if now >= next_beat {
            next_beat = now + Duration::from_secs(5);
            let _ = sock.send_to(&[MSG_BEAT], server);
        }
        send_rel(&sock, &mut rel_tx, server);
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = sock.send_to(&[MSG_CLOSE], server);
    alive
}

fn bootstrap(
    sock: &StdUdp,
    rel_tx: &mut RelSender,
    rel_rx: &mut RelReceiver,
    server: std::net::SocketAddr,
    name: &str,
) -> Boot {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut charlist: Option<u16> = None;
    let mut mapview: Option<u16> = None;
    let mut played = false;
    while Instant::now() < deadline {
        let mut buf = [0u8; 65536];
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            if buf[0] == MSG_CLOSE {
                break;
            }
            if buf[0] == MSG_REL {
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
        }
        // Fire `play` as soon as we see the charlist.
        if let Some(wid) = charlist {
            if !played {
                played = true;
                let mut play = hnh_proto::MessageBuf::new();
                play.uint8(RMSG_WDGMSG)
                    .uint16(wid)
                    .string("play")
                    .lstr(name)
                    .lend();
                rel_tx.queue(&play.finish());
            }
        }
        send_rel(sock, rel_tx, server);
        if mapview.is_some() && played {
            return Boot { mapview };
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Boot { mapview }
}

fn drain_inbound(sock: &StdUdp, rel_rx: &mut RelReceiver, alive: &mut bool) {
    let mut buf = [0u8; 65536];
    while let Ok((n, _)) = sock.recv_from(&mut buf) {
        match buf[0] {
            MSG_REL => {
                let _ = rel_rx.on_rel(&buf[1..n]);
            }
            MSG_CLOSE => *alive = false,
            _ => {}
        }
    }
}

fn send_rel(sock: &StdUdp, rel: &mut RelSender, server: std::net::SocketAddr) {
    for d in rel.poll_transmit(Instant::now(), 1200) {
        let _ = sock.send_to(&d, server);
    }
}
