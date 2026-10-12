//! The fake watcher backend (§14.1).
//!
//! Injects arbitrary [`FsEvent`] sequences without a real filesystem —
//! *including events that lie* (drop a create, duplicate a rename, interleave
//! unrelated dirs). Model-based tests assert the index converges to the truth
//! after rescans. Runs in milliseconds, 100× faster than FS-backed tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{BackendCaps, WatchBackend};
use crate::event::FsEvent;
use crate::roots::Root;

/// Shared handle used by tests to inject events and simulate faults.
#[derive(Clone, Default)]
pub struct FakeHandle {
    inner: Arc<Mutex<FakeInner>>,
}

#[derive(Default)]
struct FakeInner {
    queue: VecDeque<FsEvent>,
    roots: Vec<u32>,
    /// (dir id, path) learned from `watch_dir` — the same mapping inotify
    /// learns from those calls. Only read by the Windows cookie scan.
    #[cfg(windows)]
    watched: Vec<(u32, std::path::PathBuf)>,
    /// Number of simulated `IN_Q_OVERFLOW`s, for fault-injection assertions.
    pub overflows: u64,
}

impl FakeHandle {
    pub fn new() -> FakeHandle {
        FakeHandle::default()
    }

    /// Inject one event (as the kernel would deliver it, in order).
    pub fn inject(&self, ev: FsEvent) {
        self.inner.lock().unwrap().queue.push_back(ev);
    }

    pub fn inject_all(&self, evs: impl IntoIterator<Item = FsEvent>) {
        let mut g = self.inner.lock().unwrap();
        for e in evs {
            g.queue.push_back(e);
        }
    }

    /// Simulate an inotify queue overflow: the backend emits a rescan of each
    /// watched root, exactly as the kernel-level overflow would (§6).
    pub fn inject_overflow(&self, dirs: impl IntoIterator<Item = u32>) {
        let mut g = self.inner.lock().unwrap();
        g.overflows += 1;
        for d in dirs {
            g.queue.push_back(FsEvent::rescan(
                crate::event::DirKey::new(d),
                crate::event::RescanCause::Overflow,
            ));
        }
    }

    /// Simulate a dropped watch on a directory (`IN_IGNORED`): the daemon must
    /// rescan that subtree to heal.
    pub fn inject_watch_dropped(&self, dir: u32) {
        self.inner.lock().unwrap().queue.push_back(FsEvent::rescan(
            crate::event::DirKey::new(dir),
            crate::event::RescanCause::WatchDropped,
        ));
    }

    pub fn pending(&self) -> usize {
        self.inner.lock().unwrap().queue.len()
    }

    /// A lying event stream: drop the create for `x` entirely, then later
    /// resurrect it. Used to prove a rescan heals a missed event.
    pub fn inject_missing_then_real(&self, parent: u32, name: &[u8], is_dir: bool) {
        // First the "real" filesystem grows the file but no event arrives, then
        // an unrelated event triggers a rescan of the parent.
        self.inject(FsEvent::rescan(
            crate::event::DirKey::new(parent),
            crate::event::RescanCause::Manual,
        ));
        let _ = (name, is_dir);
    }
}

/// The fake backend itself.
pub struct FakeBackend {
    handle: FakeHandle,
}

impl FakeBackend {
    pub fn new(handle: FakeHandle) -> Self {
        FakeBackend { handle }
    }

}

/// Scan `(dir id, path)` pairs for sync-cookie files and emit create events
/// for them. Wired into `poll` on Windows only (unix watchers deliver cookie
/// events from the kernel): without a kernel watcher this is what lets
/// `sync`/`update` complete — the cookie file's presence is observed in
/// stream order like any other event, and `push_event` routes it to the
/// cookie marker (never the index; crawl skips cookies too). One
/// non-recursive readdir per watched dir per poll; watched dirs are roots,
/// which are small.
#[cfg(any(windows, test))]
fn scan_cookie_events(watched: &[(u32, std::path::PathBuf)]) -> Vec<FsEvent> {
    let mut out = Vec::new();
    for (id, path) in watched {
        let Ok(rd) = std::fs::read_dir(path) else {
            continue;
        };
        for ent in rd.flatten() {
            let name = crate::applier::os_str_bytes(&ent.file_name());
            if crate::util::is_cookie(&name) {
                out.push(crate::event::FsEvent::upsert(
                    crate::event::DirKey::new(*id),
                    name,
                    false,
                ));
            }
        }
    }
    out
}

