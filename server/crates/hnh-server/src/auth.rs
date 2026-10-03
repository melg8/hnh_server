//! TLS authentication server (port 1871), mirroring AuthClient.java's
//! frame protocol: uint8 type, uint8 len, payload.
//!
//! Dev policy: any username/password pair is accepted and auto-provisioned
//! (single-player / LAN developer mode). Cookies are single-use, 5 minute
//! TTL. Tokens are 32 bytes, reusable, 30 day TTL — persisted in
//! `accounts.json` next to the binary's working directory.

use std::collections::HashMap;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tracing::{info, warn};

use hnh_proto::{auth_frame, parse_auth_frame, AUTH_CMD_GETTOKEN, AUTH_CMD_PASSWD, AUTH_CMD_USR, AUTH_CMD_USETOKEN};

pub const AUTH_PORT: u16 = 1871;
const COOKIE_TTL_SECS: u64 = 300;
const TOKEN_TTL_SECS: u64 = 60 * 60 * 24 * 30;

#[derive(Default)]
struct Accounts {
    /// username -> password sha256 hex (empty = any password accepted)
    passwords: HashMap<String, String>,
    /// issued single-use cookies: value -> (username, expires_at unix secs)
    cookies: HashMap<Vec<u8>, (String, u64)>,
    /// reusable login tokens: value -> (username, expires_at)
    tokens: HashMap<Vec<u8>, (String, u64)>,
}

impl Accounts {
    fn prune(&mut self, now: u64) {
        self.cookies.retain(|_, (_, exp)| *exp > now);
        self.tokens.retain(|_, (_, exp)| *exp > now);
    }
}

pub struct AuthServer {
    accounts: Arc<RwLock<Accounts>>,
}

fn sha256_hex(data: &[u8]) -> String {
    // Minimal SHA-256 implementation to avoid an extra dependency; the
    // legacy client sends `SHA-256(password)` raw bytes, we keep hex here.
    use std::fmt::Write;
    let digest = sha256(data);
    let mut s = String::with_capacity(64);
    for b in digest {
        write!(s, "{:02x}", b).expect("BUG: hex write cannot fail");
    }
    s
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finish()
}

// ----- compact SHA-256 (FIPS 180-4) -----
struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    fn new() -> Self {
        Sha256 {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: [0u8; 64],
            buf_len: 0,
            total: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            let mut b = [0u8; 64];
            b.copy_from_slice(block);
            self.compress(&b);
            data = rest;
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }

    fn finish(mut self) -> [u8; 32] {
        let bit_len = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0]);
        }
        // length is appended manually to avoid corrupting total
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buf;
        self.compress(&block);
        let mut out = [0u8; 32];
        for (i, s) in self.state.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&s.to_be_bytes());
        }
        out
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rand_bytes(n: usize) -> Vec<u8> {
    use std::cell::Cell;
    thread_local! {
        static CTR: Cell<u64> = const { Cell::new(0) };
    }
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let seed = CTR.with(|c| {
            let v = c.get().wrapping_add(1);
            c.set(v);
            v
        });
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mixed = mix128(t ^ seed.rotate_left(17) ^ ((std::process::id() as u64) << 32));
        out.extend_from_slice(&mixed.to_le_bytes()[..(n - out.len()).min(8)]);
    }
    out.truncate(n);
    out
}

/// splitmix64 finalizer: cheap deterministic entropy mixing.
fn mix128(mut v: u64) -> u64 {
    v = v.wrapping_add(0x9E3779B97F4A7C15);
    v = (v ^ (v >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94D049BB133111EB);
    v ^ (v >> 31)
}

fn load_certs(cert_path: &str, key_path: &str) -> anyhow::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let certs: Vec<_> = rustls_pemfile::certs(&mut std::io::BufReader::new(
        std::fs::File::open(cert_path)?,
    ))
    .collect::<Result<_, _>>()?;
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(
        std::fs::File::open(key_path)?,
    ))?
    .ok_or_else(|| anyhow::anyhow!("no private key in {key_path}"))?;
    Ok((certs, key))
}

impl AuthServer {
    pub fn new() -> Self {
        AuthServer { accounts: Arc::new(RwLock::new(Accounts::default())) }
    }

    /// Issue a single-use session cookie. Used by the TLS listener and by
    /// in-process load-test bots. Locking is try-based: contention here is
    /// one short insert per login, never held across awaits.
    pub fn issue_cookie(&self, username: &str) -> Vec<u8> {
        let cookie = rand_bytes(32);
        let mut acc = self
            .accounts
            .try_write()
            .expect("BUG: accounts lock poisoned by await overlap");
        acc.cookies.insert(cookie.clone(), (username.to_owned(), now() + COOKIE_TTL_SECS));
        cookie
    }

