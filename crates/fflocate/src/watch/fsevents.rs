//! The FSEvents backend (§6): macOS realtime watching.
//!
//! FSEvents watches **whole volumes**, not directories: one stream per root
//! delivers coalesced, path-named events for everything below it. Two
//! consequences shape this backend:
//!
//! * It advertises [`BackendCaps::whole_mount`], so the applier filters
//!   exclusions itself (see `excludes`); `watch_dir` only records the
//!   path → directory-id mapping the events are resolved against.
//! * The event stream is lossy by design. `MustScanSubDirs`, `UserDropped`,
//!   `KernelDropped` and `EventIdsWrapped` all mean "you did not see
//!   everything", and each becomes an explicit [`FsEvent::RescanSubtree`] —
//!   never a silent gap (§7).
//!
//! Renames arrive as two events (old path, new path), both flagged
//! `ItemRenamed`, with no pairing information. The backend therefore stats the
//! path: an existing path is an upsert, a missing one a remove. That is why
//! [`BackendCaps::atomic_rename`] is `false`.
//!
//! Streaming starts at "now" (`kFSEventStreamEventIdSinceNow`): replaying
//! yesterday's history into a fresh index would be wasted work, and the
//! startup crawl already establishes ground truth (§5). Crash recovery is the
//! same reconcile-on-start path.
//!
//! The stream is created with `FSEventStreamSetDispatchQueue`, so events are
//! delivered on a libdispatch queue owned by this backend and forwarded over a
//! channel; `poll` drains that channel. No CFRunLoop is involved.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::io;
use std::os::raw::{c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};

use super::{BackendCaps, WatchBackend};
use crate::event::{DirKey, FsEvent, RescanCause};
use crate::roots::Root;

/// How many raw events one `poll` will translate before returning. The rest
/// stay queued for the next poll, keeping applier batches bounded.
const MAX_BATCH: usize = 4096;

/// `kCFStringEncodingUTF8`.
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// `kFSEventStreamEventIdSinceNow`.
const SINCE_NOW: u64 = u64::MAX;

/// Stream flags: `FileEvents | NoDefer | WatchRoot`.
const CREATE_FLAGS: u32 = 0x0000_0010 | 0x0000_0002 | 0x0000_0004;

/// Event flags we act on (FSEvents.h).
mod ev {
    pub const MUST_SCAN_SUBDIRS: u32 = 0x0000_0001;
    pub const USER_DROPPED: u32 = 0x0000_0002;
    pub const KERNEL_DROPPED: u32 = 0x0000_0004;
    pub const EVENT_IDS_WRAPPED: u32 = 0x0000_0008;
    pub const ROOT_CHANGED: u32 = 0x0000_0020;
    pub const MOUNT: u32 = 0x0000_0040;
    pub const UNMOUNT: u32 = 0x0000_0080;
    pub const ITEM_CREATED: u32 = 0x0000_0100;
    pub const ITEM_REMOVED: u32 = 0x0000_0200;
    pub const ITEM_INODE_META_MOD: u32 = 0x0000_0400;
    pub const ITEM_RENAMED: u32 = 0x0000_0800;
    pub const ITEM_CHANGE_OWNER: u32 = 0x0000_4000;
    pub const ITEM_IS_DIR: u32 = 0x0002_0000;
}

/// The loss flags: everything the kernel dropped, plus the cases where it
/// tells us a rescan is mandatory.
const LOSSY: u32 = ev::MUST_SCAN_SUBDIRS | ev::USER_DROPPED | ev::KERNEL_DROPPED | ev::EVENT_IDS_WRAPPED;

/// Mount/source changes: the root (or a volume below it) changed identity.
const STRUCTURAL: u32 = ev::ROOT_CHANGED | ev::MOUNT | ev::UNMOUNT;

/// Metadata changes that can alter the executable bit, without touching the
/// namespace. `ItemInodeMetaMod` is chmod/chown; content writes never reach
/// this backend's index (§1).
const METADATA: u32 = ev::ITEM_INODE_META_MOD | ev::ITEM_CHANGE_OWNER;

