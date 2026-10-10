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

use std::collections::{HashMap, HashSet};
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
/// `HNH_SESSION_TIMEOUT_SECS` overrides it (integration tests shrink it;
/// operators on lossy client links may raise it).
const SESSION_TIMEOUT: Duration = Duration::from_secs(60);

/// Resolve the session timeout once per session boot. The 1 s floor
/// keeps a typo from ever disabling the timeout entirely.
fn session_timeout() -> Duration {
    let secs = std::env::var("HNH_SESSION_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(SESSION_TIMEOUT.as_secs());
    Duration::from_secs(secs.max(1))
}
/// Max datagram size we send (fits any sane MTU).
pub const OUT_MTU: usize = 1200;

/// Commands the net layer sends to the game task.
pub enum NetCmd {
    /// New session accepted; game assigns a sid and returns it.
    /// `username` is the authenticated account the session belongs to
    /// (character save keys are account-scoped).
    Accept {
        username: String,
        game_tx: mpsc::UnboundedSender<Vec<u8>>,
        raw_tx: mpsc::Sender<crate::state::BlockBytes>,
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
fn bind_shard_socket(bind: Option<std::net::IpAddr>, port: u16) -> anyhow::Result<UdpSocket> {
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
    // Multi-machine semantics: a clustered node binds its own
    // CLUSTER_SPEC address so every reply it sends CARRIES that address
    // as the source. The real Java client (Session.java RWorker) drops
    // datagrams whose source != the server address it dialed, so a
    // wildcard bind on machine B would source replies from the
    // machine's primary route address and a client entering through B
    // would silently discard ALL of B's traffic. Single-node keeps the
    // wildcard contract (local clients, the bot fleet).
    let ip = bind.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    sock.bind(&std::net::SocketAddr::from((ip, port)).into())?;
    let std_sock: std::net::UdpSocket = sock.into();
    Ok(UdpSocket::from_std(std_sock)?)
}

/// Spawn `shards` UDP shard loops on the game port. Sessions distribute
/// across shards by kernel 4-tuple hash; `shards = 1` degenerates to the
/// original single-socket layout. The accept log carries the shard id,
/// giving operations a direct per-shard occupancy histogram.
///
/// `bind` is the node's own CLUSTER_SPEC address in a cluster (see
/// `bind_shard_socket`), `None` for the single-node wildcard contract.
pub async fn spawn(
    game_tx: mpsc::UnboundedSender<NetCmd>,
    shards: usize,
    port: u16,
    bind: Option<std::net::IpAddr>,
) -> anyhow::Result<()> {
    let shard_count = shards.max(1);
    for id in 0..shard_count {
        let socket = Arc::new(bind_shard_socket(bind, port)?);
        info!(shard = id, port, ?bind, "game server (UDP) shard listening");
        let game_tx = game_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = recv_loop(socket, game_tx, id).await {
                tracing::error!(shard = id, error = %e, "udp shard loop died");
            }
        });
    }
    Ok(())
}

/// Collapse bursts of identical recv errors into one line per window.
/// Windows repeats WSAECONNRESET on every keepalive after a client
/// disappears (once per outbound datagram), which previously spammed the
/// log at the keepalive cadence; the deduper keeps one line per 30 s plus
/// a suppressed count.
#[derive(Default)]
struct ErrorDeduper {
    last: Option<(String, Instant)>,
    suppressed: u64,
}

const DEDUPE_WINDOW: Duration = Duration::from_secs(30);

impl ErrorDeduper {
    /// True when this error should be logged now.
    fn should_log(&mut self, msg: &str) -> bool {
        match &self.last {
            Some((m, t)) if *m == msg && t.elapsed() < DEDUPE_WINDOW => {
                self.suppressed += 1;
                false
            }
            _ => {
                self.last = Some((msg.to_owned(), Instant::now()));
                true
            }
        }
    }

    fn take_suppressed(&mut self) -> u64 {
        std::mem::take(&mut self.suppressed)
    }
}

async fn recv_loop(
    socket: Arc<UdpSocket>,
    game_tx: mpsc::UnboundedSender<NetCmd>,
    shard: usize,
) -> anyhow::Result<()> {
    // Shared per-shard session table. A std Mutex suffices: critical
    // sections are map lookups/inserts and never held across awaits.
    let sessions: Arc<std::sync::Mutex<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>> =
        Arc::new(std::sync::Mutex::new(HashMap::new()));
    // Peers with an accept in flight (game-task handshake not finished
    // yet). Duplicate MSG_SESS from these gets the idempotent 0 reply
    // instead of a cookie re-consume (single-use -> spurious AUTH error).
    let pending: Arc<std::sync::Mutex<HashSet<SocketAddr>>> =
        Arc::new(std::sync::Mutex::new(HashSet::new()));
    let mut throttle = AcceptThrottle::new();
    let mut buf = vec![0u8; 65536];
    let mut deduper = ErrorDeduper::default();
    loop {
        // Windows surfaces stale ICMP port-unreachable replies (e.g. from a
        // send to a client that just disconnected) as WSAECONNRESET on the
        // next recv_from. These are per-datagram noise, not shard failures;
        // log (rate-limited) and continue so one reset cannot kill every
        // session on the shard.
        let (n, peer) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                let msg = e.to_string();
                if deduper.should_log(&msg) {
                    let suppressed = deduper.take_suppressed();
                    if suppressed > 0 {
                        tracing::warn!(shard, suppressed, "udp recv errors suppressed");
                    }
                    tracing::warn!(error = %msg, shard, "udp recv error");
                }
                continue;
            }
        };
        let data = &buf[..n];
        match data.first().copied() {
            Some(MSG_SESS) => {
                // Inline part: parse + validate + throttle + reply. No
                // game-task round trip here — during a login storm that
                // await stalled the whole shard's recv loop (handshake
                // latency for every other session grew by the game
                // task's queueing delay).
                let already = {
                    let s = sessions.lock().unwrap();
                    s.contains_key(&peer) || pending.lock().unwrap().contains(&peer)
                };
                let mut m = hnh_proto::MessageBuf::from_slice(&data[1..]);
                let (Ok(_flavour), Ok(_game), Ok(pver), Ok(username)) =
                    (m.u16(), m.str(), m.u16(), m.str())
                else {
                    continue;
                };
                let cookie = m.rest().to_vec();
                let err = if pver != PVER {
                    SESSERR_PVER
                } else if already {
                    0 // idempotent re-accept of a live/in-flight session
                } else if !throttle.take() {
                    // Storm control: silently drop. The legacy client
                    // retransmits MSG_SESS every 2 s up to 10 times
                    // (Session.java RWorker), so it backs off naturally
                    // until the storm drains. No cookie consumed.
                    debug!(%peer, shard, "accept throttled (no tokens)");
                    continue;
                } else {
                    match crate::auth().consume_cookie(&cookie) {
                        Some(_user) => 0,
                        None => SESSERR_AUTH,
                    }
                };
                let _ = socket.send_to(&[MSG_SESS, err], peer).await;
                if err != 0 || already {
                    continue;
                }
                pending.lock().unwrap().insert(peer);
                // Game-task handshake + driver spawn run off the recv
                // loop path; the loop keeps draining datagrams meanwhile.
                tokio::spawn(finish_accept(
                    username,
                    peer,
                    Arc::clone(&socket),
                    game_tx.clone(),
                    Arc::clone(&sessions),
                    Arc::clone(&pending),
                    shard,
                ));
            }
            Some(_) => {
                let tx = sessions.lock().unwrap().get(&peer).cloned();
                if let Some(tx) = tx {
                    // Bounded queue: drop on full (a starved session loses
                    // old client datagrams instead of blocking the shard
                    // accept loop or growing without bound).
                    let _ = tx.try_send(data.to_vec());
                }
                // Datagrams from unknown peers (other than SESS) are ignored.
            }
            None => {}
        }
    }
}

