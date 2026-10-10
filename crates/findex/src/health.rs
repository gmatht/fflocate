//! Health counters and telemetry (§7, §15).
//!
//! "Loudly stale beats silently wrong." Everything the daemon knows about its
//! own health lives here, and `fx stats` renders it verbatim.

use crate::event::RescanCause;

/// Change log: a bounded ring of `Cursor`s at which the index changed. Used by
/// `since` and `subscribe` (§11).
#[derive(Debug)]
pub struct ChangeLog {
    ring: std::collections::VecDeque<crate::cursor::Cursor>,
    cap: usize,
}

impl ChangeLog {
    pub fn new(cap: usize) -> Self {
        ChangeLog { ring: std::collections::VecDeque::with_capacity(cap), cap: cap.max(1) }
    }
    pub fn push(&mut self, c: crate::cursor::Cursor) {
        if self.ring.len() == self.cap {
            self.ring.pop_front();
        }
        self.ring.push_back(c);
    }
    /// The earliest cursor still retained. If the caller's cursor predates this,
    /// they must resync (`fresh = true`).
    pub fn earliest(&self) -> Option<crate::cursor::Cursor> {
        self.ring.front().copied()
    }
    /// All changes newer than `c` (same generation).
    pub fn since(&self, c: crate::cursor::Cursor) -> Vec<crate::cursor::Cursor> {
        self.ring
            .iter()
            .filter(|x| x.gen == c.gen && x.lsn > c.lsn)
            .copied()
            .collect()
    }
    pub fn len(&self) -> usize {
        self.ring.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

/// Counters surfaced by `fx stats` and `fx stats --roots`.
#[derive(Clone, Debug, Default)]
pub struct Health {
    pub recrawl_count: u64,
    pub last_rescan_cause: Option<RescanCause>,
    pub last_rescan_at_ms: u64,
    pub rescan_count: u64,
    pub rescans_deferred: u64,
    pub generation_bumps: u64,
    pub journal_write_errors: u64,
    pub journal_corrupt: bool,
    pub read_dir_errors: u64,
    /// Watches the kernel refused to install (e.g. watch-limit exhaustion, §15).
    pub watch_failures: u64,
    pub watch_limit_hits: u64,
    /// Mount changes detected by the watchdog (§16 mount churn).
    pub mount_events: u64,
    pub dropped_events: u64,
    pub escalated_events: u64,
    pub last_compaction_ms: u64,
    pub catching_up: bool,
    /// Watcher queue depth, mirrored here for `stats`.
    pub queue_depth: u64,
    /// Number of roots currently in Degraded state.
    pub degraded_roots: u64,
}

impl Health {
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "recrawl_count": self.recrawl_count,
            "last_rescan_cause": self.last_rescan_cause.map(|c| c.as_str()),
            "last_rescan_at_ms": self.last_rescan_at_ms,
            "rescan_count": self.rescan_count,
            "rescans_deferred": self.rescans_deferred,
            "generation_bumps": self.generation_bumps,
            "journal_write_errors": self.journal_write_errors,
            "journal_corrupt": self.journal_corrupt,
            "read_dir_errors": self.read_dir_errors,
            "watch_failures": self.watch_failures,
            "watch_limit_hits": self.watch_limit_hits,
            "mount_events": self.mount_events,
            "dropped_events": self.dropped_events,
            "escalated_events": self.escalated_events,
            "last_compaction_ms": self.last_compaction_ms,
            "catching_up": self.catching_up,
            "queue_depth": self.queue_depth,
            "degraded_roots": self.degraded_roots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::Cursor;

    #[test]
    fn change_log_since() {
        let mut cl = ChangeLog::new(4);
        for lsn in 1..=6 {
            cl.push(Cursor::new(1, lsn));
        }
        assert_eq!(cl.len(), 4);
        // entries 3..=6 retained
        let got = cl.since(Cursor::new(1, 2));
        assert_eq!(got.iter().map(|c| c.lsn).collect::<Vec<_>>(), vec![3, 4, 5, 6]);
        let got = cl.since(Cursor::new(1, 5));
        assert_eq!(got.iter().map(|c| c.lsn).collect::<Vec<_>>(), vec![6]);
    }

    #[test]
    fn change_log_generation_mismatch_yields_nothing() {
        let mut cl = ChangeLog::new(4);
        cl.push(Cursor::new(2, 10));
        assert!(cl.since(Cursor::new(1, 1)).is_empty());
    }
}
