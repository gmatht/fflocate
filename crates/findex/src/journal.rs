//! The journal (§9): an append-only log of index mutations, and simultaneously
//! the clock that cursors are expressed against.
//!
//! ```text
//! [op u8][len u32][crc32][payload]
//! ```
//!
//! `LSN = monotonic record index`. Every record is CRC'd on its payload so a
//! torn tail bounds the damage: we keep the records that verify, stop at the
//! first one that does not, and let the caller enqueue a rescan. Records after
//! a bad one are unreachable by construction (we stop at the first failure) —
//! a later valid record cannot be recovered, and pretending otherwise would
//! mean silently skipping mutations.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::util::crc32;

/// Journal opcodes. ADD/DEL/MOVE cover the whole namespace story (§1: content
/// writes generate zero index work). FENCE and MARK are markers used by
/// compaction and sync.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Add = 1,
    Del = 2,
    Move = 3,
    Fence = 4,
    Mark = 5,
}

impl OpKind {
    pub fn from_u8(b: u8) -> Option<OpKind> {
        Some(match b {
            1 => OpKind::Add,
            2 => OpKind::Del,
            3 => OpKind::Move,
            4 => OpKind::Fence,
            5 => OpKind::Mark,
            _ => return None,
        })
    }
}

/// A decoded journal record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    /// Insert/upsert a child under a parent. `name` is raw bytes (never UTF-8
    /// coerced). `id` is the id the applier assigned.
    Add { id: u32, parent: u32, name: Vec<u8>, flags: u8 },
    /// Tombstone an entry by id.
    Del { id: u32 },
    /// Move/rename an entry: new parent, new name.
    Move { id: u32, new_parent: u32, new_name: Vec<u8> },
    /// A fence marker; carries no payload. Used to bound replay when a
    /// snapshot was written concurrently.
    Fence,
    /// An arbitrary marker (e.g. compaction generation note).
    Mark { tag: u64 },
}

impl Record {
    pub fn kind(&self) -> OpKind {
        match self {
            Record::Add { .. } => OpKind::Add,
            Record::Del { .. } => OpKind::Del,
            Record::Move { .. } => OpKind::Move,
            Record::Fence => OpKind::Fence,
            Record::Mark { .. } => OpKind::Mark,
        }
    }

    fn encode_payload(&self) -> Vec<u8> {
        let mut p = Vec::new();
        match self {
            Record::Add { id, parent, name, flags } => {
                p.extend_from_slice(&id.to_le_bytes());
                p.extend_from_slice(&parent.to_le_bytes());
                p.push(*flags);
                p.extend_from_slice(&(name.len() as u32).to_le_bytes());
                p.extend_from_slice(name);
            }
            Record::Del { id } => p.extend_from_slice(&id.to_le_bytes()),
            Record::Move { id, new_parent, new_name } => {
                p.extend_from_slice(&id.to_le_bytes());
                p.extend_from_slice(&new_parent.to_le_bytes());
                p.extend_from_slice(&(new_name.len() as u32).to_le_bytes());
                p.extend_from_slice(new_name);
            }
            Record::Fence => {}
            Record::Mark { tag } => p.extend_from_slice(&tag.to_le_bytes()),
        }
        p
    }

    fn decode_payload(kind: OpKind, p: &[u8]) -> Option<Record> {
        let mut r = ByteReader::new(p);
        Some(match kind {
            OpKind::Add => {
                let id = r.u32()?;
                let parent = r.u32()?;
                let flags = r.u8()?;
                let nlen = r.u32()? as usize;
                let name = r.bytes(nlen)?;
                Record::Add { id, parent, name: name.to_vec(), flags }
            }
            OpKind::Del => Record::Del { id: r.u32()? },
            OpKind::Move => {
                let id = r.u32()?;
                let new_parent = r.u32()?;
                let nlen = r.u32()? as usize;
                let new_name = r.bytes(nlen)?;
                Record::Move { id, new_parent, new_name: new_name.to_vec() }
            }
            OpKind::Fence => Record::Fence,
            OpKind::Mark => Record::Mark { tag: r.u64()? },
        })
    }
}

struct ByteReader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> ByteReader<'a> {
    fn new(b: &'a [u8]) -> Self {
        ByteReader { b, i: 0 }
    }
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.i)?;
        self.i += 1;
        Some(v)
    }
    fn u32(&mut self) -> Option<u32> {
        let end = self.i + 4;
        let s = self.b.get(self.i..end)?;
        self.i = end;
        Some(u32::from_le_bytes(s.try_into().unwrap()))
    }
    fn u64(&mut self) -> Option<u64> {
        let end = self.i + 8;
        let s = self.b.get(self.i..end)?;
        self.i = end;
        Some(u64::from_le_bytes(s.try_into().unwrap()))
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.i + n;
        let s = self.b.get(self.i..end)?;
        self.i = end;
        Some(s)
    }
}

