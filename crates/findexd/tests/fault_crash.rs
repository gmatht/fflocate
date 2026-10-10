//! §14.3 fault injection against a *real* daemon process.
//!
//! These tests spawn `findexd` as a child, apply real filesystem churn, and
//! crash it. They are the "kill -9 mid-journal-write" and "mount/umount during
//! crawl" cases from DESIGN.md §14.3.
//!
//! They need the release binaries, so they are gated behind the
//! `FINDEX_BIN_DIR` env var (set by `scripts/e2e.sh`); without it they skip
//! cleanly so a plain `cargo test` stays hermetic.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin_dir() -> Option<PathBuf> {
    std::env::var("FINDEX_BIN_DIR").ok().map(PathBuf::from)
}

fn binaries() -> Option<(PathBuf, PathBuf)> {
    let d = bin_dir()?;
    let findexd = d.join(format!("findexd{}", std::env::consts::EXE_SUFFIX));
    let ff = d.join(format!("ff{}", std::env::consts::EXE_SUFFIX));
    if findexd.exists() && ff.exists() {
        Some((findexd, ff))
    } else {
        None
    }
}

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

/// Watch backend for the spawned daemon: the real watcher on Linux, the fake
/// backend elsewhere (no kernel watcher is ported yet; FSEvents is v1.5).
/// Crash/restart/rescan recovery is backend-independent; live-event delivery
/// during the churn window is weaker with `fake`, which the assertions
/// tolerate by converging via explicit rescans.
fn backend_arg() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "inotify"
    }
    #[cfg(not(target_os = "linux"))]
    {
        "fake"
    }
}

struct DaemonProc {
    child: Child,
    socket: PathBuf,
}

impl DaemonProc {
    fn spawn(findexd: &Path, socket: &Path, state: &Path, root: &Path) -> DaemonProc {
        let child = Command::new(findexd)
            .arg("--socket")
            .arg(socket)
            .arg("--state-dir")
            .arg(state)
            .arg("--root")
            .arg(root)
            .arg("--backend")
            .arg(backend_arg())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn findexd");
        let p = DaemonProc { child, socket: socket.to_path_buf() };
        p.wait_ready(Duration::from_secs(10));
        p
    }

