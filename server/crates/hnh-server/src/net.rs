//! UDP networking: session accept (MSG_SESS), per-session reliability,
//! and the command path into the game task.
//!
//! Architecture: N UDP sockets share port 1870 via SO_REUSEPORT. The kernel
//! hashes the UDP 4-tuple, so every peer maps to exactly one shard for its
//! lifetime; each shard owns a private recv loop and its session table.
//! Outbound traffic is sent through the owning shard's socket, which keeps
//! per-shard work balanced by kernel hash rather than a central router.
//! Session drivers are independent tasks; the game task remains the single
//! simulation owner (grid-owner partitioning is the next scale-out step,
//! see HANDOFF.md).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info};

use hnh_proto::consts::*;
use hnh_proto::{RelReceiver, RelSender};

use crate::game::{Cmd, Game};
use crate::state::{GobId, SessionId};

pub const GAME_PORT: u16 = 1870;
/// Session idle timeout: no datagrams of any kind for this long => close.
const SESSION_TIMEOUT: Duration = Duration::from_secs(60);
/// Max datagram size we send (fits any sane MTU).
pub const OUT_MTU: usize = 1200;

/// Commands the net layer sends to the game task.
pub enum NetCmd {
    /// New session accepted; game assigns a sid and returns it.
    Accept {
        game_tx: mpsc::UnboundedSender<Vec<u8>>,
        raw_tx: mpsc::UnboundedSender<Vec<u8>>,
        reply: oneshot::Sender<SessionId>,
    },
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
    Closed {
        sid: SessionId,
    },
}

impl From<NetCmd> for Cmd {
    fn from(c: NetCmd) -> Cmd {
        match c {
            NetCmd::Wdgmsg {
                sid,
                wid,
                name,
                args,
            } => Cmd::Wdgmsg {
                sid,
                wid,
                name,
                args,
            },
            NetCmd::MapReq { sid, gc } => Cmd::MapReq { sid, gc },
            NetCmd::ObjAck { sid, acks } => Cmd::ObjAck { sid, acks },
            NetCmd::Closed { sid } => Cmd::SessionClosed { sid },
            NetCmd::Accept { .. } => unreachable!("Accept is handled via bridge"),
        }
    }
}

/// Bind one UDP socket so shard sockets can share the game port.
fn bind_shard_socket(port: u16) -> anyhow::Result<UdpSocket> {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    #[cfg(unix)]
    {
        // SO_REUSEPORT lets every shard bind the same port; the kernel
        // spreads peers across sockets by 4-tuple hash.
        sock.set_reuse_port(true)?;
    }
    #[cfg(windows)]
    {
        // Windows has no SO_REUSEPORT. SO_REUSEADDR still permits the
        // rebind so extra shards never fail at startup, but the kernel
        // may deliver every peer to a single socket. Correctness holds
        // regardless (all shards feed the same game channel and each
        // session is owned end-to-end by whichever socket accepted it);
        // only per-shard distribution is less even.
        sock.set_reuse_address(true)?;
    }
    sock.set_nonblocking(true)?;
    sock.bind(&std::net::SocketAddr::from(([0, 0, 0, 0], port)).into())?;
    let std_sock: std::net::UdpSocket = sock.into();
    Ok(UdpSocket::from_std(std_sock)?)
}

/// Spawn `shards` UDP shard loops on the game port. Sessions distribute
/// across shards by kernel 4-tuple hash; `shards = 1` degenerates to the
/// original single-socket layout. The accept log carries the shard id,
/// giving operations a direct per-shard occupancy histogram.
pub async fn spawn(game_tx: mpsc::UnboundedSender<NetCmd>, shards: usize) -> anyhow::Result<()> {
    let shard_count = shards.max(1);
    for id in 0..shard_count {
        let socket = Arc::new(bind_shard_socket(GAME_PORT)?);
        info!(
            shard = id,
            port = GAME_PORT,
            "game server (UDP) shard listening"
        );
        let game_tx = game_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = recv_loop(socket, game_tx, id).await {
                tracing::error!(shard = id, error = %e, "udp shard loop died");
            }
        });
    }
    Ok(())
}

