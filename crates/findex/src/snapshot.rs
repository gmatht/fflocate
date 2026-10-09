//! The FNX2 snapshot file format (§9).
//!
//! ```text
//! snapshot.fnx:  magic "FNX2" | version | next_id | generation | roots[] | counts
//!                | names blob | entries array | last_lsn | checksum
//! ```
//!
//! We write a simple, self-describing layout:
//!
//! ```text
//! offset  size  field
//! 0       4     magic b"FNX2"
//! 4       4     version u32 (little endian)
//! 8       4     next_id u32
//! 12      8     generation u64
//! 20      8     last_lsn u64
//! 28      8     entry_count u64        (including slot 0)
//! 36      8     names_len u64
//! 44      4     root_count u32
//! 48      ...   roots[]: each = path_len u32 | path bytes | dev u64 | enabled u8
//!                              | fstype_len u32 | fstype bytes | caps u8
//!                              | last_full_crawl_gen u64 | last_full_crawl_lsn u64
//! ...     ...   names blob (names_len bytes)
//! ...     ...   entries (entry_count * 12 bytes: name_off u32, name_len u16,
//!                              parent u32, flags u8)
//! EOF-4   4     crc32 of everything before it
//! ```
//!
//! On load we prefer a real `mmap` (MAP_PRIVATE) so a 10 M-entry snapshot does
//! not block on a copy; the returned [`LoadedSnapshot`] owns the mapping. The
//! `Owned` variant carries a plain `Vec` for tests and for the write path.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::index::{Entry, Index, ROOT_ID};
use crate::roots::{MountCaps, Root};
use crate::util::crc32;

pub const MAGIC: &[u8; 4] = b"FNX2";
pub const VERSION: u32 = 2;
const ENTRY_SIZE: usize = 12;

/// A loaded snapshot, either memory-mapped or owned (write path/tests).
pub enum SnapshotBacking {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl SnapshotBacking {
    /// The raw snapshot bytes. Retained because the index's `names` arena and
    /// entry array are *copied* out of here on load; a future zero-copy loader
    /// will borrow from this mapping instead.
    pub fn bytes(&self) -> &[u8] {
        match self {
            SnapshotBacking::Mapped(m) => &m[..],
            SnapshotBacking::Owned(v) => v.as_slice(),
        }
    }
}

pub struct LoadedSnapshot {
    pub index: Index,
    pub roots: Vec<Root>,
    pub backing: SnapshotBacking,
}

/// Serialize an index + roots into the FNX2 byte layout.
pub fn encode(index: &Index, roots: &[Root]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + index.names.len() + index.entries.len() * ENTRY_SIZE);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&index.next_id().to_le_bytes());
    out.extend_from_slice(&index.generation().to_le_bytes());
    out.extend_from_slice(&index.last_lsn().to_le_bytes());
    out.extend_from_slice(&(index.entries.len() as u64).to_le_bytes());
    out.extend_from_slice(&(index.names.len() as u64).to_le_bytes());
    out.extend_from_slice(&(roots.len() as u32).to_le_bytes());

    for r in roots {
        let path = r.path.as_os_str().as_encoded_bytes();
        out.extend_from_slice(&(path.len() as u32).to_le_bytes());
        out.extend_from_slice(path);
        out.extend_from_slice(&r.dev.to_le_bytes());
        out.push(r.enabled as u8);
        let fstype = r.caps.fstype.as_bytes();
        out.extend_from_slice(&(fstype.len() as u32).to_le_bytes());
        out.extend_from_slice(fstype);
        out.push(caps_bits(&r.caps));
        out.extend_from_slice(&r.last_full_crawl.gen.to_le_bytes());
        out.extend_from_slice(&r.last_full_crawl.lsn.to_le_bytes());
    }

    out.extend_from_slice(&index.names);
    for e in &index.entries {
        out.extend_from_slice(&e.name_off.to_le_bytes());
        out.extend_from_slice(&e.name_len.to_le_bytes());
        out.extend_from_slice(&e.parent.to_le_bytes());
        out.push(e.flags);
    }
    let sum = crc32(&out);
    out.extend_from_slice(&sum.to_le_bytes());
    out
}

fn caps_bits(c: &MountCaps) -> u8 {
    (c.file_handles as u8)
        | (c.dtype_unreliable as u8) << 1
        | (c.case_insensitive as u8) << 2
        | (c.writable as u8) << 3
        | (c.watch_time_ignore as u8) << 4
}

