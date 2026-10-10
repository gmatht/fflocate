//! §14.3 fault injection: mount/umount during crawl, and watch-limit exhaustion.
//!
//! These need root/CAP_SYS_ADMIN for bind mounts and the ability to lower an
//! inotify sysctl, so they are gated behind `FINDEX_BIN_DIR` (see
//! `scripts/e2e.sh`). If the capabilities are missing they skip with a note
//! rather than fail — but they print *why*, so a skip is visible.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin_dir() -> Option<PathBuf> {
    let d = std::env::var("FINDEX_BIN_DIR").ok().map(PathBuf::from)?;
    (d.join("findexd").exists() && d.join("fx").exists()).then_some(d)
}

/// Watch backend for the spawned daemon (see fault_crash.rs). These tests
/// skip without bind-mount/sysctl capabilities anyway; on non-Linux that is
/// always the case.
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
            .args(["--socket", &socket.to_string_lossy()])
            .args(["--state-dir", &state.to_string_lossy()])
            .args(["--root", &root.to_string_lossy()])
            .args(["--backend", backend_arg()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn findexd");
        let p = DaemonProc { child, socket: socket.to_path_buf() };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !p.socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(80));
        p
    }

    /// Ask the daemon to exit cleanly (SIGTERM) and reap it.
    fn stop_and_wait(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => break,
            }
        }
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

impl Drop for DaemonProc {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

fn fx(fxbin: &Path, socket: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(fxbin)
        .args(args)
        .env("FINDEX_SOCKET", socket)
        .output()
        .expect("run fx");
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.code().unwrap_or(-1))
}

fn stats_json(fxbin: &Path, socket: &Path) -> Option<serde_json::Value> {
    let (o, c) = fx(fxbin, socket, &["stats", "--json", "--roots"]);
    (c == 0 || !o.is_empty()).then(|| serde_json::from_str(&o).ok()).flatten()
}

fn paths_under(fxbin: &Path, socket: &Path) -> std::collections::BTreeSet<String> {
    let (o, _) = fx(fxbin, socket, &["-l", "0", ""]);
    o.lines().filter(|l| !l.is_empty()).map(|s| s.to_string()).collect()
}

/// Wait until the daemon is serving.
fn wait_serving(fxbin: &Path, socket: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let (_, c) = fx(fxbin, socket, &["stats"]);
        if c == 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Can we bind-mount? (root + a mountable src/dst.)
fn can_bind_mount(src: &Path, dst: &Path) -> bool {
    let ok = Command::new("mount")
        .args(["--bind"])
        .arg(src)
        .arg(dst)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        let _ = Command::new("umount").arg(dst).status();
    }
    ok
}

#[test]
fn mount_umount_during_crawl_degrades_and_recovers() {
    let Some(bins) = bin_dir() else {
        eprintln!("skipping (no FINDEX_BIN_DIR): mount/umount during crawl");
        return;
    };
    let findexd = bins.join("findexd");
    let fxbin = bins.join("fx");

    // The *root itself* is a mount point, which is the realistic case: mount a
    // filesystem, index it, unmount it under the daemon, remount it. The backing
    // store lives on a *different* filesystem (a tmpfs) so that the device id
    // genuinely changes across the unmount — which is what the daemon's mount
    // watchdog uses to notice (there is no IN_UNMOUNT on this platform).
    let d = tempfile::tempdir().unwrap();
    let backing = d.path().join("backing"); // will be a tmpfs
    let root = d.path().join("root"); // the mount point == the indexed root
    std::fs::create_dir_all(&backing).unwrap();
    std::fs::create_dir_all(&root).unwrap();

    let tmpfs_ok = Command::new("mount")
        .args(["-t", "tmpfs", "tmpfs"])
        .arg(&backing)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !tmpfs_ok {
        eprintln!("skipping (cannot mount tmpfs): mount/umount during crawl");
        return;
    }
    struct UmountTmpfs(PathBuf);
    impl Drop for UmountTmpfs {
        fn drop(&mut self) {
            let _ = Command::new("umount").arg(&self.0).status();
        }
    }
    let _tmpfs_guard = UmountTmpfs(backing.clone());

    std::fs::write(backing.join("mounted.rs"), b"x").unwrap();
    std::fs::write(backing.join("plain.rs"), b"x").unwrap();

    if !can_bind_mount(&backing, &root) {
        eprintln!("skipping (bind mount unavailable): mount/umount during crawl");
        return;
    }
    assert!(Command::new("mount").args(["--bind"]).arg(&backing).arg(&root).status().unwrap().success());

    let state = d.path().join("state");
    let socket = d.path().join("findex.sock");
    let mut daemon = DaemonProc::spawn(&findexd, &socket, &state, &root);
    assert!(wait_serving(&fxbin, &socket, Duration::from_secs(10)), "daemon never served");

    // Crawl should see the mounted content.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let p = paths_under(&fxbin, &socket);
        if p.iter().any(|x| x.ends_with("mounted.rs")) {
            break;
        }
        assert!(Instant::now() < deadline, "crawl never indexed the mount: {p:?}");
        std::thread::sleep(Duration::from_millis(80));
    }

    // Unmount under the daemon. `IN_UNMOUNT` is not delivered on this platform
    // (WSL2), so the daemon's mount watchdog must notice the device id change.
    // The mount point still exists, so this is a "different fs is mounted now"
    // event: the daemon rescans with cause=mount_event — loudly, visible in
    // stats, never silent.
    assert!(Command::new("umount").arg(&root).status().unwrap().success());

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(s) = stats_json(&fxbin, &socket) {
            if s.get("mount_events").and_then(|v| v.as_u64()).unwrap_or(0) > 0 {
                let roots = s.get("roots").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let cause = roots
                    .iter()
                    .filter_map(|r| r.get("last_rescan_cause").and_then(|v| v.as_str()))
                    .any(|c| c == "mount_event");
                assert!(cause, "a mount change must be logged with cause=mount_event: {s}");
                break;
            }
        }
        assert!(Instant::now() < deadline, "unmount was never noticed (no mount_event)");
        std::thread::sleep(Duration::from_millis(100));
    }

    // The daemon must keep answering while the mount churns, and must converge
    // to the new (empty) view of the mount point.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let p = paths_under(&fxbin, &socket);
        if !p.iter().any(|x| x.ends_with("mounted.rs")) {
            break;
        }
        assert!(Instant::now() < deadline, "the unmounted content must be reconciled away: {p:?}");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Remount the same backing store: the daemon must recover and re-index it.
    assert!(Command::new("mount").args(["--bind"]).arg(&backing).arg(&root).status().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let _ = fx(&fxbin, &socket, &["rescan", &root.to_string_lossy()]);
        std::thread::sleep(Duration::from_millis(200));
        let p = paths_under(&fxbin, &socket);
        if p.iter().any(|x| x.ends_with("mounted.rs")) {
            break;
        }
        assert!(Instant::now() < deadline, "remount was never recovered: {p:?}");
    }
    let _ = Command::new("umount").arg(&root).status();
    daemon.stop_and_wait();
}

