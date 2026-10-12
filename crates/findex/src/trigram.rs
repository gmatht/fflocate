//! Trigram posting-list accelerator (§10, §17 v2).
//!
//! plocate's idea, adapted to findex's in-memory index: build a posting list
//! per **name trigram** and use it to cut the linear scan of [`crate::query`]
//! down to the entries that can possibly match.
//!
//! ```text
//! trigram  ->  sorted [entry id, …]
//! "rs/"    ->  [12, 91, 4003, …]
//! ```
//!
//! Trigrams are taken over the **ASCII-lowercased** bytes of each live entry's
//! own name. Names shorter than three bytes contribute nothing (they cannot be
//! constrained by a trigram).
//!
//! ## Correctness contract
//!
//! This is a **filter, never an authority**. [`TrigramIndex::candidates`] may
//! only ever *exclude* entries that provably cannot match; the caller must
//! still run the real matcher ([`crate::query::Query::matches`]) over every
//! candidate. A literal shorter than three bytes, a smart-case term, a glob, a
//! regex and a `path:` term all carry no usable trigram constraint, so they are
//! simply not applied here and fall through to the authoritative check. The
//! differential tests in `tests/trigram_differential.rs` pin this: accelerated
//! and unaccelerated queries return byte-identical results.
//!
//! ## Why a name index (and not a path index)
//!
//! findex stores no path strings (§3) — a path is materialized on demand by
//! walking the parent chain. Indexing *names* keeps the accelerator independent
//! of the tree shape and makes directory renames free, at the cost of `path:`
//! terms not being accelerated (they still work, just at scan speed). This
//! mirrors the design's "linear scan is enough; postings are an accelerator"
//! stance.

use std::collections::HashMap;

use crate::index::Index;

/// One posting list per lowercased name trigram, ids ascending.
///
/// Ids are the index's entry ids, which are stable across rescans (§3), so a
/// posting list stays meaningful as long as the generation it was built for is
/// current.
#[derive(Clone, Debug, Default)]
pub struct TrigramIndex {
    /// trigram (3 lowercased bytes) → sorted, deduplicated entry ids.
    postings: HashMap<[u8; 3], Vec<u32>>,
    /// Number of live, non-root entries that were indexed.
    indexed: u64,
    /// Total ids across all posting lists (for memory reporting).
    total_postings: u64,
}

/// The minimal form of a literal as far as the accelerator cares.
///
/// A caller that wants acceleration for a substring builds this with
/// [`TrigramIndex::literal_trigrams`]; the intersection over all of a query's
/// literals is what [`TrigramIndex::candidates`] consumes.
pub struct TrigramPlan {
    /// For each required literal, the *distinct* trigrams it contains. The
    /// candidate name must contain every one of them.
    pub literals: Vec<Vec<[u8; 3]>>,
}

impl TrigramPlan {
    /// True when the plan constrains nothing (no literal long enough for a
    /// trigram) — the caller should just scan.
    pub fn is_empty(&self) -> bool {
        self.literals.is_empty()
    }
}

impl TrigramIndex {
    /// Build the accelerator over every live, non-root entry of `index`.
    ///
    /// Cost is O(total name bytes). Callers cache the result keyed by the index
    /// generation, and the daemon warms it in the background at startup so the
    /// first *user* query does not pay for it.
    ///
    /// Measured on a 325 k-entry index: a single pass with a reused lowercase
    /// buffer. A two-pass "count then allocate at final size" version was tried
    /// and was **slower** (1.3–1.8 s against 0.55–1.1 s), because the second walk
    /// over every name and the extra `HashMap` churn cost more than the
    /// reallocations it saved.
    pub fn build(index: &Index) -> TrigramIndex {
        let mut postings: HashMap<[u8; 3], Vec<u32>> = HashMap::new();
        let mut indexed = 0u64;
        let mut total_postings = 0u64;
        // One buffer for the whole build: allocating a `Vec` per entry was the
        // single biggest cost in the first version.
        let mut lower: Vec<u8> = Vec::with_capacity(256);

        for (id, entry) in index.iter_live() {
            // The root dir is never a search hit (§10), so it is never a
            // candidate either.
            if entry.flags().is_root_dir() {
                continue;
            }
            indexed += 1;
            let name = index.name(id);
            if name.len() < 3 {
                continue;
            }
            // Fold case in place into the reusable buffer.
            lower.clear();
            lower.extend(name.iter().map(|b| b.to_ascii_lowercase()));
            for w in lower.windows(3) {
                let tri = [w[0], w[1], w[2]];
                let list = postings.entry(tri).or_default();
                // Names can repeat a trigram; keep ids sorted+deduped. Ids ascend,
                // so a repeat within this name is the only duplicate possible and
                // `last()` catches it.
                if list.last() != Some(&id) {
                    list.push(id);
                    total_postings += 1;
                }
            }
        }

        TrigramIndex {
            postings,
            indexed,
            total_postings,
        }
    }