/// Per-shard accept rate limiter (token bucket).
///
/// New-session accepts are the only unbounded work a remote peer can
/// trigger per datagram (sid allocation, MAPDATA bootstrap, gob spawns),
/// so a flood of MSG_SESS handshakes would otherwise queue unbounded
/// work on the game task. Refill: 50 accepts/s sustained, burst 100 —
/// a 1000-bot login storm drains over ~18 s instead of stampeding the
/// game task; real players logging in alongside never notice.
pub struct AcceptThrottle {
    tokens: f64,
    last: Instant,
}

/// Sustained new-session accept rate (tokens per second).
pub const ACCEPT_RATE: f64 = 50.0;
/// Maximum burst size (tokens).
pub const ACCEPT_BURST: f64 = 100.0;

impl AcceptThrottle {
    pub fn new() -> Self {
        Self {
            tokens: ACCEPT_BURST,
            last: Instant::now(),
        }
    }

    /// Try to take one token now.
    pub fn take(&mut self) -> bool {
        self.take_at(Instant::now())
    }

    /// Take with an explicit clock (tests inject synthetic time).
    pub fn take_at(&mut self, now: Instant) -> bool {
        let dt = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * ACCEPT_RATE).min(ACCEPT_BURST);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl Default for AcceptThrottle {
    fn default() -> Self {
        Self::new()
    }
}

/// Game-task half of the accept path (runs as its own task): allocate
/// the sid, wire the channels, spawn the session driver. The inline
/// part already validated PVER, throttled and consumed the cookie.
async fn finish_accept(
    username: String,
    peer: SocketAddr,
    socket: Arc<UdpSocket>,
    game_tx: mpsc::UnboundedSender<NetCmd>,
    sessions: Arc<std::sync::Mutex<HashMap<SocketAddr, mpsc::Sender<Vec<u8>>>>>,
    pending: Arc<std::sync::Mutex<HashSet<SocketAddr>>>,
    shard: usize,
) {
    // Always clear the in-flight marker, even on error paths.
    let _guard = PendingGuard(&pending, peer);
    // Ask the game task to allocate a sid and register the session.
    // Raw datagram fan-out is BOUNDED with drop-on-full semantics: MAPDATA
    // and OBJDATA are unreliable by protocol design, and an unbounded queue
    // behind a starved sender task OOM-killed the process at the 1000-
    // session scale (measured: 3.8 GB RSS before the kill).
    let (gameq_tx, gameq_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (raw_tx, raw_rx) = mpsc::channel::<crate::state::BlockBytes>(128);
    let (reply_tx, reply_rx) = oneshot::channel();
    if game_tx
        .send(NetCmd::Accept {
            username: username.clone(),
            game_tx: gameq_tx.clone(),
            raw_tx: raw_tx.clone(),
            reply: reply_tx,
        })
        .is_err()
    {
        return;
    }
    let Ok(sid) = reply_rx.await else {
        return;
    };
    // Inbound client datagrams are also bounded: a flooded client loses
    // old commands instead of growing the queue without bound.
    let (dgram_tx, dgram_rx) = mpsc::channel::<Vec<u8>>(1024);
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
    sessions.lock().unwrap().insert(peer, dgram_tx.clone());
    info!(%peer, sid, shard, %username, "session accepted");
    tokio::spawn(async move {
        run_session(peer, sid, dgram_rx, gameq_rx, raw_rx, cmd_tx, socket).await;
    });
}

/// Clears the pending-accept marker when the accept task ends.
struct PendingGuard<'a>(&'a Arc<std::sync::Mutex<HashSet<SocketAddr>>>, SocketAddr);

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap().remove(&self.1);
    }
}

