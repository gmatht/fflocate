//! The pending collection (§7) — the single ordering domain.
//!
//! Everything that mutates the index flows through here: sync cookies,
//! filesystem events, and crawl/rescan work items. One ordered structure owned
//! by the applier is what makes the sync guarantee (§8) provable: a cookie
//! enqueued after a batch of events is necessarily applied after them.
//!
//! ```text
//! pending collection  (cookie > events > events > work)
//! ```

use std::collections::VecDeque;

use crate::event::{FsEvent, RescanCause};
use crate::index::ROOT_ID;

/// Max queued subtree rescans before ancestry collapse kicks in (§7).
///
/// Past this many, newly enqueued (non-manual) rescans first fold the largest
/// group of queued siblings into their shared parent — one parent diff covers
/// the whole group, so a storm of dirty dirs becomes O(budget) work items
/// instead of O(dirs). Manual rescans are exempt: the user pointed at exactly
/// that dir.
pub const RESCAN_BUDGET: usize = 256;

/// Parent-chain oracle for ancestry collapse: dir id → parent dir id.
///
/// `None` (model tests without an index) disables collapsing; coalescing is
/// unaffected. Returning `None` or `ROOT_ID` means "cannot climb further".
pub type ParentOf = std::sync::Arc<dyn Fn(u32) -> Option<u32> + Send + Sync>;

/// A cursor at which a sync cookie was created, with a reply channel.
#[derive(Debug)]
pub struct Cookie {
    pub token: u64,
    /// The root the cookie belongs to.
    pub root: u32,
    /// The cookie file name observed by the watcher.
    pub name: Vec<u8>,
    /// Whether the cookie has been *seen* by the watcher stream.
    pub seen: bool,
}

/// Work items produced by the crawler/rescanner (as opposed to watcher events).
///
/// `RescanSubtree` items are coalesced per directory: re-enqueueing a dir that
/// is already queued is a no-op (the rescan will re-verify the whole subtree
/// anyway).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkItem {
    /// Enumerate the directory `dir` and add any children not already known.
    CrawlDir { dir: u32 },
    /// Re-enumerate the subtree rooted at `dir` and diff against the index.
    Rescan { dir: u32, cause: RescanCause },
    /// Drop everything under `dir` (mount gone / root removed).
    DropSubtree { dir: u32 },
}

/// A queued item stamped with its arrival time (wall ms since epoch).
///
/// The stamp answers "how far behind the filesystem is the daemon": the age
/// of the oldest unapplied change is the lag. Stamped at enqueue, read at
/// pop/peek — never mutated in between, so collapse/coalesce (which drops
/// or folds items) can only make the surviving stamp newer, never older.
/// A folded parent carries the *oldest* member's stamp (see `collapse_one`),
/// so collapse never hides lag either.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamped<T> {
    at_ms: u64,
    item: T,
}

impl<T> Stamped<T> {
    fn now(item: T) -> Self {
        Stamped {
            at_ms: crate::applier::now_ms(),
            item,
        }
    }
}

/// The ordered pending collection.
#[derive(Default)]
pub struct Pending {
    events: VecDeque<Stamped<FsEvent>>,
    work: VecDeque<Stamped<WorkItem>>,
    cookies: VecDeque<Cookie>,
    /// Dirs with a Rescan already queued, for coalescing.
    rescan_queued: std::collections::HashSet<u32>,
    /// Dirs with a CrawlDir already queued.
    crawl_queued: std::collections::HashSet<u32>,
    /// Capacity of the event queue; a full queue escalates to a rescan rather
    /// than dropping events (§6).
    pub capacity: usize,
    /// Parent-chain oracle for ancestry collapse (see [`ParentOf`]). `None`
    /// (queue-only use, e.g. unit tests without an index) means no safe fold
    /// exists, so an over-budget set simply grows. Set it when an index is
    /// available for parent lookups.
    parent_of: Option<ParentOf>,
    /// How many queued rescans were folded into a parent by collapse (§15).
    pub rescans_collapsed: u64,
}