    fn wait_ready(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.socket.exists() {
                // Socket appears before accept; a tiny settle is enough.
                std::thread::sleep(Duration::from_millis(50));
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// `kill -9` — no cleanup, no flush, no chance to handle a signal.
    fn kill9(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DaemonProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Ask the CLI a question; returns the CLI's stdout lines.
fn ff(fxbin: &Path, socket: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(fxbin)
        .args(args)
        .env("FINDEX_SOCKET", socket)
        .output()
        .expect("run ff");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn truth(root: &Path) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for ent in rd.flatten() {
            let p = ent.path();
            out.insert(p.to_string_lossy().into_owned());
            if ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(p);
            }
        }
    }
    out
}

fn indexed(fxbin: &Path, socket: &Path, root: &Path) -> std::collections::BTreeSet<String> {
    // Force an explicit reconcile first so we compare the *healed* state, which
    // is what the design promises after a crash (§7: degrade to work).
    let root_s = root.to_string_lossy().into_owned();
    let _ = ff(fxbin, socket, &["rescan", &root_s]);
    std::thread::sleep(Duration::from_millis(50));
    let (out, _) = ff(fxbin, socket, &["-l", "0", ""]);
    out.lines()
        .filter(|l| !l.is_empty() && !l.starts_with("ff:"))
        .map(|l| l.to_string())
        .collect()
}

#[test]
fn kill9_mid_write_then_restart_is_consistent_and_converges() {
    let Some((findexd, fxbin)) = binaries() else {
        eprintln!("skipping: set FINDEX_BIN_DIR to target/release (see scripts/e2e.sh)");
        return;
    };
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    let state = d.path().join("state");
    let socket = test_socket(d.path());
    std::fs::create_dir_all(&root).unwrap();

    // Seed a known baseline.
    for i in 0..50 {
        std::fs::write(root.join(format!("base{i:03}.rs")), b"x").unwrap();
    }

    // Phase 1: start, crawl, then crash *during* sustained writes.
    {
        let mut daemon = DaemonProc::spawn(&findexd, &socket, &state, &root);
        // Let the crawl land.
        for _ in 0..50 {
            let (_, code) = ff(&fxbin, &socket, &["--sync", "", "-l", "1", ""]);
            if code == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Hammer the journal with writes while the daemon is live, then kill it
        // immediately afterwards — the journal tail is very likely torn (the
        // applier is still writing records for these 3000 creates when SIGKILL
        // lands; emit() uses write(2) without fsync, so a tail record can be
        // half-written).
        for i in 0..3000 {
            std::fs::write(root.join(format!("churn{i:04}.rs")), b"y").unwrap();
            if i % 250 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        daemon.kill9();
        std::thread::sleep(Duration::from_millis(100));
    }

    // The journal must exist and be a *decodeable prefix*: never a wedge. The
    // restart below proves the daemon tolerates a torn tail (§9).
    let state2 = state.clone();
    assert!(state2.join("journal.log").exists() || state2.join("snapshot.fnx").exists(),
        "the store must have left a journal or a snapshot behind");

    // Phase 2: restart on the same state dir. This must not panic or hang, and
    // must converge to filesystem truth after a rescan (§7).
    let mut daemon = DaemonProc::spawn(&findexd, &socket, &state, &root);
    // Wait for the daemon to be serving (hello).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (out, code) = ff(&fxbin, &socket, &["stats"]);
        if code == 0 && out.contains("entries") {
            break;
        }
        assert!(Instant::now() < deadline, "daemon never came back up after kill -9");
        std::thread::sleep(Duration::from_millis(50));
    }

    // Converge: drain + rescan until the index equals the filesystem.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        indexed(&fxbin, &socket, &root); // triggers a rescan
        std::thread::sleep(Duration::from_millis(100));
        let got = {
            let (out, _) = ff(&fxbin, &socket, &["-l", "0", ""]);
            out.lines().filter(|l| !l.is_empty()).map(|s| s.to_string()).collect::<std::collections::BTreeSet<_>>()
        };
        let want = truth(&root);
        if got == want {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "index never converged after kill -9.\n  missing: {:?}\n  extra: {:?}",
            want.difference(&got).take(5).collect::<Vec<_>>(),
            got.difference(&want).take(5).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    daemon.kill9();
}

#[test]
fn torn_journal_tail_after_crash_recovers_without_wedging() {
    // A `kill -9` on a page-cache-backed tmpfs rarely leaves a torn record,
    // because the bytes are already in the page cache. On real storage a crash
    // mid-write *does*. So we make the torn tail deterministic: crash the
    // daemon, then chop the journal mid-record (exactly the byte pattern a
    // power loss produces). The daemon must detect it, keep serving, and
    // converge after a rescan — never wedge.
    let Some((findexd, fxbin)) = binaries() else {
        eprintln!("skipping: set FINDEX_BIN_DIR");
        return;
    };
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    let state = d.path().join("state");
    let socket = test_socket(d.path());
    std::fs::create_dir_all(&root).unwrap();
    for i in 0..80 {
        std::fs::write(root.join(format!("f{i:03}.rs")), b"x").unwrap();
    }

    // Populate the journal, then crash.
    {
        let mut daemon = DaemonProc::spawn(&findexd, &socket, &state, &root);
        std::thread::sleep(Duration::from_millis(200));
        daemon.kill9();
    }
    let journal = state.join("journal.log");
    let size = std::fs::metadata(&journal).map(|m| m.len()).unwrap_or(0);
    assert!(size > 32, "expected a non-trivial journal, got {size} bytes");

    // Chop mid-record.
    let f = std::fs::OpenOptions::new().write(true).open(&journal).unwrap();
    f.set_len(size - 37).unwrap();
    drop(f);

    // Restart: must come up, report journal_corrupt, and converge.
    let mut daemon = DaemonProc::spawn(&findexd, &socket, &state, &root);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (out, code) = ff(&fxbin, &socket, &["stats", "--json"]);
        if code == 0 {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) {
                if v.get("journal_corrupt").and_then(|b| b.as_bool()) == Some(true) {
                    break;
                }
            }
        }
        assert!(Instant::now() < deadline, "torn tail was not reported as corrupt");
        std::thread::sleep(Duration::from_millis(50));
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        indexed(&fxbin, &socket, &root);
        std::thread::sleep(Duration::from_millis(100));
        let (out, _) = ff(&fxbin, &socket, &["-l", "0", ""]);
        let got: std::collections::BTreeSet<String> =
            out.lines().filter(|l| !l.is_empty()).map(|s| s.to_string()).collect();
        if got == truth(&root) {
            break;
        }
        assert!(Instant::now() < deadline, "did not converge after a torn journal");
        std::thread::sleep(Duration::from_millis(100));
    }
    daemon.kill9();
}

#[test]
fn kill9_repeatedly_never_wedges_the_store() {
    // Crash and restart the daemon several times in a row against the same
    // state dir. Recovery must stay consistent every time (no corruption wedge).
    let Some((findexd, fxbin)) = binaries() else {
        eprintln!("skipping: set FINDEX_BIN_DIR");
        return;
    };
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    let state = d.path().join("state");
    let socket = test_socket(d.path());
    std::fs::create_dir_all(&root).unwrap();
    for i in 0..20 {
        std::fs::write(root.join(format!("f{i}.txt")), b"x").unwrap();
    }

    for round in 0..4 {
        let mut daemon = DaemonProc::spawn(&findexd, &socket, &state, &root);
        std::thread::sleep(Duration::from_millis(100));
        for i in 0..200 {
            std::fs::write(root.join(format!("r{round}_{i}.txt")), b"y").unwrap();
        }
        std::thread::sleep(Duration::from_millis(50));
        daemon.kill9();
    }

    // Final restart must serve and converge.
    let mut daemon = DaemonProc::spawn(&findexd, &socket, &state, &root);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (out, code) = ff(&fxbin, &socket, &["stats"]);
        if code == 0 && out.contains("entries") {
            break;
        }
        assert!(Instant::now() < deadline, "store wedged after repeated kill -9");
        std::thread::sleep(Duration::from_millis(50));
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        indexed(&fxbin, &socket, &root);
        std::thread::sleep(Duration::from_millis(100));
        let (out, _) = ff(&fxbin, &socket, &["-l", "0", ""]);
        let got: std::collections::BTreeSet<String> =
            out.lines().filter(|l| !l.is_empty()).map(|s| s.to_string()).collect();
        if got == truth(&root) {
            break;
        }
        assert!(Instant::now() < deadline, "did not converge after repeated crashes");
        std::thread::sleep(Duration::from_millis(100));
    }
    daemon.kill9();
}
