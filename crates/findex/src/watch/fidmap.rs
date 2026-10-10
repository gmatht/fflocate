//! FID → directory-id resolution for the fanotify backend (§6).
//!
//! A fanotify event names its parent directory by an opaque **file handle**, not
//! a path and not a watch descriptor. To turn that into the index's [`DirKey`]
//! we must have asked the kernel for the same handle while we had the directory
//! open during the crawl. That is this module's whole job:
//!
//! ```text
//! crawl dir  ──name_to_handle_at──►  handle ──┐
//!                                             ├─► FidMap ──► DirKey
//! fanotify event ──DFID record──────►  handle ─┘
//! ```
//!
//! ## Why the fsid is not trusted as a key
//!
//! The DFID record carries an `fsid`, and `name_to_handle_at` returns a
//! `mount_id`. They are *not* the same value on all kernels/filesystems, so
//! keying on them would silently fail to match. Instead the map is keyed on
//! `(handle_type, handle_bytes)` — the kernel's own identity for the object —
//! and the fsid is kept only as a secondary check when both sides have one.
//!
//! ## Never silently drop
//!
//! A whole-mount fanotify watch sees the entire filesystem, so most events
//! name directories we never indexed and have no handle for. Those are the
//! *expected* majority and are simply ignored. But an event whose parent **is**
//! one of our roots and still fails to resolve is a different thing: it means
//! the map is stale, and dropping it would be a silent desync (§7). The resolver
//! distinguishes the two and reports the second as a rescan request.

use std::collections::HashMap;

use crate::event::{DirKey, FsEvent, RescanCause};
use crate::mount::HandleId;

/// A learned `handle → directory id` mapping.
///
/// Keys are the kernel's handle identity; values are index directory ids.
#[derive(Debug, Default)]
pub struct FidMap {
    by_handle: HashMap<HandleKey, u32>,
    /// Handles that belong to the roots we index, so an unresolved event can be
    /// told apart from "someone else's file".
    root_handles: std::collections::HashSet<HandleKey>,
    /// How many events resolved. For `ff stats`.
    pub resolved: u64,
    /// How many events failed to resolve *and* looked like they should have.
    /// Each one also produced a rescan. For `ff stats`.
    pub escalated: u64,
    /// How many events were ignored because they were plainly not ours.
    pub ignored_foreign: u64,
}

/// The kernel's identity for a file handle, independent of paths and of the
/// (not-always-equal) mount id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HandleKey {
    pub handle_type: i32,
    pub handle: Vec<u8>,
}

impl HandleKey {
    pub fn of(h: &HandleId) -> HandleKey {
        HandleKey { handle_type: h.handle_type, handle: h.handle.clone() }
    }

    /// The key for a fanotify-decoded [`crate::watch::fanotify::Fid`] identity.
    /// The fsid is deliberately dropped: it is not comparable across the two
    /// syscalls that produce handles.
    pub fn of_identity(fid: &crate::watch::fanotify::Fid) -> HandleKey {
        HandleKey { handle_type: fid.handle_type, handle: fid.handle.clone() }
    }

    /// As [`HandleKey::of_identity`], for the name-less identity a caller passes
    /// to `learn_fid`.
    pub fn of_fid_identity(fid: &crate::watch::fanotify::FidIdentity) -> HandleKey {
        HandleKey { handle_type: fid.handle_type, handle: fid.handle.clone() }
    }
}

impl FidMap {
    pub fn new() -> FidMap {
        FidMap::default()
    }

    /// Learn that `handle` names directory `dir_id`.
    pub fn learn(&mut self, handle: &HandleId, dir_id: u32) {
        self.by_handle.insert(HandleKey::of(handle), dir_id);
    }

    /// Mark a handle as belonging to a root we index. An event aimed at a root
    /// directory that we cannot resolve is a stale-map error, not foreign
    /// traffic.
    pub fn learn_root(&mut self, handle: &HandleId, dir_id: u32) {
        let k = HandleKey::of(handle);
        self.by_handle.insert(k.clone(), dir_id);
        self.root_handles.insert(k);
    }

    pub fn resolve(&self, handle_type: i32, handle: &[u8]) -> Option<u32> {
        self.by_handle
            .get(&HandleKey { handle_type, handle: handle.to_vec() })
            .copied()
    }

    /// Forget every mapping for `dir_id` (root removed / remounted).
    pub fn forget_dir(&mut self, dir_id: u32) {
        self.by_handle.retain(|_, v| *v != dir_id);
        self.root_handles.retain(|k| self.by_handle.get(k).copied() != Some(dir_id));
    }

    pub fn len(&self) -> usize {
        self.by_handle.len()
    }
    pub fn is_empty(&self) -> bool {
        self.by_handle.is_empty()
    }

    /// Resolve one fanotify DFID into a directory id, or explain why not.
    pub fn lookup(&self, handle_type: i32, handle: &[u8]) -> Lookup {
        match self.resolve(handle_type, handle) {
            Some(id) => Lookup::Found(DirKey::new(id)),
            None => Lookup::Unknown,
        }
    }

