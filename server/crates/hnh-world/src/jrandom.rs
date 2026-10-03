//! Bit-exact port of `java.util.Random` (48-bit LCG), required because the
//! legacy protocol derives flavor objects from this exact PRNG seeded by
//! absolute tile coordinates (docs/mechanics/world/map-and-terrain.md,
//! `MCache.mkrandoom`).

const MULTIPLIER: u64 = 0x5DEECE66D;
const ADDEND: u64 = 0xB;
const MASK: u64 = (1 << 48) - 1;

#[derive(Debug, Clone)]
pub struct JavaRandom {
    seed: u64,
}

impl JavaRandom {
    #[inline]
    pub fn new(seed: i64) -> Self {
        let mut r = JavaRandom { seed: 0 };
        r.set_seed(seed);
        r
    }

    #[inline]
    pub fn set_seed(&mut self, seed: i64) {
        self.seed = (seed as u64 ^ MULTIPLIER) & MASK;
    }

    #[inline]
    fn next(&mut self, bits: u32) -> i32 {
        self.seed = (self.seed.wrapping_mul(MULTIPLIER).wrapping_add(ADDEND)) & MASK;
        (self.seed >> (48 - bits)) as i32
    }

    /// `java.util.Random.nextInt()`: 32 random bits.
    #[inline]
    pub fn next_int(&mut self) -> i32 {
        self.next(32)
    }

    /// `java.util.Random.nextInt(bound)` for bound > 0 (the non-rejection
    /// power-of-two fast path plus the modulo path, exactly like the JDK).
    #[inline]
    pub fn next_bounded(&mut self, bound: i32) -> i32 {
        debug_assert!(bound > 0, "BUG: bound must be positive");
        if bound & (bound - 1) == 0 {
            (((bound as i64) * (self.next(31) as i64)) >> 31) as i32
        } else {
            loop {
                let bits = self.next(31);
                let val = bits % bound;
                if bits.wrapping_sub(val).wrapping_add(bound - 1) >= 0 {
                    return val;
                }
            }
        }
    }
}

/// `MCache.mkrandoom(c)`: `r.setSeed(c.x); r.setSeed(r.nextInt() ^ c.y);`
/// seeded from the ABSOLUTE tile coordinate.
#[inline]
pub fn mkrandoom(tx: i32, ty: i32) -> JavaRandom {
    let mut r = JavaRandom::new(0);
    r.set_seed(tx as i64);
    let n = r.next_int();
    r.set_seed((n as i64) ^ (ty as i64));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference values verified against a real JDK 21
    /// (`new Random(42).nextInt()` sequence, single-file source launch).
    #[test]
    fn matches_jdk_reference_values() {
        let mut r = JavaRandom::new(42);
        assert_eq!(r.next_int(), -1170105035);
        assert_eq!(r.next_int(), 234785527);
        assert_eq!(r.next_int(), -1360544799);
    }

    /// mkrandoom(123, 456).nextInt() == -238732510 per the same JDK run.
    #[test]
    fn mkrandoom_matches_jdk() {
        let mut r = mkrandoom(123, 456);
        assert_eq!(r.next_int(), -238732510);
    }

    #[test]
    fn next_bounded_range() {
        let mut r = JavaRandom::new(7);
        for _ in 0..1000 {
            let v = r.next_bounded(100);
            assert!((0..100).contains(&v));
        }
    }

    #[test]
    fn mkrandoom_deterministic() {
        let a = {
            let mut r = mkrandoom(123, 456);
            r.next_int()
        };
        let b = {
            let mut r = mkrandoom(123, 456);
            r.next_int()
        };
        assert_eq!(a, b);
    }
}