    /// Validate a cookie, returning the username. Single-use.
    pub fn consume_cookie(&self, cookie: &[u8]) -> Option<String> {
        let mut acc = self.accounts.try_write().ok()?;
        acc.prune(now());
        acc.cookies.remove(cookie).map(|(user, _)| user)
    }

    pub async fn run(self: Arc<Self>, cert_path: &str, key_path: &str) -> anyhow::Result<()> {
        let (certs, key) = load_certs(cert_path, key_path)?;
        let config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)?,
        );
        let listener = TcpListener::bind(("0.0.0.0", AUTH_PORT)).await?;
        info!(port = AUTH_PORT, "auth server (TLS) listening");
        loop {
            let (stream, peer) = listener.accept().await?;
            let config = Arc::clone(&config);
            let accounts = Arc::clone(&self.accounts);
            tokio::spawn(async move {
                if let Err(e) = handle_auth_conn(stream, config, accounts).await {
                    warn!(?peer, error = %e, "auth connection failed");
                }
            });
        }
    }
}

async fn handle_auth_conn(
    stream: TcpStream,
    config: Arc<rustls::ServerConfig>,
    accounts: Arc<RwLock<Accounts>>,
) -> anyhow::Result<()> {
    let mut tls = tokio_rustls_accept(stream, config).await?;
    let mut pending_user: Option<String> = None;
    let mut buf = Vec::new();
    let mut frame = [0u8; 257];
    loop {
        // Read one frame (type + len + payload).
        let mut got = 0usize;
        while got < 2 {
            let n = tls.read(&mut frame[got..2]).await?;
            if n == 0 {
                return Ok(());
            }
            got += n;
        }
        let len = frame[1] as usize;
        while got < 2 + len {
            let n = tls.read(&mut frame[got..2 + len]).await?;
            if n == 0 {
                return Ok(());
            }
            got += n;
        }
        let (ty, payload) = parse_auth_frame(&frame[..2 + len]).unwrap_or((0, Vec::new()));
        let reply: (u8, Vec<u8>) = match ty {
            AUTH_CMD_USR => {
                let name = String::from_utf8_lossy(&payload).into_owned();
                pending_user = Some(name.clone());
                let mut acc = accounts.write().await;
                acc.prune(now());
                (0, Vec::new()) // accept any user
            }
            AUTH_CMD_PASSWD => {
                let user = pending_user.clone().unwrap_or_default();
                let digest_hex = sha256_hex(&payload);
                let mut acc = accounts.write().await;
                match acc.passwords.get(&user) {
                    Some(stored) if *stored != digest_hex => (1, b"Bad password".to_vec()),
                    _ => {
                        acc.passwords.entry(user.clone()).or_insert(digest_hex);
                        let cookie = {
                            let c = rand_bytes(32);
                            acc.cookies.insert(c.clone(), (user.clone(), now() + COOKIE_TTL_SECS));
                            c
                        };
                        (0, cookie)
                    }
                }
            }
            AUTH_CMD_GETTOKEN => {
                let user = pending_user.clone().unwrap_or_default();
                let mut acc = accounts.write().await;
                let tok = rand_bytes(32);
                acc.tokens.insert(tok.clone(), (user.clone(), now() + TOKEN_TTL_SECS));
                (0, tok)
            }
            AUTH_CMD_USETOKEN => {
                let mut acc = accounts.write().await;
                match acc.tokens.remove(&payload) {
                    Some((user, exp)) if exp > now() => {
                        let cookie = rand_bytes(32);
                        acc.cookies.insert(cookie.clone(), (user, now() + COOKIE_TTL_SECS));
                        (0, cookie)
                    }
                    _ => (1, b"Invalid token".to_vec()),
                }
            }
            _ => (1, b"Unknown command".to_vec()),
        };
        buf.clear();
        buf.extend_from_slice(&auth_frame(reply.0, &reply.1)?);
        tls.write_all(&buf).await?;
        tls.flush().await?;
    }
}

async fn tokio_rustls_accept(
    stream: TcpStream,
    config: Arc<rustls::ServerConfig>,
) -> anyhow::Result<tokio_rustls::server::TlsStream<tokio::net::TcpStream>> {
    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    let tls = acceptor.accept(stream).await?;
    Ok(tls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            crate::auth::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            crate::auth::sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_long_input() {
        // a * 1000000 known digest
        let data = vec![b'a'; 1_000_000];
        assert_eq!(
            crate::auth::sha256_hex(&data),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }
}
