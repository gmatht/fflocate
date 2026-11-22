//! The live change feed, end to end: a real `subscribe` connection over the
//! daemon's own socket, real index mutations applied by the applier, and the
//! events a watching client receives.
//!
//! What this pins, and why each assertion exists:
//!
//! * a create arrives as `action: "create"` naming the exact path — not a
//!   coarse "something changed" (that was the `since`/`flush` preview);
//! * a delete arrives as `action: "delete"` with the same path (the applier
//!   resolves it *before* tombstoning, which is the only moment it can);
//! * a change that does not match the subscription's `q` is **not** sent, so a
//!   watching GUI is not woken (or re-queried) for files it cannot show;
//! * an unusable cursor is answered with `action: "resync"` rather than
//!   silence, so a client knows to re-query instead of showing a stale list.
//!
//! The daemon runs the fake backend, so the test injects the same `FsEvent`s
//! the real watchers produce; nothing here depends on kernel watch limits.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fflocate::event::{DirKey, FsEvent};
use fflocate::proto::{Request, Response};
use fflocated::config::{BackendChoice, Config};
use fflocated::daemon::Daemon;
use fflocated::net;

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
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        d.tick();
        if d.applier_mut().pending().is_drained() {
            d.tick();
            if d.applier_mut().pending().is_drained() {
                return;
            }
        }
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn subscribe_line(q: &str, cursor: Option<&str>) -> String {
    serde_json::to_string(&Request::Subscribe {
        name: Some("subscribe_live".into()),
        cursor: cursor.map(|c| c.to_string()),
        q: q.to_string(),
        case_sensitive: None,
    })
    .expect("encode subscribe")
}

/// Serve exactly one connection with the daemon's own client loop.
fn serve_one(ctx: std::sync::Arc<fflocate::server::ServerCtx>, sock: &std::path::Path) {
    let listener = net::bind(sock).expect("bind the test socket");
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        stream.set_nonblocking(false).expect("blocking client");
        net::serve_client(ctx, stream);
    });
}

/// Read one response line if one is already there; `None` on a read timeout.
///
/// The inotify test needs this: its daemon applies events only on `tick`, which
/// must keep running while the client waits, so the two circumstances have to
/// alternate instead of blocking in `read`.
fn try_response(reader: &mut BufReader<fflocate::transport::Stream>) -> Option<Response> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => panic!("the subscribe feed closed while the client was reading"),
        Ok(_) => Some(
            serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("bad response line {line:?}: {e}")),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => None,
        Err(e) => panic!("reading the subscribe feed failed: {e}"),
    }
}

/// Read one response line, failing with the read error if the feed goes quiet.
fn read_response(reader: &mut BufReader<fflocate::transport::Stream>) -> Response {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("the subscribe feed closed before sending an event");
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad response line {line:?}: {e}"))
}

#[test]
fn subscribe_reports_matching_creates_and_deletes_with_their_paths() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("keep-alpha.txt"), b"x").unwrap();

    let cfg = test_config(d.path(), root.clone());
    let sock = cfg.socket_path.clone();
    let mut daemon = Daemon::build(cfg).unwrap();
    daemon.start().unwrap();
    settle(&mut daemon, 2000);

    let root_id = {
        let ix = daemon.shared.index.read().unwrap();
        ix.root_dirs()[0]
    };
    serve_one(std::sync::Arc::new(daemon.server_ctx()), &sock);

    let mut stream = fflocate::transport::connect(&sock).expect("connect the subscribe socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    writeln!(stream, "{}", subscribe_line("alpha", None)).expect("send subscribe");
    stream.flush().expect("flush subscribe");
    let mut reader = BufReader::new(stream);

    match read_response(&mut reader) {
        Response::Meta { .. } => {}
        other => panic!("a subscribe must open with Meta, got {other:?}"),
    }

    // A change the subscription does not match: it must be filtered out, so
    // the *next* event the client sees is still the matching one below.
    std::fs::write(root.join("noise.log"), b"x").unwrap();
    daemon.applier_mut().push(FsEvent::upsert(
        DirKey::new(root_id),
        b"noise.log".to_vec(),
        false,
    ));
    settle(&mut daemon, 1000);

    std::fs::write(root.join("alpha-new.rs"), b"x").unwrap();
    daemon.applier_mut().push(FsEvent::upsert(
        DirKey::new(root_id),
        b"alpha-new.rs".to_vec(),
        false,
    ));
    settle(&mut daemon, 1000);

    let created = read_response(&mut reader);
    match &created {
        Response::Event { action, path, .. } => {
            assert_eq!(action, "create", "the create action, got {created:?}");
            assert!(
                path.ends_with("alpha-new.rs"),
                "the event names the wrong path: {path:?}"
            );
            assert!(
                !path.ends_with("noise.log"),
                "a non-matching change was delivered: {created:?}"
            );
        }
        other => panic!("expected a create event, got {other:?}"),
    }
    eprintln!("subscribe: create -> {created:?}");

    // Delete the same file and inject the removal the watcher would send.
    std::fs::remove_file(root.join("alpha-new.rs")).unwrap();
    daemon.applier_mut().push(FsEvent::remove(
        DirKey::new(root_id),
        b"alpha-new.rs".to_vec(),
    ));
    settle(&mut daemon, 1000);

    let deleted = read_response(&mut reader);
    match &deleted {
        Response::Event { action, path, .. } => {
            assert_eq!(action, "delete", "the delete action, got {deleted:?}");
            assert!(
                path.ends_with("alpha-new.rs"),
                "the delete names the wrong path: {path:?}"
            );
        }
        other => panic!("expected a delete event, got {other:?}"),
    }
    eprintln!("subscribe: delete -> {deleted:?}");

    daemon.stop();
}

