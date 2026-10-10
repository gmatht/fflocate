//! The fanotify-FID backend (§6, v1.5).
//!
//! Where inotify needs one watch per directory, fanotify can watch a whole
//! **filesystem** and report *file handles* rather than watch descriptors. That
//! is the "fanotify FID" design in §6: `FAN_MARK_FILESYSTEM` +
//! `FAN_REPORT_DFID_NAME`, zero per-directory kernel memory, and atomic rename
//! events.
//!
//! ## What arrives
//!
//! Every event is a `fanotify_event_metadata` header followed by one or more
//! info records. With `FAN_REPORT_DFID_NAME` a namespace event carries a
//! `FAN_EVENT_INFO_TYPE_DFID_NAME` record:
//!
//! ```text
//! fanotify_event_info_fid { hdr{type,pad,len}, fsid[2] }  file_handle { handle_bytes, handle_type, f_handle[] }  name\0
//! ```
//!
//! The **DFID is the parent directory** of the affected name — that is the
//! `DirKey` the applier needs. The name follows the handle, NUL-terminated.
//!
//! ## Consequences of a whole-mount watch
//!
//! * We cannot decline a subtree at watch time, so **exclusions are the
//!   applier's job** (§6 "ignore asymmetry"). [`BackendCaps::whole_mount`] is
//!   true and `per_dir_watch` false, which is how the applier learns this
//!   without special-casing fanotify by name.
//! * A `FAN_MARK_FILESYSTEM` watch fires for *every* path on that mount that is
//!   not excluded, including other users' activity. The DFID→dir-id map (see
//!   [`crate::watch::fidmap`]) is what turns that firehose back into events about
//!   the roots we actually index; anything that does not resolve is dropped by
//!   the resolver, and the *resolver*, not the backend, decides when to escalate.
//!
//! ## Renames
//!
//! fanotify has no rename cookie. A rename arrives as `FAN_MOVED_FROM` then
//! `FAN_MOVED_TO`, and the two share the moved object's **file handle**, so that
//! is the pairing key. An unpaired `MOVED_FROM` (partner not yet seen) is held;
//! if no partner arrives it degrades to `Remove`, and the subsequent `MOVED_TO`
//! — if any — becomes an `Upsert`. Both orders converge to the same index state,
//! which is the property that matters (§7).
//!
//! Content writes are never requested (§1).

use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::Path;
use std::time::Duration;

use super::{BackendCaps, WatchBackend};
use crate::event::{DirKey, FsEvent};
use crate::roots::Root;
use crate::watch::fidmap::HandleKey;

// fanotify constants. `libc` does not expose the DFID ones, so they are defined
// here from linux/fanotify.h (kernel ≥ 5.13 for FAN_REPORT_DFID_NAME).
#[allow(dead_code)]
pub mod masks {
    pub const FAN_CLASS_NOTIF: u32 = 0x0000_0000;
    pub const FAN_CLOEXEC: u32 = 0x0000_0001;
    pub const FAN_NONBLOCK: u32 = 0x0000_0002;
    /// `FAN_REPORT_DIR_FID | FAN_REPORT_NAME`.
    ///
    /// `FAN_REPORT_NAME` is `0x800`, not `0x200` — getting this wrong yields
    /// `FAN_EVENT_INFO_TYPE_DFID` records with no name at all, which is a silent
    /// loss of every event. Verified against `/usr/include/linux/fanotify.h`.
    pub const FAN_REPORT_DFID_NAME: u32 = 0x0000_0400 | 0x0000_0800;
    pub const FAN_REPORT_FID: u32 = 0x0000_0200;

    pub const FAN_MARK_ADD: u32 = 0x0000_0001;
    pub const FAN_MARK_FILESYSTEM: u32 = 0x0000_0100;

    pub const FAN_MOVED_FROM: u64 = 0x0000_0040;
    pub const FAN_MOVED_TO: u64 = 0x0000_0080;
    pub const FAN_CREATE: u64 = 0x0000_0100;
    pub const FAN_DELETE: u64 = 0x0000_0200;
    pub const FAN_MOVE_SELF: u64 = 0x0000_0800;
    pub const FAN_DELETE_SELF: u64 = 0x0000_0400;
    pub const FAN_Q_OVERFLOW: u64 = 0x0000_4000;
    pub const FAN_ONDIR: u64 = 0x4000_0000;
    pub const FAN_EVENT_ON_CHILD: u64 = 0x0800_0000;

