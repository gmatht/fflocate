//! The index data model (§3).
//!
//! ```text
//! struct Entry {          // 12 bytes
//!     name_off: u32,      // offset into names arena
//!     name_len: u16,
//!     parent: u32,        // ROOT = 0
//!     flags: u8,          // IS_DIR | IS_SYMLINK | TOMBSTONE | IS_EXEC
//! }
//! ```
//!
//! Beside the entries sits a parallel `created: Vec<u32>` of creation (birth)
//! times in **minutes since the unix epoch**, 0 meaning unknown. It is 4 bytes
//! per entry rather than 8 (or 12, with nanoseconds) because the only consumers
//! compare calendar days or whole seconds at a coarse `since` boundary, and a
//! minute is fine enough for both; a `u32` of minutes lasts past the year 10000.
//! Keeping it out of `Entry` leaves the on-disk entry record at 12 bytes and
//! costs nothing for the queries that never look at time.
//!
//! `IS_EXEC` means "any execute bit set" (`find -perm /111`), deliberately
//! *not* effective access for the querying user: the system daemon serves many
//! users from one index, and per-user executability needs the full
//! `mode+uid+gid` (the ownership sidecar already carries that). Directories
//! are never marked executable even when their `x` (traverse) bit is set —
//! `type:exec` answers "is this file executable", and a directory is not a
//! file. Symlinks `lstat` as `0777`, so the bit must be decided from the
//! target's mode, not the link's.
//!
//! No path strings are stored. Paths are materialized by walking the parent
//! chain on demand, which makes directory renames O(1). Deletions are
//! tombstones; path materialization drops results through tombstoned dirs and
//! compaction purges them.
//!
//! IDs are stable across rescans (§0 lesson 4): a rescan matches surviving
//! entries by `(parent, name)` and keeps their ids. IDs are never reused.

use std::borrow::Cow;

/// Reserved id for the virtual root parent. Every `Root`'s directory entry
/// has `parent == ROOT_ID`.
pub const ROOT_ID: u32 = 0;

pub mod flags {
    pub const IS_DIR: u8 = 0b0000_0001;
    pub const IS_SYMLINK: u8 = 0b0000_0010;
    pub const TOMBSTONE: u8 = 0b0000_0100;
    /// Set for entries that belong to a root directory itself (they are never
    /// matched by a bare `parent == ROOT_ID` walk, since a root is referenced by
    /// its own id).
    pub const IS_ROOT_DIR: u8 = 0b0000_1000;
    /// Any execute bit set (`mode & 0o111 != 0`, decided from `stat` so
    /// symlinks report their target's mode). Never set on directories.
    pub const IS_EXEC: u8 = 0b0001_0000;
}

/// Bit flags stored compactly in an [`Entry`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct EntryFlags(pub u8);

impl EntryFlags {
    pub const EMPTY: EntryFlags = EntryFlags(0);

    pub fn is_dir(self) -> bool {
        self.0 & flags::IS_DIR != 0
    }
    pub fn is_symlink(self) -> bool {
        self.0 & flags::IS_SYMLINK != 0
    }
    pub fn is_tombstone(self) -> bool {
        self.0 & flags::TOMBSTONE != 0
    }
    pub fn is_root_dir(self) -> bool {
        self.0 & flags::IS_ROOT_DIR != 0
    }
    pub fn is_exec(self) -> bool {
        self.0 & flags::IS_EXEC != 0
    }
    pub fn with_dir(mut self, v: bool) -> Self {
        self.set(flags::IS_DIR, v);
        self
    }
    pub fn with_exec(mut self, v: bool) -> Self {
        self.set(flags::IS_EXEC, v);
        self
    }
    pub fn with_symlink(mut self, v: bool) -> Self {
        self.set(flags::IS_SYMLINK, v);
        self
    }
    pub fn with_tombstone(mut self, v: bool) -> Self {
        self.set(flags::TOMBSTONE, v);
        self
    }
    pub fn with_root_dir(mut self, v: bool) -> Self {
        self.set(flags::IS_ROOT_DIR, v);
        self
    }
    fn set(&mut self, bit: u8, v: bool) {
        if v {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }
}

/// One index entry. Deliberately 12 bytes on disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Entry {
    pub name_off: u32,
    pub name_len: u16,
    pub parent: u32,
    pub flags: u8,
}

impl Entry {
    pub fn flags(&self) -> EntryFlags {
        EntryFlags(self.flags)
    }
}

/// The stored creation-time scale, defined here and nowhere else: whole
/// **minutes since the unix epoch**, with [`CREATED_UNKNOWN`] meaning unknown.
/// Writers convert a `SystemTime` with [`created_minutes_at`], readers convert
/// back with [`created_secs_from_min`], and the snapshot encoder writes exactly
/// one `u32` of this per entry — so the crawler, the journal, the snapshot, the
/// query engine and the output boundary cannot disagree about what a stored
/// number means.
pub const SECONDS_PER_MINUTE: i64 = 60;

/// The "unknown" sentinel: no birth time reported, or one before the epoch,
/// which the field cannot represent.
///
/// Deliberately **not** `0`: minute 0 is a real minute — the first minute of
/// 1970 — so an entry created 0–59 s after the epoch has to be datable, and a
/// stored `0` must never again stand for two different things at once. This
/// value lies outside every representable minute because [`created_minutes_at`]
/// saturates at [`CREATED_LATEST_MINUTE`].
pub const CREATED_UNKNOWN: u32 = u32::MAX;

/// The largest representable minute: one below [`CREATED_UNKNOWN`], so
/// saturation can never be mistaken for "unknown". Minute `u32::MAX - 1` falls
/// in the year 10135, far past any timestamp a filesystem reports.
pub const CREATED_LATEST_MINUTE: u32 = CREATED_UNKNOWN - 1;