/// CoreServices / CoreFoundation / libdispatch, declared here rather than
/// pulled in as a dependency: the surface is six functions and three types,
/// and this repo keeps its build self-contained.
mod ffi {
    use super::*;

    pub type CFAllocatorRef = *const c_void;
    pub type CFArrayRef = *const c_void;
    pub type CFStringRef = *const c_void;
    pub type CFIndex = isize;
    pub type FSEventStreamRef = *mut c_void;
    pub type ConstFSEventStreamRef = *const c_void;
    pub type FSEventStreamCreateFlags = u32;
    pub type DispatchQueue = *mut c_void;

    /// `FSEventStreamContext`. `FSEventStreamCreate` takes a pointer to this,
    /// **not** the callback's info pointer, and copies it: the `info` below is
    /// what the callback receives. `version` 0 means the three fields after
    /// `info` are honoured; they are NULL, so CoreServices neither retains nor
    /// releases the sink — the backend keeps it alive until every stream is
    /// invalidated.
    #[repr(C)]
    pub struct FSEventStreamContext {
        pub version: CFIndex,
        pub info: *mut c_void,
        pub retain: *const c_void,
        pub release: *const c_void,
        pub copy_description: *const c_void,
    }

    pub type Callback = extern "C" fn(
        ConstFSEventStreamRef,
        *mut c_void,
        usize,
        *mut c_void,
        *const u32,
        *const u64,
    );

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        pub fn FSEventStreamCreate(
            allocator: CFAllocatorRef,
            callback: Callback,
            context: *const FSEventStreamContext,
            paths: CFArrayRef,
            since: u64,
            latency: f64,
            flags: FSEventStreamCreateFlags,
        ) -> FSEventStreamRef;
        pub fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: DispatchQueue);
        /// Returns a C `Boolean` (unsigned char): 1 on success.
        pub fn FSEventStreamStart(stream: FSEventStreamRef) -> u8;
        pub fn FSEventStreamStop(stream: FSEventStreamRef);
        pub fn FSEventStreamInvalidate(stream: FSEventStreamRef);
        pub fn FSEventStreamRelease(stream: FSEventStreamRef);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFStringCreateWithBytes(
            allocator: CFAllocatorRef,
            bytes: *const u8,
            num_bytes: CFIndex,
            encoding: u32,
            is_external: u8,
        ) -> CFStringRef;
        pub fn CFArrayCreate(
            allocator: CFAllocatorRef,
            values: *const *const c_void,
            num_values: CFIndex,
            callbacks: *const c_void,
        ) -> CFArrayRef;
        pub fn CFRelease(cf: *const c_void);
        pub static kCFTypeArrayCallBacks: c_void;
    }

    #[link(name = "System")]
    extern "C" {
        pub fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
        pub fn dispatch_release(obj: *mut c_void);
    }
}

/// One event as the kernel reported it, before it is resolved against the
/// directory map.
#[derive(Clone, Debug)]
struct RawEvent {
    path: PathBuf,
    flags: u32,
}

/// The callback's context: a channel sender. Boxed inside the backend and
/// kept alive for as long as any stream can call back.
struct Sink {
    tx: Sender<RawEvent>,
}

struct StreamHandle {
    stream: ffi::FSEventStreamRef,
    path: PathBuf,
}

pub struct FseventsBackend {
    sink: Box<Sink>,
    rx: Receiver<RawEvent>,
    queue: ffi::DispatchQueue,
    streams: Vec<StreamHandle>,
    /// Directory path → index directory id, recorded from `watch_dir`.
    dirs: HashMap<PathBuf, u32>,
    /// Root id + canonical path, for escalating lossy events to a rescan.
    roots: Vec<(u32, PathBuf)>,
}

// The stream and queue handles are raw pointers into CoreServices objects that
// only this thread touches; the backend is moved (never shared) between the
// daemon's threads, so sending it is sound.
unsafe impl Send for FseventsBackend {}

