//! OBJDATA retransmission regime: unacked-block bookkeeping and
//! the schedule constants the retransmit sweep obeys.
//!
//! Split out of the single-file state.rs (session 94); the
//! session-numbered comments inside are the living changelog.

use std::time::{Duration, Instant};

// OBJDATA retransmission regime (session 64). The legacy client
// repeats its batched MSG_OBJACK every 200 ms (Session.java SWorker),
// so a recorded block still unconfirmed past RETRANS_FAST_MS is
// presumed lost and resent. Critical blocks (spawn / retract / full
// re-render) have NO client-side recovery path (a lost spawn is never
// re-requested), so they retire later than self-healing ones
// (movement finalizers, hp ticks - the next tick's frame supersedes
// them).
pub const RETRANS_FAST_MS: u64 = 250;
/// Slow-schedule delay: the last resort window before a block retires.
pub const RETRANS_SLOW_MS: u64 = 1000;
const RETRANS_CRIT_FAST_TRIES: u8 = 5;
const RETRANS_PLAIN_FAST_TRIES: u8 = 3;
/// Hard age ceiling for any pending block (session 65). The try-count
/// retirement above assumes attempts actually BURN - but a raw-queue-full
/// refusal deliberately does not burn one, so a peer that stopped draining
/// its raw queue (a dead bot cohort, a suspended client) would otherwise
/// pin its pending table forever: the 1000-bot run measured 1.6M pending
/// blocks, a 426 ms sweep and 85% queue-full refusals exactly because the
/// cohort never OBJACKs. Age caps every block's residence regardless of
/// send attempts and bounds the sweep walk to the recent past.
pub const RETRANS_MAX_AGE_MS: u64 = 10_000;
/// After a raw-queue-full refusal the session's retransmit pass is skipped
/// for this long: the queue needs time to drain, and hammering try_send
/// against a full bounded channel (85% refusals measured) only burns tick
/// budget. Retries never fire into a saturated queue.
pub(crate) const RETRANS_THROTTLE_MS: u64 = 1_000;

/// Critical blocks give up after 9 sends (~250*5 + 1000*4 ms of
/// protection), plain ones after 5 - sessions that never OBJACK (load
/// bots, dead peers) self-clean instead of accumulating retransmit
/// state.
const RETRANS_CRIT_MAX_TRIES: u8 = 9;
const RETRANS_PLAIN_MAX_TRIES: u8 = 5;

/// Upper bound for the ack-lag adaptive first-retry delay (session 68):
/// `2 * ack_lag_ema_ms` is floored onto the legacy schedule, capped here
/// so a pathologically slow acker still gets retried inside its
/// try-count window; RETRANS_MAX_AGE_MS stays the absolute ceiling.
pub(crate) const ACK_LAG_RTO_CAP_MS: u32 = 4_000;

/// Global per-sweep retransmission budget (session 68). The entry-burst
/// measurement put 44-73K `bytes.clone()` + `try_send` calls into ONE
/// sweep pass (85-107 ms of the 100 ms tick budget): a clone is ~1 us of
/// allocator work, so the budget is what actually caps the sweep's
/// worst case, not the table walk. Retransmissions deferred by the
/// budget are picked up by the next sweep (one per 3 ticks) inside the
/// 10 s age ceiling; real peers ack well inside the window, load bots
/// retire through RETRANS_MAX_AGE_MS either way.
pub(crate) const RETRANS_SWEEP_BUDGET: usize = 8_192;
/// Minimum per-session share of the sweep budget: a session with a
/// handful of pending blocks drains them in one pass even when a large
/// cohort shares the ring (the global budget still caps the sum).
pub(crate) const RETRANS_SESSION_SHARE_MIN: usize = 64;

/// Inline byte budget for ONE recorded OBJDATA block (session 78).
/// The entry-burst blocks are small: an OD_REM is ~13 bytes, a static
/// spawn ~25, an animal spawn ~30, a player spawn with a full doll
/// rides ~90. Oversize blocks (long names + heavy equipment) spill to
/// the heap and pay the old clone cost - the profile showed none of
/// them in the pending table's hot path.
pub(crate) const UNACKED_INLINE: usize = 144;

