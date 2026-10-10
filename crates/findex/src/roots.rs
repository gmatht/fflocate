//! Roots table and per-mount capabilities (§3, §4, §15).
//!
//! A root is a directory tree that findex indexes. Each root records the
//! probed mount capabilities so backends and the applier can make honest
//! decisions (cookie sync only on writable mounts, ignore-at-watch-time only
//! where the backend supports it, per-root case folding, …).

use crate::cursor::Cursor;
use crate::event::RootId;
use std::path::PathBuf;

/// Capabilities probed at root-add time and on mount events (§4).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MountCaps {
    /// `name_to_handle_at` works on the root dir → fanotify-FID backend usable.
    pub file_handles: bool,
    /// `getdents64` returned `DT_UNKNOWN` on the first probe dir → crawls need
    /// an `fstatat` fallback.
    pub dtype_unreliable: bool,
    /// Case-insensitive by fstype table or temp-file-pair probe.
    pub case_insensitive: bool,
    /// The root dir is writable by us → cookies (§8) are possible.
    pub writable: bool,
    /// fstype string as reported by mountinfo (e.g. "ext4", "overlay").
    pub fstype: String,
    /// Whether the backend can apply exclusions at watch time. When false the
    /// applier must filter excluded paths instead of the kernel doing it.
    pub watch_time_ignore: bool,
}

/// Operational state of a root (§7 degraded mode).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootState {
    Ok,
    /// Queries keep answering from what is known; every response meta lists
    /// stale roots. Loudly stale beats silently wrong.
    Degraded { reason: String },
    Rescanning,
}

impl RootState {
    pub fn is_ok(&self) -> bool {
        matches!(self, RootState::Ok)
    }
    pub fn label(&self) -> String {
        match self {
            RootState::Ok => "ok".to_string(),
            RootState::Degraded { reason } => format!("degraded({reason})"),
            RootState::Rescanning => "rescanning".to_string(),
        }
    }
}

/// Rescan rate-limiting / backoff bookkeeping for one root (§7).
#[derive(Clone, Debug)]
pub struct Backoff {
    /// Minimum interval between rescans; doubles on each rescan, capped.
    pub min_interval_ms: u64,
    pub cap_ms: u64,
    pub current_ms: u64,
    /// When the last rescan started (ms since UNIX epoch), 0 if never.
    pub last_rescan_ms: u64,
    /// Consecutive rescans that failed to clear the cause.
    pub consecutive: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            min_interval_ms: 1_000,
            cap_ms: 300_000, // 5 min
            current_ms: 1_000,
            last_rescan_ms: 0,
            consecutive: 0,
        }
    }
}

impl Backoff {
    /// Would we be allowed to start a rescan at `now_ms`?
    pub fn permits(&self, now_ms: u64) -> bool {
        self.last_rescan_ms == 0 || now_ms.saturating_sub(self.last_rescan_ms) >= self.current_ms
    }

    pub fn record(&mut self, now_ms: u64) {
        self.last_rescan_ms = now_ms;
        self.consecutive = self.consecutive.saturating_add(1);
        self.current_ms = (self.current_ms.saturating_mul(2)).min(self.cap_ms);
    }

    /// A rescan that fully succeeded clears the penalty (§7: backoff must
    /// relax, not ratchet forever).
    pub fn reset(&mut self) {
        self.current_ms = self.min_interval_ms;
        self.consecutive = 0;
    }

    /// The earliest instant a fresh rescan may start for this root.
    pub fn next_allowed_ms(&self) -> u64 {
        if self.last_rescan_ms == 0 {
            0
        } else {
            self.last_rescan_ms.saturating_add(self.current_ms)
        }
    }
}

/// One indexed root.
#[derive(Clone, Debug)]
pub struct Root {
    pub id: RootId,
    pub path: PathBuf,
    /// Device id of the mount the root lives on.
    pub dev: u64,
    /// Physical drive identity, re-probed every boot (§4, `drive`). Memory
    /// only — never in the snapshot — so adding it needed no format change.
    /// `None` means unprobed or unidentifiable.
    pub drive: Option<crate::drive::DriveId>,
    /// Which OS last recorded this root (`std::env::consts::OS`). `None` is a
    /// legacy root (predates recording): always treated as local. A root
    /// recorded by another OS is foreign: visible in stats, excluded from
    /// queries/syncs/rescans here, and never deleted by this OS (§15).
    pub owner_os: Option<String>,
    /// Per-location watch toggle (FSearch lesson, §4).
    pub enabled: bool,
    pub caps: MountCaps,
    pub state: RootState,
    pub backoff: Backoff,
    /// Cursor of the last completed full crawl of this root.
    pub last_full_crawl: Cursor,
    /// Watchman-style health counters (§7, §15).
    pub recrawl_count: u64,
    pub last_rescan_cause: Option<crate::event::RescanCause>,
    pub last_rescan_at_ms: u64,
    pub pending_items: u64,
}

