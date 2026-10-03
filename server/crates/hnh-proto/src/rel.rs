//! Reliable ordered transport layer for MSG_REL datagrams.
//!
//! Faithful reimplementation of `src/haven/Session.java` reliability:
//! 16-bit wrapping sequence numbers per direction, cumulative MSG_ACK,
//! hold-back buffer for out-of-order sub-messages, and the exact
//! retransmission backoff table. Direction-agnostic: the game server and
//! the load-test bots both drive one instance each.

use std::collections::HashMap;
use std::time::Duration;

use crate::consts::{MSG_ACK, MSG_REL};
use crate::message::MessageBuf;

/// Retransmission backoff per `Session.SWorker` (lines 713-723).
fn backoff(retx: u32) -> Duration {
    match retx {
        0 => Duration::from_millis(80),
        1..=3 => Duration::from_millis(200),
        4..=9 => Duration::from_millis(620),
        _ => Duration::from_millis(2000),
    }
}

/// Milliseconds of receiver ACK delay (`ackthresh`, Session.java line 68).
pub const ACK_THRESH: Duration = Duration::from_millis(30);

/// One pending outgoing reliable sub-message.
struct Pending {
    seq: u16,
    payload: Vec<u8>,
    retx: u32,
    next_send: std::time::Instant,
}

/// Send side of the reliability layer.
pub struct RelSender {
    tseq: u16,
    pending: Vec<Pending>,
    /// Upper bound on buffered unacked sub-messages; when exceeded, the
    /// oldest pending messages are dropped to avoid unbounded growth if a
    /// peer stalls (session is closed by the driver on timeout anyway).
    window: usize,
}

impl Default for RelSender {
    fn default() -> Self {
        Self::new()
    }
}

impl RelSender {
    pub fn new() -> Self {
        Self { tseq: 0, pending: Vec::new(), window: 4096 }
    }

    /// Queue one sub-message payload for reliable ordered delivery.
    pub fn queue(&mut self, payload: &[u8]) {
        if self.pending.len() >= self.window {
            // Session is seriously stalled; drop oldest to keep memory
            // bounded (evict-oldest keeps ordering consistent for the rest).
            let drop = self.pending.len() / 4 + 1;
            self.pending.drain(..drop);
        }
        self.pending.push(Pending {
            seq: self.tseq,
            payload: payload.to_vec(),
            retx: 0,
            next_send: std::time::Instant::now(),
        });
        self.tseq = self.tseq.wrapping_add(1);
    }

    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Produce MSG_REL datagrams carrying every sub-message whose send time
    /// has arrived. Sub-messages are bundled up to `mtu` bytes per datagram,
    /// using the explicit-length form for all but the last sub-message.
    pub fn poll_transmit(&mut self, now: std::time::Instant, mtu: usize) -> Vec<Vec<u8>> {
        let due: Vec<usize> = self
            .pending
            .iter()
            .enumerate()
            .filter(|(_, p)| p.next_send <= now)
            .map(|(i, _)| i)
            .collect();
        if due.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut cur = MessageBuf::with_capacity(mtu);
        cur.uint8(MSG_REL);
        // Sequence number of the FIRST sub-message in this datagram.
        let first_seq = self.pending[due[0]].seq;
        cur.uint16(first_seq);
        let mut bundled = 0usize;
        for &i in &due {
            let p = &self.pending[i];
            let len = p.payload.len();
            let overhead = 3 + len + 1; // seq header amortized + len prefix + type
            if cur.len() + overhead > mtu && bundled > 0 {
                out.push(cur.finish());
                cur = MessageBuf::with_capacity(mtu);
                cur.uint8(MSG_REL);
                cur.uint16(p.seq);
                bundled = 0;
            }
            if i == *due.last().unwrap() {
                // Last due sub-message: no length prefix, fills the rest.
                cur.uint8(p.payload[0]);
                cur.bytes(&p.payload[1..]);
            } else {
                cur.uint8(p.payload[0] | 0x80);
                cur.uint16(len as u16 - 1);
                cur.bytes(&p.payload[1..]);
            }
            bundled += 1;
        }
        if cur.len() > 1 {
            out.push(cur.finish());
        }
        for &i in &due {
            let p = &mut self.pending[i];
            p.retx = p.retx.saturating_add(1);
            p.next_send = now + backoff(p.retx);
        }
        out
    }

    /// Handle a cumulative MSG_ACK: retire everything up to and including seq.
    pub fn on_ack(&mut self, seq: u16) {
        self.pending.retain(|p| seq_wrapped_lt(seq, p.seq));
    }
}

/// True when `a < b` in 16-bit serial arithmetic.
fn seq_wrapped_lt(a: u16, b: u16) -> bool {
    let d = b.wrapping_sub(a);
    d != 0 && d < 0x8000
}

/// Receive side of the reliability layer.
pub struct RelReceiver {
    rseq: u16,
    waiting: HashMap<u16, Vec<u8>>,
    ack_due: Option<std::time::Instant>,
    /// Highest in-order sequence eligible for cumulative ack (rseq - 1).
    ack_seq: u16,
    ack_dirty: bool,
}

impl Default for RelReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl RelReceiver {
    pub fn new() -> Self {
        Self {
            rseq: 0,
            waiting: HashMap::new(),
            ack_due: None,
            ack_seq: u16::MAX,
            ack_dirty: false,
        }
    }

