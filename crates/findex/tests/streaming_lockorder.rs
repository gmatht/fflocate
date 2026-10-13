//! Streaming-query lock-nesting regression test (§15).
//!
//! Observed live via gdb (two wedged daemons, same signature): main parked in
//! `RwLock::write_contended` (`drain → apply_event → apply_upsert → emit →
//! apply_live`), every serve thread parked in `RwLock::read_contended` on the
//! same index lock, no holder, zero CPU, journal frozen, `stats` silent,
//! `hello` alive. The futex word sat at `0xc0000001` on both.
//!
//! Root cause was in the streaming query path: it held the index read guard
//! for the whole scan and (a) emitted `Meta` via `meta_preamble` — which
//! takes the index read lock *again* on the same thread — and (b) ran the
//! ownership filter (`visible_chain`, also index-reading) per hit under the
//! same guard. A nested read is normally granted, but fatal when the
//! applier's write arrives in between: std's write-preferring RwLock parks
//! the nested read behind the pending writer while the writer waits for the
//! outer read — a self-deadlock. It triggered within seconds under churn
//! (the pending-writer window is near-certain) and never in quiet tests.
//!
//! The fix: Meta is emitted before any scan guard is taken; system mode
//! collects under guards and filters guard-free after; personal mode never
//! acquires anything beneath the scan guard. This test forces the overlap:
//! applier writes churning on one thread while a streaming query runs on
//! another, bounded by `recv_timeout` so a regression fails loudly instead
//! of hanging the suite.

use std::sync::Arc;
use std::time::{Duration, Instant};

use findex::applier::{Applier, Shared};
use findex::event::{DirKey, FsEvent};
use findex::index::{EntryFlags, Index, ROOT_ID};
use findex::persist::Store;
use findex::proto::Response;
use findex::roots::Root;
use findex::server::{handle_expr_streaming, handle_fuzzy_streaming, handle_query_streaming};
use findex::sync::SyncRegistry;

fn harness(n_seed: u32) -> (Applier, Arc<Shared>, tempfile::TempDir, u32) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("state")).unwrap();
    let mut ix = Index::new();
    let root = ix.insert(
        ROOT_ID,
        b"model",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let shared = Shared::new(ix, vec![Root::new(0, std::path::PathBuf::from("/model"))]);
    let mut applier = Applier::new(
        shared.clone(),
        store,
        Arc::new(findex::applier::RealFs),
        SyncRegistry::new(),
    );
    applier.journaling = false;
    applier.rebuild_dir_cache();
    for i in 0..n_seed {
        applier.push(FsEvent::Upsert {
            parent: DirKey(root),
            name: format!("seed-file-{i:04}.rs").into_bytes(),
            is_dir: false,
        });
    }
    assert!(applier.drain() > 0, "seeding must apply records");
    (applier, shared, dir, root)
}

fn ctx_for(shared: &Arc<Shared>) -> findex::server::ServerCtx {
    findex::server::ServerCtx {
        shared: shared.clone(),
        sync: SyncRegistry::new(),
        backend: "fake".into(),
        control: None,
        caller: None,
    }
}

/// Run `f` on a thread while the applier churns writes beneath it. Returns
/// `f`'s output; panics on timeout (the wedge signature: parked forever).
fn race_with_churn<T: Send + 'static>(
    mut applier: Applier,
    root: u32,
    rounds: u32,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    // Churn first: queue writes so the applier's write lock is near-constantly
    // pending while the query runs — this is what parks a nested read.
    let (churn_done_tx, churn_done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for i in 0..rounds {
            applier.push(FsEvent::Upsert {
                parent: DirKey(root),
                name: format!("churn-{i:05}.rs").into_bytes(),
                is_dir: false,
            });
            // Drain every push with no sleeps: back-to-back `apply_live`
            // writes keep a writer pending ~constantly, which is what parked
            // the nested Meta read pre-fix within seconds live.
            applier.drain();
        }
        let _ = churn_done_tx.send(());
    });
    let (out_tx, out_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = f();
        let _ = out_tx.send(out);
    });
    let out = out_rx.recv_timeout(Duration::from_secs(30)).expect(
        "streaming query must complete while the applier churns writes: \
         nested index-read self-deadlock (Meta/filter under the scan guard)",
    );
    churn_done_rx.recv_timeout(Duration::from_secs(30)).expect("churn must finish");
    out
}

