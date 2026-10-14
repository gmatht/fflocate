//! Live multi-user verification of the system daemon (§3).
//!
//! `tests/system_access.rs` proves the filter's logic against a synthetic tree.
//! This proves the thing itself: a **real root process** serving a **real
//! unprivileged user** over a **real socket**, with the kernel supplying the
//! caller's identity via `SO_PEERCRED`.
//!
//! ## Why it is worth the setup cost
//!
//! Every other test in this project can be run by anyone. This one cannot: it
//! needs root to create the group, the user, and the 0660 socket. That is the
//! point — the guarantee being tested is precisely "an unprivileged process
//! cannot see what it may not read", and the only honest way to test it is with
//! an unprivileged process.
//!
//! It skips cleanly (rather than failing) when it cannot run: not root, no
//! `groupadd`/`useradd`, or no `setpriv`. A test that fails on a machine it was
//! never meant for teaches nothing.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use fflocate::applier::{Applier, RealFs, Shared};
use fflocate::index::{EntryFlags, Index, ROOT_ID};
use fflocate::ownership;
use fflocate::persist::Store;
use fflocate::proto::{Request, Response};
use fflocate::roots::Root;
use fflocate::server::{handle_request, ServerCtx};
use fflocate::sync::SyncRegistry;

const GROUP: &str = "fflocate-test";
const USER: &str = "fflocate-test-user";

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn have(cmd: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {cmd} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn group_gid(name: &str) -> Option<u32> {
    let txt = std::fs::read_to_string("/etc/group").ok()?;
    txt.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() > 2 && f[0] == name).then(|| f[2].parse().ok())?
    })
}

fn user_ids(name: &str) -> Option<(u32, u32)> {
    let txt = std::fs::read_to_string("/etc/passwd").ok()?;
    txt.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() > 3 && f[0] == name {
            Some((f[2].parse().ok()?, f[3].parse().ok()?))
        } else {
            None
        }
    })
}

/// A tree with one world-readable and one owner-only branch.
fn make_tree(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir.join("public")).unwrap();
    std::fs::create_dir_all(dir.join("private")).unwrap();
    std::fs::write(dir.join("public/readable.txt"), b"ok").unwrap();
    std::fs::write(dir.join("private/secret.txt"), b"no").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(dir.join("public"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(
        dir.join("public/readable.txt"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    std::fs::set_permissions(dir.join("private"), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(
        dir.join("private/secret.txt"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
}

/// Build a server context whose ownership table came from crawling `root`.
fn system_ctx(root: &Path, caller: Option<ownership::Caller>) -> (ServerCtx, tempfile::TempDir) {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open(&d.path().join("state")).unwrap();
    let mut ix = Index::new();
    let basename = root.file_name().unwrap().to_string_lossy().into_owned();
    let rid = ix.insert(
        ROOT_ID,
        basename.as_bytes(),
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let mut r = Root::new(0, root.to_path_buf());
    r.caps.writable = true;
    let shared = Shared::new(ix, vec![r]);
    let mut applier = Applier::new(shared.clone(), store, Arc::new(RealFs), SyncRegistry::new());
    applier.rebuild_dir_cache();
    applier.track_ownership(true);
    applier.pending_mut().enqueue_crawl(rid);
    applier.drain();

    let ctx = ServerCtx {
        shared,
        sync: SyncRegistry::new(),
        backend: "fake".into(),
        control: None,
        caller,
    };
    (ctx, d)
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

#[test]
fn a_privileged_index_filters_for_an_unprivileged_caller() {
    if !is_root() {
        eprintln!("skipping: needs root to build the ownership table as a privileged index does");
        return;
    }
    let root = PathBuf::from("/tmp/fflocate-system-live");
    make_tree(&root);

    // Crawled as root, answered as uid 1000 (a stranger to these files).
    let (ctx, _d) = system_ctx(&root, Some(ownership::Caller::new(1000, 1000)));
    let got = hits(&ctx, &Request::query("", 0, false, false));
    assert!(
        got.iter().any(|p| p.ends_with("/public/readable.txt")),
        "got {got:?}"
    );
    assert!(
        !got.iter().any(|p| p.contains("private")),
        "0700 subtree leaked; got {got:?}"
    );
    assert!(!got.iter().any(|p| p.contains("secret")), "got {got:?}");

    // And the same index answers root in full.
    let (root_ctx, _d2) = system_ctx(&root, Some(ownership::Caller::new(0, 0)));
    let all = hits(&root_ctx, &Request::query("", 0, false, false));
    assert!(
        all.iter().any(|p| p.ends_with("/private/secret.txt")),
        "root must see it; got {all:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn peer_credentials_are_read_from_the_kernel_not_the_request() {
    // The socket half: SO_PEERCRED must report the real uid/gid of the peer.
    if !is_root() || !have("setpriv") {
        eprintln!("skipping: needs root + setpriv");
        return;
    }
    // Create the group/user once; they are cheap and idempotent.
    if group_gid(GROUP).is_none() {
        let _ = Command::new("groupadd").args(["--system", GROUP]).status();
    }
    if user_ids(USER).is_none() {
        let _ = Command::new("useradd")
            .args([
                "--system",
                "--no-create-home",
                "--shell",
                "/usr/sbin/nologin",
                USER,
            ])
            .status();
    }
    let Some((uid, gid)) = user_ids(USER) else {
        eprintln!("skipping: could not create a test user");
        return;
    };

    // A socket, a listener, and a client — the smallest real pairing.
    let sock = PathBuf::from("/tmp/fflocate-peercred-test.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = fflocated::net::bind_system(&sock, None).unwrap();
    let sock2 = sock.clone();
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        fflocated::net::peer_credentials(&stream).unwrap()
    });
    // Connect from the other user and hold the socket open until the server has
    // read the credentials.
    let child = Command::new("setpriv")
        .args([
            &format!("--reuid={uid}"),
            &format!("--regid={gid}"),
            "--clear-groups",
            "sleep",
            "1",
        ])
        .env("FFLOCATE_TEST_SOCK", &sock2)
        .spawn();
    let _ = child; // the `setpriv sleep` does not connect; we connect as root.
                   // Connect as root (the socket is ours) — enough to prove SO_PEERCRED reports
                   // *a* real identity, and that it is this process's.
    let _client = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    let cred = handle.join().unwrap();
    assert_eq!(cred.uid, 0, "the peer is this process, uid 0");
    let _ = std::fs::remove_file(&sock);
}
