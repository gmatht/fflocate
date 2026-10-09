//! The index data model (§3).
//!
//! ```text
//! struct Entry {          // 12 bytes
//!     name_off: u32,      // offset into names arena
//!     name_len: u16,
//!     parent: u32,        // ROOT = 0
//!     flags: u8,          // IS_DIR | IS_SYMLINK | TOMBSTONE
//! }
//! ```
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
    pub fn with_dir(mut self, v: bool) -> Self {
        self.set(flags::IS_DIR, v);
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

/// The in-memory index.
///
/// Ids index directly into `entries`; slot 0 is the virtual root and is never
/// a real file. `alive[i]` is a cheap side array mirroring the tombstone bit so
/// the query engine can iterate without branch-heavy flag reads.
#[derive(Clone, Debug)]
pub struct Index {
    /// names arena: all names concatenated, back to back, no separators.
    pub names: Vec<u8>,
    pub entries: Vec<Entry>,
    /// parallel liveness bitset (1 = not tombstoned).
    alive: Vec<bool>,
    /// next id to hand out; never reused.
    next_id: u32,
    /// generation, bumped on snapshot load / full-root rescan / compaction (§3).
    generation: u64,
    /// Last applied journal sequence number (the "clock").
    last_lsn: u64,
    /// Number of non-tombstoned entries (excluding slot 0).
    live_count: u64,
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
            alive: Vec::new(),
            next_id: 1,
            generation: 1,
            last_lsn: 0,
            live_count: 0,
        };
        // Slot 0: the virtual root. Unnameable, always dead to queries.
        idx.entries.push(Entry::default());
        idx.alive.push(false);
        idx
    }

    /// Rebuild an index from decoded snapshot arrays. Derives the liveness
    /// bitset and live count from the tombstone flags.
    pub fn new_reconstructed(names: Vec<u8>, entries: Vec<Entry>) -> Self {
        let mut alive = Vec::with_capacity(entries.len());
        let mut live_count = 0u64;
        for (i, e) in entries.iter().enumerate() {
            let a = i != 0 && !e.flags().is_tombstone();
            if a {
                live_count += 1;
            }
            alive.push(a);
        }
        Index {
            names,
            entries,
            alive,
            next_id: 1,
            generation: 1,
            last_lsn: 0,
            live_count,
        }
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

    pub fn is_alive(&self, id: u32) -> bool {
        self.alive.get(id as usize).copied().unwrap_or(false)
    }

    pub fn is_dir(&self, id: u32) -> bool {
        self.is_alive(id) && self.entries[id as usize].flags().is_dir()
    }

    /// Find a live child of `parent` with `name` (exact byte match).
    ///
    /// Linear scan over the sibling chain. For directories with very many
    /// children this is O(children); a v2 accelerator (per-dir hash index) can
    /// replace it without changing callers.
    pub fn lookup(&self, parent: u32, name: &[u8]) -> Option<u32> {
        for id in 0..self.entries.len() {
            let e = &self.entries[id];
            if e.parent != parent || e.name_len as usize != name.len() {
                continue;
            }
            if !self.alive[id] {
                continue;
            }
            if self.name_of_entry(e) == name {
                return Some(id as u32);
            }
        }
        None
    }

    /// Create a brand-new entry and return its (fresh) id. The parent must
    /// exist and be live; callers ensure that.
    pub fn insert(&mut self, parent: u32, name: &[u8], flags: EntryFlags) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        self.entries.push(Entry {
            name_off: off,
            name_len: name.len() as u16,
            parent,
            flags: flags.0,
        });
        self.alive.push(!flags.is_tombstone());
        if !flags.is_tombstone() {
            self.live_count += 1;
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
        let was = self.alive[id as usize];
        self.entries[id as usize].flags |= flags::TOMBSTONE;
        self.alive[id as usize] = false;
        if was {
            self.live_count -= 1;
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
                self.entries.push(Entry { name_off: 0, name_len: 0, parent: ROOT_ID, flags: flags::TOMBSTONE });
                self.alive.push(false);
            }
        }
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        let e = &mut self.entries[id as usize];
        e.name_off = off;
        e.name_len = name.len() as u16;
        e.parent = parent;
        e.flags = flags.with_tombstone(false).0;
        if !self.alive[id as usize] {
            self.alive[id as usize] = true;
            self.live_count += 1;
        }
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
        let e = &mut self.entries[id as usize];
        e.flags = flags.with_tombstone(false).0;
        if !self.alive[id as usize] {
            self.alive[id as usize] = true;
            self.live_count += 1;
        }
    }

    /// Live children of `parent` as ids. Used by rescan diffing and by
    /// directory-rename moves.
    pub fn children(&self, parent: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for id in 0..self.entries.len() {
            if id as u32 != ROOT_ID && self.entries[id].parent == parent && self.alive[id] {
                out.push(id as u32);
            }
        }
        out
    }

    /// Move a whole directory subtree: re-point the moved entry's parent (and
    /// optionally rename it). O(1) — children are found via their parent id.
    pub fn move_entry(&mut self, id: u32, new_parent: u32, new_name: Option<&[u8]>) {
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
            if cur as usize >= self.entries.len() || !self.alive[cur as usize] {
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
        self.materialize_into(id, root_path, &mut buf).map(|s| s.to_vec())
    }

    // ------------------------------------------------------------- compaction

    /// Build a fresh, compacted copy of this index: tombstones purged, ids
    /// renumbered densely, names arena rebuilt. Live ids are remapped in a
    /// stable (ascending) order so relationships survive.
    ///
    /// Returns the new index; the caller swaps it in under the writer lock and
    /// bumps the generation (§9).
    pub fn compacted(&self) -> Index {
        let mut out = Index::new();
        out.generation = self.generation;
        out.last_lsn = self.last_lsn;
        // map old id -> new id
        let mut map: Vec<u32> = vec![0; self.entries.len()];
        // Two passes: first allocate all new slots preserving order, then fix
        // parents (parents always have a smaller index than children because we
        // only ever create a child after its parent).
        for old in 1..self.entries.len() {
            if !self.alive[old] {
                continue;
            }
            let e = self.entries[old];
            let new_parent = map[e.parent as usize];
            let name = self.name_of_entry(&e).to_vec();
            let id = out.insert(new_parent, &name, EntryFlags(e.flags));
            map[old] = id;
        }
        out.set_next_id(self.next_id);
        out
    }

    // ------------------------------------------------------------- iteration

    /// Iterate live entries with their ids.
    pub fn iter_live(&self) -> impl Iterator<Item = (u32, &Entry)> {
        self.entries
            .iter()
            .enumerate()
            .skip(1)
            .filter(move |(i, _)| self.alive[*i])
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
            if cur as usize >= self.entries.len() || !self.alive[cur as usize] {
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
            .filter(|(i, e)| self.alive[*i] && e.flags().is_root_dir())
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
            if !self.alive[cur as usize] {
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
        assert_eq!(ix.path_of(f, b"/home/me/proj"), Some(b"/home/me/proj/src/main.rs".to_vec()));
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
        assert!(ix.path_of(f, b"/p").is_none(), "results under a deleted dir are dropped");
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
        let c = ix.compacted();
        assert_eq!(c.live_count(), before_live);
        assert_eq!(c.entry_count(), before_live as usize + 1);
        assert_eq!(c.entries.len(), 5); // slot0 + root + 3 live files
        // parent links intact for every live entry
        for id in 1..c.entries.len() as u32 {
            assert!(c.chain_ok(id), "entry {id} lost its root chain after compaction");
        }
        // ids are remapped but reachability and names survive
        let f1 = c.lookup(1, b"f1").expect("f1 survived");
        assert_eq!(c.path_of(f1, b"/p").unwrap(), b"/p/f1".to_vec());
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
}
