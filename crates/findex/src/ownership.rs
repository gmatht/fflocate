//! Per-entry ownership, for the system daemon (§3, §15).
//!
//! The core index deliberately stores no per-file metadata: `Entry` is 12 bytes
//! and the design lists "per-file metadata in the core index" as an explicit
//! non-goal (§1), deferring it to a "lazy attr sidecar". This is that sidecar —
//! built only for the privileged system instance, where it is load-bearing
//! rather than optional.
//!
//! ```text
//! snapshot.fnx   the index (names, entries, parents)   ← unchanged
//! ownership.own   entry id → Attr { uid, gid, mode }   ← this module
//! ```
//!
//! ## Why it lives outside the entry
//!
//! Three reasons, in order of weight:
//!
//! 1. **Memory.** `Attr` is 12 bytes on top of a 12-byte entry — it would double
//!    the index for every user of every mode, to serve one mode.
//! 2. **Cost.** Filling it needs an `lstat` per file, which the crawl otherwise
//!    avoids entirely (`read_dir` gives the type without a stat). A system index
//!    pays that; a personal one should not.
//! 3. **Correctness.** Ownership changes without the *name* changing (a `chown`
//!    generates no namespace event), so this data is allowed to be stale in a way
//!    `Entry` is not. Keeping it separate keeps that weaker guarantee visible
//!    instead of pretending the two have the same lifetime.
//!
//! ## Staleness, stated plainly
//!
//! Ownership is captured at crawl time and refreshed on rescan. A `chown` or
//! `chmod` between rescans is not seen — fanotify's namespace mask does not
//! report it, and adding `FAN_ATTRIB` would make every `touch` an event for no
//! benefit to a *name* index. So this is a **best-effort** filter: it is exact at
//! crawl time and can lag. The failure direction matters and is the safe one —
//! see [`Ownership::visible_to`].

use std::collections::HashMap;

/// The access-relevant attributes of one entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Attr {
    pub uid: u32,
    pub gid: u32,
    /// The full `st_mode`, of which the low 12 bits are the permission bits.
    pub mode: u32,
}

impl Attr {
    /// The permission bits alone (`st_mode & 0o7777`).
    pub fn perms(&self) -> u32 {
        self.mode & 0o7777
    }

    pub fn is_dir(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
}

/// The caller, as established from `SO_PEERCRED` (never from anything the client
/// says about itself).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    pub gid: u32,
    /// Supplementary groups, resolved by the daemon from `/proc/<pid>/status` or
    /// `getgrouplist`. Empty means "primary group only".
    pub groups: &'static [u32],
}

impl Caller {
    pub fn new(uid: u32, gid: u32) -> Caller {
        Caller {
            uid,
            gid,
            groups: &[],
        }
    }

    /// uid 0 may read everything; the system index exists to serve everyone else.
    pub fn is_root(&self) -> bool {
        self.uid == 0
    }

    fn in_group(&self, gid: u32) -> bool {
        self.gid == gid || self.groups.contains(&gid)
    }
}

/// Ownership for every live entry, keyed by the index's stable entry id.
#[derive(Clone, Debug, Default)]
pub struct Ownership {
    attrs: HashMap<u32, Attr>,
}

impl Ownership {
    pub fn new() -> Ownership {
        Ownership::default()
    }

    pub fn len(&self) -> usize {
        self.attrs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.attrs.is_empty()
    }

    pub fn set(&mut self, id: u32, attr: Attr) {
        self.attrs.insert(id, attr);
    }

    pub fn get(&self, id: u32) -> Option<Attr> {
        self.attrs.get(&id).copied()
    }

    pub fn remove(&mut self, id: u32) {
        self.attrs.remove(&id);
    }

    /// Remap every id through `old_to_new`, dropping records whose entry did not
    /// survive compaction.
    ///
    /// Compaction renumbers ids densely (tombstones purged), so a table keyed by
    /// id goes stale the moment it runs. Stale in the *wrong* direction matters:
    /// a record left at an old id now describes whichever file moved into that
    /// slot, so the filter would answer about one file using another's
    /// permissions. `index.compacted()` hands over the map so this cannot be
    /// skipped by accident.
    pub fn remap(&mut self, old_to_new: &[u32]) {
        let mut out = HashMap::with_capacity(self.attrs.len());
        for (old, attr) in self.attrs.drain() {
            // Slot 0 is the virtual root and is never a real entry; ids past the
            // end did not survive.
            let Some(&new) = old_to_new.get(old as usize) else {
                continue;
            };
            if new == 0 {
                continue; // tombstoned: no slot in the compacted index
            }
            out.insert(new, attr);
        }
        self.attrs = out;
    }