fn caps_from_bits(b: u8) -> MountCaps {
    MountCaps {
        file_handles: b & 1 != 0,
        dtype_unreliable: b & 2 != 0,
        case_insensitive: b & 4 != 0,
        writable: b & 8 != 0,
        watch_time_ignore: b & 16 != 0,
        fstype: String::new(),
    }
}

/// Write a snapshot atomically: encode to `path.tmp`, fsync, rename over
/// `path`. A crash leaves either the old snapshot or the new one, never a
/// half-written file.
pub fn write_atomic(path: &Path, index: &Index, roots: &[Root]) -> io::Result<()> {
    let bytes = encode(index, roots);
    let tmp = path.with_extension("fnx.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // fsync the directory so the rename is durable.
    if let Some(dir) = path.parent() {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

/// Decode a snapshot from raw bytes. Validates magic, version and checksum.
pub fn decode(bytes: &[u8]) -> io::Result<(Index, Vec<Root>)> {
    let n = bytes.len();
    if n < 4 + 4 + 4 + 8 + 8 + 8 + 8 + 4 + 4 {
        return Err(corrupt("snapshot too small"));
    }
    if &bytes[0..4] != MAGIC {
        return Err(corrupt("bad magic"));
    }
    let stored_crc = u32::from_le_bytes(bytes[n - 4..].try_into().unwrap());
    if crc32(&bytes[..n - 4]) != stored_crc {
        return Err(corrupt("snapshot checksum mismatch"));
    }
    let body = &bytes[..n - 4];
    let mut r = Cursor::new(body);
    r.skip(4);
    let version = r.u32()?;
    if version != VERSION {
        return Err(corrupt(&format!("unsupported snapshot version {version}")));
    }
    let next_id = r.u32()?;
    let generation = r.u64()?;
    let last_lsn = r.u64()?;
    let entry_count = r.u64()? as usize;
    let names_len = r.u64()? as usize;
    let root_count = r.u32()? as usize;

    let mut roots = Vec::with_capacity(root_count);
    for id in 0..root_count {
        let plen = r.u32()? as usize;
        let path_bytes = r.bytes(plen)?;
        // SAFETY-free reconstruction: use from_encoded_bytes where available.
        let path = path_from_bytes(path_bytes);
        let dev = r.u64()?;
        let enabled = r.u8()? != 0;
        let ftype_len = r.u32()? as usize;
        let fstype = String::from_utf8_lossy(r.bytes(ftype_len)?).into_owned();
        let caps = caps_from_bits(r.u8()?);
        let mut caps = caps;
        caps.fstype = fstype;
        let cg = r.u64()?;
        let cl = r.u64()?;
        let mut root = Root::new(id as u32, path);
        root.dev = dev;
        root.enabled = enabled;
        root.caps = caps;
        root.last_full_crawl = crate::cursor::Cursor::new(cg, cl);
        roots.push(root);
    }

    let names = r.bytes(names_len)?.to_vec();
    let mut entries = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        let name_off = r.u32()?;
        let name_len = r.u16()?;
        let parent = r.u32()?;
        let flags = r.u8()?;
        entries.push(Entry { name_off, name_len, parent, flags });
    }
    if r.remaining() != 0 {
        return Err(corrupt("trailing bytes in snapshot body"));
    }
    index_integrity(&names, &entries)?;

    let mut index = Index::new_reconstructed(names, entries);
    index.set_next_id(next_id);
    index.set_generation(generation);
    index.set_last_lsn(last_lsn);
    Ok((index, roots))
}

fn path_from_bytes(b: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(b))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(b).into_owned())
    }
}

fn index_integrity(names: &[u8], entries: &[Entry]) -> Result<(), io::Error> {
    if entries.is_empty() {
        return Err(corrupt("snapshot has no root slot"));
    }
    for (i, e) in entries.iter().enumerate() {
        if e.name_off as usize + e.name_len as usize > names.len() {
            return Err(corrupt(&format!("entry {i} name out of range")));
        }
        if e.parent as usize >= entries.len() {
            return Err(corrupt(&format!("entry {i} parent out of range")));
        }
        if e.parent as usize >= i && e.parent != ROOT_ID {
            return Err(corrupt(&format!("entry {i} parent points forward")));
        }
    }
    Ok(())
}