/// A filesystem timestamp as the stored representation.
///
/// Seconds are truncated, not rounded: 59 s past the epoch is minute 0 and 60 s
/// is minute 1, so a stored minute is the *window* `[min*60, min*60 + 59]`, not
/// a single instant. A lower bound (`since <ts>`) must therefore be compared
/// against the window's **last** second (see [`created_latest_secs_from_min`]):
/// a bound anywhere inside a minute includes that minute's entries, which is
/// what keeps a coarse clock from silently dropping fresh files. A time before
/// the epoch reads as unknown — the field cannot represent it, and "unknown" is
/// the honest reading, never a wrapped giant number or 1970. A time past
/// [`CREATED_LATEST_MINUTE`] saturates there instead of wrapping.
pub fn created_minutes_at(t: std::time::SystemTime) -> u32 {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let mins = d.as_secs() / SECONDS_PER_MINUTE as u64;
            if mins >= u64::from(CREATED_LATEST_MINUTE) {
                CREATED_LATEST_MINUTE
            } else {
                mins as u32
            }
        }
        Err(_) => CREATED_UNKNOWN,
    }
}

/// The start of the stored minute as absolute unix seconds: the instant to
/// display, never the value a `since` bound is compared against.
pub fn created_secs_from_min(min: u32) -> i64 {
    min as i64 * SECONDS_PER_MINUTE
}

/// The **last** second the stored minute can hold (`min*60 + 59`).
///
/// This is the value a lower-bound (`since`) test compares: an entry stored in
/// minute `m` may have been created anywhere inside that minute, so it belongs
/// to `since ts` whenever `ts <= m*60 + 59`. Comparing the minute's *start*
/// instead would drop a file created at `:59` from a query made at `:30` of the
/// same minute — the truncation would lose a file it should have shown.
pub fn created_latest_secs_from_min(min: u32) -> i64 {
    created_secs_from_min(min) + SECONDS_PER_MINUTE - 1
}

/// The in-memory index.
///
/// Ids index directly into `entries`; slot 0 is the virtual root and is never
/// a real file. `alive` is a cheap side bitset mirroring the tombstone bit so
/// the query engine can iterate without branch-heavy flag reads.
#[derive(Clone, Debug)]
pub struct Index {
    /// names arena: all names concatenated, back to back, no separators.
    pub names: Vec<u8>,
    pub entries: Vec<Entry>,
    /// Creation (birth) time per entry, in minutes since the unix epoch;
    /// [`CREATED_UNKNOWN`] means unknown (no birth time on this filesystem, or
    /// a time before 1970). Minute 0 is a real minute, not a sentinel.
    /// Parallel to `entries`: the two are always the same length, and a caller
    /// that appends an entry must append a slot here. Read through
    /// [`Index::created_min`] so the `0` sentinel never leaks as a real time.
    created: Vec<u32>,
    /// parallel liveness bitset (1 = not tombstoned), 1 bit per entry id.
    /// A bitvec rather than `Vec<bool>` (which costs a full byte per entry):
    /// at 3M entries this is ~0.4 MiB instead of ~3 MiB. Derived from the
    /// tombstone flags on snapshot load, never serialized itself.
    alive: Vec<u64>,
    /// number of bits stored in `alive` (always == entries.len()).
    alive_len: usize,
    /// next id to hand out; never reused.
    next_id: u32,
    /// generation, bumped on snapshot load / full-root rescan / compaction (§3).
    generation: u64,
    /// Last applied journal sequence number (the "clock").
    last_lsn: u64,
    /// Number of non-tombstoned entries (excluding slot 0).
    live_count: u64,
    /// Live ids per parent, ascending.
    ///
    /// `lookup` and `children` are called once per directory entry while
    /// crawling, so answering them from a whole-index scan made a crawl of N
    /// entries cost O(N²): measured at 197 µs per lookup with 100k entries and
    /// 784 µs with 300k, i.e. minutes of one core on a few hundred thousand
    /// files — with the applier taking the index lock throughout, which is what
    /// starved a concurrent `stats` reader until `ff` looked frozen.
    ///
    /// Only live ids are stored (a tombstone is unlinked by `remove` and
    /// relinked by `revive`/`apply_add`; `move_entry` moves the link), and each
    /// list stays sorted by id so `lookup`'s first match is still the lowest-id
    /// match the old scan returned. It is derived state, never serialized: a
    /// decoded snapshot rebuilds it in `new_reconstructed`, compaction rebuilds
    /// it through `insert`.
    children_of: std::collections::HashMap<u32, Vec<u32>>,
}

impl Default for Index {
    fn default() -> Self {
        Self::new()
    }
}

impl Index {
    pub fn new() -> Self {
        let mut idx = Index {
            names: Vec::new(),
            entries: Vec::new(),
            created: Vec::new(),
            alive: Vec::new(),
            alive_len: 0,
            next_id: 1,
            generation: 1,
            last_lsn: 0,
            live_count: 0,
            children_of: std::collections::HashMap::new(),
        };
        // Slot 0: the virtual root. Unnameable, always dead to queries.
        idx.entries.push(Entry::default());
        idx.created.push(CREATED_UNKNOWN);
        idx.alive_push(false);
        idx
    }

    /// Rebuild an index from decoded snapshot arrays. Derives the liveness
    /// bitset and live count from the tombstone flags.
    ///
    /// `created` is padded (or truncated) to the entry count: a snapshot from a
    /// format that predates creation times decodes with none, which reads as
    /// "unknown" rather than failing to load.
    pub fn new_reconstructed(names: Vec<u8>, entries: Vec<Entry>, mut created: Vec<u32>) -> Self {
        // A short array can only come from a format that carried no times: pad
        // with "unknown", never with a real minute.
        created.resize(entries.len(), CREATED_UNKNOWN);
        let mut idx = Index {
            names,
            entries,
            created,
            alive: Vec::new(),
            alive_len: 0,
            next_id: 1,
            generation: 1,
            last_lsn: 0,
            live_count: 0,
            children_of: std::collections::HashMap::new(),
        };
        idx.alive.reserve((idx.entries.len() + 63) / 64);
        for i in 0..idx.entries.len() {
            let a = i != 0 && !idx.entries[i].flags().is_tombstone();
            if a {
                idx.live_count += 1;
            }
            idx.alive_push(a);
        }
        // The child index is derived state, like `alive`: rebuild it from the
        // decoded entries in one ascending pass, which also leaves each list
        // sorted by id as `link` maintains it.
        idx.children_of.reserve(idx.entries.len() / 8);
        for i in 1..idx.entries.len() {
            if idx.alive_get(i) {
                let parent = idx.entries[i].parent;
                idx.children_of.entry(parent).or_default().push(i as u32);
            }
        }
        idx
    }

