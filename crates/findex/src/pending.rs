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

/// The ordered pending collection.
#[derive(Debug, Default)]
pub struct Pending {
    events: VecDeque<FsEvent>,
    work: VecDeque<WorkItem>,
    cookies: VecDeque<Cookie>,
    /// Dirs with a Rescan already queued, for coalescing.
    rescan_queued: std::collections::HashSet<u32>,
    /// Dirs with a CrawlDir already queued.
    crawl_queued: std::collections::HashSet<u32>,
    /// Capacity of the event queue; a full queue escalates to a rescan rather
    /// than dropping events (§6).
    pub capacity: usize,
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
        Pending { capacity, ..Default::default() }
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
        self.events.push_back(ev);
        PushOutcome::Accepted
    }

    /// Register a cookie the daemon is waiting on.
    pub fn register_cookie(&mut self, token: u64, root: u32, name: Vec<u8>) {
        self.cookies.push_back(Cookie { token, root, name, seen: false });
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
                ready.push(Cookie { token: c.token, root: c.root, name: c.name.clone(), seen: true });
                false
            } else {
                true
            }
        });
        ready
    }

    pub fn enqueue_rescan(&mut self, dir: u32, cause: RescanCause) {
        if self.rescan_queued.insert(dir) {
            self.work.push_back(WorkItem::Rescan { dir, cause });
        }
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
            self.work.push_back(WorkItem::CrawlDir { dir });
        }
    }

    pub fn enqueue_drop(&mut self, dir: u32) {
        self.work.push_back(WorkItem::DropSubtree { dir });
    }

    /// Pop the next event, if any (events before work, per the ordering).
    pub fn pop_event(&mut self) -> Option<FsEvent> {
        self.events.pop_front()
    }

    /// Pop the next work item.
    pub fn pop_work(&mut self) -> Option<WorkItem> {
        let w = self.work.pop_front();
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
        assert_eq!(p.pop_event(), Some(FsEvent::upsert(DirKey::new(1), b"a".to_vec(), false)));
        assert_eq!(p.pop_event(), Some(FsEvent::upsert(DirKey::new(1), b"b".to_vec(), false)));
        assert_eq!(p.pop_event(), None);
    }

    #[test]
    fn cookie_events_are_filtered_not_indexed() {
        let mut p = Pending::new(100);
        p.register_cookie(1, 0, b".findex-cookie-x".to_vec());
        let out = p.push_event(FsEvent::upsert(DirKey::ROOT, b".findex-cookie-x".to_vec(), false));
        assert_eq!(out, PushOutcome::CookieMarker);
        assert_eq!(p.events_len(), 0, "cookie must never become an indexable event");
    }

    #[test]
    fn overflow_escalates_to_rescan() {
        let mut p = Pending::new(2);
        p.push_event(FsEvent::upsert(DirKey::new(1), b"a".to_vec(), false));
        p.push_event(FsEvent::upsert(DirKey::new(1), b"b".to_vec(), false));
        let out = p.push_event(FsEvent::upsert(DirKey::new(1), b"c".to_vec(), false));
        assert_eq!(out, PushOutcome::Escalated);
        assert_eq!(p.events_len(), 2);
        assert_eq!(p.work_len(), 1, "the dropped event becomes a rescan, not silence");
        assert!(matches!(p.pop_work(), Some(WorkItem::Rescan { dir: 1, cause: RescanCause::Overflow })));
    }

    #[test]
    fn rescans_coalesce_per_dir() {
        let mut p = Pending::new(10);
        p.enqueue_rescan(5, RescanCause::Overflow);
        p.enqueue_rescan(5, RescanCause::Manual);
        p.enqueue_rescan(6, RescanCause::Manual);
        assert_eq!(p.work_len(), 2);
        assert!(matches!(p.pop_work(), Some(WorkItem::Rescan { dir: 5, cause: RescanCause::Overflow })));
        // After popping, the dir can be re-enqueued.
        p.enqueue_rescan(5, RescanCause::Manual);
        assert_eq!(p.work_len(), 2);
    }

    #[test]
    fn cookie_completes_only_when_drained() {
        let mut p = Pending::new(10);
        p.register_cookie(7, 0, b".findex-cookie-a".to_vec());
        p.push_event(FsEvent::upsert(DirKey::ROOT, b".findex-cookie-a".to_vec(), false));
        // queue still has the marker? No: marker events are not enqueued, but a
        // real event before the cookie keeps the scope undrained.
        p.push_event(FsEvent::upsert(DirKey::new(1), b"x".to_vec(), false));
        assert!(p.take_completed_cookies().is_empty(), "must not complete while work is pending");
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
}
