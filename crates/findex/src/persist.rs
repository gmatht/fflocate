//! Persistence wiring (§9): the store owns the snapshot file and the journal,
//! and knows how to load, replay, and compact them.
//!
//! Layout inside the versioned state dir:
//!
//! ```text
//! <state>/<format-version>/snapshot.fnx
//! <state>/<format-version>/journal.log
//! ```

use std::io;
use std::path::{Path, PathBuf};

use crate::index::Index;
use crate::journal::{Journal, ReplayOutcome, Record};
use crate::roots::Root;
use crate::snapshot;

/// The persistent store: a directory containing one snapshot and one journal.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    pub journal: Journal,
}

/// What happened when the store was loaded. The daemon surfaces
/// `journal_corrupt` / `catching_up` from this (§9, §15).
#[derive(Clone, Debug, Default)]
pub struct LoadReport {
    pub had_snapshot: bool,
    pub replayed: u64,
    pub journal_corrupt: bool,
    pub catching_up: bool,
}

impl Store {
    pub fn snapshot_path(dir: &Path) -> PathBuf {
        dir.join("snapshot.fnx")
    }
    pub fn journal_path(dir: &Path) -> PathBuf {
        dir.join("journal.log")
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Open (or create) a store in `dir`.
    pub fn open(dir: &Path) -> io::Result<Store> {
        std::fs::create_dir_all(dir)?;
        let journal = Journal::open(&Self::journal_path(dir))?;
        Ok(Store { dir: dir.to_path_buf(), journal })
    }

    /// Load the snapshot if present. Returns `None` when there is none.
    pub fn load_snapshot(&self) -> io::Result<Option<snapshot::LoadedSnapshot>> {
        let p = Self::snapshot_path(&self.dir);
        if !p.exists() {
            return Ok(None);
        }
        match snapshot::load_mmap(&p) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                // A corrupt snapshot must not wedge startup: report and move on
                // (the caller will recrawl).
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Replay the journal onto `index`, calling `apply` for each record but
    /// *not* writing new journal entries (replay is not a new mutation).
    pub fn replay<F: FnMut(u64, &Record)>(
        &mut self,
        index: &mut Index,
        mut apply: F,
    ) -> io::Result<ReplayOutcome> {
        let start = index.last_lsn();
        let out = self.journal.replay(|lsn, rec| {
            if lsn > start {
                apply(lsn, &rec);
            }
        })?;
        index.set_last_lsn(out.replayed);
        Ok(out)
    }

    /// Write a snapshot of the current index+roots and truncate the journal.
    ///
    /// Compaction builds the fresh image off-thread in production (§9); this
    /// method just performs the durable swap under the writer lock.
    pub fn compact(&mut self, index: &Index, roots: &[Root]) -> io::Result<()> {
        let path = Self::snapshot_path(&self.dir);
        // 1. Write the snapshot atomically. It records the current last_lsn.
        snapshot::write_atomic(&path, index, roots)?;
        // 2. Truncate the journal only after the snapshot is durable, so a
        //    crash between the two leaves a snapshot that is a superset of the
        //    journal tail (replay is idempotent for adds, and DEL/MOVE of ids
        //    that no longer exist are no-ops).
        self.journal.truncate()?;
        self.journal.sync()?;
        Ok(())
    }

    /// Append a record and return its LSN.
    pub fn append(&mut self, rec: &Record) -> io::Result<u64> {
        self.journal.append(rec)
    }

    pub fn sync(&mut self) -> io::Result<()> {
        self.journal.sync()
    }
}

/// Apply a replayed record to an index. These are the same rules the live
/// applier uses, factored out so replay and live application cannot diverge.
pub fn apply_record(index: &mut Index, rec: &Record) {
    use crate::index::EntryFlags;
    match rec {
        Record::Add { id, parent, name, flags } => {
            index.apply_add(*id, *parent, name, EntryFlags(*flags));
        }
        Record::Del { id } => {
            index.remove(*id);
        }
        Record::Move { id, new_parent, new_name } => {
            index.move_entry(*id, *new_parent, Some(new_name));
        }
        Record::Fence | Record::Mark { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{EntryFlags, ROOT_ID};
    use crate::journal::Record;

    #[test]
    fn compact_then_replay_is_consistent() {
        let d = tempfile::tempdir().unwrap();
        let mut store = Store::open(d.path()).unwrap();
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"p", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        store.append(&Record::Add { id: r, parent: ROOT_ID, name: b"p".to_vec(), flags: EntryFlags::EMPTY.with_dir(true).with_root_dir(true).0 }).unwrap();
        let f = ix.insert(r, b"a", EntryFlags::EMPTY);
        store.append(&Record::Add { id: f, parent: r, name: b"a".to_vec(), flags: 0 }).unwrap();
        ix.set_last_lsn(store.journal.count());
        let roots = vec![];
        store.compact(&ix, &roots).unwrap();
        assert_eq!(store.journal.count(), 0);

        // Reopen: snapshot present, journal empty.
        let mut store2 = Store::open(d.path()).unwrap();
        let mut snap = store2.load_snapshot().unwrap().unwrap();
        assert_eq!(snap.index.entry_count(), ix.entry_count());
        let out = store2.replay(&mut snap.index, |_, _rec: &crate::journal::Record| {}).unwrap();
        assert_eq!(out.replayed, 0);
    }

    #[test]
    fn replay_after_compact_applies_tail() {
        let d = tempfile::tempdir().unwrap();
        let mut store = Store::open(d.path()).unwrap();
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"p", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        ix.set_last_lsn(store.journal.count());
        store.compact(&ix, &[]).unwrap();
        // Two tail records: add then del, at known ids.
        let a = ix.insert(r, b"a", EntryFlags::EMPTY);
        store.append(&Record::Add { id: a, parent: r, name: b"a".to_vec(), flags: 0 }).unwrap();
        store.append(&Record::Del { id: a }).unwrap();

        let mut store2 = Store::open(d.path()).unwrap();
        let mut snap = store2.load_snapshot().unwrap().unwrap().index;
        let out = store2.replay(&mut snap, |_, _rec| {}).unwrap();
        assert_eq!(out.replayed, 2);
        assert!(!out.corrupt_tail);
        // Now actually apply the tail: the add recreates `a`, the del removes it.
        let mut store3 = Store::open(d.path()).unwrap();
        let mut snap3 = store3.load_snapshot().unwrap().unwrap().index;
        let mut applied = Vec::new();
        store3.replay(&mut snap3, |lsn, rec| applied.push((lsn, rec.clone()))).unwrap();
        for (_, rec) in &applied {
            apply_record(&mut snap3, rec);
        }
        let r2 = snap3.lookup(ROOT_ID, b"p").unwrap();
        assert_eq!(snap3.lookup(r2, b"a"), None, "del replayed last → gone");
    }

    #[test]
    fn apply_record_is_idempotent() {
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"p", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        let rec = Record::Add { id: r, parent: ROOT_ID, name: b"p".to_vec(), flags: EntryFlags::EMPTY.with_dir(true).with_root_dir(true).0 };
        apply_record(&mut ix, &rec);
        let before = ix.live_count();
        apply_record(&mut ix, &rec);
        assert_eq!(ix.live_count(), before, "replaying an add twice must not duplicate");
    }
}
