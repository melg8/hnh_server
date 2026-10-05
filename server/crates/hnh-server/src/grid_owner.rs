//! Grid-owner partitioning (the multi-node 10k-target layout).
//!
//! The world partitions into the VisIndex cell lattice (see `visidx.rs`).
//! Every cell is owned by exactly one node, chosen by rendezvous
//! (highest-random-weight) hashing:
//!
//! - deterministic: the same cell + node set always picks the same owner,
//!   on every process, with no shared state;
//! - minimal migration on scale-out: joining a node moves only the cells
//!   that the new node wins (~1/(N+1) of the lattice), never reshuffling
//!   the rest — cells do not migrate between the existing nodes;
//! - no ring bookkeeping (unlike consistent hashing with virtual nodes):
//!   the owner is `argmax_i hash(cell, i)` over live nodes only.
//!
//! Inside one process the same partition drives the data-parallel tick:
//! work items (animal intent candidates, session visibility scans) are
//! grouped by their owner cell so each rayon task processes one
//! partition's slice of the world. That group is the exact unit a
//! separate node process would own in the multi-node deployment, so the
//! tick fan-out exercises the partitioning contract the cluster mode will
//! rely on.

use std::num::NonZeroUsize;

/// Owning node index `0..nodes` for one VisIndex cell. Rendezvous
/// hashing: score every node for this cell, take the highest. Scores mix
/// the cell coordinates with the node index through splitmix-style
/// avalanching so neighbouring cells scatter across nodes.
pub fn owner_of(cell: (i32, i32), nodes: NonZeroUsize) -> usize {
    let n = nodes.get();
    let mut best_node = 0usize;
    let mut best_score = u64::MIN;
    for node in 0..n {
        let score = node_score(cell, node);
        // Strict > keeps the lowest-index node on ties (deterministic).
        if score > best_score || node == 0 {
            best_score = score;
            best_node = node;
        }
    }
    best_node
}

/// Rendezvous score of one node for one cell: a 64-bit mix of the packed
/// cell key and the node index. Wrapping arithmetic keeps it total.
#[inline]
pub fn node_score(cell: (i32, i32), node: usize) -> u64 {
    // Pack both coordinates into 64 bits (i32::MAX fits in 31 bits each;
    // the cast is lossless for the xor-mix purpose even for negatives).
    let mut h = (cell.0 as i64 as u64).wrapping_mul(0x9E3779B97F4A7C15)
        ^ (cell.1 as i64 as u64).wrapping_mul(0xC2B2AE3D27D4EB4F)
        ^ (node as u64).wrapping_mul(0x165667B19E3779F9);
    // splitmix64 finalizer: avalanche the bits so that neighbouring
    // cells and node indices produce unrelated scores.
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58476D1CE4E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D049BB133111EB);
    h ^= h >> 31;
    h
}

