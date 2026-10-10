//! Persistence wiring (§9): the store owns the snapshot file and the journal,
//! and knows how to load, replay, and compact them.
//!
//! Layout inside the versioned state dir:
//!
//! ```text
//! <state>/<format-version>/snapshot.fnx
//! <state>/<format-version>/journal.log
//! ```

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::drive::DriveId;
use crate::index::Index;
use crate::journal::{Journal, ReplayOutcome, Record};
use crate::roots::Root;
use crate::snapshot;

/// Store ownership stamp (§15): which format, OS and backend wrote this
/// store, whether cross-OS sharing is opted in, and the per-root drive map.
/// Lives beside the snapshot/journal as `store.meta` (JSON: tiny, read once
/// per boot) so the snapshot format never churns for metadata — the same
/// sidecar precedent as `ownership.own`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct StoreStamp {
    /// `findex::config_paths::FORMAT_VERSION` of the writer.
    pub format_version: u32,
    /// `std::env::consts::OS` that created the store.
    pub created_by_os: String,
    /// `std::env::consts::OS` that last opened it for writing.
    pub last_writer_os: String,
    /// Resolved backend name (`inotify`, `fanotify`, `fake`).
    pub backend: String,
    /// Operator opted into cross-OS sharing (`--shared-store`).
    pub shared: bool,
    /// Root path → last recorded identity and owner.
    pub drives: BTreeMap<String, RecordedRoot>,
}

/// A root's recorded identity, for drive-change detection and foreign-root
/// rules (§15). `owner_os` is `None` only for hand-written stamps in tests.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RecordedRoot {
    pub drive: Option<DriveId>,
    pub owner_os: Option<String>,
}

/// The open-time gate decision (§15). Version checks run first: a store from
/// a newer format refuses to start rather than recrawling over data it
/// cannot read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenGate {
    /// Proceed. `handoff` is true when the last writer was another OS (only
    /// possible with sharing on): bump the generation (invalidating stale
    /// cursors, §9) and rescan local roots.
    Open { handoff: bool },
    /// Refuse to start; the message explains how to proceed.
    Refuse(String),
}

pub fn current_os() -> &'static str {
    std::env::consts::OS
}