/// An append-only journal file handle plus its in-memory high-water mark.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    file: File,
    /// Number of records successfully written (the current LSN high-water).
    count: u64,
    /// Whether a compaction fence is pending; the writer refuses further
    /// appends until the snapshot is committed (keeps replay deterministic).
    fenced: bool,
}

impl Journal {
    pub fn open(path: &Path) -> io::Result<Journal> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let mut j = Journal { path: path.to_path_buf(), file, count: 0, fenced: false };
        j.rescan_len()?;
        Ok(j)
    }

    /// Create a fresh (truncated) journal.
    pub fn create(path: &Path) -> io::Result<Journal> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        Ok(Journal { path: path.to_path_buf(), file, count: 0, fenced: false })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn count(&self) -> u64 {
        self.count
    }
    pub fn is_fenced(&self) -> bool {
        self.fenced
    }
    pub fn clear_fence(&mut self) {
        self.fenced = false;
    }

    fn rescan_len(&mut self) -> io::Result<()> {
        let len = self.file.metadata()?.len();
        // Count records without decoding: scan headers only (op+len+crc).
        self.file.seek(SeekFrom::Start(0))?;
        let mut buf = Vec::with_capacity(len as usize);
        self.file.read_to_end(&mut buf)?;
        self.count = count_records(&buf).0;
        Ok(())
    }

    /// Append a record, returning the LSN it was assigned (1-based).
    ///
    /// Writes are `write_all` + no implicit flush; callers that care about
    /// crash-durability before a sync point call [`Journal::sync`].
    pub fn append(&mut self, rec: &Record) -> io::Result<u64> {
        if self.fenced {
            return Err(io::Error::other("journal is fenced pending compaction"));
        }
        let payload = rec.encode_payload();
        let op = rec.kind() as u8;
        let mut hdr = [0u8; 9];
        hdr[0] = op;
        hdr[1..5].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        hdr[5..9].copy_from_slice(&crc32(&payload).to_le_bytes());
        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&hdr)?;
        self.file.write_all(&payload)?;
        self.count += 1;
        Ok(self.count)
    }

    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }

    /// Replay all intact records in order, invoking `f` with `(lsn, record)`.
    ///
    /// Returns the number of records replayed and, if the tail was corrupt, a
    /// boolean indicating that. A torn tail is not an error: the caller keeps
    /// serving and enqueues `RescanSubtree(cause=journal_corrupt)` (§9).
    pub fn replay<F: FnMut(u64, Record)>(&mut self, mut f: F) -> io::Result<ReplayOutcome> {
        let len = self.file.metadata()?.len();
        let mut buf = Vec::with_capacity(len as usize);
        self.file.seek(SeekFrom::Start(0))?;
        self.file.read_to_end(&mut buf)?;
        let (mut off, mut lsn) = (0usize, 0u64);
        let cap = buf.len();
        while off + 9 <= cap {
            let op = buf[off];
            let plen = u32::from_le_bytes(buf[off + 1..off + 5].try_into().unwrap()) as usize;
            let crc = u32::from_le_bytes(buf[off + 5..off + 9].try_into().unwrap());
            let pstart = off + 9;
            let pend = pstart + plen;
            if pend > cap {
                break; // truncated payload
            }
            let payload = &buf[pstart..pend];
            if crc32(payload) != crc {
                break; // torn/corrupt record: stop, don't try to resync
            }
            let Some(kind) = OpKind::from_u8(op) else { break };
            let Some(rec) = Record::decode_payload(kind, payload) else { break };
            lsn += 1;
            f(lsn, rec);
            off = pend;
        }
        self.count = lsn;
        let corrupt_tail = off != cap;
        Ok(ReplayOutcome { replayed: lsn, bytes_ok: off as u64, corrupt_tail })
    }

    /// Truncate the journal to zero records. Used after a snapshot is swapped
    /// in (§9 compaction).
    pub fn truncate(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.count = 0;
        self.fenced = false;
        Ok(())
    }

    /// Mark the journal fenced so that no more records can be appended. The
    /// writer calls this before writing a snapshot; if it crashes mid-snapshot
    /// the fence plus the snapshot's own LSN keep the state consistent. The
    /// fence is itself a record, so the LSN advances.
    pub fn fence(&mut self) -> io::Result<u64> {
        let lsn = self.append(&Record::Fence)?;
        self.sync()?;
        self.fenced = true;
        Ok(lsn)
    }
}

/// Outcome of a replay pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayOutcome {
    pub replayed: u64,
    pub bytes_ok: u64,
    pub corrupt_tail: bool,
}