impl FseventsBackend {
    pub fn new() -> io::Result<FseventsBackend> {
        let (tx, rx) = unbounded();
        let label = CString::new("dev.fflocate.fsevents").expect("no NUL in literal");
        let queue = unsafe { ffi::dispatch_queue_create(label.as_ptr(), std::ptr::null()) };
        if queue.is_null() {
            return Err(io::Error::other("dispatch_queue_create failed"));
        }
        Ok(FseventsBackend {
            sink: Box::new(Sink { tx }),
            rx,
            queue,
            streams: Vec::new(),
            dirs: HashMap::new(),
            roots: Vec::new(),
        })
    }

    fn canonical(path: &Path) -> PathBuf {
        // FSEvents reports real paths (`/private/var/…` for `/var/…`), so the
        // root we register must be real too, or nothing resolves.
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    fn start_stream(&mut self, path: &Path) -> io::Result<()> {
        if self.streams.iter().any(|s| s.path == path) {
            return Ok(());
        }
        let bytes = path.as_os_str().as_bytes();
        if bytes.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "root path contains NUL",
            ));
        }
        // The callback's `info` must stay valid until every stream is stopped:
        // the box lives in `self.sink` and is dropped only in `Drop`, after the
        // streams are invalidated. `FSEventStreamCreate` copies this struct, so
        // a stack temporary is enough.
        let context = ffi::FSEventStreamContext {
            version: 0,
            info: self.sink.as_ref() as *const Sink as *mut c_void,
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        };
        unsafe {
            let cf_path = ffi::CFStringCreateWithBytes(
                std::ptr::null(),
                bytes.as_ptr(),
                bytes.len() as ffi::CFIndex,
                CF_STRING_ENCODING_UTF8,
                0,
            );
            if cf_path.is_null() {
                return Err(io::Error::other("CFStringCreateWithBytes failed"));
            }
            let values = [cf_path as *const c_void];
            let array = ffi::CFArrayCreate(
                std::ptr::null(),
                values.as_ptr(),
                1,
                &ffi::kCFTypeArrayCallBacks as *const c_void,
            );
            ffi::CFRelease(cf_path);
            if array.is_null() {
                return Err(io::Error::other("CFArrayCreate failed"));
            }
            let stream = ffi::FSEventStreamCreate(
                std::ptr::null(),
                callback,
                &context,
                array,
                SINCE_NOW,
                0.15, // latency: below human perception, above event-storm rate
                CREATE_FLAGS,
            );
            ffi::CFRelease(array);
            if stream.is_null() {
                return Err(io::Error::other("FSEventStreamCreate failed"));
            }
            ffi::FSEventStreamSetDispatchQueue(stream, self.queue);
            if ffi::FSEventStreamStart(stream) == 0 {
                ffi::FSEventStreamInvalidate(stream);
                ffi::FSEventStreamRelease(stream);
                return Err(io::Error::other("FSEventStreamStart failed"));
            }
            self.streams.push(StreamHandle {
                stream,
                path: path.to_path_buf(),
            });
        }
        Ok(())
    }

    fn stop_streams(&mut self) {
        for s in self.streams.drain(..) {
            unsafe {
                ffi::FSEventStreamStop(s.stream);
                ffi::FSEventStreamInvalidate(s.stream);
                ffi::FSEventStreamRelease(s.stream);
            }
        }
    }

    /// The id of the deepest known directory containing `path`.
    fn nearest_known_dir(&self, path: &Path) -> Option<u32> {
        let mut best: Option<(usize, u32)> = None;
        for (dir, id) in &self.dirs {
            if path.starts_with(dir) {
                let n = dir.as_os_str().len();
                if best.map(|(bn, _)| n > bn).unwrap_or(true) {
                    best = Some((n, *id));
                }
            }
        }
        best.map(|(_, id)| id)
    }

    /// Resolve an event path to (parent dir id, name).
    fn resolve(&self, path: &Path) -> Option<(u32, Vec<u8>)> {
        let parent = path.parent()?;
        let name = path.file_name()?;
        let id = self
            .dirs
            .get(parent)
            .copied()
            .or_else(|| self.dirs.get(&Self::canonical(parent)).copied())?;
        Some((id, crate::util::normalize_name(name.as_bytes()).into_owned()))
    }

    fn root_dir_for(&self, path: &Path) -> Option<u32> {
        let mut best: Option<(usize, u32)> = None;
        for (_rid, rpath) in &self.roots {
            if path.starts_with(rpath) {
                let n = rpath.as_os_str().len();
                if best.map(|(bn, _)| n > bn).unwrap_or(true) {
                    if let Some(dir) = self.dirs.get(rpath) {
                        best = Some((n, *dir));
                    }
                }
            }
        }
        best.map(|(_, dir)| dir)
    }

    fn translate(&self, raw: Vec<RawEvent>) -> Vec<FsEvent> {
        let mut out = Vec::new();
        // Coalescing: one poll batch often repeats the same path (create +
        // modify + attrib). Keep the last kind emitted per path.
        let mut seen: HashSet<(PathBuf, u8)> = HashSet::new();
        for r in raw {
            let flags = r.flags;
            if flags & LOSSY != 0 || flags & STRUCTURAL != 0 {
                // We were told we missed events: rescan the owning root, or
                // the deepest directory we do know about.
                let cause = if flags & STRUCTURAL != 0 {
                    RescanCause::MountEvent
                } else {
                    RescanCause::Overflow
                };
                if let Some(dir) = self.root_dir_for(&r.path).or_else(|| self.nearest_known_dir(&r.path)) {
                    let key = (r.path.clone(), 99);
                    if seen.insert(key) {
                        out.push(FsEvent::rescan(DirKey::new(dir), cause));
                    }
                }
                continue;
            }

            // Created / renamed: the file may or may not exist now. A rename
            // reports both names but cannot pair them, so existence decides.
            let created = flags & ev::ITEM_CREATED != 0;
            let renamed = flags & ev::ITEM_RENAMED != 0;
            let removed = flags & ev::ITEM_REMOVED != 0;
            let kind: u8;
            let is_dir = flags & ev::ITEM_IS_DIR != 0;
            let exists = if renamed {
                std::fs::symlink_metadata(&r.path).is_ok()
            } else {
                true
            };
            if created || (renamed && exists) {
                kind = 1; // upsert
            } else if removed || renamed {
                kind = 2; // remove
            } else if flags & METADATA != 0 {
                kind = 3; // refresh exec
            } else {
                // Content writes, finder-info and xattr changes: no index
                // work (§1).
                continue;
            }

            let Some((parent, name)) = self.resolve(&r.path) else {
                // A parent we have never indexed (a directory created and
                // written to before the crawl reached it). Rescan the nearest
                // known ancestor instead of dropping the event.
                if let Some(dir) = self.nearest_known_dir(&r.path) {
                    if seen.insert((r.path.clone(), 98)) {
                        out.push(FsEvent::rescan(
                            DirKey::new(dir),
                            RescanCause::WatchDropped,
                        ));
                    }
                }
                continue;
            };
            let key = (r.path.clone(), kind);
            if !seen.insert(key) {
                continue;
            }
            let parent = DirKey::new(parent);
            match kind {
                1 => out.push(FsEvent::upsert(parent, name, is_dir)),
                2 => out.push(FsEvent::remove(parent, name)),
                _ => out.push(FsEvent::refresh_exec(parent, name)),
            }
        }
        out
    }
}