    pub const FAN_EVENT_INFO_TYPE_FID: u8 = 1;
    pub const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
    pub const FAN_EVENT_INFO_TYPE_DFID: u8 = 3;

    pub const FAN_NOFD: i32 = -1;
    /// `sizeof(struct fanotify_event_metadata)`.
    pub const FAN_EVENT_METADATA_LEN: usize = 24;
    /// `sizeof(struct fanotify_event_info_header)`.
    pub const INFO_HEADER_LEN: usize = 4;
    /// `sizeof(__kernel_fsid_t)` = `int[2]`.
    pub const FSID_LEN: usize = 8;
    /// `sizeof(struct file_handle)` up to (not including) `f_handle[]`.
    pub const FILE_HANDLE_LEN: usize = 8;
}
use masks::*;

/// Events we ask for. Content writes are deliberately absent (§1).
const MASK: u64 = FAN_CREATE
    | FAN_DELETE
    | FAN_MOVED_FROM
    | FAN_MOVED_TO
    | FAN_DELETE_SELF
    | FAN_MOVE_SELF
    | FAN_ONDIR
    | FAN_EVENT_ON_CHILD;

/// `struct fanotify_event_metadata` (24 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct EventMetadata {
    pub event_len: u32,
    pub vers: u8,
    pub reserved: u8,
    pub metadata_len: u16,
    pub mask: u64,
    pub fd: i32,
    pub pid: i32,
}

/// A decoded `FAN_EVENT_INFO_TYPE_DFID[_NAME]` record.
///
/// `handle` is the raw `f_handle[]` bytes plus `handle_type`; `dev` is the fsid
/// from the info record. Together they identify a directory in a way that is
/// stable across the crawl and the watch (both come from the kernel).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fid {
    pub handle_bytes: u32,
    pub handle_type: i32,
    pub handle: Vec<u8>,
    /// `fsid` from the info record, two i32s; kept raw because the kernel's
    /// `__kernel_fsid_t` is not necessarily the same as `st_dev`.
    pub fsid: [i32; 2],
    /// The entry name for a DFID_NAME record (empty for a bare DFID).
    pub name: Vec<u8>,
    /// True when this was a `DFID_NAME` record (so `name` is meaningful).
    pub has_name: bool,
}

impl Fid {
    /// The `(fsid, handle_bytes, handle_type, handle)` identity, i.e. everything
    /// but the name. Two events about the same object share it.
    pub fn identity(&self) -> FidIdentity {
        FidIdentity {
            fsid: self.fsid,
            handle_bytes: self.handle_bytes,
            handle_type: self.handle_type,
            handle: self.handle.clone(),
        }
    }
}

/// Identity of a file handle, without the name. Used to key the DFID→dir map and
/// to pair rename halves.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FidIdentity {
    pub fsid: [i32; 2],
    pub handle_bytes: u32,
    pub handle_type: i32,
    pub handle: Vec<u8>,
}

/// One decoded fanotify event, before it becomes an [`FsEvent`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodedEvent {
    /// A namespace op: the parent directory's DFID plus the entry name.
    Namespace { mask: u64, parent: Fid, name: Vec<u8> },
    /// The queue overflowed; rescan the whole root.
    Overflow,
    /// An event carrying no DFID_NAME we can use.
    Ignored,
}

impl DecodedEvent {
    pub fn is_dir(&self) -> bool {
        matches!(self, DecodedEvent::Namespace { mask, .. } if mask & FAN_ONDIR != 0)
    }
    pub fn mask(&self) -> u64 {
        match self {
            DecodedEvent::Namespace { mask, .. } => *mask,
            _ => 0,
        }
    }
}

