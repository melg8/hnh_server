//! A minimal FxHash-style hasher for server-internal id containers.
//!
//! The default `SipHash` hasher costs a full hash pass with two mixing
//! rounds per 8-byte chunk plus a random-seeded setup. On the visibility
//! hot path that is hundreds of thousands of lookups per tick (session
//! `visible` membership, VisIndex cell buckets), where hashing a small
//! integer key is pure overhead.
//!
//! This hasher is the well-known FxHash construction (multiply-rotate
//! per chunk, fixed seed). It is NOT HashDoS-resistant and MUST only be
//! used for keys the server itself generates or computes:
//! - `GobId` slot ids (server-allocated, generational);
//! - VisIndex cell coordinates (derived from server-side positions);
//! - frame counters and other internal bookkeeping keys.
//!
//! Never key a container with attacker-controlled bytes (usernames,
//! resource names, raw client strings) with this hasher.

use std::hash::{BuildHasherDefault, Hasher};

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

/// Multiply-rotate hasher (FxHash construction, fixed seed).
#[derive(Default, Clone)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.hash = (self.hash.rotate_left(5) ^ w).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        // Hasher's required fallback for byte slices: fold 8-byte chunks,
        // then the remainder. Only reached through `Hash::hash_slice`
        // paths; the hot keys below use the numeric `write_*` methods.
        let (chunks, rem) = bytes.as_chunks::<8>();
        for c in chunks {
            self.add(u64::from_le_bytes(*c));
        }
        if !rem.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rem.len()].copy_from_slice(rem);
            self.add(u64::from_le_bytes(buf));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64);
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(i as u64);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
}

/// `BuildHasher` for [`FxHasher`].
pub type FxBuild = BuildHasherDefault<FxHasher>;

/// `HashMap` keyed by server-internal ids (see the module docs: never
/// use for attacker-controlled keys).
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, FxBuild>;

/// `HashSet` of server-internal ids (same restriction).
pub type FxHashSet<T> = std::collections::HashSet<T, FxBuild>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::hash::Hash;

    #[test]
    fn same_key_same_hash() {
        let mut h1 = FxHasher::default();
        let mut h2 = FxHasher::default();
        42i32.hash(&mut h1);
        42i32.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());
    }

    #[test]
    fn distinct_keys_distinct_hash() {
        // Small-key sensitivity: (x, y) cell keys must not collide in a
        // dense neighbourhood - the VisIndex relies on cheap exactness.
        let mut seen = HashSet::new();
        for x in -40..40 {
            for y in -40..40 {
                let mut h = FxHasher::default();
                (x, y).hash(&mut h);
                assert!(seen.insert(h.finish()), "collision at {x},{y}");
            }
        }
    }

    #[test]
    fn write_bytes_path_agrees_with_chunking() {
        // The byte-slice fallback must consume every byte (used by
        // string-keyed types if anyone ever routes them here).
        let mut h = FxHasher::default();
        b"hello world".hash(&mut h);
        let a = h.finish();
        let mut h2 = FxHasher::default();
        b"hello world!".hash(&mut h2);
        assert_ne!(a, h2.finish());
    }
}
