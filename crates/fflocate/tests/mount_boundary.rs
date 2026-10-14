//! Mount-aware crawl tests (§4): one database, one root per filesystem.
//!
//! A crawl never crosses a mount boundary: each filesystem is indexed through
//! its own root, nested mounts are indexed exactly once, and mounts that are
//! not roots never leak in via a parent root. The mountpoint directory entry
//! itself is still recorded; only the descend (watch + enumerate) stops.

use std::collections::HashSet;
use std::sync::Arc;

use fflocate::applier::{root_dir_id, Applier, Shared};
use fflocate::event::RescanCause;
use fflocate::index::{EntryFlags, Index, ROOT_ID};
use fflocate::modelfs::{ModelFs, Node};
use fflocate::persist::Store;
use fflocate::roots::{root_entry_name, Root};
use fflocate::sync::SyncRegistry;

fn mps(paths: &[&str]) -> HashSet<Vec<u8>> {
    paths.iter().map(|p| p.as_bytes().to_vec()).collect()
}

/// Build an applier over a model filesystem with the given roots, each with
/// a path-qualified root entry exactly like the daemon creates, mountpoints
/// installed. Returns the applier, shared state, tempdir guard and the
/// resolved root-dir ids in root order.
fn harness(
    model: ModelFs,
    root_paths: &[&str],
    mountpoints: HashSet<Vec<u8>>,
) -> (Applier, Arc<Shared>, tempfile::TempDir, Vec<u32>) {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open(&d.path().join("state")).unwrap();
    let mut ix = Index::new();
    let mut roots = Vec::new();
    for (i, rp) in root_paths.iter().enumerate() {
        ix.insert(
            ROOT_ID,
            &root_entry_name(std::path::Path::new(rp)),
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let mut r = Root::new(i as u32, std::path::PathBuf::from(rp));
        r.caps.writable = true;
        roots.push(r);
    }
    let shared = Shared::new(ix, roots);
    let mut applier = Applier::new(shared.clone(), store, Arc::new(model), SyncRegistry::new());
    applier.set_mountpoints(mountpoints);
    applier.rebuild_dir_cache();
    let rids: Vec<u32> = {
        let ix = shared.index.read().unwrap();
        let roots = shared.roots.read().unwrap();
        roots.iter().map(|r| root_dir_id(&ix, r)).collect()
    };
    (applier, shared, d, rids)
}

/// `path -> is_dir` for everything the query path could surface under `root`.
fn index_paths_under(shared: &Shared, root: &[u8]) -> std::collections::BTreeMap<String, bool> {
    let ix = shared.index.read().unwrap();
    let mut out = std::collections::BTreeMap::new();
    for (id, e) in ix.iter_live() {
        if e.flags().is_root_dir() {
            continue;
        }
        // Only entries whose chain reaches *this* root (a second root's
        // subtree must not leak into the first root's view).
        let mut cur = id;
        let mut ok = false;
        loop {
            if cur == ROOT_ID {
                break;
            }
            let Some(entry) = ix.entry(cur) else { break };
            if entry.flags().is_root_dir() {
                ok = ix.name(cur) == root;
                break;
            }
            cur = entry.parent;
        }
        if ok {
            if let Some(p) = ix.path_of(id, root) {
                out.insert(String::from_utf8_lossy(&p).into_owned(), e.flags().is_dir());
            }
        }
    }
    out
}

fn model_with_mount() -> ModelFs {
    let mut mnt_children = std::collections::BTreeMap::new();
    mnt_children.insert(b"inner.rs".to_vec(), Node::File);
    ModelFs::with([("a.rs", Node::File), ("mnt", Node::Dir(mnt_children))])
}

#[test]
fn crawl_stops_at_a_mount_boundary() {
    let (mut a, shared, _d, rids) = harness(model_with_mount(), &["/model"], mps(&["/model/mnt"]));
    a.pending_mut().enqueue_crawl(rids[0]);
    a.drain();

    let paths = index_paths_under(&shared, b"/model");
    assert_eq!(paths.get("/model/a.rs"), Some(&false));
    // The mountpoint itself is recorded as a leaf ...
    assert_eq!(paths.get("/model/mnt"), Some(&true));
    // ... but its filesystem is never enumerated through the parent root.
    assert!(
        !paths.contains_key("/model/mnt/inner.rs"),
        "must not cross the mount: {paths:?}"
    );

    // ... and no watch is installed for it either.
    let watches = a.take_watch_requests();
    assert!(
        !watches
            .iter()
            .any(|(_, p)| p.to_string_lossy() == "/model/mnt"),
        "a skipped mount must not be watched: {watches:?}"
    );
}

#[test]
fn rescan_heals_legacy_leakage_beneath_a_mount() {
    // A store written before boundaries existed has content under the mount.
    let (mut a, shared, _d, rids) = harness(model_with_mount(), &["/model"], mps(&[]));
    a.pending_mut().enqueue_crawl(rids[0]);
    a.drain();
    assert!(index_paths_under(&shared, b"/model").contains_key("/model/mnt/inner.rs"));

    // Boundaries arrive (daemon restart with mountinfo): the next rescan of
    // the parent drops the leaked subtree and keeps the mountpoint leaf.
    a.set_mountpoints(mps(&["/model/mnt"]));
    a.pending_mut().enqueue_rescan(rids[0], RescanCause::Manual);
    a.drain();

    let paths = index_paths_under(&shared, b"/model");
    assert!(
        !paths.contains_key("/model/mnt/inner.rs"),
        "rescan must drop the leaked subtree: {paths:?}"
    );
    assert_eq!(
        paths.get("/model/mnt"),
        Some(&true),
        "the mountpoint leaf itself stays"
    );
}

#[test]
fn a_nested_root_indexes_its_own_filesystem_exactly_once() {
    let (mut a, shared, _d, rids) = harness(
        model_with_mount(),
        &["/model", "/model/mnt"],
        mps(&["/model/mnt"]),
    );
    assert_ne!(rids[0], rids[1], "same-basename roots must not collide");
    for rid in rids.clone() {
        a.pending_mut().enqueue_crawl(rid);
    }
    a.drain();

    // The parent root's view stops at the boundary ...
    let parent = index_paths_under(&shared, b"/model");
    assert!(!parent.contains_key("/model/mnt/inner.rs"), "{parent:?}");
    // ... and the nested root's view holds the file, exactly once.
    let nested = index_paths_under(&shared, b"/model/mnt");
    assert_eq!(
        nested.get("/model/mnt/inner.rs"),
        Some(&false),
        "{nested:?}"
    );
    let ix = shared.index.read().unwrap();
    let inners: Vec<u32> = ix
        .iter_live()
        .filter(|(id, e)| !e.flags().is_root_dir() && ix.name(*id) == b"inner.rs")
        .map(|(id, _)| id)
        .collect();
    assert_eq!(inners.len(), 1, "one filesystem, one entry");
}

#[test]
fn same_basename_roots_resolve_independently() {
    // `/model/a` (legacy basename entry, as an old store holds it) and
    // `/x/a` (path-qualified): each root must find its own entry.
    let d = tempfile::tempdir().unwrap();
    let store = Store::open(&d.path().join("state")).unwrap();
    let mut ix = Index::new();
    let legacy = ix.insert(
        ROOT_ID,
        b"a",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let qualified = ix.insert(
        ROOT_ID,
        b"/x/a",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let roots = vec![
        Root::new(0, std::path::PathBuf::from("/model/a")),
        Root::new(1, std::path::PathBuf::from("/x/a")),
    ];
    let shared = Shared::new(ix, roots);
    let _applier = Applier::new(
        shared.clone(),
        store,
        Arc::new(ModelFs::new()),
        SyncRegistry::new(),
    );
    let ix = shared.index.read().unwrap();
    let roots = shared.roots.read().unwrap();
    // `/model/a` has no path-qualified entry, so it adopts the legacy one —
    // exactly what an upgraded store does.
    assert_eq!(root_dir_id(&ix, &roots[0]), legacy);
    assert_eq!(root_dir_id(&ix, &roots[1]), qualified);
    assert_ne!(legacy, qualified);
}