struct Driver {
    addr: SocketAddr,
    sid: SessionId,
    rel_tx: RelSender,
    rel_rx: RelReceiver,
    dgram_rx: mpsc::Receiver<Vec<u8>>,
    game_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    raw_rx: mpsc::Receiver<crate::state::BlockBytes>,
    cmd_tx: mpsc::UnboundedSender<NetCmd>,
    last_recv: Instant,
    /// Throttle stamp for OUR outgoing beats - deliberately NOT a
    /// liveness input (session 89: the old code bumped `last_recv` by
    /// +4 s on every beat, so the silence clock oscillated 1..5 s
    /// forever, the 60 s timeout was unreachable, and a crashed
    /// client kept its session - and its character "online" - until
    /// a server restart, which NACKed every re-login).
    last_beat: Instant,
    closed: bool,
}

async fn run_session(
    addr: SocketAddr,
    sid: SessionId,
    dgram_rx: mpsc::Receiver<Vec<u8>>,
    game_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    raw_rx: mpsc::Receiver<crate::state::BlockBytes>,
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
        last_beat: Instant::now(),
        closed: false,
    };
    let timeout = session_timeout();
    // Timer budget: retransmit flushes piggyback on a 20 ms scheduler.
    let mut next_flush = tokio::time::Instant::now() + Duration::from_millis(20);
    loop {
        if d.closed {
            break;
        }
        // Biased polling fixes the poll priority: inbound datagrams
        // first (ACKs drive the reliable window), then the reliable
        // stream (game_rx), and raw datagrams last. Session 34: RESID
        // announcements ride the reliable stream while the OBJDATA
        // blocks that REFERENCE them ride raw; without the
        // game_rx-before-raw_rx bias, tokio's random branch choice
        // could emit the raw block first, leaving the client unable to
        // resolve the gob's resource (the probe caught gobs stuck with
        // an unresolved resid).
        tokio::select! {
            biased;
            datagram = d.dgram_rx.recv() => {
                match datagram {
                    Some(data) => {
                        d.last_recv = Instant::now();
                        handle_datagram(&mut d, &data, &sock).await;
                    }
                    None => break, // net task dropped the session
                }
            }
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
                        let _ = sock.send_to(p.as_slice(), d.addr).await;
                    }
                    None => break,
                }
            }
            _ = tokio::time::sleep_until(next_flush) => {
                next_flush = tokio::time::Instant::now() + Duration::from_millis(20);
                flush_reliable(&mut d, &sock).await;
                // Liveness: beat on 5 s idle, timeout on 60 s of TRUE
                // silence. `last_beat` only throttles the server-side
                // beats so the 20 ms scheduler cannot spam them; the
                // timeout stays keyed on the last REAL datagram, so a
                // peer that stops acking, beating and sending anything
                // at all is dropped exactly once the timeout passes.
                if d.last_recv.elapsed() >= Duration::from_secs(5) {
                    if d.last_recv.elapsed() > timeout {
                        info!(sid = d.sid, "session timed out");
                        break;
                    }
                    if d.last_beat.elapsed() >= Duration::from_secs(5) {
                        let _ = sock.send_to(&[MSG_BEAT], d.addr).await;
                        d.last_beat = Instant::now();
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduper_collapses_identical_bursts() {
        let mut d = ErrorDeduper::default();
        assert!(d.should_log("os error 10054"));
        assert!(!d.should_log("os error 10054"));
        assert!(!d.should_log("os error 10054"));
        assert_eq!(d.take_suppressed(), 2);
        // A different error message is always logged.
        assert!(d.should_log("os error 10049"));
        // take_suppressed resets the counter.
        assert_eq!(d.take_suppressed(), 0);
    }

    #[test]
    fn accept_throttle_burst_then_refill() {
        // Full burst is available immediately...
        let mut t = AcceptThrottle::new();
        let t0 = Instant::now();
        for i in 0..ACCEPT_BURST as u32 {
            assert!(t.take_at(t0), "token {i} of the burst must be available");
        }
        // ...then the bucket is empty: the storm is dropped. A 1 ms
        // pause refills 0.05 tokens — not enough for one accept.
        assert!(!t.take_at(t0 + Duration::from_millis(1)));

        // Sustained refill: after a full drain, 1 s of idle time refills
        // exactly ACCEPT_RATE tokens, then the bucket is empty again.
        let mut accepted = 0;
        for _ in 0..ACCEPT_RATE as u32 {
            if t.take_at(t0 + Duration::from_secs(1)) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, ACCEPT_RATE as u32, "1 s refills one rate window");
        assert!(!t.take_at(t0 + Duration::from_secs(1)), "drained again");

        // Idleness refills up to the burst cap, never beyond.
        let mut t2 = AcceptThrottle::new();
        let t1 = t2.last;
        let far = t1 + Duration::from_secs(10_000);
        for _ in 0..ACCEPT_BURST as u32 {
            assert!(t2.take_at(far));
        }
        assert!(!t2.take_at(far), "burst cap, not infinite credit");
    }
}