    /// Turn an event's parent DFID + name + mask into [`FsEvent`]s, following the
    /// never-silently-drop rule.
    ///
    /// `parent` is the raw handle from the event; `root_dir_for` answers "which
    /// root, if any, does this unresolved handle plausibly belong to?" — the
    /// caller supplies it because only the daemon knows the roots. Returning
    /// `Some(root)` turns an unresolved event into `RescanSubtree(root)`, which
    /// is work, not silence.
    pub fn translate<F>(
        &mut self,
        handle_type: i32,
        handle: &[u8],
        mask: u64,
        name: &[u8],
        is_dir: bool,
        mut root_dir_for: F,
    ) -> Vec<FsEvent>
    where
        F: FnMut(&[u8]) -> Option<u32>,
    {
        match self.resolve(handle_type, handle) {
            Some(id) => {
                self.resolved += 1;
                namespace_event(mask, DirKey::new(id), name, is_dir)
            }
            None => {
                if let Some(root) = root_dir_for(handle) {
                    // It looked like ours (a root subtree) but we have no handle
                    // for it: the map is stale. Rescan; do not drop.
                    self.escalated += 1;
                    vec![FsEvent::rescan(DirKey::new(root), RescanCause::WatchDropped)]
                } else {
                    self.ignored_foreign += 1;
                    Vec::new()
                }
            }
        }
    }
}

/// The outcome of a handle lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    Found(DirKey),
    Unknown,
}

/// Map a namespace event's mask + resolved parent into index events. Rename
/// pairing is the backend's job (it needs cross-event state); this is the
/// single-event half.
pub fn namespace_event(mask: u64, parent: DirKey, name: &[u8], is_dir: bool) -> Vec<FsEvent> {
    use crate::watch::fanotify::masks::*;
    if mask & FAN_MOVED_FROM != 0 {
        return vec![FsEvent::remove(parent, name.to_vec())];
    }
    if mask & FAN_MOVED_TO != 0 {
        return vec![FsEvent::upsert(parent, name.to_vec(), is_dir)];
    }
    if mask & (FAN_DELETE | FAN_DELETE_SELF) != 0 {
        return vec![FsEvent::remove(parent, name.to_vec())];
    }
    if mask & (FAN_CREATE | FAN_MOVE_SELF) != 0 {
        return vec![FsEvent::upsert(parent, name.to_vec(), is_dir)];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::fanotify::masks::{FAN_CREATE, FAN_DELETE, FAN_MOVED_FROM, FAN_MOVED_TO};

    fn h(bytes: &[u8]) -> HandleId {
        HandleId { mount_id: 1, handle_type: 1, handle: bytes.to_vec() }
    }

    #[test]
    fn learn_then_resolve() {
        let mut m = FidMap::new();
        m.learn(&h(b"abc"), 7);
        assert_eq!(m.resolve(1, b"abc"), Some(7));
        assert_eq!(m.resolve(1, b"zzz"), None);
        // A different handle_type is a different object.
        assert_eq!(m.resolve(2, b"abc"), None);
    }

    #[test]
    fn forget_dir_drops_only_that_dir() {
        let mut m = FidMap::new();
        m.learn(&h(b"a"), 1);
        m.learn(&h(b"b"), 2);
        m.forget_dir(1);
        assert_eq!(m.resolve(1, b"a"), None);
        assert_eq!(m.resolve(1, b"b"), Some(2));
    }

    #[test]
    fn unresolved_foreign_event_is_dropped_quietly() {
        let mut m = FidMap::new();
        let evs = m.translate(1, b"stranger", FAN_CREATE, b"x", false, |_| None);
        assert!(evs.is_empty());
        assert_eq!(m.ignored_foreign, 1);
        assert_eq!(m.escalated, 0, "foreign traffic is not an error");
    }

    #[test]
    fn unresolved_root_event_escalates_to_a_rescan() {
        // The map is missing a handle that belongs to a root we index: dropping
        // it would be a silent desync, so it becomes work.
        let mut m = FidMap::new();
        let evs = m.translate(1, b"stale", FAN_CREATE, b"x", false, |_| Some(42));
        assert_eq!(evs, vec![FsEvent::rescan(DirKey::new(42), RescanCause::WatchDropped)]);
        assert_eq!(m.escalated, 1);
    }

    #[test]
    fn resolved_create_becomes_upsert() {
        let mut m = FidMap::new();
        m.learn(&h(b"dir"), 5);
        let evs = m.translate(1, b"dir", FAN_CREATE, b"new.rs", false, |_| None);
        assert_eq!(evs, vec![FsEvent::upsert(DirKey::new(5), b"new.rs".to_vec(), false)]);
        assert_eq!(m.resolved, 1);
    }

    #[test]
    fn namespace_event_covers_every_mask_we_subscribe_to() {
        use crate::watch::fanotify::masks::*;
        let d = DirKey::new(3);
        assert_eq!(
            namespace_event(FAN_CREATE, d, b"a", false),
            vec![FsEvent::upsert(d, b"a".to_vec(), false)]
        );
        assert_eq!(
            namespace_event(FAN_DELETE, d, b"a", false),
            vec![FsEvent::remove(d, b"a".to_vec())]
        );
        assert_eq!(
            namespace_event(FAN_MOVED_TO, d, b"a", true),
            vec![FsEvent::upsert(d, b"a".to_vec(), true)]
        );
        assert_eq!(
            namespace_event(FAN_MOVED_FROM, d, b"a", false),
            vec![FsEvent::remove(d, b"a".to_vec())]
        );
        // A mask we do not handle (e.g. a bare ONDIR with no op) yields nothing.
        assert!(namespace_event(0, d, b"a", false).is_empty());
    }

    #[test]
    fn stats_are_counted_separately() {
        let mut m = FidMap::new();
        m.learn(&h(b"d"), 1);
        m.translate(1, b"d", FAN_CREATE, b"a", false, |_| None); // resolved
        m.translate(1, b"nope", FAN_CREATE, b"a", false, |_| None); // foreign
        m.translate(1, b"stale", FAN_CREATE, b"a", false, |_| Some(9)); // escalated
        assert_eq!((m.resolved, m.ignored_foreign, m.escalated), (1, 1, 1));
    }
}
