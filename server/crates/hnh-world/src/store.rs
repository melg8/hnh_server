//! Grid store with LRU eviction; grids generate on demand and can be
//! shared between nodes later (the key space is (gx, gy), values are pure).

use std::collections::HashMap;

use crate::gen::{Grid, WorldGen};

/// Maximum grids resident in memory. 10 KB each => 65_536 grids ~= 640 MB.
pub const MAX_CACHED_GRIDS: usize = 65_536;

pub struct GridStore {
    gen: WorldGen,
    grids: HashMap<(i32, i32), std::sync::Arc<Grid>>,
    tick_clock: u64,
    last_use: HashMap<(i32, i32), u64>,
    /// Tile-coordinate overrides (terraforming): (tx, ty) -> tile id.
    /// Applied after on-demand generation so mutations survive LRU
    /// eviction and process restarts (persisted by the game task).
    pub overrides: HashMap<(i32, i32), u8>,
}

impl GridStore {
    pub fn new(seed: u64) -> Self {
        GridStore {
            gen: WorldGen::new(seed),
            grids: HashMap::new(),
            tick_clock: 0,
            last_use: HashMap::new(),
            overrides: HashMap::new(),
        }
    }

    pub fn seed(&self) -> u64 {
        self.gen.seed
    }

    /// Get-or-generate a grid. Generation is deterministic, so any node
    /// produces identical bytes for the same key (grid-shard ready).
    /// Tile overrides (terraforming state) are re-applied on top.
    pub fn grid(&mut self, gc: (i32, i32)) -> std::sync::Arc<Grid> {
        self.tick_clock += 1;
        if let Some(g) = self.grids.get(&gc) {
            self.last_use.insert(gc, self.tick_clock);
            return std::sync::Arc::clone(g);
        }
        let grid = std::sync::Arc::new(self.gen.gen_grid(gc.0, gc.1));
        self.grids.insert(gc, std::sync::Arc::clone(&grid));
        self.last_use.insert(gc, self.tick_clock);
        let grid = self.apply_overrides(gc);
        if self.grids.len() > MAX_CACHED_GRIDS {
            self.evict();
        }
        grid
    }

    /// Re-apply persisted tile overrides for one grid (clone-on-write).
    fn apply_overrides(&mut self, gc: (i32, i32)) -> std::sync::Arc<Grid> {
        let affected: Vec<((i32, i32), u8)> = self
            .overrides
            .iter()
            .filter(|(&(tx, _), _)| tx.div_euclid(100) == gc.0)
            .filter(|(&(_, ty), _)| ty.div_euclid(100) == gc.1)
            .map(|(k, v)| (*k, *v))
            .collect();
        if affected.is_empty() {
            return std::sync::Arc::clone(self.grids.get(&gc).expect("grid present"));
        }
        let old = std::sync::Arc::clone(self.grids.get(&gc).expect("grid present"));
        let mut tiles = *old.tiles;
        for ((tx, ty), tile) in affected {
            let x = tx.rem_euclid(100) as usize;
            let y = ty.rem_euclid(100) as usize;
            tiles[Grid::idx(x, y)] = tile;
        }
        let new_grid = std::sync::Arc::new(Grid {
            gc,
            tiles: Box::new(tiles),
            mnm: old.mnm.clone(),
        });
        self.grids.insert(gc, std::sync::Arc::clone(&new_grid));
        new_grid
    }

    /// Mutate one tile in place (terraforming). Because grids are shared
    /// via Arc, the mutation clones the tile array and reinserts a new Arc;
    /// previous readers keep their consistent snapshot. The override is
    /// recorded so generation replays it after eviction or restart.
    pub fn mutate_tile(
        &mut self,
        gc: (i32, i32),
        x: usize,
        y: usize,
        tile: u8,
    ) -> Option<std::sync::Arc<Grid>> {
        let old = self.grid(gc);
        let mut tiles = *old.tiles;
        tiles[Grid::idx(x, y)] = tile;
        let new_grid = std::sync::Arc::new(Grid {
            gc,
            tiles: Box::new(tiles),
            mnm: old.mnm.clone(),
        });
        self.grids.insert(gc, std::sync::Arc::clone(&new_grid));
        let tx = gc.0 * 100 + x as i32;
        let ty = gc.1 * 100 + y as i32;
        self.overrides.insert((tx, ty), tile);
        Some(new_grid)
    }

    /// Record a remote tile mutation (cluster TileMutation). If the grid
    /// is resident, this is a full `mutate_tile` (live copy + override).
    /// If it is NOT resident, only the override is recorded - never
    /// materialize a grid nobody looks at just to shadow a mutation;
    /// the next `grid(gc)` generation replays the override anyway.
    pub fn note_override_maybe(&mut self, gc: (i32, i32), x: usize, y: usize, tile: u8) {
        if self.grids.contains_key(&gc) {
            self.mutate_tile(gc, x, y, tile);
        } else {
            let tx = gc.0 * 100 + x as i32;
            let ty = gc.1 * 100 + y as i32;
            self.overrides.insert((tx, ty), tile);
        }
    }

    /// True when `gc` is currently resident (used by callers that want to
    /// know whether a mutation touched a live copy or only the overlay).
    pub fn is_resident(&self, gc: (i32, i32)) -> bool {
        self.grids.contains_key(&gc)
    }

    fn evict(&mut self) {
        // Drop the least recently used quarter.
        let mut order: Vec<((i32, i32), u64)> =
            self.last_use.iter().map(|(k, v)| (*k, *v)).collect();
        order.sort_unstable_by_key(|(_, t)| *t);
        let drop = order.len() / 4;
        for (k, _) in order.into_iter().take(drop) {
            self.grids.remove(&k);
            self.last_use.remove(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_reuse_and_mutation() {
        let mut s = GridStore::new(42);
        let a = s.grid((0, 0));
        let b = s.grid((0, 0));
        assert!(std::sync::Arc::ptr_eq(&a, &b));
        let g = s.mutate_tile((0, 0), 0, 0, 9).unwrap();
        assert_eq!(g.tile(0, 0), 9);
    }

    #[test]
    fn note_override_maybe_resident_vs_not() {
        let mut s = GridStore::new(42);
        // The pure generated value for the same seed, read BEFORE any
        // override exists (determinism anchor for the last assertion).
        let pure_val = GridStore::new(42).grid((5, -7)).tile(3, 4);
        // Non-resident grid: only the override is recorded; nothing is
        // materialized and no live copy exists.
        s.note_override_maybe((5, -7), 3, 4, 8);
        assert!(
            !s.is_resident((5, -7)),
            "no materialization on a remote note"
        );
        assert_eq!(s.overrides.get(&(5 * 100 + 3, -7 * 100 + 4)), Some(&8));
        // Materializing later replays the override on top of the pure
        // generation (tile 8 must win over the generated value).
        let g = s.grid((5, -7));
        assert_eq!(g.tile(3, 4), 8);
        assert!(s.is_resident((5, -7)));
        // Resident grid: full mutation - live copy AND override.
        s.note_override_maybe((5, -7), 3, 4, 2);
        assert_eq!(s.grid((5, -7)).tile(3, 4), 2);
        assert_eq!(s.overrides.get(&(5 * 100 + 3, -7 * 100 + 4)), Some(&2));
        // A fresh store with the same seed still regenerates the pure
        // value (overrides never leak into other stores by seed alone).
        assert_eq!(GridStore::new(42).grid((5, -7)).tile(3, 4), pure_val);
    }
}