    /// Drop everything about ids the caller no longer considers live. Called
    /// after compaction renumbers entries.
    pub fn retain_ids(&mut self, keep: impl Fn(u32) -> bool) {
        self.attrs.retain(|id, _| keep(*id));
    }

    /// May `caller` read this entry?
    ///
    /// ## The failure direction is the point
    ///
    /// An entry with **no recorded ownership** is *not* shown. That is the
    /// conservative choice, and it is deliberate: this filter guards a system
    /// index whose whole reason to run as root is to see things an unprivileged
    /// process cannot. If the sidecar is missing, stale, or half-built, the
    /// correct answer is "I cannot vouch for this file", which is "no".
    ///
    /// (The root user is the exception — uid 0 may read anything, so a missing
    /// record does not need to hide a file from root.)
    ///
    /// The classic rule is applied per component in the standard way: owner
    /// bits if uid matches, then group bits if any group matches, then other
    /// bits. Directory traversal needs `x`; a file needs `r`. Entries in between
    /// are the caller's job to check along the chain — which the query path does,
    /// because it materializes the whole path anyway.
    pub fn visible_to(&self, id: u32, caller: &Caller) -> bool {
        if caller.is_root() {
            return true;
        }
        let Some(attr) = self.get(id) else {
            return false; // unknown ⇒ not shown (see above)
        };
        let p = attr.perms();
        let bits = if attr.uid == caller.uid {
            (p >> 6) & 0o7
        } else if caller.in_group(attr.gid) {
            (p >> 3) & 0o7
        } else {
            p & 0o7
        };
        // Directories must be traversable to be useful; files must be readable.
        if attr.is_dir() {
            bits & 0o1 != 0
        } else {
            bits & 0o4 != 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(mode: u32, uid: u32, gid: u32) -> Attr {
        Attr {
            uid,
            gid,
            mode: 0o100000 | mode,
        }
    }
    fn dir(mode: u32, uid: u32, gid: u32) -> Attr {
        Attr {
            uid,
            gid,
            mode: 0o040000 | mode,
        }
    }

    #[test]
    fn owner_reads_their_own_file() {
        let mut o = Ownership::new();
        o.set(7, file(0o600, 1000, 1000));
        assert!(o.visible_to(7, &Caller::new(1000, 1000)));
        assert!(
            !o.visible_to(7, &Caller::new(1001, 1001)),
            "0600 is owner-only"
        );
    }

    #[test]
    fn group_membership_grants_group_bits() {
        let mut o = Ownership::new();
        o.set(7, file(0o640, 1000, 2000));
        assert!(o.visible_to(7, &Caller::new(1000, 999)));
        assert!(o.visible_to(7, &Caller::new(5555, 2000)), "primary group");
        let mut c = Caller::new(5555, 999);
        c.groups = &[2000];
        assert!(o.visible_to(7, &c), "supplementary group");
        assert!(
            !o.visible_to(7, &Caller::new(5555, 999)),
            "no group, no access"
        );
    }

    #[test]
    fn other_bits_apply_to_strangers() {
        let mut o = Ownership::new();
        o.set(7, file(0o644, 1000, 1000));
        assert!(o.visible_to(7, &Caller::new(4242, 4242)), "world-readable");
        o.set(8, file(0o600, 1000, 1000));
        assert!(
            !o.visible_to(8, &Caller::new(4242, 4242)),
            "not world-readable"
        );
    }

    #[test]
    fn directory_needs_traverse_not_read() {
        let mut o = Ownership::new();
        // 0711: others may enter but not list. A name index should still be able
        // to say the file exists (that is what the directory's own x buys).
        o.set(9, dir(0o711, 1000, 1000));
        assert!(o.visible_to(9, &Caller::new(4242, 4242)));
        // 0700: nobody else may even traverse.
        o.set(10, dir(0o700, 1000, 1000));
        assert!(!o.visible_to(10, &Caller::new(4242, 4242)));
    }

    #[test]
    fn root_sees_everything_including_unknown_ids() {
        let o = Ownership::new();
        assert!(
            o.visible_to(999, &Caller::new(0, 0)),
            "root may read anything"
        );
    }

    #[test]
    fn unknown_entry_is_hidden_from_non_root() {
        // The conservative direction: no record ⇒ not shown. A half-built
        // sidecar must fail closed.
        let o = Ownership::new();
        assert!(!o.visible_to(1, &Caller::new(1000, 1000)));
    }

    #[test]
    fn perms_and_dir_predicate() {
        let a = file(0o644, 0, 0);
        assert_eq!(a.perms(), 0o644);
        assert!(!a.is_dir());
        assert!(dir(0o755, 0, 0).is_dir());
    }
}