impl Drop for FseventsBackend {
    fn drop(&mut self) {
        // Invalidate first: after it returns, the callback can no longer run,
        // so freeing the sink context below is safe.
        self.stop_streams();
        unsafe {
            ffi::dispatch_release(self.queue);
        }
    }
}

/// The kernel's callback. Runs on the libdispatch queue; it must not block,
/// so it only copies the path and forwards it.
extern "C" fn callback(
    _stream: ffi::ConstFSEventStreamRef,
    info: *mut c_void,
    num_events: usize,
    paths: *mut c_void,
    flags: *const u32,
    _ids: *const u64,
) {
    if info.is_null() || paths.is_null() || flags.is_null() {
        return;
    }
    // Safety: `info` is the `Sink` pointer handed to FSEventStreamCreate, and
    // the stream is invalidated before the sink is dropped.
    let sink = unsafe { &*(info as *const Sink) };
    let paths = paths as *const *const c_char;
    for i in 0..num_events {
        let p = unsafe { *paths.add(i) };
        if p.is_null() {
            continue;
        }
        let bytes = unsafe { CStr::from_ptr(p) }.to_bytes();
        if bytes.is_empty() {
            continue;
        }
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
        let flags = unsafe { *flags.add(i) };
        // A closed channel means the backend is gone; dropping is correct.
        let _ = sink.tx.send(RawEvent { path, flags });
    }
}

