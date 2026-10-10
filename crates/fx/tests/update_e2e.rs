//! `ff update` end-to-end: the offline-edits scenario (§7/§15).
//!
//! A daemon indexes a tree and dies (kill -9). The tree is edited while no
//! watcher runs — a file deleted, a file added. After restart the index is
//! stale (a restart crawl only *adds*; nothing removes the deleted file),
//! and `ff update` must converge it to filesystem truth. The stats assertion
//! (`last_rescan_cause=manual`) proves the update's own rescan did the work,
//! not startup.
//!
//! Needs release binaries behind `FINDEX_BIN_DIR` like the other e2e suites;
//! without it the tests skip.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin_dir() -> Option<PathBuf> {
    let d = std::env::var("FINDEX_BIN_DIR").ok().map(PathBuf::from)?;
    (d.join("findexd").exists() && d.join("ff").exists()).then_some(d)
}

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

fn spawn(findexd: &Path, socket: &Path, state: &Path, root: &Path) -> Child {
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
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(100));
    child
}

fn kill9(child: &mut Child) {
    unsafe {
        libc::kill(child.id() as i32, libc::SIGKILL);
    }
    let _ = child.wait();
}

/// Run the real `ff` with an isolated socket; returns (stdout, exit code).
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

fn listing(fxbin: &Path, socket: &Path) -> std::collections::BTreeSet<String> {
    let (out, _) = ff(fxbin, socket, &["-l", "0", ""]);
    out.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect()
}

fn settle(fxbin: &Path, socket: &Path) {
    for _ in 0..100 {
        let (_, code) = ff(fxbin, socket, &["--sync", "", "-l", "1", ""]);
        if code == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("daemon never settled");
}

#[test]
fn update_heals_offline_edits_after_downtime() {
    let Some(bindir) = bin_dir() else {
        eprintln!("skipping: set FINDEX_BIN_DIR to target/release (see scripts/e2e.sh)");
        return;
    };
    let findexd = bindir.join("findexd");
    let fxbin = bindir.join("ff");
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    let state = d.path().join("state");
    let socket = d.path().join("findex.sock");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("keep.txt"), b"k").unwrap();
    std::fs::write(root.join("gone.txt"), b"g").unwrap();

    // Baseline: daemon up, both files indexed.
    let mut daemon = spawn(&findexd, &socket, &state, &root);
    settle(&fxbin, &socket);
    let rel = |p: &str| root.join(p).to_string_lossy().into_owned();
    assert!(listing(&fxbin, &socket).contains(&rel("gone.txt")));

    // Downtime with offline edits: one deletion, one addition, no watcher.
    kill9(&mut daemon);
    std::thread::sleep(Duration::from_millis(100));
    std::fs::remove_file(root.join("gone.txt")).unwrap();
    std::fs::write(root.join("new.txt"), b"n").unwrap();

    // Restart, but no update yet: the restart crawl picks up the addition,
    // yet nothing removes the offline deletion — the index is stale.
    let mut daemon = spawn(&findexd, &socket, &state, &root);
    settle(&fxbin, &socket);
    assert!(
        listing(&fxbin, &socket).contains(&rel("gone.txt")),
        "restart alone must not heal an offline deletion (that is update's job)"
    );

    // The manual update converges to filesystem truth.
    let (out, code) = ff(&fxbin, &socket, &["update", "--timeout", "60"]);
    assert_eq!(code, 0, "update output:\n{out}");
    assert!(out.contains("ok"), "per-root status expected:\n{out}");
    assert!(out.contains(&root.to_string_lossy().into_owned()), "output names the root:\n{out}");
    let got = listing(&fxbin, &socket);
    let want: std::collections::BTreeSet<String> =
        [rel("keep.txt"), rel("new.txt")].into_iter().collect();
    assert_eq!(got, want, "index must equal filesystem truth after update");

    // And it was update's own rescan that did it.
    let (stats, _) = ff(&fxbin, &socket, &["stats", "--roots"]);
    assert!(
        stats.contains("last_rescan_cause=manual"),
        "update must land a manual rescan:\n{stats}"
    );

    kill9(&mut daemon);
}

#[test]
fn update_unknown_root_is_an_error() {
    let Some(bindir) = bin_dir() else {
        eprintln!("skipping: set FINDEX_BIN_DIR to target/release (see scripts/e2e.sh)");
        return;
    };
    let findexd = bindir.join("findexd");
    let fxbin = bindir.join("ff");
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    let state = d.path().join("state");
    let socket = d.path().join("findex.sock");
    std::fs::create_dir_all(&root).unwrap();
    // Seed file: settle() waits on a query hit, which an empty tree never
    // produces.
    std::fs::write(root.join("seed.txt"), b"s").unwrap();

    let mut daemon = spawn(&findexd, &socket, &state, &root);
    settle(&fxbin, &socket);

    let (out, code) = ff(&fxbin, &socket, &["update", "/no-such-root-xyz"]);
    assert_eq!(code, 2, "unknown root must be an error, got:\n{out}");

    // Scoped update of the real root succeeds.
    let (out, code) = ff(
        &fxbin,
        &socket,
        &["update", &root.to_string_lossy()],
    );
    assert_eq!(code, 0, "scoped update output:\n{out}");
    assert!(out.contains("ok"), "per-root status expected:\n{out}");

    kill9(&mut daemon);
}
