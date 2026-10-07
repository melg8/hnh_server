//! Dirty-cell spatial index for the visibility scan
//! (session 15 leaf-1.2.3: the 10k-target optimization).
//!
//! The per-tick visibility update used to rescan every alive gob for
//! every session (`O(sessions x gobs)` distance checks). This index
//! buckets gobs into coarse square cells and tracks which cells changed
//! since the last tick:
//!
//! - spawn/kill/move mark exactly the touched cells dirty;
//! - an active mover keeps its current cell dirty every tick so viewers
//!   keep receiving LINSTEP progress and boundary exits are caught;
//! - a session whose view square intersects no dirty cell — and whose
//!   own cell did not change — skips the scan and the retract sweep
//!   entirely: nothing in its view can have changed.
//!
//! Cell size must exceed the per-tick movement bound so a gob exiting
//! the view always dirties a boundary cell that still intersects the
//! view square (250 subtiles vs the ~50-subtile/tick speed bound).

use crate::fxhash::{FxHashMap, FxHashSet};
use crate::state::GobId;

/// Square cell edge in subtiles. 250 ≫ max per-tick movement, small
/// enough that a view square (±300..1000 subtiles, view radius and the
/// 2x retract square) touches at most ~60 cells.
pub const CELL: i32 = 250;

#[inline]
pub fn cell_of(x: i32, y: i32) -> (i32, i32) {
    (x.div_euclid(CELL), y.div_euclid(CELL))
}

#[derive(Default)]
pub struct VisIndex {
    /// Cell -> gob ids in that cell (materialized on demand). All keys
    /// are server-computed coordinates -> the fxhash id hasher.
    cells: FxHashMap<(i32, i32), Vec<GobId>>,
    /// Per-gob current cell (reindex bookkeeping; ids never reuse
    /// in-session, so a HashMap keyed by GobId cannot collide).
    cell_of_gob: FxHashMap<GobId, (i32, i32)>,
    /// Cells whose contents changed since the last `clear_dirty`.
    dirty: FxHashSet<(i32, i32)>,
    /// Per-tick touched lists (session 30): every gob that spawned, died
    /// or MOVED is recorded under each cell relevant to the change (old
    /// and new for a boundary crosser). A session whose position is
    /// unchanged can then patch its last scan RESULT from the touched
    /// lists instead of rescanning: leavers are re-filtered from the old
    /// result by their current position, enterers are exactly the touched
    /// ids now in range. Cleared together with `dirty`.
    touched: FxHashMap<(i32, i32), Vec<GobId>>,
}

impl VisIndex {
    pub fn insert(&mut self, gob: GobId, pos: (i32, i32)) {
        let c = cell_of(pos.0, pos.1);
        self.cells.entry(c).or_default().push(gob);
        self.cell_of_gob.insert(gob, c);
        self.dirty.insert(c);
        self.touched.entry(c).or_default().push(gob);
    }

    pub fn remove(&mut self, gob: GobId) {
        if let Some(c) = self.cell_of_gob.remove(&gob) {
            if let Some(v) = self.cells.get_mut(&c) {
                v.retain(|&g| g != gob);
                if v.is_empty() {
                    self.cells.remove(&c);
                }
            }
            self.dirty.insert(c);
            self.touched.entry(c).or_default().push(gob);
        }
    }

    /// Reindex after a position change; both the old and the new cell are
    /// marked dirty (a boundary exit must be visible to the scan that
    /// retracts the gob).
    pub fn reposition(&mut self, gob: GobId, new_pos: (i32, i32)) {
        let nc = cell_of(new_pos.0, new_pos.1);
        match self.cell_of_gob.get(&gob) {
            Some(&oc) if oc == nc => {
                self.dirty.insert(nc);
                self.touched.entry(nc).or_default().push(gob);
            }
            Some(&oc) => {
                if let Some(v) = self.cells.get_mut(&oc) {
                    v.retain(|&g| g != gob);
                    if v.is_empty() {
                        self.cells.remove(&oc);
                    }
                }
                self.cells.entry(nc).or_default().push(gob);
                self.cell_of_gob.insert(gob, nc);
                self.dirty.insert(oc);
                self.dirty.insert(nc);
                // The mover is discoverable under BOTH cells: an unmoved
                // viewer's patch finds leavers through their old cell.
                self.touched.entry(oc).or_default().push(gob);
                self.touched.entry(nc).or_default().push(gob);
            }
            None => {
                self.insert(gob, new_pos);
            }
        }
    }

    /// Mark a mover's current cell dirty without reindexing: an active
    /// mover needs LINSTEP streaming every tick it is in view.
    pub fn mark_mover(&mut self, gob: GobId) {
        if let Some(&c) = self.cell_of_gob.get(&gob) {
            self.dirty.insert(c);
            self.touched.entry(c).or_default().push(gob);
        }
    }