/// Parse a `DFID[_NAME]` info record starting at `buf`.
///
/// Returns the decoded [`Fid`] and the number of bytes consumed.
pub fn parse_fid_record(buf: &[u8]) -> Option<(Fid, usize)> {
    if buf.len() < INFO_HEADER_LEN {
        return None;
    }
    let info_type = buf[0];
    // buf[1] is pad.
    let info_len = u16::from_ne_bytes([buf[2], buf[3]]) as usize;
    if info_len < INFO_HEADER_LEN || info_len > buf.len() {
        return None;
    }
    let rec = &buf[..info_len];
    match info_type {
        FAN_EVENT_INFO_TYPE_DFID_NAME | FAN_EVENT_INFO_TYPE_DFID | FAN_EVENT_INFO_TYPE_FID => {}
        // Some other info record (e.g. a pidfd or an error record): not ours.
        _ => return None,
    }
    // hdr(4) + fsid(8) + file_handle{handle_bytes:u32, handle_type:i32, f_handle[]}
    let min = INFO_HEADER_LEN + FSID_LEN + FILE_HANDLE_LEN;
    if rec.len() < min {
        return None;
    }
    let fsid = [
        i32::from_ne_bytes([rec[4], rec[5], rec[6], rec[7]]),
        i32::from_ne_bytes([rec[8], rec[9], rec[10], rec[11]]),
    ];
    let hb_off = INFO_HEADER_LEN + FSID_LEN;
    let handle_bytes = u32::from_ne_bytes([rec[hb_off], rec[hb_off + 1], rec[hb_off + 2], rec[hb_off + 3]]);
    let handle_type =
        i32::from_ne_bytes([rec[hb_off + 4], rec[hb_off + 5], rec[hb_off + 6], rec[hb_off + 7]]);
    let hbytes = handle_bytes as usize;
    let handle_start = hb_off + FILE_HANDLE_LEN;
    let handle_end = handle_start.saturating_add(hbytes);
    if handle_end > rec.len() {
        return None;
    }
    let handle = rec[handle_start..handle_end].to_vec();

    let has_name = info_type == FAN_EVENT_INFO_TYPE_DFID_NAME;
    let name = if has_name && handle_end < rec.len() {
        // NUL-terminated, and the length is padded, so stop at the first NUL.
        rec[handle_end..]
            .iter()
            .copied()
            .take_while(|b| *b != 0)
            .collect()
    } else {
        Vec::new()
    };

    Some((
        Fid { handle_bytes, handle_type, handle, fsid, name, has_name },
        info_len,
    ))
}

/// Split one event's body (everything after the metadata header) into its
/// decoded forms.
pub fn parse_event(mask: u64, body: &[u8]) -> Vec<DecodedEvent> {
    if mask & FAN_Q_OVERFLOW != 0 {
        return vec![DecodedEvent::Overflow];
    }
    // Namespace events (the ones we subscribe to) carry a DFID_NAME record.
    // Walk the info records; the first DFID_NAME is the parent + name.
    let mut off = 0usize;
    while off + INFO_HEADER_LEN <= body.len() {
        let info_len = u16::from_ne_bytes([body[off + 2], body[off + 3]]) as usize;
        if info_len < INFO_HEADER_LEN || off + info_len > body.len() {
            break;
        }
        if let Some((fid, used)) = parse_fid_record(&body[off..off + info_len]) {
            if fid.has_name {
                return vec![DecodedEvent::Namespace { mask, parent: fid.clone(), name: fid.name }];
            }
            let _ = used;
        }
        off += info_len;
    }
    vec![DecodedEvent::Ignored]
}

/// The set of events the backend subscribes to (exposed for tests/docs).
pub fn subscribed_mask() -> u64 {
    MASK
}

pub struct FanotifyBackend {
    fd: RawFd,
    /// dfid → directory id map. The backend never walks the tree; it asks the
    /// resolver "which dir is this DFID?" and the resolver answers.
    ///
    /// Keyed by [`HandleKey`] — `handle_type` + handle bytes — deliberately *not*
    /// by the event's `fsid`: fanotify's fsid and `name_to_handle_at`'s mount id
    /// are different namespaces, and keying on them silently fails to match
    /// (learned the hard way).
    dir_of_fid: HashMap<HandleKey, u32>,
    /// Root dirs, for overflow escalation.
    root_dirs: Vec<u32>,
    /// Pending `MOVED_FROM`s awaiting their `MOVED_TO` partner, keyed by the
    /// moved object's handle identity.
    pending_move: HashMap<HandleKey, (u32, Vec<u8>)>,
    /// Marks applied so far (mount path → true), so `add_root` is idempotent.
    marked: HashMap<std::path::PathBuf, bool>,
    /// Counted for `ff stats`: events whose DFID we could not resolve.
    pub unresolved_events: u64,
}

