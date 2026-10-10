//! Trigram accelerator benchmark (§17 v2).
//!
//! Requires the default-on `trigram` cargo feature; without it the binary is a
//! no-op stub (so `--no-default-features` still builds the example target).
//!
//! Dependency-free (no criterion), so it runs anywhere the crate builds:
//!
//! ```sh
//! cargo run --release -p findex --example trigram_bench
//! ```
//!
//! It builds a synthetic index of `N` entries (default 1,000,000 — the §17
//! scale) whose names are drawn from a realistic-ish corpus, then reports:
//!
//! * **build** — time and resident size of the trigram posting lists;
//! * **speedup** — accelerated vs linear scan for a range of query
//!   selectivities, with the *same* result count asserted on both sides so a
//!   fast number can never come from a wrong answer.
//!
//! The absolute numbers printed here are the machine's, not a promise; the
//! point is the shape (selectivity vs speedup) and that memory stays proportional
//! to indexed bytes.

#[cfg(feature = "trigram")]
use std::time::Instant;

#[cfg(feature = "trigram")]
use findex::index::{EntryFlags, Index, ROOT_ID};
#[cfg(feature = "trigram")]
use findex::query::{self, Query};
#[cfg(feature = "trigram")]
use findex::trigram::TrigramIndex;

/// Without the `trigram` feature there is nothing to benchmark; keep the
/// example target buildable.
#[cfg(not(feature = "trigram"))]
fn main() {
    eprintln!("trigram_bench: built without the `trigram` feature; nothing to measure.");
}

/// With the feature on, `main` runs the benchmark.
#[cfg(feature = "trigram")]
fn main() {
    bench::main();
}

/// Everything below needs the accelerator, so it lives in a `cfg`-gated module.
#[cfg(feature = "trigram")]
mod bench {
use super::*;

/// A tiny deterministic PRNG so a run reproduces.
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
}

const WORDS: &[&str] = &[
    "main", "lib", "mod", "test", "util", "index", "query", "server", "client", "core",
    "render", "widget", "window", "buffer", "parser", "lexer", "token", "socket", "thread",
    "async", "future", "task", "queue", "node", "graph", "edge", "cache", "store", "log",
];
const EXTS: &[&str] = &["rs", "md", "toml", "json", "c", "h", "cpp", "py", "txt", "lock"];

/// Build an index of `n` entries under a synthetic tree.
fn build_index(n: usize) -> (Index, Vec<(u32, Vec<u8>)>) {
    let mut rng = Rng::new(0xC0FFEE);
    let mut ix = Index::new();
    let root = ix.insert(ROOT_ID, b"root", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));

    // A pool of directories to spread children across (keeps the tree shallow so
    // path materialization isn't the bottleneck).
    let ndirs = (n / 200).max(16);
    let mut dirs: Vec<u32> = Vec::with_capacity(ndirs);
    for i in 0..ndirs {
        let name = format!("d{i}");
        dirs.push(ix.insert(root, name.as_bytes(), EntryFlags::EMPTY.with_dir(true)));
    }

    for _ in 0..n {
        let parent = dirs[rng.below(dirs.len() as u64) as usize];
        let w = WORDS[rng.below(WORDS.len() as u64) as usize];
        let name = match rng.below(3) {
            0 => format!("{w}.{}", EXTS[rng.below(EXTS.len() as u64) as usize]),
            1 => format!("{w}_{}.{}", WORDS[rng.below(WORDS.len() as u64) as usize], EXTS[rng.below(EXTS.len() as u64) as usize]),
            _ => format!(
                "{w}_{}_{}.{}",
                WORDS[rng.below(WORDS.len() as u64) as usize],
                rng.below(1000),
                EXTS[rng.below(EXTS.len() as u64) as usize]
            ),
        };
        ix.insert(parent, name.as_bytes(), EntryFlags::EMPTY);
    }

    let root_path = b"/bench/root".to_vec();
    (ix, vec![(root, root_path)])
}

fn main_run() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);

    println!("trigram accelerator benchmark — {} entries", n);
    println!("{}", "-".repeat(72));

    let t = Instant::now();
    let (ix, rp) = build_index(n);
    println!("index build:        {:>8.1} ms  ({} live entries)", t.elapsed().as_secs_f64() * 1000.0, ix.live_count());

    let t = Instant::now();
    let tg = TrigramIndex::build(&ix);
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    let names_bytes: usize = ix.names.len();
    let tg_bytes = tg.approx_bytes();
    println!(
        "trigram build:      {:>8.1} ms  ({} trigrams, {} postings)",
        build_ms,
        tg.trigram_count(),
        tg.posting_total()
    );
    println!(
        "names arena:        {:>8.1} MiB",
        names_bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "trigram postings:   {:>8.1} MiB  ({:.0}% of name bytes)",
        tg_bytes as f64 / (1024.0 * 1024.0),
        100.0 * tg_bytes as f64 / names_bytes.max(1) as f64
    );
    println!();

    // A selection of the design's phrase shapes, from very selective to broad.
    let queries = [
        ("very selective", "render_widget"),
        ("selective", "parser.socket"),
        ("word + ext", "client ext:rs"),
        ("common word", "main"),
        ("common + ext", "test ext:rs"),
        ("broad (3-byte)", "net"),
        ("unsatisfiable", "qqqqqq"),
    ];

    println!("{:<18} {:>8} {:>12} {:>12} {:>9}", "query", "matches", "linear ms", "trigram ms", "speedup");
    println!("{}", "-".repeat(72));

    for (label, raw) in queries.iter().copied() {
        let q = Query::compile(raw).unwrap();

        // Warm both paths once (page-cache / branch predictor), then time the
        // best of a few runs to shed scheduling noise.
        let _ = query::execute(&ix, &rp, &q, 0, false);
        let _ = query::execute_with_trigram(&ix, &rp, &q, 0, false, Some(&tg));

        let reps = 5;
        let mut linear = f64::MAX;
        let mut accel = f64::MAX;
        let mut matches = 0u64;
        for _ in 0..reps {
            let t = Instant::now();
            let a = query::execute(&ix, &rp, &q, 0, false);
            linear = linear.min(t.elapsed().as_secs_f64() * 1000.0);

            let t = Instant::now();
            let b = query::execute_with_trigram(&ix, &rp, &q, 0, false, Some(&tg));
            accel = accel.min(t.elapsed().as_secs_f64() * 1000.0);

            // Never let a fast number come from a wrong answer.
            assert_eq!(a.total, b.total, "query {raw:?}: totals diverged");
            assert_eq!(
                a.hits.iter().map(|h| h.id).collect::<Vec<_>>(),
                b.hits.iter().map(|h| h.id).collect::<Vec<_>>(),
                "query {raw:?}: result order diverged"
            );
            matches = a.total;
        }

        let speedup = if accel > 0.0 { linear / accel } else { f64::INFINITY };
        println!(
            "{label:<18} {matches:>8} {linear:>12.2} {accel:>12.2} {speedup:>8.1}x"
        );
        println!("    q = {raw:?}");
    }
    println!();
    println!("All accelerated results were asserted identical to the linear scan.");
}

/// Entry point for the gated outer `main`.
pub fn main() {
    main_run();
}
} // mod bench
