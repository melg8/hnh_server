//! Packed movement-block fan-out (session 41, dense cell index session 57).
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
//! - a session iterates only the NON-EMPTY cells, rejects whole cells
//!   with one rectangle test against its own 2x-retract-hysteresis
//!   square, and applies the exact `visible.contains` filter per
//!   surviving block.
//!
//! Session 57 (fan-out profiling): the per-session cell walk was the
//! measured `mvbat_fanout_us` dominant (1.1-6.5 ms windows at 300
//! sessions) because it iterated a `HashMap<(i32, i32), Vec<u32>>` - a
//! cache-miss per bucket per session, `sessions x cells` times per
//! batch. The cells now live in a DENSE SORTED index instead: one
//! `Vec<CellGroup>` (~24 B per non-empty cell, key-ordered by y then x)
//! plus a `Vec<u32>` of block indices grouped by cell. A session binary
//! -searches its y-cell range and x-tests inside - strictly sequential
//! memory, a few KiB total, resident in L1/L2. The pair work (every
//! visible (session, mover) pair must append its bytes to that session's
//! datagram regardless of the data structure) is untouched: that is the
//! true lower bound of the fan-out.

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

/// One non-empty cell's fan-out group: the cell coords plus the
/// `order[off..off+len]` range of its block indices. ~24 bytes, stored
/// densely and sorted by `(y, x)` - the fan-out's sequential scan
/// substrate.
pub struct CellGroup {
    pub x: i32,
    pub y: i32,
    /// `order[off..off + len]` are this cell's block indices.
    pub off: u32,
    pub len: u32,
}

/// Per-tick packed movement blocks + dense cell index. Reused across
/// ticks: `clear` keeps the allocated capacity (mem-reuse-collections:
/// no allocator churn on the 10 Hz hot path).
#[derive(Default)]
pub struct MoveBatch {
    data: Vec<u8>,
    blocks: Vec<BlockRef>,
    /// Block -> fan-out cell (parallel to `blocks`, push-time).
    block_cell: Vec<(i32, i32)>,
    /// Non-empty cell groups sorted by (y, x); built lazily by
    /// `ensure_groups` on the first fan-out after a push.
    groups: Vec<CellGroup>,
    /// Block indices grouped by `groups`: group i covers
    /// `order[off..off+len]`.
    order: Vec<u32>,
    groups_valid: bool,
}

impl MoveBatch {
    /// Reset for a new tick's encoding; capacity is retained.
    pub fn clear(&mut self) {
        self.data.clear();
        self.blocks.clear();
        self.block_cell.clear();
        self.groups.clear();
        self.order.clear();
        self.groups_valid = true;
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
        self.blocks.push(BlockRef {
            id,
            frame,
            off,
            len: block.len() as u32,
            fin,
            patch,
        });
        self.block_cell.push(cell);
        self.groups_valid = false;
    }

    /// Number of distinct non-empty cells (fan-out probe width).
    pub fn cell_count(&mut self) -> usize {
        self.ensure_groups();
        self.groups.len()
    }

    /// Rebuild the dense cell index if any push invalidated it. Split
    /// from `groups` so the fan-out can take the rebuilt borrow
    /// immutably (the lazy rebuild needs `&mut`).
    pub fn ensure_groups(&mut self) {
        if !self.groups_valid {
            self.rebuild_groups();
        }
    }

    /// The dense cell index: `(groups, order)` with `groups` sorted by
    /// (y, x) and group i covering `order[off..off+len]`. Call
    /// `ensure_groups` after pushing; this borrow is immutable.
    pub fn groups(&self) -> (&[CellGroup], &[u32]) {
        (&self.groups, &self.order)
    }

