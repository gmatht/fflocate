//! What the stored creation minute buys on the hot created-time query.
//!
//! Run with `cargo run --release -p fflocate --example created_query_cost`.
//!
//! Both halves answer the same question — "every entry created at or after
//! `<ts>`" — over the same 20 000 rows:
//!
//! * **now**: `since <ts> oclock` reads the `u32` minute stored beside each
//!   entry (no `stat`, no path lookup on the filesystem);
//! * **before**: the answer the query engine had to reach by `stat`ting each
//!   candidate path for its birth time, which is what the GUI did per row.
//!
//! The timings are wall-clock medians of repeated runs in this process, so they
//! compare the storage shapes; they are not a disk benchmark.

use std::time::{Duration, Instant, UNIX_EPOCH};

use fflocate::expr::{ClockSpec, Expr, SinceField};
use fflocate::index::{EntryFlags, Index, ROOT_ID};
use fflocate::query::execute_expr;

/// Rows in the tree, as many as the commit that landed the stored minute
/// measured against.
const N: u32 = 20_000;
/// 2026-01-02T00:00:00Z in whole minutes since the epoch.
const BASE: u32 = 29_455_200;

fn since(ts: i64) -> Expr {
    Expr::Since {
        spec: ClockSpec::Timestamp(ts),
        field: SinceField::Oclock,
    }
}

fn median(mut runs: Vec<Duration>) -> Duration {
    runs.sort();
    runs[runs.len() / 2]
}

fn ms(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1000.0)
}

/// An index of `N` files under one root, spread over the last day of minutes so
/// a range query selects a real subset.
fn bench_index() -> Index {
    let mut ix = Index::new();
    let root = ix.insert(
        ROOT_ID,
        b"bench",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    for i in 0..N {
        let name = format!("f{i:05}");
        let id = ix.insert(root, name.as_bytes(), EntryFlags::EMPTY);
        ix.set_created(id, BASE - (i % 1440));
    }
    ix
}

fn main() {
    let ix = bench_index();
    let roots = vec![(1u32, b"/bench".to_vec())];

    // Every dated row: the "give me what I made, newest range" shape the GUI's
    // created range asks for. The bound is the oldest stored minute.
    let all_since = since(i64::from(BASE - 1439) * 60);
    // The last hour of stored minutes: a selective range.
    let hour_since = since(i64::from(BASE - 59) * 60);

    let mut all_runs = Vec::new();
    let mut hour_runs = Vec::new();
    let mut count_runs = Vec::new();
    let mut all_hits = 0;
    let mut hour_hits = 0;
    for _ in 0..7 {
        let t = Instant::now();
        let out = execute_expr(&ix, &roots, b"", &all_since, 0, false);
        all_runs.push(t.elapsed());
        all_hits = out.hits.len();

        let t = Instant::now();
        let out = execute_expr(&ix, &roots, b"", &hour_since, 0, false);
        hour_runs.push(t.elapsed());
        hour_hits = out.hits.len();

        let t = Instant::now();
        let out = execute_expr(&ix, &roots, b"", &all_since, 0, true);
        count_runs.push(t.elapsed());
        assert_eq!(out.total as usize, all_hits);
    }

    // The pre-change shape: read each candidate's birth time from the
    // filesystem and compare it. Same N rows, real files, so the `stat` cost is
    // real rather than simulated.
    let dir = std::env::temp_dir().join(format!("ff-created-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::with_capacity(N as usize);
    for i in 0..N {
        let p = dir.join(format!("f{i:05}"));
        std::fs::write(&p, b"x").unwrap();
        paths.push(p);
    }
    let bound = std::time::SystemTime::now() - Duration::from_secs(60 * 60 * 48);
    let mut stat_runs = Vec::new();
    let mut stat_hits = 0;
    for _ in 0..3 {
        let t = Instant::now();
        let mut kept = 0usize;
        for p in &paths {
            let Ok(meta) = std::fs::metadata(p) else { continue };
            let Ok(created) = meta.created() else { continue };
            if created.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
                >= bound
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            {
                kept += 1;
            }
        }
        // Keep the result observable so the loop is not optimised away.
        std::hint::black_box(kept);
        stat_runs.push(t.elapsed());
        stat_hits = kept;
    }
    let _ = std::fs::remove_dir_all(&dir);

    let index_all = median(all_runs.clone());
    let index_hour = median(hour_runs);
    let index_count = median(count_runs);
    let stat_all = median(stat_runs.clone());

    println!("rows: {N} (files created in the last 24 h of minutes)");
    println!("selectivity: since-all keeps {all_hits}, last-hour keeps {hour_hits}, stat keeps {stat_hits}");
    println!();
    println!("stored minute, since-all (paths materialised): {}", ms(index_all));
    println!("stored minute, since-all (count only):         {}", ms(index_count));
    println!("stored minute, last-hour window:               {}", ms(index_hour));
    println!("stat each candidate for its birth time:        {}", ms(stat_all));
    println!();
    println!(
        "speed-up on the hot query: {:.1}x ({} -> {})",
        stat_all.as_secs_f64() / index_all.as_secs_f64(),
        ms(stat_all),
        ms(index_all)
    );
    println!(
        "best observed: stored {} vs stat {}",
        ms(all_runs.into_iter().min().unwrap()),
        ms(stat_runs.into_iter().min().unwrap())
    );
}