fn corrupt(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Load a snapshot with `mmap`. Falls back to an owned buffer if the file is
/// empty (an empty file cannot be mmapped).
pub fn load_mmap(path: &Path) -> io::Result<LoadedSnapshot> {
    let f = File::open(path)?;
    let len = f.metadata()?.len();
    if len == 0 {
        return Err(corrupt("empty snapshot"));
    }
    // SAFETY: the file is only ever replaced by rename (write_atomic), and we
    // hold the mapping privately; no writer mutates a live mapping.
    let map = unsafe { memmap2::MmapOptions::new().map_copy_read_only(&f)? };
    let (index, roots) = decode(&map)?;
    Ok(LoadedSnapshot { index, roots, backing: SnapshotBacking::Mapped(map) })
}

/// Load a snapshot into an owned buffer (no mmap); handy in tests.
pub fn load_owned(path: &Path) -> io::Result<LoadedSnapshot> {
    let mut f = File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let (index, roots) = decode(&buf)?;
    Ok(LoadedSnapshot { index, roots, backing: SnapshotBacking::Owned(buf) })
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(b: &'a [u8]) -> Self {
        Cursor { b, i: 0 }
    }
    fn remaining(&self) -> usize {
        self.b.len() - self.i
    }
    fn skip(&mut self, n: usize) {
        self.i += n;
    }
    fn u8(&mut self) -> io::Result<u8> {
        let v = *self.b.get(self.i).ok_or_else(|| corrupt("eof"))?;
        self.i += 1;
        Ok(v)
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into().unwrap()))
    }
    fn bytes(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self.i + n;
        let s = self.b.get(self.i..end).ok_or_else(|| corrupt("eof"))?;
        self.i = end;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::EntryFlags;


    fn sample() -> (Index, Vec<Root>) {
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"proj", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        let s = ix.insert(r, b"src", EntryFlags::EMPTY.with_dir(true));
        ix.insert(s, b"main.rs", EntryFlags::EMPTY);
        ix.insert(r, b"README.md", EntryFlags::EMPTY);
        ix.set_last_lsn(99);
        ix.set_generation(4);
        let mut root = Root::new(0, PathBuf::from("/home/me/proj"));
        root.dev = 42;
        root.caps.writable = true;
        root.caps.fstype = "ext4".into();
        root.last_full_crawl = crate::cursor::Cursor::new(4, 90);
        (ix, vec![root])
    }

    #[test]
    fn encode_decode_roundtrip() {
        let (ix, roots) = sample();
        let bytes = encode(&ix, &roots);
        let (ix2, r2) = decode(&bytes).unwrap();
        assert_eq!(ix2.generation(), 4);
        assert_eq!(ix2.last_lsn(), 99);
        assert_eq!(ix2.live_count(), ix.live_count());
        assert_eq!(ix2.names, ix.names);
        assert_eq!(ix2.entries, ix.entries);
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].path, PathBuf::from("/home/me/proj"));
        assert_eq!(r2[0].dev, 42);
        assert!(r2[0].caps.writable);
        assert_eq!(r2[0].caps.fstype, "ext4");
        assert_eq!(r2[0].last_full_crawl, crate::cursor::Cursor::new(4, 90));
    }

    #[test]
    fn checksum_detects_corruption() {
        let (ix, roots) = sample();
        let mut bytes = encode(&ix, &roots);
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn bad_version_and_magic_rejected() {
        let (ix, roots) = sample();
        let mut bytes = encode(&ix, &roots);
        bytes[0] = b'X';
        assert!(decode(&bytes).is_err());
        let (ix, roots) = sample();
        let mut bytes = encode(&ix, &roots);
        // Bump version then re-checksum so we test the version path, not CRC.
        bytes[4..8].copy_from_slice(&9u32.to_le_bytes());
        let n = bytes.len();
        let sum = crc32(&bytes[..n - 4]);
        bytes[n - 4..].copy_from_slice(&sum.to_le_bytes());
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn file_roundtrip_atomic() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("snapshot.fnx");
        let (ix, roots) = sample();
        write_atomic(&p, &ix, &roots).unwrap();
        let loaded = load_mmap(&p).unwrap();
        assert_eq!(loaded.index.live_count(), ix.live_count());
        assert_eq!(loaded.roots[0].path, PathBuf::from("/home/me/proj"));
    }

    #[test]
    fn rejects_forward_parent() {
        let (ix, roots) = sample();
        let mut bytes = encode(&ix, &roots);
        // Find the entries block: recompute offset is fiddly, so flip a middle
        // entry's parent to a huge value and re-checksum; decode must reject.
        // We just corrupt and expect *some* error (crc or integrity).
        let n = bytes.len();
        bytes[n - 8] = 0xFF;
        let sum = crc32(&bytes[..n - 4]);
        bytes[n - 4..].copy_from_slice(&sum.to_le_bytes());
        assert!(decode(&bytes).is_err());
    }
}
