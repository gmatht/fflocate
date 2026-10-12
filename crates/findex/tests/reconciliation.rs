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
    assert_eq!(
        paths.get("/model/ghost.rs"),
        Some(&false),
        "rescan must discover the missed create: {paths:?}"
    );
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
    assert!(
        !paths.contains_key("/model/a.rs"),
        "rescan must drop the deleted file"
    );
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
    assert_eq!(
        before, after,
        "survivor ids must not change across a rescan"
    );
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

    a.push(FsEvent::upsert(
        DirKey::new(root),
        b"phantom.rs".to_vec(),
        false,
    ));
    a.push(FsEvent::upsert(
        DirKey::new(root),
        b"real.rs".to_vec(),
        false,
    ));
    a.push(FsEvent::upsert(
        DirKey::new(root),
        b"real.rs".to_vec(),
        false,
    ));
    a.push(FsEvent::remove(DirKey::new(root), b"real.rs".to_vec()));
    a.drain();

    // Now heal with a rescan.
    a.pending_mut().enqueue_rescan(root, RescanCause::Manual);
    a.drain();

    let paths = index_paths(&shared);
    // `phantom.rs` was upserted (the index believed the lie) but the rescan must
    // discover it does not exist and drop it.
    assert!(
        !paths.contains_key("/model/phantom.rs"),
        "rescan must drop the phantom: {paths:?}"
    );
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
        a.push(FsEvent::upsert(
            DirKey::new(root),
            format!("junk{i}").into_bytes(),
            false,
        ));
    }
    a.drain();

    let paths = index_paths(&shared);
    // The rescan diff drops all the junk files that do not exist...
    assert!(
        !paths.keys().any(|k| k.contains("junk")),
        "rescan must purge phantom junk: {paths:?}"
    );
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
    a.push(FsEvent::rescan(
        DirKey::new(root),
        RescanCause::WatchDropped,
    ));
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

#[test]
fn restart_recrawl_rerequests_watches_for_known_dirs() {
    // The /root/wavtest incident: after a snapshot/journal replay every
    // directory is "known", and the crawl walked them without re-requesting
    // watches — so the daemon held one watch (the root) and went blind to
    // every create until a rescan. The crawl of known dirs must record watch
    // requests exactly like the crawl of new dirs.
    //
    // A restart is modelled faithfully: a FRESH applier over the already
    // populated index (a new process has empty `watch_requested` dedup state,
    // so reusing the first applier would prove nothing either way).
    //
    // Asserts on what the code under test itself recorded
    // (`take_watch_requests`), not a recomputation.
    let model = ModelFs::new();
    model.set_root_children(vec![(
        b"src".to_vec(),
        Node::Dir(
            [(b"inner".to_vec(), Node::Dir([].into_iter().collect()))]
                .into_iter()
                .collect(),
        ),
    )]);
    let (mut a, shared, d, root) = harness(model.clone());

    // First pass: fresh crawl (all dirs new) — drains queued sub-crawls too.
    a.pending_mut().enqueue_crawl(root);
    a.drain();
    a.drain();
    assert_eq!(index_paths(&shared), model.flatten("/model"));
    // Fresh crawl requests watches for everything it found.
    let fresh_reqs: std::collections::HashSet<std::path::PathBuf> = a
        .take_watch_requests()
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    assert!(
        fresh_reqs.iter().any(|p| p.ends_with("src")),
        "new-dir crawl must request src: {fresh_reqs:?}"
    );
    assert!(
        fresh_reqs.iter().any(|p| p.ends_with("src/inner")),
        "new-dir crawl must request inner: {fresh_reqs:?}"
    );

    // Second pass: a new applier over the populated index, same model fs.
    // Every dir is now "known" — the recrawl must still request every watch.
    let store2 = findex::persist::Store::open(&d.path().join("state2")).unwrap();
    let mut a2 = Applier::new(
        shared.clone(),
        store2,
        Arc::new(model.clone()),
        findex::sync::SyncRegistry::new(),
    );
    a2.rebuild_dir_cache();
    a2.pending_mut().enqueue_crawl(root);
    a2.drain();
    a2.drain();
    let re_reqs: std::collections::HashSet<std::path::PathBuf> = a2
        .take_watch_requests()
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    assert!(
        re_reqs.iter().any(|p| p.ends_with("src")),
        "known-dir recrawl must re-request src: {re_reqs:?}"
    );
    assert!(
        re_reqs.iter().any(|p| p.ends_with("src/inner")),
        "known-dir recrawl must re-request inner: {re_reqs:?}"
    );
}

