//! Health counters and telemetry (§7, §15).
//!
//! "Loudly stale beats silently wrong." Everything the daemon knows about its
//! own health lives here, and `ff stats` renders it verbatim.

use crate::event::RescanCause;

/// What the daemon is doing right now (§15, "say what you are doing").
///
/// Before this existed a client could only tell *that* the daemon was behind
/// (`Meta.catching_up` + `Meta.lag_ms`) — never *why*. A cold daemon replaying
/// its journal, one crawling a freshly-configured root, and one re-verifying a
/// subtree after a watch drop all looked identical from outside, so a GUI
/// could only invent a cause ("starting fflocated…") and get it wrong. The
/// phase is the daemon's own answer, derived from what is actually queued and
/// running, and shipped on every query's `Meta` and in `stats`.
///
/// Ordered least-to-most settled, which is also the priority order when more
/// than one is true: replay gates everything (nothing else is applied while
/// the journal tail drains — see `Applier::drain`), a crawl is the next most
/// fundamental statement about the index, and a rescan is local upkeep on an
/// otherwise-served index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    /// The startup journal tail is still being replayed. The snapshot is
    /// served, but history is not fully applied: nothing else runs until it
    /// drains, so this is the honest first phase after a restart.
    Replaying,
    /// An initial (or newly-configured-root) crawl is in flight: directories
    /// are still being enumerated. The index is a growing partial view.
    Crawling,
    /// No crawl or replay outstanding, but rescan/diff work is queued: the
    /// index is complete-ish and being re-verified (watch drop, mount churn,
    /// manual `rescan`).
    Rescanning,
    /// Nothing queued, nothing replaying: the index is as current as the
    /// daemon can make it. This is the steady state.
    #[default]
    Idle,
}

impl Phase {
    /// The wire/label spelling: lowercase, stable, and the same string the
    /// protocol, `ff stats`, and the GUI all use.
    pub fn as_str(&self) -> &'static str {        match self {
            Phase::Replaying => "replaying",
            Phase::Crawling => "crawling",
            Phase::Rescanning => "rescanning",
            Phase::Idle => "idle",
        }
    }

    /// A short human sentence for a status line. Kept here, next to the enum,
    /// so every client (CLI and GUI) words the same phase the same way.
    pub fn label(&self) -> &'static str {
        match self {
            Phase::Replaying => "replaying journal",
            Phase::Crawling => "crawling",
            Phase::Rescanning => "re-verifying",
            Phase::Idle => "idle",
        }
    }

    /// Parse the wire spelling back. Unknown/absent strings are `None` (an old
    /// daemon that never sends a phase must not be invented as idle).
    pub fn parse(s: &str) -> Option<Phase> {
        match s {
            "replaying" => Some(Phase::Replaying),
            "crawling" => Some(Phase::Crawling),
            "rescanning" => Some(Phase::Rescanning),
            "idle" => Some(Phase::Idle),
            _ => None,
        }
    }

    /// Whether this phase means the index is still moving (anything but
    /// `Idle`). The GUI uses it the way it used to use `catching_up`, but now
    /// with a cause attached.
    pub fn is_busy(&self) -> bool {
        !matches!(self, Phase::Idle)
    }
}

/// Classify the daemon's phase from what is actually outstanding.
///
/// Pure, so the rules are unit-testable without a daemon — and so the applier
/// tick has exactly one place to get this right. Priority mirrors the enum
/// order and the real dependency chain:
///
/// * **replay gates everything.** While the startup journal tail remains,
///   `Applier::drain` applies nothing else (see its ordering note), so a
///   nonzero `replay` is `Replaying` regardless of what else is queued.
/// * **a crawl beats a rescan.** Directories still being enumerated mean the
///   index is an incomplete view; re-verifying a complete view is the lesser
///   claim, so `crawl_dirs > 0` wins.
/// * otherwise `rescans > 0` is `Rescanning`, and nothing outstanding is
///   `Idle`.
pub fn classify_phase(replay: usize, crawl_dirs: usize, rescans: usize) -> Phase {
    if replay > 0 {
        Phase::Replaying
    } else if crawl_dirs > 0 {
        Phase::Crawling
    } else if rescans > 0 {
        Phase::Rescanning
    } else {
        Phase::Idle
    }
}


