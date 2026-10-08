//! Deterministic, seed-fixed world generation.
//!
//! Terrain is derived from multi-octave value noise over the world seed;
//! every grid of 100x100 tiles can be generated independently and
//! identically on any node (horizontal scaling requires no shard
//! coordination for terrain).

/// Tile ids as consumed by the client minimap color table
/// (docs/mechanics/world/map-and-terrain.md).
pub mod tile {
    pub const DEEP_WATER: u8 = 0;
    pub const WATER: u8 = 1;
    pub const STONE_PAVED: u8 = 8;
    pub const PLOWED: u8 = 9;
    pub const CONIFER: u8 = 10;
    pub const BROADLEAF: u8 = 11;
    pub const GRASS: u8 = 13;
    pub const MOOR: u8 = 14;
    pub const HEATH: u8 = 15;
    pub const SWAMP1: u8 = 16;
    pub const DIRT: u8 = 19;
    pub const SAND: u8 = 20;
    pub const CAVE: u8 = 25;
    pub const MOUNTAIN: u8 = 26;
}

/// Tileset resource names bound by RMSG_TILES; indices are tile ids above.
pub const TILESETS: &[(u8, &str, u16)] = &[
    (tile::DEEP_WATER, "gfx/tiles/water/deep", 1),
    (tile::WATER, "gfx/tiles/water", 1),
    (tile::CONIFER, "gfx/tiles/wald/wald", 1),
    (tile::BROADLEAF, "gfx/tiles/wald/leaf", 6),
    (tile::GRASS, "gfx/tiles/grass", 1),
    (tile::MOOR, "gfx/tiles/moor", 1),
    (tile::HEATH, "gfx/tiles/heath", 1),
    (tile::SWAMP1, "gfx/tiles/swamp", 1),
    (tile::DIRT, "gfx/tiles/dirt", 1),
    (tile::SAND, "gfx/tiles/playa", 1),
    (tile::MOUNTAIN, "gfx/tiles/mountain", 1),
    (tile::CAVE, "gfx/tiles/mountain", 1),
];

/// Server-side grid: exactly 10_000 tile bytes, row-major over y then x.
pub struct Grid {
    pub gc: (i32, i32),
    pub tiles: Box<[u8; 10_000]>,
    /// Stable per-grid identity (minimap name on the wire).
    pub mnm: String,
}

impl Grid {
    /// In-grid tile index from in-grid tile coords (x, y in 0..100).
    #[inline]
    pub fn idx(x: usize, y: usize) -> usize {
        y * 100 + x
    }

    pub fn tile(&self, x: usize, y: usize) -> u8 {
        self.tiles[Self::idx(x, y)]
    }
}

