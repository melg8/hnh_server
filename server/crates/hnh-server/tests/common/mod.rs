//! Black-box wire harness for the hnh-server integration tests.
//!
//! The tests in `tests/wire.rs` speak the REAL protocol over the REAL
//! transport against the compiled binary: TLS auth (rustls client,
//! webpki trust path), UDP MSG_SESS handshake, the reliable RMSG
//! stream, raw MAPDATA/OBJDATA datagrams, and the OBJDATA op stream.
//! Nothing here links against server internals - every byte matches
//! what the legacy client puts on the wire.
//!
//! Contract sources: docs/mechanics/network/*, scripts/test_client.py,
//! scripts/probe_walk.py (the python probes this module mirrors).

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hnh_proto::{
    AUTH_CMD_PASSWD, AUTH_CMD_USR, MSG_ACK, MSG_MAPDATA, MSG_MAPREQ, MSG_OBJACK, MSG_OBJDATA,
    MSG_REL, MSG_SESS, OD_END, OD_LINBEG, OD_LINSTEP, OD_MOVE, PVER, RMSG_CATTR, RMSG_DSTWDG,
    RMSG_NEWWDG, RMSG_PAGINAE, RMSG_RESID, RMSG_WDGMSG,
};

// ---------------------------------------------------------------------------
// Server fixture (RAII)
// ---------------------------------------------------------------------------

/// A running hnh-server child process on ephemeral ports. Kills the
/// process on Drop so a failing test cannot leak a server
/// (test-fixture-raii).
/// Legacy client retransmit backoff (Session.java RWorker: 80/200/620/
/// 2000 ms, then every 2 s).
const REL_BACKOFF_MS: [u64; 4] = [80, 200, 620, 2000];

/// One reliable datagram awaiting its cumulative MSG_ACK.
struct PendingRel {
    datagram: Vec<u8>,
    /// Highest submessage sequence this datagram carries (the ACK is
    /// cumulative, so covering `last_seq` covers the whole datagram).
    last_seq: u16,
    next_at: Instant,
    attempt: usize,
}

pub struct ServerGuard {
    child: Option<Child>,
    pub auth_port: u16,
    pub game_port: u16,
    pub res_port: u16,
    /// DER of the certificate the server was started with; the test TLS
    /// client trusts exactly this cert.
    pub cert_der: Vec<u8>,
    workdir: PathBuf,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.workdir);
    }
}

/// Reserve an ephemeral TCP port by binding and immediately releasing
/// it. The race window is acceptable for a test fixture: a lost race
/// makes the server fail loudly at boot, not misbehave.
fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

impl ServerGuard {
    /// Boot the real binary with a per-test save file and a per-test
    /// self-signed TLS pair (rcgen - the same library the server's dev
    /// cert path uses). CARGO_BIN_EXE_* is provided by cargo.
    pub fn boot(tag: &str) -> Self {
        let auth_port = free_port();
        let game_port = free_port();
        let res_port = free_port();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let workdir = std::env::temp_dir().join(format!("hnh_it_{tag}_{nanos}"));
        std::fs::create_dir_all(&workdir).expect("create workdir");

        let ck =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("gen test cert");
        let cert_path = workdir.join("authsrv.crt.pem");
        let key_path = workdir.join("authsrv.key.pem");
        std::fs::write(&cert_path, ck.cert.pem()).expect("write cert pem");
        std::fs::write(&key_path, ck.key_pair.serialize_pem()).expect("write key pem");

        let log = std::fs::File::create(workdir.join("server.log")).expect("create server log");
        let child = Command::new(env!("CARGO_BIN_EXE_hnh-server"))
            .arg("--seed")
            .arg("42")
            .arg("--game-port")
            .arg(game_port.to_string())
            .arg("--auth-port")
            .arg(auth_port.to_string())
            .arg("--res-port")
            .arg(res_port.to_string())
            .arg("--cert")
            .arg(&cert_path)
            .arg("--key")
            .arg(&key_path)
            .env("HNH_SAVE_FILE", workdir.join("save.json"))
            .env("RUST_LOG", "hnh_server=info")
            .stdout(Stdio::from(log.try_clone().expect("clone log")))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn hnh-server binary");

        let guard = Self {
            child: Some(child),
            auth_port,
            game_port,
            res_port,
            cert_der: ck.cert.der().to_vec(),
            workdir,
        };
        guard.wait_ready();
        guard
    }