// NOTE (unblocking fix, not the owner's change): `ParentOf` is a closure and
// cannot derive `Debug`. Manual impl prints presence, not the closure —
// replace with whatever the rescan-budget work wants long-term.
impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending")
            .field("events", &self.events)
            .field("work", &self.work)
            .field("cookies", &self.cookies)
            .field("rescan_queued", &self.rescan_queued)
            .field("crawl_queued", &self.crawl_queued)
            .field("capacity", &self.capacity)
            .field("parent_of", &self.parent_of.as_ref().map(|_| ".."))
            .field("rescans_collapsed", &self.rescans_collapsed)
            .finish()
    }
}

/// What happened when an event was pushed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    Accepted,
    /// The queue was full; the event was converted into a `RescanSubtree`
    /// escalation. Never a silent drop.
    Escalated,
    /// The event was a sync-cookie event and was recorded as a marker.
    CookieMarker,
}

impl Pending {
    pub fn new(capacity: usize) -> Pending {
        Pending {
            capacity,
            ..Default::default()
        }
    }

    /// Push a filesystem event.
    ///
    /// Cookie events are special-cased *before* the capacity check so a sync
    /// can never be starved by a flood of ordinary events (§8).
    pub fn push_event(&mut self, ev: FsEvent) -> PushOutcome {
        if let FsEvent::Upsert { parent, name, .. } = &ev {
            if crate::util::is_cookie(name) {
                // A cookie create event: record the marker (matched by parent
                // dir id + name) but *never* enqueue it as an indexable event.
                for c in self.cookies.iter_mut() {
                    if &c.name == name && c.root == parent.0 {
                        c.seen = true;
                        return PushOutcome::CookieMarker;
                    }
                }
                return PushOutcome::CookieMarker;
            }
        }
        if let FsEvent::Remove { name, .. } = &ev {
            if crate::util::is_cookie(name) {
                return PushOutcome::CookieMarker;
            }
        }
        if let FsEvent::RescanSubtree { dir, cause } = ev {
            self.enqueue_rescan(dir.0, cause);
            return PushOutcome::Accepted;
        }
        if self.events.len() >= self.capacity {
            // Full queue → escalate to a rescan of the event's dir, so no
            // mutation is silently lost.
            if let Some(dir) = ev.target_dir() {
                self.enqueue_rescan(dir.0, RescanCause::Overflow);
            }
            return PushOutcome::Escalated;
        }
        self.events.push_back(Stamped::now(ev));
        PushOutcome::Accepted
    }

    /// Register a cookie the daemon is waiting on.
    pub fn register_cookie(&mut self, token: u64, root: u32, name: Vec<u8>) {
        self.cookies.push_back(Cookie {
            token,
            root,
            name,
            seen: false,
        });
    }

    /// Mark a cookie as seen (the watcher delivered its create event). The
    /// applier calls this; completion additionally requires the queues drained.
    pub fn mark_cookie_seen(&mut self, token: u64) -> bool {
        for c in self.cookies.iter_mut() {
            if c.token == token {
                c.seen = true;
                return true;
            }
        }
        false
    }

    /// Cookies whose create events have been seen and whose scope is drained.
    pub fn take_completed_cookies(&mut self) -> Vec<Cookie> {
        let drained = self.events.is_empty() && self.work.is_empty();
        if !drained {
            return Vec::new();
        }
        let mut ready = Vec::new();
        self.cookies.retain(|c| {
            if c.seen {
                ready.push(Cookie {
                    token: c.token,
                    root: c.root,
                    name: c.name.clone(),
                    seen: true,
                });
                false
            } else {
                true
            }
        });
        ready
    }

