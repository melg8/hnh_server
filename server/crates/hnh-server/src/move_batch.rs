//! Packed movement-block fan-out (session 41).
//!
//! Movement progress/finalizer OBJDATA blocks used to be encoded into a
//! per-block `Vec<u8>` and then merged per session by scanning EVERY
//! block for EVERY session (`O(sessions x movers)` hash probes per tick;
//! the dominant guests-phase cost in the 2x300 duel-cohort cluster
//! profile: p95 7.9 ms at 200 sessions, linear in session count). The
//! packed batch inverts the fan-out through the visibility cells:
//!
//! - blocks are encoded once into ONE shared byte buffer (reused across
//!   ticks via `clear`, no per-tick reallocation);
//! - each block records the VisIndex cell of the gob position at encode
//!   time;
//! - a session iterates only the NON-EMPTY cells (a few dozen for a
//!   moving crowd), rejects whole cells with one rectangle test against
//!   its own 2x-retract-hysteresis square, and applies the exact
//!   `visible.contains` filter per surviving block.
//!
//! The fan-out cost drops from `sessions x movers` hash probes to
//! `sessions x moving_cells` rectangle tests plus one probe per
//! actually-visible (session, mover) pair - the pair work is the true
//! lower bound of the fan-out: every visible pair must append its bytes
//! to that session's datagram regardless of the data structure.

use std::collections::HashMap;

use crate::state::GobId;

/// One encoded OBJDATA block: bytes live in the shared `MoveBatch::data`
/// buffer, this is the reference + fan-out metadata.
pub struct BlockRef {
    pub id: GobId,
    /// Wire frame counter carried by the block (dedup key for `unacked`).
    pub frame: u32,
    off: u32,
    len: u32,
    /// Finalizer (move end): lands in `unacked` for OBJACK retransmission.
    /// Progress frames are superseded by the next tick's frame and are
    /// deliberately never recorded.
    pub fin: bool,
    /// Per-session wire-id patch (session 44): the block embeds
    /// session-local resource wire ids at fixed byte offsets, encoded
    /// from game-global index placeholders. Each receiving session
    /// rewrites those bytes with its own wire ids at fan-out time (and
    /// first-announces each resource there) - one encoded block serves
    /// every viewer even though wire ids are session-local. `None` for
    /// blocks with no session-local fields (movement blocks are pure gob
    /// data).
    pub patch: Option<Patch>,
}

/// The session-local wire id patches of one block: a single overlay wire
/// (`One`, no allocation) or a multi-slot layer list (`Many`, the pose
/// blocks' base + every layer wire).
pub enum Patch {
    One { slot: [(u16, u32); 1] },
    Many { entries: Vec<(u16, u32)> },
}

impl Patch {
    /// Iterate `(global index, byte offset)` pairs.
    pub fn entries(&self) -> &[(u16, u32)] {
        match self {
            Patch::One { slot } => slot.as_slice(),
            Patch::Many { entries } => entries.as_slice(),
        }
    }
}

/// Per-tick packed movement blocks + cell index. Reused across ticks:
/// `clear` keeps the allocated capacity (mem-reuse-collections: no
/// allocator churn on the 10 Hz hot path).
#[derive(Default)]
pub struct MoveBatch {
    data: Vec<u8>,
    blocks: Vec<BlockRef>,
    /// Cell -> indices into `blocks`; only non-empty cells exist.
    by_cell: HashMap<(i32, i32), Vec<u32>>,
}

