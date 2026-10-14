//! Daemon lifecycle (§15): locate the `fflocated` binary and auto-spawn it.
//!
//! Shared by `ff` (which spawns on a failed connect) and `ff-gui` (which
//! attempts one spawn at startup when the daemon is missing), so the two
//! cannot disagree on where the binary lives, which backend to ask for, or
//! which roots to hand over.

use std::path::{Path, PathBuf};

use crate::config_paths::state_dir_from_env;

/// Spawning the daemon failed. The message already says how to fix it (e.g.
/// `FFLOCATED_BIN`), so callers surface it verbatim.
#[derive(Debug)]
pub enum SpawnError {
    Io(String),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for SpawnError {}

/// Where an auto-spawned daemon's output goes: beside its other state
/// (`journal.log`, snapshots), so a death leaves its last words — startup
/// notices, fatal errors, panics — next to the data it was serving.
pub fn log_path_for_state_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("fflocated.log")
}

/// The pidfile a daemon serving `socket` locks: `<socket>.pid` on unix (see
/// `fflocated::config::with_socket`; the default and `--system` layouts agree).
/// There is no pidfile on Windows — the TCP bind itself is the exclusion.
pub fn pidfile_for_socket(socket: &Path) -> PathBuf {
    socket.with_extension("pid")
}

/// Is a *live* daemon holding the pidfile lock for `socket`?
///
/// This is the deaf-daemon detector (§15): a process that holds the lock but
/// answers nothing on the socket — typically an old binary that lost its
/// listener, or a wedged one. The check is a non-destructive shared-lock
/// probe (`LOCK_SH`, never exclusive, never writes), so it cannot disturb a
/// healthy daemon and cannot corrupt the pidfile. Always `false` on Windows
/// and when the pidfile does not exist (no daemon, nothing to detect).
pub fn daemon_lock_held(pidfile: &Path) -> bool {
    #[cfg(unix)]
    {
        let Ok(file) = std::fs::OpenOptions::new().read(true).open(pidfile) else {
            return false;
        };
        use std::os::unix::io::AsRawFd;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
        if rc == 0 {
            // Acquired: nobody holds the exclusive daemon lock.
            return false;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK)
    }
    #[cfg(not(unix))]
    {
        let _ = pidfile;
        false
    }
}

/// Best-effort pid recorded in `pidfile`, for error messages. `None` when the
/// file is missing or unparsable — callers must never treat that as "no
/// daemon", only the lock in [`daemon_lock_held`] decides that.
pub fn pid_in_pidfile(pidfile: &Path) -> Option<i32> {
    std::fs::read_to_string(pidfile).ok()?.trim().parse().ok()
}

/// Loud, actionable wording for the deaf-daemon case: the socket refuses but a
/// live process holds the lock, so spawning another daemon (or waiting) can
/// never help. Never kills anything automatically — the operator decides.
pub fn deaf_daemon_message(socket: &Path, pidfile: &Path) -> String {
    let who = pid_in_pidfile(pidfile)
        .map(|p| format!("pid {p}"))
        .unwrap_or_else(|| "an unknown pid".to_string());
    format!(
        "fflocated at {} refuses connections (os error 111) but {who} holds {}: \
         the daemon is deaf (alive, no listener). Kill it (`kill {pid}`) or remove {} and {}, \
         then retry — `ff` will spawn a fresh daemon",
        socket.display(),
        pidfile.display(),
        socket.display(),
        pidfile.display(),
        pid = pid_in_pidfile(pidfile)
            .map(|p| p.to_string())
            .unwrap_or_else(|| "<pid>".to_string()),
    )
}