    pub fn enqueue_rescan(&mut self, dir: u32, cause: RescanCause) {
        // Already queued: a re-enqueue is a no-op (the rescan re-verifies the
        // whole subtree anyway).
        if self.rescan_queued.contains(&dir) {
            return;
        }
        // Over budget: fold one group of queued siblings into their shared
        // parent instead of growing the set. A parent diff covers every child,
        // so coverage never shrinks — only the item count does. Ancestry (not
        // path prefix) decides the grouping, via `parent_of`; without an
        // oracle no safe fold exists and the set grows past budget.
        if cause != RescanCause::Manual && self.rescan_queued.len() >= RESCAN_BUDGET {
            if self.collapse_one(dir) {
                // `dir`'s family collapsed (maybe onto `dir` itself): the
                // earlier `contains` check is stale, re-test.
                if self.rescan_queued.contains(&dir) {
                    return;
                }
            }
        }
        if self.rescan_queued.insert(dir) {
            self.work.push_back(Stamped::now(WorkItem::Rescan { dir, cause }));
        }
    }

    /// Install the parent-chain oracle that makes [`Self::enqueue_rescan`]
    /// collapse over-budget sibling groups into their shared parent (§7).
    pub fn set_parent_of(&mut self, f: ParentOf) {
        self.parent_of = Some(f);
    }

    /// How many queued rescans were folded into a parent by collapse.
    pub fn rescans_collapsed(&self) -> u64 {
        self.rescans_collapsed
    }

    /// Fold one group of queued dirs into their shared parent to make room.
    ///
    /// Counts queued dirs by parent (plus the incoming `dir` itself, which
    /// counts as a vote for its own parent), picks the parent with the most
    /// queued children in the same family, removes those children from the
    /// queue, and enqueues the parent instead. Returns true when a fold
    /// happened. Ancestor work items targeting a dir *above* the folded group
    /// already cover it, so a vote for an already-covered parent folds nothing
    /// and returns false — the incoming dir is simply already covered.
    ///
    /// Never climbs past a root dir (a root-dir parent is the ceiling: the
    /// fold lands on the root, which is a legitimate rescan target) and never
    /// past `ROOT_ID`/unknown ids. Cross-root merges are impossible by
    /// construction: every id climbs only its own parent chain.
    fn collapse_one(&mut self, dir: u32) -> bool {
        let parent_of = match &self.parent_of {
            Some(f) => f.clone(),
            None => return false,
        };
        // Tally votes: each queued dir (and the newcomer) votes for its parent.
        // Skip votes that cannot climb (unknown / ROOT_ID / root dirs are the
        // ceiling, not voters — a root dir has no parent worth folding to).
        let mut votes: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
        let consider = |id: u32, votes: &mut std::collections::HashMap<u32, Vec<u32>>| {
            let p = parent_of(id);
            match p {
                Some(p) if p != ROOT_ID && p != id => {
                    votes.entry(p).or_default().push(id);
                }
                _ => {}
            }
        };
        for &q in self.rescan_queued.iter() {
            consider(q, &mut votes);
        }
        consider(dir, &mut votes);
        // The parent with the most queued children wins; ties go to the
        // smallest id for determinism (test snapshots depend on it).
        let mut best: Option<(u32, Vec<u32>)> = None;
        for (p, members) in votes {
            // A parent already covered by an ancestor's queued rescan needs no
            // fold: the work is already scheduled above it.
            if self.covered_by_ancestor(p, &parent_of) {
                continue;
            }
            let better = match &best {
                None => true,
                Some((bp, bm)) => {
                    members.len() > bm.len() || (members.len() == bm.len() && p < *bp)
                }
            };
            if better {
                best = Some((p, members));
            }
        }
        let (parent, members) = match best {
            Some(b) => b,
            None => return false,
        };
        // Evict the folded children from both the set and the FIFO, then
        // enqueue the parent with the overflow cause (the group formed under
        // load; attributing it to one member's cause would lie in stats).
        // The parent inherits the oldest evicted stamp: the fold covers work
        // that arrived then, so the lag must not reset to now.
        let mut evicted = 0u64;
        let mut oldest_ms: Option<u64> = None;
        for m in &members {
            if self.rescan_queued.remove(m) {
                evicted += 1;
            }
        }
        self.work.retain(|w| {
            if matches!(&w.item, WorkItem::Rescan { dir: d, .. } if members.contains(d)) {
                oldest_ms = Some(oldest_ms.map_or(w.at_ms, |o: u64| o.min(w.at_ms)));
                false
            } else {
                true
            }
        });
        // The parent itself may already be queued (voted by another branch):
        // then the folded children are simply covered — no new item.
        if self.rescan_queued.insert(parent) {
            let at_ms = oldest_ms.unwrap_or_else(crate::applier::now_ms);
            self.work.push_back(Stamped {
                at_ms,
                item: WorkItem::Rescan {
                    dir: parent,
                    cause: RescanCause::Overflow,
                },
            });
        }
        self.rescans_collapsed += evicted;
        true
    }

