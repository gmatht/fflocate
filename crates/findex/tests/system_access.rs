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
