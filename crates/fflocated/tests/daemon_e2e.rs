//! End-to-end daemon tests: build a daemon over a temp tree with the fake
//! backend, drive it through the applier loop, and assert on `Shared`.

use std::path::PathBuf;
use std::time::Duration;

use fflocate::proto::{Request, Response};
use fflocated::config::{BackendChoice, Config};
use fflocated::daemon::Daemon;
use fflocated::net;


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
        dir.join("fflocate.sock")
    }
}


fn test_config(dir: &std::path::Path, root: PathBuf) -> Config {
    Config {
        socket_path: test_socket(dir),
        pidfile: dir.join("fflocate.pid"),
        state_dir: dir.join("state"),
        roots: vec![root],
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
    let out = fflocate::server::handle_request(&ctx, &Request::query("", 0, true, false));
    let paths: Vec<String> = out
        .iter()
        .filter_map(|r| match r {
            Response::Hit { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert!(
        paths.iter().any(|p| p.ends_with("/src/main.rs")),
        "got {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("/src/deep/nested.txt")),
        "got {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("/README.md")),
        "got {paths:?}"
    );

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
    daemon.applier_mut().push(fflocate::event::FsEvent::upsert(
        fflocate::event::DirKey::new(root_id),
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

    // Rebuild from the same state dir: the snapshot serves immediately
    // while the journal tail replays in the background (§9). `build` must
    // NOT have applied it yet — that is the whole point.
    let cfg = test_config(d.path(), root);
    let mut daemon = Daemon::build(cfg).unwrap();
    assert!(
        daemon.applier_mut().pending_replay_len() > 0,
        "restart must stash a replay tail, not apply it in build"
    );
    assert!(
        daemon.shared.health.read().unwrap().catching_up,
        "snapshot serves stale-but-honest until the tail lands"
    );
    // No crawl enqueued (`start` is never called): the file must come back
    // from replay alone, proving the tail — not a recrawl — did it.
    assert!(daemon.settle(std::time::Duration::from_secs(2)));
    {
        let ix = daemon.shared.index.read().unwrap();
        assert!(
            ix.iter_live().any(|(_, e)| e.name_len == 5),
            "a.txt (5 bytes) must be present after replay"
        );
    }
    assert!(
        !daemon.shared.health.read().unwrap().catching_up,
        "catching_up clears once replay + queue drain"
    );
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
    assert!(resp
        .iter()
        .any(|r| matches!(r, Response::Hit { path, .. } if path.ends_with("hello.rs"))));
    daemon.stop();
}

#[test]
fn progress_opt_in_streams_scan_fraction_over_the_socket() {
    // End-to-end wire pin: a query with `progress: true` yields Meta, then
    // Progress records with monotone `scanned` and a stable `total`, then
    // Hits and an exact Done. Without the flag the reply is Progress-free.
    // Slow the serve thread's Progress path so the 100 ms rate gate lets
    // several records through on a small tree — real slowness (a 2M-entry
    // scan) is what spaces them in production.
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    for i in 0..6000 {
        std::fs::write(root.join(format!("file_{i:04}.txt")), b"x").unwrap();
    }

    let cfg = test_config(d.path(), root);
    let sock = cfg.socket_path.clone();
    let mut daemon = Daemon::build(cfg).unwrap();
    daemon.start().unwrap();
    settle(&mut daemon, 4000);

    // One listener, serving as many connections as the test makes: the
    // queries below each open a fresh connection (`roundtrip` is one
    // exchange per connection), and `net::bind` refuses a second bind while
    // the first listener is up — so the accept loop must keep accepting
    // instead of being re-bound per query.
    let ctx = std::sync::Arc::new(daemon.server_ctx());
    let listener = net::bind(&sock).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let accept_stop = stop.clone();
    std::thread::spawn(move || {
        while !accept_stop.load(std::sync::atomic::Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    // See net::accept_loop: accepted streams must be
                    // blocking (on Windows they inherit the listener's
                    // nonblocking mode).
                    stream.set_nonblocking(false).unwrap();
                    let ctx = ctx.clone();
                    std::thread::spawn(move || net::serve_client(ctx, stream));
                }
                Err(_) => break,
            }
        }
    });

    let query = |progress: bool| {
        let mut req = Request::query("file", 0, true, false);
        if let Request::Query { progress: p, .. } = &mut req {
            *p = progress;
        }
        // `roundtrip` reads to the terminal record; Progress is
        // non-terminal so it is collected, never swallowed.
        net::roundtrip(&sock, &req).unwrap()
    };

    let plain = query(false);
    assert!(
        !plain.iter().any(|r| matches!(r, Response::Progress { .. })),
        "opt-out reply must be Progress-free"
    );

    // The same accept loop serves the second query (one connection per
    // roundtrip); no re-bind, which `net::bind` would refuse while the first
    // listener is up.
    let with = query(true);
    assert!(matches!(with.first(), Some(Response::Meta { .. })));
    assert!(matches!(with.last(), Some(Response::Done { .. })));
    let mut n_progress = 0usize;
    let mut last_scanned = 0u64;
    let mut total = None;
    for r in &with {
        if let Response::Progress { scanned, total: t } = r {
            n_progress += 1;
            assert!(*scanned >= last_scanned, "scanned is monotone");
            last_scanned = *scanned;
            match total {
                None => total = Some(*t),
                Some(prev) => assert_eq!(prev, *t, "denominator stable"),
            }
        }
    }
    assert!(n_progress > 0, "a 6k opt-in scan must emit Progress");
    let done_count = with.iter().find_map(|r| match r {
        Response::Done { count, .. } => Some(*count),
        _ => None,
    });
    let plain_count = plain.iter().find_map(|r| match r {
        Response::Done { count, .. } => Some(*count),
        _ => None,
    });
    assert_eq!(done_count, plain_count, "Progress changes timing, not totals");
    assert_eq!(done_count, Some(6000));
    daemon.stop();
}

#[test]
fn large_replayed_tail_compacts_away_on_boot() {
    // §9 deferred compaction: a tail past the threshold is *not* folded at
    // build time anymore (that renumbered ids while nothing was live). The
    // daemon serves the snapshot, stashes the tail, drains it in chunks on
    // the tick, and compacts once via the normal `maybe_compact` path after
    // the tail lands — so the *next* boot replays ~nothing, and
    // post-compact writes land on the reset epoch (LSN 1..) instead of being
    // skipped by the `lsn > last_lsn` replay filter.
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"x").unwrap();
    // Two files: the crawl journals one Add per file (the root-dir entry
    // itself is inserted directly in build, never journaled), so the
    // inherited tail is ≥ 2 records.
    std::fs::write(root.join("c.txt"), b"x").unwrap();

    // Phase 1: journal a tail without compacting (high threshold), so the
    // next boot inherits a real tail on disk.
    let mut cfg = test_config(d.path(), root.clone());
    cfg.compaction_threshold = 1_000_000;
    {
        let mut daemon = Daemon::build(cfg.clone()).unwrap();
        // Fresh store: no snapshot, no journal — nothing to stash.
        assert_eq!(
            daemon.applier_mut().pending_replay_len(),
            0,
            "fresh store has no tail to replay"
        );
        daemon.start().unwrap();
        settle(&mut daemon, 2000);
        assert!(
            daemon.applier_mut().store().journal.count() >= 2,
            "crawl must have journaled root + a.txt"
        );
        daemon.stop();
    }

    // Phase 2: rebuild with a low threshold. The inherited tail (≥ 2
    // records) must be stashed for background replay — not folded in build
    // (the old assertion `last_lsn == 0` right after build pinned the
    // build-time fold; the new contract defers it until the tail drains).
    // Same `dir` → same state dir, so the tail from phase 1 is on disk.
    let mut cfg2 = test_config(d.path(), root.clone());
    cfg2.compaction_threshold = 2;
    {
        let mut daemon = Daemon::build(cfg2.clone()).unwrap();
        assert!(
            daemon.applier_mut().pending_replay_len() >= 2,
            "inherited tail must be stashed for background replay, not folded in build"
        );
        assert!(
            daemon.shared.health.read().unwrap().catching_up,
            "snapshot serves stale-but-honest until the tail lands"
        );
        daemon.start().unwrap();
        settle(&mut daemon, 2000);
        // After the tail drained, the deferred compaction fired: journal
        // truncated, epoch reset, all through the live path.
        assert_eq!(
            daemon.shared.index.read().unwrap().last_lsn(),
            0,
            "deferred compaction must reset the epoch once the tail lands"
        );
        assert_eq!(daemon.shared.snapshot_lsn(), 0);
        assert_eq!(
            daemon.applier_mut().store().journal.count(),
            0,
            "journal must be truncated after deferred compaction"
        );
        daemon.stop();
    }

    // A post-compact write, then a second boot: it must survive, and the
    // second boot must have (almost) nothing to replay.
    std::fs::write(root.join("b.txt"), b"y").unwrap();
    {
        let mut daemon = Daemon::build(cfg.clone()).unwrap();
        daemon.start().unwrap();
        settle(&mut daemon, 2000);
        let (has_a, has_b) = {
            let ix = daemon.shared.index.read().unwrap();
            let rid = ix.root_dirs()[0];
            (
                ix.lookup(rid, b"a.txt").is_some(),
                ix.lookup(rid, b"b.txt").is_some(),
            )
        };
        assert!(has_a, "a.txt must survive the compacted snapshot");
        assert!(has_b, "b.txt (post-compact crawl) must survive the second boot");
        daemon.stop();
    }
}