impl FanotifyBackend {
    /// Open a fanotify fd with the FID/DIFD name reporting we need.
    ///
    /// The fd is always non-blocking: [`WatchBackend::poll`] waits with `poll(2)`
    /// and then drains until `EAGAIN`. A blocking fd would make the drain loop
    /// hang forever once the queue drained, which is exactly the bug this
    /// documents.
    ///
    /// Fails with a clear error when the kernel or the container does not
    /// support it — the caller then falls back to inotify (§4 probe).
    pub fn new() -> io::Result<FanotifyBackend> {
        let flags = FAN_CLASS_NOTIF | FAN_REPORT_DFID_NAME | FAN_CLOEXEC | FAN_NONBLOCK;
        let fd = unsafe { libc::fanotify_init(flags as libc::c_uint, (libc::O_RDONLY | libc::O_LARGEFILE) as libc::c_uint) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(FanotifyBackend {
            fd,
            dir_of_fid: HashMap::new(),
            root_dirs: Vec::new(),
            pending_move: HashMap::new(),
            marked: HashMap::new(),
            unresolved_events: 0,
        })
    }

    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Tell the backend which index directory a file handle names. The resolver
    /// builds this from the crawl.
    pub fn learn_fid(&mut self, fid: FidIdentity, dir_id: u32) {
        self.dir_of_fid.insert(HandleKey::of_fid_identity(&fid), dir_id);
    }

    /// Forget everything learned about a subtree's fids (used when a root is
    /// removed or remounted).
    pub fn forget_dir(&mut self, dir_id: u32) {
        self.dir_of_fid.retain(|_, v| *v != dir_id);
    }

    pub fn note_root_dir(&mut self, dir_id: u32) {
        if !self.root_dirs.contains(&dir_id) {
            self.root_dirs.push(dir_id);
        }
    }

    /// Mark a *filesystem* for this fd. This is the whole-mount watch: it covers
    /// every unmounted subtree, which is why the applier must filter exclusions.
    pub fn mark_filesystem(&mut self, path: &Path) -> io::Result<()> {
        if self.marked.get(path).copied().unwrap_or(false) {
            return Ok(());
        }
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let rc = unsafe {
            libc::fanotify_mark(
                self.fd,
                FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                MASK,
                libc::AT_FDCWD,
                c.as_ptr(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        self.marked.insert(path.to_path_buf(), true);
        Ok(())
    }

    pub fn is_marked(&self, path: &Path) -> bool {
        self.marked.get(path).copied().unwrap_or(false)
    }

    /// Turn a decoded event into [`FsEvent`]s, using the fid→dir map.
    ///
    /// An event whose DFID is unknown is **counted and dropped**: a whole-mount
    /// watch sees the entire filesystem, so unknown parents are the normal case
    /// (other users' trees), not an error. The caller can compare
    /// [`Self::unresolved_events`] against a threshold and escalate; it is not
    /// this function's job to decide (that lives in the resolver, §7).
    pub fn translate(&mut self, decoded: DecodedEvent) -> Vec<FsEvent> {
        match decoded {
            DecodedEvent::Overflow => self
                .root_dirs
                .iter()
                .map(|d| FsEvent::rescan(DirKey::new(*d), crate::event::RescanCause::Overflow))
                .collect(),
            DecodedEvent::Ignored => Vec::new(),
            DecodedEvent::Namespace { mask, parent, name } => {
                let Some(dir) = self.dir_of_fid.get(&HandleKey::of_identity(&parent)).copied() else {
                    self.unresolved_events += 1;
                    return Vec::new();
                };
                let is_dir = mask & FAN_ONDIR != 0;
                if mask & FAN_MOVED_FROM != 0 {
                    // Remember by the *object's* handle so the matching MOVED_TO
                    // (which names the destination dir) can complete the pair.
                    self.pending_move
                        .insert(HandleKey::of_identity(&parent), (dir, name.clone()));
                    return Vec::new();
                }
                if mask & FAN_MOVED_TO != 0 {
                    if let Some((from_dir, from_name)) = self.pending_move.remove(&HandleKey::of_identity(&parent)) {
                        return vec![
                            FsEvent::remove(DirKey::new(from_dir), from_name),
                            FsEvent::upsert(DirKey::new(dir), name, is_dir),
                        ];
                    }
                    return vec![FsEvent::upsert(DirKey::new(dir), name, is_dir)];
                }
                if mask & (FAN_DELETE | FAN_DELETE_SELF) != 0 {
                    return vec![FsEvent::remove(DirKey::new(dir), name)];
                }
                if mask & (FAN_CREATE | FAN_MOVE_SELF) != 0 {
                    return vec![FsEvent::upsert(DirKey::new(dir), name, is_dir)];
                }
                Vec::new()
            }
        }
    }

    /// Decode a raw buffer (one or more events back to back) into [`FsEvent`]s.
    /// Testable without a live watch.
    pub fn parse_and_translate(&mut self, buf: &[u8]) -> Vec<FsEvent> {
        let mut out = Vec::new();
        let mut off = 0usize;
        while off + FAN_EVENT_METADATA_LEN <= buf.len() {
            let meta: EventMetadata =
                unsafe { std::ptr::read_unaligned(buf[off..].as_ptr() as *const EventMetadata) };
            let len = meta.event_len as usize;
            if len < FAN_EVENT_METADATA_LEN || off + len > buf.len() {
                break;
            }
            // Body = everything after the fixed metadata header.
            let body_start = off + FAN_EVENT_METADATA_LEN.min(len);
            let body = &buf[body_start..off + len];
            for d in parse_event(meta.mask, body) {
                out.extend(self.translate(d));
            }
            off += len;
        }
        out
    }

    fn readable(&mut self, timeout: Duration) -> bool {
        let mut pfd = libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        n > 0 && (pfd.revents & libc::POLLIN) != 0
    }

    fn drain_raw(&mut self) -> io::Result<Vec<u8>> {
        let mut all = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                match e.raw_os_error() {
                    Some(libc::EAGAIN) => break,
                    Some(libc::EINTR) => continue,
                    _ => return Err(e),
                }
            }
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n as usize]);
        }
        Ok(all)
    }
}