    // ------------------------------------------------------------ bitset ops

    /// Append one liveness bit. Only ever called with the next sequential id
    /// (insert/pad paths), so no sparse-set support is needed.
    fn alive_push(&mut self, v: bool) {
        let i = self.alive_len;
        if i % 64 == 0 {
            self.alive.push(0);
        }
        if v {
            self.alive[i / 64] |= 1u64 << (i % 64);
        }
        self.alive_len += 1;
    }

    fn alive_get(&self, i: usize) -> bool {
        if i >= self.alive_len {
            return false;
        }
        (self.alive[i / 64] >> (i % 64)) & 1 != 0
    }

    fn alive_set(&mut self, i: usize, v: bool) {
        debug_assert!(i < self.alive_len, "alive_set out of range: {i}");
        if v {
            self.alive[i / 64] |= 1u64 << (i % 64);
        } else {
            self.alive[i / 64] &= !(1u64 << (i % 64));
        }
    }

    /// Resident bytes of the liveness bitset (diagnostics/tests).
    #[cfg(test)]
    pub fn alive_bytes(&self) -> usize {
        self.alive.len() * std::mem::size_of::<u64>()
    }

    /// Test-only: link `id` under `parent` without touching `entries`, which
    /// reproduces the stale child-index state an older store could carry.
    #[cfg(test)]
    pub fn link_stale_for_test(&mut self, id: u32, parent: u32) {
        self.link(id, parent);
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn set_generation(&mut self, g: u64) {
        self.generation = g;
    }
    pub fn bump_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }
    pub fn last_lsn(&self) -> u64 {
        self.last_lsn
    }
    pub fn set_last_lsn(&mut self, lsn: u64) {
        self.last_lsn = lsn;
    }
    pub fn next_id(&self) -> u32 {
        self.next_id
    }
    pub fn set_next_id(&mut self, id: u32) {
        self.next_id = id.max(1);
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.live_count == 0
    }
    pub fn live_count(&self) -> u64 {
        self.live_count
    }
    /// Total entries including tombstones; useful for compaction triggers.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
    pub fn tombstone_count(&self) -> u64 {
        (self.entries.len() as u64 - 1).saturating_sub(self.live_count)
    }

    // ---------------------------------------------------------------- names

    pub fn name(&self, id: u32) -> &[u8] {
        let e = &self.entries[id as usize];
        let off = e.name_off as usize;
        &self.names[off..off + e.name_len as usize]
    }

    fn name_of_entry(&self, e: &Entry) -> &[u8] {
        let off = e.name_off as usize;
        &self.names[off..off + e.name_len as usize]
    }

    // -------------------------------------------------------------- lookups

    pub fn entry(&self, id: u32) -> Option<&Entry> {
        self.entries.get(id as usize)
    }

    pub fn entry_mut(&mut self, id: u32) -> &mut Entry {
        &mut self.entries[id as usize]
    }

    /// Record an entry's creation time, in minutes since the unix epoch.
    /// [`CREATED_UNKNOWN`] clears it back to "unknown".
    pub fn set_created(&mut self, id: u32, minutes: u32) {
        if self.created.len() < self.entries.len() {
            self.created.resize(self.entries.len(), CREATED_UNKNOWN);
        }
        if let Some(slot) = self.created.get_mut(id as usize) {
            *slot = minutes;
        }
    }

    /// An entry's creation time in minutes since the epoch, or `None` when the
    /// index does not know it (stored as [`CREATED_UNKNOWN`]).
    pub fn created_min(&self, id: u32) -> Option<u32> {
        self.created
            .get(id as usize)
            .copied()
            .filter(|m| *m != CREATED_UNKNOWN)
    }

    /// The start of an entry's creation minute in unix seconds, or `None` when
    /// unknown. For display; a `since` bound must compare
    /// [`created_latest_secs_from_min`] instead.
    pub fn created_secs(&self, id: u32) -> Option<i64> {
        self.created_min(id).map(created_secs_from_min)
    }

    /// The last second an entry's creation minute can hold, or `None` when
    /// unknown. This is the clock a lower-bound (`since`) query compares.
    pub fn created_latest_secs(&self, id: u32) -> Option<i64> {
        self.created_min(id).map(created_latest_secs_from_min)
    }

    /// The raw creation array, for the snapshot encoder and diagnostics.
    pub fn created_array(&self) -> &[u32] {
        &self.created
    }

    pub fn is_alive(&self, id: u32) -> bool {
        self.alive_get(id as usize)
    }

    pub fn is_dir(&self, id: u32) -> bool {
        self.is_alive(id) && self.entries[id as usize].flags().is_dir()
    }

    /// Live entries whose ancestor chain runs through an entry that is not a
    /// directory — what a store written when a crawl still followed symlinks
    /// holds beneath Wine's `dosdevices/z:` → `/`.
    ///
    /// `children()` answers "what is under this id" by scanning every entry, so
    /// asking it per entry would be O(entries²). Walking each entry's own parent
    /// chain answers it in one pass instead. A parent that is dead ends the
    /// chain too: an entry a tombstone cannot be reached through is not a hit
    /// either. The depth cap only bounds a store we could not have written.
    pub fn entries_under_non_dirs(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for id in 0..self.entries.len() {
            if id as u32 == ROOT_ID || !self.alive_get(id) {
                continue;
            }
            let mut parent = self.entries[id].parent;
            for _ in 0..4096 {
                if parent == ROOT_ID
                    || parent as usize >= self.entries.len()
                    || !self.alive_get(parent as usize)
                {
                    break;
                }
                if !self.entries[parent as usize].flags().is_dir() {
                    out.push(id as u32);
                    break;
                }
                parent = self.entries[parent as usize].parent;
            }
        }
        out
    }