    /// Is `dir` already covered by a queued rescan of one of its ancestors?
    /// Walks the parent chain up to a root dir; any queued ancestor covers.
    fn covered_by_ancestor(&self, dir: u32, parent_of: &ParentOf) -> bool {
        let mut cur = dir;
        // Depth-bounded: no real chain exceeds the index size, and a hostile
        // oracle must not spin us. 1024 comfortably exceeds real depths.
        for _ in 0..1024 {
            let p = match parent_of(cur) {
                Some(p) => p,
                None => return false,
            };
            if p == ROOT_ID || p == cur {
                return false;
            }
            if self.rescan_queued.contains(&p) {
                return true;
            }
            cur = p;
        }
        false
    }

    /// Forget which directories have been walked.
    ///
    /// A restart builds a fresh `Pending`, so the crawl queue is empty and every
    /// directory will be walked again — which is what re-reads ownership for the
    /// entries the snapshot restored. This method exists so that re-boot can be
    /// modelled in-process, and so the dedupe set cannot silently suppress a
    /// second pass.
    pub fn reset_for_restart(&mut self) {
        self.crawl_queued.clear();
    }

    pub fn enqueue_crawl(&mut self, dir: u32) {
        if self.crawl_queued.insert(dir) {
            self.work.push_back(Stamped::now(WorkItem::CrawlDir { dir }));
        }
    }

    pub fn enqueue_drop(&mut self, dir: u32) {
        self.work.push_back(Stamped::now(WorkItem::DropSubtree { dir }));
    }

    /// Pop the next event, if any (events before work, per the ordering).
    pub fn pop_event(&mut self) -> Option<FsEvent> {
        self.events.pop_front().map(|s| s.item)
    }

    /// Pop the next work item.
    pub fn pop_work(&mut self) -> Option<WorkItem> {
        let w = self.work.pop_front().map(|s| s.item);
        if let Some(w) = &w {
            match w {
                WorkItem::Rescan { dir, .. } => {
                    self.rescan_queued.remove(dir);
                }
                WorkItem::CrawlDir { dir } => {
                    self.crawl_queued.remove(dir);
                }
                WorkItem::DropSubtree { .. } => {}
            }
        }
        w
    }

    /// Arrival wall-ms of the oldest still-queued change (event or work),
    /// or `None` when both queues are empty.
    ///
    /// This is the daemon's lag behind the filesystem: everything older has
    /// been applied, everything still queued arrived at or after this. The
    /// server turns it into a saturating age for `Meta.lag_ms`; the GUI
    /// renders seconds from it.
    pub fn oldest_queued_ms(&self) -> Option<u64> {
        let e = self.events.front().map(|s| s.at_ms);
        let w = self.work.front().map(|s| s.at_ms);
        match (e, w) {
            (None, None) => None,
            (Some(a), None) | (None, Some(a)) => Some(a),
            (Some(a), Some(b)) => Some(a.min(b)),
        }
    }