/// What a recorded change did to the index. Deliberately two outcomes: a live
/// rename arrives as a delete plus a create, which is exactly what a watcher
/// wants to hear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Create,
    Delete,
}

impl ChangeKind {
    /// The `Response::Event.action` spelling.
    pub fn action(self) -> &'static str {
        match self {
            ChangeKind::Create => "create",
            ChangeKind::Delete => "delete",
        }
    }
}

/// One recorded index change: the cursor it landed at, what it did, and enough
/// of the entry to decide relevance without re-reading the index (the entry may
/// already be gone by the time a subscriber asks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Monotonic push index, stamped by [`ChangeLog::push`]. A subscriber tracks
    /// the last one it consumed, so whether a change it had not read has been
    /// evicted is answered exactly, without guessing from cursors.
    pub seq: u64,
    pub cursor: crate::cursor::Cursor,
    pub kind: ChangeKind,
    /// Absolute path. Empty when the applier could not resolve one.
    pub path: Vec<u8>,
    pub is_dir: bool,
    pub is_exec: bool,
}

/// Change log: a bounded ring of changes at which the index changed. Used by
/// `since` (coarse cursors) and `subscribe` (§11).
#[derive(Debug)]
pub struct ChangeLog {
    ring: std::collections::VecDeque<Change>,
    cap: usize,
    /// Changes ever recorded. With the ring's length this gives both the index
    /// of the oldest retained change and the count of evicted ones.
    pushed: u64,
}

