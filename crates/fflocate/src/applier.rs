//! The applier (§7, §13) — the single writer.
//!
//! One thread owns the [`Pending`] collection and the index (behind a
//! `RwLock`). Every mutation — an event, a rescan diff, a crawl — is applied
//! here, in stream order, assigning a journal LSN to each. Queries take the
//! read lock for the duration of a scan (~10–50 ms worst case); writer stalls
//! are absorbed by the bounded channel feeding this thread.
//!
//! Reconciliation lives here too: a `Rescan` is a **diff**, not a replace. It
//! enumerates the real subtree, matches survivors by `(parent, name)`, keeps
//! their ids, and emits synthetic create/delete for genuine differences — so
//! any number of missed events heals in one pass (§7).

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::event::{FsEvent, RescanCause};
use crate::health::{ChangeKind, Health};
use crate::index::{EntryFlags, Index, CREATED_UNKNOWN, ROOT_ID};
use crate::journal::Record;
use crate::pending::{Pending, PushOutcome, WorkItem};
use crate::persist::{apply_record, Store};
use crate::roots::{Root, RootState};
use crate::sync::SyncRegistry;

/// A snapshot of the filesystem view the applier needs, abstracted so the
/// fake-backend tests can drive reconciliation without touching a real FS.
pub trait FsView: Send + Sync {
    /// List `(name, is_dir, is_symlink)` for the directory at `abs_path`.
    fn read_dir(&self, abs_path: &Path) -> io::Result<Vec<DirItem>>;
    /// Canonical identity of a path, if it exists (used to detect renames and
    /// to distinguish files from dirs).
    fn stat(&self, abs_path: &Path) -> io::Result<Option<StatInfo>>;

    /// Ownership of a path, or `None` when the implementation cannot say.
    ///
    /// Only the **system** daemon asks for this (see [`crate::ownership`]): it
    /// costs an `lstat` per entry, which the default crawl otherwise avoids
    /// entirely. The default implementation returns `None`, so a model or fake
    /// filesystem is unaffected — which is what keeps the extra cost opt-in
    /// rather than a tax on every user of every mode.
    fn attr(&self, _abs_path: &Path) -> io::Result<Option<crate::ownership::Attr>> {
        Ok(None)
    }

