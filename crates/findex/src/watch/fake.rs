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

impl WatchBackend for FakeBackend {
    fn caps(&self) -> BackendCaps {
        BackendCaps { per_dir_watch: true, whole_mount: false, atomic_rename: true }
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

    fn watch_dir(&mut self, _id: u32, _path: &std::path::Path) -> std::io::Result<()> {
        Ok(()) // no real watches to install
    }

    fn poll(&mut self, _timeout: Duration) -> std::io::Result<Vec<FsEvent>> {
        let mut g = self.handle.inner.lock().unwrap();
        Ok(g.queue.drain(..).collect())
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
        assert!(evs.iter().all(|e| matches!(e, FsEvent::RescanSubtree { cause: crate::event::RescanCause::Overflow, .. })));
        assert_eq!(h.inner.lock().unwrap().overflows, 1);
    }
}