#[test]
fn an_unusable_cursor_is_answered_with_a_resync_not_silence() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"x").unwrap();

    let cfg = test_config(d.path(), root);
    let sock = cfg.socket_path.clone();
    let mut daemon = Daemon::build(cfg).unwrap();
    daemon.start().unwrap();
    settle(&mut daemon, 2000);
    serve_one(std::sync::Arc::new(daemon.server_ctx()), &sock);

    let mut stream = fflocate::transport::connect(&sock).expect("connect the subscribe socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    // A cursor from a generation this daemon is not in: it cannot be resumed.
    writeln!(stream, "{}", subscribe_line("", Some("c:999999:1"))).expect("send subscribe");
    stream.flush().expect("flush subscribe");
    let mut reader = BufReader::new(stream);

    // A feed always opens with Meta, even when the resume cursor is refused.
    match read_response(&mut reader) {
        Response::Meta { .. } => {}
        other => panic!("a subscribe must open with Meta, got {other:?}"),
    }
    // Then the refusal: a cursor this daemon cannot resume must ask for a
    // re-query rather than replay nothing.
    match read_response(&mut reader) {
        Response::Event { action, .. } => assert_eq!(
            action, "resync",
            "a stale cursor must ask for a resync, not replay nothing"
        ),
        other => panic!("expected a resync event after Meta, got {other:?}"),
    }

    daemon.stop();
}

/// The same feed against a **real kernel watcher**: `inotify` reports a file
/// this test actually creates, and the event that reaches the client names it.
///
/// The fake-backend test above proves the feed's plumbing; this proves the
/// watcher → applier → change-ring → socket path end to end, which is what a
/// live GUI depends on. Linux-only (inotify is a Linux API) and skips cleanly
/// where the backend cannot be built.
#[cfg(target_os = "linux")]
#[test]
fn subscribe_reports_a_real_kernel_create() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();

    let mut cfg = test_config(d.path(), root.clone());
    cfg.backend = BackendChoice::Inotify;
    let sock = cfg.socket_path.clone();
    let mut daemon = match Daemon::build(cfg) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("subscribe: inotify unavailable here ({e}); skipping");
            return;
        }
    };
    if daemon.start().is_err() {
        eprintln!("subscribe: inotify backend would not start; skipping");
        return;
    }
    settle(&mut daemon, 2000);
    serve_one(std::sync::Arc::new(daemon.server_ctx()), &sock);

    let mut stream = fflocate::transport::connect(&sock).expect("connect the subscribe socket");
    // Short reads: this loop must call `tick` between them, or the event the
    // kernel already delivered would never be applied.
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("read timeout");
    writeln!(stream, "{}", subscribe_line("kernel-made", None)).expect("send subscribe");
    stream.flush().expect("flush subscribe");
    let mut reader = BufReader::new(stream);
    let mut opened = false;
    while !opened {
        daemon.tick();
        if let Some(r) = try_response(&mut reader) {
            match r {
                Response::Meta { .. } => opened = true,
                other => panic!("a subscribe must open with Meta, got {other:?}"),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // A real create in the watched tree, with no injected event at all.
    std::fs::write(root.join("kernel-made-file.txt"), b"x").unwrap();

    let start = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "inotify's create never reached the subscriber"
        );
        // The watcher thread owns the applier; ticking the daemon keeps its
        // drain loop running in this single-threaded harness.
        daemon.tick();
        if let Some(r) = try_response(&mut reader) {
            match r {
                Response::Event { action, path, .. } => {
                    if path.ends_with("kernel-made-file.txt") {
                        assert_eq!(action, "create", "the kernel create arrived as {action}");
                        eprintln!("subscribe: inotify create -> {path}");
                        break;
                    }
                }
                Response::Meta { .. } => {}
                other => panic!("unexpected response on the feed: {other:?}"),
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    daemon.stop();
}
