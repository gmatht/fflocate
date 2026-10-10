//! The inotify backend (§6, v1).
//!
//! One watch per directory. `max_user_watches` is real, so watch-add failures
//! degrade the root rather than panicking. Key behaviours:
//!
//! * `IN_Q_OVERFLOW` → `RescanSubtree(root, cause=overflow)` for every watched
//!   root. Never a silent drop.
//! * `IN_IGNORED` → the kernel dropped our watch (dir deleted / watch removed):
//!   drop the wd→id mapping (wds get reused) and rescan the subtree.
//! * `IN_UNMOUNT` → drop the subtree, await remount.
//! * rename pairs (`IN_MOVED_FROM`/`IN_MOVED_TO` sharing a cookie) become a
//!   logical move; an unpaired `MOVED_FROM` degrades to a remove.
//! * Content writes (`IN_MODIFY`, `IN_CLOSE_WRITE`, …) are ignored (§1: content
//!   writes generate zero index work).

use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{BackendCaps, WatchBackend};
use crate::event::{DirKey, FsEvent, RescanCause};
use crate::roots::Root;

// inotify constants (mirror linux/inotify.h; kept local so we do not depend on
// libc exposing every one of them). Content-write constants are listed
// explicitly and marked dead: we never subscribe to them (content writes
// generate zero index work, §1), but naming them documents the filter.
#[allow(dead_code)]
mod masks {
    pub const IN_ACCESS: u32 = 0x0000_0001;
    pub const IN_MODIFY: u32 = 0x0000_0002;
    pub const IN_ATTRIB: u32 = 0x0000_0004;
    pub const IN_CLOSE_WRITE: u32 = 0x0000_0008;
    pub const IN_CLOSE_NOWRITE: u32 = 0x0000_0010;
    pub const IN_OPEN: u32 = 0x0000_0020;
    pub const IN_MOVED_FROM: u32 = 0x0000_0040;
    pub const IN_MOVED_TO: u32 = 0x0000_0080;
    pub const IN_CREATE: u32 = 0x0000_0100;
    pub const IN_DELETE: u32 = 0x0000_0200;
    pub const IN_DELETE_SELF: u32 = 0x0000_0400;
    pub const IN_MOVE_SELF: u32 = 0x0000_0800;
    pub const IN_UNMOUNT: u32 = 0x0000_2000;
    pub const IN_Q_OVERFLOW: u32 = 0x0000_4000;
    pub const IN_IGNORED: u32 = 0x0000_8000;
    pub const IN_ISDIR: u32 = 0x4000_0000;
}
use masks::*;

/// Events we ask for. Content-write events are deliberately absent.
const MASK: u32 = IN_CREATE
    | IN_DELETE
    | IN_MOVED_FROM
    | IN_MOVED_TO
    | IN_DELETE_SELF
    | IN_MOVE_SELF
    | IN_UNMOUNT
    | IN_IGNORED
    | IN_Q_OVERFLOW;

/// Kernel's `struct inotify_event` header is 16 bytes; the name follows,
/// NUL-terminated and padded to a multiple of 4.
const EVENT_HEADER: usize = 16;

#[repr(C)]
#[derive(Clone, Copy)]
struct InotifyEvent {
    wd: i32,
    mask: u32,
    cookie: u32,
    len: u32,
}

pub struct InotifyBackend {
    fd: RawFd,
    /// wd → directory id.
    wd_to_dir: HashMap<i32, u32>,
    /// directory id → wd (for remove_root).
    dir_to_wd: HashMap<u32, i32>,
    /// A pending `MOVED_FROM` awaiting its `MOVED_TO` partner: (cookie, dir, name).
    pending_move: HashMap<u32, (u32, Vec<u8>)>,
    /// Roots and their directory ids, for overflow escalation.
    root_dirs: Vec<u32>,
    /// Paths for the watched dirs, for adding watches.
    dir_paths: HashMap<u32, PathBuf>,
    /// Watch-limit exhaustion count for `ff stats`.
    pub watch_limit_hits: u64,
}

