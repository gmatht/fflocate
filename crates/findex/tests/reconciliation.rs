//! Model-based reconciliation tests (§14.1).
//!
//! These drive the applier through its public surface with the in-memory
//! [`ModelFs`], inject arbitrary event sequences (including events that *lie*),
//! and assert that a diff-based rescan converges the index to the model's
//! truth. They run in milliseconds.

use std::sync::Arc;

use findex::applier::{Applier, Shared};
use findex::event::{DirKey, FsEvent, RescanCause};
use findex::index::{EntryFlags, Index, ROOT_ID};
use findex::modelfs::{model_root, ModelFs, Node};
use findex::persist::Store;
use findex::roots::Root;
use findex::sync::SyncRegistry;

/// Build an applier over a model filesystem whose root dir is indexed.
fn harness(model: ModelFs) -> (Applier, Arc<Shared>, tempfile::TempDir, u32) {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open(&d.path().join("state")).unwrap();

    let mut ix = Index::new();
    let root = ix.insert(
        ROOT_ID,
        b"model",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    // Seed the single top-level child so abs_path resolution works:
    // the model root is /model, and `abs_path_for(root)` is /model.
    let mut r = Root::new(0, model_root());
    r.caps.writable = true;
    let shared = Shared::new(ix, vec![r]);

    let mut applier = Applier::new(shared.clone(), store, Arc::new(model), SyncRegistry::new());
    // Prime the path cache for the root dir.
    applier.rebuild_dir_cache();
    (applier, shared, d, root)
}

/// Flatten the *index* into `path -> is_dir`, using the model root prefix.
/// Only live entries whose chain reaches the root dir are included, mirroring
/// what a query would surface.
fn index_paths(shared: &Shared) -> std::collections::BTreeMap<String, bool> {
    let ix = shared.index.read().unwrap();
    let mut out = std::collections::BTreeMap::new();
    let root_path = b"/model".to_vec();
    for (id, e) in ix.iter_live() {
        if e.flags().is_root_dir() {
            continue;
        }
        if let Some(p) = ix.path_of(id, &root_path) {
            out.insert(String::from_utf8_lossy(&p).into_owned(), e.flags().is_dir());
        }
    }
    out
}

#[test]
fn rescan_converges_after_a_missed_create() {
    // Start with an empty model, crawl it, then create a file on disk with NO
    // event delivered, then rescan: the diff must discover it.
    let model = ModelFs::new();
    model.set_root_children(vec![]);
    let (mut a, shared, _d, root) = harness(model.clone());

    a.pending_mut().enqueue_crawl(root);
    a.drain();
    assert!(index_paths(&shared).is_empty());

    // The filesystem grows, silently.
    model.put(b"ghost.rs", Node::File);

    // A rescan heals it (the §7 promise: degrade to work, never to silence).
    a.pending_mut().enqueue_rescan(root, RescanCause::Manual);
    a.drain();

    let paths = index_paths(&shared);
    assert_eq!(paths.get("/model/ghost.rs"), Some(&false), "rescan must discover the missed create: {paths:?}");
    assert_eq!(paths, model.flatten("/model"));
}

#[test]
fn rescan_converges_after_a_missed_delete() {
    let model = ModelFs::new();
    model.set_root_children(vec![(b"a.rs".to_vec(), Node::File)]);
    let (mut a, shared, _d, root) = harness(model.clone());
    a.pending_mut().enqueue_crawl(root);
    a.drain();
    assert!(index_paths(&shared).contains_key("/model/a.rs"));

    model.remove(b"a.rs");
    a.pending_mut().enqueue_rescan(root, RescanCause::Manual);
    a.drain();

    let paths = index_paths(&shared);
    assert!(!paths.contains_key("/model/a.rs"), "rescan must drop the deleted file");
    assert_eq!(paths, model.flatten("/model"));
}

#[test]
fn rescan_keeps_survivor_ids_stable() {
    // §3/§7: a rescan must keep ids for surviving entries.
    let model = ModelFs::new();
    model.set_root_children(vec![
        (b"keep.rs".to_vec(), Node::File),
        (b"gone.rs".to_vec(), Node::File),
    ]);
    let (mut a, shared, _d, root) = harness(model.clone());
    a.pending_mut().enqueue_crawl(root);
    a.drain();

    let before = {
        let ix = shared.index.read().unwrap();
        ix.lookup(root, b"keep.rs").unwrap()
    };

    model.remove(b"gone.rs");
    model.put(b"new.rs", Node::File);
    a.pending_mut().enqueue_rescan(root, RescanCause::Manual);
    a.drain();

    let after = {
        let ix = shared.index.read().unwrap();
        ix.lookup(root, b"keep.rs").unwrap()
    };
    assert_eq!(before, after, "survivor ids must not change across a rescan");
}

#[test]
fn nested_dirs_are_discovered_recursively() {
    let model = ModelFs::new();
    model.set_root_children(vec![(
        b"src".to_vec(),
        Node::Dir(
            [(
                b"inner".to_vec(),
                Node::Dir([(b"deep.rs".to_vec(), Node::File)].into_iter().collect()),
            )]
            .into_iter()
            .collect(),
        ),
    )]);
    let (mut a, shared, _d, root) = harness(model.clone());
    a.pending_mut().enqueue_crawl(root);
    a.drain();
    // Crawl enqueues crawls for new dirs; drain again to finish.
    a.drain();

    let paths = index_paths(&shared);
    assert_eq!(paths, model.flatten("/model"));
    assert!(paths.contains_key("/model/src/inner/deep.rs"));
}

#[test]
fn lying_event_stream_still_converges() {
    // Inject a stream of events that lie: a create for a nonexistent file, a
    // duplicate upsert, a remove for something that exists, then a rescan.
    let model = ModelFs::new();
    model.set_root_children(vec![(b"real.rs".to_vec(), Node::File)]);
    let (mut a, shared, _d, root) = harness(model.clone());
    a.pending_mut().enqueue_crawl(root);
    a.drain();

    a.push(FsEvent::upsert(DirKey::new(root), b"phantom.rs".to_vec(), false));
    a.push(FsEvent::upsert(DirKey::new(root), b"real.rs".to_vec(), false));
    a.push(FsEvent::upsert(DirKey::new(root), b"real.rs".to_vec(), false));
    a.push(FsEvent::remove(DirKey::new(root), b"real.rs".to_vec()));
    a.drain();

    // Now heal with a rescan.
    a.pending_mut().enqueue_rescan(root, RescanCause::Manual);
    a.drain();

    let paths = index_paths(&shared);
    // `phantom.rs` was upserted (the index believed the lie) but the rescan must
    // discover it does not exist and drop it.
    assert!(!paths.contains_key("/model/phantom.rs"), "rescan must drop the phantom: {paths:?}");
    assert_eq!(paths, model.flatten("/model"));
}

#[test]
fn overflow_escalates_and_converges() {
    // A tiny pending capacity forces an overflow escalation to a rescan.
    let model = ModelFs::new();
    model.set_root_children(vec![]);
    let (mut a, shared, _d, root) = harness(model.clone());
    a.pending_mut().enqueue_crawl(root);
    a.drain();

    model.put(b"during-flood.rs", Node::File);

    // Shrink the queue so 200 events reliably overflow and escalate.
    a.pending_mut().capacity = 16;
    for i in 0..200 {
        a.push(FsEvent::upsert(DirKey::new(root), format!("junk{i}").into_bytes(), false));
    }
    a.drain();

    let paths = index_paths(&shared);
    // The rescan diff drops all the junk files that do not exist...
    assert!(!paths.keys().any(|k| k.contains("junk")), "rescan must purge phantom junk: {paths:?}");
    // ...and discovers the real file that appeared during the flood.
    assert!(paths.contains_key("/model/during-flood.rs"));
}

#[test]
fn dropped_watch_triggers_rescan() {
    let model = ModelFs::new();
    model.set_root_children(vec![]);
    let (mut a, shared, _d, root) = harness(model.clone());
    a.pending_mut().enqueue_crawl(root);
    a.drain();

    model.put(b"after-drop.rs", Node::File);
    a.push(FsEvent::rescan(DirKey::new(root), RescanCause::WatchDropped));
    a.drain();

    assert!(index_paths(&shared).contains_key("/model/after-drop.rs"));
}

#[test]
fn generation_bumps_on_root_rescan() {
    let model = ModelFs::new();
    model.set_root_children(vec![]);
    let (mut a, shared, _d, root) = harness(model.clone());
    let gen_before = shared.generation();
    a.pending_mut().enqueue_rescan(root, RescanCause::Manual);
    a.drain();
    assert!(
        shared.generation() > gen_before,
        "a full-root rescan must bump the generation so cursors invalidate (§9)"
    );
}