    /// Whether any dirty cell intersects the view square around (px, py)
    /// expanded by one cell (boundary-exit tolerance). `span` is the view
    /// half-width in subtiles.
    ///
    /// Retained for the visidx unit tests (the skip-proof semantics they
    /// document); the session-30 result-cache path probes
    /// `any_touched_in_view` instead, which is strictly finer.
    #[allow(dead_code)]
    ///
    /// Cost is O(view cells) with early exit — the view square touches at
    /// most ~36 cells at the session-24 view radius — probing the dirty
    /// HashSet per cell. The previous implementation iterated the whole
    /// dirty set per session (O(sessions x dirty cells): at the 1000-
    /// session scale that was the dominant vis-phase cost when many
    /// movers keep their cells dirty across the whole lattice).
    pub fn any_dirty_in_view(&self, px: i32, py: i32, span: i32) -> bool {
        let (cx0, cx1) = cell_range(px - span - CELL, px + span + CELL);
        let (cy0, cy1) = cell_range(py - span - CELL, py + span + CELL);
        for cy in cy0..=cy1 {
            for cx in cx0..=cx1 {
                if self.dirty.contains(&(cx, cy)) {
                    return true;
                }
            }
        }
        false
    }

    /// Gobs in the cells intersecting the view square. The caller applies
    /// the exact distance filter (cells are coarse buckets).
    pub fn gobs_in_view(&self, px: i32, py: i32, span: i32) -> Vec<GobId> {
        let mut out = Vec::new();
        self.gobs_in_view_into(px, py, span, &mut out);
        out
    }

    /// Allocation-free variant of [`Self::gobs_in_view`]: reuses `out`
    /// (cleared first). Hot path - one call per rescanned session per
    /// tick; the buffer lives for the whole scan pass, not per session.
    pub fn gobs_in_view_into(&self, px: i32, py: i32, span: i32, out: &mut Vec<GobId>) {
        out.clear();
        let (cx0, cx1) = cell_range(px - span, px + span);
        let (cy0, cy1) = cell_range(py - span, py + span);
        for cy in cy0..=cy1 {
            for cx in cx0..=cx1 {
                if let Some(v) = self.cells.get(&(cx, cy)) {
                    out.extend_from_slice(v);
                }
            }
        }
    }

    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
        self.touched.clear();
    }

    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// Total touched gob count under cells intersecting the view square
    /// (no collection; dupes counted). The session-30 patch path uses
    /// this as a size guard: patch work is proportional to the touched
    /// set, so when it approaches the view population a full rescan is
    /// the cheaper operation.
    pub fn touched_count_in_view(&self, px: i32, py: i32, span: i32) -> usize {
        let (cx0, cx1) = cell_range(px - span, px + span);
        let (cy0, cy1) = cell_range(py - span, py + span);
        let mut n = 0;
        for cy in cy0..=cy1 {
            for cx in cx0..=cx1 {
                if let Some(v) = self.touched.get(&(cx, cy)) {
                    n += v.len();
                }
            }
        }
        n
    }

    /// Touched gob ids recorded under cells intersecting the view square.
    /// MAY CONTAIN DUPLICATES (a boundary crosser is recorded under both
    /// its old and its new cell) - the caller dedups; no HashSet churn on
    /// this hot path. These are the only ids whose view membership can
    /// have changed since the last tick for a position-stable session.
    /// Allocation-free: `out` is cleared first and refilled.
    pub fn touched_in_view_into(&self, px: i32, py: i32, span: i32, out: &mut Vec<GobId>) {
        out.clear();
        let (cx0, cx1) = cell_range(px - span, px + span);
        let (cy0, cy1) = cell_range(py - span, py + span);
        for cy in cy0..=cy1 {
            for cx in cx0..=cx1 {
                if let Some(v) = self.touched.get(&(cx, cy)) {
                    out.extend_from_slice(v);
                }
            }
        }
    }
}