    /// Index one entry's name into existing postings (incremental maintenance).
    ///
    /// This is the per-entry half of [`TrigramIndex::build`], factored so the
    /// applier can keep a cached accelerator current as entries arrive instead
    /// of rebuilding over the whole index. The caller supplies the entry id
    /// and its name bytes; ids must ascend per posting list for the
    /// sorted+deduped invariant to hold.
    ///
    /// That holds in practice: live `Add` records always carry fresh ids from
    /// `Index::insert` (`next_id` strictly grows, never reused), while
    /// `apply_add` replays and `revive` restore an id under its *original*
    /// name — whose postings already name that id, so the per-trigram
    /// `last() != id` check makes the re-insertion a no-op rather than a
    /// duplicate. Root dirs and sub-3-byte names contribute nothing, exactly
    /// as in `build`.
    ///
    /// Returns the number of (trigram, id) postings added.
    pub fn index_entry(&mut self, id: u32, name: &[u8], is_root_dir: bool) -> u64 {
        if is_root_dir {
            return 0;
        }
        // Counted as indexed whether or not the name yields trigrams, exactly
        // as `build` counts every live non-root entry it visits.
        self.indexed += 1;
        if name.len() < 3 {
            return 0;
        }
        let lower: Vec<u8> = name.iter().map(|b| b.to_ascii_lowercase()).collect();
        let mut added = 0u64;
        // Distinct trigrams only: a repeat within one name must not push twice.
        let mut prev: Option<[u8; 3]> = None;
        let mut tris: Vec<[u8; 3]> = lower.windows(3).map(|w| [w[0], w[1], w[2]]).collect();
        tris.sort_unstable();
        for tri in tris {
            if prev == Some(tri) {
                continue;
            }
            prev = Some(tri);
            let list = self.postings.entry(tri).or_default();
            // Ids ascend within a generation (fresh ids always exceed every
            // id already posted), so `last()` decides membership: equal means
            // "already indexed under this name" (replay/revive idempotence),
            // and anything else means "append".
            if list.last() != Some(&id) {
                // Defensive: a replayed `apply_add` can reuse an id *below*
                // ids posted since (pad-forward slots). A blind push would
                // break the ascending invariant that `candidates` intersects
                // on, so insert in order instead. This path is rare (replay
                // only) and short lists dominate.
                if list.last().is_some_and(|&last| last > id) {
                    match list.binary_search(&id) {
                        Ok(_) => {}
                        Err(pos) => {
                            list.insert(pos, id);
                            added += 1;
                            self.total_postings += 1;
                        }
                    }
                } else {
                    list.push(id);
                    added += 1;
                    self.total_postings += 1;
                }
            }
        }
        added
    }

    /// Forget one entry's postings (incremental maintenance for deletions).
    ///
    /// Removal is best-effort pruning, not correctness: tombstoned ids are
    /// already filtered by the authoritative matcher (`is_alive`), so a stale
    /// posting only ever adds a wasted candidate. This keeps lists from
    /// growing without bound on churn-heavy indexes; it removes the id from
    /// the posting lists of its *current* name's trigrams and drops lists
    /// left empty. Returns the number of postings removed.
    pub fn remove_entry(&mut self, id: u32, name: &[u8]) -> u64 {
        if name.len() < 3 {
            return 0;
        }
        let lower: Vec<u8> = name.iter().map(|b| b.to_ascii_lowercase()).collect();
        let mut removed = 0u64;
        let mut prev: Option<[u8; 3]> = None;
        let mut tris: Vec<[u8; 3]> = lower.windows(3).map(|w| [w[0], w[1], w[2]]).collect();
        tris.sort_unstable();
        let mut empty: Vec<[u8; 3]> = Vec::new();
        for tri in tris {
            if prev == Some(tri) {
                continue;
            }
            prev = Some(tri);
            if let Some(list) = self.postings.get_mut(&tri) {
                let before = list.len();
                list.retain(|&x| x != id);
                let gone = (before - list.len()) as u64;
                removed += gone;
                self.total_postings -= gone;
                if list.is_empty() {
                    empty.push(tri);
                }
            }
        }
        for tri in empty {
            self.postings.remove(&tri);
        }
        // Keep `indexed` (live non-root entries visited) exact for add/remove
        // cycles: an entry counted by `index_entry` un-counts here. Entries
        // whose names post nothing (sub-3-byte) were still counted, so they
        // decrement too. This over-counts by one per rename/replayed-Add
        // (same id indexed twice without an intervening remove) — rare, and
        // documented rather than tracked with a per-id set. `indexed` is
        // diagnostic only; answers never depend on it.
        if removed > 0 || name.len() < 3 {
            self.indexed = self.indexed.saturating_sub(1);
        }
        removed
    }