    /// The two per-entry facts an event's target needs from one `stat`: the
    /// execute bit and the creation minute.
    ///
    /// Defaults to the real filesystem helper. A model or fake view overrides
    /// this so an event-driven upsert can report a date (and an exec bit)
    /// without the path existing on disk; the crawl path takes the same facts
    /// from [`DirItem`], which every view already fills in.
    fn facts(&self, abs_path: &Path, is_dir: bool, is_symlink: bool) -> (bool, u32) {
        fs_facts(abs_path, is_dir, is_symlink)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirItem {
    pub name: Vec<u8>,
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Any execute bit set on the entry (`find -perm /111`). Decided from
    /// `stat`, so symlinks report their *target's* mode (`lstat` says 0777 on
    /// every link); always false for directories (`type:exec` answers "is
    /// this file executable"). Unstatable entries read false.
    pub is_exec: bool,
    /// Creation (birth) time in whole minutes since the unix epoch, or
    /// [`CREATED_UNKNOWN`] when the filesystem has none or the entry is
    /// unstatable. Captured from the same `stat` that decides `is_exec`, so the
    /// crawl does not pay a second syscall for it.
    pub created_min: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatInfo {
    pub is_dir: bool,
    pub is_symlink: bool,
    /// `st_dev`/`st_ino`; zero when unavailable.
    pub dev: u64,
    pub ino: u64,
    /// Creation (birth) time in whole minutes since the unix epoch;
    /// [`CREATED_UNKNOWN`] when unknown.
    pub created_min: u32,
}

/// The real filesystem, using `getdents64` for speed but `stat` for identity.
pub struct RealFs;

impl FsView for RealFs {
    fn read_dir(&self, abs_path: &Path) -> io::Result<Vec<DirItem>> {
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(abs_path) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for ent in rd {
            let ent = ent?;
            let name = os_str_bytes(&ent.file_name());
            let ft = ent.file_type()?;
            let is_dir = ft.is_dir();
            let is_symlink = ft.is_symlink();
            let (is_exec, created_min) = fs_facts(&ent.path(), is_dir, is_symlink);
            out.push(DirItem {
                name,
                is_dir,
                is_symlink,
                is_exec,
                created_min,
            });
        }
        Ok(out)
    }

    fn stat(&self, abs_path: &Path) -> io::Result<Option<StatInfo>> {
        match std::fs::symlink_metadata(abs_path) {
            Ok(md) => {
                // Stable device/inode identity is unix-only (Windows needs
                // GetFileInformationByHandle); elsewhere report zeros, which
                // the struct already means as "unavailable".
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    Ok(Some(StatInfo {
                        is_dir: md.is_dir(),
                        is_symlink: md.file_type().is_symlink(),
                        dev: md.dev(),
                        ino: md.ino(),
                        created_min: created_minutes(&md),
                    }))
                }
                #[cfg(not(unix))]
                {
                    Ok(Some(StatInfo {
                        is_dir: md.is_dir(),
                        is_symlink: md.file_type().is_symlink(),
                        dev: 0,
                        ino: 0,
                        created_min: created_minutes(&md),
                    }))
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn attr(&self, abs_path: &Path) -> io::Result<Option<crate::ownership::Attr>> {
        // uid/gid/mode have no stable-std source off unix: report "cannot
        // say", exactly like the default implementation.
        #[cfg(not(unix))]
        {
            let _ = abs_path;
            Ok(None)
        }
        #[cfg(unix)]
        match std::fs::symlink_metadata(abs_path) {
            Ok(md) => {
                use std::os::unix::fs::MetadataExt;
                Ok(Some(crate::ownership::Attr {
                    uid: md.uid(),
                    gid: md.gid(),
                    mode: md.mode(),
                }))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

pub fn os_str_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        s.as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        s.to_string_lossy().as_bytes().to_vec()
    }
}

/// Decide `IS_EXEC` for one directory entry: any execute bit set
/// (`find -perm /111`), read from `stat` so symlinks report their *target's*
/// mode — `lstat` claims 0777 for every link. Directories are never
/// executable (`type:exec` is a file predicate); unstatable entries and
/// non-unix platforms read false rather than failing the whole listing.
pub fn is_executable(path: &Path, is_dir: bool, is_symlink: bool) -> bool {
    fs_facts(path, is_dir, is_symlink).0
}

/// The two per-entry facts the crawl needs from `stat`: the execute bit and the
/// creation time. One call, one syscall: the crawl already paid for the `stat`
/// to decide `is_exec`, and a second one for the birth time would double it.
///
/// Unstatable entries read `(false, 0)` — no execute bit, no known creation
/// time — exactly the fail-quiet behaviour `is_executable` had.
pub fn fs_facts(path: &Path, is_dir: bool, _is_symlink: bool) -> (bool, u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(path) {
            Ok(md) => (
                !is_dir && md.is_file() && md.mode() & 0o111 != 0,
                created_minutes(&md),
            ),
            Err(_) => (false, CREATED_UNKNOWN),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, is_dir);
        (false, CREATED_UNKNOWN)
    }
}

/// A metadata entry's creation (birth) time as whole minutes since the unix
/// epoch, or [`CREATED_UNKNOWN`] when the platform/filesystem does not record
/// one.
fn created_minutes(md: &std::fs::Metadata) -> u32 {
    match md.created() {
        Ok(t) => created_minutes_at(t),
        Err(_) => CREATED_UNKNOWN,
    }
}

/// The ingestion half of the stored creation-time scale. It is defined once,
/// in [`crate::index`] beside the `u32`-minutes field it writes, and re-exported
/// here because the crawl (which owns the `stat`) is its only caller.
pub use crate::index::created_minutes_at;

/// The shared, query-visible state. Readers take this lock; the applier is the
/// only writer.
pub struct Shared {
    pub index: RwLock<Index>,
    pub roots: RwLock<Vec<Root>>,
    pub generation: AtomicU64,
    pub lsn: AtomicU64,
    pub health: RwLock<Health>,
    /// A broadcast of change events for `subscribe` (cursor-based consumers).
    pub changes: RwLock<crate::health::ChangeLog>,
    /// How many `subscribe` handlers are attached right now. The applier only
    /// pays for change paths (a small walk per mutation) while this is
    /// non-zero; CLI-only daemons see no extra work at all.
    subscribers: AtomicUsize,
    /// §17 v2 trigram accelerator, cached against the index it was built from.
    /// Present only with the default-on `trigram` cargo feature.
    ///
    /// Keyed by **generation only**, and kept current incrementally: every
    /// `Record::Add` funnels through `Applier::emit`, which indexes the new
    /// name's trigrams into the cached postings under the same index lock
    /// that mutates the index. Removals need no work for correctness
    /// (tombstoned ids are filtered by the authoritative matcher), though
    /// `emit` prunes them best-effort so lists do not bloat on churn.
    ///
    /// The `RwLock` inside the `Arc` is what makes concurrent maintenance
    /// safe: queries hold the read guard while intersecting postings, and
    /// `emit` holds the write guard for one entry's trigrams. Without it a
    /// mutation landing mid-query would force dropping the whole cache —
    /// which, on a churning index, would reintroduce the per-query rebuild
    /// this design exists to eliminate.
    ///
    /// A generation bump (whole-root rescan / compaction, which may remap
    /// ids) still invalidates: the next query rebuilds once, in the
    /// background-warmed path if the daemon is idle, on demand otherwise.
    ///
    /// Lock discipline (deadlock observed live, §15): the query path holds the
    /// postings *read* guard across its whole intersection while taking the
    /// index read lock, so `emit` must never take the postings *write* guard
    /// while holding the index write guard. `emit` therefore snapshots the
    /// delta under the index guard, releases it, and only then touches the
    /// slot/postings — slot → postings with nothing else held cannot invert
    /// against postings → index.
    #[cfg(feature = "trigram")]
    pub trigram: RwLock<Option<(u64, Arc<RwLock<crate::trigram::TrigramIndex>>)>>,
    /// Per-entry ownership, for the privileged system instance (§3, §15).
    ///
    /// Read by the query path to answer "may *this* caller see this file?".
    /// Empty unless the daemon runs in system mode, in which case a missing
    /// record means "not shown" — the filter fails closed.
    pub ownership: RwLock<crate::ownership::Ownership>,
}

impl Shared {
    pub fn new(index: Index, roots: Vec<Root>) -> Arc<Shared> {
        let gen = index.generation();
        let lsn = index.last_lsn();
        Arc::new(Shared {
            index: RwLock::new(index),
            roots: RwLock::new(roots),
            generation: AtomicU64::new(gen),
            lsn: AtomicU64::new(lsn),
            health: RwLock::new(Health::default()),
            changes: RwLock::new(crate::health::ChangeLog::new(4096)),
            subscribers: AtomicUsize::new(0),
            #[cfg(feature = "trigram")]
            trigram: RwLock::new(None),
            ownership: RwLock::new(crate::ownership::Ownership::new()),
        })
    }

    /// The trigram accelerator for the current index, building it if the cache
    /// is cold or from an older generation. Cheap after the first call for a
    /// given generation: within a generation the cached postings are kept
    /// current by `Applier::emit`, so the entry count is deliberately *not*
    /// part of the key — a churning index must not pay a full rebuild per query.
    ///
    /// Returns the shared, internally-locked index: queries take the read
    /// guard for the duration of candidate intersection (see the call sites
    /// in `server.rs`), while `emit` takes the write guard for one entry's
    /// trigrams. The index read lock is taken by the caller separately; this
    /// takes only the cache lock and, on a miss, the index lock to build.
    /// Callers must not hold the index read lock while calling this to avoid
    /// a lock-order hazard — the query path builds the accelerator *before*
    /// taking the index lock.
    ///
    /// Only available with the default-on `trigram` cargo feature.
    #[cfg(feature = "trigram")]
    pub fn trigram_index(&self) -> Arc<RwLock<crate::trigram::TrigramIndex>> {
        let gen = self.index.read().unwrap().generation();
        if let Some((g, idx)) = self.trigram.read().unwrap().as_ref() {
            if *g == gen {
                return Arc::clone(idx);
            }
        }
        // Rebuild under the index read lock, then publish.
        let built = {
            let ix = self.index.read().unwrap();
            Arc::new(RwLock::new(crate::trigram::TrigramIndex::build(&ix)))
        };
        let mut slot = self.trigram.write().unwrap();
        // Another thread may have built it while we waited; keep its result.
        if let Some((g, idx)) = slot.as_ref() {
            if *g == gen {
                return Arc::clone(idx);
            }
        }
        *slot = Some((gen, Arc::clone(&built)));
        built
    }

    /// Drop the cached accelerator (used by tests and by explicit invalidation).
    #[cfg(feature = "trigram")]
    pub fn invalidate_trigram(&self) {
        *self.trigram.write().unwrap() = None;
    }

    pub fn snapshot_lsn(&self) -> u64 {
        self.lsn.load(Ordering::Acquire)
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// The current cursor: `c:<gen>:<lsn>`.
    pub fn cursor(&self) -> crate::cursor::Cursor {
        crate::cursor::Cursor::new(self.generation(), self.snapshot_lsn())
    }

    /// Whether any `subscribe` handler is attached (see [`Applier::change_for`]).
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.load(Ordering::Acquire)
    }

    /// Register a subscriber for the lifetime of `guard`.
    ///
    /// Release/acquire on the counter orders an attaching subscriber before
    /// the changes the applier records for it. That narrows — it cannot fully
    /// close — the window where a mutation racing the attach is neither
    /// recorded nor replayed; the subscriber's next change or query settles it.
    pub fn subscribe(self: &Arc<Self>) -> SubscriberGuard {
        self.subscribers.fetch_add(1, Ordering::AcqRel);
        SubscriberGuard {
            shared: Arc::clone(self),
        }
    }
}

/// Keeps [`Shared::subscriber_count`] honest even when a subscribe handler
/// returns early (bad cursor, client gone, emit refused).
pub struct SubscriberGuard {
    shared: Arc<Shared>,
}

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        self.shared.subscribers.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One entry of the applier's dir-path cache.
///
/// Absolute paths duplicate every ancestor's bytes into every descendant
/// (`/a/b/c` stored separately in `c`, and again as a prefix of every path
/// under `c`). This stores each directory as its *own name plus its parent*,
/// so a shared prefix lives once — at the root, which alone holds an absolute
/// path. Materialize with `abs_path_for` (walks to a root, O(depth)).
///
/// Name bytes are lossily converted at insert, exactly as the old
/// absolute-`PathBuf` cache did when it joined them in — only the directory→
/// absolute-path cache used to *reach* the filesystem goes through this;
/// the index itself keeps byte-exact names (§15 deviation 4).
#[derive(Clone, Debug)]
enum DirPathNode {
    /// A root directory: absolute path, seeded from the roots table.
    Root(PathBuf),
    /// Any other known directory: parent id + own (lossy) name.
    Child { parent: u32, name: OsString },
}

/// The applier: single writer over [`Shared`].
pub struct Applier {
    pub shared: Arc<Shared>,
    store: Store,
    pending: Pending,
    fs: Arc<dyn FsView>,
    sync: Arc<SyncRegistry>,
    /// Path cache: dir id → how to reach it. Values are parent-relative
    /// ([`DirPathNode`]), so shared ancestor prefixes are stored once (at the
    /// root) instead of duplicated into every descendant's absolute `PathBuf`.
    /// Use [`Self::abs_path_for`] to materialize; never assume a cached
    /// absolute path.
    dir_paths: HashMap<u32, DirPathNode>,
    /// Root dir id per root index.
    root_dir_ids: Vec<u32>,
    /// Each root's configured path, byte-normalized the same way
    /// [`crate::roots::root_entry_name`] normalizes it, parallel to
    /// `root_dir_ids`. A root nested inside another root is a **boundary** for
    /// the parent's walk (§4): the nested root's own crawl indexes that
    /// subtree, so letting the parent crawl into it indexes every file there
    /// twice, under two different parents. Mount boundaries cannot cover this
    /// — a nested root need not be a mount point, and on a model/plain
    /// directory tree there is no mountinfo at all.
    root_abs: Vec<Vec<u8>>,
    /// When true, journal writes are suppressed (used during initial crawl
    /// replay where the snapshot will capture everything).
    pub journaling: bool,
    /// Count of journal records written since last compaction.
    pub records_since_compaction: u64,
    /// Threshold to trigger compaction.
    pub compaction_threshold: u64,
    /// The token counter for cookies.
    next_cookie_token: u64,
    /// Directories the daemon should watch, discovered during crawl/diff.
    watch_requests: Vec<(u32, PathBuf)>,
    watch_requested: std::collections::HashSet<u32>,
    /// Exclusion rules (§4/§6). An excluded directory is never inserted, which
    /// is what prunes its whole subtree for a whole-mount backend that cannot
    /// decline the watch.
    excludes: crate::excludes::Excludes,
    /// Mount points the crawl must not descend into (§4): each filesystem is
    /// indexed through its own root, so a crawl never crosses into another
    /// mount — no double indexing of nested mounts, no leakage of
    /// never-indexed mounts (network, pseudo-FS) via a parent root. The
    /// mountpoint directory entry itself is still inserted; only the descend
    /// (watch + enumerate) is skipped. Refreshed from mountinfo on the
    /// daemon's mount-watchdog tick; empty (notably under ModelFs/fake
    /// filesystems and on platforms without mountinfo) means no boundaries —
    /// the historical crossing behaviour.
    mountpoints: std::collections::HashSet<Vec<u8>>,
    /// How many names were dropped by [`Self::excludes`]. For `ff stats`.
    pub excluded_names: u64,
    /// Whether to capture ownership as entries are created. Populated only in
    /// system mode, because it costs an `lstat` per entry — a price the personal
    /// mode should not pay. The records themselves live in
    /// [`Shared::ownership`] so the query path can read them.
    track_ownership: bool,
    /// Journal tail decoded at startup but not yet applied (§9 background
    /// replay). `Daemon::build` serves the snapshot immediately and stashes
    /// the tail here; [`Applier::drain`] applies it in bounded chunks before
    /// anything else. Each entry carries its file LSN: the index advances
    /// past exactly the applied prefix (crash → re-skip), while the shared
    /// clock takes the max over replay and live appends (which interleave by
    /// tick past the tail end), keeping cursors monotonic.
    pending_replay: std::collections::VecDeque<(u64, Record)>,
    /// Compaction credit for the startup tail: added to
    /// `records_since_compaction` when the last replay chunk drains, so a
    /// large inherited tail compacts once through the normal `maybe_compact`
    /// path instead of replaying on every boot (the old build-time fold,
    /// deferred until the ids are live and serving).
    replay_compaction_credit: u64,
    /// Directories the OS refused to let us read (macOS TCC, or a plain
    /// `chmod 000`), keyed by absolute path.
    ///
    /// Retrying cannot succeed, and on macOS **every retry re-raises the
    /// "fflocated would like to access files in your Desktop folder" dialog** — so a
    /// refused folder plus a watcher that keeps reporting its contents becomes
    /// an endless prompt loop (list → refuse → event → list again). A refusal
    /// is therefore remembered for the life of the process and the folder is
    /// never touched again: no listing, no `stat` of its children, no watch
    /// request. Entries already indexed stay indexed — "may not look" is not
    /// "deleted".
    denied_dirs: std::collections::HashSet<Vec<u8>>,
    /// Wall ms when the startup replay tail began draining. `None` when there
    /// is no tail. Feeds the daemon's lag floor: a replaying daemon is behind
    /// by however long it has been replaying.
    pending_replay_started_ms: Option<u64>,
}

/// Max wall time one drain call spends before yielding to the rest of the
/// tick (control, watch forwarding, health flags). A count budget cannot bound
/// latency: with linear-scan lookups over a large index a single work item can
/// take seconds, so N items is unbounded minutes of starvation. FIFO order is
/// preserved; only the rate changes.
const DRAIN_TIME_BUDGET: std::time::Duration = std::time::Duration::from_millis(100);

/// One entry's worth of trigram-postings maintenance, snapshotted under the
/// index guard and published after it is released (see the lock-discipline
/// note on [`Shared::trigram`]). Owned bytes throughout: nothing borrows the
/// index past the guard's lifetime.
#[cfg(feature = "trigram")]
enum TrigDelta {
    Add {
        id: u32,
        name: Vec<u8>,
        is_root: bool,
    },
    Del {
        id: u32,
        name: Vec<u8>,
    },
}

impl Applier {
    pub fn new(
        shared: Arc<Shared>,
        store: Store,
        fs: Arc<dyn FsView>,
        sync: Arc<SyncRegistry>,
    ) -> Applier {
        let root_dir_ids = {
            let ix = shared.index.read().unwrap();
            let roots = shared.roots.read().unwrap();
            roots.iter().map(|r| root_dir_id(&ix, r)).collect()
        };
        let mut pending = Pending::new(100_000);
        // The collapse oracle: parent ids straight from the live index.
        // `RwLock` + `Arc` because `ParentOf` is `Fn` (shared, not exclusive)
        // and `Applier::new` only has `shared` by move — the closure clones it.
        let ix_for_parents = shared.clone();
        pending.set_parent_of(std::sync::Arc::new(move |id: u32| {
            let ix = ix_for_parents.index.read().unwrap();
            let e = ix.entry(id)?;
            // Root dirs are the ceiling: report them as their own parent so
            // collapse lands on them rather than climbing past into ROOT_ID.
            if e.flags().is_root_dir() {
                return Some(id);
            }
            let p = e.parent;
            (p != ROOT_ID && p != id).then_some(p)
        }));
        let root_abs = root_abs_of(&shared);
        Applier {
            shared,
            store,
            pending,
            fs,
            sync,
            dir_paths: HashMap::new(),
            root_dir_ids,
            root_abs,
            journaling: true,
            records_since_compaction: 0,
            compaction_threshold: 100_000,
            next_cookie_token: 1,
            watch_requests: Vec::new(),
            watch_requested: std::collections::HashSet::new(),
            excludes: crate::excludes::Excludes::new(),
            excluded_names: 0,
            mountpoints: std::collections::HashSet::new(),
            track_ownership: false,
            pending_replay: std::collections::VecDeque::new(),
            replay_compaction_credit: 0,
            pending_replay_started_ms: None,
            denied_dirs: std::collections::HashSet::new(),
        }
    }

    /// Has the OS already refused this directory? Checked before every syscall
    /// on it, because the syscall itself is what re-prompts on macOS.
    fn is_denied(&self, abs: &Path) -> bool {
        self.denied_dirs.contains(&os_str_bytes(abs.as_os_str()))
    }

    /// Remember (once) that `abs` cannot be read, and say so in the daemon log.
    fn note_denied(&mut self, abs: &Path) {
        const HINT: &str = if cfg!(target_os = "macos") {
            " (grant Full Disk Access in System Settings → Privacy & Security to index it)"
        } else {
            ""
        };
        if self.denied_dirs.insert(os_str_bytes(abs.as_os_str())) {
            eprintln!(
                "fflocated: not indexing {}: permission denied{HINT}; not retrying",
                abs.display()
            );
        }
    }

    pub fn pending(&self) -> &Pending {
        &self.pending
    }

    /// Install the exclusion rules (§4/§6). Called before the initial crawl so
    /// an excluded subtree is never enumerated in the first place.
    pub fn set_excludes(&mut self, e: crate::excludes::Excludes) {
        self.excludes = e;
    }

    pub fn excludes(&self) -> &crate::excludes::Excludes {
        &self.excludes
    }

    /// Install the mount-boundary set (§4). Called before the initial crawl
    /// and refreshed as mounts change, so a crawl never crosses into another
    /// filesystem. Backends must likewise not deliver events for unindexed
    /// mounts (inotify watches and fanotify marks only ever cover descended
    /// roots, so this holds by construction).
    pub fn set_mountpoints(&mut self, mps: std::collections::HashSet<Vec<u8>>) {
        self.mountpoints = mps;
    }

    /// Is `abs` a mount point (and therefore not to be descended into)?
    /// Exact byte match against the mountinfo spellings; mountinfo paths are
    /// unescaped and slash-stripped by the parser helpers, and crawl-built
    /// paths never carry a trailing slash.
    fn is_mountpoint(&self, abs: &std::path::Path) -> bool {
        self.mountpoints.contains(&os_str_bytes(abs.as_os_str()))
    }

    /// May `dir` (at `abs`) be enumerated? Two kinds of boundary stop a walk,
    /// and both leave the directory entry itself recorded (only the descend —
    /// enumerate, watch, queue — is skipped), because each subtree is indexed
    /// by whatever owns it:
    ///
    /// * a **mount point** that is not itself a root: another filesystem, and
    ///   the parent root must not read across it (§4);
    /// * a **root nested inside another root**: that subtree is its own
    ///   root's to index. Without this the outer root's crawl walks straight
    ///   into it and every file beneath it is inserted twice, under two
    ///   different parents — one copy per enclosing root. A nested root need
    ///   not be a mount point (`ff roots add ~/src` under a `/home` root), and
    ///   on a model or plain-directory tree there is no mountinfo at all, so
    ///   the mount check alone cannot cover this.
    ///
    /// A directory that *is itself* a root is always descended into: it is
    /// where that root's walk starts.
    fn may_descend(&self, dir: u32, abs: &std::path::Path) -> bool {
        // A folder the OS refused is never opened again, not even to confirm
        // the refusal: on macOS the open is exactly what raises the dialog.
        if self.is_denied(abs) {
            return false;
        }
        if self.root_dir_ids.contains(&dir) {
            return true;
        }
        !self.is_mountpoint(abs) && !self.is_foreign_root(dir, abs)
    }

    /// Is `abs` the configured path of a root other than the one `dir` belongs
    /// to? Such a directory is a boundary for this walk (see [`Self::may_descend`]).
    ///
    /// Matched by absolute path rather than by id, because the entry reached
    /// through this walk is a *different* index entry from the root's own
    /// root-dir entry: `/model/a` crawled from `/model` is a child of the
    /// `/model` root-dir, while the root added for `/model/a` is a top-level
    /// entry named `/model/a`. Path is the identity the roots table is keyed
    /// by (see [`crate::roots::root_entry_name`]), so that is what is compared
    /// — byte-normalized through the same function, so a trailing slash or a
    /// `\\` separator in the configuration cannot miss.
    ///
    /// The root `dir` itself belongs to is not a boundary: a walk into its own
    /// root's configured path is what a crawl of that root *is*.
    fn is_foreign_root(&self, dir: u32, abs: &std::path::Path) -> bool {
        if self.root_abs.is_empty() {
            return false;
        }
        let here = root_abs_bytes(abs);
        // A walk never reaches *itself* — `dir` being a root returned above —
        // but a path reached twice (a store with two roots at one path) must
        // not be treated as foreign to itself either.
        let own_root = self
            .root_index_of_dir(dir)
            .and_then(|i| self.root_abs.get(i))
            .is_some_and(|p| *p == here);
        if own_root {
            return false;
        }
        self.root_abs.iter().any(|p| *p == here)
    }

    /// Turn on per-entry ownership capture (system mode). Once on, every entry
    /// the crawler creates records its `uid`/`gid`/`mode`.
    ///
    /// This is deliberately a switch, not a default: it is the difference
    /// between a crawl that stats nothing and one that stats every file, and
    /// only the privileged system index has a reason to pay for it.
    ///
    /// It also records the **root directories themselves**. That is easy to
    /// forget and fatal to miss: every entry's visibility is checked up the
    /// parent chain, so a root with no ownership record hides its entire subtree
    /// — the filter would look like it worked while showing nobody anything.
    pub fn track_ownership(&mut self, on: bool) {
        self.track_ownership = on;
        if !on {
            return;
        }
        let roots = self.shared.roots.read().unwrap().clone();
        for r in &roots {
            let rid = root_dir_id(&self.shared.index.read().unwrap(), r);
            if rid != ROOT_ID {
                self.note_ownership(rid, &r.path);
            }
        }
    }

    pub fn tracks_ownership(&self) -> bool {
        self.track_ownership
    }

    /// Record ownership for one entry. A no-op unless tracking is on.
    fn note_ownership(&mut self, id: u32, abs: &Path) {
        if !self.track_ownership {
            return;
        }
        if let Ok(Some(a)) = self.fs.attr(abs) {
            // The query path reads this table; the applier is its only writer.
            self.shared.ownership.write().unwrap().set(id, a);
        }
    }

    pub fn pending_mut(&mut self) -> &mut Pending {
        &mut self.pending
    }

    /// Stash a decoded journal tail for background replay (§9). Called once
    /// by `Daemon::build` after serving the snapshot; `credit` is the tail's
    /// record count, added to `records_since_compaction` when the last chunk
    /// drains so the inherited tail compacts once via the normal path.
    pub fn set_pending_replay(&mut self, tail: Vec<(u64, Record)>, credit: u64) {
        self.pending_replay_started_ms = if tail.is_empty() {
            None
        } else {
            Some(now_ms())
        };
        self.pending_replay = tail.into();
        self.replay_compaction_credit = credit;
    }

    /// When the startup replay tail began draining, if it still is. A daemon
    /// replaying history is behind by definition, so this joins the queue
    /// age as the daemon's lag floor — a replaying daemon reports how long
    /// it has been replaying rather than "caught up".
    pub fn pending_replay_started_ms(&self) -> Option<u64> {
        if self.pending_replay.is_empty() {
            None
        } else {
            self.pending_replay_started_ms
        }
    }

    /// Records still waiting for background replay. `catching_up` stays true
    /// while this is non-empty.
    pub fn pending_replay_len(&self) -> usize {
        self.pending_replay.len()
    }

    /// Watch requests recorded during crawl/diff so the daemon can ask the
    /// watcher thread to install watches (the applier never touches the backend).
    pub fn take_watch_requests(&mut self) -> Vec<(u32, std::path::PathBuf)> {
        std::mem::take(&mut self.watch_requests)
    }

    fn record_watch_request(&mut self, id: u32, path: PathBuf) {
        if self.watch_requested.insert(id) {
            self.watch_requests.push((id, path));
        }
    }

    /// Rebuild the dir-path cache from the index roots. Called after load and
    /// after every structural change to the roots table.
    ///
    /// Only real directory ids are cached (root dirs seeded from the roots
    /// table; the rest discovered by walking parent chains during crawls).
    pub fn rebuild_dir_cache(&mut self) {
        self.dir_paths.clear();
        let ix = self.shared.index.read().unwrap();
        let roots = self.shared.roots.read().unwrap();
        self.root_dir_ids = roots.iter().map(|r| root_dir_id(&ix, r)).collect();
        self.root_abs = roots.iter().map(|r| root_abs_bytes(&r.path)).collect();
        for (i, r) in roots.iter().enumerate() {
            let rid = self.root_dir_ids[i];
            if rid != ROOT_ID {
                self.dir_paths.insert(rid, DirPathNode::Root(r.path.clone()));
            }
        }
    }

    /// Absolute path of directory `id`, by walking parent links to the
    /// nearest root. Non-UTF-8 names lose fidelity through this path (only used to
    /// reach real directories); the byte-exact crawler path does not.
    ///
    /// Terminates on a broken or cyclic parent chain by returning `None`: an id
    /// repeating mid-walk proves a cycle (`a`↔`b` needs no self-parent to loop
    /// a naive walk forever), and a misconfigured root must be an error, not
    /// a stack overflow.
    fn abs_path_for(&mut self, id: u32) -> Option<PathBuf> {
        // Collect (id, parent, own-name) links from `id` up to (not
        // including) the root, following cached links first and falling back
        // to the index itself for uncached ids (a dir known to the index but
        // not yet crawled this session — e.g. after a snapshot load — must
        // still resolve). Memoizes the walked chain on success, so repeated
        // resolves are amortized O(1): the old absolute-PathBuf cache
        // memoized too, but at the cost of duplicating every ancestor's bytes
        // per entry, while each entry here is just (parent, own name).
        let mut chain: Vec<(u32, u32, OsString)> = Vec::new();
        let mut cur = id;
        let root_abs = loop {
            // Cycle guard: an id repeating in this walk proves a parent
            // cycle (a↔b needs no self-parent to loop forever). Quadratic
            // but depths are ~single digits; a misconfigured root must be an
            // error, not a hang.
            if chain.iter().any(|(c, _, _)| *c == cur) {
                return None;
            }
            let (parent, name) = match self.dir_paths.get(&cur) {
                Some(DirPathNode::Root(p)) => break p.clone(),
                Some(DirPathNode::Child { parent, name }) => (*parent, name.clone()),
                None => {
                    let ix = self.shared.index.read().unwrap();
                    let e = ix.entry(cur)?;
                    // Lossy here, exactly as the old cache did when it joined
                    // names in (see DirPathNode docs); the index keeps
                    // byte-exact names. Via String so this builds off Unix too
                    // (OsString::from(Vec<u8>) is Unix-only).
                    let name = OsString::from(String::from_utf8_lossy(ix.name(cur)).into_owned());
                    (e.parent, name)
                }
            };
            chain.push((cur, parent, name));
            cur = parent;
        };
        for (cid, parent, name) in &chain {
            self.dir_paths.insert(
                *cid,
                DirPathNode::Child {
                    parent: *parent,
                    name: name.clone(),
                },
            );
        }
        let mut p = root_abs;
        for (_, _, name) in chain.iter().rev() {
            p.push(name);
        }
        Some(p)
    }

    /// Record that directory `id` (a child of `parent`) is named `name`.
    /// Overwrites any previous entry — re-crawls re-learn the same link.
    fn insert_child_path(&mut self, id: u32, parent: u32, name: &[u8]) {
        self.dir_paths.insert(
            id,
            DirPathNode::Child {
                parent,
                name: OsString::from(String::from_utf8_lossy(name).into_owned()),
            },
        );
    }

    fn emit(&mut self, rec: Record) {
        if self.journaling {
            match self.store.append(&rec) {
                Ok(lsn) => {
                    // High-water mark, never an exact store: during §9
                    // background replay the tail applies historical LSNs
                    // while live writes append past the tail end — two
                    // sources feed one clock, so only max-advance keeps
                    // cursors monotonic (see `drain_replay_chunk`). In
                    // steady state the journal LSN always exceeds, and this
                    // is a plain store.
                    self.shared
                        .lsn
                        .fetch_max(lsn, Ordering::Release);
                    self.records_since_compaction += 1;
                }
                Err(_) => {
                    self.shared.health.write().unwrap().journal_write_errors += 1;
                }
            }
        }
        self.apply_live(rec);
    }

    /// Apply one record to the live index without journaling it.
    ///
    /// The shared tail of [`Applier::emit`] (which appends to the journal
    /// first) and background journal replay: ownership, trigram maintenance,
    /// and the index mutation itself. One funnel — a second copy would
    /// diverge, and the ownership bug this funnel fixed is the precedent.
    fn apply_live(&mut self, rec: Record) {
        // Ownership is recorded here, at the single funnel every creation passes
        // through, rather than at each of the three call sites (crawl, rescan
        // diff, and the event path). Having it in only some of them was a real
        // bug: a file discovered by a *rescan* had no record, and the filter —
        // which fails closed — then hid a world-readable file from everyone but
        // root. One funnel means a fourth creation path cannot forget.
        if let Record::Add {
            id, parent, name, ..
        } = &rec
        {
            self.note_ownership_for(*id, *parent, name);
        }
        // §11 live feed: describe the mutation *before* it lands, so a delete
        // can still name the entry it removes (afterwards only the tombstone
        // and its name bytes survive, with no parent to walk). Skipped unless a
        // subscriber is attached, so a CLI-only daemon pays nothing.
        let changes = self.change_for(&rec);
        // Apply to the live index regardless of journaling.
        // §17 v2 incremental maintenance: keep the cached trigram postings
        // current, so the cache stays keyed on generation alone and a churning
        // index never pays a full rebuild. Done here — the single funnel every
        // mutation passes through — so no creation path can forget (the same
        // reason ownership is noted here).
        //
        // Lock order matters more than locality: the query path holds the
        // postings read guard across its whole intersection while taking the
        // index read lock (`handle_query`), so taking the postings write guard
        // while holding the index write guard here is an ABBA deadlock — a
        // query intersecting over millions of entries overlaps this window
        // with certainty, wedging the applier, the querier and every later
        // reader (observed live: frozen CPU, handlers piling up, stats
        // silent). The snapshot-then-publish below keeps the funnel but drops
        // the index guard first: the slot check re-validates the generation
        // after re-locking, so a bump in between only skips one incremental
        // update (the next query rebuilds), never corrupts.
        #[cfg(feature = "trigram")]
        let trigram_delta: Option<(u64, TrigDelta)> = {
            use crate::journal::Record as Rec;
            // Read-only: the mutation happens in the next block. A read guard
            // also keeps query readers flowing past the snapshot.
            let ix = self.shared.index.read().unwrap();
            let delta = match &rec {
                Rec::Add {
                    id, name, flags, ..
                } => {
                    let is_root = crate::index::EntryFlags(*flags).is_root_dir();
                    Some(TrigDelta::Add {
                        id: *id,
                        name: name.clone(),
                        is_root,
                    })
                }
                Rec::Del { id } => {
                    // Best-effort prune (correctness never depends on it: the
                    // matcher filters tombstones). The name bytes survive
                    // tombstoning, so copy the name it had while guarded.
                    if (*id as usize) < ix.len() {
                        Some(TrigDelta::Del {
                            id: *id,
                            name: ix.name(*id).to_vec(),
                        })
                    } else {
                        None
                    }
                }
                // A rename keeps the id but changes the name: the old
                // postings go stale-but-safe (filtered by the authoritative
                // matcher), and the new name's trigrams must be added or the
                // entry becomes invisible to the accelerator.
                Rec::Move { id, new_name, .. } => Some(TrigDelta::Add {
                    id: *id,
                    name: new_name.clone(),
                    is_root: false,
                }),
                Rec::Fence | Rec::Mark { .. } => None,
            };
            // A generation bump (whole-root rescan / compaction, which may
            // remap ids) invalidates; the next query rebuilds. Within a
            // generation, ids only ascend, so postings only gain.
            delta.map(|d| (ix.generation(), d))
        };
        {
            let mut ix = self.shared.index.write().unwrap();
            apply_record(&mut ix, &rec);
        }
        #[cfg(feature = "trigram")]
        if let Some((gen, delta)) = trigram_delta {
            // Index guard already released: slot → postings nests with
            // nothing else held, so no lock order can invert.
            let slot = self.shared.trigram.read().unwrap();
            if let Some((g, idx)) = slot.as_ref() {
                if *g == gen {
                    let mut tg = idx.write().unwrap();
                    match delta {
                        TrigDelta::Add { id, name, is_root } => {
                            tg.index_entry(id, &name, is_root);
                        }
                        TrigDelta::Del { id, name } => {
                            tg.remove_entry(id, &name);
                        }
                    }
                }
            }
        }
        #[cfg(not(feature = "trigram"))]
        {
            let mut ix = self.shared.index.write().unwrap();
            apply_record(&mut ix, &rec);
        }
        if let Some(changes) = changes {
            self.publish_changes(changes);
        }
    }

    /// The live-feed view of one record: what a `subscribe` client should hear.
    ///
    /// `None` when nothing is subscribed, when the record carries no path
    /// change (`Fence`/`Mark`), or when the path cannot be resolved (an event
    /// for a parent the index does not know yet): silence is better than a
    /// path that is not the file's.
    fn change_for(&mut self, rec: &Record) -> Option<Vec<(ChangeKind, Vec<u8>, bool, bool)>> {
        if self.shared.subscriber_count() == 0 {
            return None;
        }
        match rec {
            Record::Add {
                parent,
                name,
                flags,
                ..
            } => {
                let path = self.child_path(*parent, name)?;
                let f = crate::index::EntryFlags(*flags);
                Some(vec![(ChangeKind::Create, path, f.is_dir(), f.is_exec())])
            }
            Record::Del { id } => {
                let (path, is_dir, is_exec) = self.change_path_of(*id)?;
                Some(vec![(ChangeKind::Delete, path, is_dir, is_exec)])
            }
            Record::Move {
                id,
                new_parent,
                new_name,
            } => {
                // A rename is a delete plus a create: that is exactly what a
                // follower of one path needs, and the journal's `Move` is rare
                // (persist/compaction), so the extra event costs nothing. The
                // entry keeps its kind across the rename, so the create half
                // carries the same flags — a `kind:dir` subscription would
                // otherwise filter out a moved directory.
                let mut out = Vec::new();
                if let Some((path, is_dir, is_exec)) = self.change_path_of(*id) {
                    out.push((ChangeKind::Delete, path, is_dir, is_exec));
                    if let Some(path) = self.child_path(*new_parent, new_name) {
                        out.push((ChangeKind::Create, path, is_dir, is_exec));
                    }
                }
                (!out.is_empty()).then_some(out)
            }
            Record::Fence | Record::Mark { .. } => None,
        }
    }

    /// `parent`'s absolute path plus `name`, as raw bytes.
    fn child_path(&mut self, parent: u32, name: &[u8]) -> Option<Vec<u8>> {
        let parent_path = self.abs_path_for(parent)?;
        let mut path = parent_path.to_string_lossy().into_owned().into_bytes();
        if path.last() != Some(&b'/') {
            path.push(b'/');
        }
        path.extend_from_slice(name);
        Some(path)
    }

    /// Materialize `id`'s absolute path (and its kind) from the index.
    ///
    /// Only used for deletions, where the record itself names no path: the
    /// entry is still live at this point, so walking its parent chain (and
    /// tying the chain's root-dir entry back to the root table for the prefix)
    /// is exact. Two short read guards, taken before the write that removes it;
    /// never nested under another index guard.
    fn change_path_of(&mut self, id: u32) -> Option<(Vec<u8>, bool, bool)> {
        let ix = self.shared.index.read().unwrap();
        let flags = ix.entry(id)?.flags();
        let mut names: Vec<Vec<u8>> = Vec::new();
        let mut cur = id;
        let root_dir = loop {
            if cur == ROOT_ID {
                return None;
            }
            let entry = ix.entry(cur)?;
            if entry.flags().is_root_dir() {
                break cur;
            }
            if entry.flags().is_tombstone() {
                return None;
            }
            names.push(ix.name(cur).to_vec());
            cur = entry.parent;
        };
        let roots = self.shared.roots.read().unwrap();
        let root = roots
            .iter()
            .find(|r| root_dir_id(&ix, r) == root_dir)?;
        let mut path = root.path.as_os_str().to_string_lossy().into_owned().into_bytes();
        for name in names.iter().rev() {
            if path.last() != Some(&b'/') {
                path.push(b'/');
            }
            path.extend_from_slice(name);
        }
        Some((path, flags.is_dir(), flags.is_exec()))
    }

    /// Publish one mutation's changes at the current cursor.
    fn publish_changes(&mut self, changes: Vec<(ChangeKind, Vec<u8>, bool, bool)>) {
        let cursor = self.shared.cursor();
        let mut log = self.shared.changes.write().unwrap();
        for (kind, path, is_dir, is_exec) in changes {
            log.push(crate::health::Change {
                seq: 0,
                cursor,
                kind,
                path,
                is_dir,
                is_exec,
            });
        }
    }

    /// Record ownership for a newly inserted entry, resolving its absolute path
    /// from the parent's cached path.
    ///
    /// Best-effort by design: if the parent path is unknown or the stat fails, no
    /// record is written and the entry is simply not shown to unprivileged
    /// callers. That is the safe direction, and it is *visible* — the ownership
    /// table's size is reported by `ff stats`.
    fn note_ownership_for(&mut self, id: u32, parent: u32, name: &[u8]) {
        if !self.track_ownership {
            return;
        }
        let Some(parent_path) = self.abs_path_for(parent) else {
            return;
        };
        let abs = parent_path.join(std::ffi::OsString::from(
            String::from_utf8_lossy(name).into_owned(),
        ));
        if let Ok(Some(a)) = self.fs.attr(&abs) {
            // The query path reads this table; the applier is its only writer.
            self.shared.ownership.write().unwrap().set(id, a);
        }
    }

    /// Apply one event.
    fn apply_event(&mut self, ev: FsEvent) {
        match ev {
            FsEvent::Upsert {
                parent,
                name,
                is_dir,
            } => {
                self.apply_upsert(parent.0, name, is_dir);
            }
            FsEvent::RefreshExec { parent, name } => {
                self.apply_refresh_exec(parent.0, &name);
            }
            FsEvent::Remove { parent, name } => {
                let ix = self.shared.index.write().unwrap();
                let removed = ix.lookup(parent.0, &name);
                drop(ix);
                if let Some(id) = removed {
                    self.emit(Record::Del { id });
                }
            }
            FsEvent::RescanSubtree { dir, cause } => {
                self.pending.enqueue_rescan(dir.0, cause);
            }
            FsEvent::RootGone { dir } => {
                // Drop the subtree rooted at this directory. If it *is* a root
                // dir, the whole root goes; otherwise just the mounted subtree.
                if dir.0 != ROOT_ID {
                    self.drop_subtree(dir.0);
                }
            }
        }
    }

    fn apply_upsert(&mut self, parent: u32, name: Vec<u8>, is_dir_hint: bool) {
        if crate::util::is_cookie(&name) {
            // The cookie's create event is a marker: it carries parent = the
            // root dir id, which is exactly the key the sync engine registered
            // the cookie under. Cookies are never indexed.
            if let Some(token) = self.sync.token_for_name(parent, &name) {
                self.pending.mark_cookie_seen(token);
            }
            return;
        }
        let ix = self.shared.index.write().unwrap();
        if parent as usize >= ix.entry_count() || !ix.is_alive(parent) || !ix.is_dir(parent) {
            return; // event for a dir we do not (yet) know
        }
        // The same exclusion rule as the crawl (§4/§6): a whole-mount backend
        // cannot decline the watch, so an excluded directory must be refused
        // here too. Because it is never inserted, its children never resolve,
        // and the subtree stays pruned without an ancestor walk.
        if is_dir_hint && self.excludes.excludes_dir_name(&name) {
            self.excluded_names += 1;
            return;
        }
        // Use the hint to pick flags; the crawler will correct type later.
        // Make sure the parent's path is cached before the entry is emitted, so
        // `note_ownership_for` can resolve this file's absolute path. This takes
        // the index read lock, so it must happen before we take the write lock.
        let parent_known = ix.is_alive(parent) && ix.is_dir(parent);
        drop(ix);
        if !parent_known {
            return;
        }
        // An event under a folder we may not read would make us `stat` a path
        // inside it, which raises the macOS permission dialog again. The
        // watcher reports those folders happily (FSEvents needs no TCC grant),
        // so the event is dropped before any syscall.
        if self
            .abs_path_for(parent)
            .map(|p| self.is_denied(&p))
            .unwrap_or(false)
        {
            return;
        }
        let _ = self.abs_path_for(parent);
        // The exec bit is statted now rather than defaulting false: a fresh
        // file that arrives via event (not crawl) must carry its real bit
        // from birth, not only after the next rescan. The parent path was
        // just cached above, so this is one `stat`, not a walk.
        let fs = self.fs.clone();
        let (exec_hint, created_min) = self
            .abs_path_for(parent)
            .map(|p| {
                fs.facts(
                    &p.join(std::ffi::OsString::from(
                        String::from_utf8_lossy(&name).into_owned(),
                    )),
                    is_dir_hint,
                    false,
                )
            })
            .unwrap_or((false, CREATED_UNKNOWN));
        let flags = EntryFlags::EMPTY.with_dir(is_dir_hint).with_exec(exec_hint);
        let mut ix = self.shared.index.write().unwrap();
        let lookup = ix.lookup(parent, &name);
        let (created_id, refreshed, existing_id) = match lookup {
            Some(id) => {
                // Refresh the type if it changed (e.g. file → dir via rename).
                let cur = ix.entry(id).unwrap().flags();
                if cur.is_dir() != is_dir_hint {
                    let nf = EntryFlags(cur.0).with_dir(is_dir_hint);
                    ix.entry_mut(id).flags = nf.0;
                }
                // A deleted-and-recreated file keeps its id but has a new birth
                // time. Refresh it in place and journal it as the same `Add` a
                // fresh entry gets, so the new minute is durable in the same
                // append as the name instead of waiting for a snapshot (or a
                // rescan that relearns it). Never overwrite a known time with
                // "unknown" (a filesystem that does not report birth times),
                // and only when it actually changed: a create event for an
                // unchanged file must not grow the journal.
                let refreshed = if created_min != CREATED_UNKNOWN
                    && ix.created_min(id) != Some(created_min)
                {
                    ix.set_created(id, created_min);
                    // The *live* flags, not the event's hint: this id already
                    // carries IS_SYMLINK / IS_ROOT_DIR bits the hint omits, and
                    // replay resurrects the entry with exactly this record.
                    Some(Record::Add {
                        id,
                        parent,
                        name: name.clone(),
                        flags: ix.entry(id).unwrap().flags().0,
                        created: created_min,
                    })
                } else {
                    None
                };
                (None, refreshed, Some(id))
            }
            None => {
                let id = ix.insert(parent, &name, flags);
                (Some(id), None, None)
            }
        };
        drop(ix);
        if let Some(id) = created_id {
            let rec = Record::Add {
                id,
                parent,
                name: name.clone(),
                flags: flags.0,
                created: created_min,
            };
            self.emit(rec);
        } else if let Some(rec) = refreshed {
            // `emit` notes ownership for an Add as well, so the refresh needs
            // no separate call here.
            self.emit(rec);
        } else if let Some(id) = existing_id {
            // An upsert for an entry we already have. The name is unchanged, but
            // the *thing* may not be: a file deleted and recreated, or replaced by
            // a rename, can have different ownership. Refresh it, or the filter
            // keeps answering with the old answer — which for a chowned file means
            // showing it to someone who has since lost access, the one direction
            // that must not happen.
            self.note_ownership_for(id, parent, &name);
        }

        // A directory, new or re-typed: record its path so a later crawl/diff can
        // reach it, ask the backend to watch it, and **crawl it** to learn its
        // children.
        //
        // The crawl is not an optimization, it is load-bearing for a FID backend:
        // a fanotify event names its parent by *file handle*, and the handle for a
        // directory created after we started watching is only learned by asking
        // for it — which the crawl does. Without this, every event for the new
        // directory's children is unresolvable, and the subtree goes silently
        // unseen. (A per-dir-watch backend gets the equivalent from its watch.)
        if is_dir_hint {
            let existing = {
                let ix = self.shared.index.read().unwrap();
                ix.lookup(parent, &name)
            };
            if let Some(id) = existing {
                self.insert_child_path(id, parent, &name);
                if let Some(child_abs) = self.abs_path_for(id) {
                    self.record_watch_request(id, child_abs);
                }
                self.pending.enqueue_crawl(id);
            }
        }
        // (the id is not needed past this point; the crawl is what matters)
    }

    fn drop_subtree(&mut self, dir: u32) {
        let ids = {
            let ix = self.shared.index.read().unwrap();
            let mut v = vec![dir];
            v.extend(descendants(&ix, dir));
            v
        };
        for id in ids.into_iter().rev() {
            self.emit(Record::Del { id });
        }
    }

    /// Drop the entries an index written by an older build kept beneath a file
    /// or a symlink, and return how many went.
    ///
    /// Nothing current descends into a symlink, so nothing current inserts
    /// beneath one; a store that does hold such entries was written before that
    /// rule. The case that bites is Wine's `dosdevices/z:` → `/`: a walked
    /// prefix carries one alias of the whole filesystem (`…/z:/tmp/x`) beside
    /// the real `/tmp/x`, which turns one search hit into forty. No crawl or
    /// rescan ever visits them again (there is no directory there to enumerate
    /// and no parent entry to diff), so the sweep at startup is the only thing
    /// that can remove them.
    pub fn drop_entries_under_non_dirs(&mut self) -> usize {
        let orphaned: Vec<u32> = {
            let ix = self.shared.index.read().unwrap();
            ix.entries_under_non_dirs()
        };
        for id in orphaned.iter().rev() {
            self.emit(Record::Del { id: *id });
        }
        orphaned.len()
    }

    /// Re-read one entry's exec bit after a metadata change (`chmod`), correcting    /// Re-read one entry's exec bit after a metadata change (`chmod`), correcting
    /// the flag in place.
    ///
    /// Deliberately journal-free: the bit is derived state, re-derivable from the
    /// filesystem on every crawl and rescan diff. A replayed journal that
    /// re-applies Add records rebuilds the same flags from the crawl path, so a
    /// `RefreshExec` record would be redundant — and worse, it would pin a stale
    /// bit across a restart instead of re-reading the live one. The rescan diff
    /// (which corrects the same flag on survivors) is the durability story.
    fn apply_refresh_exec(&mut self, parent: u32, name: &[u8]) {
        if crate::util::is_cookie(name) {
            return;
        }
        let id = {
            let ix = self.shared.index.read().unwrap();
            match ix.lookup(parent, name) {
                Some(id) => id,
                None => return, // not indexed: nothing to correct
            }
        };
        let (is_dir, cur_exec) = {
            let ix = self.shared.index.read().unwrap();
            match ix.entry(id) {
                Some(e) => (e.flags().is_dir(), e.flags().is_exec()),
                None => return,
            }
        };
        if is_dir {
            return; // directories are never executable
        }
        let abs = match self.abs_path_for(parent) {
            Some(p) => p.join(std::ffi::OsString::from(
                String::from_utf8_lossy(name).into_owned(),
            )),
            None => return,
        };
        let fresh = is_executable(&abs, false, false);
        if fresh != cur_exec {
            let mut ix = self.shared.index.write().unwrap();
            if let Some(e) = ix.entry(id) {
                let nf = e.flags().with_exec(fresh);
                ix.entry_mut(id).flags = nf.0;
            }
        }
    }

    // ------------------------------------------------------------ work items

    /// Run one work item. Returns true if it made progress (a deferred rescan
    /// returns false so the drain loop can stop and retry on the next tick).
    fn run_work(&mut self, w: WorkItem) -> bool {
        match w {
            WorkItem::CrawlDir { dir } => {
                self.crawl_dir(dir);
                true
            }
            WorkItem::Rescan { dir, cause } => self.rescan(dir, cause),
            WorkItem::DropSubtree { dir } => {
                self.drop_subtree(dir);
                true
            }
        }
    }

    /// Add any children of `dir` not yet indexed. Used after an upsert of a new
    /// directory, and by the initial crawl's work loop.
    fn crawl_dir(&mut self, dir: u32) {
        let Some(abs) = self.abs_path_for(dir) else {
            return;
        };
        // Record this dir's own path first, so `note_ownership_for` can resolve a
        // child's absolute path the moment it is emitted — and record the
        // directory's own ownership too. Every visibility check walks the parent
        // chain, so a directory without a record hides its whole subtree; after a
        // restart nothing is "new", and the root dir in particular would have no
        // record at all unless the walk that descends into it records it.
        // (`abs_path_for` memoized `dir`'s chain above: no re-insert needed.)
        self.note_ownership(dir, &abs);
        // Mount boundary (§4): another filesystem is never ours to enumerate —
        // its own root covers it, or it is not indexed at all. The directory
        // itself stays recorded (see above); only the descend stops here. This
        // runs before the listing so a huge foreign mount is never even read.
        if !self.may_descend(dir, &abs) {
            return;
        }
        let items = match self.fs.read_dir(&abs) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                // macOS TCC / an unreadable folder: permanent for this process.
                // Counting it as a transient error and retrying is what makes
                // the permission dialog appear again and again.
                self.note_denied(&abs);
                return;
            }
            Err(_) => {
                self.shared.health.write().unwrap().read_dir_errors += 1;
                return;
            }
        };
        let mut new_dirs = Vec::new();
        let mut known_dirs = Vec::new();
        for it in items {
            if it.name == b"." || it.name == b".." {
                continue;
            }
            // Sync cookies must never be indexed (§8, §16); `diff_subtree` and
            // the event path filter them, and so must the initial/subtree crawl.
            if crate::util::is_cookie(&it.name) {
                continue;
            }
            // Exclusions (§4/§6): an excluded *directory* is never inserted, and
            // because events name their parent by id, that one omission prunes
            // its entire subtree — including for a whole-mount backend that
            // could not have declined the watch.
            if it.is_dir && !it.is_symlink && self.excludes.excludes_dir_name(&it.name) {
                self.excluded_names += 1;
                continue;
            }
            if self.excludes.excludes_path(
                abs.join(std::ffi::OsString::from(
                    String::from_utf8_lossy(&it.name).into_owned(),
                ))
                .to_string_lossy()
                .as_bytes(),
            ) {
                self.excluded_names += 1;
                continue;
            }
            let exists = {
                let ix = self.shared.index.read().unwrap();
                ix.lookup(dir, &it.name)
            };
            if exists.is_none() {
                let flags = EntryFlags::EMPTY
                    .with_dir(it.is_dir)
                    .with_symlink(it.is_symlink)
                    .with_exec(it.is_exec);
                let child_abs = abs.join(std::ffi::OsString::from(
                    String::from_utf8_lossy(&it.name).into_owned(),
                ));
                let mut ix = self.shared.index.write().unwrap();
                let id = ix.insert(dir, &it.name, flags);
                drop(ix);
                self.emit(Record::Add {
                    id,
                    parent: dir,
                    name: it.name.clone(),
                    flags: flags.0,
                    created: it.created_min,
                });
                if it.is_dir && !it.is_symlink {
                    // Watch before enumerating: install a watch, then queue the
                    // crawl of the new directory (§5 race rule) — unless a
                    // mount boundary says this subtree is not ours (§4): the
                    // entry stays as a leaf, covered by its own root if at all.
                    if self.may_descend(id, &child_abs) {
                        self.insert_child_path(id, dir, &it.name);
                        self.record_watch_request(id, child_abs);
                        new_dirs.push(id);
                    }
                }
            } else if it.is_dir && !it.is_symlink {
                // A directory we already know: make sure its path is cached so
                // later `crawl_dir`/`rescan` calls can reach it, and queue it for a
                // walk.
                //
                // Queueing a *known* directory matters after a restart: the snapshot
                // restores the directory entries, so nothing is "new", and a crawl
                // that only descended into new directories would never reach the
                // files inside them — leaving them without ownership and therefore
                // invisible to every unprivileged caller. The queue dedupes, so a
                // directory already scheduled is not walked twice.
                if let Some(id) = exists {
                    let _ = self.abs_path_for(id);
                    // ...but a mount boundary still stops the descend: another
                    // filesystem is covered by its own root, if at all.
                    let child_abs = abs.join(std::ffi::OsString::from(
                        String::from_utf8_lossy(&it.name).into_owned(),
                    ));
                    if self.may_descend(id, &child_abs) {
                        // A restart must re-request the watch, not just the
                        // walk: after a snapshot/journal replay every directory
                        // is "known", so without this no per-dir watch is ever
                        // installed and the daemon goes blind to creates until
                        // a rescan. The request dedupes (`watch_requested`), so
                        // a directory already watched costs one HashSet probe.
                        self.insert_child_path(id, dir, &it.name);
                        self.record_watch_request(id, child_abs);
                        known_dirs.push(id);
                    }
                }
            }
            // An entry that already exists still needs its ownership recorded, and
            // this is the *common* case after a restart: the snapshot restores the
            // entries, but the ownership table is derived from the filesystem, so a
            // re-crawl that skipped existing entries would leave every one of them
            // unrecorded — and the filter, failing closed, would then show an
            // unprivileged caller nothing at all. Backfill on every pass; it is one
            // `lstat` per entry, which is what this mode already pays.
            if let Some(id) = exists {
                let child_abs = abs.join(std::ffi::OsString::from(
                    String::from_utf8_lossy(&it.name).into_owned(),
                ));
                self.note_ownership(id, &child_abs);
            }
        }
        for d in new_dirs.into_iter().chain(known_dirs) {
            self.pending.enqueue_crawl(d);
        }
        // (`dir`'s chain was memoized by the `abs_path_for` at entry.)
    }

    /// Diff-based rescan (§7). Enumerate reality, compare with the index,
    /// keep ids for survivors, emit synthetic creates/deletes for differences.
    fn rescan(&mut self, dir: u32, cause: RescanCause) -> bool {
        self.shared.health.write().unwrap().rescan_count += 1;
        // Backoff: if this root is being hammered, defer (§7). A deferred rescan
        // is *not* dropped — it is re-enqueued so the work still happens once the
        // rate-limit window opens. Returns `true` when the rescan actually ran.
        {
            let now = now_ms();
            let mut roots = self.shared.roots.write().unwrap();
            if let Some(ri) = self.root_index_of_dir(dir) {
                if !roots[ri].backoff.permits(now) && cause != RescanCause::Manual {
                    roots[ri].pending_items = roots[ri].pending_items.saturating_add(1);
                    drop(roots);
                    self.shared.health.write().unwrap().rescans_deferred += 1;
                    self.pending.enqueue_rescan(dir, cause);
                    return false;
                }
                roots[ri].backoff.record(now);
                roots[ri].recrawl_count += 1;
                roots[ri].last_rescan_cause = Some(cause);
                roots[ri].last_rescan_at_ms = now;
                roots[ri].state = RootState::Rescanning;
                roots[ri].pending_items = roots[ri].pending_items.saturating_sub(1);
            }
        }

        let abs = match self.abs_path_for(dir) {
            Some(p) => p,
            None => {
                self.mark_root_ok_if_applicable(dir);
                return true;
            }
        };
        self.diff_subtree(dir, &abs);

        // A rescan that touched an entire root may warrant a generation bump:
        // we bump when a root dir's own rescan completes, per §7.
        if self.root_dir_ids.contains(&dir) {
            self.bump_generation();
        }
        self.mark_root_ok_if_applicable(dir);
        true
    }

    fn diff_subtree(&mut self, dir: u32, abs: &Path) {
        // As in `crawl_dir`: the parent's path must be cached before any child is
        // emitted, so ownership can be resolved for it — and the *directory itself*
        // must be recorded too. Every visibility check walks the parent chain, so a
        // directory with no record hides everything beneath it; after a restart the
        // root dir in particular has none, and the whole tree would go dark.
        // (`dir`'s chain was memoized by the `abs_path_for` at entry.)
        self.note_ownership(dir, abs);
        // Mount boundary (§4): like `crawl_dir`, never enumerate another
        // filesystem — unless this dir is itself a root, whose mount *is* the
        // assignment.
        if !self.may_descend(dir, abs) {
            return;
        }
        // 1. Snapshot the index's children of `dir`.
        let indexed: HashMap<Vec<u8>, u32> = {
            let ix = self.shared.index.read().unwrap();
            ix.children(dir)
                .into_iter()
                .map(|id| (ix.name(id).to_vec(), id))
                .collect()
        };
        // 2. Enumerate reality.
        let real = match self.fs.read_dir(abs) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                // Not a deletion: we simply may not look. Dropping the subtree
                // here deleted every indexed file under a TCC-protected folder
                // on each rescan, and the next crawl re-read the folder — one
                // more dialog. Keep what we have and stop asking.
                self.note_denied(abs);
                return;
            }
            Err(_) => {
                // Dir vanished: drop the subtree.
                self.drop_subtree(dir);
                return;
            }
        };
        let mut real_names: HashMap<Vec<u8>, DirItem> = HashMap::new();
        for it in real {
            if it.name == b"." || it.name == b".." || crate::util::is_cookie(&it.name) {
                continue;
            }
            real_names.insert(it.name.clone(), it);
        }

        // 3. Deletions: indexed but not real.
        for (name, id) in &indexed {
            if !real_names.contains_key(name) {
                self.drop_subtree(*id);
            }
        }
        // 4. Creations & type fixes.
        for (name, it) in &real_names {
            match indexed.get(name) {
                None => {
                    let flags = EntryFlags::EMPTY
                        .with_dir(it.is_dir)
                        .with_symlink(it.is_symlink)
                        .with_exec(it.is_exec);
                    let id = {
                        let mut ix = self.shared.index.write().unwrap();
                        ix.insert(dir, name, flags)
                    };
                    self.emit(Record::Add {
                        id,
                        parent: dir,
                        name: name.clone(),
                        flags: flags.0,
                        created: it.created_min,
                    });
                    if it.is_dir && !it.is_symlink {
                        let child_abs = abs.join(std::ffi::OsString::from(
                            String::from_utf8_lossy(name).into_owned(),
                        ));
                        // Mount boundary: record the mountpoint, never descend.
                        if self.may_descend(id, &child_abs) {
                            self.insert_child_path(id, dir, name);
                            self.record_watch_request(id, child_abs);
                            self.pending.enqueue_crawl(id);
                        } else {
                            self.insert_child_path(id, dir, name);
                        }
                    }
                }
                Some(id) => {
                    let cur = {
                        let ix = self.shared.index.read().unwrap();
                        ix.entry(*id).map(|e| e.flags())
                    };
                    if let Some(cur) = cur {
                        if cur.is_dir() != it.is_dir {
                            let nf = cur.with_dir(it.is_dir);
                            self.shared.index.write().unwrap().entry_mut(*id).flags = nf.0;
                        }
                        // A survivor's exec bit can have changed even though its
                        // name did not (`chmod +x` between rescans): refresh it
                        // in place — no journal record, since flags-only edits
                        // ride the existing type-fix path. Without this the
                        // rescan diff converges names but not permission bits.
                        if cur.is_exec() != it.is_exec {
                            let nf = cur.with_exec(it.is_exec);
                            self.shared.index.write().unwrap().entry_mut(*id).flags = nf.0;
                        }
                        // A rescan can *learn* a creation time the index never
                        // had, or see a new one after a replace that no create
                        // event reached us for. It must never erase one it
                        // knows: `0` means unknown, not "the epoch".
                        //
                        // Only a change to an *already known* minute is
                        // journaled (below). A first learn is memory-only: the
                        // first rescan of a store that predates creation times
                        // learns a date for every entry at once, and one `Add`
                        // per entry would append a record for the whole index
                        // to re-derive state the next snapshot carries anyway.
                        // A learned minute is still live in the index at once,
                        // and is relearned by the next rescan after a restart
                        // until a snapshot pins it.
                        let prev_created = {
                            let ix = self.shared.index.read().unwrap();
                            ix.created_min(*id)
                        };
                        let created_changed = matches!(
                            prev_created,
                            Some(prev)
                                if it.created_min != CREATED_UNKNOWN && prev != it.created_min
                        );
                        let created_learned =
                            prev_created.is_none() && it.created_min != CREATED_UNKNOWN;
                        if created_changed || created_learned {
                            self.shared
                                .index
                                .write()
                                .unwrap()
                                .set_created(*id, it.created_min);
                        }
                        // A survivor's ownership can have changed even though its
                        // name did not (a chown or a replaced file), and after a
                        // restart the record may be missing entirely. Either way a
                        // rescan is the moment to re-read it, or the filter keeps
                        // answering with the old — or no — answer.
                        let survivor_abs = abs.join(std::ffi::OsString::from(
                            String::from_utf8_lossy(name).into_owned(),
                        ));
                        self.note_ownership(*id, &survivor_abs);
                        if created_changed {
                            // Replay treats this Add as an upsert at `*id`: the
                            // name and flags are rewritten and the slot
                            // resurrected, then `set_created` lands the minute —
                            // the same record a fresh entry gets. (A first
                            // learn is not journaled here; see above.)
                            let flags = self
                                .shared
                                .index
                                .read()
                                .unwrap()
                                .entry(*id)
                                .map(|e| e.flags().0)
                                .unwrap_or(0);
                            self.emit(Record::Add {
                                id: *id,
                                parent: dir,
                                name: name.clone(),
                                flags,
                                created: it.created_min,
                            });
                        }
                        // Recurse into surviving directories — but never across a
                        // mount boundary: beneath one only legacy leakage can
                        // exist (nothing current is ever inserted there), so
                        // drop it and let the mountpoint stand as a leaf. This
                        // is what heals pre-boundary stores on their next rescan.
                        if it.is_dir && !it.is_symlink {
                            self.insert_child_path(*id, dir, name);
                            if self.may_descend(*id, &survivor_abs) {
                                // As in `crawl_dir`'s known-dir branch: a
                                // rescan that only re-walks without
                                // re-requesting the watch leaves restarted
                                // subtrees unwatched (every dir here is a
                                // survivor, never "new"). Dedupes via
                                // `watch_requested`.
                                self.record_watch_request(*id, survivor_abs.clone());
                                self.diff_subtree(*id, &survivor_abs);
                            } else {
                                let stale: Vec<u32> = {
                                    let ix = self.shared.index.read().unwrap();
                                    descendants(&ix, *id)
                                };
                                for dead in stale.into_iter().rev() {
                                    self.emit(Record::Del { id: dead });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn mark_root_ok_if_applicable(&mut self, dir: u32) {
        if let Some(ri) = self.root_index_of_dir(dir) {
            let mut roots = self.shared.roots.write().unwrap();
            roots[ri].state = RootState::Ok;
            roots[ri].backoff.reset();
        }
    }

    fn root_index_of_dir(&self, dir: u32) -> Option<usize> {
        let ix = self.shared.index.read().unwrap();
        let root_id = ix.root_of(dir)?;
        self.root_dir_ids.iter().position(|&r| r == root_id)
    }

    fn bump_generation(&mut self) {
        let mut ix = self.shared.index.write().unwrap();
        let g = ix.bump_generation();
        drop(ix);
        self.shared.generation.store(g, Ordering::Release);
        self.shared.health.write().unwrap().generation_bumps += 1;
    }

    // -------------------------------------------------------------- the loop

    /// Apply one bounded chunk of the startup journal tail (§9 background
    /// replay). Records carry their file LSNs; the index advances past
    /// exactly the applied prefix (so a crash mid-replay re-skips precisely
    /// that prefix — re-application is idempotent), while the shared clock
    /// is a high-water mark over replay LSNs and live journal appends (which
    /// interleave by tick past the tail end). Ownership and trigram
    /// maintenance ride the shared [`Applier::apply_live`] funnel. Returns
    /// the records applied.
    ///
    /// When the last chunk drains, the startup credit folds into
    /// `records_since_compaction` so a large inherited tail compacts once via
    /// the normal `maybe_compact` path (deferred from build time, when the
    /// compaction remap would have had no live ids to fix).
    fn drain_replay_chunk(&mut self, t0: &std::time::Instant) -> usize {
        let mut n = 0;
        while let Some((lsn, rec)) = self.pending_replay.pop_front() {
            self.apply_live(rec);
            // The index tracks replay progress exactly (crash → re-replay
            // from the snapshot skips `lsn <= last_lsn`; re-application is
            // idempotent, see the test). The shared clock is a high-water
            // mark: live writes interleave by tick at journal positions past
            // the tail end, so an exact store here could move a served cursor
            // backwards — only max-advance keeps `c:gen:lsn` monotonic.
            self.shared.lsn.fetch_max(lsn, Ordering::Release);
            {
                let mut ix = self.shared.index.write().unwrap();
                ix.set_last_lsn(lsn);
            }
            n += 1;
            if t0.elapsed() >= DRAIN_TIME_BUDGET {
                break;
            }
        }
        if self.pending_replay.is_empty() && self.replay_compaction_credit > 0 {
            self.records_since_compaction += self.replay_compaction_credit;
            self.replay_compaction_credit = 0;
        }
        n
    }

    /// Apply everything currently pending. Returns the number of items applied.
    ///
    /// Work items are paced: at most [] per call. Events
    /// always fully drain first (they are the latency-sensitive live updates,
    /// and a flood escalates to a rescan via queue capacity anyway), but an
    /// unbounded work loop starves everything else a tick does — control
    /// requests (rescan/cookies), watch-request forwarding, and the
    ///  clear. Observed live: a post-replay crawl of ~760k
    /// entries (each directory paying linear-scan lookups) held a single
    /// drain call for so long that  never ran, no per-dir watch
    /// was ever installed, and every sync went weak. Pacing keeps the
    /// FIFO order (only the *rate* changes), so the §7/§8 guarantees hold.
    pub fn drain(&mut self) -> usize {
        let t0 = std::time::Instant::now();
        let mut n = 0;
        // §9 background replay drains first and exclusively: while the
        // startup tail remains, nothing else is popped — no crawl, no rescan,
        // no event, no cookie completion. Order is why: a rescan or crawl
        // diff matches survivors by `(parent, name)` and would see the tail's
        // deletes as already-applied while missing its creates, minting
        // duplicate entries with fresh ids. Replay is the crash-recovery
        // prefix of history; everything else is ordered after it.
        n += self.drain_replay_chunk(&t0);
        if !self.pending_replay.is_empty() {
            // Tail remains: report progress and yield. `catching_up` stays
            // true (see `Daemon::tick`); cookies cannot complete while the
            // queue is undrained, so no sync can claim a stale settle.
            return n;
        }
        loop {
            let mut progressed = false;
            while let Some(ev) = self.pending.pop_event() {
                self.apply_event(ev);
                n += 1;
                progressed = true;
            }
            if t0.elapsed() >= DRAIN_TIME_BUDGET {
                break;
            }
            if let Some(w) = self.pending.pop_work() {
                let progressed_this = self.run_work(w);
                if !progressed_this {
                    // A deferred rescan: stop this drain pass so we don't spin.
                    // The item is re-queued and retried on a later tick.
                    self.check_cookies();
                    break;
                }
                n += 1;
                progressed = true;
            }
            self.check_cookies();
            if !progressed {
                break;
            }
        }
        n
    }

    /// Check for completed cookies and notify waiters (§8).
    fn check_cookies(&mut self) {
        for c in self.pending.take_completed_cookies() {
            // Grab the path *before* completing: `complete` forgets the cookie,
            // which would make `cookie_path` return None and leak the file.
            let path = self.sync.cookie_path(c.token);
            self.sync.complete(c.token);
            if let Some(path) = path {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// Push an event from the watcher (called by the watcher thread via a
    /// channel; kept here for the fake-backend model tests).
    pub fn push(&mut self, ev: FsEvent) -> PushOutcome {
        self.pending.push_event(ev)
    }

    pub fn register_cookie(&mut self, root: u32, name: Vec<u8>) -> u64 {
        let token = self.next_cookie_token;
        self.next_cookie_token += 1;
        self.pending.register_cookie(token, root, name);
        self.sync.note_pending(token);
        token
    }

    /// Register a cookie whose token was chosen by the caller (the daemon's
    /// sync engine reserves the token before creating the file, so the name and
    /// the registry entry agree).
    pub fn register_cookie_existing(&mut self, token: u64, root: u32) {
        let name = crate::sync::cookie_name(token);
        self.pending.register_cookie(token, root, name);
        // Keep our token counter ahead so we never collide.
        if token >= self.next_cookie_token {
            self.next_cookie_token = token + 1;
        }
    }

    /// Resolve the absolute path of a directory id using the roots table.
    pub fn abs_path_public(&mut self, id: u32) -> Option<PathBuf> {
        self.abs_path_for(id)
    }

    /// Inverse of the path cache: the directory id whose cached absolute path
    /// equals `path`. Used by tests that emulate the watcher's `wd -> dir id`
    /// lookup; `None` if the directory is not known. Materializes each entry
    /// (the cache holds parent-relative links, not absolute paths) — fine for
    /// test scale, never called on the hot path.
    pub fn dir_id_for_path(&self, path: &Path) -> Option<u32> {
        // Read-only walk: resolve each node without memoizing (this is `&self`).
        // Mirrors `abs_path_for`'s chain logic minus the index fallback.
        let mut ids: Vec<u32> = self.dir_paths.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let mut rev: Vec<OsString> = Vec::new();
            let mut cur = id;
            let mut guard = 0usize;
            let root_abs = loop {
                guard += 1;
                if guard > self.dir_paths.len() + 1 {
                    break None; // cycle guard, as in `abs_path_for`
                }
                match self.dir_paths.get(&cur) {
                    Some(DirPathNode::Root(p)) => break Some(p.clone()),
                    Some(DirPathNode::Child { parent, name }) => {
                        rev.push(name.clone());
                        cur = *parent;
                    }
                    None => break None,
                }
            };
            let Some(mut abs) = root_abs else { continue };
            for name in rev.iter().rev() {
                abs.push(name);
            }
            if abs.as_path() == path {
                return Some(id);
            }
        }
        None
    }

    /// A watch could not be installed for `dir` (e.g. inotify watch-limit
    /// exhaustion). Mark the owning root degraded and record it in health —
    /// loudly stale, never silently wrong (§7, §15). The subtree stays in the
    /// index and remains searchable.
    pub fn note_watch_failure(&mut self, dir: u32, reason: &str) {
        let is_limit = reason.contains("No space left") || reason.contains("os error 28");
        {
            let mut h = self.shared.health.write().unwrap();
            if is_limit {
                h.watch_limit_hits += 1;
            }
            h.watch_failures += 1;
        }
        if let Some(ri) = self.root_index_of_dir_or_self(dir) {
            let mut roots = self.shared.roots.write().unwrap();
            let why = if is_limit {
                format!("watch_limit_exhausted: {reason}")
            } else {
                format!("watch_failed: {reason}")
            };
            roots[ri].state = RootState::Degraded { reason: why };
            roots[ri].last_rescan_cause = Some(crate::event::RescanCause::WatchDropped);
            roots[ri].last_rescan_at_ms = now_ms();
        }
    }

    /// Like `root_index_of_dir`, but falls back to matching `dir` against the
    /// root dir ids directly (a failed watch may be for a dir we could not
    /// resolve through the index yet).
    fn root_index_of_dir_or_self(&self, dir: u32) -> Option<usize> {
        if let Some(ri) = self.root_index_of_dir(dir) {
            return Some(ri);
        }
        self.root_dir_ids.iter().position(|&r| r == dir)
    }

    /// The mount backing a root disappeared: mark it degraded, loudly.
    pub fn note_mount_gone(&mut self, dir: u32, path: &str) {
        if let Some(ri) = self.root_index_of_dir_or_self(dir) {
            let mut roots = self.shared.roots.write().unwrap();
            let mut r = roots.swap_remove(ri);
            r.state = RootState::Degraded {
                reason: format!("root_gone: {path}"),
            };
            r.last_rescan_cause = Some(RescanCause::MountEvent);
            r.last_rescan_at_ms = now_ms();
            roots.insert(ri, r);
        }
    }

    /// A different filesystem is now mounted at a root: record the new device id
    /// and mark the root rescanning.
    pub fn note_mount_remounted(&mut self, dir: u32, old_dev: u64, new_dev: u64) {
        if let Some(ri) = self.root_index_of_dir_or_self(dir) {
            let mut roots = self.shared.roots.write().unwrap();
            roots[ri].dev = new_dev;
            roots[ri].state = RootState::Rescanning;
            let _ = old_dev;
        }
    }

    /// Compact if the journal has grown past the threshold, or tombstones have
    /// accumulated past their ratio. Returns true if a snapshot was written.
    ///
    /// Two arms, either of which fires:
    /// * journal records since last compaction (write amplification: the
    ///   journal replays on every boot);
    /// * tombstone ratio (dead slots + leaked arena names resident for no
    ///   benefit — a churn-then-quiet index accumulates tombstones without
    ///   new journal records, so the first arm alone never bounds them).
    ///
    /// Both are gated on a minimum absolute tombstone count so a tiny index
    /// (where a generation bump + trigram rebuild costs more than the saving)
    /// never compacts for a handful of deletes.
    pub fn maybe_compact(&mut self) -> bool {
        if self.records_since_compaction >= self.compaction_threshold {
            return self.compact_now();
        }
        let (tombs, entries) = {
            let ix = self.shared.index.read().unwrap();
            (ix.tombstone_count(), ix.entry_count() as u64)
        };
        if tombs >= Self::COMPACTION_MIN_TOMBSTONES
            && entries > 0
            && tombs * 100 / entries >= Self::COMPACTION_TOMBSTONE_RATIO_PCT
        {
            return self.compact_now();
        }
        false
    }

    /// Minimum dead slots before the tombstone-ratio arm can fire. Below this
    /// a compaction's generation bump (cursor invalidation + trigram rebuild)
    /// costs more than the reclaimed bytes.
    pub const COMPACTION_MIN_TOMBSTONES: u64 = 10_000;
    /// Tombstone percentage of total slots that triggers a compaction even
    /// when the journal is quiet (churn-then-idle leaves dead slots resident).
    pub const COMPACTION_TOMBSTONE_RATIO_PCT: u64 = 20;

    pub fn compact_now(&mut self) -> bool {
        let (ix_snapshot, roots_snapshot) = {
            let ix = self.shared.index.read().unwrap();
            let roots = self.shared.roots.read().unwrap();
            (ix.clone(), roots.clone())
        };
        // Build the compacted index off the lock, then swap it in.
        let (compacted, id_map) = ix_snapshot.compacted();
        let new_gen = self.shared.generation() + 1;
        let mut compacted = compacted;
        compacted.set_generation(new_gen);
        // §9 LSN epoch reset: the journal is truncated below, so its clock
        // restarts at 0. Carrying the old high-water into the new snapshot
        // would make next boot's `lsn > last_lsn` replay skip filter out
        // every post-compact write (silent loss). The generation bump above
        // already invalidates pre-compact `c:gen:lsn` cursors.
        compacted.set_last_lsn(0);
        {
            let mut ix = self.shared.index.write().unwrap();
            *ix = compacted;
        }
        // Compaction renumbers ids, so anything keyed by id outside the index must
        // be remapped in the same breath. Forgetting this does not fail loudly:
        // the ownership sidecar would keep answering about the *old* numbering,
        // i.e. about whichever files now sit at those ids.
        {
            let mut own = self.shared.ownership.write().unwrap();
            own.remap(&id_map);
        }
        // The root-dir table is keyed by id too: without this every
        // watch/crawl/rescan/drop targets the wrong dir after a compaction.
        // A remap to 0 (the entry died) means unresolvable, exactly like a
        // missing root entry — callers already treat `ROOT_ID` that way.
        for rid in self.root_dir_ids.iter_mut() {
            if (*rid as usize) < id_map.len() {
                *rid = id_map[*rid as usize];
            }
        }
        // The dir-path cache is keyed by id as well, and unlike the root
        // table it cannot be remapped in place (it holds absolute paths, not
        // roles). A stale entry is worse than a miss: `abs_path_for` trusts a
        // cache hit, so a remapped-to-a-different-live-entry id would resolve
        // to the wrong directory silently. Drop everything but the roots
        // (re-seeded below); the rest is re-learned on demand by crawls.
        self.dir_paths.clear();
        {
            let roots = self.shared.roots.read().unwrap();
            for (i, r) in roots.iter().enumerate() {
                let rid = self.root_dir_ids.get(i).copied().unwrap_or(ROOT_ID);
                if rid != ROOT_ID {
                    self.dir_paths.insert(rid, DirPathNode::Root(r.path.clone()));
                }
            }
        }
        self.shared.generation.store(new_gen, Ordering::Release);
        // Epoch reset, continued: the shared clock restarts with the journal.
        // (The index copy above already carries last_lsn 0 for the snapshot.)
        self.shared.lsn.store(0, Ordering::Release);
        let (snap_index, snap_roots) = {
            let ix = self.shared.index.read().unwrap();
            (ix.clone(), roots_snapshot)
        };
        if self.store.compact(&snap_index, &snap_roots).is_err() {
            self.shared.health.write().unwrap().journal_write_errors += 1;
            return false;
        }
        self.records_since_compaction = 0;
        self.shared.health.write().unwrap().last_compaction_ms = now_ms();
        true
    }

    pub fn store(&self) -> &Store {
        &self.store
    }
    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }
}

/// Find the root-dir entry id for a root path by looking up the root's own
/// stored path against the index — roots are keyed by a synthetic entry whose
/// name is the full path and parent is ROOT_ID.
pub fn root_dir_id(ix: &Index, root: &Root) -> u32 {
    // Roots are inserted at crawl time under `root_entry_name` (the full
    // path, which cannot collide). Stores written before path qualification
    // used the bare basename: fall back to that so they resolve with no
    // migration — and the daemon only inserts when *neither* resolves, so a
    // legacy entry is adopted rather than duplicated.
    if let Some(id) = ix.lookup(ROOT_ID, &crate::roots::root_entry_name(&root.path)) {
        return id;
    }
    match crate::roots::legacy_root_entry_name(&root.path) {
        Some(old) => ix.lookup(ROOT_ID, &old).unwrap_or(ROOT_ID),
        None => ROOT_ID,
    }
}

/// A root path in the byte form the roots table compares by: separators
/// normalized and one trailing slash stripped (except for `/` itself), the
/// same normalization [`crate::roots::root_entry_name`] applies so that a
/// root's configured path and its index identity agree.
///
/// Non-UTF-8 paths lose fidelity through the lossy conversion, exactly as
/// `root_entry_name` already does for root identities.
fn root_abs_bytes(path: &Path) -> Vec<u8> {
    crate::roots::root_entry_name(path)
}

/// Every configured root path in [`root_abs_bytes`] form, in roots-table
/// order (parallel to `Applier::root_dir_ids`).
fn root_abs_of(shared: &Shared) -> Vec<Vec<u8>> {
    let roots = shared.roots.read().unwrap();
    roots.iter().map(|r| root_abs_bytes(&r.path)).collect()
}

/// All descendants of `dir` (not including `dir`).
pub fn descendants(ix: &Index, dir: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for c in ix.children(d) {
            out.push(c);
            stack.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS TCC: the first listing of a protected folder (Desktop, Documents,
    /// Downloads) raises the "would like to access files…" dialog, and
    /// dismissing it makes the listing fail with `PermissionDenied`. Retrying
    /// is what turns one refusal into an endless dialog loop, so the refusal is
    /// remembered: the folder is never listed again, paths inside it are never
    /// statted, and what was already indexed is not mistaken for a deletion.
    #[test]
    fn a_permission_denied_directory_is_never_touched_again() {
        use crate::event::{DirKey, FsEvent, RescanCause};
        use crate::modelfs::{ModelFs, Node};

        let d = tempfile::tempdir().unwrap();
        let store = Store::open(&d.path().join("state")).unwrap();
        let mut ix = Index::new();
        let r0 = ix.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let shared = Shared::new(ix, vec![Root::new(0, PathBuf::from("/model"))]);
        let model = ModelFs::with([
            (
                "Desktop",
                Node::dir(&[b"notes.txt".as_slice(), b"todo.txt".as_slice()]),
            ),
            ("keep.txt", Node::File),
        ]);
        let listed = Arc::new(AtomicUsize::new(0));
        let statted = Arc::new(AtomicUsize::new(0));
        let view: Arc<dyn FsView> = Arc::new(DeniedFs {
            inner: model,
            denied: PathBuf::from("/model/Desktop"),
            listed: listed.clone(),
            statted: statted.clone(),
        });
        let mut a = Applier::new(shared.clone(), store, view, SyncRegistry::new());
        a.rebuild_dir_cache();
        a.pending_mut().enqueue_crawl(r0);
        a.drain();

        assert_eq!(
            listed.load(Ordering::SeqCst),
            1,
            "the folder must be attempted exactly once, not once per pass"
        );
        let desktop = {
            let ix = shared.index.read().unwrap();
            let desktop = ix
                .lookup(r0, b"Desktop")
                .expect("the parent listing still indexes the folder itself");
            assert!(
                ix.children(desktop).is_empty(),
                "a folder we may not read has no indexed children"
            );
            assert!(ix.lookup(r0, b"keep.txt").is_some());
            desktop
        };
        assert_eq!(
            shared.health.read().unwrap().read_dir_errors,
            0,
            "a refusal is permanent, not a transient read error"
        );

        // A rescan must not read "may not look" as "deleted", and must not ask
        // again.
        a.pending_mut().enqueue_rescan(r0, RescanCause::Manual);
        a.drain();
        assert_eq!(
            listed.load(Ordering::SeqCst),
            1,
            "rescan re-opened the denied folder"
        );
        {
            let ix = shared.index.read().unwrap();
            assert!(
                ix.lookup(r0, b"Desktop").is_some(),
                "the denied folder was dropped as vanished"
            );
            assert!(ix.lookup(r0, b"keep.txt").is_some());
        }

        // The watcher reports those folders even when we may not read them
        // (FSEvents needs no TCC grant). Such an event must be dropped before
        // the `stat` that would raise the dialog again.
        let before = statted.load(Ordering::SeqCst);
        a.apply_event(FsEvent::upsert(
            DirKey::new(desktop),
            b"notes.txt".to_vec(),
            false,
        ));
        assert_eq!(
            statted.load(Ordering::SeqCst),
            before,
            "an event under a denied folder statted a path inside it"
        );
        assert_eq!(
            shared.index.read().unwrap().lookup(desktop, b"notes.txt"),
            None
        );

        // And a fresh crawl re-walks the tree without opening it again.
        a.pending_mut().enqueue_crawl(r0);
        a.drain();
        assert_eq!(
            listed.load(Ordering::SeqCst),
            1,
            "crawl re-opened the denied folder"
        );
    }

    /// A view that refuses one directory the way macOS refuses a TCC-protected
    /// folder: `PermissionDenied` on every attempt, counted so a retry shows up
    /// as a second permission dialog.
    struct DeniedFs {
        inner: crate::modelfs::ModelFs,
        denied: PathBuf,
        listed: Arc<AtomicUsize>,
        statted: Arc<AtomicUsize>,
    }

    impl FsView for DeniedFs {
        fn read_dir(&self, abs_path: &Path) -> io::Result<Vec<DirItem>> {
            if abs_path == self.denied {
                self.listed.fetch_add(1, Ordering::SeqCst);
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "operation not permitted",
                ));
            }
            self.inner.read_dir(abs_path)
        }

        fn stat(&self, abs_path: &Path) -> io::Result<Option<StatInfo>> {
            self.inner.stat(abs_path)
        }

        fn facts(&self, abs_path: &Path, is_dir: bool, is_symlink: bool) -> (bool, u32) {
            self.statted.fetch_add(1, Ordering::SeqCst);
            self.inner.facts(abs_path, is_dir, is_symlink)
        }
    }

    #[test]
    fn descendants_walk() {
        let mut ix = Index::new();
        let r = ix.insert(
            ROOT_ID,
            b"r",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let a = ix.insert(r, b"a", EntryFlags::EMPTY.with_dir(true));
        let b = ix.insert(r, b"b", EntryFlags::EMPTY.with_dir(true));
        ix.insert(a, b"x", EntryFlags::EMPTY);
        ix.insert(b, b"y", EntryFlags::EMPTY);
        let d = descendants(&ix, r);
        assert_eq!(d.len(), 4);
    }

    /// A `dosdevices/z:` that points at `/` was walked once in a real index: the
    /// store holds a whole filesystem of aliases (`…/z:/tmp/x`) under an entry
    /// that is a symlink. Nothing descends there again, so the startup sweep is
    /// what removes them.
    #[test]
    fn the_startup_sweep_drops_entries_under_a_symlink() {
        use crate::modelfs::{model_root, ModelFs, Node};
        use crate::persist::Store;
        use crate::roots::Root;
        use crate::sync::SyncRegistry;
        use std::sync::Arc;

        let model = ModelFs::with([("z", Node::dir(&["alias.txt"])), ("real.txt", Node::File)]);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state")).unwrap();
        let mut ix = Index::new();
        let r0 = ix.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let shared = Shared::new(ix, vec![Root::new(0, model_root())]);
        let mut a = Applier::new(
            shared.clone(),
            store,
            Arc::new(model.clone()),
            SyncRegistry::new(),
        );
        a.rebuild_dir_cache();
        a.pending_mut().enqueue_crawl(r0);
        a.drain();
        {
            let ix = shared.index.read().unwrap();
            let z = ix.lookup(r0, b"z").expect("z is indexed");
            assert!(
                ix.lookup(z, b"alias.txt").is_some(),
                "the crawl indexes a real directory's children"
            );
        }

        // What an older build left behind: `z` is a symlink and the directory's
        // children are still filed under it.
        {
            let mut ix = shared.index.write().unwrap();
            let z = ix.lookup(r0, b"z").expect("z is indexed");
            let f = ix.entry(z).unwrap().flags();
            ix.entry_mut(z).flags = f.with_symlink(true).with_dir(false).0;
        }
        assert_eq!(a.drop_entries_under_non_dirs(), 1, "one orphaned child");

        let ix = shared.index.read().unwrap();
        let z = ix.lookup(r0, b"z").expect("the symlink itself stays indexed");
        assert!(
            ix.lookup(z, b"alias.txt").is_none(),
            "a symlink cannot keep the children the directory had"
        );
        assert!(
            ix.lookup(r0, b"real.txt").is_some(),
            "a file under a real directory is not an orphan"
        );
    }

    #[test]
    fn compaction_resets_the_lsn_epoch() {
        use crate::modelfs::{model_root, ModelFs, Node};
        use crate::persist::Store;
        use crate::roots::Root;
        use crate::sync::SyncRegistry;
        use std::sync::Arc;

        // The epoch reset is what makes a truncated journal safe to reopen:
        // snapshot, shared clock, and journal all restart at 0, so next
        // boot's `lsn > last_lsn` replay skip cannot discard post-compact
        // writes.
        let model = ModelFs::with([("a.rs", Node::File)]);
        let dir = tempfile::tempdir().unwrap();
        let statedir = dir.path().join("state");
        let store = Store::open(&statedir).unwrap();
        let mut ix = Index::new();
        let r0 = ix.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        ix.set_last_lsn(41);
        let shared = Shared::new(ix, vec![Root::new(0, model_root())]);
        shared.lsn.store(41, Ordering::Release);
        let mut a = Applier::new(
            shared.clone(),
            store,
            Arc::new(model.clone()),
            SyncRegistry::new(),
        );
        a.rebuild_dir_cache();
        a.pending_mut().enqueue_crawl(r0);
        a.drain();
        assert!(a.compact_now());
        assert_eq!(shared.index.read().unwrap().last_lsn(), 0);
        assert_eq!(shared.snapshot_lsn(), 0);
        assert_eq!(a.store().journal.count(), 0);

        // A post-compact write gets LSN 1 and survives a reopen+replay.
        a.pending_mut().enqueue_crawl(r0);
        a.drain();
        model.put(b"b.rs", Node::File);
        a.pending_mut()
            .enqueue_rescan(r0, crate::event::RescanCause::Manual);
        a.drain();
        assert_eq!(shared.snapshot_lsn(), 1, "first post-epoch write is LSN 1");
        drop(a);

        let mut store2 = Store::open(&statedir).unwrap();
        let snap = store2.load_snapshot().unwrap().unwrap();
        assert_eq!(snap.index.last_lsn(), 0, "snapshot carries the reset epoch");
        let mut ix2 = snap.index;
        let mut applied = Vec::new();
        let out = store2
            .replay(&mut ix2, |lsn, rec| applied.push((lsn, rec.clone())))
            .unwrap();
        assert_eq!(out.replayed, 1, "the post-compact write must not be skipped");
        for (_, rec) in &applied {
            crate::persist::apply_record(&mut ix2, rec);
        }
        let r0b = crate::applier::root_dir_id(&ix2, &Root::new(0, model_root()));
        assert!(
            ix2.lookup(r0b, b"b.rs").is_some(),
            "b.rs must survive a restart after the epoch reset"
        );
    }

    #[test]
    fn compaction_remaps_root_dir_ids() {
        use crate::modelfs::{model_root, ModelFs, Node};
        use crate::persist::Store;
        use crate::roots::Root;
        use crate::sync::SyncRegistry;
        use std::sync::Arc;

        // The second root arrives *after* files exist (a newly configured
        // mount), so its entry sits above a tombstone: compaction renumbers
        // it down, and without the remap the root table would keep pointing
        // at whatever moved into the old slot.
        let model = ModelFs::with([("gone.rs", Node::File), ("sub", Node::dir(&["kept.rs"]))]);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state")).unwrap();
        let mut ix = Index::new();
        let r0 = ix.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let roots = vec![Root::new(0, model_root())];
        let shared = Shared::new(ix, roots);
        let mut a = Applier::new(
            shared.clone(),
            store,
            Arc::new(model.clone()),
            SyncRegistry::new(),
        );
        a.rebuild_dir_cache();
        a.pending_mut().enqueue_crawl(r0);
        a.drain();

        let r1 = shared.index.write().unwrap().insert(
            ROOT_ID,
            b"/model/sub",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        shared
            .roots
            .write()
            .unwrap()
            .push(Root::new(1, model_root().join("sub")));
        a.rebuild_dir_cache();
        assert_eq!(a.root_dir_ids, vec![r0, r1]);
        a.pending_mut().enqueue_crawl(r1);
        a.drain();

        model.remove(b"gone.rs");
        a.pending_mut()
            .enqueue_rescan(r0, crate::event::RescanCause::Manual);
        a.drain();
        assert!(a.compact_now());

        // The table must track the renumbering, matching a fresh resolution.
        let ix = shared.index.read().unwrap();
        let roots = shared.roots.read().unwrap();
        let expect: Vec<u32> = roots.iter().map(|r| root_dir_id(&ix, r)).collect();
        assert_ne!(r1, expect[1], "the test must actually move the second root");
        assert_eq!(
            a.root_dir_ids, expect,
            "compaction must remap the root table"
        );
        // ...and the second root still names a live root-dir entry.
        let rid1 = a.root_dir_ids[1];
        assert_ne!(rid1, ROOT_ID);
        assert!(ix.is_alive(rid1) && ix.entry(rid1).unwrap().flags().is_root_dir());
    }

    /// The trigram cache survives churn without rebuilding, and stays correct.
    ///
    /// Drives the real mutation funnel (`apply_event`, which funnels through
    /// `emit`) with interleaved inserts and deletes, then asserts:
    /// (a) the cache key is still the same generation (no rebuild happened —
    /// exactly the disease this fixes: every insert used to invalidate);
    /// (b) an accelerated query finds a file created *after* the cache was
    /// built (incremental postings, not a stale snapshot);
    /// (c) a deleted file is gone from accelerated results (no phantoms);
    /// (d) accelerated and linear results agree on the whole corpus.
    #[cfg(feature = "trigram")]
    #[test]
    fn trigram_cache_survives_churn_without_rebuilding() {
        use crate::event::{DirKey, FsEvent};
        use crate::modelfs::{model_root, ModelFs, Node};
        use crate::persist::Store;
        use crate::query::{self, Query};
        use crate::roots::Root;
        use crate::sync::SyncRegistry;
        use std::sync::Arc;

        let model = ModelFs::with([("seed_alpha.rs", Node::File)]);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state")).unwrap();
        let mut ix = Index::new();
        let r0 = ix.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let shared = Shared::new(ix, vec![Root::new(0, model_root())]);
        let mut a = Applier::new(
            shared.clone(),
            store,
            Arc::new(model.clone()),
            SyncRegistry::new(),
        );
        a.rebuild_dir_cache();
        a.pending_mut().enqueue_crawl(r0);
        a.drain();

        // Build the accelerator once, over the crawled index.
        let first = shared.trigram_index();
        let gen = shared.index.read().unwrap().generation();
        let rp = vec![(r0, b"/model".to_vec())];

        // Churn: 50 inserts + 20 deletes through the real event funnel.
        for i in 0..50 {
            let name = format!("churn_file_{i:03}_beta.rs");
            a.apply_event(FsEvent::Upsert {
                parent: DirKey(r0),
                name: name.into_bytes(),
                is_dir: false,
            });
        }
        for i in (0..50).step_by(5) {
            // Delete every 5th (10 files); plus the seed file.
            let name = format!("churn_file_{i:03}_beta.rs");
            a.apply_event(FsEvent::Remove {
                parent: DirKey(r0),
                name: name.into_bytes(),
            });
        }
        a.apply_event(FsEvent::Remove {
            parent: DirKey(r0),
            name: b"seed_alpha.rs".to_vec(),
        });

        // (a) same generation, same cache object: no rebuild.
        let second = shared.trigram_index();
        assert_eq!(
            shared.index.read().unwrap().generation(),
            gen,
            "test must not bump generation (that path rebuilds by design)"
        );
        assert!(
            Arc::ptr_eq(&first, &second),
            "cache must be the same object after churn: a rebuild happened"
        );
        // Indexed count grew by the net inserts: 1 seed + 50 churned files
        // were built over... actually the build saw seed + crawled entries;
        // what matters is the delta: +50 inserts, -11 removes (10 churned +
        // the seed). Recompute from the live index instead of hardcoding.
        // (`live_count` includes the root dir, which the accelerator never
        // indexes — hence minus one.)
        let live_nonroot = shared
            .index
            .read()
            .unwrap()
            .iter_live()
            .filter(|(_, e)| !e.flags().is_root_dir())
            .count() as u64;
        assert_eq!(
            second.read().unwrap().indexed_entries(),
            live_nonroot,
            "incremental indexed count must track the live non-root index"
        );

        // (b) a file created after the cache build is found accelerated.
        let q = Query::compile("churn_file_042_beta").unwrap();
        let accel = query::execute_with_trigram(
            &shared.index.read().unwrap(),
            &rp,
            &q,
            0,
            false,
            Some(&second.read().unwrap()),
        );
        assert_eq!(
            accel.total, 1,
            "incrementally-indexed file must be a candidate"
        );
        assert!(
            accel.hits[0]
                .path
                .ends_with(b"churn_file_042_beta.rs".as_slice()),
            "wrong hit: {:?}",
            String::from_utf8_lossy(&accel.hits[0].path)
        );

        // (c) deleted files are gone (no phantoms from pruned-or-stale postings).
        for raw in ["churn_file_005_beta", "seed_alpha"] {
            let q = Query::compile(raw).unwrap();
            let accel = query::execute_with_trigram(
                &shared.index.read().unwrap(),
                &rp,
                &q,
                0,
                false,
                Some(&second.read().unwrap()),
            );
            assert_eq!(accel.total, 0, "q={raw:?}: deleted file must not match");
        }

        // (d) full-corpus agreement between accelerated and linear.
        for raw in ["churn", "beta", "rs", "alpha", "file_01", "ext:rs", "main"] {
            let q = Query::compile(raw).unwrap();
            let plain = query::execute(&shared.index.read().unwrap(), &rp, &q, 0, false);
            let accel = query::execute_with_trigram(
                &shared.index.read().unwrap(),
                &rp,
                &q,
                0,
                false,
                Some(&second.read().unwrap()),
            );
            assert_eq!(
                plain.total, accel.total,
                "q={raw:?}: total differs after churn"
            );
            let p: Vec<u32> = plain.hits.iter().map(|h| h.id).collect();
            let x: Vec<u32> = accel.hits.iter().map(|h| h.id).collect();
            assert_eq!(p, x, "q={raw:?}: rows differ after churn");
        }
    }
}

#[cfg(test)]
mod replay_tests {
    use super::*;
    use crate::journal::Record;
    use crate::persist::Store;
    use crate::roots::Root;
    use crate::sync::SyncRegistry;
    use std::sync::Arc;

    fn test_applier() -> (Applier, Arc<Shared>, u32) {
        let dir = tempfile::tempdir().unwrap();
        // Leak the tempdir: the store borrows its path, and the applier
        // outlives this frame only via the returned value — the dir must
        // outlive the test. `Box::leak` is the honest one-liner.
        let dir: &'static std::path::Path = Box::leak(Box::new(dir.keep()));
        let store = Store::open(&dir.join("state")).unwrap();
        let mut ix = Index::new();
        let r0 = ix.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let shared = Shared::new(ix, vec![Root::new(0, PathBuf::from("/model"))]);
        let fs: Arc<dyn FsView> = Arc::new(crate::modelfs::ModelFs::with([]));
        let mut a = Applier::new(shared.clone(), store, fs, SyncRegistry::new());
        a.rebuild_dir_cache();
        (a, shared, r0)
    }

    /// Replay drains first and exclusively: while the tail remains, no
    /// event, work item, or cookie completes — and each chunk advances the
    /// index and the clock past exactly the applied prefix.
    #[test]
    fn replay_chunking_is_ordered_and_monotonic() {
        let (mut a, shared, r0) = test_applier();
        // Tail: add x (lsn 1), add y (lsn 2), del x (lsn 3).
        let x = r0 + 1;
        let y = r0 + 2;
        a.set_pending_replay(
            vec![
                (
                    1,
                    Record::Add {
                        id: x,
                        parent: r0,
                        name: b"x".to_vec(),
                        flags: 0,
                        created: 0,
                    },
                ),
                (
                    2,
                    Record::Add {
                        id: y,
                        parent: r0,
                        name: b"y".to_vec(),
                        flags: 0,
                        created: 0,
                    },
                ),
                (3, Record::Del { id: x }),
            ],
            0,
        );
        // A live event queued *behind* the tail: it must not apply until
        // replay drains, or ordering breaks (the crawl/rescan duplicate
        // case in `drain`'s docs).
        a.push(crate::event::FsEvent::upsert(
            crate::event::DirKey::new(r0),
            b"live".to_vec(),
            false,
        ));
        assert_eq!(a.pending_replay_len(), 3);

        // Drain fully: replay first, then the queued event.
        let n = a.drain();
        assert_eq!(n, 4, "3 replay records + 1 live event");
        assert_eq!(a.pending_replay_len(), 0);
        let ix = shared.index.read().unwrap();
        assert!(ix.lookup(r0, b"x").is_none(), "tail del must win");
        assert!(ix.lookup(r0, b"y").is_some(), "tail add must land");
        assert!(ix.lookup(r0, b"live").is_some(), "live event applies after");
        assert_eq!(ix.last_lsn(), 3, "index clock follows applied prefix");
        assert_eq!(shared.snapshot_lsn(), 3, "shared clock follows applied prefix");
    }

    /// The startup credit folds into the compaction counter when the last
    /// replay chunk drains — deferred compaction fires via the normal path.
    #[test]
    fn replay_credit_defers_compaction() {
        let (mut a, _shared, r0) = test_applier();
        a.compaction_threshold = 100;
        let x = r0 + 1;
        a.set_pending_replay(
            vec![(
                7,
                Record::Add {
                    id: x,
                    parent: r0,
                    name: b"x".to_vec(),
                    flags: 0,
                    created: 0,
                },
            )],
            100,
        );
        assert_eq!(a.records_since_compaction, 0);
        a.drain();
        assert_eq!(a.pending_replay_len(), 0);
        assert_eq!(
            a.records_since_compaction, 100,
            "credit lands when the last chunk drains"
        );
        assert!(a.maybe_compact(), "deferred compaction fires via normal path");
    }

    /// The tombstone-ratio arm fires when dead slots accumulate even though
    /// the journal is quiet (churn-then-idle). Below the absolute floor it
    /// stays quiet: a tiny index must not pay a generation bump for a few
    /// deletes.
    #[test]
    fn tombstone_ratio_triggers_compaction() {
        let (mut a, shared, r0) = test_applier();
        // Quiet journal: the record-count arm cannot fire.
        a.compaction_threshold = u64::MAX;
        a.records_since_compaction = 0;
        // Few deletes on a tiny index: below the absolute floor, no fire.
        a.push(crate::event::FsEvent::upsert(
            crate::event::DirKey::new(r0),
            b"small.rs".to_vec(),
            false,
        ));
        a.drain();
        let sid = shared.index.read().unwrap().lookup(r0, b"small.rs").unwrap();
        shared.index.write().unwrap().remove(sid);
        assert!(
            !a.maybe_compact(),
            "a handful of tombstones must not compact a tiny index"
        );
        // Bulk insert + bulk delete through the real event funnel: tombstone
        // ratio crosses 20% with 10k+ dead slots, journal still quiet.
        for i in 0..12_000 {
            a.push(crate::event::FsEvent::upsert(
                crate::event::DirKey::new(r0),
                format!("bulk_{i:05}.rs").into_bytes(),
                false,
            ));
        }
        a.drain();
        for i in 0..12_000 {
            a.push(crate::event::FsEvent::remove(
                crate::event::DirKey::new(r0),
                format!("bulk_{i:05}.rs").into_bytes(),
            ));
        }
        a.drain();
        let tombs = shared.index.read().unwrap().tombstone_count();
        assert!(tombs >= 10_000, "test must build real tombstone pressure, got {tombs}");
        assert!(a.maybe_compact(), "ratio arm must fire on 10k+ tombstones at >20%");
        let tombs_after = shared.index.read().unwrap().tombstone_count();
        assert_eq!(tombs_after, 0, "compaction must purge tombstones, left {tombs_after}");
        // The survivor is still resolvable (ids remapped, root table fixed).
        assert!(
            shared.index.read().unwrap().lookup(r0, b"small.rs").is_none(),
            "small.rs was deleted above; lookup must stay gone, not corrupt"
        );
    }

    /// Parent-relative dir links resolve to the same absolute paths the old
    /// absolute-PathBuf cache returned, share prefixes (one ancestor stored
    /// once), and terminate on cycles.
    #[test]
    fn relative_links_resolve_share_and_terminate() {
        let (mut a, _shared, r0) = test_applier();
        // Deep chain under the root: each level shares its parent's prefix.
        let mut parent = r0;
        let mut expect = std::path::PathBuf::from("/model");
        for name in ["a", "bb", "ccc", "dddd"] {
            let id = a.shared.index.write().unwrap().insert(
                parent,
                name.as_bytes(),
                crate::index::EntryFlags::EMPTY.with_dir(true),
            );
            a.insert_child_path(id, parent, name.as_bytes());
            expect.push(name);
            let got = a.abs_path_for(id).expect("chain must resolve");
            assert_eq!(got, expect, "level {name}");
            parent = id;
        }
        // Shared prefix stored once: the map holds 1 root + 4 small links,
        // not 4 absolute paths re-storing every ancestor's bytes.
        let small = a
            .dir_paths
            .values()
            .filter(|n| matches!(n, DirPathNode::Child { .. }))
            .count();
        assert_eq!(small, 4);
        for node in a.dir_paths.values() {
            if let DirPathNode::Child { name, .. } = node {
                assert!(name.len() <= 4, "links hold own names only, got {name:?}");
            }
        }
        // Reverse lookup still works through materialization.
        assert_eq!(a.dir_id_for_path(&expect), Some(parent));
        assert_eq!(a.dir_id_for_path(std::path::Path::new("/nope")), None);
        // A 2-cycle terminates instead of hanging.
        let x = 900_001u32;
        let y = 900_002u32;
        a.dir_paths.insert(
            x,
            DirPathNode::Child {
                parent: y,
                name: OsString::from("x"),
            },
        );
        a.dir_paths.insert(
            y,
            DirPathNode::Child {
                parent: x,
                name: OsString::from("y"),
            },
        );
        assert_eq!(a.abs_path_for(x), None, "cycle must terminate as None");
    }

    /// Replaying twice is safe: records are idempotent (`Add` re-applied,
    #[test]
    fn creation_minutes_are_whole_and_before_epoch_is_unknown() {
        use std::time::{Duration, UNIX_EPOCH};
        assert_eq!(super::created_minutes_at(UNIX_EPOCH), 0);
        assert_eq!(super::created_minutes_at(UNIX_EPOCH + Duration::from_secs(59)), 0);
        assert_eq!(super::created_minutes_at(UNIX_EPOCH + Duration::from_secs(60)), 1);
        assert_eq!(
            super::created_minutes_at(UNIX_EPOCH + Duration::from_secs(29_491_680 * 60)),
            29_491_680,
            "2026-01-02 rounds down to its own minute"
        );
        assert_eq!(
            super::created_minutes_at(UNIX_EPOCH - Duration::from_secs(60)),
            CREATED_UNKNOWN,
            "before the epoch reads as unknown, not as a giant number or 1970"
        );
        // The first minute of 1970 is a real, storable time, distinct from the
        // unknown sentinel: a file created at 59 s is not an undated file.
        assert_ne!(super::created_minutes_at(UNIX_EPOCH + Duration::from_secs(59)), CREATED_UNKNOWN);
    }

    #[test]
    fn a_real_file_carries_its_birth_minute_out_of_read_dir() {
        use std::time::UNIX_EPOCH;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("dated.txt");
        std::fs::write(&p, b"x").unwrap();
        let (_, created) = super::fs_facts(&p, false, false);
        if created == CREATED_UNKNOWN {
            // No birth time on this filesystem/kernel: the documented
            // degradation is "unknown", and there is nothing to assert beyond
            // that it did not invent a time.
            eprintln!("this filesystem reports no birth time; skipping the dated half");
            return;
        }
        let now_min = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            / 60;
        assert!(
            now_min.saturating_sub(created as u64) <= 5,
            "a file written just now should be dated now, got {created} vs {now_min}"
        );
        // The crawl's own listing must carry the same fact — that is the path
        // the index is populated from.
        let listing = <RealFs as FsView>::read_dir(&RealFs, d.path()).unwrap();
        let item = listing
            .iter()
            .find(|i| i.name == b"dated.txt")
            .expect("the file is in the listing");
        assert_eq!(item.created_min, created);
    }

    /// `Del` of a tombstone a no-op), so a crash mid-replay converges.
    #[test]
    fn replay_is_idempotent() {
        let (mut a, shared, r0) = test_applier();
        let x = r0 + 1;
        let tail = vec![
            (
                1,
                Record::Add {
                    id: x,
                    parent: r0,
                    name: b"x".to_vec(),
                    flags: 0,
                    created: 0,
                },
            ),
            (2, Record::Del { id: x }),
        ];
        a.set_pending_replay(tail.clone(), 0);
        a.drain();
        // Simulate a crash before the journal advanced: replay the same tail.
        a.set_pending_replay(tail, 0);
        a.drain();
        let ix = shared.index.read().unwrap();
        assert!(ix.lookup(r0, b"x").is_none());
        assert_eq!(ix.last_lsn(), 2);
    }
}

#[cfg(test)]
mod replay_clock_tests {
    use super::*;
    use crate::journal::Record;
    use crate::persist::Store;
    use crate::roots::Root;
    use crate::sync::SyncRegistry;
    use std::sync::Arc;

    /// The shared clock never moves backwards when live writes interleave
    /// with background replay: the journal appends at LSN 100 while the
    /// replay tail still carries single-digit LSNs. `fetch_max` on both
    /// paths is what keeps `c:gen:lsn` cursors monotonic — an exact store
    /// on either side would regress a served cursor.
    #[test]
    fn shared_clock_is_monotonic_under_interleave() {
        let dir = tempfile::tempdir().unwrap();
        let dir: &'static std::path::Path = Box::leak(Box::new(dir.keep()));
        let store = Store::open(&dir.join("state")).unwrap();
        let mut ix = Index::new();
        let r0 = ix.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let shared = Shared::new(ix, vec![Root::new(0, PathBuf::from("/model"))]);
        let fs: Arc<dyn FsView> = Arc::new(crate::modelfs::ModelFs::with([]));
        let mut a = Applier::new(shared.clone(), store, fs, SyncRegistry::new());
        a.rebuild_dir_cache();

        // A live write lands first, at journal LSN 1... but pretend the
        // journal is far ahead (a previous life wrote 100 records): the
        // clock must already read past anything replay will carry.
        a.emit(Record::Add {
            id: r0 + 10,
            parent: r0,
            name: b"live".to_vec(),
            flags: 0,
            created: 0,
        });
        shared.lsn.store(100, Ordering::Release);

        // Now the startup tail replays history at LSNs 1..3. The index
        // tracks the replay prefix exactly; the shared clock must NOT
        // follow it down.
        let x = r0 + 1;
        a.set_pending_replay(
            vec![
                (
                    1,
                    Record::Add {
                        id: x,
                        parent: r0,
                        name: b"x".to_vec(),
                        flags: 0,
                        created: 0,
                    },
                ),
                (2, Record::Del { id: x }),
            ],
            0,
        );
        a.drain();
        assert_eq!(
            shared.index.read().unwrap().last_lsn(),
            2,
            "index tracks the replay prefix exactly (crash → re-skip)"
        );
        assert_eq!(
            shared.snapshot_lsn(),
            100,
            "shared clock is a high-water mark: live 100 beats replay 2"
        );
    }

    /// A view whose entries carry creation minutes and exec bits the test can
    /// change, so a create event can report a *new* birth time (a file deleted
    /// and recreated under the same name) without waiting a minute on a real
    /// disk. An entry with no date carries [`CREATED_UNKNOWN`], exactly as a
    /// real filesystem without birth times reports it.
    struct DatedFs {
        inner: crate::modelfs::ModelFs,
        /// name -> (created minute, exec)
        facts: std::sync::Mutex<HashMap<Vec<u8>, (u32, bool)>>,
    }

    impl DatedFs {
        fn new(inner: crate::modelfs::ModelFs, name: &[u8], created: u32, exec: bool) -> DatedFs {
            let mut facts = HashMap::new();
            facts.insert(name.to_vec(), (created, exec));
            DatedFs {
                inner,
                facts: std::sync::Mutex::new(facts),
            }
        }

        fn set(&self, name: &[u8], created: u32, exec: bool) {
            self.facts
                .lock()
                .unwrap()
                .insert(name.to_vec(), (created, exec));
        }
    }

    impl FsView for DatedFs {
        fn read_dir(&self, abs_path: &Path) -> io::Result<Vec<DirItem>> {
            let mut items = self.inner.read_dir(abs_path)?;
            let facts = self.facts.lock().unwrap();
            for it in &mut items {
                if let Some((created, exec)) = facts.get(&it.name) {
                    it.created_min = *created;
                    it.is_exec = *exec;
                }
            }
            Ok(items)
        }

        fn stat(&self, abs_path: &Path) -> io::Result<Option<StatInfo>> {
            self.inner.stat(abs_path)
        }

        fn facts(&self, abs_path: &Path, _is_dir: bool, _is_symlink: bool) -> (bool, u32) {
            let name = abs_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned().into_bytes())
                .unwrap_or_default();
            self.facts
                .lock()
                .unwrap()
                .get(&name)
                .map(|(created, exec)| (*exec, *created))
                .unwrap_or((false, 0))
        }
    }

    /// The durability contract for a recreated entry: the refreshed minute is
    /// an `Add` keyed to the same id, written with the name, so replaying the
    /// journal alone (no snapshot) restores the row with its id, its new
    /// minute, and its flags. A rescan that learns nothing writes nothing.
    #[test]
    fn a_recreated_entrys_new_minute_is_journaled_with_its_name_and_survives_replay() {
        use crate::modelfs::{ModelFs, Node};
        use crate::persist::{apply_record, Store};
        use crate::roots::Root;
        use crate::sync::SyncRegistry;
        use std::sync::Arc;

        let first = 29_455_200u32; // 2026-01-02, in minutes
        let second = first + 1_440; // ...one day later

        let dir = tempfile::tempdir().unwrap();
        let statedir = dir.path().join("state");
        let store = Store::open(&statedir).unwrap();
        let mut ix = Index::new();
        let r0 = ix.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let shared = Shared::new(ix, vec![Root::new(0, PathBuf::from("/model"))]);
        let fs = Arc::new(DatedFs::new(
            ModelFs::with([("keep.txt", Node::File)]),
            b"keep.txt",
            first,
            false,
        ));
        let view: Arc<dyn FsView> = fs.clone();
        let mut a = Applier::new(shared.clone(), store, view, SyncRegistry::new());
        a.rebuild_dir_cache();
        a.pending_mut().enqueue_crawl(r0);
        a.drain();

        let keep = shared
            .index
            .read()
            .unwrap()
            .lookup(r0, b"keep.txt")
            .expect("the crawl must index keep.txt");
        assert_eq!(
            shared.index.read().unwrap().created_min(keep),
            Some(first),
            "the crawl must store the first birth minute"
        );
        let after_crawl = a.store().journal.count();

        // A rescan that sees the same names, minutes and flags must append
        // nothing, or idle rescans would grow the journal forever.
        a.pending_mut()
            .enqueue_rescan(r0, crate::event::RescanCause::Manual);
        a.drain();
        assert_eq!(
            a.store().journal.count(),
            after_crawl,
            "a no-op rescan grew the journal"
        );
        assert_eq!(shared.index.read().unwrap().created_min(keep), Some(first));

        // The same name is recreated (a replace no delete event reached us
        // for), with a new birth minute. The create path must keep the id and
        // journal the new minute with the name, and the record it writes must
        // carry the *live* flags — the event hint is not authoritative for an
        // entry that already exists.
        let live_flags = shared.index.read().unwrap().entry(keep).unwrap().flags().0;
        fs.set(b"keep.txt", second, false);
        a.apply_event(FsEvent::Upsert {
            parent: crate::event::DirKey::new(r0),
            name: b"keep.txt".to_vec(),
            is_dir: false,
        });
        assert_eq!(
            a.store().journal.count(),
            after_crawl + 1,
            "the refresh must be exactly one record"
        );
        {
            let ix = shared.index.read().unwrap();
            assert_eq!(
                ix.lookup(r0, b"keep.txt"),
                Some(keep),
                "the recreated entry must keep its id"
            );
            assert_eq!(ix.created_min(keep), Some(second));
            assert_eq!(
                ix.entry(keep).unwrap().flags().0,
                live_flags,
                "the refresh record must carry the live flags, not the hint"
            );
        }
        drop(a);

        // Rebuild from the journal alone: a fresh index seeded only with the
        // root entry (the one thing a snapshot would supply), then every
        // record. No snapshot exists in this store.
        let mut store2 = Store::open(&statedir).unwrap();
        assert!(
            store2.load_snapshot().unwrap().is_none(),
            "the test must prove the journal alone carries the minute"
        );
        let mut ix2 = Index::new();
        let r0b = ix2.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        assert_eq!(r0b, r0);
        let mut applied: Vec<(u64, Record)> = Vec::new();
        store2
            .replay(&mut ix2, |lsn, rec| applied.push((lsn, rec.clone())))
            .unwrap();
        for (_, rec) in &applied {
            apply_record(&mut ix2, rec);
        }
        let replayed = ix2
            .lookup(r0, b"keep.txt")
            .expect("replay must restore keep.txt");
        assert_eq!(replayed, keep, "replay must keep the entry's id");
        assert_eq!(
            ix2.created_min(replayed),
            Some(second),
            "replay lost the refreshed creation minute"
        );
        assert_eq!(
            ix2.entry(replayed).unwrap().flags().0,
            live_flags,
            "the same record must restore the flags too"
        );
        assert_eq!(ix2.iter_live().count(), 2, "no duplicate slot may be created");
    }

    /// The first rescan of a store that predates creation times learns a date
    /// for *every* entry. That is a learn, not a change, so it must not turn
    /// into one journal record per file; but the minutes must still be live in
    /// the index at once, and a later change to one of them must still be
    /// journaled (one record), because a restart would otherwise resurrect the
    /// old date.
    #[test]
    fn an_upgrade_rescan_learns_every_date_without_a_journal_burst() {
        use crate::modelfs::{ModelFs, Node};

        let dir = tempfile::tempdir().unwrap();
        let statedir = dir.path().join("state");
        let store = Store::open(&statedir).unwrap();
        let mut ix = Index::new();
        let r0 = ix.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let shared = Shared::new(ix, vec![Root::new(0, PathBuf::from("/model"))]);
        // A view with no dates yet: the crawl behaves as a build that never
        // stored creation times did.
        let fs = Arc::new(DatedFs::new(
            ModelFs::with([
                ("a.txt", Node::File),
                ("b.txt", Node::File),
                ("c.txt", Node::File),
            ]),
            b"a.txt",
            CREATED_UNKNOWN,
            false,
        ));
        let view: Arc<dyn FsView> = fs.clone();
        let mut a = Applier::new(shared.clone(), store, view, SyncRegistry::new());
        a.rebuild_dir_cache();
        a.pending_mut().enqueue_crawl(r0);
        a.drain();

        let mut crawled: Vec<(Vec<u8>, u32)> = Vec::new();
        for name in [b"a.txt".as_slice(), b"b.txt", b"c.txt"] {
            let ix = shared.index.read().unwrap();
            let id = ix.lookup(r0, name).expect("the crawl must index every file");
            assert_eq!(
                ix.created_min(id),
                None,
                "the store starts out undated, as a pre-v3 snapshot leaves it"
            );
            crawled.push((name.to_vec(), id));
        }
        let after_crawl = a.store().journal.count();

        // First rescan after the upgrade: every entry gets a date. One `Add`
        // per entry here would journal the entire index for re-derived state.
        fs.set(b"a.txt", 1_000, false);
        fs.set(b"b.txt", 1_001, false);
        fs.set(b"c.txt", 1_002, false);
        a.pending_mut()
            .enqueue_rescan(r0, crate::event::RescanCause::Manual);
        a.drain();
        assert_eq!(
            a.store().journal.count(),
            after_crawl,
            "the first rescan of an undated store must not journal every file"
        );
        let b_id = {
            let ix = shared.index.read().unwrap();
            // The learn must reach the *survivor* branch: a re-insert would
            // journal an Add and hand out new ids.
            for (name, id) in &crawled {
                assert_eq!(
                    ix.lookup(r0, name),
                    Some(*id),
                    "the rescan must keep the crawl's ids, not re-insert"
                );
            }
            let b_id = ix.lookup(r0, b"b.txt").unwrap();
            for (id, minute) in [
                (ix.lookup(r0, b"a.txt").unwrap(), 1_000u32),
                (b_id, 1_001),
                (ix.lookup(r0, b"c.txt").unwrap(), 1_002),
            ] {
                assert_eq!(
                    ix.created_min(id),
                    Some(minute),
                    "the learned date must be live in the index immediately"
                );
            }
            b_id
        };

        // A rescan that learns nothing new writes nothing...
        a.pending_mut()
            .enqueue_rescan(r0, crate::event::RescanCause::Manual);
        a.drain();
        assert_eq!(
            a.store().journal.count(),
            after_crawl,
            "a rescan that learns nothing new grew the journal"
        );

        // ... and a filesystem that cannot report a birth time must not erase
        // one the index already knows: "unknown" is not the epoch.
        fs.set(b"b.txt", CREATED_UNKNOWN, false);
        a.pending_mut()
            .enqueue_rescan(r0, crate::event::RescanCause::Manual);
        a.drain();
        assert_eq!(
            a.store().journal.count(),
            after_crawl,
            "an unknown minute must not be journaled over a known one"
        );
        assert_eq!(
            shared.index.read().unwrap().created_min(b_id),
            Some(1_001),
            "a known minute must never be clobbered back to unknown"
        );

        // A minute that *changed* is still durable: exactly one record.
        fs.set(b"b.txt", 2_000, false);
        a.pending_mut()
            .enqueue_rescan(r0, crate::event::RescanCause::Manual);
        a.drain();
        assert_eq!(
            a.store().journal.count(),
            after_crawl + 1,
            "a changed minute must be exactly one record"
        );
        assert_eq!(
            shared.index.read().unwrap().created_min(b_id),
            Some(2_000)
        );
        drop(a);

        // Rebuild from the journal alone: the changed minute survives a
        // restart under the entry's original id.
        let mut store2 = Store::open(&statedir).unwrap();
        let mut ix2 = Index::new();
        let r0b = ix2.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        assert_eq!(r0b, r0);
        let mut applied: Vec<Record> = Vec::new();
        store2
            .replay(&mut ix2, |_, rec| applied.push(rec.clone()))
            .unwrap();
        for rec in &applied {
            crate::persist::apply_record(&mut ix2, rec);
        }
        let replayed = ix2
            .lookup(r0, b"b.txt")
            .expect("replay must restore b.txt");
        assert_eq!(replayed, b_id, "replay must keep the entry's id");
        assert_eq!(
            ix2.created_min(replayed),
            Some(2_000),
            "replay lost the changed minute"
        );
        assert_eq!(
            ix2.iter_live().count(),
            4,
            "replay must not duplicate slots: one root plus three files"
        );
    }

    /// The refresh record is an upsert on replay, not an insert: a slot that is
    /// a tombstone in the rebuilt index comes back alive under the same id,
    /// with the record's name, flags and minute.
    #[test]
    fn replaying_an_add_over_a_tombstoned_slot_resurrects_it_with_the_records_flags() {
        use crate::persist::apply_record;

        let mut ix = Index::new();
        let r0 = ix.insert(
            ROOT_ID,
            b"/model",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let id = ix.insert(r0, b"gone.txt", EntryFlags::EMPTY);
        ix.set_created(id, 10);
        assert!(ix.remove(id));
        assert!(!ix.is_alive(id));

        apply_record(
            &mut ix,
            &Record::Add {
                id,
                parent: r0,
                name: b"gone.txt".to_vec(),
                flags: EntryFlags::EMPTY.with_exec(true).0,
                created: 20,
            },
        );

        assert!(
            ix.is_alive(id),
            "replay must resurrect the slot, not pad a new one"
        );
        assert_eq!(ix.lookup(r0, b"gone.txt"), Some(id), "the id must be kept");
        assert_eq!(ix.name(id), b"gone.txt".as_slice());
        assert_eq!(ix.created_min(id), Some(20));
        assert!(ix.entry(id).unwrap().flags().is_exec());
        assert_eq!(
            ix.iter_live().count(),
            2,
            "no duplicate slot may be created"
        );
    }
}