/// OBJDATA block bytes that keep the retransmit hot path out of the
/// allocator (session 78). Every resend used to deep-clone a `Vec<u8>`
/// (malloc + copy + free on the tick thread); the inline variant turns
/// that into a single memcpy, and the raw UDP channel now ships this
/// enum end to end so the copy is the ONLY wire-side work left.
#[derive(Debug)]
pub enum BlockBytes {
    Inline { len: u16, buf: [u8; UNACKED_INLINE] },
    Heap(Vec<u8>),
}

impl BlockBytes {
    /// Take ownership of an encoded datagram. Small payloads move into
    /// the inline buffer (the source allocation is dropped), large ones
    /// keep their heap buffer by value.
    pub fn from_vec(v: Vec<u8>) -> Self {
        if v.len() <= UNACKED_INLINE {
            let mut buf = [0u8; UNACKED_INLINE];
            buf[..v.len()].copy_from_slice(&v);
            BlockBytes::Inline {
                len: v.len() as u16,
                buf,
            }
        } else {
            BlockBytes::Heap(v)
        }
    }

    /// Copy shared encoded bytes (session 82). The fan-out hot path
    /// records finalizer blocks whose bytes live in the batch's packed
    /// `data` buffer - `record_unacked` used to take them as `to_vec()`
    /// (malloc + copy on the tick thread, then a second copy into this
    /// inline buffer and a free), ~24k times per tick at the 1000-bot
    /// scale. This constructor copies straight into the inline variant;
    /// only oversize blocks pay one heap allocation.
    pub fn from_slice(s: &[u8]) -> Self {
        if s.len() <= UNACKED_INLINE {
            let mut buf = [0u8; UNACKED_INLINE];
            buf[..s.len()].copy_from_slice(s);
            BlockBytes::Inline {
                len: s.len() as u16,
                buf,
            }
        } else {
            BlockBytes::Heap(s.to_vec())
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        match self {
            BlockBytes::Inline { len, buf } => &buf[..*len as usize],
            BlockBytes::Heap(v) => v,
        }
    }
}

/// Block bytes read like a byte slice (`msg.len()`, `msg[off]`,
/// `msg.first()`, `msg.windows(..)`) - the UDP sender task and the
/// test harness walk the payload the same way they walked the old
/// `Vec<u8>`.
impl std::ops::Deref for BlockBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Clone for BlockBytes {
    fn clone(&self) -> Self {
        match self {
            BlockBytes::Inline { len, buf } => BlockBytes::Inline {
                len: *len,
                buf: {
                    let mut copy = [0u8; UNACKED_INLINE];
                    copy[..*len as usize].copy_from_slice(&buf[..*len as usize]);
                    copy
                },
            },
            BlockBytes::Heap(v) => BlockBytes::Heap(v.clone()),
        }
    }
}

/// One recorded OBJDATA block awaiting its OBJACK.
#[derive(Debug, Clone)]
pub struct UnackedBlock {
    /// Wire frame the block carries. Was the BTreeMap key before
    /// session 78; the flat per-gob vector orders blocks by it.
    pub frame: u32,
    pub bytes: BlockBytes,
    /// Last transmit instant (initial send or a retransmission); the
    /// sweep compares its age against the schedule delay.
    pub last_sent: Instant,
    /// First transmit instant; RETRANS_MAX_AGE_MS counts from here so a
    /// throttled session (queue-full refusals never burn attempts) still
    /// drains its pending table deterministically.
    pub(crate) born: Instant,
    /// Total sends so far (initial + retransmissions).
    pub tries: u8,
    /// Critical-loss block: the client cannot recover it any other way
    /// (spawn, retract, full re-render).
    pub critical: bool,
}

/// Pending OBJDATA blocks of ONE gob, ordered by frame ascending - the
/// order the original sends had on the wire (session 78). Replaces
/// `BTreeMap<u32, UnackedBlock>`: with the per-gob cap of 4 entries the
/// tree paid a heap node per block and a pointer chase per walk step;
/// the flat vector keeps the identical ordered walk over contiguous
/// memory and allocates ONCE per gob (capacity reserved for the cap).
/// Frames grow monotonically, so inserts are tail pushes in practice.
#[derive(Debug, Default)]
pub struct PerGobPending {
    pub blocks: Vec<UnackedBlock>,
}

impl PerGobPending {
    pub(crate) fn with_capacity(cap: usize) -> Self {
        PerGobPending {
            blocks: Vec::with_capacity(cap),
        }
    }