impl Drop for FanotifyBackend {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

impl WatchBackend for FanotifyBackend {
    fn caps(&self) -> BackendCaps {
        // Whole-mount, no per-dir watches, and atomic rename halves.
        BackendCaps { per_dir_watch: false, whole_mount: true, atomic_rename: true }
    }

    fn add_root(&mut self, root: &Root) -> io::Result<()> {
        self.note_root_dir(root.id);
        self.mark_filesystem(&root.path)
    }

    fn watch_dir(&mut self, _id: u32, _path: &Path) -> io::Result<()> {
        // Documented no-op: a filesystem mark already covers every directory.
        // The applier learns this from `caps().per_dir_watch == false`.
        Ok(())
    }

    fn remove_root(&mut self, root: &Root) -> io::Result<()> {
        self.root_dirs.retain(|d| *d != root.id);
        // The mark stays: it is per-mount and shared. Per-root dirs are dropped
        // from the map so their events stop resolving.
        Ok(())
    }

    fn poll(&mut self, timeout: Duration) -> io::Result<Vec<FsEvent>> {
        if !self.readable(timeout) {
            return Ok(Vec::new());
        }
        let raw = self.drain_raw()?;
        Ok(self.parse_and_translate(&raw))
    }

    fn name(&self) -> &'static str {
        "fanotify"
    }
}

// ---------------------------------------------------------------------------
// Raw encoders, so the byte-level parsing can be tested without a kernel watch.
// These mirror the kernel's layout exactly (see the header docs above).
// ---------------------------------------------------------------------------

