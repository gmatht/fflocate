//! The system daemon's access filter (§3).
//!
//! A privileged daemon answering unprivileged callers is the one place in this
//! project where a mistake is a security bug rather than a wrong answer. This
//! suite pins the properties that make it safe, from the inside out:
//!
//! * the ownership decision itself (owner / group / other, read vs traverse);
//! * that a *chain* is checked, not just the entry — a world-readable file under
//!   a `0700` directory is not readable;
//! * that the filter fails **closed**: no record, no visibility;
//! * that every query path (exact, fuzzy, expression, count) goes through the
//!   filter — the fuzzy and expression paths leaked before this suite existed.
//!
//! Live multi-user verification (a real root daemon, a real unprivileged caller)
//! is in `tests/system_live.rs`, because it needs a user and a group to exist.

//! Unix-only, like the system daemon itself (mode bits, chown, peercred have
//! no Windows meaning). The portable decision logic underneath is pinned by
//! `ownership.rs` unit tests, which run everywhere.
#![cfg(unix)]

use std::sync::Arc;

use findex::applier::{Applier, Shared};
use findex::index::{EntryFlags, Index, ROOT_ID};
use findex::modelfs::{model_root, ModelFs};
use findex::ownership::{Attr, Caller, Ownership};
use findex::persist::Store;
use findex::proto::{Request, Response};
use findex::roots::Root;
use findex::server::{handle_request, ServerCtx};
use findex::sync::SyncRegistry;

fn file(mode: u32, uid: u32, gid: u32) -> Attr {
    Attr { uid, gid, mode: 0o100000 | mode }
}
fn dir(mode: u32, uid: u32, gid: u32) -> Attr {
    Attr { uid, gid, mode: 0o040000 | mode }
}

/// A tree at `/model`:
///
/// ```text
/// /model            (root dir, 0755, root)
/// ├── public/       (0755, root)   readable.txt (0644, root)
/// └── private/      (0700, root)   secret.txt   (0600, root)
/// ```
///
/// `alice` (uid 1000) owns nothing; `bob` (uid 0's file but a different uid) is
/// the control. The interesting case is `private/secret.txt`: the *file* is only
/// 0600, but even a 0644 file inside `private/` must stay hidden, which is the
/// chain check.
struct Fixture {
    ctx: ServerCtx,
    _d: tempfile::TempDir,
    _shared: Arc<Shared>,
}