    /// Link a live id into its parent's child list, keeping the list sorted by
    /// id (so `lookup` still finds the lowest id first, as the old scan did).
    fn link(&mut self, id: u32, parent: u32) {
        let kids = self.children_of.entry(parent).or_default();
        if let Err(pos) = kids.binary_search(&id) {
            kids.insert(pos, id);
        }
    }

    /// Drop an id from its parent's child list (a tombstone, or a move).
    fn unlink(&mut self, id: u32, parent: u32) {
        if let Some(kids) = self.children_of.get_mut(&parent) {
            if let Some(pos) = kids.iter().position(|&c| c == id) {
                kids.remove(pos);
            }
            if kids.is_empty() {
                // Don't keep a map entry for every directory that empties out:
                // a long-lived daemon sees plenty of those.
                self.children_of.remove(&parent);
            }
        }
    }

    /// Find a live child of `parent` with `name` (exact byte match).
    ///
    /// Scans only `parent`'s own children (see [`Index::children_of`]), in
    /// ascending id order — O(siblings), not O(entries). The former whole-index
    /// scan made every crawl quadratic; the field's note has the measurements.
    pub fn lookup(&self, parent: u32, name: &[u8]) -> Option<u32> {
        for &id in self.children_of.get(&parent)? {
            // A link with no slot is skipped, never indexed. `children_of` is
            // derived state, but every crawl and every startup root lookup
            // runs through here, so a stale id in it must cost a miss, not a
            // panic that takes the daemon down before it serves anything.
            let Some(e) = self.entries.get(id as usize) else {
                continue;
            };
            if e.name_len as usize == name.len() && self.name_of_entry(e) == name {
                return Some(id);
            }
        }
        None
    }

    /// Create a brand-new entry and return its (fresh) id. The parent must
    /// exist and be live; callers ensure that.
    pub fn insert(&mut self, parent: u32, name: &[u8], flags: EntryFlags) -> u32 {
        // The id *is* the slot (`entries[id]`), so it must be the slot about to
        // be appended to — not `next_id`, which is only a high-water mark and
        // can run ahead of the array. It does exactly that after a compaction:
        // compaction renumbers live entries densely but keeps the old next id.
        // Using it here linked an id past the end of `entries`, so
        // `children_of` held a slot that did not exist and the next `lookup`
        // through it panicked (`index out of bounds: the len is 3705012 but
        // the index is 3733663` — a store written before this invariant was
        // restored, which crashed every startup that added a root).
        let id = self.entries.len() as u32;
        if id >= self.next_id {
            self.next_id = id + 1;
        }
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        self.entries.push(Entry {
            name_off: off,
            name_len: name.len() as u16,
            parent,
            flags: flags.0,
        });
        self.created.push(CREATED_UNKNOWN);
        self.alive_push(!flags.is_tombstone());
        if !flags.is_tombstone() {
            self.live_count += 1;
            self.link(id, parent);
        }
        id
    }

    /// Idempotent upsert: return the existing id if a live entry with
    /// `(parent, name)` exists, else insert. Ids of survivors are preserved
    /// (§3: stable IDs across rescans).
    pub fn upsert(&mut self, parent: u32, name: &[u8], flags: EntryFlags) -> (u32, bool) {
        if let Some(id) = self.lookup(parent, name) {
            let cur = &mut self.entries[id as usize];
            // Preserve tombstone state (it is live), refresh type flags.
            let tb = cur.flags().is_tombstone();
            let mut nf = flags;
            nf = nf.with_tombstone(tb);
            cur.flags = nf.0;
            (id, false)
        } else {
            let id = self.insert(parent, name, flags);
            (id, true)
        }
    }

    /// Mark an entry as tombstoned. Returns true if it was live before.
    pub fn remove(&mut self, id: u32) -> bool {
        if id == ROOT_ID || id as usize >= self.entries.len() {
            return false;
        }
        let was = self.alive_get(id as usize);
        let parent = self.entries[id as usize].parent;
        self.entries[id as usize].flags |= flags::TOMBSTONE;
        self.alive_set(id as usize, false);
        if was {
            self.live_count -= 1;
            self.unlink(id, parent);
        }
        was
    }

    /// Remove a live child by `(parent, name)`; true if something was removed.
    pub fn remove_child(&mut self, parent: u32, name: &[u8]) -> bool {
        match self.lookup(parent, name) {
            Some(id) => self.remove(id),
            None => false,
        }
    }

    /// Apply an ADD record at a *specific* id. This is the replay path.
    ///
    /// Journal ids are assigned by the applier, so normally the snapshot already
    /// has a slot for `id`. When it does not (snapshot older than journal, or a
    /// torn snapshot), we pad forward with tombstoned slots so ids stay aligned.
    pub fn apply_add(&mut self, id: u32, parent: u32, name: &[u8], flags: EntryFlags) {
        if id as usize >= self.entries.len() {
            while self.entries.len() <= id as usize {
                let pad_id = self.entries.len() as u32;
                let _ = pad_id;
                self.entries.push(Entry {
                    name_off: 0,
                    name_len: 0,
                    parent: ROOT_ID,
                    flags: flags::TOMBSTONE,
                });
                self.created.push(CREATED_UNKNOWN);
                self.alive_push(false);
            }
        }
        // A replay re-add can land on a slot that is already live: unlink it
        // from its *old* parent before the record re-points it.
        let was_live = self.alive_get(id as usize);
        if was_live {
            let old_parent = self.entries[id as usize].parent;
            self.unlink(id, old_parent);
        }
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        let e = &mut self.entries[id as usize];
        e.name_off = off;
        e.name_len = name.len() as u16;
        e.parent = parent;
        e.flags = flags.with_tombstone(false).0;
        if !was_live {
            self.alive_set(id as usize, true);
            self.live_count += 1;
        }
        self.link(id, parent);
        if id >= self.next_id {
            self.next_id = id + 1;
        }
    }

    /// Restore a tombstoned entry to life (used when a rescan diff discovers a
    /// tombstone still exists on disk — keeps the id stable).
    pub fn revive(&mut self, id: u32, flags: EntryFlags) {
        if id as usize >= self.entries.len() {
            return;
        }
        let parent = self.entries[id as usize].parent;
        let e = &mut self.entries[id as usize];
        e.flags = flags.with_tombstone(false).0;
        if !self.alive_get(id as usize) {
            self.alive_set(id as usize, true);
            self.live_count += 1;
            self.link(id, parent);
        }
    }

