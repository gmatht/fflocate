//! Deaf-daemon detector tests (§15).
//!
//! A live process holding the pidfile lock while serving nothing on the
//! socket is the signature of connection-refused-111-with-a-live-daemon.
//! These pin the detector headless: held reads as held, free reads as free,
//! the probe never writes, and the message names the socket, the pid and the
//! remedy.
//!
//! Lives in an integration target because the concurrent `expr.rs` work
//! currently breaks the `findex` lib-test target (missing `is_exec` test
//! initializers); the unit tests in `src/spawn.rs` cover the same ground and
//! will run once that is fixed.

use std::path::Path;

#[test]
fn lock_probe_distinguishes_held_from_free() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("findex.pid");
    // Missing pidfile: no daemon.
    assert!(!findex::spawn::daemon_lock_held(&pidfile));
    // Free pidfile: probe reads, never writes.
    std::fs::write(&pidfile, "12345").unwrap();
    assert!(!findex::spawn::daemon_lock_held(&pidfile));
    assert_eq!(std::fs::read_to_string(&pidfile).unwrap(), "12345");
    // Held lock: an exclusive LOCK_EX|LOCK_NB in this process conflicts
    // exactly the way a live daemon's lock does.
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pidfile)
            .unwrap();
        let rc = unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "test setup: exclusive lock must acquire");
        assert!(
            findex::spawn::daemon_lock_held(&pidfile),
            "held must read as held"
        );
    }
}

#[test]
fn deaf_message_names_socket_pid_and_remedy() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("findex.pid");
    std::fs::write(&pidfile, "424242").unwrap();
    let socket = Path::new("/run/findex.sock");
    let msg = findex::spawn::deaf_daemon_message(socket, &pidfile);
    assert!(msg.contains("/run/findex.sock"), "{msg}");
    assert!(msg.contains("424242"), "{msg}");
    assert!(msg.contains("111"), "{msg}");
    assert!(msg.contains("Kill it"), "{msg}");
    assert!(msg.contains("spawn a fresh daemon"), "{msg}");
}

#[test]
fn pidfile_for_socket_mirrors_daemon_layout() {
    // `with_socket` in findexd's config uses `<socket>.pid`; the detector
    // must look in the same place, or it probes the wrong file.
    assert_eq!(
        findex::spawn::pidfile_for_socket(Path::new("/run/findex.sock")),
        Path::new("/run/findex.pid")
    );
}
