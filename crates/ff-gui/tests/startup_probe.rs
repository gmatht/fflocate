//! Startup-probe tests: the deferred daemon check behind first paint.
//!
//! These live in an integration target (not `src/lib.rs` unit tests) so they
//! run headless with real loopback sockets and never exec a daemon.

use ff_gui::{DaemonClient, StartupPoll, StartupProbe};

/// A socket path for a canned daemon: a temp file on unix, an ephemeral
/// loopback port on Windows (where the "path" carries `127.0.0.1:PORT`
/// text — see `fflocate::transport`). Same shape as the e2e harnesses'
/// `test_socket`.
fn test_socket(dir: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let _ = dir;
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
        let port = l.local_addr().expect("bound addr").port();
        std::path::PathBuf::from(format!("127.0.0.1:{port}"))
    }
    #[cfg(not(windows))]
    {
        dir.join("s.sock")
    }
}

#[test]
fn connected_status_line_names_socket_entries_and_backend() {
    let stats = serde_json::json!({"entries": 325000u64, "backend": "inotify"});
    assert_eq!(
        DaemonClient::connected_status_line(std::path::Path::new("/run/fflocate.sock"), &stats),
        "connected to fflocated at /run/fflocate.sock (325000 entries, inotify)"
    );
    // Unknown shapes degrade to placeholders, never a panic.
    assert_eq!(
        DaemonClient::connected_status_line(std::path::Path::new("/s"), &serde_json::Value::Null),
        "connected to fflocated at /s (0 entries, ?)"
    );
}

