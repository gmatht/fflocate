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
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::event::{FsEvent, RescanCause};
use crate::health::Health;
use crate::index::{EntryFlags, Index, ROOT_ID};
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatInfo {
    pub is_dir: bool,
    pub is_symlink: bool,
    /// `st_dev`/`st_ino`; zero when unavailable.
    pub dev: u64,
    pub ino: u64,
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
            out.push(DirItem {
                name,
                is_dir,
                is_symlink,
                is_exec: is_executable(&ent.path(), is_dir, is_symlink),
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
                    }))
                }
                #[cfg(not(unix))]
                {
                    Ok(Some(StatInfo {
                        is_dir: md.is_dir(),
                        is_symlink: md.file_type().is_symlink(),
                        dev: 0,
                        ino: 0,
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
pub fn is_executable(path: &Path, is_dir: bool, _is_symlink: bool) -> bool {
    if is_dir {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(path) {
            Ok(md) => md.is_file() && md.mode() & 0o111 != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

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
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The applier: single writer over [`Shared`].
pub struct Applier {
    pub shared: Arc<Shared>,
    store: Store,
    pending: Pending,
    fs: Arc<dyn FsView>,
    sync: Arc<SyncRegistry>,
    /// Path cache: dir id → absolute path, for the real backend.
    dir_paths: HashMap<u32, PathBuf>,
    /// Root dir id per root index.
    root_dir_ids: Vec<u32>,
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
        Applier {
            shared,
            store,
            pending,
            fs,
            sync,
            dir_paths: HashMap::new(),
            root_dir_ids,
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

    /// May `dir` (at `abs`) be enumerated? A mount point that is not itself a
    /// root is another filesystem's business: the directory itself is
    /// recorded, but it is never descended into, watched, or queued — its own
    /// root (if indexed at all) covers it.
    fn may_descend(&self, dir: u32, abs: &std::path::Path) -> bool {
        self.root_dir_ids.contains(&dir) || !self.is_mountpoint(abs)
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
        for (i, r) in roots.iter().enumerate() {
            let rid = self.root_dir_ids[i];
            if rid != ROOT_ID {
                self.dir_paths.insert(rid, r.path.clone());
            }
        }
    }

    /// Absolute path of directory `id`, descending from the nearest cached
    /// ancestor. Non-UTF-8 names lose fidelity through this path (only used to
    /// reach real directories); the byte-exact crawler path does not.
    ///
    /// Terminates on a broken or cyclic parent chain by returning `None`. A
    /// naive recursive walk would loop forever if an entry's parent chain never
    /// reached a cached ancestor (an un-cached root, a self-parented entry) —
    /// which turns a misconfigured root into a stack overflow instead of an
    /// error. The depth bound is the index size: no real chain is longer.
    fn abs_path_for(&mut self, id: u32) -> Option<PathBuf> {
        if let Some(p) = self.dir_paths.get(&id) {
            return Some(p.clone());
        }
        let (name, parent) = {
            let ix = self.shared.index.read().unwrap();
            let e = ix.entry(id)?;
            (ix.name(id).to_vec(), e.parent)
        };
        // A single-entry cycle (an entry parented to itself) is the degenerate
        // case that bites first; refuse it before recursing.
        if parent == id {
            return None;
        }
        let parent_path = self.abs_path_for(parent)?;
        let mut p = parent_path;
        p.push(std::ffi::OsString::from(
            String::from_utf8_lossy(&name).into_owned(),
        ));
        self.dir_paths.insert(id, p.clone());
        Some(p)
    }

    fn emit(&mut self, rec: Record) {
        if self.journaling {
            match self.store.append(&rec) {
                Ok(lsn) => {
                    self.shared.lsn.store(lsn, Ordering::Release);
                    self.records_since_compaction += 1;
                }
                Err(_) => {
                    self.shared.health.write().unwrap().journal_write_errors += 1;
                }
            }
        }
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
        let Some(parent_path) = self.dir_paths.get(&parent).cloned() else {
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
        self.record_change();
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
        let _ = self.abs_path_for(parent);
        // The exec bit is statted now rather than defaulting false: a fresh
        // file that arrives via event (not crawl) must carry its real bit
        // from birth, not only after the next rescan. The parent path was
        // just cached above, so this is one `stat`, not a walk.
        let exec_hint = self
            .dir_paths
            .get(&parent)
            .map(|p| {
                is_executable(
                    &p.join(std::ffi::OsString::from(
                        String::from_utf8_lossy(&name).into_owned(),
                    )),
                    is_dir_hint,
                    false,
                )
            })
            .unwrap_or(false);
        let flags = EntryFlags::EMPTY.with_dir(is_dir_hint).with_exec(exec_hint);
        let mut ix = self.shared.index.write().unwrap();
        let lookup = ix.lookup(parent, &name);
        let (created_id, existing_id) = match lookup {
            Some(id) => {
                // Refresh the type if it changed (e.g. file → dir via rename).
                let cur = ix.entry(id).unwrap().flags();
                if cur.is_dir() != is_dir_hint {
                    let nf = EntryFlags(cur.0).with_dir(is_dir_hint);
                    ix.entry_mut(id).flags = nf.0;
                }
                (None, Some(id))
            }
            None => {
                let id = ix.insert(parent, &name, flags);
                (Some(id), None)
            }
        };
        drop(ix);
        if let Some(id) = created_id {
            let rec = Record::Add {
                id,
                parent,
                name: name.clone(),
                flags: flags.0,
            };
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
                if let Some(abs) = self.abs_path_for(parent) {
                    let child_abs = abs.join(std::ffi::OsString::from(
                        String::from_utf8_lossy(&name).into_owned(),
                    ));
                    self.dir_paths.insert(id, child_abs.clone());
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

    /// Re-read one entry's exec bit after a metadata change (`chmod`), correcting
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

    fn record_change(&mut self) {
        let cursor = self.shared.cursor();
        self.shared.changes.write().unwrap().push(cursor);
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
        self.dir_paths.insert(dir, abs.clone());
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
                });
                if it.is_dir && !it.is_symlink {
                    // Watch before enumerating: install a watch, then queue the
                    // crawl of the new directory (§5 race rule) — unless a
                    // mount boundary says this subtree is not ours (§4): the
                    // entry stays as a leaf, covered by its own root if at all.
                    if self.may_descend(id, &child_abs) {
                        self.dir_paths.insert(id, child_abs.clone());
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
                        self.dir_paths.insert(id, child_abs.clone());
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
        // Register the dir's path in the cache if we just learned it.
        self.dir_paths.insert(dir, abs);
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
        self.dir_paths.insert(dir, abs.to_path_buf());
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
                    });
                    if it.is_dir && !it.is_symlink {
                        let child_abs = abs.join(std::ffi::OsString::from(
                            String::from_utf8_lossy(name).into_owned(),
                        ));
                        // Mount boundary: record the mountpoint, never descend.
                        if self.may_descend(id, &child_abs) {
                            self.dir_paths.insert(id, child_abs.clone());
                            self.record_watch_request(id, child_abs);
                            self.pending.enqueue_crawl(id);
                        } else {
                            self.dir_paths.insert(id, child_abs);
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
                        // A survivor's ownership can have changed even though its
                        // name did not (a chown or a replaced file), and after a
                        // restart the record may be missing entirely. Either way a
                        // rescan is the moment to re-read it, or the filter keeps
                        // answering with the old — or no — answer.
                        let survivor_abs = abs.join(std::ffi::OsString::from(
                            String::from_utf8_lossy(name).into_owned(),
                        ));
                        self.note_ownership(*id, &survivor_abs);
                        // Recurse into surviving directories — but never across a
                        // mount boundary: beneath one only legacy leakage can
                        // exist (nothing current is ever inserted there), so
                        // drop it and let the mountpoint stand as a leaf. This
                        // is what heals pre-boundary stores on their next rescan.
                        if it.is_dir && !it.is_symlink {
                            self.dir_paths.insert(*id, survivor_abs.clone());
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
    /// lookup; `None` if the directory is not known.
    pub fn dir_id_for_path(&self, path: &Path) -> Option<u32> {
        self.dir_paths
            .iter()
            .find(|(_, p)| p.as_path() == path)
            .map(|(id, _)| *id)
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

    /// Compact if the journal has grown past the threshold. Returns true if a
    /// snapshot was written.
    pub fn maybe_compact(&mut self) -> bool {
        if self.records_since_compaction < self.compaction_threshold {
            return false;
        }
        self.compact_now()
    }

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