/// Deterministic 64-bit integer hash (xorshift-based, splitmix finalizer).
#[inline]
pub fn hash64(mut v: u64) -> u64 {
    v = v.wrapping_add(0x9E3779B97F4A7C15);
    v = (v ^ (v >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94D049BB133111EB);
    v ^ (v >> 31)
}

/// Seeded value-noise sampler. Deterministic across processes and nodes.
pub struct Noise {
    seed: u64,
}

impl Noise {
    pub fn new(seed: u64) -> Self {
        Noise { seed }
    }

    fn corner(&self, cx: i64, cy: i64) -> f64 {
        let h = hash64(hash64(cx as u64 ^ self.seed.rotate_left(17)) ^ (cy as u64).rotate_left(33));
        (h & 0xFFFF_FFFF) as f64 / u32::MAX as f64
    }

    /// Smooth 2D value noise in [0, 1).
    fn value(&self, x: f64, y: f64) -> f64 {
        let x0 = x.floor() as i64;
        let y0 = y.floor() as i64;
        let fx = x - x.floor();
        let fy = y - y.floor();
        let sx = fx * fx * (3.0 - 2.0 * fx);
        let sy = fy * fy * (3.0 - 2.0 * fy);
        let a = self.corner(x0, y0);
        let b = self.corner(x0 + 1, y0);
        let c = self.corner(x0, y0 + 1);
        let d = self.corner(x0 + 1, y0 + 1);
        let top = a + (b - a) * sx;
        let bot = c + (d - c) * sx;
        top + (bot - top) * sy
    }

    /// Fractal Brownian motion over `octaves` octaves, result in [0, 1).
    pub fn fbm(&self, x: f64, y: f64, octaves: u32, lacunarity: f64, gain: f64) -> f64 {
        let mut amp = 1.0;
        let mut freq = 1.0;
        let mut sum = 0.0;
        let mut norm = 0.0;
        for _ in 0..octaves {
            sum += amp * self.value(x * freq, y * freq);
            norm += amp;
            amp *= gain;
            freq *= lacunarity;
        }
        sum / norm
    }
}

/// The world generator: one fixed seed, reproducible forever.
pub struct WorldGen {
    pub seed: u64,
    elevation: Noise,
    moisture: Noise,
    scatter: Noise,
}

impl WorldGen {
    pub fn new(seed: u64) -> Self {
        WorldGen {
            seed,
            elevation: Noise::new(seed),
            moisture: Noise::new(seed ^ 0xA5A5_5A5A_1234_5678),
            scatter: Noise::new(seed ^ 0xDEAD_BEEF_CAFE_F00D),
        }
    }

    /// Absolute tile -> tile id. Pure function of (seed, x, y).
    pub fn tile_at(&self, tx: i32, ty: i32) -> u8 {
        let fx = tx as f64;
        let fy = ty as f64;
        // Continental elevation: big landmasses with coastlines.
        let e = self.elevation.fbm(fx / 220.0, fy / 220.0, 5, 2.0, 0.55);
        // Local roughness for beaches and lakes.
        let r = self.scatter.fbm(fx / 24.0, fy / 24.0, 3, 2.1, 0.5);
        let m = self.moisture.fbm(fx / 90.0, fy / 90.0, 4, 2.0, 0.5);

        if e < 0.38 + r * 0.02 {
            return if e < 0.335 {
                tile::DEEP_WATER
            } else {
                tile::WATER
            };
        }
        if e > 0.78 {
            return if e > 0.83 { tile::MOUNTAIN } else { tile::CAVE };
        }
        // Coastline sand belt.
        if e < 0.405 {
            return tile::SAND;
        }
        if m < 0.34 {
            if m < 0.26 {
                return tile::DIRT;
            }
            return tile::HEATH;
        }
        if m > 0.72 {
            if e < 0.44 {
                return tile::SWAMP1;
            }
            return if self.scatter.fbm(fx / 9.0, fy / 9.0, 2, 2.0, 0.5) > 0.52 {
                tile::CONIFER
            } else {
                tile::BROADLEAF
            };
        }
        if m > 0.58 {
            return if self.scatter.fbm(fx / 11.0, fy / 11.0, 2, 2.0, 0.5) > 0.55 {
                tile::MOOR
            } else {
                tile::GRASS
            };
        }
        tile::GRASS
    }

    /// Nearest tile satisfying `pred` within a square ring scan around
    /// (cx, cy). Scans expanding square rings (Chebyshev radius 0, 1, 2,
    /// ...) up to `max_radius` and returns the first hit with its radius.
    /// Search aid for world-design assertions and dev tooling: it answers
    /// "how far is X from spawn on this seed" without booting a server.
    pub fn find_tile(
        &self,
        cx: i32,
        cy: i32,
        max_radius: i32,
        pred: impl Fn(u8) -> bool,
    ) -> Option<((i32, i32), i32)> {
        if pred(self.tile_at(cx, cy)) {
            return Some(((cx, cy), 0));
        }
        for r in 1..=max_radius {
            for dy in -r..=r {
                for dx in -r..=r {
                    // Ring cells only: the inner square was scanned by the
                    // previous iterations.
                    if dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let (x, y) = (cx + dx, cy + dy);
                    if pred(self.tile_at(x, y)) {
                        return Some(((x, y), r));
                    }
                }
            }
        }
        None
    }

    /// Generate one 100x100 grid from absolute grid coordinate.
    pub fn gen_grid(&self, gx: i32, gy: i32) -> Grid {
        let mut tiles = Box::new([0u8; 10_000]);
        for y in 0..100usize {
            let ty = gy as i64 * 100 + y as i64;
            for x in 0..100usize {
                let tx = gx as i64 * 100 + x as i64;
                tiles[Grid::idx(x, y)] = self.tile_at(tx as i32, ty as i32);
            }
        }
        let mnm = format!(
            "{:016x}",
            hash64(self.seed ^ hash64((gx as u64) << 32) ^ (gy as u64))
        );
        Grid {
            gc: (gx, gy),
            tiles,
            mnm,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grids_are_deterministic() {
        let g1 = WorldGen::new(42).gen_grid(3, -7);
        let g2 = WorldGen::new(42).gen_grid(3, -7);
        assert_eq!(g1.tiles.as_slice(), g2.tiles.as_slice());
        assert_eq!(g1.mnm, g2.mnm);
    }

    #[test]
    fn world_has_variety() {
        let w = WorldGen::new(42);
        let grid = w.gen_grid(0, 0);
        let mut counts = std::collections::HashMap::new();
        for t in grid.tiles.iter() {
            *counts.entry(*t).or_insert(0) += 1;
        }
        assert!(counts.len() >= 4, "world too uniform: {counts:?}");
    }

    #[test]
    fn coordinates_seamless_across_origin() {
        // div/rem semantics must be Euclidean across the origin.
        let w = WorldGen::new(42);
        let _ = w.tile_at(-1, -1);
        let _ = w.tile_at(-101, 199);
    }

    /// World-design assertion behind the metal chain: rocky terrain
    /// (mountain or cave tiles - the ore-bearing ground) must exist within
    /// walking range of the spawn area on the dev seed, or ore deposits
    /// would be unreachable and the smelter chain unplayable. Prints the
    /// measured distance for world-design review.
    #[test]
    fn rocky_terrain_is_reachable_from_spawn() {
        let w = WorldGen::new(42);
        let rocky = |t: u8| t == tile::MOUNTAIN || t == tile::CAVE;
        let ((x, y), r) = w
            .find_tile(50, 50, 400, rocky)
            .expect("no rocky terrain within 400 tiles of spawn on seed 42");
        assert!(r <= 250, "rocky terrain too far from spawn: {r} tiles");
        println!("nearest rocky tile on seed 42: ({x}, {y}), {r} tiles from (50, 50)");
        // Belt size report: rocky-tile count in the 61x61 window around the
        // nearest hit. Drives the ore-deposit spawn roll (a sparse belt
        // needs a higher per-tile roll to stay playable).
        let mut belt = 0usize;
        for ty in y - 30..=y + 30 {
            for tx in x - 30..=x + 30 {
                if rocky(w.tile_at(tx, ty)) {
                    belt += 1;
                }
            }
        }
        println!("rocky tiles in the 61x61 belt window: {belt}");
    }
}