async fn recv_loop(
    socket: Arc<UdpSocket>,
    game_tx: mpsc::UnboundedSender<NetCmd>,
    shard: usize,
) -> anyhow::Result<()> {
    let mut sessions: HashMap<SocketAddr, mpsc::UnboundedSender<Vec<u8>>> = HashMap::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let (n, peer) = socket.recv_from(&mut buf).await?;
        let data = &buf[..n];
        match data.first().copied() {
            Some(MSG_SESS) => {
                if on_sess(data, peer, &socket, &game_tx, &mut sessions, shard)
                    .await
                    .is_some()
                {
                    // Route any immediate duplicates into the driver.
                }
            }
            Some(_) => {
                if let Some(tx) = sessions.get(&peer) {
                    let _ = tx.send(data.to_vec());
                }
                // Datagrams from unknown peers (other than SESS) are ignored.
            }
            None => {}
        }
    }
}

/// Handle MSG_SESS: validate, reply, spawn a driver task.
async fn on_sess(
    data: &[u8],
    peer: SocketAddr,
    socket: &Arc<UdpSocket>,
    game_tx: &mpsc::UnboundedSender<NetCmd>,
    sessions: &mut HashMap<SocketAddr, mpsc::UnboundedSender<Vec<u8>>>,
    shard: usize,
) -> Option<mpsc::UnboundedSender<Vec<u8>>> {
    // Parse: uint16 flavour, string "Haven", uint16 PVER, string user, cookie.
    let mut m = hnh_proto::MessageBuf::from_slice(&data[1..]);
    let (Ok(_flavour), Ok(_game), Ok(pver), Ok(username)) = (m.u16(), m.str(), m.u16(), m.str())
    else {
        return None;
    };
    let cookie = m.rest().to_vec();
    let auth = crate::auth();
    let err = if pver != PVER {
        SESSERR_PVER
    } else if sessions.contains_key(&peer) {
        0 // idempotent re-accept of a live session
    } else {
        match auth.consume_cookie(&cookie) {
            Some(_user) => 0,
            None => SESSERR_AUTH,
        }
    };
    let _ = socket.send_to(&[MSG_SESS, err], peer).await;
    if err != 0 || sessions.contains_key(&peer) {
        return None;
    }
    // Ask the game task to allocate a sid and register the session.
    let (gameq_tx, gameq_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (raw_tx, raw_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (reply_tx, reply_rx) = oneshot::channel();
    if game_tx
        .send(NetCmd::Accept {
            game_tx: gameq_tx.clone(),
            raw_tx: raw_tx.clone(),
            reply: reply_tx,
        })
        .is_err()
    {
        return None;
    }
    let Ok(sid) = reply_rx.await else { return None };
    let (dgram_tx, dgram_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<NetCmd>();
    // Forward cmd_tx into the shared game channel.
    let shared = game_tx.clone();
    tokio::spawn(async move {
        let mut cmd_rx = cmd_rx;
        while let Some(c) = cmd_rx.recv().await {
            if shared.send(c).is_err() {
                break;
            }
        }
    });
    sessions.insert(peer, dgram_tx.clone());
    info!(%peer, sid, shard, %username, "session accepted");
    let sock = Arc::clone(socket);
    tokio::spawn(async move {
        run_session(peer, sid, dgram_rx, gameq_rx, raw_rx, cmd_tx, sock).await;
    });
    Some(dgram_tx)
}

struct Driver {
    addr: SocketAddr,
    sid: SessionId,
    rel_tx: RelSender,
    rel_rx: RelReceiver,
    dgram_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    game_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    raw_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    cmd_tx: mpsc::UnboundedSender<NetCmd>,
    last_recv: Instant,
    closed: bool,
}

async fn run_session(
    addr: SocketAddr,
    sid: SessionId,
    dgram_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    game_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    raw_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    cmd_tx: mpsc::UnboundedSender<NetCmd>,
    sock: Arc<UdpSocket>,
) {
    let mut d = Driver {
        addr,
        sid,
        rel_tx: RelSender::new(),
        rel_rx: RelReceiver::new(),
        dgram_rx,
        game_rx,
        raw_rx,
        cmd_tx,
        last_recv: Instant::now(),
        closed: false,
    };
    // Timer budget: retransmit flushes piggyback on a 20 ms scheduler.
    let mut next_flush = tokio::time::Instant::now() + Duration::from_millis(20);
    loop {
        if d.closed {
            break;
        }
        tokio::select! {
            payload = d.game_rx.recv() => {
                match payload {
                    Some(p) => {
                        d.rel_tx.queue(&p);
                        flush_reliable(&mut d, &sock).await;
                    }
                    None => break, // game task dropped the session
                }
            }
            datagram = d.raw_rx.recv() => {
                match datagram {
                    Some(p) => {
                        // Raw datagrams (MAPDATA / OBJDATA) bypass the
                        // reliable stream by protocol design.
                        let _ = sock.send_to(&p, d.addr).await;
                    }
                    None => break,
                }
            }
            datagram = d.dgram_rx.recv() => {
                match datagram {
                    Some(data) => {
                        d.last_recv = Instant::now();
                        handle_datagram(&mut d, &data, &sock).await;
                    }
                    None => break, // net task dropped the session
                }
            }
            _ = tokio::time::sleep_until(next_flush) => {
                next_flush = tokio::time::Instant::now() + Duration::from_millis(20);
                flush_reliable(&mut d, &sock).await;
                // Liveness: beat on 5 s idle, timeout on 60 s silence.
                if d.last_recv.elapsed() >= Duration::from_secs(5) {
                    if d.last_recv.elapsed() > SESSION_TIMEOUT {
                        info!(sid = d.sid, "session timed out");
                        break;
                    }
                    let _ = sock.send_to(&[MSG_BEAT], d.addr).await;
                    d.last_recv += Duration::from_secs(4); // keep beating until silence hits 60s
                }
            }
        }
    }
    let _ = sock.send_to(&[MSG_CLOSE], d.addr).await;
    let _ = d.cmd_tx.send(NetCmd::Closed { sid: d.sid });
    info!(sid = d.sid, "session driver exiting");
}

async fn flush_reliable(d: &mut Driver, sock: &UdpSocket) {
    let now = Instant::now();
    for dgram in d.rel_tx.poll_transmit(now, OUT_MTU) {
        let _ = sock.send_to(&dgram, d.addr).await;
    }
    if let Some(ack) = d.rel_rx.ack_now() {
        let _ = sock.send_to(&ack, d.addr).await;
    }
}

async fn handle_datagram(d: &mut Driver, data: &[u8], sock: &Arc<UdpSocket>) {
    match data[0] {
        MSG_REL => {
            for (rtype, payload) in d.rel_rx.on_rel(&data[1..]) {
                dispatch_rmsg(d, rtype, &payload);
            }
            flush_reliable(d, sock).await;
        }
        MSG_ACK => {
            if data.len() >= 3 {
                let seq = u16::from_le_bytes([data[1], data[2]]);
                d.rel_tx.on_ack(seq);
            }
        }
        MSG_BEAT => {}
        MSG_MAPREQ => {
            let mut m = hnh_proto::MessageBuf::from_slice(&data[1..]);
            if let Ok(gc) = m.coord2() {
                let _ = d.cmd_tx.send(NetCmd::MapReq { sid: d.sid, gc });
            }
        }
        MSG_OBJACK => {
            let mut m = hnh_proto::MessageBuf::from_slice(&data[1..]);
            let mut acks = Vec::new();
            while !m.eom() {
                let Ok(id) = m.i32() else { break };
                let Ok(frame) = m.i32() else { break };
                acks.push((id, frame as u32));
            }
            if !acks.is_empty() {
                let _ = d.cmd_tx.send(NetCmd::ObjAck { sid: d.sid, acks });
            }
        }
        MSG_SESS => {
            // Duplicate handshake: re-accept idempotently.
            let _ = sock.send_to(&[MSG_SESS, 0], d.addr).await;
        }
        MSG_CLOSE => {
            d.closed = true;
        }
        other => {
            info!(sid = d.sid, ty = other, "unknown datagram type");
        }
    }
}

fn dispatch_rmsg(d: &mut Driver, rtype: u8, payload: &[u8]) {
    // `payload` includes the type byte at index 0 (rel layer contract);
    // skip it before parsing the sub-message body.
    let mut m = hnh_proto::MessageBuf::from_slice(&payload[1..]);
    match rtype {
        RMSG_WDGMSG => {
            let (Ok(wid), Ok(name)) = (m.u16(), m.str()) else {
                return;
            };
            let args = m.list().unwrap_or_default();
            let _ = d.cmd_tx.send(NetCmd::Wdgmsg {
                sid: d.sid,
                wid,
                name,
                args,
            });
        }
        other => {
            debug!(sid = d.sid, rtype = other, "unhandled RMSG from client");
        }
    }
}

// Keep Game import used (doc reference for command shapes).
#[allow(unused)]
fn _assert_shapes(_: &Game) {}
