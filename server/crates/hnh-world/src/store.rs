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
}

impl GridStore {
    pub fn new(seed: u64) -> Self {
        GridStore {
            gen: WorldGen::new(seed),
            grids: HashMap::new(),
            tick_clock: 0,
            last_use: HashMap::new(),
        }
    }

    pub fn seed(&self) -> u64 {
        self.gen.seed
    }

    /// Get-or-generate a grid. Generation is deterministic, so any node
    /// produces identical bytes for the same key (grid-shard ready).
    pub fn grid(&mut self, gc: (i32, i32)) -> std::sync::Arc<Grid> {
        self.tick_clock += 1;
        if let Some(g) = self.grids.get(&gc) {
            self.last_use.insert(gc, self.tick_clock);
            return std::sync::Arc::clone(g);
        }
        let grid = std::sync::Arc::new(self.gen.gen_grid(gc.0, gc.1));
        self.grids.insert(gc, std::sync::Arc::clone(&grid));
        self.last_use.insert(gc, self.tick_clock);
        if self.grids.len() > MAX_CACHED_GRIDS {
            self.evict();
        }
        grid
    }

    /// Mutate one tile in place (terraforming). Because grids are shared
    /// via Arc, the mutation clones the tile array and reinserts a new Arc;
    /// previous readers keep their consistent snapshot.
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
        Some(new_grid)
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
}