fn cell_range(lo: i32, hi: i32) -> (i32, i32) {
    (lo.div_euclid(CELL), hi.div_euclid(CELL))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: i32) -> Vec<GobId> {
        // Gob ids are slot + generation packed; any distinct i32 works
        // for index-level tests.
        (1..=n).collect()
    }

    #[test]
    fn spawn_move_kill_mark_exactly_touched_cells() {
        let mut v = VisIndex::default();
        v.insert(1, (0, 0));
        assert!(v.any_dirty_in_view(0, 0, 500));
        v.clear_dirty();
        assert!(!v.any_dirty_in_view(0, 0, 500), "clean world is clean");

        // Move within the same cell: still dirty (LINSTEP streaming).
        v.reposition(1, (100, 100));
        assert!(v.any_dirty_in_view(0, 0, 500));
        v.clear_dirty();
        assert!(!v.any_dirty_in_view(0, 0, 500));

        // Move across a cell boundary: both cells dirty.
        v.reposition(1, (300, 0)); // cell 1
        assert!(v.any_dirty_in_view(0, 0, 500), "old cell dirty");
        v.clear_dirty();
        assert!(!v.any_dirty_in_view(0, 0, 500));
        assert!(!v.any_dirty_in_view(500, 0, 500));

        v.remove(1);
        assert!(v.any_dirty_in_view(500, 0, 500), "removal dirties the cell");
        v.clear_dirty();
        assert!(!v.any_dirty_in_view(500, 0, 500));
    }

    #[test]
    fn stationary_session_skips_when_view_has_no_dirty_cells() {
        let mut v = VisIndex::default();
        let ids = ids(5);
        for (i, &g) in ids.iter().enumerate() {
            // Two gobs inside the view, three far away.
            let pos = if i < 2 {
                (10 * (i as i32 + 1), 10 * (i as i32 + 1))
            } else {
                (100_000 + 250 * i as i32, 100_000)
            };
            v.insert(g, pos);
        }
        v.clear_dirty();
        // An idle session at the origin: nothing dirty in view -> skip.
        assert!(!v.any_dirty_in_view(0, 0, 500));
        // A mover near the session dirties its own cell -> rescan.
        v.mark_mover(ids[2]);
        // ids[2] is far away, so the session still skips.
        assert!(!v.any_dirty_in_view(0, 0, 500));
        v.reposition(ids[0], (200, 0));
        assert!(v.any_dirty_in_view(0, 0, 500));
    }

    #[test]
    fn view_query_equals_full_scan() {
        let mut v = VisIndex::default();
        let ids = ids(40);
        let mut all: Vec<(GobId, (i32, i32))> = Vec::new();
        for (i, &g) in ids.iter().enumerate() {
            // Spread gobs across a large area, including cell boundaries.
            let x = (i as i32 * 173) % 4000 - 2000;
            let y = (i as i32 * 271) % 4000 - 2000;
            let pos = (x, y);
            v.insert(g, pos);
            all.push((g, pos));
        }
        let px = 150;
        let py = -330;
        let span = 1000;
        let mut from_index = v.gobs_in_view(px, py, span);
        from_index.retain(|&g| {
            let (_, pos) = all.iter().find(|(id, _)| *id == g).unwrap();
            (pos.0 - px).abs() <= span && (pos.1 - py).abs() <= span
        });
        from_index.sort();
        let mut full: Vec<GobId> = all
            .iter()
            .filter(|(_, pos)| (pos.0 - px).abs() <= span && (pos.1 - py).abs() <= span)
            .map(|(g, _)| *g)
            .collect();
        full.sort();
        assert_eq!(from_index, full, "cell query must equal the full scan");
    }

    #[test]
    fn boundary_exit_dirties_a_view_intersecting_cell() {
        // A gob walking out of the view: its last in-view cell must stay
        // dirty so the exit tick rescans and retracts it.
        let mut v = VisIndex::default();
        v.insert(7, (990, 0)); // just inside span 1000
        v.clear_dirty();
        assert!(!v.any_dirty_in_view(0, 0, 1000));
        // Walk one cell over (crossing the boundary).
        v.reposition(7, (1100, 0));
        // The old cell (0,0) intersects the view; the new cell (1,0) is
        // within the +1 tolerance even though it is outside the square.
        assert!(v.any_dirty_in_view(0, 0, 1000));
    }

    #[test]
    fn skip_check_is_cell_probe_not_dirty_set_walk() {
        // Session-26 perf pin: with a huge dirty population spread across
        // the lattice, the skip decision must still be correct and must
        // probe only the session's own view cells (the old implementation
        // walked the whole dirty set per session).
        let mut v = VisIndex::default();
        // 200 far-apart movers keep their cells dirty every tick.
        for i in 0..200 {
            let x = 10_000 + (i % 40) * 250;
            let y = 10_000 + (i / 40) * 250;
            v.insert(100 + i, (x, y));
        }
        // Nothing near the origin: skip (no dirty cell in view).
        assert!(
            !v.any_dirty_in_view(0, 0, 300),
            "far dirt must not trip the skip check"
        );
        assert!(!v.any_dirty_in_view(0, 0, 1000));
        // One dirty cell inside the view: rescan.
        v.insert(1, (100, 100));
        assert!(v.any_dirty_in_view(0, 0, 300));
        v.clear_dirty();
        // Dirty cell just outside the view but within the +1-cell
        // boundary tolerance: still rescans (a mover leaving the view).
        v.insert(2, (250 * 2, 0)); // cell (2,0): outside span 300, inside +CELL
        assert!(v.any_dirty_in_view(0, 0, 300));
        // Far outside even the tolerance: skip.
        assert!(!v.any_dirty_in_view(0, 0, 100));
    }
}
