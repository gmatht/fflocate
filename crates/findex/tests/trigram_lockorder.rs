//! Trigram/applier lock-order regression test (§15, §17).
//!
//! Observed live: a query intersecting trigram postings over a large index
//! holds the postings *read* guard for seconds while taking the index read
//! lock, and the applier's per-record maintenance took the postings *write*
//! guard while holding the index *write* lock — ABBA deadlock. Symptoms were
//! total: frozen CPU, handler threads piling up per connection, `stats`
//! silent, fresh daemon wedged minutes after startup the moment a query
//! overlapped the crawl.
//!
//! The fix keeps the single-funnel maintenance but drops the index guard
//! before touching slot/postings (`Applier::emit` snapshots, then
//! publishes). This test forces the overlap deterministically: hold the
//! postings read guard (a query mid-intersection), run `emit` on another
//! thread via `push`+`drain`, then take the index read lock while still
//! holding postings-read. Pre-fix that triple wedges forever; post-fix the
//! emit completes. Bounded by `recv_timeout` so a regression fails loudly
//! instead of hanging the suite.
//!
//! Integration target (not a `src/` unit test) because the concurrent
//! `expr.rs` work currently breaks the `findex` lib-test target.

use std::sync::Arc;
use std::time::Duration;

use findex::applier::{Applier, Shared};
use findex::event::{DirKey, FsEvent};
use findex::index::{EntryFlags, Index, ROOT_ID};
use findex::persist::Store;
use findex::roots::Root;
use findex::sync::SyncRegistry;

fn harness() -> (Applier, Arc<Shared>, tempfile::TempDir, u32) {
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
    (applier, shared, dir, root)
}

#[test]
#[cfg(feature = "trigram")]
fn emit_completes_while_a_query_holds_postings_read() {
    let (mut applier, shared, _dir, root) = harness();

    // Seed entries and warm the cache, so `emit` actually reaches the
    // postings write guard (cold cache skips maintenance — no deadlock
    // either way, and no test).
    for i in 0..50u32 {
        applier.push(FsEvent::Upsert {
            parent: DirKey(root),
            name: format!("seed-file-{i:03}.rs").into_bytes(),
            is_dir: false,
        });
    }
    assert!(applier.drain() > 0, "seeding must apply records");
    assert!(
        shared.trigram_index().read().unwrap().trigram_count() > 0,
        "warmed cache must hold postings"
    );

    // Query mid-intersection: postings read guard held, index read lock next.
    let tg = shared.trigram_index();
    let _postings = tg.read().unwrap();

    // Applier maintenance on another thread, completion reported back.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        applier.push(FsEvent::Upsert {
            parent: DirKey(root),
            name: b"concurrent-arrival.rs".to_vec(),
            is_dir: false,
        });
        applier.drain();
        let _ = done_tx.send(());
    });

    // Guarantee the overlap: the emit is inside its postings-wait (pre-fix)
    // or racing through (post-fix) while we now take the index read lock —
    // the second half of the query's order. Pre-fix this wedges: emit holds
    // index-write waiting on postings-write, we hold postings-read waiting
    // on index-read. `try_read` with a deadline (not a blocking `read`) so a
    // regression fails loudly instead of hanging the suite.
    std::thread::sleep(Duration::from_millis(300));
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(_ix) = shared.index.try_read() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "index read lock unavailable while holding postings-read: \
                 ABBA deadlock between query intersection and applier maintenance"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(_postings);

    done_rx.recv_timeout(Duration::from_secs(10)).expect(
        "emit must complete while a query holds postings-read: \
         ABBA deadlock between query intersection and applier maintenance",
    );
}