#[test]
fn root_vanishing_degrades_loudly() {
    // When a root's path disappears entirely, the watchdog must mark the root
    // degraded (reason root_gone) rather than silently serving stale data.
    let Some(bins) = bin_dir() else {
        eprintln!("skipping (no FINDEX_BIN_DIR): root vanishing");
        return;
    };
    let findexd = bins.join("findexd");
    let fxbin = bins.join("fx");
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/a.rs"), b"x").unwrap();

    let state = d.path().join("state");
    let socket = d.path().join("findex.sock");
    let mut daemon = DaemonProc::spawn(&findexd, &socket, &state, &root);
    assert!(wait_serving(&fxbin, &socket, Duration::from_secs(10)));
    // Let the crawl land.
    std::thread::sleep(Duration::from_millis(300));

    // Remove the whole root.
    std::fs::remove_dir_all(&root).unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(s) = stats_json(&fxbin, &socket) {
            if s.get("mount_events").and_then(|v| v.as_u64()).unwrap_or(0) > 0 {
                let roots = s.get("roots").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let reason = roots
                    .iter()
                    .find_map(|r| r.get("state").and_then(|v| v.as_str()))
                    .unwrap_or("");
                assert!(
                    reason.starts_with("degraded") && reason.contains("root_gone"),
                    "a vanished root must be degraded with reason root_gone, got {reason:?}"
                );
                daemon.stop_and_wait();
                return;
            }
        }
        assert!(Instant::now() < deadline, "a vanished root was never noticed");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn watch_limit_exhaustion_degrades_loudly_without_panic() {
    let Some(bins) = bin_dir() else {
        eprintln!("skipping (no FINDEX_BIN_DIR): watch-limit exhaustion");
        return;
    };
    let findexd = bins.join("findexd");
    let fxbin = bins.join("fx");

    // Lower the per-user inotify watch limit so our watches run out. Restore it
    // even if the test panics, via a guard.
    let orig = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    let Some(orig) = orig else {
        eprintln!("skipping: cannot read /proc/sys/fs/inotify/max_user_watches");
        return;
    };
    struct Restore(u64);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = Command::new("sysctl")
                .arg("-w")
                .arg(format!("fs.inotify.max_user_watches={}", self.0))
                .status();
        }
    }

    let lowered = Command::new("sysctl")
        .arg("-w")
        .arg("fs.inotify.max_user_watches=8")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !lowered {
        eprintln!("skipping: cannot lower fs.inotify.max_user_watches");
        return;
    }
    let _guard = Restore(orig);

    // A tree with more directories than the (tiny) watch limit.
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    for i in 0..40 {
        let sub = root.join(format!("d{i:02}/sub"));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("f.txt"), b"x").unwrap();
    }
    let state = d.path().join("state");
    let socket = d.path().join("findex.sock");
    let daemon = DaemonProc::spawn(&findexd, &socket, &state, &root);
    assert!(wait_serving(&fxbin, &socket, Duration::from_secs(10)), "daemon never served");

    // The daemon must not panic or hang. Eventually it should report watch
    // failures and mark the root degraded — loudly, not silently.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(s) = stats_json(&fxbin, &socket) {
            let wf = s.get("watch_failures").and_then(|v| v.as_u64()).unwrap_or(0);
            let wl = s.get("watch_limit_hits").and_then(|v| v.as_u64()).unwrap_or(0);
            if wf > 0 || wl > 0 {
                // Verify the root is reported degraded in `stats --roots`.
                let roots = s.get("roots").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                let degraded = roots.iter().any(|r| {
                    r.get("state").and_then(|v| v.as_str()).unwrap_or("").starts_with("degraded")
                });
                assert!(
                    degraded,
                    "watch-limit exhaustion must degrade the root loudly, not silently: {s}"
                );
                drop(daemon);
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "expected watch failures under a lowered inotify watch limit (daemon may have panicked)"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
