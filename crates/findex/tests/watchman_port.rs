//! Behaviour tests ported from Watchman's integration suite.
//!
//! Watchman's own tests drive a daemon over the wire with a JSON protocol. The
//! ported query-syntax tests live as unit tests next to the engine they
//! exercise (`crates/findex/src/expr.rs`); this file ports the *watch/event*
//! behaviour tests, which need a real tree and the applier loop:
//!
//! | Watchman test | Here |
//! |---|---|
//! | `test_dir_move.py::test_atomicMove` | [`dir_move_atomic_swap_converges`] |
//! | `test_dir_move.py::test_NonAtomicMove` | [`dir_move_nonatomic_recreate_converges`] |
//! | `test_remove_then_add.py::test_remove_then_add` | [`remove_then_re_add_converges`] |
//! | `test_remove.py` (root destroyed) | [`root_gone_drops_subtree`] |
//! | `test_cookie.py` (cookies invisible) | [`cookie_files_are_never_indexed`] |
//! | `test_cursor.py::test_cursor` | [`cursor_advances_and_since_reports_changes`] |
//! | `test_since.py::test_sinceFreshInstance` | [`since_reports_fresh_on_generation_bump`] |
//! | `test_force_recrawl.py::test_force_recrawl` | [`recrawl_after_overflow_converges`] |
//!
//! Each test drives a real temp directory through the same `Applier` the daemon
//! uses, so it exercises the reconciliation path end to end without a socket.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use findex::applier::{Applier, Shared};
use findex::event::{DirKey, FsEvent, RescanCause};
use findex::index::{EntryFlags, Index, ROOT_ID};
use findex::persist::Store;
use findex::roots::Root;
use findex::sync::SyncRegistry;

/// A real temp tree driven by a real applier.
struct Fx {
    applier: Applier,
    shared: Arc<Shared>,
    _dir: tempfile::TempDir,
    root: PathBuf,
    root_id: u32,
}