/// Why there is no deterministic behavioral test for the wedge itself: the
/// wedge needed a writer pending in the nanosecond window between the scan
/// guard acquisition and the nested Meta read. Under live drain churn
/// (30k-deep queue, back-to-back `apply_live` writes) a writer is pending
/// ~always, so the first streaming query wedged twice; in tests the window
/// must be forced. Forcing needs an outer read held on another thread — but
/// then even the fixed code legitimately waits (Meta itself reads the
/// index), so "Meta arrives while wedged" cannot distinguish. The raw lock
/// mechanism (`nested_read_parks_behind_a_pending_writer`) plus the churn
/// tripwires below plus live validation under catch-up churn are the
/// three-legged case. Do not reintroduce `meta_preamble`/`visible_chain`
/// under a held scan guard — grep for it in review.
#[test]
fn query_completes_with_writer_parked_then_released() {
    // Sanity: a parked writer delays the query (correct write-preferring
    // behavior) but must not wedge it — after release everything finishes.
    let (_applier, shared, _dir, _root) = harness(50);
    let ctx = ctx_for(&shared);
    let _outer = shared.index.read().unwrap();
    let shared2 = shared.clone();
    let writer_handle = std::thread::spawn(move || {
        let _w = shared2.index.write().unwrap();
    });
    std::thread::sleep(Duration::from_millis(200));
    let collected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let collected2 = collected.clone();
    let query_handle = std::thread::spawn(move || {
        let t0 = Instant::now();
        handle_query_streaming(&ctx, "seed", 200, false, false, t0, &mut |r| {
            collected2.lock().unwrap().push(r);
            true
        });
    });
    std::thread::sleep(Duration::from_millis(200));
    drop(_outer);
    writer_handle.join().unwrap();
    query_handle.join().unwrap();
    let out: Vec<Response> = collected.lock().unwrap().drain(..).collect();
    assert!(matches!(out.first(), Some(Response::Meta { .. })), "Meta first");
    assert!(matches!(out.last(), Some(Response::Done { .. })), "Done last");
}

fn collect_query(ctx: &findex::server::ServerCtx, q: &str) -> Vec<Response> {
    let mut out = Vec::new();
    let t0 = Instant::now();
    handle_query_streaming(ctx, q, 200, false, false, t0, &mut |r| {
        out.push(r);
        true
    });
    out
}

#[test]
fn streaming_query_completes_under_applier_churn() {
    let (applier, shared, _dir, root) = harness(500);
    let ctx = ctx_for(&shared);
    let out = race_with_churn(applier, root, 400, move || collect_query(&ctx, "seed"));
    // Wire shape: Meta first, Hits, Done last with exact total.
    assert!(matches!(out.first(), Some(Response::Meta { .. })), "Meta first");
    assert!(
        matches!(out.last(), Some(Response::Done { .. })),
        "Done last, got {:?}",
        out.last()
    );
    let hits = out
        .iter()
        .filter(|r| matches!(r, Response::Hit { .. }))
        .count();
    let total = match out.last() {
        Some(Response::Done { count, .. }) => *count,
        _ => 0,
    };
    assert!(hits > 0, "query must find seeded hits");
    assert!(total >= hits as u64, "total covers hits");
}

fn assert_wire_shape(name: &str, recs: &[Response]) {
    assert!(
        matches!(recs.first(), Some(Response::Meta { .. })),
        "{name}: Meta first"
    );
    assert!(
        matches!(recs.last(), Some(Response::Done { .. })),
        "{name}: Done last"
    );
}

#[test]
fn streaming_fuzzy_completes_under_churn() {
    let (applier, shared, _dir, root) = harness(200);
    let ctx = ctx_for(&shared);
    let out = race_with_churn(applier, root, 200, move || {
        let mut fz = Vec::new();
        handle_fuzzy_streaming(&ctx, "sed-file", None, 50, false, Instant::now(), &mut |r| {
            fz.push(r);
            true
        });
        fz
    });
    assert_wire_shape("fuzzy", &out);
}

#[test]
fn streaming_expr_completes_under_churn() {
    let (applier, shared, _dir, root) = harness(200);
    let ctx = ctx_for(&shared);
    let out = race_with_churn(applier, root, 200, move || {
        let expr = serde_json::json!({"suffix": ["rs"]});
        let mut ex = Vec::new();
        handle_expr_streaming(&ctx, &expr, 50, false, Instant::now(), &mut |r| {
            ex.push(r);
            true
        });
        ex
    });
    assert_wire_shape("expr", &out);
}
