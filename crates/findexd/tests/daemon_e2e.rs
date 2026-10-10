//! End-to-end daemon tests: build a daemon over a temp tree with the fake
//! backend, drive it through the applier loop, and assert on `Shared`.

use std::path::PathBuf;
use std::time::Duration;

use findex::proto::{Request, Response};
use findexd::config::{BackendChoice, Config};
use findexd::daemon::Daemon;
use findexd::net;


/// Isolated IPC endpoint for this test. Unix socket file everywhere except
/// Windows, where it is loopback TCP text (no unix sockets). The port is
/// OS-assigned per call (bind-then-release; the millisecond TOCTOU is
/// accepted test-harness practice).
fn test_socket(dir: &std::path::Path) -> PathBuf {
    #[cfg(windows)]
    {
        let _ = dir;
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
        let port = l.local_addr().expect("bound addr").port();
        PathBuf::from(format!("127.0.0.1:{port}"))
    }
    #[cfg(not(windows))]
    {
        dir.join("findex.sock")
    }
}


fn test_config(dir: &std::path::Path, root: PathBuf) -> Config {
    Config {
        socket_path: test_socket(dir),
        pidfile: dir.join("findex.pid"),
        state_dir: dir.join("state"),
        roots: vec![root],
        backend: BackendChoice::Fake,
        compaction_threshold: 1_000_000,
        sync_timeout_ms: 500,
    }
}

/// Drive the daemon's tick loop until quiescent.
fn settle(d: &mut Daemon, ms: u64) {
    let deadline = std::time::Instant::now() + Duration::from_millis(ms);
    loop {
        d.tick();
        if d.applier_mut().pending().is_drained() {
            // a couple more ticks to let watch requests flow
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

#[test]
fn crawl_indexes_a_real_tree() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(root.join("src/deep")).unwrap();
    std::fs::write(root.join("src/main.rs"), b"x").unwrap();
    std::fs::write(root.join("src/deep/nested.txt"), b"y").unwrap();
    std::fs::write(root.join("README.md"), b"z").unwrap();

    let cfg = test_config(d.path(), root);
    let mut daemon = Daemon::build(cfg).unwrap();
    daemon.start().unwrap();
    settle(&mut daemon, 2000);

    let ctx = daemon.server_ctx();
    let out = findex::server::handle_request(
        &ctx,
        &Request::query("", 0, true, false),
    );
    let paths: Vec<String> = out
        .iter()
        .filter_map(|r| match r {
            Response::Hit { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert!(paths.iter().any(|p| p.ends_with("/src/main.rs")), "got {paths:?}");
    assert!(paths.iter().any(|p| p.ends_with("/src/deep/nested.txt")), "got {paths:?}");
    assert!(paths.iter().any(|p| p.ends_with("/README.md")), "got {paths:?}");

    daemon.stop();
}

#[test]
fn live_events_are_applied_via_fake_backend() {
    // Build a daemon, then feed it a synthetic event and check the index.
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    let cfg = test_config(d.path(), root.clone());
    let mut daemon = Daemon::build(cfg).unwrap();
    daemon.start().unwrap();
    settle(&mut daemon, 2000);

    // Find the root dir id and inject a create event for a file that really
    // exists (so a later rescan agrees).
    std::fs::write(root.join("brand_new.rs"), b"x").unwrap();
    let root_id = {
        let ix = daemon.shared.index.read().unwrap();
        ix.root_dirs()[0]
    };
    daemon.applier_mut().push(findex::event::FsEvent::upsert(
        findex::event::DirKey::new(root_id),
        b"brand_new.rs".to_vec(),
        false,
    ));
    settle(&mut daemon, 1000);

    {
        let ix = daemon.shared.index.read().unwrap();
        assert!(
            ix.lookup(root_id, b"brand_new.rs").is_some(),
            "the injected create must be applied"
        );
    }
    daemon.stop();
}

#[test]
fn journal_records_survive_restart() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"x").unwrap();

    {
        let cfg = test_config(d.path(), root.clone());
        let mut daemon = Daemon::build(cfg).unwrap();
        daemon.start().unwrap();
        settle(&mut daemon, 2000);
        let entries = daemon.shared.index.read().unwrap().live_count();
        assert!(entries >= 2, "root dir + a.txt");
        daemon.stop();
    }

    // Rebuild from the same state dir: the journal replay must reproduce it.
    let cfg = test_config(d.path(), root);
    let mut daemon = Daemon::build(cfg).unwrap();
    {
        let ix = daemon.shared.index.read().unwrap();
        assert!(
            ix.iter_live().any(|(_, e)| e.name_len == 5),
            "a.txt (5 bytes) must be present after replay"
        );
    }
    daemon.stop();
}

#[test]
fn socket_roundtrip_serves_a_query() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("hello.rs"), b"x").unwrap();

    let cfg = test_config(d.path(), root);
    let sock = cfg.socket_path.clone();
    let mut daemon = Daemon::build(cfg).unwrap();
    daemon.start().unwrap();
    settle(&mut daemon, 2000);

    // Serve requests on a background thread, then round-trip from here.
    {
        let ctx = std::sync::Arc::new(daemon.server_ctx());
        let listener = net::bind(&sock).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // See net::accept_loop: accepted streams must be blocking (on
            // Windows they inherit the listener's nonblocking mode).
            stream.set_nonblocking(false).unwrap();
            net::serve_client(ctx, stream);
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    }

    let resp = net::roundtrip(&sock, &Request::query("hello", 0, true, false)).unwrap();
    assert!(matches!(resp.first(), Some(Response::Meta { .. })));
    assert!(resp.iter().any(|r| matches!(r, Response::Hit { path, .. } if path.ends_with("hello.rs"))));
    daemon.stop();
}
