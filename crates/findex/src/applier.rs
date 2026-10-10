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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirItem {
    pub name: Vec<u8>,
    pub is_dir: bool,
    pub is_symlink: bool,
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
            out.push(DirItem {
                name,
                is_dir: ft.is_dir(),
                is_symlink: ft.is_symlink(),
            });
        }
        Ok(out)
    }

    fn stat(&self, abs_path: &Path) -> io::Result<Option<StatInfo>> {
        match std::fs::symlink_metadata(abs_path) {
            Ok(md) => {
                use std::os::unix::fs::MetadataExt;
                Ok(Some(StatInfo {
                    is_dir: md.is_dir(),
                    is_symlink: md.file_type().is_symlink(),
                    dev: md.dev(),
                    ino: md.ino(),
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
    /// Keyed by `(generation, entry_count)`:
    /// * a generation bump (snapshot load / full rescan / compaction) always
    ///   invalidates it;
    /// * within a generation, `entry_count` rises by one for every new entry, so
    ///   any addition invalidates it too.
    ///
    /// A *removal* does not bump `entry_count`, so the cached postings may still
    /// name tombstoned ids. That is safe by construction: the accelerator is a
    /// filter and the authoritative matcher drops dead ids — a stale posting can
    /// only ever add a candidate, never remove a real hit.
    #[cfg(feature = "trigram")]
    pub trigram: RwLock<Option<(u64, usize, Arc<crate::trigram::TrigramIndex>)>>,
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
        })
    }

    /// The trigram accelerator for the current index, building it if the cache
    /// is cold or stale. Cheap after the first call for a given index shape.
    ///
    /// The index read lock is taken by the caller separately; this takes only
    /// the cache lock and, on a miss, the index lock to build. Callers must not
    /// hold the index read lock while calling this to avoid a lock-order hazard
    /// — the query path builds the accelerator *before* taking the index lock.
    ///
    /// Only available with the default-on `trigram` cargo feature.
    #[cfg(feature = "trigram")]
    pub fn trigram_index(&self) -> Arc<crate::trigram::TrigramIndex> {
        let (gen, entries) = {
            let ix = self.index.read().unwrap();
            (ix.generation(), ix.entry_count())
        };
        if let Some((g, e, idx)) = self.trigram.read().unwrap().as_ref() {
            if *g == gen && *e == entries {
                return Arc::clone(idx);
            }
        }
        // Rebuild under the index read lock, then publish.
        let built = {
            let ix = self.index.read().unwrap();
            Arc::new(crate::trigram::TrigramIndex::build(&ix))
        };
        let mut slot = self.trigram.write().unwrap();
        // Another thread may have built it while we waited; keep its result.
        if let Some((g, e, idx)) = slot.as_ref() {
            if *g == gen && *e == entries {
                return Arc::clone(idx);
            }
        }
        *slot = Some((gen, entries, Arc::clone(&built)));
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
        Applier {
            shared,
            store,
            pending: Pending::new(100_000),
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
        }
    }

    pub fn pending(&self) -> &Pending {
        &self.pending
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
    fn abs_path_for(&mut self, id: u32) -> Option<PathBuf> {
        if let Some(p) = self.dir_paths.get(&id) {
            return Some(p.clone());
        }
        let (name, parent) = {
            let ix = self.shared.index.read().unwrap();
            let e = ix.entry(id)?;
            (ix.name(id).to_vec(), e.parent)
        };
        let parent_path = self.abs_path_for(parent)?;
        let mut p = parent_path;
        p.push(std::ffi::OsString::from(String::from_utf8_lossy(&name).into_owned()));
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
        // Apply to the live index regardless of journaling.
        let mut ix = self.shared.index.write().unwrap();
        apply_record(&mut ix, &rec);
    }

    /// Apply one event.
    fn apply_event(&mut self, ev: FsEvent) {
        match ev {
            FsEvent::Upsert { parent, name, is_dir } => {
                self.apply_upsert(parent.0, name, is_dir);
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
        let mut ix = self.shared.index.write().unwrap();
        if parent as usize >= ix.entry_count() || !ix.is_alive(parent) || !ix.is_dir(parent) {
            return; // event for a dir we do not (yet) know
        }
        // Use the hint to pick flags; the crawler will correct type later.
        let flags = EntryFlags::EMPTY.with_dir(is_dir_hint);
        match ix.lookup(parent, &name) {
            Some(id) => {
                // Refresh the type if it changed (e.g. file → dir via rename).
                let cur = ix.entry(id).unwrap().flags();
                if cur.is_dir() != is_dir_hint {
                    let nf = EntryFlags(cur.0).with_dir(is_dir_hint);
                    ix.entry_mut(id).flags = nf.0;
                }
                let _ = id;
            }
            None => {
                let id = ix.insert(parent, &name, flags);
                drop(ix);
                let rec = Record::Add {
                    id,
                    parent,
                    name,
                    flags: flags.0,
                };
                self.emit(rec);
                return;
            }
        }
        drop(ix);
        // New directory: watch it, then crawl it to learn its children.
        if is_dir_hint {
            let existing = {
                let ix = self.shared.index.read().unwrap();
                ix.lookup(parent, &name)
            };
            if let Some(id) = existing {
                if let Some(abs) = self.abs_path_for(parent) {
                    let child_abs =
                        abs.join(std::ffi::OsString::from(String::from_utf8_lossy(&name).into_owned()));
                    self.dir_paths.insert(id, child_abs.clone());
                    self.record_watch_request(id, child_abs);
                }
                self.pending.enqueue_crawl(id);
            }
        }
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
        let Some(abs) = self.abs_path_for(dir) else { return };
        let items = match self.fs.read_dir(&abs) {
            Ok(v) => v,
            Err(_) => {
                self.shared.health.write().unwrap().read_dir_errors += 1;
                return;
            }
        };
        let mut new_dirs = Vec::new();
        for it in items {
            if it.name == b"." || it.name == b".." {
                continue;
            }
            // Sync cookies must never be indexed (§8, §16); `diff_subtree` and
            // the event path filter them, and so must the initial/subtree crawl.
            if crate::util::is_cookie(&it.name) {
                continue;
            }
            let exists = {
                let ix = self.shared.index.read().unwrap();
                ix.lookup(dir, &it.name)
            };
            if exists.is_none() {
                let flags = EntryFlags::EMPTY.with_dir(it.is_dir).with_symlink(it.is_symlink);
                let mut ix = self.shared.index.write().unwrap();
                let id = ix.insert(dir, &it.name, flags);
                drop(ix);
                self.emit(Record::Add { id, parent: dir, name: it.name.clone(), flags: flags.0 });
                if it.is_dir && !it.is_symlink {
                    // Watch before enumerating: install a watch, then queue the
                    // crawl of the new directory (§5 race rule).
                    let child_abs = abs.join(std::ffi::OsString::from(
                        String::from_utf8_lossy(&it.name).into_owned(),
                    ));
                    self.dir_paths.insert(id, child_abs.clone());
                    self.record_watch_request(id, child_abs);
                    new_dirs.push(id);
                }
            } else if it.is_dir && !it.is_symlink {
                // A directory we already know: make sure its path is cached so
                // later `crawl_dir`/`rescan` calls can reach it.
                if let Some(id) = exists {
                    let _ = self.abs_path_for(id);
                }
            }
        }
        for d in new_dirs {
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
                    let flags = EntryFlags::EMPTY.with_dir(it.is_dir).with_symlink(it.is_symlink);
                    let id = {
                        let mut ix = self.shared.index.write().unwrap();
                        ix.insert(dir, name, flags)
                    };
                    self.emit(Record::Add { id, parent: dir, name: name.clone(), flags: flags.0 });
                    if it.is_dir && !it.is_symlink {
                        let child_abs = abs.join(std::ffi::OsString::from(
                            String::from_utf8_lossy(name).into_owned(),
                        ));
                        self.dir_paths.insert(id, child_abs.clone());
                        self.record_watch_request(id, child_abs);
                        self.pending.enqueue_crawl(id);
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
                        // Recurse into surviving directories.
                        if it.is_dir && !it.is_symlink {
                            let child_abs = abs.join(std::ffi::OsString::from(
                                String::from_utf8_lossy(name).into_owned(),
                            ));
                            self.dir_paths.insert(*id, child_abs.clone());
                            self.diff_subtree(*id, &child_abs);
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
    pub fn drain(&mut self) -> usize {
        let mut n = 0;
        loop {
            let mut progressed = false;
            while let Some(ev) = self.pending.pop_event() {
                self.apply_event(ev);
                n += 1;
                progressed = true;
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
        self.dir_paths.iter().find(|(_, p)| p.as_path() == path).map(|(id, _)| *id)
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
        let compacted = ix_snapshot.compacted();
        let new_gen = self.shared.generation() + 1;
        let mut compacted = compacted;
        compacted.set_generation(new_gen);
        compacted.set_last_lsn(self.shared.snapshot_lsn());
        {
            let mut ix = self.shared.index.write().unwrap();
            *ix = compacted;
        }
        self.shared.generation.store(new_gen, Ordering::Release);
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
/// name is the final component of the path and parent is ROOT_ID.
pub fn root_dir_id(ix: &Index, root: &Root) -> u32 {
    // Roots are inserted at crawl time with the basename as their name.
    let basename = root
        .path
        .file_name()
        .map(os_str_bytes)
        .unwrap_or_default();
    if basename.is_empty() {
        return ROOT_ID;
    }
    ix.lookup(ROOT_ID, &basename).unwrap_or(ROOT_ID)
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
        let r = ix.insert(ROOT_ID, b"r", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        let a = ix.insert(r, b"a", EntryFlags::EMPTY.with_dir(true));
        let b = ix.insert(r, b"b", EntryFlags::EMPTY.with_dir(true));
        ix.insert(a, b"x", EntryFlags::EMPTY);
        ix.insert(b, b"y", EntryFlags::EMPTY);
        let d = descendants(&ix, r);
        assert_eq!(d.len(), 4);
    }
}
