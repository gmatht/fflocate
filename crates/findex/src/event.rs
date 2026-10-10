//! The internal filesystem event language (§6).
//!
//! Every watcher backend translates its native events into this one type, so
//! the applier and reconciliation layer never learn about inotify vs fanotify
//! vs FSEvents.

use crate::index::ROOT_ID;

pub type RootId = u32;

/// An opaque directory key.
///
/// For the inotify backend this is the directory's index id (we know it from
/// the wd→id map). For a model test backend it can be any stable identifier.
/// `ROOT_ID` (0) denotes the root itself, matching the index's parent field.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct DirKey(pub u32);

impl DirKey {
    pub const ROOT: DirKey = DirKey(ROOT_ID);
    pub fn new(id: u32) -> Self {
        DirKey(id)
    }
}

/// Why a subtree rescan was requested (§7). Every rescan logs its cause; the
/// distinction between "a rescan happened" and "a rescan happened *because of
/// an overflow*" is the whole point of the health counters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RescanCause {
    Overflow,
    WatchDropped,
    CookieTimeout,
    JournalCorrupt,
    MountEvent,
    Manual,
    /// Post-crawl reconciliation: a dir got events after it was enumerated
    /// (§5).
    PostCrawl,
    /// Cross-OS handoff in a shared store (§15): another OS wrote last, so
    /// local roots are re-reconciled unconditionally at startup.
    OsHandoff,
}

impl RescanCause {
    pub fn as_str(&self) -> &'static str {
        match self {
            RescanCause::Overflow => "overflow",
            RescanCause::WatchDropped => "watch_dropped",
            RescanCause::CookieTimeout => "cookie_timeout",
            RescanCause::JournalCorrupt => "journal_corrupt",
            RescanCause::MountEvent => "mount_event",
            RescanCause::Manual => "manual",
            RescanCause::PostCrawl => "post_crawl",
            RescanCause::OsHandoff => "os_handoff",
        }
    }

    pub fn parse(s: &str) -> Option<RescanCause> {
        Some(match s {
            "overflow" => RescanCause::Overflow,
            "watch_dropped" => RescanCause::WatchDropped,
            "cookie_timeout" => RescanCause::CookieTimeout,
            "journal_corrupt" => RescanCause::JournalCorrupt,
            "mount_event" => RescanCause::MountEvent,
            "manual" => RescanCause::Manual,
            "post_crawl" => RescanCause::PostCrawl,
            "os_handoff" => RescanCause::OsHandoff,
            _ => return None,
        })
    }
}

/// A single globally-ordered filesystem event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsEvent {
    /// A name was created or its type changed. `is_dir` is a hint; the applier
    /// verifies with the index/crawler when it matters.
    Upsert {
        parent: DirKey,
        name: Vec<u8>,
        is_dir: bool,
    },
    /// A name disappeared.
    Remove { parent: DirKey, name: Vec<u8> },
    /// Re-enumerate the whole subtree rooted at `dir` and diff against the
    /// index (§7). Never a silent drop.
    RescanSubtree { dir: DirKey, cause: RescanCause },
    /// The mount backing the subtree rooted at `dir` went away; drop the
    /// subtree, await remount. `dir` is a directory id (roots are identified in
    /// the index by their root-dir entry, not by the roots-table index, which a
    /// backend cannot know).
    RootGone { dir: DirKey },
}

impl FsEvent {
    pub fn upsert(parent: DirKey, name: impl Into<Vec<u8>>, is_dir: bool) -> FsEvent {
        FsEvent::Upsert { parent, name: name.into(), is_dir }
    }
    pub fn remove(parent: DirKey, name: impl Into<Vec<u8>>) -> FsEvent {
        FsEvent::Remove { parent, name: name.into() }
    }
    pub fn rescan(dir: DirKey, cause: RescanCause) -> FsEvent {
        FsEvent::RescanSubtree { dir, cause }
    }

    /// The dir this event is *about*, for coalescing and root attribution.
    pub fn target_dir(&self) -> Option<DirKey> {
        match self {
            FsEvent::Upsert { parent, .. } | FsEvent::Remove { parent, .. } => Some(*parent),
            FsEvent::RescanSubtree { dir, .. } => Some(*dir),
            FsEvent::RootGone { .. } => None,
        }
    }

    /// Whether this event should be filtered before it reaches the index: sync
    /// cookies must never be indexed nor shown to subscribers (§8, §16).
    pub fn is_cookie_event(&self) -> bool {
        match self {
            FsEvent::Upsert { name, .. } | FsEvent::Remove { name, .. } => crate::util::is_cookie(name),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causes_roundtrip() {
        for c in [
            RescanCause::Overflow,
            RescanCause::WatchDropped,
            RescanCause::CookieTimeout,
            RescanCause::JournalCorrupt,
            RescanCause::MountEvent,
            RescanCause::Manual,
            RescanCause::PostCrawl,
        ] {
            assert_eq!(RescanCause::parse(c.as_str()), Some(c));
        }
        assert_eq!(RescanCause::parse("nope"), None);
    }

    #[test]
    fn cookie_detection() {
        let e = FsEvent::upsert(DirKey::ROOT, b".findex-cookie-1".to_vec(), false);
        assert!(e.is_cookie_event());
        let e = FsEvent::upsert(DirKey::ROOT, b"main.rs".to_vec(), false);
        assert!(!e.is_cookie_event());
    }
}