/// Group items into per-owner partitions: `out[i]` holds the items whose
/// owner cell belongs to node `i`. Order inside a partition preserves the
/// input order (the tick's apply phase is order-sensitive per item only).
pub fn partition_by_owner<K, I>(
    cells: impl Fn(&K) -> (i32, i32),
    items: I,
    nodes: NonZeroUsize,
) -> Vec<Vec<K>>
where
    K: Clone,
    I: IntoIterator<Item = K>,
{
    let n = nodes.get();
    let mut parts: Vec<Vec<K>> = (0..n).map(|_| Vec::new()).collect();
    for item in items {
        let owner = owner_of(cells(&item), nodes);
        parts[owner].push(item);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nz(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).expect("nonzero test constant")
    }

    /// The full lattice over a test region, one cell per lattice point.
    fn lattice(range: std::ops::Range<i32>) -> Vec<(i32, i32)> {
        let mut cells = Vec::new();
        for cx in range.clone() {
            for cy in range.clone() {
                cells.push((cx, cy));
            }
        }
        cells
    }

    #[test]
    fn owner_is_deterministic_and_in_range() {
        for n in [1usize, 2, 3, 4, 7, 16] {
            let nodes = nz(n);
            for cell in lattice(-8..8) {
                let o = owner_of(cell, nodes);
                assert!(o < n, "owner {o} out of range for {n} nodes");
                assert_eq!(o, owner_of(cell, nodes), "owner must be stable");
            }
        }
    }

    #[test]
    fn one_node_owns_everything() {
        for cell in lattice(-4..4) {
            assert_eq!(owner_of(cell, nz(1)), 0);
        }
    }

    #[test]
    fn scale_out_migrates_only_the_new_nodes_share() {
        // Rendezvous guarantee: when a node joins, a cell either keeps its
        // old owner (if the old node still wins) or moves to the NEW node
        // (only the new node can beat the incumbent for that cell).
        let cells = lattice(-16..16); // 1024 cells
        let four = nz(4);
        let five = nz(5);
        let mut moved = 0usize;
        for cell in &cells {
            let o4 = owner_of(*cell, four);
            let o5 = owner_of(*cell, five);
            if o5 != o4 {
                moved += 1;
                assert_eq!(o5, 4, "cell moved to an existing node, not the joiner");
            }
        }
        // ~1/5 of the lattice moves; a wide margin keeps this robust
        // without making the test meaningless (0.05x..0.35x).
        let share = moved as f64 / cells.len() as f64;
        assert!(
            (0.05..=0.35).contains(&share),
            "migration share {share:.3} outside the rendezvous band"
        );
    }

    #[test]
    fn lattice_spreads_across_nodes() {
        let cells = lattice(-16..16);
        for n in [2usize, 4, 8] {
            let mut counts = vec![0usize; n];
            for cell in &cells {
                counts[owner_of(*cell, nz(n))] += 1;
            }
            let total = cells.len();
            // No node may own nothing, none may own everything; the
            // deviating node holds no more than 2x the fair share.
            for (i, c) in counts.iter().enumerate() {
                assert!(*c > 0, "node {i} owns nothing at {n} nodes");
                assert!(*c < total, "node {i} owns everything at {n} nodes");
                assert!(
                    *c as f64 <= 2.0 * total as f64 / n as f64,
                    "node {i} owns {c}/{total} at {n} nodes (> 2x fair share)"
                );
            }
        }
    }

    #[test]
    fn partition_covers_every_item_exactly_once() {
        let items: Vec<(i32, i32)> = lattice(-12..12);
        let cells = |c: &(i32, i32)| *c;
        let parts = partition_by_owner(cells, items.iter().cloned(), nz(3));
        assert_eq!(parts.len(), 3);
        let mut seen: Vec<(i32, i32)> = parts.iter().flatten().copied().collect();
        assert_eq!(
            seen.len(),
            items.len(),
            "partition lost or duplicated items"
        );
        seen.sort();
        let mut want = items;
        want.sort();
        assert_eq!(seen, want);
        // Every partition entry belongs to its partition's owner.
        for (node, part) in parts.iter().enumerate() {
            for cell in part {
                assert_eq!(owner_of(*cell, nz(3)), node, "item in a foreign partition");
            }
        }
    }

    #[test]
    fn partition_preserves_input_order_within_a_partition() {
        // Indexed items: the input rank travels with the cell, so a
        // partition's ranks must come out strictly ascending.
        let items: Vec<((i32, i32), usize)> = vec![
            ((0, 0), 0),
            ((1, 1), 1),
            ((2, 2), 2),
            ((3, 3), 3),
            ((-5, 7), 4),
            ((100, -100), 5),
        ];
        let parts = partition_by_owner(|it| it.0, items, nz(2));
        for part in &parts {
            let mut ranks: Vec<usize> = part.iter().map(|(_, r)| *r).collect();
            let ascending = ranks.windows(2).all(|w| w[0] < w[1]);
            assert!(ascending, "partition ranks not ascending: {ranks:?}");
            ranks.dedup();
            assert_eq!(ranks.len(), part.len(), "duplicate rank in a partition");
        }
    }
}