    /// Live children of `parent` as ids, ascending. Used by rescan diffing and
    /// by directory-rename moves; answered from the child index, so it costs
    /// O(children) rather than a scan of every entry.
    pub fn children(&self, parent: u32) -> Vec<u32> {
        // Links past the end are dropped here too (see [`Index::lookup`]): the
        // callers of `children` index entries with what they get back, and a
        // stale id must not travel further than this boundary.
        self.children_of
            .get(&parent)
            .map(|kids| {
                kids.iter()
                    .copied()
                    .filter(|id| (*id as usize) < self.entries.len())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Move a whole directory subtree: re-point the moved entry's parent (and
    /// optionally rename it). O(1) — children are found via their parent id.
    pub fn move_entry(&mut self, id: u32, new_parent: u32, new_name: Option<&[u8]>) {
        let old_parent = self.entries[id as usize].parent;
        // The borrow dance: mutate name arena first, then the entry.
        let (name_off, name_len) = if let Some(n) = new_name {
            let off = self.names.len() as u32;
            self.names.extend_from_slice(n);
            (off, n.len() as u16)
        } else {
            let e = &self.entries[id as usize];
            (e.name_off, e.name_len)
        };
        let e = &mut self.entries[id as usize];
        e.parent = new_parent;
        e.name_off = name_off;
        e.name_len = name_len;
        // A moved entry changes which child list it belongs to. Its own
        // children keep pointing at it, so only this one link moves.
        if old_parent != new_parent && self.alive_get(id as usize) {
            self.unlink(id, old_parent);
            self.link(id, new_parent);
        }
    }

    // -------------------------------------------------------------- paths

    /// Materialize the path of `id` under `root_path` into `buf`.
    ///
    /// Returns `None` when any ancestor (excluding the root dir itself) is a
    /// tombstone — a result inside a deleted directory must not be reported.
    /// The buffer is reused across calls; callers pass a fresh/cleared Vec.
    pub fn materialize_into<'a>(
        &'a self,
        id: u32,
        root_path: &[u8],
        buf: &'a mut Vec<u8>,
    ) -> Option<&'a [u8]> {
        buf.clear();
        // Collect ids from the entry up to (but not including) the root dir.
        let mut chain: Vec<u32> = Vec::with_capacity(8);
        let mut cur = id;
        let mut hit_root_dir = false;
        loop {
            if cur == ROOT_ID {
                break;
            }
            if cur as usize >= self.entries.len() || !self.alive_get(cur as usize) {
                return None;
            }
            let e = &self.entries[cur as usize];
            if e.flags().is_root_dir() {
                hit_root_dir = true;
                break;
            }
            if e.flags().is_tombstone() {
                return None;
            }
            chain.push(cur);
            cur = e.parent;
        }
        if !hit_root_dir {
            return None; // chain never reached a root dir: orphaned/incomplete
        }
        // `chain` holds the entry and every ancestor *below* the root dir; the
        // root dir's own name is deliberately excluded because `root_path`
        // already names it (and may be a mount point or symlink target).
        if root_path == b"/" {
            buf.push(b'/');
        } else {
            buf.extend_from_slice(root_path);
        }
        for &cid in chain.iter().rev() {
            if buf.last() != Some(&b'/') {
                buf.push(b'/');
            }
            buf.extend_from_slice(self.name(cid));
        }
        Some(buf.as_slice())
    }

    /// Convenience wrapper allocating a `Vec`.
    pub fn path_of(&self, id: u32, root_path: &[u8]) -> Option<Vec<u8>> {
        let mut buf = Vec::new();
        self.materialize_into(id, root_path, &mut buf)
            .map(|s| s.to_vec())
    }

    // ------------------------------------------------------------- compaction

    /// Build a fresh, compacted copy of this index: tombstones purged, ids
    /// renumbered densely, names arena rebuilt. Live ids are remapped in a
    /// stable (ascending) order so relationships survive.
    ///
    /// Returns the new index **and the old→new id map**, because compaction
    /// renumbers ids and anything keyed by id outside the index — the ownership
    /// sidecar, for one — must be remapped with it. Returning the map here
    /// rather than recomputing it keeps the two in step: a caller cannot forget
    /// it, because it is handed over with the new index.
    pub fn compacted(&self) -> (Index, Vec<u32>) {
        let mut out = Index::new();
        out.generation = self.generation;
        out.last_lsn = self.last_lsn;
        // map old id -> new id
        let mut map: Vec<u32> = vec![0; self.entries.len()];
        // Two passes: first allocate all new slots preserving order, then fix
        // parents (parents always have a smaller index than children because we
        // only ever create a child after its parent).
        for old in 1..self.entries.len() {
            if !self.alive_get(old) {
                continue;
            }
            let e = self.entries[old];
            let new_parent = map[e.parent as usize];
            let name = self.name_of_entry(&e).to_vec();
            let id = out.insert(new_parent, &name, EntryFlags(e.flags));
            // `.get` rather than `[]`: a hand-built index (tests, a future
            // loader) may carry a short array, and "unknown" is the safe
            // reading of a missing slot rather than a panic mid-compaction.
            out.set_created(
                id,
                self.created.get(old).copied().unwrap_or(CREATED_UNKNOWN),
            );
            map[old] = id;
        }
        out.set_next_id(self.next_id);
        (out, map)
    }

    // ------------------------------------------------------------- iteration

    /// Iterate live entries with their ids.
    pub fn iter_live(&self) -> impl Iterator<Item = (u32, &Entry)> {
        self.entries
            .iter()
            .enumerate()
            .skip(1)
            .filter(move |(i, _)| self.alive_get(*i))
            .map(|(i, e)| (i as u32, e))
    }

    /// For testing/diagnostics: does the parent chain reach a live root dir,
    /// and are all ancestors live?
    pub fn chain_ok(&self, id: u32) -> bool {
        let mut cur = id;
        loop {
            if cur == ROOT_ID {
                return false;
            }
            if cur as usize >= self.entries.len() || !self.alive_get(cur as usize) {
                return false;
            }
            let e = &self.entries[cur as usize];
            if e.flags().is_root_dir() {
                return true;
            }
            cur = e.parent;
        }
    }

    /// Root-dir entries (the roots table's anchors in the index).
    pub fn root_dirs(&self) -> Vec<u32> {
        self.entries
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(i, e)| self.alive_get(*i) && e.flags().is_root_dir())
            .map(|(i, _)| i as u32)
            .collect()
    }