impl WatchBackend for FakeBackend {
    fn caps(&self) -> BackendCaps {
        BackendCaps {
            per_dir_watch: true,
            whole_mount: false,
            atomic_rename: true,
        }
    }

    fn add_root(&mut self, root: &Root) -> std::io::Result<()> {
        self.handle.inner.lock().unwrap().roots.push(root.id);
        Ok(())
    }

    fn remove_root(&mut self, root: &Root) -> std::io::Result<()> {
        let mut g = self.handle.inner.lock().unwrap();
        g.roots.retain(|r| *r != root.id);
        Ok(())
    }

    fn watch_dir(&mut self, id: u32, path: &std::path::Path) -> std::io::Result<()> {
        // No real watches to install — but record the mapping (as inotify
        // would learn it) for the Windows cookie scan below.
        #[cfg(windows)]
        {
            let mut g = self.handle.inner.lock().unwrap();
            if !g.watched.iter().any(|(i, _)| *i == id) {
                g.watched.push((id, path.to_path_buf()));
            }
        }
        let _ = (id, path);
        Ok(())
    }

    fn poll(&mut self, _timeout: Duration) -> std::io::Result<Vec<FsEvent>> {
        let mut g = self.handle.inner.lock().unwrap();
        // `mut` for the Windows cookie scan below; plain drain elsewhere.
        #[allow(unused_mut)]
        let mut out: Vec<FsEvent> = g.queue.drain(..).collect();
        #[cfg(windows)]
        {
            let watched = g.watched.clone();
            out.extend(scan_cookie_events(&watched));
        }
        Ok(out)
    }

    fn name(&self) -> &'static str {
        "fake"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::DirKey;

    #[test]
    fn inject_and_poll_in_order() {
        let h = FakeHandle::new();
        let mut b = FakeBackend::new(h.clone());
        h.inject(FsEvent::upsert(DirKey::new(1), b"a".to_vec(), false));
        h.inject(FsEvent::remove(DirKey::new(1), b"b".to_vec()));
        let evs = b.poll(Duration::ZERO).unwrap();
        assert_eq!(evs.len(), 2);
        assert!(matches!(evs[0], FsEvent::Upsert { .. }));
        assert!(matches!(evs[1], FsEvent::Remove { .. }));
        assert_eq!(b.poll(Duration::ZERO).unwrap().len(), 0);
    }

    #[test]
    fn overflow_emits_rescans() {
        let h = FakeHandle::new();
        let mut b = FakeBackend::new(h.clone());
        h.inject_overflow([1, 2]);
        let evs = b.poll(Duration::ZERO).unwrap();
        assert_eq!(evs.len(), 2);
        assert!(evs.iter().all(|e| matches!(
            e,
            FsEvent::RescanSubtree {
                cause: crate::event::RescanCause::Overflow,
                ..
            }
        )));
        assert_eq!(h.inner.lock().unwrap().overflows, 1);
    }

    #[test]
    fn cookie_scan_emits_markers_that_complete_a_cookie() {
        use crate::pending::{Pending, PushOutcome};

        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".findex-cookie-7"), b"").unwrap();
        std::fs::write(d.path().join("real.txt"), b"x").unwrap();

        // Only the cookie file is emitted, keyed by the watched dir id.
        let evs = scan_cookie_events(&[(42, d.path().to_path_buf())]);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            FsEvent::Upsert { parent, name, .. } => {
                assert_eq!(parent.0, 42);
                assert_eq!(name, b".findex-cookie-7");
            }
            other => panic!("expected an Upsert marker, got {other:?}"),
        }

        // And end-to-end through the pending collection: the marker is
        // recorded (never indexed) and completes the registered cookie once
        // the queues are drained — the chain `sync` relies on.
        let mut p = Pending::new(16);
        p.register_cookie(7, 42, b".findex-cookie-7".to_vec());
        for e in evs {
            assert_eq!(p.push_event(e), PushOutcome::CookieMarker);
        }
        assert_eq!(p.take_completed_cookies().len(), 1);
    }
}