/// Build a `FAN_EVENT_INFO_TYPE_DFID_NAME` record for `dir` + `name`.
pub fn encode_dfid_name_record(fsid: [i32; 2], handle_type: i32, handle: &[u8], name: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&fsid[0].to_ne_bytes());
    payload.extend_from_slice(&fsid[1].to_ne_bytes());
    payload.extend_from_slice(&(handle.len() as u32).to_ne_bytes());
    payload.extend_from_slice(&handle_type.to_ne_bytes());
    payload.extend_from_slice(handle);
    payload.extend_from_slice(name);
    payload.push(0);
    // The kernel pads the whole record to 8 bytes.
    while (INFO_HEADER_LEN + payload.len()) % 8 != 0 {
        payload.push(0);
    }
    let total = INFO_HEADER_LEN + payload.len();
    let mut out = Vec::with_capacity(total);
    out.push(FAN_EVENT_INFO_TYPE_DFID_NAME);
    out.push(0);
    out.extend_from_slice(&(total as u16).to_ne_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Wrap a set of info records in a `fanotify_event_metadata` header.
pub fn encode_event(mask: u64, infos: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = infos.iter().flatten().copied().collect();
    let event_len = FAN_EVENT_METADATA_LEN + body.len();
    let mut out = Vec::with_capacity(event_len);
    out.extend_from_slice(&(event_len as u32).to_ne_bytes());
    out.push(3); // vers
    out.push(0); // reserved
    out.extend_from_slice(&(FAN_EVENT_METADATA_LEN as u16).to_ne_bytes());
    out.extend_from_slice(&mask.to_ne_bytes());
    out.extend_from_slice(&FAN_NOFD.to_ne_bytes()); // fd
    out.extend_from_slice(&0i32.to_ne_bytes()); // pid
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fid(h: &[u8]) -> FidIdentity {
        FidIdentity { fsid: [7, 9], handle_bytes: h.len() as u32, handle_type: 1, handle: h.to_vec() }
    }

    fn backend_with_dir(fidbytes: &[u8], dir: u32) -> FanotifyBackend {
        let mut b = FanotifyBackend::new().unwrap();
        b.learn_fid(fid(fidbytes), dir);
        b.note_root_dir(dir);
        b
    }

    #[test]
    fn parses_a_dfid_name_record() {
        let rec = encode_dfid_name_record([7, 9], 1, &[0xaa, 0xbb], b"main.rs");
        let (parsed, used) = parse_fid_record(&rec).expect("record parses");
        assert_eq!(used, rec.len());
        assert_eq!(parsed.fsid, [7, 9]);
        assert_eq!(parsed.handle_bytes, 2);
        assert_eq!(parsed.handle_type, 1);
        assert_eq!(parsed.handle, vec![0xaa, 0xbb]);
        assert!(parsed.has_name);
        assert_eq!(parsed.name, b"main.rs".to_vec());
    }

    #[test]
    fn name_padding_is_trimmed_at_the_nul() {
        // A name whose record needs padding must not leak the pad bytes.
        let rec = encode_dfid_name_record([1, 2], 1, &[1], b"abc");
        let (parsed, _) = parse_fid_record(&rec).unwrap();
        assert_eq!(parsed.name, b"abc".to_vec());
    }

    #[test]
    fn short_or_malformed_records_are_rejected_not_panicked() {
        assert!(parse_fid_record(&[]).is_none());
        assert!(parse_fid_record(&[2, 0, 4, 0]).is_none(), "header only, no fid body");
        // info_len larger than the buffer
        assert!(parse_fid_record(&[2, 0, 0xff, 0xff, 0, 0, 0, 0]).is_none());
    }

    #[test]
    fn create_becomes_upsert_against_the_learning_map() {
        let mut b = backend_with_dir(&[1, 2, 3], 100);
        let ev = encode_event(FAN_CREATE, &[encode_dfid_name_record([7, 9], 1, &[1, 2, 3], b"new.rs")]);
        let out = b.parse_and_translate(&ev);
        assert_eq!(out, vec![FsEvent::upsert(DirKey::new(100), b"new.rs".to_vec(), false)]);
    }

    #[test]
    fn dir_flag_sets_isdir() {
        let mut b = backend_with_dir(&[1, 2, 3], 100);
        let ev = encode_event(
            FAN_CREATE | FAN_ONDIR,
            &[encode_dfid_name_record([7, 9], 1, &[1, 2, 3], b"sub")],
        );
        let out = b.parse_and_translate(&ev);
        assert_eq!(out, vec![FsEvent::upsert(DirKey::new(100), b"sub".to_vec(), true)]);
    }

    #[test]
    fn delete_becomes_remove() {
        let mut b = backend_with_dir(&[1, 2, 3], 100);
        let ev = encode_event(FAN_DELETE, &[encode_dfid_name_record([7, 9], 1, &[1, 2, 3], b"gone.rs")]);
        let out = b.parse_and_translate(&ev);
        assert_eq!(out, vec![FsEvent::remove(DirKey::new(100), b"gone.rs".to_vec())]);
    }

    #[test]
    fn rename_pair_becomes_remove_then_upsert() {
        let mut b = backend_with_dir(&[1, 2, 3], 100);
        // MOVED_FROM: the moved object (name "a") leaves dir 100.
        let from = encode_event(FAN_MOVED_FROM, &[encode_dfid_name_record([7, 9], 1, &[1, 2, 3], b"a")]);
        assert!(b.parse_and_translate(&from).is_empty(), "half a rename emits nothing yet");
        // MOVED_TO: the same object arrives as "b".
        let to = encode_event(FAN_MOVED_TO, &[encode_dfid_name_record([7, 9], 1, &[1, 2, 3], b"b")]);
        let out = b.parse_and_translate(&to);
        assert_eq!(
            out,
            vec![
                FsEvent::remove(DirKey::new(100), b"a".to_vec()),
                FsEvent::upsert(DirKey::new(100), b"b".to_vec(), false),
            ]
        );
    }

    #[test]
    fn moved_to_without_a_partner_is_a_bare_upsert() {
        let mut b = backend_with_dir(&[1, 2, 3], 100);
        let to = encode_event(FAN_MOVED_TO, &[encode_dfid_name_record([7, 9], 1, &[1, 2, 3], b"incoming")]);
        let out = b.parse_and_translate(&to);
        assert_eq!(out, vec![FsEvent::upsert(DirKey::new(100), b"incoming".to_vec(), false)]);
    }

    #[test]
    fn unknown_dfid_is_counted_not_misattributed() {
        // The whole-mount watch sees other trees; an unknown parent must not be
        // attributed to a root we do know.
        let mut b = backend_with_dir(&[1, 2, 3], 100);
        let ev = encode_event(FAN_CREATE, &[encode_dfid_name_record([7, 9], 1, &[9, 9, 9], b"other.rs")]);
        let out = b.parse_and_translate(&ev);
        assert!(out.is_empty(), "unknown DFID resolves to nothing");
        assert_eq!(b.unresolved_events, 1, "…but it is counted, never silent");
    }

    #[test]
    fn overflow_escalates_every_root() {
        let mut b = backend_with_dir(&[1, 2, 3], 100);
        let ev = encode_event(FAN_Q_OVERFLOW, &[]);
        let out = b.parse_and_translate(&ev);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], FsEvent::RescanSubtree { cause: crate::event::RescanCause::Overflow, .. }));
    }

    #[test]
    fn mask_constants_match_the_kernel_header() {
        // These are hand-defined because libc lacks them. A wrong value is not a
        // cosmetic bug: FAN_REPORT_NAME=0x200 instead of 0x800 silently yields
        // name-less DFID records and every event is lost. Pin them.
        assert_eq!(FAN_REPORT_DFID_NAME, 0xC00, "FAN_REPORT_DIR_FID|FAN_REPORT_NAME");
        assert_eq!(FAN_REPORT_FID, 0x200);
        assert_eq!(FAN_MARK_FILESYSTEM, 0x100);
        assert_eq!(FAN_MARK_ADD, 0x1);
        assert_eq!(FAN_CREATE, 0x100);
        assert_eq!(FAN_DELETE, 0x200);
        assert_eq!(FAN_MOVED_FROM, 0x40);
        assert_eq!(FAN_MOVED_TO, 0x80);
        assert_eq!(FAN_ONDIR, 0x4000_0000);
        assert_eq!(FAN_EVENT_ON_CHILD, 0x0800_0000);
        assert_eq!(FAN_Q_OVERFLOW, 0x4000);
        assert_eq!(FAN_EVENT_INFO_TYPE_DFID_NAME, 2);
        assert_eq!(FAN_EVENT_METADATA_LEN, 24);
    }

    #[test]
    fn caps_say_whole_mount_and_atomic_rename() {        // The applier keys exclusion filtering off this: a whole-mount backend
        // cannot decline a subtree at watch time.
        let b = FanotifyBackend::new().unwrap();
        let c = b.caps();
        assert!(c.whole_mount);
        assert!(!c.per_dir_watch);
        assert!(c.atomic_rename);
    }

    /// The decisive test: parse bytes the **real kernel** produced, not our own
    /// encoder. If the struct layout in this file is wrong, this fails.
    ///
    /// Runs on a private tmpfs so the whole-filesystem mark sees only our own
    /// events. Skips cleanly where fanotify or mount is unavailable.
    #[test]
    fn live_kernel_events_parse_and_translate() {
        use crate::mount::handle_of_path;

        let dir = std::path::Path::new("/tmp/findex-fanotify-live");
        let _ = std::fs::remove_dir_all(dir);
        if std::fs::create_dir_all(dir).is_err() {
            eprintln!("skip: cannot create {dir:?}");
            return;
        }
        // A private tmpfs keeps the FAN_MARK_FILESYSTEM watch from seeing the
        // whole root filesystem's traffic.
        let mounted = std::process::Command::new("mount")
            .args(["-t", "tmpfs", "-o", "size=4M", "tmpfs"])
            .arg(dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !mounted {
            eprintln!("skip: cannot mount tmpfs (needs privilege)");
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        let cleanup = || {
            let _ = std::process::Command::new("umount").arg(dir).status();
            let _ = std::fs::remove_dir_all(dir);
        };

        let Ok(mut b) = FanotifyBackend::new() else {
            eprintln!("skip: fanotify_init unavailable");
            cleanup();
            return;
        };
        // Learn the tmpfs root's handle exactly as the crawl would.
        let Some(handle) = handle_of_path(dir) else {
            eprintln!("skip: name_to_handle_at unavailable on {dir:?}");
            cleanup();
            return;
        };
        b.learn_fid(fid(&handle.handle), 77);
        if let Err(e) = b.mark_filesystem(dir) {
            eprintln!("skip: FAN_MARK_FILESYSTEM failed: {e}");
            cleanup();
            return;
        }

        // Drive the operations and collect events.
        //
        // The ops are spaced out on purpose: a burst that races the mark can be
        // delivered before the mark is active, and then the *earliest* op is
        // simply never reported. That is fanotify's behaviour, not a bug here,
        // and a test that ignores it is flaky rather than honest.
        let script = format!(
            "sleep 0.4; echo a > {d}/a.txt; sleep 0.4; mv {d}/a.txt {d}/b.txt; sleep 0.4; rm -f {d}/b.txt",
            d = dir.display()
        );
        std::process::Command::new("sh").arg("-c").arg(script).status().unwrap();

        let mut got = Vec::new();
        for _ in 0..40 {
            got.extend(b.poll(Duration::from_millis(50)).unwrap());
        }
        let saw_create = got.iter().any(|e| matches!(e, FsEvent::Upsert { name, .. } if name == b"a.txt"));
        let saw_rename = got.iter().any(|e| matches!(e, FsEvent::Remove { name, .. } if name == b"a.txt"))
            && got.iter().any(|e| matches!(e, FsEvent::Upsert { name, .. } if name == b"b.txt"));
        let saw_delete = got.iter().any(|e| matches!(e, FsEvent::Remove { name, .. } if name == b"b.txt"));

        cleanup();
        assert!(saw_create, "expected the kernel's create event to decode; got {got:?}");
        assert!(saw_rename, "expected an atomic rename pair to decode; got {got:?}");
        assert!(saw_delete, "expected the kernel's delete event to decode; got {got:?}");
        assert_eq!(b.unresolved_events, 0, "every event's parent handle was learned");
    }
}