#[test]
fn startup_probe_reports_an_absent_daemon_without_spawning() {
    // No listener, spawning off: the probe must resolve (not hang) with a
    // "no daemon" line, so the status never claims a live connection.
    let dir = tempfile::tempdir().unwrap();
    let mut probe = StartupProbe::new(
        dir.path().join("nobody.sock"),
        std::time::Duration::from_secs(5),
        false,
    );
    let start = std::time::Instant::now();
    let mut final_line = None;
    for _ in 0..100 {
        match probe.poll() {
            StartupPoll::Done(line) => {
                final_line = Some(line);
                break;
            }
            StartupPoll::Note(line) => panic!("no spawn means no interim note, got {line:?}"),
            StartupPoll::Pending => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(probe.is_done());
    let line = final_line.expect("the probe must resolve");
    assert!(
        line.starts_with("no daemon at "),
        "an absent daemon must read as absent, got {line:?}"
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "the probe must be bounded, took {:?}",
        start.elapsed()
    );
}

#[test]
fn startup_probe_reports_a_failed_spawn_loudly() {
    // FFLOCATED_BIN pointing at nothing: the spawn fails at once and the
    // probe resolves with the "could not spawn" line — the same wording
    // the old blocking startup used.
    let dir = tempfile::tempdir().unwrap();
    let saved_bin = std::env::var("FFLOCATED_BIN").ok();
    let saved_path = std::env::var("PATH").ok();
    std::env::set_var("FFLOCATED_BIN", "/nonexistent-dir-ff-gui-test/fflocated");
    // Clear PATH so the FFLOCATED_BIN leg is the only one that can fire.
    std::env::set_var("PATH", "/nonexistent-dir-ff-gui-test");
    let mut probe = StartupProbe::new(
        dir.path().join("nobody.sock"),
        std::time::Duration::from_secs(5),
        true,
    );
    let mut final_line = None;
    for _ in 0..100 {
        match probe.poll() {
            StartupPoll::Done(line) => {
                final_line = Some(line);
                break;
            }
            _ => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    match saved_bin {
        Some(p) => std::env::set_var("FFLOCATED_BIN", p),
        None => std::env::remove_var("FFLOCATED_BIN"),
    }
    match saved_path {
        Some(p) => std::env::set_var("PATH", p),
        None => std::env::remove_var("PATH"),
    }
    assert!(probe.is_done());
    let line = final_line.expect("the probe must resolve");
    assert!(
        line.contains("could not spawn fflocated"),
        "a failed spawn must say so, got {line:?}"
    );
}

#[test]
fn startup_probe_reads_a_live_stats_reply() {
    // A canned daemon on loopback: one Stats record, then close. The probe
    // must resolve through the real non-blocking machine with the
    // connected line — this is the warm-daemon startup path, headless.
    let dir = tempfile::tempdir().unwrap();
    let path = test_socket(dir.path());
    let listener = fflocate::transport::bind(&path).unwrap();
    let server_stats = serde_json::json!({"entries": 42u64, "backend": "fake"});
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        use std::io::{BufRead, BufReader, Write};
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut req = String::new();
        reader.read_line(&mut req).unwrap();
        assert!(
            req.contains("stats"),
            "the probe must ask for stats, got {req:?}"
        );
        let resp = serde_json::to_string(&fflocate::proto::Response::Stats {
            stats: server_stats,
        })
        .unwrap();
        stream.write_all(resp.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
        // Hold the socket past the terminal record: the daemon closes
        // after answering, and the probe must not read that as failure.
        std::thread::sleep(std::time::Duration::from_millis(200));
    });
    let mut probe = StartupProbe::new(path.clone(), std::time::Duration::from_secs(5), true);
    let mut final_line = None;
    for _ in 0..500 {
        match probe.poll() {
            StartupPoll::Done(line) => {
                final_line = Some(line);
                break;
            }
            StartupPoll::Note(line) => panic!("no spawn on a live socket, got {line:?}"),
            StartupPoll::Pending => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    server.join().unwrap();
    assert!(probe.is_done());
    assert_eq!(
        final_line.as_deref(),
        Some(
            format!(
                "connected to fflocated at {} (42 entries, fake)",
                path.display()
            )
            .as_str()
        ),
        "the probe must render the connected line"
    );
}

#[test]
fn startup_probe_times_out_on_a_wedged_daemon() {
    // Accepts and never replies: the probe must give up at the deadline
    // with a "not responding" line rather than polling forever.
    let dir = tempfile::tempdir().unwrap();
    let path = test_socket(dir.path());
    let listener = fflocate::transport::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
    });
    let mut probe = StartupProbe::new(path, std::time::Duration::from_millis(120), false);
    let start = std::time::Instant::now();
    let mut final_line = None;
    for _ in 0..500 {
        match probe.poll() {
            StartupPoll::Done(line) => {
                final_line = Some(line);
                break;
            }
            _ => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    server.join().unwrap();
    assert!(probe.is_done());
    let line = final_line.expect("the probe must resolve");
    assert!(
        line.contains("not responding"),
        "a wedged daemon must read as wedged, got {line:?}"
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "the deadline must bound the probe, took {:?}",
        start.elapsed()
    );
}

#[test]
// Unix-only: the deaf-daemon signal is a pidfile `flock`, which has no
// Windows meaning (`daemon_lock_held` is always false there — see
// `fflocate::spawn`), so the probe can never report "deaf" on Windows.
#[cfg(unix)]
fn startup_probe_reports_a_deaf_daemon_at_once() {
    // Deaf daemon: a socket path that refuses (bound-then-closed listener
    // leaves the path unbound, like a daemon that lost its listener) plus a
    // LIVE pidfile lock-holder. The probe must report the deaf daemon
    // immediately — never spawn (the child would just exit "already running")
    // and never poll to the deadline.
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fflocate.sock");
    let pidfile = dir.path().join("fflocate.pid");
    {
        let dead = fflocate::transport::Listener::bind(&socket).unwrap();
        drop(dead);
    }
    assert!(
        socket.exists(),
        "a closed listener leaves the path refusing"
    );
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&pidfile)
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let rc = unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "test setup: lock must acquire");
    }
    std::fs::write(&pidfile, "999999").unwrap_or_default();
    let mut probe = StartupProbe::new(socket, std::time::Duration::from_secs(5), true);
    let start = std::time::Instant::now();
    let mut final_line = None;
    for _ in 0..100 {
        match probe.poll() {
            StartupPoll::Done(line) => {
                final_line = Some(line);
                break;
            }
            StartupPoll::Note(line) => panic!("deaf means done, not a note: {line:?}"),
            StartupPoll::Pending => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    drop(holder);
    assert!(probe.is_done());
    let line = final_line.expect("the probe must resolve");
    assert!(
        line.contains("deaf") && line.contains("111"),
        "a deaf daemon must read as deaf, got {line:?}"
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "deaf must fail fast, took {:?}",
        start.elapsed()
    );
}