impl WatchBackend for FseventsBackend {
    fn caps(&self) -> BackendCaps {
        BackendCaps {
            per_dir_watch: false,
            whole_mount: true,
            // A rename is reported as two unpaired events (see module docs).
            atomic_rename: false,
        }
    }

    fn add_root(&mut self, root: &Root) -> io::Result<()> {
        let path = Self::canonical(&root.path);
        self.roots.retain(|(id, _)| *id != root.id);
        self.roots.push((root.id, path.clone()));
        self.start_stream(&path)
    }

    fn remove_root(&mut self, root: &Root) -> io::Result<()> {
        self.roots.retain(|(id, _)| *id != root.id);
        // Streams are per root path; stop the one this root started.
        if let Some(pos) = self
            .streams
            .iter()
            .position(|s| s.path == Self::canonical(&root.path))
        {
            let s = self.streams.remove(pos);
            unsafe {
                ffi::FSEventStreamStop(s.stream);
                ffi::FSEventStreamInvalidate(s.stream);
                ffi::FSEventStreamRelease(s.stream);
            }
        }
        Ok(())
    }

    /// Whole-mount backend: this records the path → id mapping events are
    /// resolved with; there is no per-directory watch to install.
    ///
    /// FSEvents reports real paths (`/private/var/…` for `/var/…`), while the
    /// index keeps the spelling it was configured with. Both are recorded:
    /// with only the configured spelling, every event under a symlinked prefix
    /// matched no directory and was dropped.
    fn watch_dir(&mut self, id: u32, path: &Path) -> io::Result<()> {
        self.dirs.insert(path.to_path_buf(), id);
        let canonical = Self::canonical(path);
        if canonical.as_path() != path {
            self.dirs.insert(canonical, id);
        }
        Ok(())
    }

