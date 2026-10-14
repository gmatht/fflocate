//! Exclusion filtering under a whole-mount backend (§4/§6).
//!
//! The design's "ignore asymmetry": inotify can decline to watch a subtree, so
//! the kernel never reports it. fanotify-FID and FSEvents watch whole mounts and
//! *cannot* — they must filter in the applier instead. This suite pins that the
//! applier does filter, and that the mechanism is the same one the crawl uses,
//! so the two can not drift apart.
//!
//! The rule being tested: an excluded **directory** is never inserted. Because
//! events name their parent by id, that single omission prunes the whole
//! subtree — no ancestor walk, and no way for a child event to slip in.

use std::sync::Arc;

use fflocate::applier::{Applier, Shared};
use fflocate::event::{DirKey, FsEvent};
use fflocate::excludes::Excludes;
use fflocate::index::{EntryFlags, Index, ROOT_ID};
use fflocate::modelfs::{model_root, ModelFs, Node};
use fflocate::persist::Store;
use fflocate::roots::Root;
use fflocate::sync::SyncRegistry;

fn harness_with(
    excludes: Excludes,
    model: ModelFs,
) -> (Applier, Arc<Shared>, tempfile::TempDir, u32) {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open(&d.path().join("state")).unwrap();
    let mut ix = Index::new();
    let root = ix.insert(
        ROOT_ID,
        b"model",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let mut r = Root::new(0, model_root());
    r.caps.writable = true;
    let shared = Shared::new(ix, vec![r]);
    let mut applier = Applier::new(shared.clone(), store, Arc::new(model), SyncRegistry::new());
    applier.set_excludes(excludes);
    applier.rebuild_dir_cache();
    (applier, shared, d, root)
}

/// Live index paths (excluding the root dir entry itself).
fn index_paths(shared: &Shared) -> Vec<String> {
    let ix = shared.index.read().unwrap();
    let mut out: Vec<String> = ix
        .iter_live()
        .filter(|(_, e)| !e.flags().is_root_dir())
        .filter_map(|(id, _)| ix.path_of(id, b"/model"))
        .map(|p| String::from_utf8_lossy(&p).into_owned())
        .collect();
    out.sort();
    out
}

#[test]
fn crawl_skips_an_excluded_subtree() {
    // The model has a vendored dir with a file inside it, a sibling we do want,
    // and a file at the top level.
    let model = ModelFs::with([
        ("node_modules", Node::dir(&["left-pad"])),
        ("src", Node::dir(&["main.rs"])),
        ("README.md", Node::File),
    ]);
    let (mut applier, shared, _d, root) = harness_with(Excludes::defaults(), model);
    applier.pending_mut().enqueue_crawl(root);
    applier.drain();

    let paths = index_paths(&shared);
    assert!(paths.contains(&"/model/src".to_string()), "got {paths:?}");
    assert!(
        paths.contains(&"/model/src/main.rs".to_string()),
        "got {paths:?}"
    );
    assert!(
        paths.contains(&"/model/README.md".to_string()),
        "got {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains("node_modules")),
        "an excluded dir and its whole subtree must not be indexed; got {paths:?}"
    );
    assert!(
        applier.excluded_names > 0,
        "the exclusion must be counted, not silent"
    );
}

#[test]
fn whole_mount_events_cannot_reintroduce_an_excluded_dir() {
    // This is the fanotify case: the kernel *does* report the excluded dir
    // (a whole-mount watch cannot decline it), and the applier must refuse it.
    let model = ModelFs::new();
    let (mut applier, shared, _d, _root) = harness_with(Excludes::defaults(), model);

    applier.drain();
    // The event names the root dir as parent; a whole-mount watch would deliver
    // exactly this for `mkdir node_modules`.
    applier.push(FsEvent::upsert(
        DirKey::new(1),
        b"node_modules".to_vec(),
        true,
    ));
    // And an unrelated create that must still work.
    applier.push(FsEvent::upsert(DirKey::new(1), b"keep.rs".to_vec(), false));
    applier.drain();

    let paths = index_paths(&shared);
    assert!(
        !paths.iter().any(|p| p.contains("node_modules")),
        "got {paths:?}"
    );
    assert!(
        paths.contains(&"/model/keep.rs".to_string()),
        "unrelated creates still work; got {paths:?}"
    );
    assert!(
        applier.excluded_names > 0,
        "the refused dir must be counted"
    );
}

#[test]
fn excluded_subtree_stays_empty_even_for_child_events() {
    // Even if a child event arrives for the excluded dir's index id — which a
    // real whole-mount watch could produce if the dir existed before we started
    // — nothing under it may be indexed, because it was never inserted.
    let model = ModelFs::new();
    let (mut applier, shared, _d, _root) = harness_with(Excludes::defaults(), model);
    applier.drain();
    applier.push(FsEvent::upsert(DirKey::new(1), b".git".to_vec(), true));
    applier.drain();

    // A child of ".git": parent is a dir id we never created. The applier
    // resolves parent by id, so an unknown id is simply dropped.
    applier.push(FsEvent::upsert(DirKey::new(999), b"config".to_vec(), false));
    applier.drain();

    let paths = index_paths(&shared);
    assert!(!paths.iter().any(|p| p.contains(".git")), "got {paths:?}");
}

#[test]
fn exclusions_off_by_default_so_nothing_surprises() {
    // A daemon with no exclusion config must index exactly what is there.
    let model = ModelFs::with([
        ("node_modules", Node::dir(&["left-pad"])),
        ("src", Node::dir(&["main.rs"])),
    ]);
    let (mut applier, shared, _d, root) = harness_with(Excludes::new(), model);
    applier.pending_mut().enqueue_crawl(root);
    applier.drain();
    let paths = index_paths(&shared);
    assert!(
        paths.iter().any(|p| p.contains("node_modules")),
        "with no exclusions configured, nothing may be filtered; got {paths:?}"
    );
    assert_eq!(applier.excluded_names, 0);
}