/// Count well-formed records in a byte buffer (header-only scan, still CRC'd
/// against the payload so a torn mid-file record is not miscounted as good).
pub fn count_records(buf: &[u8]) -> (u64, bool) {
    let (mut off, mut n, cap) = (0usize, 0u64, buf.len());
    while off + 9 <= cap {
        let plen = u32::from_le_bytes(buf[off + 1..off + 5].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(buf[off + 5..off + 9].try_into().unwrap());
        let pstart = off + 9;
        let pend = pstart + plen;
        if pend > cap {
            return (n, true);
        }
        if crc32(&buf[pstart..pend]) != crc {
            return (n, true);
        }
        n += 1;
        off = pend;
    }
    (n, off != cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn append_replay_roundtrip() {
        let d = tmpdir();
        let p = d.path().join("journal.log");
        let mut j = Journal::create(&p).unwrap();
        assert_eq!(
            j.append(&Record::Add { id: 5, parent: 1, name: b"a.rs".to_vec(), flags: 0 }).unwrap(),
            1
        );
        assert_eq!(j.append(&Record::Del { id: 5 }).unwrap(), 2);
        assert_eq!(
            j.append(&Record::Move { id: 6, new_parent: 2, new_name: b"b".to_vec() }).unwrap(),
            3
        );
        assert_eq!(j.append(&Record::Fence).unwrap(), 4);
        assert_eq!(j.append(&Record::Mark { tag: 42 }).unwrap(), 5);
        j.sync().unwrap();

        let mut j2 = Journal::open(&p).unwrap();
        assert_eq!(j2.count(), 5);
        let mut seen = Vec::new();
        let out = j2.replay(|lsn, r| seen.push((lsn, r))).unwrap();
        assert_eq!(out.replayed, 5);
        assert!(!out.corrupt_tail);
        assert_eq!(seen.len(), 5);
        assert_eq!(seen[0].0, 1);
        assert_eq!(seen[4].1, Record::Mark { tag: 42 });
    }

    #[test]
    fn torn_tail_is_bounded() {
        let d = tmpdir();
        let p = d.path().join("journal.log");
        {
            let mut j = Journal::create(&p).unwrap();
            j.append(&Record::Del { id: 1 }).unwrap();
            j.append(&Record::Del { id: 2 }).unwrap();
            j.sync().unwrap();
        }
        // Corrupt the last record's payload byte in place.
        let mut f = OpenOptions::new().read(true).write(true).open(&p).unwrap();
        let len = f.metadata().unwrap().len();
        f.seek(SeekFrom::Start(len - 1)).unwrap();
        f.write_all(&[0xFF]).unwrap();
        drop(f);

        let mut j = Journal::open(&p).unwrap();
        let mut seen = Vec::new();
        let out = j.replay(|_, r| seen.push(r)).unwrap();
        assert!(out.corrupt_tail, "torn tail must be reported, not silently dropped");
        assert_eq!(seen, vec![Record::Del { id: 1 }]);
        assert_eq!(out.replayed, 1);
    }

    #[test]
    fn truncated_header_is_tolerated() {
        let d = tmpdir();
        let p = d.path().join("j.log");
        {
            let mut j = Journal::create(&p).unwrap();
            j.append(&Record::Fence).unwrap();
            j.sync().unwrap();
        }
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&[1, 2, 3]).unwrap(); // 3 stray bytes < header size
        drop(f);
        let mut j = Journal::open(&p).unwrap();
        let out = j.replay(|_, _| {}).unwrap();
        assert_eq!(out.replayed, 1);
        assert!(out.corrupt_tail);
    }

    #[test]
    fn fence_blocks_appends_until_cleared() {
        let d = tmpdir();
        let p = d.path().join("j.log");
        let mut j = Journal::create(&p).unwrap();
        j.fence().unwrap();
        assert!(j.append(&Record::Del { id: 9 }).is_err());
        j.clear_fence();
        assert!(j.append(&Record::Del { id: 9 }).is_ok());
    }

    #[test]
    fn truncate_resets_clock() {
        let d = tmpdir();
        let p = d.path().join("j.log");
        let mut j = Journal::create(&p).unwrap();
        j.append(&Record::Del { id: 1 }).unwrap();
        j.truncate().unwrap();
        assert_eq!(j.count(), 0);
        assert_eq!(j.append(&Record::Del { id: 2 }).unwrap(), 1);
    }

    #[test]
    fn non_utf8_names_survive() {
        let d = tmpdir();
        let p = d.path().join("j.log");
        let name = vec![0xff, 0xfe, b'a'];
        let mut j = Journal::create(&p).unwrap();
        j.append(&Record::Add { id: 3, parent: 1, name: name.clone(), flags: 0 }).unwrap();
        j.sync().unwrap();
        let mut j2 = Journal::open(&p).unwrap();
        let mut got = None;
        j2.replay(|_, r| got = Some(r)).unwrap();
        match got.unwrap() {
            Record::Add { name: n, .. } => assert_eq!(n, name),
            _ => panic!("wrong record"),
        }
    }
}