    fn poll(&mut self, timeout: Duration) -> io::Result<Vec<FsEvent>> {
        let mut raw = Vec::new();
        match self.rx.recv_timeout(timeout) {
            Ok(e) => raw.push(e),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                return Ok(Vec::new())
            }
        }
        while raw.len() < MAX_BATCH {
            match self.rx.try_recv() {
                Ok(e) => raw.push(e),
                Err(_) => break,
            }
        }
        Ok(self.translate(raw))
    }

    fn name(&self) -> &'static str {
        "fsevents"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend_with_dirs() -> FseventsBackend {
        let mut b = FseventsBackend::new().unwrap();
        b.dirs.insert(PathBuf::from("/Users/me"), 7);
        b.dirs.insert(PathBuf::from("/Users/me/Documents"), 8);
        b.roots.push((1, PathBuf::from("/Users/me")));
        b
    }

    fn raw(path: &str, flags: u32) -> RawEvent {
        RawEvent {
            path: PathBuf::from(path),
            flags,
        }
    }

    #[test]
    fn create_becomes_upsert_with_dir_hint() {
        let b = backend_with_dirs();
        let out = b.translate(vec![raw(
            "/Users/me/Documents/a.txt",
            ev::ITEM_CREATED,
        )]);
        assert_eq!(
            out,
            vec![FsEvent::upsert(
                DirKey::new(8),
                b"a.txt".to_vec(),
                false
            )]
        );

        let out = b.translate(vec![raw(
            "/Users/me/Documents/sub",
            ev::ITEM_CREATED | ev::ITEM_IS_DIR,
        )]);
        assert_eq!(
            out,
            vec![FsEvent::upsert(DirKey::new(8), b"sub".to_vec(), true)]
        );
    }

    #[test]
    fn removal_becomes_remove() {
        let b = backend_with_dirs();
        let out = b.translate(vec![raw(
            "/Users/me/Documents/gone",
            ev::ITEM_REMOVED,
        )]);
        assert_eq!(
            out,
            vec![FsEvent::remove(DirKey::new(8), b"gone".to_vec())]
        );
    }

    #[test]
    fn metadata_becomes_exec_refresh_not_upsert() {
        let b = backend_with_dirs();
        let out = b.translate(vec![raw(
            "/Users/me/Documents/script.sh",
            ev::ITEM_INODE_META_MOD,
        )]);
        assert_eq!(
            out,
            vec![FsEvent::refresh_exec(
                DirKey::new(8),
                b"script.sh".to_vec()
            )]
        );
    }

    #[test]
    fn content_writes_and_xattrs_are_ignored() {
        let b = backend_with_dirs();
        for flags in [0x0000_1000 /* ItemModified */, 0x0000_8000 /* ItemXattrMod */] {
            let out = b.translate(vec![raw("/Users/me/Documents/x", flags)]);
            assert!(out.is_empty(), "flags {flags:#x} must not touch the index");
        }
    }

    #[test]
    fn dropped_events_escalate_to_a_root_rescan() {
        let b = backend_with_dirs();
        for flags in [
            ev::MUST_SCAN_SUBDIRS,
            ev::USER_DROPPED,
            ev::KERNEL_DROPPED,
            ev::EVENT_IDS_WRAPPED,
        ] {
            let out = b.translate(vec![raw("/Users/me/Documents/x", flags)]);
            assert_eq!(
                out,
                vec![FsEvent::rescan(DirKey::new(7), RescanCause::Overflow)],
                "flags {flags:#x}"
            );
        }
    }

    #[test]
    fn unmount_escalates_with_mount_cause() {
        let b = backend_with_dirs();
        let out = b.translate(vec![raw("/Users/me/Documents", ev::UNMOUNT)]);
        assert_eq!(
            out,
            vec![FsEvent::rescan(DirKey::new(7), RescanCause::MountEvent)]
        );
    }

    #[test]
    fn unknown_parent_rescans_the_nearest_known_ancestor() {
        let b = backend_with_dirs();
        let out = b.translate(vec![raw(
            "/Users/me/NewDir/deep/file",
            ev::ITEM_CREATED,
        )]);
        assert_eq!(
            out,
            vec![FsEvent::rescan(
                DirKey::new(7),
                RescanCause::WatchDropped
            )],
            "an event under an unindexed directory must not be dropped"
        );
    }

    #[test]
    fn duplicate_events_in_one_batch_are_coalesced() {
        let b = backend_with_dirs();
        let out = b.translate(vec![
            raw("/Users/me/Documents/a", ev::ITEM_CREATED),
            raw("/Users/me/Documents/a", ev::ITEM_CREATED),
        ]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn caps_say_whole_mount_without_atomic_rename() {
        let b = FseventsBackend::new().unwrap();
        let c = b.caps();
        assert!(!c.per_dir_watch);
        assert!(c.whole_mount);
        assert!(!c.atomic_rename);
        assert_eq!(b.name(), "fsevents");
    }

    #[test]
    fn an_event_on_the_real_path_resolves_through_a_symlinked_watch() {
        // FSEvents hands the callback real paths, so a directory watched under
        // a symlinked spelling (`/var` → `/private/var`) must still resolve
        // events reported for the target, or every event is silently dropped.
        let tmp = std::env::temp_dir().join(format!("fx-fsevents-{}", std::process::id()));
        let real = tmp.join("real");
        let link = tmp.join("link");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut b = FseventsBackend::new().unwrap();
        b.watch_dir(9, &link).unwrap();

        assert_eq!(
            b.resolve(&real.join("a.txt")),
            Some((9, b"a.txt".to_vec())),
            "an event on the real path must resolve through a symlinked watch"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