/// Spawn `fflocated` detached, inheriting our socket/state overrides.
pub fn spawn_daemon(socket: &Path) -> Result<(), SpawnError> {
    let exe = daemon_binary_path()?;
    let state = state_dir_from_env();
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--socket").arg(socket);
    cmd.arg("--state-dir").arg(&state);
    // Pick a backend this platform actually has. The daemon's own default is
    // inotify, which is Linux-only: spawning it on macOS/Windows would make the
    // child exit immediately ("backend `inotify` is Linux-only") and leave a
    // stale socket behind, so `ff` would fail to connect with ECONNREFUSED.
    cmd.arg("--backend").arg(default_backend_for_platform());
    // Forward the roots to index. Without this the daemon falls back to its own
    // default ($HOME), which makes auto-spawn crawl the entire home directory
    // and can take far longer than the connect window — the daemon is healthy
    // but `ff` reports "connection refused". `FFLOCATE_ROOT` is the same
    // separator-delimited override the daemon understands.
    if let Some(roots) = roots_from_env() {
        if let Ok(joined) = std::env::join_paths(roots) {
            cmd.env("FFLOCATE_ROOT", joined);
        }
    }
    // Detach: stdin reads null, while stdout/stderr append to the daemon log
    // so a later death is diagnosable (a null-stderr daemon dies silently).
    // The daemon only writes startup notices and fatal errors, so the log
    // does not grow without bound. Fail fast when the log cannot be opened:
    // the daemon needs this directory writable for its state anyway.
    let log_path = log_path_for_state_dir(&state);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| SpawnError::Io(format!("cannot create {}: {e}", parent.display())))?;
    }
    let open_log = || {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| {
                SpawnError::Io(format!(
                    "cannot open daemon log {}: {e}",
                    log_path.display()
                ))
            })
    };
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::from(open_log()?));
    cmd.stderr(std::process::Stdio::from(open_log()?));
    let child = cmd
        .spawn()
        .map_err(|e| SpawnError::Io(format!("cannot spawn {}: {e}", exe.display())))?;
    let _ = child.id();
    Ok(())
}

/// The watcher backend to ask for when auto-spawning.
///
/// inotify exists only on Linux; elsewhere the kernel watcher is not ported yet
/// (FSEvents/FSEvents is v1.5), so the daemon runs with the fake backend, which
/// still crawls, serves queries and reconciles on demand — it just does not get
/// live events. `$FFLOCATE_BACKEND` overrides.
fn default_backend_for_platform() -> &'static str {
    if let Ok(b) = std::env::var("FFLOCATE_BACKEND") {
        if b == "inotify" || b == "fake" {
            // Leak is fine: a single short-lived string for a process that is
            // about to exec a child.
            return Box::leak(b.into_boxed_str());
        }
    }
    if cfg!(target_os = "linux") {
        "inotify"
    } else {
        "fake"
    }
}

/// The roots the daemon should index, from `$FFLOCATE_ROOT` if the user set it.
/// `None` means "let the daemon decide" (every readable native filesystem on
/// Linux, `$HOME` elsewhere).
fn roots_from_env() -> Option<Vec<PathBuf>> {
    let v = std::env::var_os("FFLOCATE_ROOT")?;
    if v.is_empty() {
        return None;
    }
    Some(std::env::split_paths(&v).collect())
}