#[test]
fn rescan_rerequests_watches_for_surviving_dirs() {
    // Same incident at the rescan level: survivors are never "new", so a
    // rescan that only requests watches for creations leaves restarted
    // subtrees unwatched just the same. Modelled with a fresh applier over
    // the populated index (a real restart has empty dedup state; reusing one
    // applier would let the request dedup mask a missing call).
    let model = ModelFs::new();
    model.set_root_children(vec![(
        b"src".to_vec(),
        Node::Dir([(b"keep.rs".to_vec(), Node::File)].into_iter().collect()),
    )]);
    let (mut a, shared, d, root) = harness(model.clone());
    a.pending_mut().enqueue_crawl(root);
    a.drain();
    a.drain();
    assert_eq!(index_paths(&shared), model.flatten("/model"));

    let store2 = findex::persist::Store::open(&d.path().join("state2")).unwrap();
    let mut a2 = Applier::new(
        shared.clone(),
        store2,
        Arc::new(model.clone()),
        findex::sync::SyncRegistry::new(),
    );
    a2.rebuild_dir_cache();
    // Nothing changed; rescan walks survivors only.
    a2.pending_mut()
        .enqueue_rescan(root, findex::event::RescanCause::Manual);
    a2.drain();
    let reqs: std::collections::HashSet<std::path::PathBuf> = a2
        .take_watch_requests()
        .into_iter()
        .map(|(_, p)| p)
        .collect();
    assert!(
        reqs.iter().any(|p| p.ends_with("src")),
        "rescan must re-request the surviving dir: {reqs:?}"
    );
    assert_eq!(index_paths(&shared), model.flatten("/model"));
}

#[test]
fn collapse_storm_converges_through_parents() {
    use findex::pending::RESCAN_BUDGET;
    // 300 sibling dirs under the root, each with one file. All files appear
    // silently (the untar race: listed-empty, filled before any watch), then
    // 300 overflow rescans arrive — one per dir. Past budget the queue must
    // fold siblings into the root instead of growing to 300 items, and the
    // drain must still converge to full truth.
    let model = ModelFs::new();
    let mut kids = Vec::new();
    for i in 0..300u32 {
        let d = format!("dir{i:03}");
        kids.push((
            d.as_bytes().to_vec(),
            Node::Dir([(b"f.txt".to_vec(), Node::File)].into_iter().collect()),
        ));
    }
    // Crawl the *empty* root first (pre-untar listing), so every dir is known
    // but empty — exactly the stale-partial state.
    model.set_root_children(vec![]);
    let (mut a, shared, _d, root) = harness(model.clone());
    a.pending_mut().enqueue_crawl(root);
    a.drain();
    assert!(index_paths(&shared).is_empty());

    // The untar lands: 300 dirs with files, zero events delivered.
    model.set_root_children(kids);

    // Storm: one overflow rescan per dir id. Dir ids are unknown before the
    // crawl discovers them, so resolve via root rescan of... no — enqueue
    // rescans for the *root* repeatedly? That coalesces to one. Instead: crawl
    // the root (discovers all 300 dirs as empty-then-full? no...).
    //
    // Direct approach: crawl root to learn the 300 dirs, then enqueue one
    // overflow rescan per discovered dir id.
    a.pending_mut().enqueue_crawl(root);
    a.drain();
    let dir_ids: Vec<u32> = {
        let ix = shared.index.read().unwrap();
        ix.children(root)
    };
    assert_eq!(dir_ids.len(), 300, "crawl must discover all 300 dirs");
    for id in &dir_ids {
        a.pending_mut().enqueue_rescan(*id, RescanCause::Overflow);
    }
    // The set held at budget: 300 dirs folded into far fewer items.
    let queued = a.pending().work_len();
    assert!(
        queued <= RESCAN_BUDGET + 1,
        "collapse must bound the set: {queued} items for 300 dirs"
    );
    assert!(
        a.pending().rescans_collapsed() > 0,
        "the fold must have fired and been counted"
    );
    a.drain();
    // Full convergence despite the fold: every file found through parents.
    let paths = index_paths(&shared);
    assert_eq!(
        paths,
        model.flatten("/model"),
        "folded rescans must still converge"
    );
    assert_eq!(paths.len(), 600, "300 dirs + 300 files");
}
