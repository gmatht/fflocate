#![cfg(feature = "trigram")]

//! Differential test for the §17 v2 trigram accelerator.
//!
//! The accelerator is allowed to change *speed*, never *answers*. This suite
//! pins that: it drives random trees and a corpus of query shapes through
//! `query::execute` twice — once as the plain linear scan, once with the
//! trigram prefilter wired in — and asserts the two are identical, including
//! result order and the `total` count.
//!
//! The query corpus is chosen to cover every interaction that could break a
//! naive filter: literals shorter than a trigram, smart-case terms, globs,
//! regexes, `path:` terms (which the name index cannot see), negation, phrases,
//! and combinations of all of the above. A single divergence fails the test.
//!
//! It also checks the *soundness edge*: after deletions the cached postings can
//! name tombstoned ids, and those must simply not appear — the authoritative
//! matcher, not the accelerator, decides.

use fflocate::index::{EntryFlags, Index, ROOT_ID};
use fflocate::query::{self, Query};
use fflocate::trigram::TrigramIndex;

/// Deterministic xorshift64* so a failure reproduces from its seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

/// Name fragments drawn from a small alphabet so trigram collisions are common
/// (a large alphabet would make everything trivially disjoint).
const WORDS: &[&str] = &[
    "main",
    "lib",
    "mod",
    "test",
    "util",
    "index",
    "query",
    "server",
    "src",
    "rs",
    "md",
    "toml",
    "foo",
    "bar",
    "baz",
    "notes",
    "README",
    "main_test",
    "core",
    "net",
    "abc",
    "xyz",
    "aaaa",
    "zz",
    "a",
    "Q",
    "UPPER",
    "MixedCase",
];

const EXTS: &[&str] = &["rs", "md", "toml", "txt", "c", "h", "o", "so"];

/// Build a random tree: a fixed root dir plus `n` entries scattered under a few
/// intermediate directories, with names that exercise case, short names and
/// repeated trigrams.
fn random_tree(seed: u64, n: usize) -> (Index, Vec<(u32, Vec<u8>)>) {
    let mut rng = Rng::new(seed);
    let mut ix = Index::new();
    let root = ix.insert(
        ROOT_ID,
        b"proj",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );

    // A handful of intermediate dirs to give parents variety.
    let mut dirs = vec![root];
    for i in 0..4 {
        let name = format!("d{i}");
        let d = ix.insert(root, name.as_bytes(), EntryFlags::EMPTY.with_dir(true));
        dirs.push(d);
    }

    for _ in 0..n {
        let parent = *rng.pick(&dirs);
        let word = rng.pick(WORDS);
        let mut name = String::from(*word);
        // Sometimes add a random second word and/or an extension, sometimes not.
        if rng.below(3) == 0 {
            name.push('_');
            name.push_str(rng.pick(WORDS));
        }
        if rng.below(4) != 0 {
            name.push('.');
            name.push_str(rng.pick(EXTS));
        }
        let is_dir = rng.below(8) == 0;
        let flags = if is_dir {
            EntryFlags::EMPTY.with_dir(true)
        } else if rng.below(8) == 0 {
            EntryFlags::EMPTY.with_symlink(true)
        } else {
            EntryFlags::EMPTY
        };
        let id = ix.insert(parent, name.as_bytes(), flags);
        if is_dir {
            dirs.push(id);
        }
    }

    let root_path = b"/tmp/proj".to_vec();
    (ix, vec![(root, root_path)])
}

/// The query corpus: every shape the engine supports, plus combinations.
fn query_corpus() -> Vec<&'static str> {
    vec![
        // plain substrings of every length incl. < 3 bytes
        "main",
        "ai",
        "a",
        "zz",
        "notes",
        "aaaa",
        "rs",
        // smart-case
        "Q",
        "UPPER",
        "MixedCase",
        "case:Mixed",
        "case:mixed",
        // ext
        "ext:rs",
        "ext:md",
        "ext:c",
        "ext:RS",
        "ext:zzz",
        // globs
        "*.rs",
        "main*",
        "?ain",
        "*.md",
        // regex
        "regex:^m.*",
        "regex:.*test.*",
        "regex:^.$",
        // path (not accelerated; must fall through correctly)
        "path:d1",
        "path:proj",
        "path:nonexistent",
        // phrases
        "\"main.rs\"",
        "\"core\"",
        // negation
        "-test",
        "main -test",
        "ext:rs -main",
        "-zz",
        // ANDs spanning shapes
        "main ext:rs",
        "index net ext:rs",
        "util path:d2",
        "main rs",
        "MIXED ext:md",
        "aaaa aaa",
        // things that match nothing
        "qqqqq",
        "ext:qqq",
        "regex:^zzzzzz$",
        // empty
        "",
    ]
}