impl ChangeLog {
    pub fn new(cap: usize) -> Self {
        ChangeLog {
            ring: std::collections::VecDeque::with_capacity(cap),
            cap: cap.max(1),
            pushed: 0,
        }
    }
    pub fn push(&mut self, mut c: Change) {
        self.pushed += 1;
        c.seq = self.pushed;
        if self.ring.len() == self.cap {
            self.ring.pop_front();
        }
        self.ring.push_back(c);
    }
    /// How many changes have ever been recorded.
    pub fn pushed(&self) -> u64 {
        self.pushed
    }
    /// How many recorded changes have been evicted.
    pub fn dropped(&self) -> u64 {
        self.pushed - self.ring.len() as u64
    }
    /// The earliest change still retained.
    pub fn earliest(&self) -> Option<crate::cursor::Cursor> {
        self.ring.front().map(|c| c.cursor)
    }
    /// Whether a subscriber at `cursor` that has accounted for `consumed`
    /// changes can no longer be fed: a change it had not read was evicted, or a
    /// generation bump remapped the ids under it.
    ///
    /// `consumed` starts at [`pushed`](Self::pushed), which is why a fresh
    /// subscriber never gaps: its cursor may well precede the retained window
    /// (nothing was recorded before it arrived), but nothing it needed is gone.
    pub fn gap(&self, cursor: crate::cursor::Cursor, consumed: u64, generation: u64) -> bool {
        generation != cursor.gen || self.dropped() > consumed
    }
    /// All changes newer than `c` (same generation).
    ///
    /// Scanned from the back: a caught-up subscriber — the common case, polled
    /// twenty times a second — stops at its own last change and never touches
    /// the rest of the ring.
    pub fn since(&self, c: crate::cursor::Cursor) -> Vec<Change> {
        let mut out: Vec<Change> = self
            .ring
            .iter()
            .rev()
            .take_while(|x| x.cursor.gen != c.gen || x.cursor.lsn > c.lsn)
            .filter(|x| x.cursor.gen == c.gen)
            .cloned()
            .collect();
        out.reverse();
        out
    }
    pub fn len(&self) -> usize {
        self.ring.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

/// Counters surfaced by `ff stats` and `ff stats --roots`.
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
    /// Queued subtree rescans folded into a shared parent by ancestry
    /// collapse (§7): the set stays bounded under storm load while coverage
    /// never shrinks.
    pub rescans_collapsed: u64,
    pub dropped_events: u64,
    pub escalated_events: u64,
    pub last_compaction_ms: u64,
    pub catching_up: bool,
    /// Watcher queue depth, mirrored here for `stats`.
    pub queue_depth: u64,
    /// Arrival time (wall ms since epoch) of the oldest still-unapplied
    /// change, mirrored from the pending queue on the applier tick. `0` means
    /// "nothing queued" — the daemon is caught up. `Meta.lag_ms` renders it as
    /// an age; `ff stats` shows it raw.
    pub oldest_queued_at_ms: u64,
    /// Number of roots currently in Degraded state.
    pub degraded_roots: u64,
    /// Times the accept loop was respawned after dying (§15 deaf-daemon
    /// watchdog). Zero is healthy; >0 means the listener died but a
    /// surviving thread re-bound and revived it.
    pub accept_restarts: u64,
    /// What the daemon is doing right now (see [`Phase`]). Mirrored from the
    /// queues on each applier tick so `Meta`/`stats` can answer "why is it
    /// slow" with a cause, not just a lag.
    pub phase: Phase,
    /// Entries discovered so far, as a crawl-progress numerator. This is the
    /// live index count, which for the *initial* crawl is the honest "how much
    /// of the tree have I seen" figure — there is no up-front known total
    /// (that is the point of a streaming crawl), so this counts up rather than
    /// reporting a fraction. Zero/meaningless when no crawl is running; clients
    /// only show it while `phase == Crawling`.
    pub crawl_entries: u64,
    /// Directories still queued to be enumerated. Zero means the crawl has
    /// drained its queue (which, together with the applier reporting no work
    /// in flight, is what moves the phase off `Crawling`).
    pub crawl_dirs_pending: u64,
    /// Rescan/diff work items still queued (local re-verification). Nonzero
    /// with no crawl ⇒ `Rescanning`.
    pub rescans_pending: u64,
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
            "rescans_collapsed": self.rescans_collapsed,
            "dropped_events": self.dropped_events,
            "escalated_events": self.escalated_events,
            "last_compaction_ms": self.last_compaction_ms,
            "catching_up": self.catching_up,
            "queue_depth": self.queue_depth,
            "oldest_queued_at_ms": self.oldest_queued_at_ms,
            "degraded_roots": self.degraded_roots,
            "accept_restarts": self.accept_restarts,
            "phase": self.phase.as_str(),
            "crawl_entries": self.crawl_entries,
            "crawl_dirs_pending": self.crawl_dirs_pending,
            "rescans_pending": self.rescans_pending,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::Cursor;

    fn change(lsn: u64, kind: ChangeKind, path: &str) -> Change {
        Change {
            seq: 0,
            cursor: Cursor::new(1, lsn),
            kind,
            path: path.as_bytes().to_vec(),
            is_dir: false,
            is_exec: false,
        }
    }

    #[test]
    fn change_log_since() {
        let mut cl = ChangeLog::new(4);
        for lsn in 1..=6 {
            cl.push(change(lsn, ChangeKind::Create, &format!("/t/{lsn}")));
        }
        assert_eq!(cl.len(), 4);
        // entries 3..=6 retained
        let got = cl.since(Cursor::new(1, 2));
        assert_eq!(
            got.iter().map(|c| c.cursor.lsn).collect::<Vec<_>>(),
            vec![3, 4, 5, 6]
        );
        let got = cl.since(Cursor::new(1, 5));
        assert_eq!(got.iter().map(|c| c.cursor.lsn).collect::<Vec<_>>(), vec![6]);
        assert_eq!(got[0].kind.action(), "create");
        assert_eq!(got[0].path, b"/t/6");
    }

    #[test]
    fn change_log_generation_mismatch_yields_nothing() {
        let mut cl = ChangeLog::new(4);
        cl.push(Change {
            seq: 0,
            cursor: Cursor::new(2, 10),
            kind: ChangeKind::Delete,
            path: b"/t/gone".to_vec(),
            is_dir: false,
            is_exec: false,
        });
        assert!(cl.since(Cursor::new(1, 1)).is_empty());
        assert_eq!(cl.earliest(), Some(Cursor::new(2, 10)));
    }

    #[test]
    fn change_log_overrun_is_visible_from_the_earliest_cursor() {
        // A subscriber sitting at lsn 1 must be able to tell that the ring no
        // longer reaches back that far: that is the resync signal.
        let mut cl = ChangeLog::new(2);
        for lsn in 1..=4 {
            cl.push(change(lsn, ChangeKind::Create, "/t/x"));
        }
        let cursor = Cursor::new(1, 1);
        let earliest = cl.earliest().expect("retained changes");
        assert!(
            earliest.lsn > cursor.lsn,
            "a cursor older than the ring must be detectable"
        );
        assert_eq!(cl.since(cursor).len(), 2);
        // What makes the gap exact: four changes were pushed, two survive, so
        // two were evicted — and a subscriber that had consumed up to three of
        // them is not asked to resync.
        assert_eq!(cl.pushed(), 4);
        assert_eq!(cl.dropped(), 2);
        assert!(cl.dropped() > 0, "the resync signal");
        assert!(
            cl.pushed() >= cl.dropped(),
            "a fresh subscriber is never behind the evictions"
        );
    }

    #[test]
    fn a_gap_is_an_unread_eviction_not_a_merely_older_cursor() {
        let mut cl = ChangeLog::new(2);
        for lsn in 1..=4 {
            cl.push(change(lsn, ChangeKind::Create, "/t/x"));
        }
        // A subscriber that starts now has accounted for every change: its
        // cursor predates the retained window, and that is not a gap.
        assert!(!cl.gap(Cursor::new(1, 4), cl.pushed(), 1));
        // One that stopped at index 1 missed two changes that were evicted.
        assert!(cl.gap(Cursor::new(1, 4), 1, 1));
        // A generation bump remaps ids, whatever the indices say.
        assert!(cl.gap(Cursor::new(1, 4), cl.pushed(), 2));
    }

    #[test]
    fn classify_phase_prioritises_replay_then_crawl_then_rescan() {
        // Replay gates everything: even with a crawl and rescans queued, the
        // phase is Replaying.
        assert_eq!(classify_phase(3, 10, 10), Phase::Replaying);
        // No replay: a crawl outranks a rescan.
        assert_eq!(classify_phase(0, 5, 7), Phase::Crawling);
        // No replay or crawl: rescans mean re-verification.
        assert_eq!(classify_phase(0, 0, 2), Phase::Rescanning);
        // Nothing outstanding: idle.
        assert_eq!(classify_phase(0, 0, 0), Phase::Idle);
    }

    #[test]
    fn phase_wire_spelling_roundtrips_and_parses_unknown_as_none() {
        for p in [Phase::Replaying, Phase::Crawling, Phase::Rescanning, Phase::Idle] {
            assert_eq!(Phase::parse(p.as_str()), Some(p), "{p:?}");
        }
        // An old daemon's absent/garbled phase must not be invented as idle.
        assert_eq!(Phase::parse(""), None);
        assert_eq!(Phase::parse("starting"), None);
        assert_eq!(Phase::parse("IDLE"), None);
        // busy-ness: everything but idle.
        assert!(Phase::Replaying.is_busy());
        assert!(Phase::Crawling.is_busy());
        assert!(Phase::Rescanning.is_busy());
        assert!(!Phase::Idle.is_busy());
    }

    #[test]
    fn health_json_carries_phase_and_crawl_progress() {
        let mut h = Health::default();
        h.phase = Phase::Crawling;
        h.crawl_entries = 1_200_000;
        h.crawl_dirs_pending = 41;
        h.rescans_pending = 2;
        let j = h.as_json();
        assert_eq!(j["phase"], "crawling");
        assert_eq!(j["crawl_entries"], 1_200_000);
        assert_eq!(j["crawl_dirs_pending"], 41);
        assert_eq!(j["rescans_pending"], 2);
        // The default is idle, and it renders.
        assert_eq!(Health::default().as_json()["phase"], "idle");
    }
}
