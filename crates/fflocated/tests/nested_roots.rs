//! A store grown from one root to nested roots (§4): every level is indexed
//! once, under its own root, and every root is crawled and watched.
//!
//! The regression pinned here: root-dir entries are never journaled (they are
//! the store's structure, created when it opens), so a journal written by a
//! one-root store numbers its first crawl entry where a three-root build has
//! just put a root entry. Replaying that tail clobbered the new roots' anchors,
//! and a root without an anchor is skipped by `start` — no crawl, no watches,
//! an empty `/home/john` while every root still reported `ok`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fflocate::proto::{Request, Response};
use fflocated::config::{BackendChoice, Config};
use fflocated::daemon::Daemon;

/// Isolated IPC endpoint for this test (loopback TCP on Windows, where there
/// are no unix sockets).
fn test_socket(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let _ = dir;
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
        let port = l.local_addr().expect("bound addr").port();
        PathBuf::from(format!("127.0.0.1:{port}"))
    }
    #[cfg(not(windows))]
    {
        dir.join("fflocate.sock")
    }
}

fn config(dir: &Path, state: PathBuf, roots: Vec<PathBuf>) -> Config {
    Config {
        socket_path: test_socket(dir),
        pidfile: dir.join("fflocate.pid"),
        state_dir: state,
        roots,
        backend: BackendChoice::Fake,
        compaction_threshold: 1_000_000,
        sync_timeout_ms: 500,
        excludes: fflocate::excludes::Excludes::new(),
        system: false,
        system_group: 0,
        shared_store: false,
    }
}

/// Drive the daemon's tick loop until quiescent.
fn settle(d: &mut Daemon, ms: u64) {
    let deadline = std::time::Instant::now() + Duration::from_millis(ms);
    loop {
        d.tick();
        if d.applier_mut().pending().is_drained() {
            d.tick();
            if d.applier_mut().pending().is_drained() {
                return;
            }
        }
        if std::time::Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Every path the query path would report.
fn hit_paths(d: &Daemon) -> Vec<String> {
    let ctx = d.server_ctx();
    fflocate::server::handle_request(&ctx, &Request::query("", 0, true, false))
        .into_iter()
        .filter_map(|r| match r {
            Response::Hit { path, .. } => Some(path),
            _ => None,
        })
        .collect()
}

#[test]
fn a_one_root_store_upgraded_to_nested_roots_indexes_each_level() {
    let d = tempfile::tempdir().unwrap();
    let tree = d.path().join("tree");
    std::fs::create_dir_all(tree.join("home/john/work")).unwrap();
    std::fs::create_dir_all(tree.join("home/other")).unwrap();
    std::fs::create_dir_all(tree.join("etc")).unwrap();
    std::fs::write(tree.join("home/john/notes.txt"), b"n").unwrap();
    std::fs::write(tree.join("home/john/work/deep.txt"), b"d").unwrap();
    std::fs::write(tree.join("home/other/x.txt"), b"x").unwrap();
    std::fs::write(tree.join("etc/hosts"), b"h").unwrap();

    let state = d.path().join("state");
    // A store with a single root, stopped before any compaction: no snapshot,
    // only the journal — the store an upgrade finds.
    {
        let cfg = config(d.path(), state.clone(), vec![tree.join("home/john")]);
        let mut first = Daemon::build(cfg).unwrap();
        first.start().unwrap();
        settle(&mut first, 2000);
        assert!(
            hit_paths(&first).iter().any(|p| p.ends_with("notes.txt")),
            "the first root must be indexed: {:?}",
            hit_paths(&first)
        );
        first.stop();
    }
    assert!(
        !fflocate::persist::Store::snapshot_path(&state).exists(),
        "the fixture must be journal-only, or the id space is not at risk"
    );

    // The three-level configuration: each level is its own root.
    let cfg = config(
        d.path(),
        state,
        vec![tree.clone(), tree.join("home"), tree.join("home/john")],
    );
    let mut daemon = Daemon::build(cfg).unwrap();
    daemon.start().unwrap();
    settle(&mut daemon, 3000);

    // Each root must still resolve to its anchor: `start` watches and crawls
    // exactly the roots that do, so a missing anchor is a silently empty root.
    {
        let ix = daemon.shared.index.read().unwrap();
        let roots = daemon.shared.roots.read().unwrap();
        for r in roots.iter() {
            assert_ne!(
                fflocate::applier::root_dir_id(&ix, r),
                fflocate::index::ROOT_ID,
                "root {} lost its anchor",
                r.key()
            );
        }
    }

    let paths = hit_paths(&daemon);
    for (name, want) in [
        ("notes.txt", "home/john/notes.txt"),
        ("deep.txt", "home/john/work/deep.txt"),
        ("x.txt", "home/other/x.txt"),
        ("hosts", "etc/hosts"),
    ] {
        let hits: Vec<&String> = paths.iter().filter(|p| p.ends_with(name)).collect();
        assert_eq!(hits.len(), 1, "{name} must be indexed once: {hits:?}");
        assert!(
            hits[0].ends_with(want),
            "{name} must carry its whole path: {hits:?}"
        );
    }
    daemon.stop();
}
