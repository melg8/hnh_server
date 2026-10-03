//! In-process load-test bots.
//!
//! Bots drive the real UDP session path (loopback): handshake, widget
//! bootstrap, character select, then a behavior loop of walking around.
//! Sessions run as tokio tasks on a single thread per runtime worker, so
//! thousands of bots fit far below the process/thread ulimit. This gives an
//! end-to-end load and correctness signal identical to real clients (same
//! socket, same reliability layer, same widget protocol).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tracing::info;

use hnh_proto::consts::*;
use hnh_proto::{RelReceiver, RelSender};

pub async fn run(count: usize) {
    info!(count, "spawning load-test bots");
    let start = Instant::now();
    let mut handles = Vec::with_capacity(count);
    for i in 0..count {
        // Stagger logins 10 ms apart to mimic a realistic ramp.
        tokio::time::sleep(Duration::from_millis(10)).await;
        handles.push(tokio::spawn(bot_session(i)));
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
        elapsed_secs = start.elapsed().as_secs(),
        "bot cohort finished"
    );
}

/// Outcome of the bootstrap phase.
struct Boot {
    mapview: Option<u16>,
}

/// One bot session: async socket, fixed behavior loop.
async fn bot_session(idx: usize) -> bool {
    let Ok(sock) = UdpSocket::bind("127.0.0.1:0").await else {
        return false;
    };
    let server: SocketAddr = "127.0.0.1:1870".parse().expect("BUG: literal");
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

    // Each bot owns a home quadrant so the cohort spreads across distinct
    // grids (realistic MAPREQ streaming, per-grid population, and fights
    // with the local wildlife population). Request the 3x3 grids around it
    // like a real client (raw MAPREQ datagrams).
    let home_x = 550 + (rng.next_bounded(21) - 10) * 3;
    let home_y = 550 + (rng.next_bounded(21) - 10) * 3;
    for gx in -1..=1 {
        for gy in -1..=1 {
            let mut req = hnh_proto::MessageBuf::new();
            req.uint8(MSG_MAPREQ).int32(home_x + gx).int32(home_y + gy);
            let _ = sock.send_to(&req.finish(), server).await;
        }
    }

    // --- behavior loop: random walks for up to 10 minutes ---
    let behavior_end = Instant::now() + Duration::from_secs(600);
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
                        let _ = (ty, payload);
                    }
                }
                MSG_CLOSE => alive = false,
                _ => {}
            }
        }
        let now = Instant::now();
        if now >= next_action {
            next_action = now + Duration::from_millis(400 + rng.next_bounded(800) as u64);
            let jx = home_x * 11 + rng.next_bounded(600) - 300;
            let jy = home_y * 11 + rng.next_bounded(600) - 300;
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
            let _ = sock.send_to(&[MSG_BEAT], server).await;
        }
        if now >= next_flush {
            next_flush = now + Duration::from_millis(20);
            send_rel(&sock, &mut rel_tx, server).await;
        }
    }
    let _ = sock.send_to(&[MSG_CLOSE], server).await;
    alive
}

async fn bootstrap(
    sock: &UdpSocket,
    rel_tx: &mut RelSender,
    rel_rx: &mut RelReceiver,
    server: SocketAddr,
    name: &str,
) -> Boot {
    let deadline = Instant::now() + Duration::from_secs(15);
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