    /// Ingest one MSG_REL datagram payload (after the MSG_REL type byte),
    /// returning sub-messages newly delivered in order.
    pub fn on_rel(&mut self, payload: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        let mut m = MessageBuf::from_slice(payload);
        let Ok(start_seq) = m.u16() else { return out };
        let mut seq = start_seq;
        let rest = m.rest().to_vec();
        let mut off = 0usize;
        loop {
            if off >= rest.len() {
                break;
            }
            let mut rtype = rest[off];
            off += 1;
            let sub: Vec<u8>;
            if rtype & 0x80 != 0 {
                rtype &= 0x7f;
                if off + 2 > rest.len() {
                    break; // truncated length; drop the tail
                }
                let len = u16::from_le_bytes([rest[off], rest[off + 1]]) as usize;
                off += 2;
                if off + len > rest.len() {
                    break;
                }
                sub = rest[off..off + len].to_vec();
                off += len;
            } else {
                sub = rest[off..].to_vec();
                off = rest.len();
            }
            let mut full = Vec::with_capacity(sub.len() + 1);
            full.push(rtype);
            full.extend_from_slice(&sub);
            if seq == self.rseq {
                out.push((rtype, full.clone()));
                self.rseq = self.rseq.wrapping_add(1);
                // Drain any consecutively buffered messages.
                while let Some(next) = self.waiting.remove(&self.rseq) {
                    let nt = next[0];
                    out.push((nt, next));
                    self.rseq = self.rseq.wrapping_add(1);
                }
                self.ack_seq = self.rseq.wrapping_sub(1);
                if !self.ack_dirty {
                    self.ack_due = Some(std::time::Instant::now() + ACK_THRESH);
                    self.ack_dirty = true;
                }
            } else if seq_wrapped_lt(self.rseq, seq) {
                self.waiting.entry(seq).or_insert(full);
            }
            // else: duplicate from retransmission; drop.
            seq = seq.wrapping_add(1);
        }
        out
    }

    /// Produce the pending cumulative MSG_ACK datagram, if due.
    pub fn poll_ack(&mut self, now: std::time::Instant) -> Option<Vec<u8>> {
        if self.ack_dirty && self.ack_due.map(|d| d <= now).unwrap_or(false) {
            self.ack_dirty = false;
            let seq = self.ack_seq;
            let buf = vec![MSG_ACK, (seq & 0xff) as u8, (seq >> 8) as u8];
            return Some(buf);
        }
        None
    }

    /// Force an ACK now (used before sending on the same path to shrink latency).
    pub fn ack_now(&mut self) -> Option<Vec<u8>> {
        if self.ack_dirty {
            self.ack_dirty = false;
            let seq = self.ack_seq;
            return Some(vec![MSG_ACK, (seq & 0xff) as u8, (seq >> 8) as u8]);
        }
        None
    }

    pub fn buffered(&self) -> usize {
        self.waiting.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant() -> std::time::Instant {
        std::time::Instant::now()
    }

    #[test]
    fn in_order_delivery_and_ack() {
        let mut tx = RelSender::new();
        let mut rx = RelReceiver::new();
        tx.queue(b"\x00hello");
        tx.queue(b"\x01world");
        let dgrams = tx.poll_transmit(instant(), 1200);
        assert_eq!(dgrams.len(), 1);
        // First datagram: MSG_REL, seq=0, first sub has len-prefix, last has none.
        assert_eq!(dgrams[0][0], MSG_REL);
        let delivered = rx.on_rel(&dgrams[0][1..]);
        assert_eq!(delivered.len(), 2);
        assert_eq!(delivered[0].0, 0);
        assert_eq!(&delivered[0].1, b"\x00hello");
        assert_eq!(delivered[1].0, 1);
        let ack = rx.poll_ack(instant() + ACK_THRESH).unwrap();
        assert_eq!(ack[0], MSG_ACK);
        let ackseq = u16::from_le_bytes([ack[1], ack[2]]);
        assert_eq!(ackseq, 1);
        tx.on_ack(ackseq);
        assert_eq!(tx.in_flight(), 0);
    }

    #[test]
    fn out_of_order_holdback() {
        let mut tx = RelSender::new();
        let mut rx = RelReceiver::new();
        // Realistic sub-messages: type byte + body.
        tx.queue(b"\x01a");
        tx.queue(b"\x01b");
        tx.queue(b"\x01c");
        let dgrams = tx.poll_transmit(instant(), 1200);
        assert_eq!(dgrams.len(), 1);
        let payload = &dgrams[0][1..];
        // Feed a datagram containing only sub #1 by re-encoding:
        // uint16 seq=1, then explicit-length form: (type|0x80), uint16 len, body.
        let d2 = [0x01u8, 0x00, 0x01 | 0x80, 0x01, 0x00, b'b'];
        let got = rx.on_rel(&d2);
        assert!(got.is_empty(), "must hold back future sub-message");
        let got = rx.on_rel(payload);
        assert_eq!(got.len(), 3, "gap fills and drains in order");
        assert_eq!(got[0].1[1], b'a');
        assert_eq!(got[1].1[1], b'b');
        assert_eq!(got[2].1[1], b'c');
    }

    #[test]
    fn duplicate_dropped() {
        let mut tx = RelSender::new();
        let mut rx = RelReceiver::new();
        tx.queue(b"x");
        let d = tx.poll_transmit(instant(), 1200);
        assert_eq!(rx.on_rel(&d[0][1..]).len(), 1);
        assert!(rx.on_rel(&d[0][1..]).is_empty());
    }
}