    /// Sort the block indices by (cell y, cell x) and cut the cell
    /// groups. A few hundred blocks per tick: microseconds, once per
    /// batch.
    fn rebuild_groups(&mut self) {
        self.order.clear();
        self.order.extend(0..self.blocks.len() as u32);
        let cells = &self.block_cell;
        self.order.sort_unstable_by_key(|&i| {
            let (cx, cy) = cells[i as usize];
            (cy, cx)
        });
        self.groups.clear();
        for (pos, &i) in self.order.iter().enumerate() {
            let (cx, cy) = cells[i as usize];
            match self.groups.last_mut() {
                Some(g) if g.x == cx && g.y == cy => g.len += 1,
                _ => self.groups.push(CellGroup {
                    x: cx,
                    y: cy,
                    off: pos as u32,
                    len: 1,
                }),
            }
        }
        self.groups_valid = true;
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

/// The FIRST cell index whose axis interval `[c*CELL, (c+1)*CELL)`
/// intersects `[p - span, p + span]` (inclusive at both ends - the
/// interval is closed on the high side of the cell). Exact integer
/// derivation: `c*CELL + CELL >= p - span` <=> `c >= (p - span - 1)/CELL`
/// (floor, div_euclid); cross-checked against the rectangle test in the
/// unit tests below.
#[inline]
pub fn axis_cell_lo(p: i32, span: i32) -> i32 {
    (p - span - 1).div_euclid(crate::visidx::CELL)
}

/// The LAST cell index whose axis interval intersects
/// `[p - span, p + span]`: `c*CELL <= p + span` <=>
/// `c <= (p + span)/CELL` (floor).
#[inline]
pub fn axis_cell_hi(p: i32, span: i32) -> i32 {
    (p + span).div_euclid(crate::visidx::CELL)
}

/// Whether the cell rectangle `[c*CELL, (c+1)*CELL)` intersects the
/// session's visibility square `[p - span, p + span]` on one axis.
/// Test-only oracle for the `axis_cell_lo/hi` pair (the fan-out walks
/// the sorted groups with the range bounds).
#[cfg(test)]
fn cell_intersects_axis(c: i32, p: i32, span: i32) -> bool {
    let lo = c * crate::visidx::CELL;
    let hi = lo + crate::visidx::CELL;
    lo <= p + span && hi >= p - span
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ids(n: i32) -> Vec<GobId> {
        (1..=n).collect()
    }

    #[test]
    fn push_clear_reuse_keeps_capacity() {
        let mut b = MoveBatch::default();
        for (i, id) in ids(4).into_iter().enumerate() {
            b.push(id, i as u32, (0, 0), false, &[1, 2, 3]);
        }
        assert_eq!(b.len(), 4);
        assert_eq!(b.cell_count(), 1);
        assert_eq!(b.block_bytes(2), &[1, 2, 3]);
        b.clear();
        assert!(b.is_empty());
        // Reuse after clear: same behavior, stale data must not leak.
        b.push(9, 0, (1, 1), true, &[4, 5]);
        assert_eq!(b.len(), 1);
        assert_eq!(b.cell_count(), 1);
        assert_eq!(b.block_bytes(0), &[4, 5]);
    }

    #[test]
    fn groups_sort_by_cell_and_group_blocks() {
        let mut b = MoveBatch::default();
        b.push(1, 0, (0, 0), false, &[1]);
        b.push(2, 0, (3, 3), false, &[2]);
        b.push(3, 0, (0, 0), true, &[3]);
        assert_eq!(b.cell_count(), 2);
        let (groups, order) = b.groups();
        // Sorted by (y, x): cell (0,0) first, then (3,3).
        assert_eq!((groups[0].x, groups[0].y), (0, 0));
        assert_eq!((groups[1].x, groups[1].y), (3, 3));
        assert_eq!(groups[0].len, 2);
        assert_eq!(groups[1].len, 1);
        let mut c00: Vec<GobId> = Vec::new();
        for &i in &order[groups[0].off as usize..(groups[0].off + groups[0].len) as usize] {
            let (id, _frame, fin) = b.block_info(i);
            c00.push(id);
            assert_eq!(i == 2, fin, "third push was the finalizer");
        }
        assert_eq!(c00, vec![1, 3]);
        let c33 = order[groups[1].off as usize..groups[1].off as usize + 1]
            .iter()
            .map(|&i| b.block_info(i).0)
            .collect::<Vec<_>>();
        assert_eq!(c33, vec![2]);
        // Pushing again invalidates the built groups; the new block is
        // included after the rebuild.
        b.push(4, 0, (0, 0), false, &[7]);
        assert_eq!(b.cell_count(), 2);
        let (groups, order) = b.groups();
        assert_eq!(groups[0].len, 3);
        assert_eq!(order.len(), 4);
    }

    #[test]
    fn axis_range_bounds_match_rectangle_test() {
        // Exhaustive cross-check of the exact integer bounds against the
        // rectangle oracle: CELL = 250, p over two cell widths including
        // negatives (cell -1 boundary), spans 0 / 100 / FANOUT-scale 1400.
        for span in [0, 100, 1400] {
            for p in -1500..=1500 {
                let lo = axis_cell_lo(p, span);
                let hi = axis_cell_hi(p, span);
                for c in -12..=12 {
                    assert_eq!(
                        (lo <= c) && (c <= hi),
                        cell_intersects_axis(c, p, span),
                        "p={p} span={span} c={c} range={lo}..={hi}"
                    );
                }
            }
        }
    }

    /// Manual micro-benchmark (run with `cargo test --release --
    /// --ignored --nocapture dense_index_bench`): the dense sorted scan
    /// vs the pre-57 per-session HashMap cell walk, at the documented
    /// 1000-session / 134-cell / 500-block scale. Timing asserts are
    /// deliberately absent (CI stability); the point is a repeatable
    /// relative ratio on any box.
    #[test]
    #[ignore]
    fn dense_index_bench() {
        const SESSIONS: usize = 1000;
        const CELLS: usize = 134;
        const BLOCKS: usize = 500;
        const ROUNDS: usize = 100;
        // 500 movers spread over 134 cells (x varies fastest, y strides).
        let mut b = MoveBatch::default();
        for i in 0..BLOCKS {
            let cx = (i % CELLS) as i32;
            let cy = (i / CELLS) as i32;
            b.push((i + 1) as GobId, 0, (cx, cy), false, &[0u8; 16]);
        }
        // Session anchor positions spread over a wider area than the
        // movers (rectangle rejects most cells per session).
        let anchors: Vec<(i32, i32)> = (0..SESSIONS)
            .map(|s| ((s * 37 % 3000) as i32 - 1500, (s * 91 % 3000) as i32 - 1500))
            .collect();
        const SPAN: i32 = 1400;
        b.ensure_groups();
        let (groups, order) = b.groups();
        let t_dense = Instant::now();
        let mut visits = 0usize;
        for _ in 0..ROUNDS {
            for &(px, py) in &anchors {
                let cy0 = axis_cell_lo(py, SPAN);
                let cy1 = axis_cell_hi(py, SPAN);
                let cx0 = axis_cell_lo(px, SPAN);
                let cx1 = axis_cell_hi(px, SPAN);
                let first = groups.partition_point(|g| g.y < cy0);
                for g in &groups[first..] {
                    if g.y > cy1 {
                        break;
                    }
                    if g.x < cx0 || g.x > cx1 {
                        continue;
                    }
                    for &i in &order[g.off as usize..(g.off + g.len) as usize] {
                        let _ = b.block_info(i);
                        visits += 1;
                    }
                }
            }
        }
        let d_dense = t_dense.elapsed();
        // Old shape: full cell scan per session (what the HashMap iteration
        // cost structurally, minus the per-bucket cache miss it also paid).
        let t_full = Instant::now();
        let mut visits_old = 0usize;
        for _ in 0..ROUNDS {
            for &(px, py) in &anchors {
                for g in groups {
                    if !cell_intersects_axis(g.x, px, SPAN) || !cell_intersects_axis(g.y, py, SPAN)
                    {
                        continue;
                    }
                    for &i in &order[g.off as usize..(g.off + g.len) as usize] {
                        let _ = b.block_info(i);
                        visits_old += 1;
                    }
                }
            }
        }
        let d_full = t_full.elapsed();
        println!(
            "dense_index_bench: dense={d_dense:?} full={d_full:?} \
             pairs(dense)={visits} pairs(full)={visits_old} \
             (sessions={SESSIONS} cells={CELLS} blocks={BLOCKS} rounds={ROUNDS})"
        );
        assert_eq!(visits, visits_old, "both walks must visit the same pairs");
    }
}