    /// Sorted insert; the frame sequence grows monotonically per gob,
    /// so the common case is a push at the tail. An equal frame
    /// REPLACES the stored block - the BTreeMap insert semantics the
    /// FX-overlay path relies on (an overlay block carries the gob's
    /// CURRENT frame and may re-record it).
    pub(crate) fn insert_sorted(&mut self, block: UnackedBlock) {
        let pos = self.blocks.partition_point(|b| b.frame < block.frame);
        if pos < self.blocks.len() && self.blocks[pos].frame == block.frame {
            self.blocks[pos] = block;
        } else if pos == self.blocks.len() {
            self.blocks.push(block);
        } else {
            self.blocks.insert(pos, block);
        }
    }

    /// Lowest pending frame (the old `keys().next()`); test-only, the
    /// production paths read `blocks[0]` directly.
    #[cfg(test)]
    pub(crate) fn first_frame(&self) -> Option<u32> {
        self.blocks.first().map(|b| b.frame)
    }

    /// Highest pending frame (the old `keys().next_back()`).
    pub(crate) fn last_frame(&self) -> Option<u32> {
        self.blocks.last().map(|b| b.frame)
    }

    #[cfg(test)]
    pub(crate) fn contains_frame(&self, frame: u32) -> bool {
        self.blocks.iter().any(|b| b.frame == frame)
    }

    #[cfg(test)]
    pub(crate) fn block(&self, frame: u32) -> Option<&UnackedBlock> {
        self.blocks.iter().find(|b| b.frame == frame)
    }

    /// The highest pending frame `<= acked` (the ack-lag sample reads
    /// its `last_sent`; the old `range(..=frame).next_back()`).
    pub(crate) fn at_or_below(&self, frame: u32) -> Option<&UnackedBlock> {
        let above = self.blocks.partition_point(|b| b.frame <= frame);
        above.checked_sub(1).map(|i| &self.blocks[i])
    }

    /// Drop every block the ack covers (the old `retain(|f, _| f >
    /// frame)`); keeps its allocation for the next spawn wave.
    pub(crate) fn retain_above(&mut self, frame: u32) {
        self.blocks.retain(|b| b.frame > frame);
    }

    /// Drop every block past its retransmit schedule (the retire pass).
    pub(crate) fn retain_unexpired(&mut self, now: Instant) {
        self.blocks.retain(|b| !b.expired(now));
    }
}

impl UnackedBlock {
    pub(crate) fn delay(&self) -> Duration {
        let fast = if self.critical {
            RETRANS_CRIT_FAST_TRIES
        } else {
            RETRANS_PLAIN_FAST_TRIES
        };
        Duration::from_millis(if self.tries < fast {
            RETRANS_FAST_MS
        } else {
            RETRANS_SLOW_MS
        })
    }

    pub(crate) fn retired(&self) -> bool {
        self.tries
            >= if self.critical {
                RETRANS_CRIT_MAX_TRIES
            } else {
                RETRANS_PLAIN_MAX_TRIES
            }
    }

    /// Past the try-count schedule OR the hard age ceiling. The sweep
    /// retires both alike; age is the guarantee that a never-acking or
    /// throttled session drains its table.
    pub(crate) fn expired(&self, now: Instant) -> bool {
        self.retired() || now.duration_since(self.born).as_millis() as u64 >= RETRANS_MAX_AGE_MS
    }
}

#[cfg(test)]
mod unacked_tests {
    use super::*;

    fn block(frame: u32, len: usize) -> UnackedBlock {
        let v = vec![0xABu8; len];
        UnackedBlock {
            frame,
            bytes: BlockBytes::from_vec(v),
            last_sent: Instant::now(),
            born: Instant::now(),
            tries: 0,
            critical: false,
        }
    }