impl MoveBatch {
    /// Reset for a new tick's encoding; capacity is retained.
    pub fn clear(&mut self) {
        self.data.clear();
        self.blocks.clear();
        self.by_cell.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Block count (fan-out pair width at full visibility).
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Append one encoded block with its fan-out metadata.
    pub fn push(&mut self, id: GobId, frame: u32, cell: (i32, i32), fin: bool, block: &[u8]) {
        self.push_patched(id, frame, cell, fin, None, block);
    }

    /// Append a block carrying session-local wire ids (see
    /// `BlockRef::patch`).
    pub fn push_patched(
        &mut self,
        id: GobId,
        frame: u32,
        cell: (i32, i32),
        fin: bool,
        patch: Option<Patch>,
        block: &[u8],
    ) {
        let off = self.data.len() as u32;
        self.data.extend_from_slice(block);
        let idx = self.blocks.len() as u32;
        self.blocks.push(BlockRef {
            id,
            frame,
            off,
            len: block.len() as u32,
            fin,
            patch,
        });
        self.by_cell.entry(cell).or_default().push(idx);
    }

    /// Number of distinct non-empty cells (fan-out probe width).
    pub fn cell_count(&self) -> usize {
        self.by_cell.len()
    }

    /// Iterate `(cell, block indices)` pairs for the fan-out.
    pub fn cells(&self) -> impl Iterator<Item = (&(i32, i32), &Vec<u32>)> {
        self.by_cell.iter()
    }

    /// Block metadata by index: `(id, frame, fin)`.
    #[inline]
    pub fn block_info(&self, idx: u32) -> (GobId, u32, bool) {
        let b = &self.blocks[idx as usize];
        (b.id, b.frame, b.fin)
    }

    /// The block's session-local wire-id patch, if any.
    #[inline]
    pub fn block_patch(&self, idx: u32) -> Option<&Patch> {
        self.blocks[idx as usize].patch.as_ref()
    }

    /// Block bytes by index (borrow from the packed buffer).
    #[inline]
    pub fn block_bytes(&self, idx: u32) -> &[u8] {
        let b = &self.blocks[idx as usize];
        let off = b.off as usize;
        let end = off + b.len as usize;
        &self.data[off..end]
    }
}

/// Whether the cell rectangle `[c*CELL, (c+1)*CELL)` intersects the
/// session's visibility square `[p - span, p + span]` on one axis.
/// `span` covers the 2x retract hysteresis plus the inter-sweep drift of
/// a full-speed mover (see `super::game::FANOUT_SPAN`).
#[inline]
pub fn cell_intersects_axis(c: i32, p: i32, span: i32) -> bool {
    let lo = c * crate::visidx::CELL;
    let hi = lo + crate::visidx::CELL;
    lo <= p + span && hi >= p - span
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: i32) -> Vec<GobId> {
        (1..=n).collect()
    }

    #[test]
    fn push_clear_reuse_keeps_capacity() {
        let mut b = MoveBatch::default();
        for (i, id) in ids(4).into_iter().enumerate() {
            b.push(id, i as u32, (0, 0), false, &[1, 2, 3]);
        }
        assert_eq!(b.blocks.len(), 4);
        assert_eq!(b.cell_count(), 1);
        assert_eq!(b.block_bytes(2), &[1, 2, 3]);
        b.clear();
        assert!(b.is_empty());
        // Reuse after clear: same behavior, stale data must not leak.
        b.push(9, 0, (1, 1), true, &[4, 5]);
        assert_eq!(b.blocks.len(), 1);
        assert_eq!(b.cell_count(), 1);
        assert_eq!(b.block_bytes(0), &[4, 5]);
    }

    #[test]
    fn cells_group_blocks_by_cell() {
        let mut b = MoveBatch::default();
        b.push(1, 0, (0, 0), false, &[1]);
        b.push(2, 0, (3, 3), false, &[2]);
        b.push(3, 0, (0, 0), true, &[3]);
        assert_eq!(b.cell_count(), 2);
        let mut c00: Vec<GobId> = Vec::new();
        let mut c33: Vec<GobId> = Vec::new();
        for (cell, idxs) in b.cells() {
            for &i in idxs {
                let (id, _frame, fin) = b.block_info(i);
                if *cell == (0, 0) {
                    c00.push(id);
                    assert_eq!(i == 2, fin, "third push was the finalizer");
                } else {
                    c33.push(id);
                }
            }
        }
        assert_eq!(c00, vec![1, 3]);
        assert_eq!(c33, vec![2]);
    }

    #[test]
    fn cell_axis_test_bounds() {
        // CELL = 250: cell 2 spans [500, 750).
        assert!(cell_intersects_axis(2, 500, 0), "left edge inclusive");
        assert!(cell_intersects_axis(2, 749, 0), "right edge inclusive");
        assert!(!cell_intersects_axis(2, 499, 0));
        assert!(!cell_intersects_axis(2, 751, 0));
        // Span widens the square symmetrically.
        assert!(cell_intersects_axis(2, 300, 200), "300+200=500 touches lo");
        assert!(!cell_intersects_axis(2, 299, 200), "299+200=499 < 500");
    }
}
