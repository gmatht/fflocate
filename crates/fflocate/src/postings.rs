//! Compressed posting sets (§10/§17 v2).
//!
//! The trigram accelerator's posting lists are sets of entry ids: ascending,
//! deduplicated, append-mostly, intersected on the query path. This module is
//! the *only* place that names the compression backend (`roaring`): everything
//! else — `trigram.rs`, `query.rs`, the applier's incremental maintenance —
//! speaks to [`PostingSet`], whose method names deliberately mirror the `Vec<u32>`
//! it replaces (`insert`/`remove`/`contains`/`len`/`iter`).
//!
//! Why the isolation matters: a dropped bit in an intersection is a silently
//! missing file, violating the filter-only superset contract. If the backend
//! ever misbehaves, the fallback is a 3-line change (`PostingSet(Vec<u32>)`
//! with the same method names) and no call site moves. The differential suite
//! (`tests/trigram_differential.rs`) pins the contract either way.
//!
//! Only compiled with the default-on `trigram` cargo feature, like `trigram.rs`
//! itself.

/// An ascending, deduplicated set of entry ids — one trigram's posting list.
///
/// Semantics match the `Vec<u32>` it replaces:
/// * iteration is ascending (result order identity with the linear scan);
/// * `insert` is idempotent (replay/revive re-indexing is a no-op, not a duplicate);
/// * out-of-order inserts are accepted (replayed `Add` below posted ids).
#[derive(Clone, Debug, Default)]
pub struct PostingSet(roaring::RoaringBitmap);

impl PostingSet {
    /// An empty set.
    pub fn new() -> Self {
        Self(roaring::RoaringBitmap::new())
    }

    /// Insert `id`. Returns `true` when the set grew, `false` when `id` was
    /// already present (the replay/revive idempotence case).
    pub fn insert(&mut self, id: u32) -> bool {
        self.0.insert(id)
    }

    /// Remove `id`. Returns `true` when something was removed.
    pub fn remove(&mut self, id: u32) -> bool {
        self.0.remove(id)
    }

    /// Membership test.
    pub fn contains(&self, id: u32) -> bool {
        self.0.contains(id)
    }

    /// Number of ids in the set.
    pub fn len(&self) -> u64 {
        self.0.len()
    }

    /// True when the set holds no ids.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Ascending iteration over the set's ids.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.0.iter()
    }

    /// Collect the set into an ascending `Vec<u32>` (candidate lists handed to
    /// the query engines, which still consume `Vec<u32>`).
    pub fn to_vec(&self) -> Vec<u32> {
        self.0.iter().collect()
    }

    /// Intersection of two sets, ascending. Used for the per-literal AND in
    /// `TrigramIndex::candidates`: a name must contain *every* trigram of the
    /// literal, so the candidate set is the AND of the posting lists.
    pub fn intersect(&self, other: &Self) -> Self {
        Self(&self.0 & &other.0)
    }

    /// Serialized size in bytes — the memory-accounting input for
    /// `TrigramIndex::approx_bytes`. This is what the set actually costs
    /// (containers + ids), not `len * 4`.
    pub fn serialized_bytes(&self) -> usize {
        self.0.serialized_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_is_idempotent_and_ascending() {
        let mut s = PostingSet::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(s.insert(5));
        assert!(s.insert(3));
        assert!(!s.insert(5), "re-insert must not duplicate");
        assert_eq!(s.len(), 2);
        assert!(s.contains(3));
        assert!(!s.contains(4));
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![3, 5]);
        assert_eq!(s.to_vec(), vec![3, 5]);
    }

    #[test]
    fn remove_prunes() {
        let mut s = PostingSet::new();
        s.insert(1);
        s.insert(2);
        assert!(s.remove(1));
        assert!(!s.remove(1));
        assert_eq!(s.to_vec(), vec![2]);
        assert!(s.remove(2));
        assert!(s.is_empty());
    }

    #[test]
    fn intersect_keeps_common_ids_ascending() {
        let mut a = PostingSet::new();
        let mut b = PostingSet::new();
        for id in [1u32, 3, 5, 7, 9] {
            a.insert(id);
        }
        for id in [2u32, 3, 4, 7, 10] {
            b.insert(id);
        }
        assert_eq!(a.intersect(&b).to_vec(), vec![3, 7]);
        assert!(a.intersect(&PostingSet::new()).is_empty());
    }
}