    /// Session 78: small blocks stay out of the heap, oversize ones
    /// spill, clones keep the payload, and Deref reads like a slice.
    #[test]
    fn block_bytes_inline_spill_and_clone() {
        assert!(matches!(
            BlockBytes::from_vec(vec![1u8; UNACKED_INLINE]),
            BlockBytes::Inline { .. }
        ));
        assert!(matches!(
            BlockBytes::from_vec(vec![1u8; UNACKED_INLINE + 1]),
            BlockBytes::Heap(_)
        ));
        let original = BlockBytes::from_vec(vec![7u8; 40]);
        let copy = original.clone();
        assert_eq!(copy.as_slice(), &[7u8; 40][..]);
        assert_eq!(copy.len(), 40, "Deref Target = [u8]");
    }

    /// Session 82: `from_slice` copies borrowed bytes straight into the
    /// inline variant (zero allocation for blocks within the inline
    /// width) and spills oversize slices to one heap buffer - the same
    /// layout contract `from_vec` keeps.
    #[test]
    fn block_bytes_from_slice_matches_from_vec() {
        for n in [0usize, 1, 8, UNACKED_INLINE, UNACKED_INLINE + 1] {
            let src: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            let borrowed = BlockBytes::from_slice(&src);
            let owned = BlockBytes::from_vec(src.clone());
            assert_eq!(borrowed.as_slice(), owned.as_slice(), "n={}", n);
            match (n <= UNACKED_INLINE, &borrowed) {
                (true, BlockBytes::Inline { .. }) => {}
                (false, BlockBytes::Heap(_)) => {}
                _ => panic!("n={} took the wrong variant", n),
            }
        }
    }

    /// The flat per-gob table keeps the BTreeMap contract: ascending
    /// order, equal-frame replace (the FX-overlay re-record), min/max
    /// accessors, the ack-coverage retain and the ack-lag lookup.
    #[test]
    fn per_gob_pending_keeps_the_ordered_contract() {
        let mut per = PerGobPending::with_capacity(4);
        for f in [30u32, 10, 20] {
            per.insert_sorted(block(f, 12));
        }
        assert_eq!(
            per.blocks.iter().map(|b| b.frame).collect::<Vec<_>>(),
            vec![10, 20, 30],
            "inserts land sorted by frame"
        );
        assert_eq!(per.first_frame(), Some(10));
        assert_eq!(per.last_frame(), Some(30));

        // Equal frame replaces the stored block (BTreeMap::insert did).
        per.insert_sorted(block(20, 24));
        assert_eq!(
            per.blocks.iter().map(|b| b.frame).collect::<Vec<_>>(),
            vec![10, 20, 30]
        );
        assert_eq!(per.block(20).map(|b| b.bytes.as_slice().len()), Some(24));

        // The ack-lag sample: the highest frame at or below the ack.
        assert_eq!(per.at_or_below(25).map(|b| b.frame), Some(20));
        assert_eq!(per.at_or_below(10).map(|b| b.frame), Some(10));
        assert!(per.at_or_below(9).is_none());

        per.retain_above(15);
        assert_eq!(
            per.blocks.iter().map(|b| b.frame).collect::<Vec<_>>(),
            vec![20, 30]
        );
        assert!(per.contains_frame(20));
        assert!(!per.contains_frame(10));
    }

    /// The cap trim drops the LOWEST frame (the BTreeMap keys().next()
    /// removal) and the retire pass drops expired blocks in place.
    #[test]
    fn per_gob_pending_cap_and_retire() {
        let mut per = PerGobPending::with_capacity(4);
        for f in [40u32, 10, 30, 20] {
            per.insert_sorted(block(f, 8));
            while per.blocks.len() > 4 {
                per.blocks.remove(0);
            }
        }
        assert_eq!(per.blocks.len(), 4);
        assert_eq!(per.first_frame(), Some(10));
        per.insert_sorted(block(50, 8));
        assert_eq!(
            per.blocks.len(),
            5,
            "trim is the caller's job (record_unacked)"
        );
        per.blocks.remove(0);
        assert_eq!(per.first_frame(), Some(20), "the min frame is blocks[0]");

        // Retire: a block born 2x the age ceiling ago must drop, a
        // fresh one stays.
        let stale = block(60, 8);
        per.insert_sorted(UnackedBlock {
            born: Instant::now() - Duration::from_millis(RETRANS_MAX_AGE_MS + 1_000),
            ..stale
        });
        per.retain_unexpired(Instant::now());
        assert!(!per.contains_frame(60), "past the age ceiling: retired");
        assert!(per.contains_frame(50), "fresh block stays");
    }
}