/// Pure gate logic: `stamp` is `None` for legacy (unstamped) stores.
/// `current_os` and `shared_flag` are parameters (not read from the
/// environment) so the matrix is unit-testable with fictitious OS names.
pub fn check_gate(
    stamp: Option<&StoreStamp>,
    snapshot_version: Option<u32>,
    current_os: &str,
    shared_flag: bool,
) -> OpenGate {
    // 1. Version first: never mutate what a newer format wrote.
    if let Some(v) = snapshot_version {
        if v > crate::config_paths::FORMAT_VERSION {
            return OpenGate::Refuse(format!(
                "store snapshot version {v} is newer than supported {} (written by a newer findex?); refusing to open",
                crate::config_paths::FORMAT_VERSION
            ));
        }
    }
    let Some(stamp) = stamp else {
        // Legacy store, no stamp: adopt exactly as today (zero behaviour
        // change for existing users). Cross-OS moves of *legacy* stores are
        // the one unprotected case — documented in DESIGN §15.
        return OpenGate::Open { handoff: false };
    };
    if stamp.format_version > crate::config_paths::FORMAT_VERSION {
        return OpenGate::Refuse(format!(
            "store stamp version {} is newer than supported {} (written by a newer findex?); refusing to open",
            stamp.format_version,
            crate::config_paths::FORMAT_VERSION
        ));
    }
    // 2. Ownership: a foreign store needs the explicit opt-in.
    if stamp.last_writer_os != current_os && !shared_flag {
        return OpenGate::Refuse(format!(
            "store last written by '{}' (this is '{}'); refusing to open a foreign store — use --shared-store to adopt it, or a different state dir",
            stamp.last_writer_os, current_os
        ));
    }
    OpenGate::Open { handoff: stamp.last_writer_os != current_os }
}

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
    pub fn stamp_path(dir: &Path) -> PathBuf {
        dir.join("store.meta")
    }

    /// Read the ownership stamp, if any. A missing or corrupt stamp means a
    /// legacy store, not a fatal error (the gate treats it as adoptable).
    pub fn read_stamp(dir: &Path) -> Option<StoreStamp> {
        let bytes = std::fs::read(Self::stamp_path(dir)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Write the stamp atomically (tmp file + rename, like snapshots).
    pub fn write_stamp(dir: &Path, stamp: &StoreStamp) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(stamp)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let tmp = dir.join("store.meta.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, Self::stamp_path(dir))?;
        Ok(())
    }

    /// Snapshot format version without decoding. `Ok(None)` when there is no
    /// snapshot; `Err` on a bad header (the legacy corrupt path recrawls).
    pub fn peek_snapshot_version(dir: &Path) -> io::Result<Option<u32>> {
        snapshot::peek_version(&Self::snapshot_path(dir))
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

    fn stamp(os: &str, shared: bool) -> StoreStamp {
        StoreStamp {
            format_version: crate::config_paths::FORMAT_VERSION,
            created_by_os: os.to_string(),
            last_writer_os: os.to_string(),
            backend: "fake".into(),
            shared,
            drives: BTreeMap::new(),
        }
    }

    #[test]
    fn stamp_roundtrips() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(Store::read_stamp(d.path()), None, "no stamp yet");
        let mut s = stamp("linux", true);
        s.drives.insert(
            "/mnt/d".into(),
            RecordedRoot {
                drive: Some(DriveId {
                    disk_serial: Some("WD-1".into()),
                    volume_uuid: None,
                }),
                owner_os: Some("linux".into()),
            },
        );
        Store::write_stamp(d.path(), &s).unwrap();
        assert_eq!(Store::read_stamp(d.path()), Some(s));
        // Corrupt bytes read as legacy, never fatal.
        std::fs::write(Store::stamp_path(d.path()), b"{nope").unwrap();
        assert_eq!(Store::read_stamp(d.path()), None);
    }

    #[test]
    fn gate_matrix() {
        use OpenGate::*;
        // Fresh store: open, no handoff.
        assert_eq!(check_gate(None, None, "linux", false), Open { handoff: false });
        // Same OS, with or without the flag: open, no handoff.
        let s = stamp("linux", false);
        assert_eq!(check_gate(Some(&s), Some(2), "linux", false), Open { handoff: false });
        assert_eq!(check_gate(Some(&s), Some(2), "linux", true), Open { handoff: false });
        // Foreign without the flag: refuse.
        let s = stamp("windows", false);
        assert!(
            matches!(check_gate(Some(&s), Some(2), "linux", false), Refuse(_)),
            "foreign store without --shared-store must refuse"
        );
        // Foreign with the flag: open with handoff.
        assert_eq!(
            check_gate(Some(&s), Some(2), "linux", true),
            Open { handoff: true }
        );
        // Version first: a newer snapshot refuses even with the flag and a
        // matching OS — never mutate what you cannot read.
        let s = stamp("linux", true);
        let newer = crate::config_paths::FORMAT_VERSION + 1;
        assert!(
            matches!(check_gate(Some(&s), Some(newer), "linux", true), Refuse(_)),
            "newer snapshot version must refuse"
        );
        assert!(
            matches!(check_gate(None, Some(newer), "linux", true), Refuse(_)),
            "newer snapshot version must refuse even unstamped"
        );
        // Older snapshots keep the legacy recrawl path (no refusal).
        assert_eq!(check_gate(None, Some(1), "linux", false), Open { handoff: false });
    }

    #[test]
    fn peek_snapshot_version_behaves() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(Store::peek_snapshot_version(d.path()).unwrap(), None, "no file");
        // A real snapshot reports its version.
        let ix = Index::new();
        let roots = vec![];
        Store::open(d.path()).unwrap();
        crate::snapshot::write_atomic(&Store::snapshot_path(d.path()), &ix, &roots).unwrap();
        assert_eq!(
            Store::peek_snapshot_version(d.path()).unwrap(),
            Some(crate::snapshot::VERSION)
        );
        // Bad magic is corruption (legacy path), not a version.
        std::fs::write(Store::snapshot_path(d.path()), b"NOPE0000junk").unwrap();
        assert!(Store::peek_snapshot_version(d.path()).is_err());
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