    /// The distinct trigrams in a literal, as a query would see them.
    ///
    /// Returns `None` when the literal cannot be constrained by trigrams (fewer
    /// than three bytes) — the caller then skips this literal entirely rather
    /// than filtering on nothing. `case` selects whether the literal is
    /// lowercased first; a case-sensitive literal cannot use the (lowercased)
    /// index safely, so it too returns `None`.
    pub fn literal_trigrams(literal: &[u8], case_sensitive: bool) -> Option<Vec<[u8; 3]>> {
        if case_sensitive {
            // The index stores lowercased names only; a case-sensitive literal
            // has no exact-case posting list to consult.
            return None;
        }
        if literal.len() < 3 {
            return None;
        }
        let lower: Vec<u8> = literal.iter().map(|b| b.to_ascii_lowercase()).collect();
        let mut tris: Vec<[u8; 3]> = lower.windows(3).map(|w| [w[0], w[1], w[2]]).collect();
        tris.sort_unstable();
        tris.dedup();
        Some(tris)
    }

    /// Build a plan from a query's required literals: `(needle, case)` pairs.
    ///
    /// Literals that carry no trigram constraint are dropped. If none survive,
    /// the plan is empty and the caller scans as before.
    pub fn plan_from_literals<'a, I>(literals: I) -> TrigramPlan
    where
        I: IntoIterator<Item = (&'a [u8], bool)>,
    {
        let mut out = Vec::new();
        for (needle, case) in literals {
            if let Some(tris) = Self::literal_trigrams(needle, case) {
                out.push(tris);
            }
        }
        TrigramPlan { literals: out }
    }

    /// Candidate ids that may match a plan, ascending.
    ///
    /// A candidate must contain **every** trigram of **every** planned literal,
    /// so we intersect. We pick the shortest posting list per literal first
    /// (the most selective), then intersect across literals. The result is a
    /// superset of the true matches — always a filter, never an authority.
    ///
    /// Returns `None` when the plan is empty or the intersection is empty: the
    /// former means "no constraint, scan everything", the latter means "zero
    /// candidates". Both are handled by the caller; use [`Self::postings`] to
    /// tell them apart if needed.
    pub fn candidates(&self, plan: &TrigramPlan) -> Option<Vec<u32>> {
        if plan.is_empty() {
            return None;
        }
        let mut acc: Option<Vec<u32>> = None;
        for tris in &plan.literals {
            // Intersect this literal's trigram postings. Start from the
            // shortest list to keep the intermediate small.
            let mut lit: Option<Vec<u32>> = None;
            for tri in tris {
                let list = match self.postings.get(tri) {
                    // A trigram absent from the whole index can match nothing:
                    // the literal is unsatisfiable, so the query is empty.
                    None => return Some(Vec::new()),
                    Some(l) => l,
                };
                lit = Some(match lit {
                    None => list.clone(),
                    Some(prev) => intersect(&prev, list),
                });
                if lit.as_ref().is_some_and(|v| v.is_empty()) {
                    return Some(Vec::new());
                }
            }
            let lit = lit.unwrap_or_default();
            acc = Some(match acc {
                None => lit,
                Some(prev) => intersect(&prev, &lit),
            });
            if acc.as_ref().is_some_and(|v| v.is_empty()) {
                return Some(Vec::new());
            }
        }
        acc
    }

    /// Does the accelerator have any posting list at all? An index built over
    /// names all shorter than three bytes has nothing to offer.
    pub fn is_useful(&self) -> bool {
        !self.postings.is_empty()
    }

    /// Number of distinct trigrams stored (a coarse memory shape indicator).
    pub fn trigram_count(&self) -> usize {
        self.postings.len()
    }

    /// Live entries the accelerator was built over.
    pub fn indexed_entries(&self) -> u64 {
        self.indexed
    }

    /// Total (trigram, id) postings stored.
    pub fn posting_total(&self) -> u64 {
        self.total_postings
    }

    /// Rough resident size in bytes: posting ids plus per-list overhead. Used by
    /// the benchmark and `stats`.
    pub fn approx_bytes(&self) -> usize {
        let ids = self.total_postings as usize * std::mem::size_of::<u32>();
        let keys = self.postings.len() * (3 + std::mem::size_of::<Vec<u32>>());
        let slack = self.postings.len() * 8; // allocator/rounding headroom
        ids + keys + slack
    }

    /// Borrow a raw posting list (diagnostics/tests).
    pub fn raw_postings(&self, tri: &[u8; 3]) -> Option<&[u32]> {
        self.postings.get(tri).map(|v| v.as_slice())
    }

    /// Candidate ids whose name shares **at least** `min_shared` of the given
    /// trigrams, ascending. This is the fuzzy generator.
    ///
    /// ## Why this is a sound superset
    ///
    /// If a name contains an approximate occurrence of a pattern within `k`
    /// edits, then at most `k` single-character edits separate the two, and a
    /// single edit can destroy at most **3** trigrams of the pattern (the edit
    /// lands in a window that covers up to three overlapping trigrams). So of
    /// the pattern's `t` distinct trigrams, at least `t - 3k` must still appear
    /// in the name.
    ///
    /// When `t - 3k <= 0` the bound says nothing: return `None`, and the caller
    /// scans everything. That is the honest outcome — a large edit budget on a
    /// short pattern cannot be accelerated.
    ///
    /// Candidate ids come from *counting* how many of the given trigrams each
    /// name shares (not intersecting), which is why this is a distinct operation
    /// from [`Self::candidates`].
    pub fn candidates_sharing_at_least(
        &self,
        trigrams: &[[u8; 3]],
        min_shared: usize,
    ) -> Option<Vec<u32>> {
        if min_shared == 0 || trigrams.is_empty() {
            return None; // no usable constraint
        }
        if min_shared > trigrams.len() {
            // Cannot possibly be satisfied: fewer trigrams available than needed.
            return Some(Vec::new());
        }
        // Count, per id, how many distinct query trigrams its postings appear in.
        let mut counts: HashMap<u32, u32> = HashMap::new();
        for tri in trigrams {
            if let Some(list) = self.postings.get(tri) {
                for &id in list {
                    *counts.entry(id).or_insert(0) += 1;
                }
            }
        }
        let need = min_shared as u32;
        let mut out: Vec<u32> = counts
            .into_iter()
            .filter(|(_, c)| *c >= need)
            .map(|(id, _)| id)
            .collect();
        out.sort_unstable();
        Some(out)
    }

    /// The number of the pattern's distinct trigrams that must survive `k` edits:
    /// `t - 3k`, saturating at zero. Zero means "no usable trigram constraint".
    pub fn required_shared_after_edits(pattern_trigrams: usize, max_distance: u32) -> usize {
        pattern_trigrams.saturating_sub(3 * max_distance as usize)
    }
}

/// Intersect two ascending, deduplicated id lists.
pub(crate) fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{EntryFlags, ROOT_ID};