fn fixture() -> Fx {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let store = Store::open(&d.path().join("state")).unwrap();

    let mut ix = Index::new();
    let basename = root.file_name().unwrap().to_string_lossy().to_string();
    let root_id = ix.insert(
        ROOT_ID,
        basename.as_bytes(),
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let mut r = Root::new(0, root.clone());
    r.caps.writable = true;
    let shared = Shared::new(ix, vec![r]);
    let mut applier = Applier::new(
        shared.clone(),
        store,
        Arc::new(findex::applier::RealFs),
        SyncRegistry::new(),
    );
    applier.rebuild_dir_cache();
    applier.pending_mut().enqueue_crawl(root_id);
    applier.drain();
    applier.drain();
    Fx { applier, shared, _dir: d, root, root_id }
}

impl Fx {
    /// Drain until quiescent, then rescan the root to reconcile any event the
    /// test did not model (the deterministic equivalent of a watcher flush).
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.applier.drain();
            if self.applier.pending().is_drained() {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        self.applier
            .pending_mut()
            .enqueue_rescan(self.root_id, RescanCause::Manual);
        loop {
            self.applier.drain();
            if self.applier.pending().is_drained() {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
        }
    }

    /// Drain only (no rescan) — for tests that feed the exact event stream.
    fn drain(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.applier.drain();
            if self.applier.pending().is_drained() || Instant::now() >= deadline {
                break;
            }
        }
    }

    /// The set of live paths relative to the root (dirs and files).
    fn paths(&self) -> BTreeSet<String> {
        let ix = self.shared.index.read().unwrap();
        let mut out = BTreeSet::new();
        for (id, e) in ix.iter_live() {
            if e.flags().is_root_dir() {
                continue;
            }
            if let Some(p) = ix.path_of(id, b"/") {
                out.insert(String::from_utf8_lossy(&p[1..]).into_owned());
            }
        }
        out
    }

    fn set(&self, v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
}

// ---------------------------------------------------------------------------
// Directory moves (test_dir_move.py)
// ---------------------------------------------------------------------------

#[test]
fn dir_move_atomic_swap_converges() {
    // Build dir/dir/a, then atomically rename dir->bye and alt->dir.
    let mut f = fixture();
    std::fs::create_dir_all(f.root.join("dir/dir")).unwrap();
    std::fs::write(f.root.join("dir/dir/a"), b"a").unwrap();
    f.settle();
    assert_eq!(f.paths(), f.set(&["dir", "dir/dir", "dir/dir/a"]));

    std::fs::create_dir_all(f.root.join("alt/alt")).unwrap();
    std::fs::write(f.root.join("alt/alt/a"), b"a").unwrap();

    // The atomic swap.
    std::fs::rename(f.root.join("dir"), f.root.join("bye")).unwrap();
    std::fs::rename(f.root.join("alt"), f.root.join("dir")).unwrap();
    f.settle();

    assert_eq!(
        f.paths(),
        f.set(&["dir", "dir/alt", "dir/alt/a", "bye", "bye/dir", "bye/dir/a"]),
        "an atomic dir swap must surface both subtrees"
    );
}

#[test]
fn dir_move_nonatomic_recreate_converges() {
    // rmtree the dir, then recreate it with the same contents.
    let mut f = fixture();
    std::fs::create_dir_all(f.root.join("dir")).unwrap();
    std::fs::write(f.root.join("dir/a"), b"a").unwrap();
    f.settle();
    assert_eq!(f.paths(), f.set(&["dir", "dir/a"]));

    std::fs::remove_dir_all(f.root.join("dir")).unwrap();
    f.settle();
    assert!(f.paths().is_empty(), "removed dir must disappear");

    std::fs::create_dir_all(f.root.join("dir")).unwrap();
    std::fs::write(f.root.join("dir/a"), b"a").unwrap();
    f.settle();
    assert_eq!(f.paths(), f.set(&["dir", "dir/a"]), "recreate must converge");
}

#[test]
fn subtree_rename_preserves_names() {
    // `test_changeCase` / `test_dir_move`: a directory rename must move the
    // whole subtree so the children appear at their new paths.
    //
    // NOTE (deviation): findex's inotify backend turns a rename pair into
    // remove+upsert, so the applier reconciles it as delete+create and the
    // children get *fresh* ids (the index's O(1) `move_entry` is not yet driven
    // by the applier). Watchman only asserts the observable names, which is what
    // we assert here.
    let mut f = fixture();
    std::fs::create_dir_all(f.root.join("foo/baz")).unwrap();
    std::fs::write(f.root.join("foo/baz/file"), b"x").unwrap();
    f.settle();
    assert_eq!(f.paths(), f.set(&["foo", "foo/baz", "foo/baz/file"]));

    std::fs::rename(f.root.join("foo"), f.root.join("bar")).unwrap();
    f.settle();

    assert_eq!(
        f.paths(),
        f.set(&["bar", "bar/baz", "bar/baz/file"]),
        "the renamed subtree's children must surface under the new name"
    );
}

// ---------------------------------------------------------------------------
// Remove then re-add (test_remove_then_add.py)
// ---------------------------------------------------------------------------

#[test]
fn remove_then_re_add_converges() {
    let mut f = fixture();
    std::fs::create_dir_all(f.root.join("foo/bar")).unwrap();
    std::fs::write(f.root.join("foo/222"), b"x").unwrap();
    f.settle();
    assert_eq!(f.paths(), f.set(&["foo", "foo/bar", "foo/222"]));

    // Remove everything, then recreate foo/222.
    std::fs::remove_dir_all(f.root.join("foo")).unwrap();
    f.settle();
    assert!(f.paths().is_empty());

    std::fs::create_dir_all(f.root.join("foo")).unwrap();
    std::fs::write(f.root.join("foo/222"), b"x").unwrap();
    f.settle();
    assert_eq!(
        f.paths(),
        f.set(&["foo", "foo/222"]),
        "a subtree removed and re-added must be re-reported, not aliased"
    );
}

#[test]
fn root_gone_drops_subtree() {
    // test_remove.py: destroying the watched root unwatches it. We model the
    // watcher's IN_UNMOUNT/root-destroyed signal as `RootGone`.
    let mut f = fixture();
    std::fs::write(f.root.join("top"), b"x").unwrap();
    f.settle();
    assert_eq!(f.paths(), f.set(&["top"]));

    f.applier.push(FsEvent::RootGone { dir: DirKey::new(f.root_id) });
    f.drain();
    assert!(f.paths().is_empty(), "RootGone must drop the whole subtree");
}

// ---------------------------------------------------------------------------
// Cookies (test_cookie.py)
// ---------------------------------------------------------------------------

#[test]
fn cookie_files_are_never_indexed() {
    let mut f = fixture();
    // A real cookie in the root, plus a look-alike in a subdir.
    std::fs::write(f.root.join(".findex-cookie-999"), b"").unwrap();
    std::fs::create_dir_all(f.root.join("sub")).unwrap();
    std::fs::write(f.root.join("sub/.findex-cookie-1"), b"").unwrap();
    std::fs::write(f.root.join("real.txt"), b"x").unwrap();
    f.settle();

    let paths = f.paths();
    assert!(paths.contains("real.txt"));
    assert!(!paths.iter().any(|p| p.contains("cookie")), "cookies must never be indexed: {paths:?}");
}

#[test]
fn cookie_event_marker_is_not_a_hit() {
    // An applier-fed cookie create event must be consumed as a marker, not
    // indexed.
    let mut f = fixture();
    let name = findex::sync::cookie_name(4242);
    f.applier.push(FsEvent::upsert(DirKey::new(f.root_id), name.clone(), false));
    f.drain();
    assert!(f.paths().is_empty(), "a cookie upsert must not create an entry");
}

// ---------------------------------------------------------------------------
// Cursors and since (test_cursor.py, test_since.py)
// ---------------------------------------------------------------------------

#[test]
fn cursor_advances_and_since_reports_changes() {
    use findex::server::{handle_request, ServerCtx};
    use findex::sync::SyncRegistry;

    let mut f = fixture();
    let ctx = ServerCtx {
        shared: f.shared.clone(),
        sync: SyncRegistry::new(),
        backend: "fake".into(),
        control: None,
        caller: None,
    };

    // Establish a cursor *after* settling (settle() rescans, which bumps the
    // generation and would itself invalidate a cursor captured earlier).
    f.settle();
    let before = f.shared.cursor();

    // Create a file. Use `drain` (not `settle`): we want the cursor to advance
    // without the generation bump a rescan would cause.
    std::fs::write(f.root.join("one"), b"x").unwrap();
    let name = b"one".to_vec();
    f.applier.push(FsEvent::upsert(DirKey::new(f.root_id), name, false));
    f.drain();
    assert!(f.paths().contains("one"));

    // The cursor advanced.
    let after = f.shared.cursor();
    assert!(after.lsn > before.lsn, "the clock must move on change");
    assert_eq!(after.gen, before.gen, "a plain event must not bump the generation");

    // A stale-generation cursor reports fresh (Watchman's is_fresh_instance).
    let stale = format!("c:{}:{}", after.gen.wrapping_sub(1), 0);
    let out = handle_request(&ctx, &findex::proto::Request::Since { cursor: stale, q: String::new() });
    match out.first() {
        Some(findex::proto::Response::Meta { fresh, .. }) => assert!(*fresh, "stale gen must be fresh"),
        other => panic!("expected meta, got {other:?}"),
    }

    // A current cursor is not fresh, and reports the change log tail.
    let current = format!("c:{}:{}", before.gen, before.lsn);
    let out = handle_request(&ctx, &findex::proto::Request::Since { cursor: current, q: String::new() });
    match out.first() {
        Some(findex::proto::Response::Meta { fresh, .. }) => assert!(!*fresh, "a live cursor is not fresh"),
        other => panic!("expected meta, got {other:?}"),
    }
    // The reply always ends with a `done` carrying the current cursor.
    assert!(matches!(out.last(), Some(findex::proto::Response::Done { .. })));
}

#[test]
fn since_reports_fresh_on_generation_bump() {
    use findex::server::{handle_request, ServerCtx};
    use findex::sync::SyncRegistry;

    let mut f = fixture();
    let ctx = ServerCtx {
        shared: f.shared.clone(),
        sync: SyncRegistry::new(),
        backend: "fake".into(),
        control: None,
        caller: None,
    };
    let c = f.shared.cursor();

    // A full-root rescan bumps the generation (test_since.py's recrawl case).
    f.applier
        .pending_mut()
        .enqueue_rescan(f.root_id, RescanCause::Manual);
    f.drain();

    let out = handle_request(
        &ctx,
        &findex::proto::Request::Since { cursor: c.to_string_repr(), q: String::new() },
    );
    match out.first() {
        Some(findex::proto::Response::Meta { fresh, .. }) => {
            assert!(*fresh, "a generation bump must invalidate cursors");
        }
        other => panic!("expected meta, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Recrawl after overflow (test_force_recrawl.py)
// ---------------------------------------------------------------------------

#[test]
fn recrawl_after_overflow_converges() {
    // Create many files while the watcher is "suspended" (no events delivered),
    // then a rescan must reconcile the whole tree.
    let mut f = fixture();
    std::fs::create_dir_all(f.root.join("foo")).unwrap();
    f.settle();

    for i in 0..200 {
        std::fs::write(f.root.join(format!("foo/{i}")), b"x").unwrap();
    }

    // No events delivered; the watcher reports overflow.
    f.applier
        .pending_mut()
        .enqueue_rescan(f.root_id, RescanCause::Overflow);
    f.drain();
    f.drain();

    let paths = f.paths();
    assert!(paths.contains("foo"));
    for i in 0..200 {
        assert!(paths.contains(&format!("foo/{i}")), "missing foo/{i}");
    }
    assert_eq!(paths.len(), 1 + 200, "exactly foo + 200 files: {paths:?}");
}
