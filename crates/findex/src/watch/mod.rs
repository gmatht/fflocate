//! Watcher backends (§6).
//!
//! One internal event language ([`FsEvent`]), pluggable backends. Every
//! backend delivers a *globally ordered* event stream — that property is what
//! makes the cookie trick (§8) sound.
//!
//! v1 ships:
//!
//! * [`fake::FakeBackend`] — a test backend that injects arbitrary event
//!   sequences without a filesystem, for model-based reconciliation tests.
//! * [`inotify::InotifyBackend`] — the real Linux backend, one watch per
//!   directory, with `IN_Q_OVERFLOW` → rescan and `IN_IGNORED`/`IN_UNMOUNT`
//!   handling.
//!
//! fanotify-FID and FSEvents slot in behind the same trait (§17, v1.5).

use std::time::Duration;

use crate::event::FsEvent;
use crate::roots::Root;

pub mod fake;
/// The real Linux backend. Gated: inotify is a Linux-only API (FSEvents is
/// v1.5, §17), so on other platforms the crate builds with the fake backend
/// only and the daemon reports `inotify` as unavailable.
#[cfg(target_os = "linux")]
pub mod inotify;

pub use fake::FakeBackend;
#[cfg(target_os = "linux")]
pub use inotify::InotifyBackend;

/// Capabilities a backend advertises, so the applier can adapt rather than
/// special-case (§6: "encode this in backend capabilities instead of
/// special-casing").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendCaps {
    /// The backend can attach watches per directory (inotify) → exclusions can
    /// be applied at watch time.
    pub per_dir_watch: bool,
    /// Whole-mount watch (fanotify/FSEvents) → the applier must filter excluded
    /// paths itself.
    pub whole_mount: bool,
    /// The backend reports atomic rename pairs, so a rename is one logical op.
    pub atomic_rename: bool,
}

/// The backend interface. `poll` returns a globally ordered batch; an empty
/// Vec with a timeout means "nothing happened", not "the stream ended".
pub trait WatchBackend: Send {
    fn caps(&self) -> BackendCaps;
    /// Start watching `root`. Idempotent.
    fn add_root(&mut self, root: &Root) -> std::io::Result<()>;
    /// Stop watching `root`.
    fn remove_root(&mut self, root: &Root) -> std::io::Result<()>;
    /// Watch a specific directory by its index id and absolute path. Backends
    /// that watch whole mounts (fanotify/FSEvents) no-op here; inotify adds a
    /// watch. Idempotent.
    fn watch_dir(&mut self, id: u32, path: &std::path::Path) -> std::io::Result<()>;
    /// Return any events that arrived within `timeout`. Ordered.
    fn poll(&mut self, timeout: Duration) -> std::io::Result<Vec<FsEvent>>;
    /// Human-readable backend name for `ff stats`.
    fn name(&self) -> &'static str;
}