    fn tree() -> Index {
        let mut ix = Index::new();
        let r = ix.insert(
            ROOT_ID,
            b"proj",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let src = ix.insert(r, b"src", EntryFlags::EMPTY.with_dir(true));
        ix.insert(src, b"main.rs", EntryFlags::EMPTY);
        ix.insert(src, b"lib.rs", EntryFlags::EMPTY);
        ix.insert(r, b"README.md", EntryFlags::EMPTY);
        ix.insert(r, b"Notes.TXT", EntryFlags::EMPTY);
        ix
    }

    #[test]
    fn builds_and_finds_by_trigram() {
        let ix = tree();
        let tg = TrigramIndex::build(&ix);
        // "main.rs" contains the trigram "ain".
        let ids = tg.raw_postings(b"ain").expect("ain indexed");
        assert_eq!(ids.len(), 1);
        assert_eq!(ix.name(ids[0]), b"main.rs");
        // Case folding: "TXT" and "txt" hit the same list.
        assert!(tg.raw_postings(b"txt").is_some(), "lowercased index");
    }

    #[test]
    fn root_dir_is_not_indexed() {
        let ix = tree();
        let tg = TrigramIndex::build(&ix);
        // "proj" is the root dir name and must never appear as a candidate.
        if let Some(ids) = tg.raw_postings(b"roj") {
            for id in ids {
                assert!(!ix.entry(*id).unwrap().flags().is_root_dir());
            }
        }
        assert_eq!(tg.indexed_entries(), 5, "5 non-root entries");
    }

    #[test]
    fn literal_shorter_than_three_has_no_trigrams() {
        assert!(TrigramIndex::literal_trigrams(b"rs", false).is_none());
        assert!(TrigramIndex::literal_trigrams(b"", false).is_none());
        assert!(TrigramIndex::literal_trigrams(b"abc", false).is_some());
        // A case-sensitive literal cannot use the lowercased index.
        assert!(TrigramIndex::literal_trigrams(b"Abc", true).is_none());
    }

    #[test]
    fn literal_trigrams_are_distinct() {
        // "aaaa" has the trigrams "aaa" twice; only one entry should be planned.
        let t = TrigramIndex::literal_trigrams(b"aaaa", false).unwrap();
        assert_eq!(t, vec![*b"aaa"]);
    }

    #[test]
    fn candidates_are_a_superset_filter() {
        let ix = tree();
        let tg = TrigramIndex::build(&ix);
        let plan = TrigramIndex::plan_from_literals([(&b"main"[..], false)]);
        let cands = tg.candidates(&plan).unwrap();
        // Exactly the one name containing "main".
        assert_eq!(cands.len(), 1);
        assert_eq!(ix.name(cands[0]), b"main.rs");
        // And a non-existent literal yields zero candidates.
        let plan = TrigramIndex::plan_from_literals([(&b"zzzz"[..], false)]);
        assert_eq!(tg.candidates(&plan).unwrap(), Vec::<u32>::new());
    }

    #[test]
    fn empty_plan_means_no_constraint() {
        let ix = tree();
        let tg = TrigramIndex::build(&ix);
        // One 2-byte literal, one case-sensitive: nothing usable.
        let plan = TrigramIndex::plan_from_literals([(&b"rs"[..], false), (&b"Abc"[..], true)]);
        assert!(plan.is_empty());
        assert!(tg.candidates(&plan).is_none(), "None = scan everything");
    }

    #[test]
    fn intersect_keeps_ascending_order() {
        let a = [1u32, 3, 5, 7, 9];
        let b = [2u32, 3, 4, 7, 10];
        assert_eq!(intersect(&a, &b), vec![3, 7]);
        assert_eq!(intersect(&[], &b), Vec::<u32>::new());
        assert_eq!(intersect(&a, &[]), Vec::<u32>::new());
    }

    #[test]
    fn multiple_literals_intersect() {
        let ix = tree();
        let tg = TrigramIndex::build(&ix);
        // "rs" is too short; "main" and "lib" are mutually exclusive.
        let plan = TrigramIndex::plan_from_literals([(&b"main"[..], false), (&b"lib"[..], false)]);
        assert_eq!(tg.candidates(&plan).unwrap(), Vec::<u32>::new());
        // "ma" (short, ignored) AND "main" -> still constrained by "main".
        let plan = TrigramIndex::plan_from_literals([(&b"ma"[..], false), (&b"main"[..], false)]);
        let c = tg.candidates(&plan).unwrap();
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn memory_estimate_is_nonzero_and_bounded() {
        let ix = tree();
        let tg = TrigramIndex::build(&ix);
        // The estimate must at least cover the ids it actually stores.
        assert!(tg.approx_bytes() >= tg.posting_total() as usize * 4);
        assert!(tg.trigram_count() > 0);
        assert_eq!(
            tg.posting_total(),
            tg.postings.values().map(|v| v.len() as u64).sum::<u64>()
        );
    }

    #[test]
    fn a_name_that_repeats_a_trigram_is_stored_once_per_trigram() {
        // "ababab" contains "aba"/"bab" repeatedly. Each must appear once in its
        // posting list, and the list must have exactly the capacity the sizing
        // pass reserved — a count that ignored the repetition would reserve too
        // much, and one that double-pushed would corrupt membership.
        let mut ix = Index::new();
        let r = ix.insert(
            ROOT_ID,
            b"p",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        ix.insert(r, b"ababab", EntryFlags::EMPTY);
        ix.insert(r, b"abcdabcd", EntryFlags::EMPTY); // 4 distinct trigrams, repeated
        let tg = TrigramIndex::build(&ix);

        let aba = tg.raw_postings(b"aba").expect("aba indexed");
        assert_eq!(aba.len(), 1, "one name, one id; got {aba:?}");
        // Every posting list must be free of duplicates, whatever the name did.
        for (tri, list) in tg.postings.iter() {
            let mut sorted = list.clone();
            sorted.dedup();
            assert_eq!(sorted.len(), list.len(), "duplicate id in list for {tri:?}");
        }
        // `total_postings` must equal the sum of the list lengths (it is a real
        // count, not an estimate used only for sizing).
        assert_eq!(
            tg.posting_total(),
            tg.postings.values().map(|v| v.len() as u64).sum::<u64>()
        );
    }

    #[test]
    fn sharing_bound_is_t_minus_3k() {
        // A 5-trigram pattern (7 bytes) tolerates 0 edits with all 5 required,
        // 1 edit with 2 required, 2 edits with none.
        assert_eq!(TrigramIndex::required_shared_after_edits(5, 0), 5);
        assert_eq!(TrigramIndex::required_shared_after_edits(5, 1), 2);
        assert_eq!(TrigramIndex::required_shared_after_edits(5, 2), 0);
        assert_eq!(
            TrigramIndex::required_shared_after_edits(2, 1),
            0,
            "saturates at zero"
        );
    }

    #[test]
    fn candidates_sharing_counts_rather_than_intersects() {
        let ix = tree();
        let tg = TrigramIndex::build(&ix);
        // "main.rs" -> trigrams {mai, ain, in., n.r, .rs}. Ask which names share
        // at least 3 of them: "main.rs" (all 5) qualifies; "Notes.TXT" shares
        // none; "README.md" shares ".md"-> not in the set, so zero.
        let tris = TrigramIndex::literal_trigrams(b"main.rs", false).unwrap();
        let cands = tg.candidates_sharing_at_least(&tris, 3).unwrap();
        let names: Vec<String> = cands
            .iter()
            .map(|id| String::from_utf8_lossy(ix.name(*id)).into_owned())
            .collect();
        assert_eq!(names, vec!["main.rs".to_string()]);

        // A relaxed threshold pulls in more (superset grows as `min_shared` drops).
        let loose = tg.candidates_sharing_at_least(&tris, 1).unwrap();
        assert!(loose.len() >= cands.len());
    }

    #[test]
    fn no_constraint_when_bound_vanishes() {
        let ix = tree();
        let tg = TrigramIndex::build(&ix);
        let tris = TrigramIndex::literal_trigrams(b"main.rs", false).unwrap();
        // min_shared == 0 means "no usable constraint": scan everything.
        assert!(tg.candidates_sharing_at_least(&tris, 0).is_none());
        // An impossible ask is empty, not a scan.
        assert_eq!(
            tg.candidates_sharing_at_least(&tris, 99).unwrap(),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn sharing_generator_never_drops_an_approximate_match() {
        // The superset contract: for every name that contains an approximate
        // occurrence of the pattern within k edits, the generator must return it.
        use crate::fuzzy::substring_within;
        let ix = tree();
        let tg = TrigramIndex::build(&ix);

        for (pattern, k) in [
            (&b"main.rs"[..], 0u32),
            (b"main.rs", 1),
            (b"mian.rs", 1),
            (b"notes", 1),
        ] {
            let tris = TrigramIndex::literal_trigrams(pattern, false).unwrap();
            let need = TrigramIndex::required_shared_after_edits(tris.len(), k);
            // True matches by brute force over every name.
            let mut truth: Vec<u32> = Vec::new();
            for (id, e) in ix.iter_live() {
                if e.flags().is_root_dir() {
                    continue;
                }
                if substring_within(ix.name(id), pattern, k).is_some() {
                    truth.push(id);
                }
            }
            if need == 0 {
                assert!(
                    tg.candidates_sharing_at_least(&tris, need).is_none(),
                    "pattern {pattern:?} k={k}: expected 'no constraint', not a candidate list"
                );
                continue; // no constraint => the caller scans; nothing to check
            }
            let cands = tg.candidates_sharing_at_least(&tris, need).unwrap();
            let cand_set: std::collections::HashSet<u32> = cands.iter().copied().collect();
            for id in &truth {
                assert!(
                    cand_set.contains(id),
                    "pattern {pattern:?} k={k}: generator dropped true match {:?}",
                    String::from_utf8_lossy(ix.name(*id))
                );
            }
        }
    }

    /// Incremental maintenance equals a full rebuild, posting for posting.
    ///
    /// Builds over half a tree, then applies the other half entry-by-entry
    /// through `index_entry` (the same call the applier's `emit` path makes),
    /// deletes a slice through `remove_entry`, and asserts the result is
    /// identical to `build` over the final index: same posting lists, same
    /// counts, same query answers.
    #[test]
    fn incremental_maintenance_matches_full_rebuild() {
        use crate::index::{EntryFlags, ROOT_ID};
        let mut ix = Index::new();
        let r = ix.insert(
            ROOT_ID,
            b"proj",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        // Deterministic names with heavy trigram overlap.
        let names: Vec<String> = (0..300)
            .map(|i| {
                format!(
                    "{}{}.{}",
                    ["alpha", "beta", "gamma", "delta"][i % 4],
                    i % 37,
                    ["rs", "md", "txt"][i % 3]
                )
            })
            .collect();
        for n in &names[..150] {
            ix.insert(r, n.as_bytes(), EntryFlags::EMPTY);
        }
        let mut tg = TrigramIndex::build(&ix);
        // Second half, one entry at a time through the incremental path.
        for n in &names[150..] {
            let id = ix.insert(r, n.as_bytes(), EntryFlags::EMPTY);
            let added = tg.index_entry(id, n.as_bytes(), false);
            assert!(added > 0, "every test name is long enough to post");
        }
        // Delete every 7th entry through the incremental path.
        let doomed: Vec<(u32, Vec<u8>)> = ix
            .iter_live()
            .filter(|(id, e)| !e.flags().is_root_dir() && id % 7 == 0)
            .map(|(id, _)| (id, ix.name(id).to_vec()))
            .collect();
        for (id, nm) in &doomed {
            ix.remove(*id);
            tg.remove_entry(*id, nm);
        }
        // Posting-for-posting equality with a fresh full build.
        let fresh = TrigramIndex::build(&ix);
        assert_eq!(
            tg.trigram_count(),
            fresh.trigram_count(),
            "distinct trigram count"
        );
        assert_eq!(tg.posting_total(), fresh.posting_total(), "total postings");
        assert_eq!(
            tg.indexed_entries(),
            fresh.indexed_entries(),
            "indexed count"
        );
        for (tri, list) in fresh.postings.iter() {
            assert_eq!(
                tg.raw_postings(tri),
                Some(list.as_slice()),
                "posting list differs for {tri:?}"
            );
        }
        // And no list the fresh build lacks.
        for tri in tg.postings.keys() {
            assert!(
                fresh.postings.contains_key(tri),
                "stale trigram {tri:?} left after deletion"
            );
        }
    }

    /// A replayed `Add` for an already-indexed id is a no-op, not a duplicate.
    ///
    /// `apply_add` can restore an id under its original name (snapshot older
    /// than journal); the per-trigram `last() != id` / binary-search guard
    /// must keep the lists deduplicated.
    #[test]
    fn reindexing_the_same_id_does_not_duplicate() {
        use crate::index::{EntryFlags, ROOT_ID};
        let mut ix = Index::new();
        let r = ix.insert(
            ROOT_ID,
            b"p",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let id = ix.insert(r, b"repeatable.rs", EntryFlags::EMPTY);
        let mut tg = TrigramIndex::build(&ix);
        let before = tg.posting_total();
        // Same id, same name: pure no-op.
        tg.index_entry(id, b"repeatable.rs", false);
        assert_eq!(tg.posting_total(), before, "replay must not duplicate");
        // Same id, *shorter* name (rename path indexes the new name; the old
        // postings stay stale-but-safe by design, tested at query level).
        tg.index_entry(id, b"a", false);
        assert_eq!(tg.posting_total(), before, "sub-trigram name posts nothing");
        for (tri, list) in tg.postings.iter() {
            let mut sorted = list.clone();
            sorted.dedup();
            assert_eq!(sorted.len(), list.len(), "duplicate in {tri:?}");
        }
    }
}