    /// Block until the TLS auth listener accepts (the server's own
    /// startup self-check gates on the same condition).
    fn wait_ready(&self) {
        let addr = ("127.0.0.1", self.auth_port)
            .to_socket_addrs()
            .expect("auth addr")
            .next()
            .expect("auth addr");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if TcpStream::connect(addr).is_ok() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "hnh-server did not open the auth port within 30s"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Diagnostics on failure: the captured server log.
    pub fn log_tail(&self, max: usize) -> String {
        let path = self.workdir.join("server.log");
        std::fs::read_to_string(&path)
            .map(|s| {
                let start = s.len().saturating_sub(max);
                s[start..].to_string()
            })
            .unwrap_or_else(|e| format!("<no server log: {e}>"))
    }
}

// ---------------------------------------------------------------------------
// TLS auth client
// ---------------------------------------------------------------------------

/// Obtain a session cookie through the REAL TLS auth channel, trusting
/// exactly the certificate the server was booted with (no dangerous()
/// verifier - the standard webpki trust path must hold).
pub fn auth_cookie(server: &ServerGuard, username: &str) -> Vec<u8> {
    // The test process has no main() to install the ring provider; do it
    // once here (idempotent, same choice the server binary makes).
    static PROVIDER: std::sync::Once = std::sync::Once::new();
    PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(
            server.cert_der.clone(),
        ))
        .expect("trust test root cert");
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name =
        rustls::pki_types::ServerName::try_from("localhost".to_string()).expect("server name");
    let conn = rustls::ClientConnection::new(std::sync::Arc::new(config), name).expect("tls conn");
    let tcp = TcpStream::connect(("127.0.0.1", server.auth_port)).expect("tcp to auth");
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    tls.sock
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");

    // Dev auth policy: SHA-256 of any password.
    let digest = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"x");
        h.finalize().to_vec()
    };

    write_frame(&mut tls, AUTH_CMD_USR, username.as_bytes());
    let (ty, _) = read_frame(&mut tls);
    assert_eq!(ty, 0, "CMD_USR rejected");
    write_frame(&mut tls, AUTH_CMD_PASSWD, &digest);
    let (ty, body) = read_frame(&mut tls);
    assert_eq!(ty, 0, "CMD_PASSWD rejected");
    assert!(!body.is_empty(), "empty cookie");
    body
}

fn write_frame(tls: &mut impl Write, ty: u8, payload: &[u8]) {
    let head = [ty, payload.len() as u8];
    tls.write_all(&head)
        .and_then(|_| tls.write_all(payload))
        .expect("write auth frame");
}

fn read_frame(tls: &mut impl Read) -> (u8, Vec<u8>) {
    let mut head = [0u8; 2];
    tls.read_exact(&mut head).expect("auth frame header");
    let mut body = vec![0u8; head[1] as usize];
    tls.read_exact(&mut body).expect("auth frame body");
    (head[0], body)
}

// ---------------------------------------------------------------------------
// Wire encoding helpers (legacy byte order: little endian)
// ---------------------------------------------------------------------------

fn le16(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

fn le32(v: i32) -> [u8; 4] {
    v.to_le_bytes()
}

fn nul_str(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

fn read_u16(blob: &[u8], off: &mut usize) -> u16 {
    let v = u16::from_le_bytes([blob[*off], blob[*off + 1]]);
    *off += 2;
    v
}

fn read_i32(blob: &[u8], off: &mut usize) -> i32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&blob[*off..*off + 4]);
    *off += 4;
    i32::from_le_bytes(b)
}

fn skip_nul_str(blob: &[u8], off: &mut usize) {
    match blob[*off..].iter().position(|&b| b == 0) {
        Some(p) => *off += p + 1,
        None => *off = blob.len(),
    }
}

// ---------------------------------------------------------------------------
// OBJDATA op stream
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Linbeg {
    pub sx: i32,
    pub sy: i32,
    pub tx: i32,
    pub ty: i32,
    pub c: i32,
}

#[derive(Debug, Default, Clone)]
pub struct GobOps {
    pub moves: Vec<(i32, i32)>,
    pub linbeg: Option<Linbeg>,
    /// Every LINBEG ever seen for this gob (interruption diagnosis).
    pub linbegs: Vec<Linbeg>,
    pub linsteps: Vec<i32>,
    pub removed: bool,
    /// Last OD_RES wire resource id (sprite state changes ride OD_RES).
    pub res: Option<u16>,
    /// Last OD_RES sprite dynamic data (the build-stage / station-lit
    /// byte for terobjs).
    pub sdt: Option<Vec<u8>>,
}

/// One decoded OBJDATA gob block.
pub struct GobBlock {
    pub gid: i32,
    pub frame: i32,
    pub removed: bool,
    pub ops: GobOps,
}

/// One typed-list element as the server encodes it in NEWWDG args and
/// WDGMSG uimsgs (hnh_proto ListArg mirror: tag byte + payload).
#[derive(Debug, Clone, PartialEq)]
pub enum ArgVal {
    Int(i32),
    Str(String),
    Coord(i32, i32),
    Color(u8, u8, u8, u8),
}