fn fixture(caller: Option<Caller>) -> Fixture {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open(&d.path().join("state")).unwrap();
    let mut ix = Index::new();
    let root = ix.insert(ROOT_ID, b"model", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
    let public = ix.insert(root, b"public", EntryFlags::EMPTY.with_dir(true));
    let private = ix.insert(root, b"private", EntryFlags::EMPTY.with_dir(true));
    let readable = ix.insert(public, b"readable.txt", EntryFlags::EMPTY);
    let also_readable = ix.insert(public, b"also-readable.txt", EntryFlags::EMPTY);
    // Deliberately world-readable *inside* the private dir: the chain must hide
    // it even though its own mode says otherwise.
    let leaky = ix.insert(private, b"world-readable.txt", EntryFlags::EMPTY);
    let secret = ix.insert(private, b"secret.txt", EntryFlags::EMPTY);

    let mut r = Root::new(0, model_root());
    r.caps.writable = true;
    let shared = Shared::new(ix, vec![r]);
    let applier = Applier::new(shared.clone(), store, Arc::new(ModelFs::new()), SyncRegistry::new());
    drop(applier);

    // Populate ownership directly — this suite is about the filter, not the crawl.
    {
        let mut own = shared.ownership.write().unwrap();
        own.set(root, dir(0o755, 0, 0));
        own.set(public, dir(0o755, 0, 0));
        own.set(private, dir(0o700, 0, 0));
        own.set(readable, file(0o644, 0, 0));
        own.set(also_readable, file(0o644, 0, 0));
        own.set(leaky, file(0o644, 0, 0)); // world-readable inside 0700
        own.set(secret, file(0o600, 0, 0));
    }

    let ctx = ServerCtx {
        shared: shared.clone(),
        sync: SyncRegistry::new(),
        backend: "fake".into(),
        control: None,
        caller,
    };
    Fixture { ctx, _d: d, _shared: shared }
}

fn hits(ctx: &ServerCtx, req: &Request) -> Vec<String> {
    handle_request(ctx, req)
        .iter()
        .filter_map(|r| match r {
            Response::Hit { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

fn counted(ctx: &ServerCtx, req: &Request) -> u64 {
    handle_request(ctx, req)
        .iter()
        .find_map(|r| match r {
            Response::Done { count, .. } => Some(*count),
            _ => None,
        })
        .unwrap()
}

#[test]
fn root_sees_everything() {
    let f = fixture(Some(Caller::new(0, 0)));
    let got = hits(&f.ctx, &Request::query("", 0, false, false));
    assert_eq!(got.len(), 6, "root sees the whole tree; got {got:?}");
}

#[test]
fn stranger_sees_only_the_world_readable_subtree() {
    let f = fixture(Some(Caller::new(1000, 1000)));
    let got = hits(&f.ctx, &Request::query("", 0, false, false));
    assert!(got.iter().any(|p| p.ends_with("/public/readable.txt")), "got {got:?}");
    assert!(!got.iter().any(|p| p.contains("private")), "0700 dir must hide its whole subtree; got {got:?}");
    assert_eq!(got.len(), 3, "public + its two files; got {got:?}");
}

#[test]
fn a_readable_file_inside_a_private_dir_stays_hidden() {
    // The chain check, which is the property most likely to be forgotten: the
    // file's own mode is 0644, so a naive per-entry filter would show it.
    let f = fixture(Some(Caller::new(1000, 1000)));
    let got = hits(&f.ctx, &Request::query("world-readable", 0, false, false));
    assert!(got.is_empty(), "a 0644 file under a 0700 dir is not readable; got {got:?}");
}

#[test]
fn count_respects_the_caller_not_root() {
    // A count that showed root's total would leak the existence of files the
    // caller cannot open — a smaller leak than a path, but a leak.
    let f = fixture(Some(Caller::new(1000, 1000)));
    assert_eq!(counted(&f.ctx, &Request::query("", 0, false, true)), 3);
    assert_eq!(counted(&f.ctx, &Request::query("secret", 0, false, true)), 0);
    let root = fixture(Some(Caller::new(0, 0)));
    assert_eq!(counted(&root.ctx, &Request::query("", 0, false, true)), 6);
}

#[test]
fn fuzzy_path_is_filtered_too() {
    // This one leaked before the filter was routed through a single place: the
    // fuzzy handler built its own response list and forgot the check.
    let f = fixture(Some(Caller::new(1000, 1000)));
    let req = Request::Query {
        q: "secret".into(),
        limit: 0,
        json: false,
        count: false,
        expression: None,
        glob: None,
        glob_includedotfiles: None,
        glob_noescape: None,
        suffix: None,
        path: None,
        relative_root: None,
        case_sensitive: None,
        fuzzy: true,
        max_distance: Some(1),
    };
    let got = hits(&f.ctx, &req);
    assert!(got.is_empty(), "fuzzy must be filtered like everything else; got {got:?}");
}

#[test]
fn expression_path_is_filtered_too() {
    let f = fixture(Some(Caller::new(1000, 1000)));
    let req = Request::Query {
        q: String::new(),
        limit: 0,
        json: false,
        count: false,
        expression: Some(serde_json::json!(["match", "*", "basename"])),
        glob: None,
        glob_includedotfiles: None,
        glob_noescape: None,
        suffix: None,
        path: None,
        relative_root: None,
        case_sensitive: None,
        fuzzy: false,
        max_distance: None,
    };
    let got = hits(&f.ctx, &req);
    assert!(!got.iter().any(|p| p.contains("private")), "got {got:?}");
    assert!(got.iter().any(|p| p.ends_with("/public/readable.txt")), "got {got:?}");
}

#[test]
fn no_caller_means_no_filtering_at_all() {
    // A per-user daemon already runs as its user; there is nothing to filter, and
    // the filter must not accidentally hide the user's own files.
    let f = fixture(None);
    let got = hits(&f.ctx, &Request::query("", 0, false, false));
    assert_eq!(got.len(), 6, "no caller ⇒ no filtering; got {got:?}");
}

#[test]
fn missing_ownership_record_fails_closed() {
    let f = fixture(Some(Caller::new(1000, 1000)));
    // An id with no record: root sees it, a stranger does not.
    let root = Ownership::new();
    assert!(root.visible_to(42, &Caller::new(0, 0)));
    assert!(!root.visible_to(42, &Caller::new(1000, 1000)));
    // And through the whole request path, an unrecorded file is not listed.
    let got = hits(&f.ctx, &Request::query("", 0, false, false));
    assert_eq!(got.len(), 3);
}

#[test]
fn group_membership_is_honoured_in_the_filter() {
    // A file 0640 owned by group 77: a caller whose primary or supplementary gid
    // is 77 may read it, others may not.
    let mut own = Ownership::new();
    own.set(1, file(0o640, 5000, 77));
    assert!(own.visible_to(1, &Caller::new(5001, 77)), "primary group");
    let mut c = Caller::new(5001, 9999);
    c.groups = &[77];
    assert!(own.visible_to(1, &c), "supplementary group");
    assert!(!own.visible_to(1, &Caller::new(5001, 9999)), "no group ⇒ no read");
}

// ---------------------------------------------------------------------------
// Ownership must be recorded by *every* path that creates an entry, not just
// the first one. Getting this wrong does not fail loudly: the filter fails
// closed, so a world-readable file simply vanishes from everyone but root.
// ---------------------------------------------------------------------------

/// A world-readable file discovered by a **rescan** (not by a watcher event) must
/// still be visible to an unprivileged caller. This was a real bug: ownership was
/// recorded in the crawl and the event path but not in the rescan diff, so after
/// a restart or any rescan, correct files were hidden.
#[test]
fn a_file_found_by_rescan_is_still_visible() {
    use std::sync::Arc;
    use findex::applier::{Applier, RealFs};
    use findex::event::RescanCause;

    let root = std::path::PathBuf::from("/tmp/findex-rescan-ownership");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("crawled.txt"), b"x").unwrap();

    let d = tempfile::tempdir().unwrap();
    let store = findex::persist::Store::open(&d.path().join("state")).unwrap();
    let mut ix = Index::new();
    let rid = ix.insert(
        ROOT_ID,
        b"findex-rescan-ownership",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let mut r = Root::new(0, root.clone());
    r.caps.writable = true;
    let shared = Shared::new(ix, vec![r]);
    let mut applier = Applier::new(shared.clone(), store, Arc::new(RealFs), SyncRegistry::new());
    applier.rebuild_dir_cache();
    applier.track_ownership(true);
    applier.pending_mut().enqueue_crawl(rid);
    applier.drain();

    // A file that appears on disk and is found by a rescan, never by an event.
    std::fs::write(root.join("rescanned.txt"), b"y").unwrap();
    applier.pending_mut().enqueue_rescan(rid, RescanCause::Manual);
    applier.drain();

    let ctx = ServerCtx {
        shared: shared.clone(),
        sync: SyncRegistry::new(),
        backend: "fake".into(),
        control: None,
        caller: Some(Caller::new(1000, 1000)),
    };
    let got = hits(&ctx, &Request::query("", 0, false, false));
    assert!(
        got.iter().any(|p| p.ends_with("/rescanned.txt")),
        "a rescan-discovered world-readable file must be visible; got {got:?}"
    );
    assert!(got.iter().any(|p| p.ends_with("/crawled.txt")), "got {got:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A file that is replaced under the same name must have its *ownership*
/// refreshed, not just its existence.
///
/// This is the dangerous direction. Ownership that is stale in the "hidden too
/// much" direction is annoying; stale in the other direction shows a file to
/// someone who has since lost the right to see it. `apply_upsert` refreshes the
/// entry's flags for an existing id, and must refresh its attributes too.
#[test]
fn re_upsert_refreshes_ownership() {
    use std::sync::Arc;
    use findex::applier::{Applier, RealFs};

    let root = std::path::PathBuf::from("/tmp/findex-refresh-ownership");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let victim = root.join("victim.txt");
    std::fs::write(&victim, b"x").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();

    let d = tempfile::tempdir().unwrap();
    let store = findex::persist::Store::open(&d.path().join("state")).unwrap();
    let mut ix = Index::new();
    let rid = ix.insert(
        ROOT_ID,
        b"findex-refresh-ownership",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let mut r = Root::new(0, root.clone());
    r.caps.writable = true;
    let shared = Shared::new(ix, vec![r]);
    let mut applier = Applier::new(shared.clone(), store, Arc::new(RealFs), SyncRegistry::new());
    applier.rebuild_dir_cache();
    applier.track_ownership(true);
    applier.pending_mut().enqueue_crawl(rid);
    applier.drain();

    let caller = Caller::new(1000, 1000);
    // While it is 0600, a stranger must not see it.
    let id = {
        let ix = shared.index.read().unwrap();
        let found = ix.iter_live().find(|(_, e)| !e.flags().is_root_dir()).map(|(id, _)| id);
        found.unwrap()
    };
    assert!(!shared.ownership.read().unwrap().visible_to(id, &caller), "0600 must be hidden");

    // The file is replaced, this time world-readable, under the same name. A
    // watcher reports this as an upsert for an id we already have.
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
    applier.push(findex::event::FsEvent::upsert(
        findex::event::DirKey::new(rid),
        b"victim.txt".to_vec(),
        false,
    ));
    applier.drain();

    assert!(
        shared.ownership.read().unwrap().visible_to(id, &caller),
        "after the file became 0644 the filter must say so, not repeat the old answer"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Compaction renumbers entry ids. The ownership sidecar is keyed by id, so it
/// must be remapped in the same breath — otherwise every record afterwards
/// describes whichever file moved into that slot, and the filter answers about
/// one file using another's permissions.
///
/// This is the bug that would be hardest to notice in production: it only goes
/// wrong after 100 000 records, and then only for whichever files happen to
/// shift, in whichever direction their neighbours' modes imply.
#[test]
fn ownership_survives_compaction() {
    use std::sync::Arc;
    use findex::applier::{Applier, RealFs};

    let root = std::path::PathBuf::from("/tmp/findex-compact-ownership");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    use std::os::unix::fs::PermissionsExt;
    // `early.txt` is created *first* so it takes the lowest id; deleting it later
    // forces compaction to renumber everything after it.
    std::fs::write(root.join("early.txt"), b"e").unwrap();
    // One world-readable and one owner-only file, at different ids.
    std::fs::write(root.join("open.txt"), b"x").unwrap();
    std::fs::write(root.join("shut.txt"), b"y").unwrap();
    std::fs::set_permissions(root.join("open.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(root.join("shut.txt"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(root.join("early.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();
    let early = root.join("early.txt");

    let d = tempfile::tempdir().unwrap();
    let store = findex::persist::Store::open(&d.path().join("state")).unwrap();
    let mut ix = Index::new();
    let rid = ix.insert(
        ROOT_ID,
        b"findex-compact-ownership",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let mut r = Root::new(0, root.clone());
    r.caps.writable = true;
    let shared = Shared::new(ix, vec![r]);
    let mut applier = Applier::new(shared.clone(), store, Arc::new(RealFs), SyncRegistry::new());
    applier.rebuild_dir_cache();
    applier.track_ownership(true);
    applier.pending_mut().enqueue_crawl(rid);
    applier.drain();

    let caller = Caller::new(1000, 1000);
    let visible = |shared: &Arc<Shared>| -> Vec<String> {
        let own = shared.ownership.read().unwrap();
        let ix = shared.index.read().unwrap();
        let mut v: Vec<String> = ix
            .iter_live()
            .filter(|(_, e)| !e.flags().is_root_dir())
            .filter(|(id, _)| own.visible_to(*id, &caller))
            .map(|(id, _)| String::from_utf8_lossy(ix.name(id)).into_owned())
            .collect();
        v.sort();
        v
    };
    let before = visible(&shared);
    assert_eq!(
        before,
        vec!["early.txt".to_string(), "open.txt".to_string()],
        "the two 0644 files are visible; shut.txt is not"
    );
    // Note the ids, so we can prove the renumbering really happened — a
    // compaction that happens to keep every id where it was would make this test
    // vacuous, which is exactly how the first version of it passed while the bug
    // was live.
    let ids_before: Vec<(u32, String)> = {
        let ix = shared.index.read().unwrap();
        let mut v: Vec<(u32, String)> = ix
            .iter_live()
            .map(|(i, _)| (i, String::from_utf8_lossy(ix.name(i)).into_owned()))
            .collect();
        v.sort();
        v
    };

    // Delete a file *earlier* in id order than the survivors, so compaction must
    // actually shift their ids. Without a tombstone ahead of them, renaming is a
    // no-op and the test proves nothing.
    std::fs::remove_file(&early).unwrap();
    applier.push(findex::event::FsEvent::remove(findex::event::DirKey::new(rid), b"early.txt".to_vec()));
    applier.drain();

    let compacted = applier.compact_now();
    assert!(compacted, "compaction should have run");
    let ids_after: Vec<(u32, String)> = {
        let ix = shared.index.read().unwrap();
        let mut v: Vec<(u32, String)> = ix
            .iter_live()
            .map(|(i, _)| (i, String::from_utf8_lossy(ix.name(i)).into_owned()))
            .collect();
        v.sort();
        v
    };
    assert!(
        ids_before.iter().any(|(id, name)| name != "early.txt" && !ids_after.contains(&(*id, name.clone()))),
        "ids must actually have been renumbered, or this test is vacuous: {ids_before:?} -> {ids_after:?}"
    );

    let after = visible(&shared);
    // `early.txt` was deleted, so it is gone from both; the survivors must be
    // exactly the ones that were visible before.
    let mut expected: Vec<String> = before.into_iter().filter(|n| n != "early.txt").collect();
    expected.sort();
    assert_eq!(
        after, expected,
        "compaction must not change *which* surviving files a caller can see; got {after:?}"
    );
    // The decisive check: `shut.txt` is 0600 and must still be hidden, and
    // `open.txt` must still be shown — after their ids moved.
    assert!(after.contains(&"open.txt".to_string()), "got {after:?}");
    assert!(!after.contains(&"shut.txt".to_string()), "got {after:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The most common case of all, and the one that broke loudest: a **restart**.
///
/// The snapshot restores entries but not ownership (which is derived from the
/// filesystem). The daemon's startup then re-crawls each root — but a crawl skips
/// entries that already exist, so without backfilling their attributes the filter
/// would find no record for anything and show an unprivileged caller *nothing at
/// all*. A system daemon that appears to work and answers every query with an
/// empty list is worse than one that refuses to start.
#[test]
fn ownership_is_backfilled_after_a_restart() {
    use std::sync::Arc;
    use findex::applier::{Applier, RealFs};

    let root = std::path::PathBuf::from("/tmp/findex-restart-ownership");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("visible.txt"), b"x").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.join("visible.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();

    let d = tempfile::tempdir().unwrap();
    let store = findex::persist::Store::open(&d.path().join("state")).unwrap();
    let mut ix = Index::new();
    let rid = ix.insert(
        ROOT_ID,
        b"findex-restart-ownership",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let mut r = Root::new(0, root.clone());
    r.caps.writable = true;
    let shared = Shared::new(ix, vec![r]);
    let mut applier = Applier::new(shared.clone(), store, Arc::new(RealFs), SyncRegistry::new());
    applier.rebuild_dir_cache();
    applier.track_ownership(true);
    applier.pending_mut().enqueue_crawl(rid);
    applier.drain();

    // A restart: the entries survive, the ownership table does not. (A real
    // `Daemon::build` makes a fresh `Pending`, so the crawl queue is empty; here
    // the applier is reused, so we drive a rescan instead — which is the stronger
    // requirement anyway, since it must refresh *survivors*, not just new files.)
    *shared.ownership.write().unwrap() = findex::ownership::Ownership::new();
    applier.rebuild_dir_cache(); // as a fresh Daemon::start would
    applier.pending_mut().enqueue_rescan(rid, findex::event::RescanCause::Manual);
    applier.drain();

    let ctx = ServerCtx {
        shared: shared.clone(),
        sync: SyncRegistry::new(),
        backend: "fake".into(),
        control: None,
        caller: Some(Caller::new(1000, 1000)),
    };
    let got = hits(&ctx, &Request::query("", 0, false, false));
    assert!(
        got.iter().any(|p| p.ends_with("/visible.txt")),
        "after a restart the re-crawl must backfill ownership, or the caller sees nothing; got {got:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A restart walks the whole tree, not just the directories that are new.
///
/// The snapshot restores *every* directory entry, so after a restart nothing is
/// new — and a crawl that only descended into new directories would never reach
/// the files inside the restored ones. Those files would have no ownership
/// record, so the filter would hide them, and a system daemon that had been
/// restarted would quietly stop showing any of its contents to non-root users.
///
/// This is what the previous test missed: wiping the table and re-crawling the
/// *root* is not the same as restarting, because the interesting question is
/// whether the crawl descends into directories it already knows about.
#[test]
fn a_restart_walks_into_already_known_directories() {
    use std::sync::Arc;
    use findex::applier::{Applier, RealFs};

    let root = std::path::PathBuf::from("/tmp/findex-restart-descent");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/deep.txt"), b"x").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(root.join("sub"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(root.join("sub/deep.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();

    let d = tempfile::tempdir().unwrap();
    let store = findex::persist::Store::open(&d.path().join("state")).unwrap();
    let mut ix = Index::new();
    let rid = ix.insert(
        ROOT_ID,
        b"findex-restart-descent",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let mut r = Root::new(0, root.clone());
    r.caps.writable = true;
    let shared = Shared::new(ix, vec![r]);
    let mut applier = Applier::new(shared.clone(), store, Arc::new(RealFs), SyncRegistry::new());
    applier.rebuild_dir_cache();
    applier.track_ownership(true);
    applier.pending_mut().enqueue_crawl(rid);
    applier.drain();

    // Restart: entries survive (nothing is "new" any more), ownership does not.
    // A fresh `Pending`, as `Daemon::build` makes.
    *shared.ownership.write().unwrap() = findex::ownership::Ownership::new();
    applier.pending_mut().reset_for_restart();
    applier.rebuild_dir_cache();
    applier.pending_mut().enqueue_crawl(rid);
    applier.drain();

    let ctx = ServerCtx {
        shared: shared.clone(),
        sync: SyncRegistry::new(),
        backend: "fake".into(),
        control: None,
        caller: Some(Caller::new(1000, 1000)),
    };
    let got = hits(&ctx, &Request::query("", 0, false, false));
    assert!(
        got.iter().any(|p| p.ends_with("/sub/deep.txt")),
        "a restart must reach files inside directories it already knew; got {got:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
