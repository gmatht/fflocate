//! Sync cookies (§8).
//!
//! The question every consumer asks: *"has the daemon seen everything that
//! happened before now?"* We answer it the Watchman way: create a uniquely
//! named file in each writable root, let its create event arrive through the
//! same globally ordered watcher stream as everything before it, and complete
//! when that marker is applied *and* the pending queues for the scope are
//! drained.
//!
//! Guarantee (success): every FS operation that completed before `sync` was
//! called is reflected in the index. Cookie names are never indexed and never
//! appear in subscriber feeds (§16).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use crate::util::COOKIE_PREFIX;

/// A cookie the daemon is waiting on.
#[derive(Debug)]
struct PendingCookie {
    root: u32,
    name: Vec<u8>,
    path: Option<PathBuf>,
    tx: Sender<()>,
}

/// Registry shared between the applier (which marks cookies seen / complete)
/// and client threads (which wait on them).
#[derive(Default)]
pub struct SyncRegistry {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    by_token: HashMap<u64, PendingCookie>,
    by_name: HashMap<(u32, Vec<u8>), u64>,
}

impl SyncRegistry {
    pub fn new() -> Arc<SyncRegistry> {
        Arc::new(SyncRegistry::default())
    }

    /// Register a cookie the daemon will wait on. Returns the receiver the
    /// caller blocks on.
    pub fn expect(&self, token: u64, root: u32, name: Vec<u8>, path: Option<PathBuf>) -> Receiver<()> {
        let (tx, rx) = bounded(1);
        let mut g = self.inner.lock().unwrap();
        g.by_name.insert((root, name.clone()), token);
        g.by_token.insert(token, PendingCookie { root, name, path, tx });
        rx
    }

    /// Note that a cookie is pending (used by the applier when it registers via
    /// the pending collection). No channel yet — the waiter registers first.
    pub fn note_pending(&self, _token: u64) {}

    /// Map a watcher-observed `(parent, name)` to a cookie token.
    pub fn token_for_name(&self, root: u32, name: &[u8]) -> Option<u64> {
        let g = self.inner.lock().unwrap();
        g.by_name.get(&(root, name.to_vec())).copied()
    }

    /// Complete a cookie: wake its waiter and forget it.
    pub fn complete(&self, token: u64) {
        let entry = {
            let mut g = self.inner.lock().unwrap();
            let e = g.by_token.remove(&token);
            if let Some(e) = &e {
                g.by_name.remove(&(e.root, e.name.clone()));
            }
            e
        };
        if let Some(e) = entry {
            let _ = e.tx.try_send(());
        }
    }

    pub fn cookie_path(&self, token: u64) -> Option<PathBuf> {
        let g = self.inner.lock().unwrap();
        g.by_token.get(&token).and_then(|c| c.path.clone())
    }

    pub fn is_waiting(&self, token: u64) -> bool {
        self.inner.lock().unwrap().by_token.contains_key(&token)
    }

    /// Abandon a cookie (timeout / error).
    pub fn forget(&self, token: u64) -> Option<PathBuf> {
        let mut g = self.inner.lock().unwrap();
        let e = g.by_token.remove(&token)?;
        g.by_name.remove(&(e.root, e.name.clone()));
        e.path
    }
}

/// Build a cookie file name for `token`.
pub fn cookie_name(token: u64) -> Vec<u8> {
    format!("{COOKIE_PREFIX}{token}").into_bytes()
}

/// Create the cookie file in `root_dir`. Returns the full path.
pub fn create_cookie(root_dir: &Path, token: u64) -> std::io::Result<PathBuf> {
    let name = cookie_name(token);
    let path = root_dir.join(std::ffi::OsString::from(String::from_utf8_lossy(&name).into_owned()));
    // Write a little content so the create is unambiguous even on lazy mounts.
    std::fs::write(&path, b"")?;
    Ok(path)
}

/// Per-root outcome of a sync (§8, §11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncStatus {
    Ok,
    /// Read-only root: no cookie possible; weak guarantee.
    Weak,
    /// A root that repeatedly fails cookie sync.
    Degraded(String),
    Timeout,
    /// A root recorded by another OS (dual-boot shared store, §15): not
    /// attempted here, by rule rather than by failure.
    Foreign,
}

impl SyncStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncStatus::Ok => "ok",
            SyncStatus::Weak => "weak",
            SyncStatus::Degraded(_) => "degraded",
            SyncStatus::Timeout => "timeout",
            SyncStatus::Foreign => "foreign",
        }
    }
}

/// Wait for `rx` up to `timeout`. Returns true if completed in time.
pub fn wait_for(rx: &Receiver<()>, timeout: Duration) -> bool {
    match rx.recv_timeout(timeout) {
        Ok(()) => true,
        Err(RecvTimeoutError::Timeout) => false,
        Err(RecvTimeoutError::Disconnected) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_name_has_prefix() {
        let n = cookie_name(3);
        assert!(n.starts_with(COOKIE_PREFIX.as_bytes()));
        assert!(crate::util::is_cookie(&n));
    }

    #[test]
    fn complete_wakes_waiter() {
        let reg = SyncRegistry::default();
        let rx = reg.expect(1, 0, cookie_name(1), None);
        assert_eq!(reg.token_for_name(0, &cookie_name(1)), Some(1));
        reg.complete(1);
        assert!(wait_for(&rx, Duration::from_millis(50)));
        assert!(!reg.is_waiting(1));
    }

    #[test]
    fn timeout_when_never_completed() {
        let reg = SyncRegistry::default();
        let rx = reg.expect(2, 0, cookie_name(2), None);
        assert!(!wait_for(&rx, Duration::from_millis(20)));
        assert!(reg.is_waiting(2));
        let _ = reg.forget(2);
    }

    #[test]
    fn cookie_file_created_and_removable() {
        let d = tempfile::tempdir().unwrap();
        let p = create_cookie(d.path(), 7).unwrap();
        assert!(p.exists());
        std::fs::remove_file(&p).unwrap();
        assert!(!p.exists());
    }
}