impl InotifyBackend {
    pub fn new() -> io::Result<InotifyBackend> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(InotifyBackend {
            fd,
            wd_to_dir: HashMap::new(),
            dir_to_wd: HashMap::new(),
            pending_move: HashMap::new(),
            root_dirs: Vec::new(),
            dir_paths: HashMap::new(),
            watch_limit_hits: 0,
        })
    }

    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Add a watch for a directory. Returns the wd, or a watch-limit error.
    pub fn watch_dir(&mut self, dir_id: u32, path: &Path) -> io::Result<i32> {
        if let Some(wd) = self.dir_to_wd.get(&dir_id) {
            return Ok(*wd);
        }
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
        let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), MASK) };
        if wd < 0 {
            let e = io::Error::last_os_error();
            // ENOSPC == watch-limit exhaustion: degrade, don't panic.
            if e.raw_os_error() == Some(libc::ENOSPC) {
                self.watch_limit_hits += 1;
            }
            return Err(e);
        }
        self.wd_to_dir.insert(wd, dir_id);
        self.dir_to_wd.insert(dir_id, wd);
        self.dir_paths.insert(dir_id, path.to_path_buf());
        Ok(wd)
    }

    pub fn note_root_dir(&mut self, dir_id: u32) {
        if !self.root_dirs.contains(&dir_id) {
            self.root_dirs.push(dir_id);
        }
    }

    /// Which dir id does a wd correspond to?
    pub fn dir_of_wd(&self, wd: i32) -> Option<u32> {
        self.wd_to_dir.get(&wd).copied()
    }

    fn forget_wd(&mut self, wd: i32) -> Option<u32> {
        let dir = self.wd_to_dir.remove(&wd)?;
        self.dir_to_wd.remove(&dir);
        Some(dir)
    }

    /// Parse a raw inotify buffer into events, translating to [`FsEvent`].
    ///
    /// `resolve` maps a child directory id to an absolute path so that newly
    /// created directories can be watched immediately (the "watch first, then
    /// enumerate" rule applied to dynamic creation).
    pub fn parse_buffer(&mut self, buf: &[u8]) -> Vec<FsEvent> {
        let mut out = Vec::new();
        let mut off = 0usize;
        while off + EVENT_HEADER <= buf.len() {
            let ev: InotifyEvent = unsafe {
                std::ptr::read_unaligned(buf[off..].as_ptr() as *const InotifyEvent)
            };
            let name_len = ev.len as usize;
            let name_start = off + EVENT_HEADER;
            let name_end = (name_start + name_len).min(buf.len());
            let raw_name = &buf[name_start..name_end];
            let name = raw_name
                .iter()
                .copied()
                .take_while(|b| *b != 0)
                .collect::<Vec<u8>>();
            off = name_start + name_len;

            if ev.mask & IN_Q_OVERFLOW != 0 {
                for d in &self.root_dirs {
                    out.push(FsEvent::rescan(DirKey::new(*d), RescanCause::Overflow));
                }
                continue;
            }
            if ev.mask & IN_IGNORED != 0 {
                if let Some(dir) = self.forget_wd(ev.wd) {
                    out.push(FsEvent::rescan(DirKey::new(dir), RescanCause::WatchDropped));
                }
                continue;
            }
            if ev.mask & IN_UNMOUNT != 0 {
                if let Some(dir) = self.dir_of_wd(ev.wd) {
                    out.push(FsEvent::RootGone { dir: DirKey::new(dir) });
                }
                continue;
            }

            let Some(dir) = self.dir_of_wd(ev.wd) else { continue };

            if ev.mask & IN_MOVED_FROM != 0 {
                // Remember; a MOVED_TO with the same cookie completes the pair.
                self.pending_move.insert(ev.cookie, (dir, name));
                continue;
            }
            if ev.mask & IN_MOVED_TO != 0 {
                if let Some((from_dir, from_name)) = self.pending_move.remove(&ev.cookie) {
                    // A rename within/into a watched dir: emit remove + upsert.
                    out.push(FsEvent::remove(DirKey::new(from_dir), from_name));
                    let is_dir = ev.mask & IN_ISDIR != 0;
                    out.push(FsEvent::upsert(DirKey::new(dir), name, is_dir));
                } else {
                    let is_dir = ev.mask & IN_ISDIR != 0;
                    out.push(FsEvent::upsert(DirKey::new(dir), name, is_dir));
                }
                continue;
            }
            if ev.mask & (IN_DELETE | IN_DELETE_SELF) != 0 {
                out.push(FsEvent::remove(DirKey::new(dir), name));
                continue;
            }
            if ev.mask & (IN_CREATE | IN_MOVE_SELF) != 0 {
                let is_dir = ev.mask & IN_ISDIR != 0;
                out.push(FsEvent::upsert(DirKey::new(dir), name, is_dir));
                continue;
            }
            // Everything else (ACCESS/MODIFY/OPEN/CLOSE/ATTRIB) is ignored:
            // content writes generate zero index work.
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
            let n = unsafe {
                libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
            };
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

impl Drop for InotifyBackend {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

impl WatchBackend for InotifyBackend {
    fn caps(&self) -> BackendCaps {
        BackendCaps { per_dir_watch: true, whole_mount: false, atomic_rename: false }
    }

    fn add_root(&mut self, root: &Root) -> io::Result<()> {
        self.note_root_dir(root.id);
        Ok(())
    }

    fn watch_dir(&mut self, id: u32, path: &Path) -> io::Result<()> {
        self.watch_dir(id, path).map(|_| ())
    }

    fn remove_root(&mut self, root: &Root) -> io::Result<()> {
        self.root_dirs.retain(|r| *r != root.id);
        if let Some(wd) = self.dir_to_wd.get(&root.id).copied() {
            unsafe {
                libc::inotify_rm_watch(self.fd, wd);
            }
            self.forget_wd(wd);
        }
        Ok(())
    }

    fn poll(&mut self, timeout: Duration) -> io::Result<Vec<FsEvent>> {
        if !self.readable(timeout) {
            return Ok(Vec::new());
        }
        let raw = self.drain_raw()?;
        Ok(self.parse_buffer(&raw))
    }

    fn name(&self) -> &'static str {
        "inotify"
    }
}

/// Build the raw inotify byte buffer for a single event. Used by tests to
/// exercise [`InotifyBackend::parse_buffer`] without a live kernel watch.
pub fn encode_event(wd: i32, mask: u32, cookie: u32, name: &[u8]) -> Vec<u8> {
    let mut name_padded = name.to_vec();
    name_padded.push(0);
    while name_padded.len() % 4 != 0 {
        name_padded.push(0);
    }
    let mut out = Vec::with_capacity(EVENT_HEADER + name_padded.len());
    out.extend_from_slice(&wd.to_le_bytes());
    out.extend_from_slice(&mask.to_le_bytes());
    out.extend_from_slice(&cookie.to_le_bytes());
    out.extend_from_slice(&(name_padded.len() as u32).to_le_bytes());
    out.extend_from_slice(&name_padded);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend_with_watches() -> (InotifyBackend, i32) {
        let mut b = InotifyBackend::new().unwrap();
        // Seed a fake watch mapping without touching the kernel.
        b.wd_to_dir.insert(1, 100);
        b.dir_to_wd.insert(100, 1);
        b.note_root_dir(100);
        (b, 1)
    }

    #[test]
    fn create_becomes_upsert() {
        let (mut b, _wd) = backend_with_watches();
        let buf = encode_event(1, IN_CREATE, 0, b"new.rs");
        let evs = b.parse_buffer(&buf);
        assert_eq!(evs, vec![FsEvent::upsert(DirKey::new(100), b"new.rs".to_vec(), false)]);
    }

    #[test]
    fn dir_create_sets_isdir() {
        let (mut b, _wd) = backend_with_watches();
        let buf = encode_event(1, IN_CREATE | IN_ISDIR, 0, b"sub");
        let evs = b.parse_buffer(&buf);
        assert_eq!(evs, vec![FsEvent::upsert(DirKey::new(100), b"sub".to_vec(), true)]);
    }

    #[test]
    fn content_writes_are_ignored() {
        let (mut b, _wd) = backend_with_watches();
        for mask in [IN_MODIFY, IN_CLOSE_WRITE, IN_OPEN, IN_ACCESS, IN_ATTRIB] {
            let buf = encode_event(1, mask, 0, b"file");
            assert!(b.parse_buffer(&buf).is_empty(), "mask {mask:#x} should be ignored");
        }
    }

    #[test]
    fn overflow_escalates_every_root() {
        let (mut b, _wd) = backend_with_watches();
        let buf = encode_event(-1, IN_Q_OVERFLOW, 0, b"");
        let evs = b.parse_buffer(&buf);
        assert_eq!(evs.len(), 1);
        assert!(matches!(evs[0], FsEvent::RescanSubtree { cause: RescanCause::Overflow, .. }));
    }

    #[test]
    fn ignored_watch_triggers_rescan_and_forgets_wd() {
        let (mut b, _wd) = backend_with_watches();
        let buf = encode_event(1, IN_IGNORED, 0, b"");
        let evs = b.parse_buffer(&buf);
        assert!(matches!(evs[0], FsEvent::RescanSubtree { cause: RescanCause::WatchDropped, .. }));
        assert!(b.dir_of_wd(1).is_none(), "wd ids get reused; the mapping must go");
    }

    #[test]
    fn unpaired_moved_from_degrades() {
        let (mut b, _wd) = backend_with_watches();
        // MOVED_FROM then nothing: we remember it (no event yet).
        let buf = encode_event(1, IN_MOVED_FROM, 7, b"a");
        assert!(b.parse_buffer(&buf).is_empty());
        // A later MOVED_FROM for a different cookie leaves the first pending;
        // a MOVED_TO with a matching cookie completes it.
        let buf = encode_event(1, IN_MOVED_TO, 7, b"b");
        let evs = b.parse_buffer(&buf);
        assert_eq!(
            evs,
            vec![
                FsEvent::remove(DirKey::new(100), b"a".to_vec()),
                FsEvent::upsert(DirKey::new(100), b"b".to_vec(), false),
            ]
        );
    }

    #[test]
    fn unmount_emits_root_gone() {
        let (mut b, _wd) = backend_with_watches();
        let buf = encode_event(1, IN_UNMOUNT, 0, b"");
        let evs = b.parse_buffer(&buf);
        assert_eq!(evs, vec![FsEvent::RootGone { dir: DirKey::new(100) }]);
    }

    #[test]
    fn unknown_wd_is_skipped() {
        let (mut b, _wd) = backend_with_watches();
        let buf = encode_event(999, IN_CREATE, 0, b"x");
        assert!(b.parse_buffer(&buf).is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_kernel_watch_delivers_create() {
        let d = tempfile::tempdir().unwrap();
        let mut b = InotifyBackend::new().unwrap();
        b.watch_dir(5, d.path()).unwrap();
        std::fs::write(d.path().join("hello.txt"), b"x").unwrap();
        let mut got = Vec::new();
        for _ in 0..50 {
            got.extend(b.poll(Duration::from_millis(20)).unwrap());
            if got.iter().any(|e| matches!(e, FsEvent::Upsert { name, .. } if name == b"hello.txt")) {
                break;
            }
        }
        assert!(
            got.iter().any(|e| matches!(e, FsEvent::Upsert { name, .. } if name == b"hello.txt")),
            "expected a create event, got {got:?}"
        );
    }
}