impl Root {
    pub fn new(id: RootId, path: PathBuf) -> Self {
        // Canonical `/` separators, so `C:\tree` keys and matches exactly
        // like the `/`-joined index paths (§3). No-op off Windows.
        let path = crate::util::normalize_separators(&path);
        Root {
            id,
            path,
            dev: 0,
            drive: None,
            owner_os: None,
            enabled: true,
            caps: MountCaps::default(),
            state: RootState::Ok,
            backoff: Backoff::default(),
            last_full_crawl: Cursor::ZERO,
            recrawl_count: 0,
            last_rescan_cause: None,
            last_rescan_at_ms: 0,
            pending_items: 0,
        }
    }

    /// Stable, human-friendly root key for the sync response and stats.
    pub fn key(&self) -> String {
        self.path.display().to_string()
    }

    /// Whether this root belongs to the calling OS: legacy (unrecorded)
    /// roots are always local; recorded roots are local only to their owner.
    /// Foreign roots stay visible in stats but are excluded from queries,
    /// syncs and rescans here — and never deleted by this OS (§15).
    pub fn is_local_for(&self, os: &str) -> bool {
        match &self.owner_os {
            None => true,
            Some(o) => o == os,
        }
    }

    /// Whether this root belongs to this OS's daemon.
    pub fn is_local(&self) -> bool {
        self.is_local_for(std::env::consts::OS)
    }
}

/// The index-entry name for a root path: its final component, except a
/// Windows drive root (`D:/`, whose `file_name` is `None`) takes its drive
/// letter so it gets a stable entry instead of falling back to `ROOT_ID`
/// (which queries skip — drive roots were silently unqueryable). `/` keeps
/// the historical `None` (indexing `/` itself remains unsupported).
pub fn root_entry_name(path: &std::path::Path) -> Option<Vec<u8>> {
    if let Some(base) = path.file_name() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            return Some(base.as_bytes().to_vec());
        }
        #[cfg(not(unix))]
        {
            return Some(base.to_string_lossy().as_bytes().to_vec());
        }
    }
    drive_root_name(&path.to_string_lossy()).map(|s| s.as_bytes().to_vec())
}

/// `D:/` (or `D:`) → `Some("D:")`; anything else without a final component
/// (`/`, `""`) → `None`. Pure and platform-independent so it is unit-tested
/// everywhere even though only Windows reaches it.
fn drive_root_name(s: &str) -> Option<&str> {
    let t = s.trim_end_matches(['/', '\\']);
    if t.len() == 2 {
        let mut c = t.chars();
        match (c.next(), c.next()) {
            (Some(d), Some(':')) if d.is_ascii_alphabetic() => return Some(t),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let mut b = Backoff::default();
        assert!(b.permits(0));
        b.record(1_000); // the *next* window doubles to 2000 ms
        assert!(!b.permits(1_500));
        assert!(!b.permits(2_999));
        assert!(b.permits(3_000));
        for i in 0..20 {
            b.record(10_000 + i * 1_000_000);
        }
        assert_eq!(b.current_ms, b.cap_ms);
        b.reset();
        assert_eq!(b.current_ms, b.min_interval_ms);
        assert_eq!(b.consecutive, 0);
    }

    #[test]
    fn state_labels() {
        assert_eq!(RootState::Ok.label(), "ok");
        assert_eq!(
            RootState::Degraded { reason: "overflow".into() }.label(),
            "degraded(overflow)"
        );
    }

    #[test]
    fn local_means_owner_or_legacy() {
        let mut r = Root::new(0, PathBuf::from("/mnt/d"));
        assert!(r.is_local(), "unrecorded roots are always local");
        assert!(r.is_local_for("windows"), "legacy rule ignores the OS");
        r.owner_os = Some("linux".into());
        assert!(r.is_local_for("linux"));
        assert!(!r.is_local_for("windows"), "foreign roots are not local");
    }

    #[test]
    fn root_entry_names() {
        assert_eq!(
            root_entry_name(std::path::Path::new("/mnt/data")),
            Some(b"data".to_vec())
        );
        assert_eq!(root_entry_name(std::path::Path::new("/")), None, "indexing / stays unsupported");
        // Drive-letter roots get a stable entry instead of ROOT_ID. On Linux
        // `D:/` already has a `file_name`; on Windows only the fallback
        // produces it — either way the name is `D:`.
        assert_eq!(root_entry_name(std::path::Path::new("D:/")), Some(b"D:".to_vec()));
        assert_eq!(root_entry_name(std::path::Path::new("D:")), Some(b"D:".to_vec()));
    }
}