/// A single result row: `(id, materialized path)`.
type Row = (u32, Vec<u8>);

/// The outcome of one query under both paths: `(rows, total)` for each.
type BothPaths = (Vec<Row>, u64, Vec<Row>, u64);

fn run_both(
    ix: &Index,
    rp: &[(u32, Vec<u8>)],
    tg: &TrigramIndex,
    q: &Query,
    limit: usize,
    count_only: bool,
) -> BothPaths {
    let plain = query::execute(ix, rp, q, limit, count_only);
    let accel = query::execute_with_trigram(ix, rp, q, limit, count_only, Some(tg));
    let plain_rows: Vec<Row> = plain.hits.iter().map(|h| (h.id, h.path.clone())).collect();
    let accel_rows: Vec<Row> = accel.hits.iter().map(|h| (h.id, h.path.clone())).collect();
    (plain_rows, plain.total, accel_rows, accel.total)
}

#[test]
fn accelerated_and_linear_agree_on_random_trees() {
    for seed in 1..=12u64 {
        let (ix, rp) = random_tree(seed, 400);
        let tg = TrigramIndex::build(&ix);
        for raw in query_corpus() {
            let q = Query::compile(raw).unwrap();
            for (limit, count_only) in [(0usize, false), (5, false), (0, true)] {
                let (plain, ptotal, accel, atotal) = run_both(&ix, &rp, &tg, &q, limit, count_only);
                assert_eq!(
                    ptotal, atotal,
                    "seed={seed} q={raw:?} limit={limit} count={count_only}: total differs"
                );
                assert_eq!(
                    plain, accel,
                    "seed={seed} q={raw:?} limit={limit} count={count_only}: rows differ"
                );
            }
        }
    }
}

#[test]
fn accelerator_actually_prunes_for_selective_queries() {
    // Guard against the accelerator silently degrading to "no constraint": for a
    // selective literal it must exclude most entries while still agreeing with
    // the linear scan on the ones it keeps.
    let (ix, rp) = random_tree(7, 2000);
    let tg = TrigramIndex::build(&ix);

    let q = Query::compile("main").unwrap();
    let plan = q.trigram_plan();
    let cands = tg
        .candidates(&plan)
        .expect("`main` yields a trigram constraint");
    assert!(
        (cands.len() as u64) < ix.live_count(),
        "expected the trigram filter to prune: {} candidates of {} live entries",
        cands.len(),
        ix.live_count()
    );

    // Every true hit must be in the candidate set (the filter is a superset).
    let plain = query::execute(&ix, &rp, &q, 0, false);
    let cand_set: std::collections::HashSet<u32> = cands.into_iter().collect();
    for h in &plain.hits {
        assert!(
            cand_set.contains(&h.id),
            "accelerator dropped a real hit id {}",
            h.id
        );
    }
}

#[test]
fn stale_postings_after_deletion_never_add_phantom_hits() {
    // Build the accelerator, then delete entries. The cached postings still name
    // the dead ids; the authoritative matcher must drop them, so the accelerated
    // result equals the linear scan.
    let (mut ix, rp) = random_tree(99, 800);
    let tg = TrigramIndex::build(&ix);

    // Delete a third of the entries.
    let doomed: Vec<u32> = ix
        .iter_live()
        .filter(|(id, e)| !e.flags().is_root_dir() && id % 3 == 0)
        .map(|(id, _)| id)
        .collect();
    for id in &doomed {
        ix.remove(*id);
    }

    for raw in ["main", "ext:rs", "test", "index net", "aaaa"] {
        let q = Query::compile(raw).unwrap();
        let (plain, ptotal, accel, atotal) = run_both(&ix, &rp, &tg, &q, 0, false);
        assert_eq!(ptotal, atotal, "q={raw:?}: total differs after deletion");
        assert_eq!(plain, accel, "q={raw:?}: rows differ after deletion");
    }
}

#[test]
fn empty_plan_behaves_exactly_like_no_accelerator() {
    // A query with nothing a trigram can constrain must be untouched by the
    // accelerator: results identical, and `candidates` reports "no constraint".
    let (ix, rp) = random_tree(5, 300);
    let tg = TrigramIndex::build(&ix);
    for raw in ["ab", "xy", "case:ab", "regex:^m", "path:src"] {
        let q = Query::compile(raw).unwrap();
        let plan = q.trigram_plan();
        if plan.is_empty() {
            assert!(
                tg.candidates(&plan).is_none(),
                "q={raw:?}: empty plan must mean 'scan everything'"
            );
        }
        let (plain, ptotal, accel, atotal) = run_both(&ix, &rp, &tg, &q, 0, false);
        assert_eq!(plain, accel, "q={raw:?}: rows differ");
        assert_eq!(ptotal, atotal, "q={raw:?}: total differs");
    }
}
