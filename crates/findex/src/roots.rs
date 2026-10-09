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
        Root {
            id,
            path,
            dev: 0,
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
}