    pub fn events_len(&self) -> usize {
        self.events.len()
    }
    pub fn work_len(&self) -> usize {
        self.work.len()
    }
    pub fn cookies_len(&self) -> usize {
        self.cookies.len()
    }
    /// Depth as surfaced in `ff stats` (§15).
    pub fn depth(&self) -> usize {
        self.events.len() + self.work.len()
    }
    pub fn is_drained(&self) -> bool {
        self.events.is_empty() && self.work.is_empty()
    }

    /// Total events pushed over the queue's lifetime — a cheap liveness signal.
    pub fn is_empty(&self) -> bool {
        self.is_drained() && self.cookies.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::DirKey;

    #[test]
    fn fifo_event_order() {
        let mut p = Pending::new(100);
        p.push_event(FsEvent::upsert(DirKey::new(1), b"a".to_vec(), false));
        p.push_event(FsEvent::upsert(DirKey::new(1), b"b".to_vec(), false));
        assert_eq!(
            p.pop_event(),
            Some(FsEvent::upsert(DirKey::new(1), b"a".to_vec(), false))
        );
        assert_eq!(
            p.pop_event(),
            Some(FsEvent::upsert(DirKey::new(1), b"b".to_vec(), false))
        );
        assert_eq!(p.pop_event(), None);
    }

    #[test]
    fn cookie_events_are_filtered_not_indexed() {
        let mut p = Pending::new(100);
        p.register_cookie(1, 0, b".fflocate-cookie-x".to_vec());
        let out = p.push_event(FsEvent::upsert(
            DirKey::ROOT,
            b".fflocate-cookie-x".to_vec(),
            false,
        ));
        assert_eq!(out, PushOutcome::CookieMarker);
        assert_eq!(
            p.events_len(),
            0,
            "cookie must never become an indexable event"
        );
    }

    #[test]
    fn overflow_escalates_to_rescan() {
        let mut p = Pending::new(2);
        p.push_event(FsEvent::upsert(DirKey::new(1), b"a".to_vec(), false));
        p.push_event(FsEvent::upsert(DirKey::new(1), b"b".to_vec(), false));
        let out = p.push_event(FsEvent::upsert(DirKey::new(1), b"c".to_vec(), false));
        assert_eq!(out, PushOutcome::Escalated);
        assert_eq!(p.events_len(), 2);
        assert_eq!(
            p.work_len(),
            1,
            "the dropped event becomes a rescan, not silence"
        );
        assert!(matches!(
            p.pop_work(),
            Some(WorkItem::Rescan {
                dir: 1,
                cause: RescanCause::Overflow
            })
        ));
    }

    #[test]
    fn rescans_coalesce_per_dir() {
        let mut p = Pending::new(10);
        p.enqueue_rescan(5, RescanCause::Overflow);
        p.enqueue_rescan(5, RescanCause::Manual);
        p.enqueue_rescan(6, RescanCause::Manual);
        assert_eq!(p.work_len(), 2);
        assert!(matches!(
            p.pop_work(),
            Some(WorkItem::Rescan {
                dir: 5,
                cause: RescanCause::Overflow
            })
        ));
        // After popping, the dir can be re-enqueued.
        p.enqueue_rescan(5, RescanCause::Manual);
        assert_eq!(p.work_len(), 2);
    }

    /// Test oracle: ids 1..N are siblings under parent 9000, ids 5000.. are
    /// siblings under parent 9500. Parents report self (the ceiling: the fold
    /// lands on them rather than climbing past). Anything else is unknown and
    /// cannot climb.
    fn sibling_oracle() -> ParentOf {
        std::sync::Arc::new(|id: u32| match id {
            1..=4000 => Some(9000),
            5000..=8999 => Some(9500),
            9000 | 9500 => Some(id),
            _ => None,
        })
    }

    fn queued_dirs(p: &mut Pending) -> Vec<u32> {
        let mut out = Vec::new();
        while let Some(w) = p.pop_work() {
            if let WorkItem::Rescan { dir, .. } = w {
                out.push(dir);
            }
        }
        out.sort_unstable();
        out
    }

    #[test]
    fn collapse_folds_siblings_into_parent() {
        let mut p = Pending::new(10);
        p.set_parent_of(sibling_oracle());
        // Fill to budget with siblings under 9000 (plus one under 9500).
        for id in 1..=(RESCAN_BUDGET as u32 - 1) {
            p.enqueue_rescan(id, RescanCause::Overflow);
        }
        p.enqueue_rescan(5000, RescanCause::Overflow);
        assert_eq!(p.rescan_queued.len(), RESCAN_BUDGET);
        // One *new* sibling under 9000: the 9000-family folds to its parent.
        p.enqueue_rescan(RESCAN_BUDGET as u32, RescanCause::Overflow);
        let queued = queued_dirs(&mut p);
        assert!(
            queued.contains(&9000),
            "siblings must fold to parent 9000: {queued:?}"
        );
        assert!(
            queued.contains(&5000),
            "the other family is untouched: {queued:?}"
        );
        assert!(queued.len() < RESCAN_BUDGET + 1, "set shrank: {queued:?}");
        assert!(p.rescans_collapsed() > 0);
    }

    #[test]
    fn collapse_never_merges_across_families() {
        let mut p = Pending::new(10);
        p.set_parent_of(sibling_oracle());
        // Alternate families so no group dominates: over budget, the winner
        // must be exactly one of the two parents — never a made-up LCA.
        for i in 0..RESCAN_BUDGET as u32 {
            let id = if i % 2 == 0 { 1 + i / 2 } else { 5000 + i / 2 };
            p.enqueue_rescan(id, RescanCause::Overflow);
        }
        assert_eq!(p.rescan_queued.len(), RESCAN_BUDGET);
        // One new dir in the first family: exactly one family folds.
        p.enqueue_rescan(1 + RESCAN_BUDGET as u32 / 2, RescanCause::Overflow);
        let queued = queued_dirs(&mut p);
        assert!(
            queued.contains(&9000) || queued.contains(&9500),
            "one family folds to its own parent: {queued:?}"
        );
        assert!(
            !(queued.contains(&9000) && queued.contains(&9500) && queued.len() > 2),
            "no cross-family parent may appear: {queued:?}"
        );
    }

    #[test]
    fn manual_rescans_are_exempt_from_collapse() {
        let mut p = Pending::new(10);
        p.set_parent_of(sibling_oracle());
        for id in 1..=(RESCAN_BUDGET as u32) {
            p.enqueue_rescan(id, RescanCause::Overflow);
        }
        let before = p.rescan_queued.len();
        // Manual always lands exactly where pointed, even over budget.
        p.enqueue_rescan(RESCAN_BUDGET as u32 + 1, RescanCause::Manual);
        assert!(
            p.rescan_queued.contains(&(RESCAN_BUDGET as u32 + 1)),
            "manual must not fold"
        );
        assert_eq!(p.rescan_queued.len(), before + 1);
        assert_eq!(p.rescans_collapsed(), 0);
    }

    #[test]
    fn no_oracle_means_no_collapse_but_no_loss() {
        let mut p = Pending::new(10);
        for id in 1..=(RESCAN_BUDGET as u32 + 5) {
            p.enqueue_rescan(id, RescanCause::Overflow);
        }
        // Everything still queued — coverage never shrinks without an oracle.
        assert_eq!(p.rescan_queued.len(), RESCAN_BUDGET + 5);
        assert_eq!(p.rescans_collapsed(), 0);
    }

    #[test]
    fn collapse_cause_is_overflow_not_member() {
        let mut p = Pending::new(10);
        p.set_parent_of(sibling_oracle());
        for id in 1..=(RESCAN_BUDGET as u32) {
            p.enqueue_rescan(id, RescanCause::WatchDropped);
        }
        // Trigger the fold with a *new* dir; the parent item must carry
        // Overflow (formed under load), not one member's WatchDropped.
        p.enqueue_rescan(RESCAN_BUDGET as u32 + 1, RescanCause::WatchDropped);
        let mut causes = Vec::new();
        while let Some(w) = p.pop_work() {
            if let WorkItem::Rescan { dir: 9000, cause } = w {
                causes.push(cause);
            }
        }
        assert_eq!(
            causes,
            vec![RescanCause::Overflow],
            "folded parent cause: {causes:?}"
        );
    }

    #[test]
    fn cookie_completes_only_when_drained() {
        let mut p = Pending::new(10);
        p.register_cookie(7, 0, b".fflocate-cookie-a".to_vec());
        p.push_event(FsEvent::upsert(
            DirKey::ROOT,
            b".fflocate-cookie-a".to_vec(),
            false,
        ));
        // queue still has the marker? No: marker events are not enqueued, but a
        // real event before the cookie keeps the scope undrained.
        p.push_event(FsEvent::upsert(DirKey::new(1), b"x".to_vec(), false));
        assert!(
            p.take_completed_cookies().is_empty(),
            "must not complete while work is pending"
        );
        p.pop_event();
        let done = p.take_completed_cookies();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].token, 7);
    }

    #[test]
    fn work_after_events() {
        let mut p = Pending::new(10);
        p.enqueue_crawl(3);
        p.push_event(FsEvent::remove(DirKey::new(3), b"x".to_vec()));
        // pop order is events first, then work
        assert!(p.pop_event().is_some());
        assert!(p.pop_work().is_some());
    }

    #[test]
    fn oldest_queued_is_none_when_drained() {
        let p = Pending::new(10);
        assert_eq!(p.oldest_queued_ms(), None);
    }

    #[test]
    fn oldest_queued_tracks_arrival_and_advances_on_pop() {
        let mut p = Pending::new(10);
        let before = crate::applier::now_ms();
        p.push_event(FsEvent::upsert(DirKey::new(1), b"a".to_vec(), false));
        p.enqueue_crawl(9);
        let oldest = p.oldest_queued_ms().expect("something queued");
        assert!(
            oldest >= before && oldest <= crate::applier::now_ms(),
            "stamp must be an arrival time, got {oldest} (before={before})"
        );
        // Events drain before work: popping the event moves the watermark to
        // the work item (stamped at or after the event).
        p.pop_event();
        let older = oldest;
        let next = p.oldest_queued_ms().expect("work still queued");
        assert!(next >= older, "watermark must not go backwards");
        p.pop_work();
        assert_eq!(p.oldest_queued_ms(), None, "drained → no lag");
    }

    #[test]
    fn oldest_queued_survives_collapse() {
        // Collapse folds siblings into a parent; the parent must inherit the
        // oldest member's stamp, or a storm would reset the lag to now.
        let mut p = Pending::new(10);
        p.set_parent_of(sibling_oracle());
        for id in 1..=(RESCAN_BUDGET as u32) {
            p.enqueue_rescan(id, RescanCause::Overflow);
        }
        let before_fold = p.oldest_queued_ms().expect("queued");
        p.enqueue_rescan(RESCAN_BUDGET as u32 + 1, RescanCause::Overflow);
        assert!(p.rescans_collapsed() > 0, "test must actually fold");
        let after_fold = p.oldest_queued_ms().expect("still queued");
        assert!(
            after_fold <= before_fold,
            "fold must inherit the oldest stamp, not now: {after_fold} > {before_fold}"
        );
    }

    #[test]
    fn coalesced_reenqueue_keeps_original_stamp() {
        // A re-enqueue of an already-queued dir is a no-op: the original
        // arrival stamp (the honest lag) must survive, not refresh.
        let mut p = Pending::new(10);
        p.enqueue_rescan(5, RescanCause::Overflow);
        let first = p.oldest_queued_ms().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        p.enqueue_rescan(5, RescanCause::Manual); // no-op: already queued
        assert_eq!(
            p.oldest_queued_ms(),
            Some(first),
            "coalesced re-enqueue must not refresh the stamp"
        );
    }
}