impl ArgVal {
    pub fn as_int(&self) -> Option<i32> {
        match self {
            ArgVal::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ArgVal::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_coord(&self) -> Option<(i32, i32)> {
        match self {
            ArgVal::Coord(x, y) => Some((*x, *y)),
            _ => None,
        }
    }
}

/// Parse a typed list (`tag byte + payload` elements closed by LIST_END)
/// starting at `*off`; on return `*off` sits past the LIST_END byte.
/// Unknown tags stop the parse (the stream is append-only by contract).
fn parse_args(blob: &[u8], off: &mut usize) -> Vec<ArgVal> {
    let mut out = Vec::new();
    while *off < blob.len() {
        let tag = blob[*off];
        *off += 1;
        match tag {
            0 => return out, // LIST_END
            1 => out.push(ArgVal::Int(read_i32(blob, off))),
            2 => {
                let start = *off;
                while *off < blob.len() && blob[*off] != 0 {
                    *off += 1;
                }
                let s = String::from_utf8_lossy(&blob[start..*off]).into_owned();
                *off += 1; // NUL
                out.push(ArgVal::Str(s));
            }
            3 => {
                let x = read_i32(blob, off);
                let y = read_i32(blob, off);
                out.push(ArgVal::Coord(x, y));
            }
            4 => {
                if *off + 4 <= blob.len() {
                    out.push(ArgVal::Color(
                        blob[*off],
                        blob[*off + 1],
                        blob[*off + 2],
                        blob[*off + 3],
                    ));
                }
                *off += 4;
            }
            _ => return out, // unknown tag: stop
        }
    }
    out
}

/// Decode one OBJDATA datagram body into per-gob op blocks. The op table
/// mirrors the server encoder (game/stream.rs encode_gob_block) and the
/// legacy client's PUtils; an unknown op stops the block (same policy as
/// scripts/probe_walk.py - the stream is append-only by contract).
pub fn decode_objdata(blob: &[u8]) -> Vec<GobBlock> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < blob.len() {
        let fl = blob[off];
        off += 1;
        let gid = read_i32(blob, &mut off);
        let frame = read_i32(blob, &mut off);
        let mut ops = GobOps::default();
        let removed = fl & 1 != 0;
        // A flag-1 block carries no ops section (bots.rs parse_objdata:
        // the removal is the whole block).
        while !removed && off < blob.len() {
            let t = blob[off];
            off += 1;
            match t {
                OD_END => break,
                OD_MOVE => {
                    let x = read_i32(blob, &mut off);
                    let y = read_i32(blob, &mut off);
                    ops.moves.push((x, y));
                }
                0 => {
                    // OD_REM: the server emits this op with no payload
                    // followed by OD_END (stream_retract). Falling into
                    // the unknown-op break here would leave the OD_END
                    // byte unconsumed and desync the next block parse.
                }
                OD_LINBEG => {
                    let sx = read_i32(blob, &mut off);
                    let sy = read_i32(blob, &mut off);
                    let tx = read_i32(blob, &mut off);
                    let ty = read_i32(blob, &mut off);
                    let c = read_i32(blob, &mut off);
                    let lb = Linbeg { sx, sy, tx, ty, c };
                    ops.linbeg = Some(lb);
                    ops.linbegs.push(lb);
                }
                OD_LINSTEP => ops.linsteps.push(read_i32(blob, &mut off)),
                2 => {
                    // OD_RES: u16 resid (+ inline name at the high bit).
                    let resid = read_u16(blob, &mut off);
                    if resid & 0x8000 != 0 {
                        let n = blob[off] as usize;
                        off += 1;
                        ops.sdt = Some(blob[off..off + n].to_vec());
                        off += n;
                    }
                    ops.res = Some(resid & 0x7FFF);
                }
                5 => {
                    // OD_SPEECH: coord + NUL string.
                    off += 8;
                    skip_nul_str(blob, &mut off);
                }
                6 | 9 => {
                    // OD_LAYERS: u16 base + u16 layer list to 65535;
                    // OD_AVATAR: the same layer list without the base
                    // (bots.rs parse_objdata layout).
                    if t == 6 {
                        off += 2; // base (kept for terminator alignment)
                    }
                    loop {
                        if read_u16(blob, &mut off) == 65535 {
                            break;
                        }
                    }
                }
                7 => off += 8,  // OD_DRAWOFF: coord
                8 => off += 11, // OD_LUMIN: coord + u16 + u8
                10 => {
                    // OD_FOLLOW: i32 oid; when not -1, u8 + coord.
                    let oid = read_i32(blob, &mut off);
                    if oid != -1 {
                        off += 1 + 8;
                    }
                }
                11 => {
                    // OD_HOMING: i32 oid; when not -1, coord + u16.
                    let oid = read_i32(blob, &mut off);
                    if oid != -1 {
                        off += 8 + 2;
                    }
                }
                12 => {
                    // OD_OVERLAY: i32 oid + u16 resid; raw 65535 means
                    // overlay REMOVAL and carries no sdt (bots.rs).
                    off += 4;
                    let raw = read_u16(blob, &mut off);
                    if raw == 65535 {
                        continue;
                    }
                    if raw & 0x8000 != 0 {
                        let n = blob[off] as usize;
                        off += 1 + n;
                    }
                }
                14 => off += 1, // OD_HEALTH: u8 indicator
                15 => {
                    // OD_BUDDY: NUL string + u8 + u8.
                    skip_nul_str(blob, &mut off);
                    off += 2;
                }
                _ => break, // unknown op: stop the block (probe policy)
            }
        }
        out.push(GobBlock {
            gid,
            frame,
            removed,
            ops,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Session driver
// ---------------------------------------------------------------------------

/// The documented character attributes the client's CharWnd constructor
/// dereferences at `chr` widget creation; a missing name NPEs the real
/// client, so the set must be complete BEFORE the `chr` NEWWDG.
pub const REQUIRED_CATTR: &[&str] = &[
    "str",
    "agil",
    "intel",
    "cons",
    "perc",
    "csm",
    "dxt",
    "psy",
    "expmod",
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
    "life",
    "night",
    "civil",
    "nature",
    "martial",
    "change",
];

/// A black-box client session over the real UDP path, mirroring the
/// legacy client's receive logic (in-order reliable stream + cumulative
/// ACK, raw MAPDATA/OBJDATA datagrams, batched MSG_OBJACK).
pub struct Session {
    sock: UdpSocket,
    server: std::net::SocketAddr,
    tseq: u16,
    rseq: u16,
    held: HashMap<u16, (u8, Vec<u8>)>,
    /// Sent-but-unacked reliable datagrams. The legacy client
    /// (Session.java RWorker) retransmits these on a backoff table
    /// until the server's cumulative MSG_ACK covers them - localhost
    /// UDP loses datagrams under parallel-test CPU load (kernel
    /// receive-buffer overflow on busy sockets), and a lost WDGMSG
    /// click that is never resent looks exactly like a server bug.
    /// Mirroring the client's retransmit duty is what fixed the
    /// movement wire test (session 56).
    pending_rel: Vec<PendingRel>,
    /// widget id -> type name
    pub widgets: HashMap<u16, String>,
    /// type name -> widget id
    pub widgets_by_name: HashMap<String, u16>,
    pub cattr_names: HashSet<String>,
    /// The cattr set as of the moment the `chr` widget was created.
    pub cattr_at_chr: Option<HashSet<String>>,
    pub paginae_atk: HashSet<String>,
    pub player_gob: Option<i32>,
    pub mapdata_datagrams: u32,
    pub objdata_datagrams: u32,
    /// Per-gob decoded state accumulated across all OBJDATA datagrams.
    pub gobs: HashMap<i32, GobOps>,
    objacks: HashMap<i32, i32>,
    last_ack: Instant,
    /// Last MAPREQ re-send sweep (raw MAPDATA is lossy UDP: the legacy
    /// MapView re-requests grids whose tiles never arrived).
    last_mapreq: Instant,
    /// RMSG_RESID announcements: wire resource id -> resource name.
    pub res_names: HashMap<u16, String>,
    /// Server->client WDGMSG uimsgs, in arrival order (wid, name, args).
    pub wdgmsgs: Vec<(u16, String, Vec<ArgVal>)>,
    /// Live "item" widgets: wid -> NEWWDG args (res, ql, drag, [coord,]
    /// label, count). Rebuilt on every inventory refresh.
    pub items: HashMap<u16, Vec<ArgVal>>,
}

impl Session {
    pub fn connect(server: &ServerGuard, username: &str) -> Self {
        let cookie = auth_cookie(server, username);
        let sock = UdpSocket::bind("127.0.0.1:0").expect("bind client udp");
        sock.set_read_timeout(Some(Duration::from_millis(200)))
            .expect("read timeout");
        let server_addr = ("127.0.0.1", server.game_port)
            .to_socket_addrs()
            .expect("game addr")
            .next()
            .expect("game addr");

        let sess = Self {
            sock,
            server: server_addr,
            tseq: 0,
            rseq: 0,
            held: HashMap::new(),
            pending_rel: Vec::new(),
            widgets: HashMap::new(),
            widgets_by_name: HashMap::new(),
            cattr_names: HashSet::new(),
            cattr_at_chr: None,
            paginae_atk: HashSet::new(),
            player_gob: None,
            mapdata_datagrams: 0,
            objdata_datagrams: 0,
            gobs: HashMap::new(),
            objacks: HashMap::new(),
            last_ack: Instant::now(),
            last_mapreq: Instant::now(),
            res_names: HashMap::new(),
            wdgmsgs: Vec::new(),
            items: HashMap::new(),
        };

        // MSG_SESS: flavour "Haven", PVER, username, cookie. The legacy
        // client retransmits until accepted (Session.java RWorker).
        let mut msg = vec![MSG_SESS];
        msg.extend_from_slice(&le16(1));
        msg.extend_from_slice(&nul_str("Haven"));
        msg.extend_from_slice(&le16(PVER));
        msg.extend_from_slice(&nul_str(username));
        msg.extend_from_slice(&cookie);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            assert!(Instant::now() < deadline, "session not accepted within 8s");
            sess.sock.send_to(&msg, sess.server).expect("send sess");
            let mut buf = [0u8; 65536];
            match sess.sock.recv_from(&mut buf) {
                Ok((n, _)) if n >= 2 && buf[0] == MSG_SESS => {
                    assert_eq!(buf[1], 0, "session rejected with error {}", buf[1]);
                    break;
                }
                Ok(_) => continue,
                Err(_) => continue, // timeout -> retransmit
            }
        }
        sess
    }

    /// Send one reliable datagram bundling `subs` RMSG submessages:
    /// every submessage but the last carries the 0x80 continuation flag
    /// with an explicit u16 length; the last is self-terminating.
    pub fn send_rel_subs(&mut self, subs: &[Vec<u8>]) {
        assert!(!subs.is_empty(), "a MSG_REL datagram needs submessages");
        let mut out = vec![MSG_REL];
        out.extend_from_slice(&le16(self.tseq));
        for (i, p) in subs.iter().enumerate() {
            if i < subs.len() - 1 {
                out.push(p[0] | 0x80);
                out.extend_from_slice(&le16((p.len() - 1) as u16));
                out.extend_from_slice(&p[1..]);
            } else {
                out.extend_from_slice(p);
            }
        }
        self.tseq = self.tseq.wrapping_add(subs.len() as u16);
        self.sock.send_to(&out, self.server).expect("send rel");
        // Track for retransmission until the cumulative ACK covers it.
        self.pending_rel.push(PendingRel {
            last_seq: self.tseq.wrapping_sub(1),
            datagram: out,
            next_at: Instant::now() + Duration::from_millis(REL_BACKOFF_MS[0]),
            attempt: 0,
        });
    }

    /// Retransmit every reliable datagram whose backoff window elapsed
    /// (the legacy client's retransmit duty; dedup is the server's
    /// RelReceiver job - a resent datagram carries the same seq).
    fn pump_pending_rel(&mut self) {
        let now = Instant::now();
        let mut due: Vec<Vec<u8>> = Vec::new();
        for p in self.pending_rel.iter_mut() {
            if p.next_at <= now {
                due.push(p.datagram.clone());
                p.attempt = (p.attempt + 1).min(REL_BACKOFF_MS.len() - 1);
                p.next_at = now + Duration::from_millis(REL_BACKOFF_MS[p.attempt]);
            }
        }
        for dgram in due {
            self.sock.send_to(&dgram, self.server).expect("resend rel");
        }
    }

    /// Diagnostics: how many sent reliable datagrams are still unacked.
    pub fn debug_pending_rel(&self) -> usize {
        self.pending_rel.len()
    }

    /// WDGMSG to a widget; `args` is the pre-encoded arg blob.
    pub fn send_wdgmsg(&mut self, wid: u16, name: &str, args: &[u8]) {
        let mut sub = vec![RMSG_WDGMSG];
        sub.extend_from_slice(&le16(wid));
        sub.extend_from_slice(&nul_str(name));
        sub.extend_from_slice(args);
        self.send_rel_subs(&[sub]);
    }

    /// Raw MAPREQ datagram for one grid.
    pub fn send_mapreq(&mut self, gx: i32, gy: i32) {
        let mut msg = vec![MSG_MAPREQ];
        msg.extend_from_slice(&le32(gx));
        msg.extend_from_slice(&le32(gy));
        self.sock.send_to(&msg, self.server).expect("send mapreq");
    }

    /// Mapview ground click at absolute subtile coords.
    pub fn click_ground(&mut self, mcx: i32, mcy: i32) {
        let wid = self.widgets_by_name["mapview"];
        let mut args = Vec::new();
        args.push(3);
        args.extend_from_slice(&le32(0)); // c0: screen coord (unused)
        args.extend_from_slice(&le32(0));
        args.push(3);
        args.extend_from_slice(&le32(mcx));
        args.extend_from_slice(&le32(mcy));
        args.push(1);
        args.extend_from_slice(&le32(1)); // button 1
        args.push(1);
        args.extend_from_slice(&le32(0)); // modflags
        args.push(0);
        self.send_wdgmsg(wid, "click", &args);
    }

    /// The legacy client's `play` wdgmsg on the charlist widget.
    pub fn send_play(&mut self, username: &str) {
        let wid = self.widgets_by_name["charlist"];
        let mut args = vec![2u8]; // arg tag: string
        args.extend_from_slice(&nul_str(username));
        args.push(0); // end of args
        self.send_wdgmsg(wid, "play", &args);
    }

    // ------------------------------------------------------------------
    // Building-flow helpers (wire.rs build contract; mirrors the wire
    // choreography of server/scripts/test_build.py run_buildbot).
    // ------------------------------------------------------------------

    /// Menugrid `act(<word>)` on the scm widget (build pagina arming).
    pub fn menu_act(&mut self, word: &str) {
        let wid = self.widgets_by_name["scm"];
        let mut args = vec![2u8]; // arg tag: string
        args.extend_from_slice(&nul_str(word));
        args.push(0);
        self.send_wdgmsg(wid, "act", &args);
    }

    /// MapView `place(coord, button, modflags)`: the ghost commit.
    pub fn send_place(&mut self, mx: i32, my: i32, button: i32, modflags: i32) {
        let wid = self.widgets_by_name["mapview"];
        let mut args = Vec::new();
        args.push(3); // arg tag: coord
        args.extend_from_slice(&le32(mx));
        args.extend_from_slice(&le32(my));
        args.push(1);
        args.extend_from_slice(&le32(button));
        args.push(1);
        args.extend_from_slice(&le32(modflags));
        args.push(0);
        self.send_wdgmsg(wid, "place", &args);
    }

    /// The wid of the inventory item widget whose resource resolves to
    /// `name` (the drag cursor's widget carries the same resource while
    /// a stack is held; inventory refreshes rebuild the set).
    pub fn item_by_res(&self, name: &str) -> Option<u16> {
        self.items
            .iter()
            .filter(|(_, args)| {
                args.first()
                    .and_then(|a| a.as_int())
                    .and_then(|wire| self.res_names.get(&(wire as u16)))
                    .map(|n| n == name)
                    .unwrap_or(false)
            })
            .map(|(wid, _)| *wid)
            .min()
        // Deterministic pick: the lowest wid (widgets allocate upward).
    }

    /// Inventory item `take(coord)`: move the stack onto the drag cursor.
    pub fn inv_take(&mut self, wid: u16) {
        let mut args = Vec::new();
        args.push(3); // arg tag: coord
        args.extend_from_slice(&le32(0));
        args.extend_from_slice(&le32(0));
        args.push(0);
        self.send_wdgmsg(wid, "take", &args);
    }

    /// MapView `itemact(cc, mc, modflags[, gobid, gobrc])`: click the map
    /// with the held stack; `gob` targets the plan/station gob.
    pub fn map_itemact(&mut self, mx: i32, my: i32, gob: i32) {
        let wid = self.widgets_by_name["mapview"];
        let mut args = Vec::new();
        args.push(3);
        args.extend_from_slice(&le32(0)); // cc: screen coord (unused)
        args.extend_from_slice(&le32(0));
        args.push(3);
        args.extend_from_slice(&le32(mx));
        args.extend_from_slice(&le32(my));
        args.push(1);
        args.extend_from_slice(&le32(0)); // modflags
        args.push(1);
        args.extend_from_slice(&le32(gob));
        args.push(3);
        args.extend_from_slice(&le32(mx));
        args.extend_from_slice(&le32(my));
        args.push(0);
        self.send_wdgmsg(wid, "itemact", &args);
    }

    /// Inventory `drop`: release the held stack back into the inventory
    /// grid (the legacy drag-release path; the inv_drop handler ignores
    /// the slot coordinate). The window ships with the client type "inv"
    /// (open_inventory: new_wdg "inv" with the 4x8 grid coord).
    pub fn inv_drop(&mut self) {
        let wid = self.widgets_by_name["inv"];
        self.send_wdgmsg(wid, "drop", &[0]);
    }

    /// The last server->client uimsg named `name` (e.g. the mapview
    /// `place` ghost-drive uimsg).
    pub fn last_wdgmsg(&self, name: &str) -> Option<&Vec<ArgVal>> {
        self.wdgmsgs
            .iter()
            .rev()
            .find(|(_, n, _)| n == name)
            .map(|(_, _, args)| args)
    }

    /// The position (last OD_MOVE) of `gob`, if any block carried one.
    pub fn gob_pos(&self, gob: i32) -> Option<(i32, i32)> {
        self.gobs.get(&gob).and_then(|g| g.moves.last().copied())
    }

    /// Drain datagrams until `cond` holds or the timeout expires.
    /// Returns true when the condition was observed.
    pub fn pump_until(&mut self, mut cond: impl FnMut(&Self) -> bool, secs: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if cond(self) {
                return true;
            }
            if Instant::now() >= deadline {
                return cond(self);
            }
            if self.last_ack.elapsed() > Duration::from_millis(200) && !self.objacks.is_empty() {
                // Client SWorker mirror: one batched MSG_OBJACK datagram.
                let mut msg = vec![MSG_OBJACK];
                for (gid, frame) in &self.objacks {
                    msg.extend_from_slice(&le32(*gid));
                    msg.extend_from_slice(&le32(*frame));
                }
                self.sock.send_to(&msg, self.server).expect("send objack");
                self.last_ack = Instant::now();
            }
            if self.last_mapreq.elapsed() > Duration::from_millis(1000) {
                // Client MapView mirror: re-request the 3x3 neighborhood
                // while grids are unfulfilled - raw MAPDATA rides lossy
                // UDP with no reliability layer.
                for gy in -1..=1 {
                    for gx in -1..=1 {
                        self.send_mapreq(gx, gy);
                    }
                }
                self.last_mapreq = Instant::now();
            }
            // Legacy RWorker mirror: retransmit unacked reliable
            // datagrams whose backoff window elapsed (a lost WDGMSG
            // click that is never resent stalls the server forever).
            self.pump_pending_rel();
            let mut buf = [0u8; 65536];
            match self.sock.recv_from(&mut buf) {
                Ok((n, _)) => self.on_datagram(&buf[..n]),
                Err(_) => continue, // read timeout: loop re-checks cond
            }
        }
    }

    fn on_datagram(&mut self, data: &[u8]) {
        match data[0] {
            MSG_MAPDATA => self.mapdata_datagrams += 1,
            MSG_OBJDATA => {
                self.objdata_datagrams += 1;
                for block in decode_objdata(&data[1..]) {
                    let entry = self.gobs.entry(block.gid).or_default();
                    entry.moves.extend(block.ops.moves);
                    if block.ops.linbeg.is_some() {
                        entry.linbeg = block.ops.linbeg;
                        entry.linbegs.extend(block.ops.linbegs);
                    }
                    entry.linsteps.extend(block.ops.linsteps);
                    if block.ops.res.is_some() {
                        entry.res = block.ops.res;
                    }
                    if block.ops.sdt.is_some() {
                        entry.sdt = block.ops.sdt;
                    }
                    if block.removed {
                        entry.removed = true;
                    }
                    let f = self
                        .objacks
                        .get(&block.gid)
                        .copied()
                        .unwrap_or(0)
                        .max(block.frame);
                    self.objacks.insert(block.gid, f);
                }
            }
            MSG_REL => self.on_rel_stream(&data[1..]),
            MSG_ACK if data.len() >= 3 => {
                // Cumulative server ACK: drop every sent datagram it
                // covers (same wrapping-window compare the server's
                // RelSender::on_ack uses).
                let ack = u16::from_le_bytes([data[1], data[2]]);
                self.pending_rel
                    .retain(|p| ack.wrapping_sub(p.last_seq) >= 0x8000);
            }
            _ => {}
        }
    }

    /// Reliable stream: in-order seq per SUBMESSAGE (the legacy layout
    /// packs several submessages per datagram, each advancing the seq),
    /// cumulative ACK after each in-order delivery, hold-back buffer for
    /// gaps (Session.java RWorker mirror, scripts/test_client.py).
    fn on_rel_stream(&mut self, body: &[u8]) {
        if body.len() < 2 {
            return;
        }
        let mut seq = u16::from_le_bytes([body[0], body[1]]);
        let mut off = 2usize;
        while off < body.len() {
            let t = body[off];
            off += 1;
            let sub: Vec<u8> = if t & 0x80 != 0 {
                assert!(off + 2 <= body.len(), "truncated continuation header");
                let ln = u16::from_le_bytes([body[off], body[off + 1]]) as usize;
                off += 2;
                assert!(off + ln <= body.len(), "truncated continuation body");
                let s = body[off..off + ln].to_vec();
                off += ln;
                s
            } else {
                let s = body[off..].to_vec();
                off = body.len();
                s
            };
            if seq == self.rseq {
                self.on_rel_sub(t & 0x7F, &sub);
                self.rseq = self.rseq.wrapping_add(1);
                while let Some((t2, b2)) = self.held.remove(&self.rseq) {
                    self.on_rel_sub(t2, &b2);
                    self.rseq = self.rseq.wrapping_add(1);
                }
                let mut ack = vec![MSG_ACK];
                ack.extend_from_slice(&le16(self.rseq.wrapping_sub(1)));
                self.sock.send_to(&ack, self.server).expect("send ack");
            } else if seq.wrapping_sub(self.rseq) < 0x8000 {
                self.held.insert(seq, (t & 0x7F, sub));
            }
            seq = seq.wrapping_add(1);
        }
    }

    /// Diagnostics: how many out-of-order submessages sit in hold-back.
    pub fn debug_held_len(&self) -> usize {
        self.held.len()
    }

    fn on_rel_sub(&mut self, t: u8, body: &[u8]) {
        match t {
            RMSG_NEWWDG => {
                if body.len() < 2 {
                    return;
                }
                let wid = u16::from_le_bytes([body[0], body[1]]);
                let mut off = 2usize;
                skip_nul_str(body, &mut off);
                if off > 2 {
                    let name = String::from_utf8_lossy(&body[2..off - 1]).into_owned();
                    self.widgets.insert(wid, name.clone());
                    self.widgets_by_name.insert(name.clone(), wid);
                    if name == "mapview" {
                        self.on_mapview_args(body, off);
                        // Real-client behavior: request the 3x3 grid
                        // neighborhood the moment mapview binds.
                        for gy in -1..=1 {
                            for gx in -1..=1 {
                                self.send_mapreq(gx, gy);
                            }
                        }
                    }
                    if name == "chr" {
                        self.cattr_at_chr = Some(self.cattr_names.clone());
                    }
                    if name == "slen" {
                        // Real-client behavior: SlenHud.binded() requests
                        // the character sheet the moment the HUD binds.
                        // The server must have queued the full cattr set
                        // by the time the `chr` widget is created.
                        self.send_wdgmsg(wid, "chr", &[0]);
                    }
                    if name == "item" {
                        // Item factory args (game/items.rs): [I(res), I(ql),
                        // I(drag), (C grab)|C(coord), S(label), I(count)]
                        // past the type-name coord + parent.
                        let mut o = off + 8 + 2;
                        let args = parse_args(body, &mut o);
                        self.items.insert(wid, args);
                    }
                }
            }
            RMSG_DSTWDG => {
                if body.len() >= 2 {
                    let wid = u16::from_le_bytes([body[0], body[1]]);
                    self.widgets.remove(&wid);
                    self.items.remove(&wid);
                    self.widgets_by_name.retain(|_, v| *v != wid);
                }
            }
            RMSG_WDGMSG => {
                if body.len() < 2 {
                    return;
                }
                let wid = u16::from_le_bytes([body[0], body[1]]);
                let mut off = 2usize;
                skip_nul_str(body, &mut off);
                if off > 2 {
                    let name = String::from_utf8_lossy(&body[2..off - 1]).into_owned();
                    let args = parse_args(body, &mut off);
                    self.wdgmsgs.push((wid, name, args));
                }
            }
            RMSG_RESID => {
                // u16 wire id + NUL name + u16 version.
                if body.len() < 4 {
                    return;
                }
                let wire = u16::from_le_bytes([body[0], body[1]]);
                let mut off = 2usize;
                skip_nul_str(body, &mut off);
                if off > 2 {
                    let name = String::from_utf8_lossy(&body[2..off - 1]).into_owned();
                    self.res_names.insert(wire, name);
                }
            }
            RMSG_CATTR => {
                // (name\0 i32 base i32 comp)*
                let mut off = 0usize;
                while off < body.len() {
                    let start = off;
                    skip_nul_str(body, &mut off);
                    if off <= start || off > body.len() {
                        break;
                    }
                    let name = String::from_utf8_lossy(&body[start..off - 1]).into_owned();
                    off += 8; // base + comp
                    self.cattr_names.insert(name);
                }
            }
            RMSG_PAGINAE => {
                // per entry: [act][name\0][u16 ver]
                let mut off = 0usize;
                while off < body.len() {
                    let act = body[off];
                    off += 1;
                    let start = off;
                    skip_nul_str(body, &mut off);
                    if off <= start || off > body.len() {
                        break;
                    }
                    let name = String::from_utf8_lossy(&body[start..off - 1]).into_owned();
                    off += 2; // ver
                    if act == 0x2B && name.starts_with("paginae/atk/") {
                        self.paginae_atk.insert(name);
                    }
                }
            }
            _ => {}
        }
    }

    /// mapview NEWWDG args: [I(0), C(spawn subtile), I(player gob id)]
    /// arrive as tag 1 (i32, collect) and tag 3 (2x i32, skip) entries;
    /// the last collected i32 is the player's own gob id
    /// (scripts/probe_walk.py mirror).
    fn on_mapview_args(&mut self, body: &[u8], mut off: usize) {
        // past type string NUL: coord c (8 bytes), parent (2 bytes)
        off += 8 + 2;
        let mut ints: Vec<i32> = Vec::new();
        while off < body.len() {
            let tag = body[off];
            off += 1;
            match tag {
                1 => {
                    if off + 4 > body.len() {
                        break;
                    }
                    ints.push(read_i32(body, &mut off));
                }
                3 => off += 8,
                _ => break,
            }
        }
        if let Some(&last) = ints.last() {
            self.player_gob = Some(last);
        }
    }
}