/// Locate the `fflocated` binary.
///
/// Search order: `$FFLOCATED_BIN`, then the directory of `argv[0]` (the invoked
/// path — a symlink is deliberately *not* resolved, so a `ff` symlinked into
/// `~/bin` finds a `fflocated` placed beside it), then the directory of
/// `current_exe()` (which on Linux resolves the symlink target), then `$PATH`.
fn daemon_binary_path() -> Result<PathBuf, SpawnError> {
    if let Ok(p) = std::env::var("FFLOCATED_BIN") {
        return Ok(PathBuf::from(p));
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    // The invoked path as given (may be a symlink). `current_exe()` resolves
    // symlinks on Linux, so this is the only way to see the symlink's own dir.
    if let Some(arg0) = std::env::args_os().next() {
        let p = Path::new(&arg0);
        if let Some(dir) = p.parent() {
            if !dir.as_os_str().is_empty() {
                dirs.push(dir.to_path_buf());
            }
        }
        // A bare `ff` (no slash) has no parent of its own; it is found via PATH.
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    for dir in dirs {
        let cand = dir.join("fflocated");
        if cand.exists() {
            return Ok(cand);
        }
        // On Windows the binary is `fflocated.exe`, and `Path::exists` tries no
        // suffixes — without this the auto-spawn beside a `ff-gui.exe` never
        // finds its daemon.
        #[cfg(windows)]
        {
            let cand_exe = dir.join("fflocated.exe");
            if cand_exe.exists() {
                return Ok(cand_exe);
            }
        }
    }
    // `split_paths` (not a literal `':'` split): PATH entries are `;`
    // separated on Windows, and a naive `':'` split shreds drive letters.
    for dir in std::env::split_paths(&std::env::var("PATH").unwrap_or_default()) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let cand = dir.join("fflocated");
        if cand.exists() {
            return Ok(cand);
        }
        #[cfg(windows)]
        {
            let cand_exe = dir.join("fflocated.exe");
            if cand_exe.exists() {
                return Ok(cand_exe);
            }
        }
    }
    Err(SpawnError::Io(
        "fflocated not found: looked next to ff, next to the invoked path, and on \
         PATH (set FFLOCATED_BIN to override)"
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ff` symlinked into a bin dir must find a `fflocated` placed *beside the
    /// symlink* — `current_exe()` resolves the symlink on Linux, so the
    /// invoked path has to be searched as well.
    #[test]
    fn finds_fflocated_beside_the_invoked_symlink() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        let bin = d.path().join("bin");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&bin).unwrap();

        // A stand-in `fflocated` that exists; the code only checks `.exists()`.
        std::fs::write(bin.join("fflocated"), b"stub").unwrap();

        // Sanity: with PATH cleared and no FFLOCATED_BIN, the search must find the
        // one in `bin/`. We cannot set argv[0] for the test process, so this
        // pins the `$PATH` leg and the directory-probing logic that the symlink
        // fix relies on.
        std::env::remove_var("FFLOCATED_BIN");
        let saved = std::env::var("PATH").ok();
        std::env::set_var("PATH", bin.as_os_str());
        let found = daemon_binary_path();
        match saved {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        let found = found.expect("fflocated must be found on PATH");
        assert_eq!(found, bin.join("fflocated"));
    }

    #[test]
    fn log_path_lives_beside_daemon_state() {
        // The log must be discoverable: next to journal.log/snapshots, not
        // in tmp or beside the socket.
        let state = Path::new("/var/lib/fflocate/2");
        assert_eq!(
            log_path_for_state_dir(state),
            PathBuf::from("/var/lib/fflocate/2/fflocated.log")
        );
    }

    /// The deaf-daemon detector: a held pidfile lock reads as held, a free
    /// one as free, and a missing pidfile as free (no daemon, nothing to
    /// detect) — never an error, never a write.
    #[test]
    fn daemon_lock_probe_distinguishes_held_from_free() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("fflocate.pid");
        // Missing pidfile: no daemon.
        assert!(!daemon_lock_held(&pidfile));
        // Free pidfile: create it unlocked and probe.
        std::fs::write(&pidfile, "12345").unwrap();
        assert!(!daemon_lock_held(&pidfile));
        // Content untouched by the probe (shared lock, never exclusive).
        assert_eq!(std::fs::read_to_string(&pidfile).unwrap(), "12345");
        // Held lock: a second handle with LOCK_EX|LOCK_NB in this process
        // conflicts exactly the way a live daemon's lock does.
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
            assert!(daemon_lock_held(&pidfile), "a held lock must read as held");
        }
    }

    #[test]
    fn deaf_daemon_message_names_socket_pid_and_remedy() {
        let msg = deaf_daemon_message(Path::new("/run/fflocate.sock"), Path::new("/run/fflocate.pid"));
        // No pid recorded here (missing file): still actionable, never a panic.
        assert!(msg.contains("/run/fflocate.sock"), "{msg}");
        assert!(msg.contains("/run/fflocate.pid"), "{msg}");
        assert!(msg.contains("111"), "{msg}");
        assert!(msg.contains("Kill it"), "{msg}");
        assert!(msg.contains("spawn a fresh daemon"), "{msg}");
    }
}