    /// Which root dir does `id` fall under, if any?
    pub fn root_of(&self, id: u32) -> Option<u32> {
        let mut cur = id;
        loop {
            if cur == ROOT_ID || cur as usize >= self.entries.len() {
                return None;
            }
            let e = &self.entries[cur as usize];
            if e.flags().is_root_dir() {
                return Some(cur);
            }
            if !self.alive_get(cur as usize) {
                return None;
            }
            cur = e.parent;
        }
    }
}

/// A display helper for a name that may contain non-UTF-8 bytes.
pub fn name_lossy(name: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> EntryFlags {
        EntryFlags::EMPTY.with_dir(true)
    }
    fn file() -> EntryFlags {
        EntryFlags::EMPTY
    }

    /// A store whose `next_id` ran ahead of its array (compaction renumbers
    /// densely but keeps the high-water mark) must still append at slots that
    /// exist: the id *is* the slot, and the next lookup goes through it.
    #[test]
    fn insert_lands_in_range_when_next_id_ran_ahead() {
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"p", dir().with_root_dir(true));
        ix.insert(root, b"a", file());
        ix.set_next_id(1_000_000);

        let before = ix.len();
        let second = ix.insert(ROOT_ID, b"/home/me", dir().with_root_dir(true));
        assert_eq!(
            second as usize, before,
            "the id must be the slot that was appended, not next_id"
        );
        assert_eq!(ix.lookup(ROOT_ID, b"/home/me"), Some(second));
        assert_eq!(ix.lookup(ROOT_ID, b"p"), Some(root));
        for id in ix.children(ROOT_ID) {
            assert!((id as usize) < ix.len(), "child {id} has no slot");
        }
    }

    /// A stale link in the child index is a miss, never a panic: every crawl
    /// and every startup root lookup answers through `lookup`.
    #[test]
    fn lookup_skips_a_stale_child_link() {
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"p", dir().with_root_dir(true));
        let a = ix.insert(root, b"a", file());
        let stale = ix.len() as u32 + 5;
        ix.link_stale_for_test(stale, root);

        assert_eq!(ix.lookup(root, b"zzz"), None, "scanning past the stale id");
        assert_eq!(ix.lookup(root, b"a"), Some(a));
        assert!(
            !ix.children(root).contains(&stale),
            "a link with no slot must not be handed out"
        );
    }

    #[test]
    fn insert_lookup_remove() {
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"proj", dir().with_root_dir(true));
        let a = ix.insert(root, b"a.rs", file());
        assert_eq!(ix.lookup(root, b"a.rs"), Some(a));
        assert_eq!(ix.lookup(root, b"b.rs"), None);
        assert!(ix.remove(a));
        assert_eq!(ix.lookup(root, b"a.rs"), None);
        assert!(!ix.is_alive(a));
        assert_eq!(ix.live_count(), 1); // root only
    }

    #[test]
    fn upsert_preserves_id() {
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"p", dir().with_root_dir(true));
        let (id1, created1) = ix.upsert(root, b"x", file());
        let (id2, created2) = ix.upsert(root, b"x", dir());
        assert!(created1 && !created2);
        assert_eq!(id1, id2, "rescan must keep ids stable");
        assert!(ix.is_dir(id1));
    }

    #[test]
    fn revision_of_tombstone_keeps_id() {
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"p", dir().with_root_dir(true));
        let (id, _) = ix.upsert(root, b"x", file());
        ix.remove(id);
        assert_eq!(ix.lookup(root, b"x"), None);
        let (id2, _) = ix.upsert(root, b"x", file());
        assert_ne!(id, id2, "a new file after delete gets a fresh id");
        ix.remove(id2);
        ix.revive(id2, file());
        assert_eq!(ix.lookup(root, b"x"), Some(id2));
    }

    #[test]
    fn paths_materialize() {
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"proj", dir().with_root_dir(true));
        let sub = ix.insert(root, b"src", dir());
        let f = ix.insert(sub, b"main.rs", file());
        // The root dir's own name is not repeated: `root_path` already names it.
        assert_eq!(
            ix.path_of(f, b"/home/me/proj"),
            Some(b"/home/me/proj/src/main.rs".to_vec())
        );
        // root at "/"
        assert_eq!(ix.path_of(f, b"/"), Some(b"/src/main.rs".to_vec()));
    }

    #[test]
    fn path_dropped_through_tombstoned_dir() {
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"p", dir().with_root_dir(true));
        let sub = ix.insert(root, b"gone", dir());
        let f = ix.insert(sub, b"x.txt", file());
        assert!(ix.path_of(f, b"/p").is_some());
        ix.remove(sub);
        assert!(
            ix.path_of(f, b"/p").is_none(),
            "results under a deleted dir are dropped"
        );
    }

    #[test]
    fn move_is_o1_and_keeps_children() {
        let mut ix = Index::new();
        let r1 = ix.insert(ROOT_ID, b"a", dir().with_root_dir(true));
        let r2 = ix.insert(ROOT_ID, b"b", dir().with_root_dir(true));
        let d = ix.insert(r1, b"d", dir());
        let f = ix.insert(d, b"f", file());
        ix.move_entry(d, r2, Some(b"renamed"));
        assert_eq!(ix.path_of(f, b"/b").unwrap(), b"/b/renamed/f".to_vec());
        assert_eq!(ix.children(d), vec![f]);
    }

    #[test]
    fn compaction_purges_and_remaps() {
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"p", dir().with_root_dir(true));
        let mut ids = vec![];
        for i in 0..5 {
            ids.push(ix.insert(root, format!("f{i}").as_bytes(), file()));
        }
        ix.remove(ids[0]);
        ix.remove(ids[2]);
        let before_live = ix.live_count();
        let (c, map) = ix.compacted();
        assert_eq!(c.live_count(), before_live);
        assert_eq!(c.entry_count(), before_live as usize + 1);
        assert_eq!(c.entries.len(), 5); // slot0 + root + 3 live files
                                        // parent links intact for every live entry
        for id in 1..c.entries.len() as u32 {
            assert!(
                c.chain_ok(id),
                "entry {id} lost its root chain after compaction"
            );
        }
        // ids are remapped but reachability and names survive
        let f1 = c.lookup(1, b"f1").expect("f1 survived");
        assert_eq!(c.path_of(f1, b"/p").unwrap(), b"/p/f1".to_vec());

        // The map must describe that renumbering exactly, because anything keyed
        // by id outside the index (the ownership sidecar) is remapped through it.
        // Tombstoned ids map to 0 — "no slot" — and live ones to a real slot whose
        // name still matches. A map that silently dropped this would make the
        // access filter answer about the wrong files after every compaction.
        assert_eq!(map[ids[0] as usize], 0, "a removed id must map to no slot");
        assert_eq!(map[ids[2] as usize], 0);
        for live in [ids[1], ids[3], ids[4]] {
            let new = map[live as usize];
            assert_ne!(new, 0, "a surviving id must map to a slot");
            assert_eq!(c.name(new), ix.name(live), "the map must preserve identity");
        }
    }

    #[test]
    fn alive_bitvec_matches_tombstone_flags_and_stays_compact() {
        // Liveness must agree with the tombstone flag across insert/remove/
        // revive/apply_add, and the bitset must cost ~1 bit per entry, not
        // ~1 byte (the Vec<bool> it replaced).
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"p", dir().with_root_dir(true));
        let mut ids = Vec::new();
        for i in 0..200 {
            ids.push(ix.insert(root, format!("f{i:03}").as_bytes(), file()));
        }
        for id in ids.iter().step_by(3) {
            ix.remove(*id);
        }
        for (i, e) in ix.entries.iter().enumerate() {
            let expect = i != 0 && !e.flags().is_tombstone();
            assert_eq!(ix.is_alive(i as u32), expect, "id {i}");
        }
        ix.revive(ids[0], file());
        assert!(ix.is_alive(ids[0]));
        assert_eq!(ix.alive_bytes(), (ix.len() + 63) / 64 * 8);
        assert!(
            ix.alive_bytes() * 8 <= ix.len() + 64,
            "bitvec must be ~1 bit/entry, got {} bytes for {} entries",
            ix.alive_bytes(),
            ix.len()
        );
        // Reconstructed index derives the same bits from flags alone.
        let ix2 = Index::new_reconstructed(
            ix.names.clone(),
            ix.entries.clone(),
            ix.created_array().to_vec(),
        );
        for i in 0..ix.len() {
            assert_eq!(ix2.is_alive(i as u32), ix.is_alive(i as u32), "id {i}");
        }
    }

    #[test]
    fn root_of_and_flags_bits() {
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"p", dir().with_root_dir(true));
        let sub = ix.insert(root, b"s", dir());
        let f = ix.insert(sub, b"f", file().with_symlink(true));
        assert_eq!(ix.root_of(f), Some(root));
        assert!(ix.entry(f).unwrap().flags().is_symlink());
        assert!(!ix.entry(f).unwrap().flags().is_dir());
    }

    #[test]
    fn creation_minutes_are_parallel_and_survive_compaction() {
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"p", dir().with_root_dir(true));
        let dated = ix.insert(root, b"old.txt", file());
        let undated = ix.insert(root, b"unknown.txt", file());
        // 2026-01-02T00:00:00Z, in whole minutes.
        let minutes: u32 = 29_455_200;
        ix.set_created(dated, minutes);
        assert_eq!(ix.created_min(dated), Some(minutes));
        assert_eq!(ix.created_secs(dated), Some(minutes as i64 * 60));
        // A fresh entry is "unknown", never the epoch itself.
        assert_eq!(ix.created_min(undated), None);
        assert_eq!(
            ix.created_array()[undated as usize],
            super::CREATED_UNKNOWN,
            "unknown is its own sentinel, not minute 0"
        );
        // Minute 0 is a real, datable minute: the first minute of 1970.
        ix.set_created(undated, 0);
        assert_eq!(ix.created_min(undated), Some(0));
        assert_eq!(ix.created_secs(undated), Some(0));
        assert_eq!(ix.created_latest_secs(undated), Some(59));
        assert_eq!(ix.created_array().len(), ix.len(), "parallel to entries");

        // Compaction renumbers ids; the time must follow the file, not the slot.
        let (c, map) = ix.compacted();
        let moved = map[dated as usize];
        assert_eq!(c.created_min(moved), Some(minutes));
        assert_eq!(c.created_secs(moved), Some(minutes as i64 * 60));
        // A tombstoned id maps to no slot.
        ix.remove(undated);
        let (c2, map2) = ix.compacted();
        assert_eq!(map2[undated as usize], 0);
        assert_eq!(c2.created_min(moved), Some(minutes));
    }

    /// The two conversions are inverses at the boundaries that matter, and the
    /// scale has no second opinion: what the crawler stores is what a query
    /// compares and what the output boundary prints.
    #[test]
    fn creation_minutes_round_trip_across_the_edges() {
        use std::time::{Duration, UNIX_EPOCH};
        // Whole minutes only, and the truncation boundary is 60 s, not "about".
        assert_eq!(super::created_minutes_at(UNIX_EPOCH), 0, "the epoch is minute 0");
        assert_eq!(
            super::created_minutes_at(UNIX_EPOCH + Duration::from_secs(59)),
            0,
            "59 s truncates down to minute 0"
        );
        assert_eq!(
            super::created_minutes_at(UNIX_EPOCH + Duration::from_secs(60)),
            1,
            "exactly 60 s is minute 1"
        );
        assert_eq!(
            super::created_minutes_at(UNIX_EPOCH + Duration::from_secs(119)),
            1
        );
        // Before the epoch cannot be represented; it reads as unknown rather
        // than wrapping to a huge minute — and "unknown" is not minute 0, so a
        // file from 1969 never claims to be from 1970.
        assert_eq!(
            super::created_minutes_at(UNIX_EPOCH - Duration::from_secs(1)),
            super::CREATED_UNKNOWN
        );
        // Far future saturates below the sentinel instead of wrapping: minute
        // u32::MAX - 1 is year 10135, well past any timestamp a filesystem
        // reports, and saturation can never read back as "unknown".
        assert_eq!(
            super::created_minutes_at(UNIX_EPOCH + Duration::from_secs(u64::MAX / 2)),
            super::CREATED_LATEST_MINUTE
        );
        assert_ne!(super::CREATED_LATEST_MINUTE, super::CREATED_UNKNOWN);
        // The first minute of 1970 is representable and distinct from unknown:
        // this is the collision the old `0`-means-unknown rule had.
        assert_eq!(
            super::created_minutes_at(UNIX_EPOCH + Duration::from_secs(1)),
            0
        );
        // And back out again: the stored minute is the start of that minute,
        // while a lower bound compares its last second.
        assert_eq!(super::created_secs_from_min(0), 0);
        assert_eq!(super::created_latest_secs_from_min(0), 59);
        assert_eq!(super::created_latest_secs_from_min(1), 119);
        assert_eq!(super::created_secs_from_min(1), 60);
        assert_eq!(super::created_secs_from_min(29_455_200), 1_767_312_000);
        assert_eq!(
            super::created_secs_from_min(u32::MAX),
            u32::MAX as i64 * 60,
            "the top of the range converts without overflowing i64"
        );
    }

    #[test]
    fn reconstruction_pads_missing_creation_array() {
        // A snapshot from a format without creation times decodes with none:
        // the slot count must still match, and every read reports "unknown".
        let mut ix = Index::new();
        let root = ix.insert(ROOT_ID, b"p", dir().with_root_dir(true));
        ix.insert(root, b"a", file());
        let ix2 = Index::new_reconstructed(ix.names.clone(), ix.entries.clone(), Vec::new());
        assert_eq!(ix2.created_array().len(), ix2.len());
        for id in 0..ix2.len() as u32 {
            assert_eq!(ix2.created_min(id), None);
        }
    }

    /// Reference implementation: the whole-index scan that `lookup` and
    /// `children` used to do. Kept here so every accelerated answer can be
    /// checked against it after any sequence of mutations.
    fn brute_lookup(ix: &Index, parent: u32, name: &[u8]) -> Option<u32> {
        (1..ix.len() as u32).find(|&id| {
            ix.is_alive(id) && ix.entries[id as usize].parent == parent && ix.name(id) == name
        })
    }

    fn brute_children(ix: &Index, parent: u32) -> Vec<u32> {
        (1..ix.len() as u32)
            .filter(|&id| ix.is_alive(id) && ix.entries[id as usize].parent == parent)
            .collect()
    }

    fn assert_child_index_consistent(ix: &Index) {
        for parent in 0..ix.len() as u32 {
            assert_eq!(
                ix.children(parent),
                brute_children(ix, parent),
                "children of {parent}"
            );
            for id in ix.children(parent) {
                let name = ix.name(id).to_vec();
                assert_eq!(
                    ix.lookup(parent, &name),
                    brute_lookup(ix, parent, &name),
                    "lookup of {:?} under {parent}",
                    String::from_utf8_lossy(&name)
                );
            }
        }
        assert_eq!(ix.lookup(ROOT_ID, b"definitely-not-indexed"), None);
    }

    /// The child index must agree with the entries themselves after every kind
    /// of mutation — insert, remove, revive, move (rename + reparent), a replay
    /// re-add over a live slot, compaction, and a decoded snapshot.
    #[test]
    fn child_index_survives_every_mutation() {
        let mut ix = Index::new();
        let r1 = ix.insert(ROOT_ID, b"a", dir().with_root_dir(true));
        let r2 = ix.insert(ROOT_ID, b"b", dir().with_root_dir(true));
        let d1 = ix.insert(r1, b"d1", dir());
        let mut ids = Vec::new();
        for i in 0..40 {
            ids.push(ix.insert(d1, format!("f{i}").as_bytes(), file()));
        }
        assert_eq!(ix.lookup(d1, b"f7"), Some(ids[7]));
        assert_eq!(ix.children(d1).len(), 40);

        // remove + revive keep the id addressable, and only that id.
        assert!(ix.remove(ids[3]));
        assert_eq!(ix.lookup(d1, b"f3"), None);
        assert_eq!(ix.children(d1).len(), 39);
        ix.revive(ids[3], file());
        assert_eq!(ix.lookup(d1, b"f3"), Some(ids[3]));
        assert_eq!(ix.children(d1).len(), 40);

        // A rename + reparent moves exactly one link.
        let sub = ix.insert(d1, b"sub", dir());
        ix.move_entry(sub, r2, Some(b"moved"));
        assert_eq!(ix.lookup(d1, b"sub"), None);
        assert_eq!(ix.lookup(r2, b"moved"), Some(sub));
        assert_eq!(ix.children(d1).len(), 40);
        assert!(!ix.children(r2).contains(&sub) || ix.lookup(r2, b"moved") == Some(sub));

        // A journal re-add over a *live* slot (reparent) and over a tombstone.
        let x = ix.insert(d1, b"x", file());
        ix.apply_add(x, r2, b"x", file());
        assert_eq!(ix.lookup(d1, b"x"), None);
        assert_eq!(ix.lookup(r2, b"x"), Some(x));
        ix.remove(ids[9]);
        ix.apply_add(ids[9], d1, b"f9", file());
        assert_eq!(ix.lookup(d1, b"f9"), Some(ids[9]));

        assert_child_index_consistent(&ix);

        // Compaction rebuilds ids through `insert`; a decoded snapshot must
        // rebuild the child index from the entries array alone.
        let (c, _) = ix.compacted();
        assert_child_index_consistent(&c);
        let decoded =
            Index::new_reconstructed(ix.names.clone(), ix.entries.clone(), ix.created_array().to_vec());
        assert_child_index_consistent(&decoded);
    }
}
