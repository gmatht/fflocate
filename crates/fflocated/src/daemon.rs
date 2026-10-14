//! The fflocated daemon (§2, §5–§9, §13, §15).
//!
//! Threads:
//!
//! * **watcher** — polls the `WatchBackend` and pushes `FsEvent` batches into a
//!   bounded channel.
//! * **applier** — the single writer; owns the `Pending` collection, applies
//!   events/work, journals, compacts. Also installs watches for newly
//!   discovered directories (the backend is behind a `Mutex`, shared with the
//!   watcher thread).
//! * **socket accept loop** — spawns a thread per client; each client thread
//!   handles a request via `fflocate::server`. Supervised (§15): an
//!   `AcceptSupervisor` re-binds and respawns the loop if it dies, ticked from
//!   both the applier loop and the watcher thread, and counted in
//!   `health.accept_restarts`.
//!
//! Startup is *load → replay → serve immediately*, with `catching_up: true` in
//! meta until the initial crawl lands (§9/§15). Watch first, then enumerate
//! (§5): watches are installed on every directory before it is crawled.
//!
//! Channel discipline: the watcher and the applier exchange work through two
//! bounded channels (events up, watch requests down). Neither side may block
//! indefinitely on its send, because each side's send is the other's consume
//! step — one parked producer wedges both threads forever. The watcher sends
//! event batches with a deadline and services watch requests between retries
//! (see the loop in `start`); the applier's `watch_dir` send is thereby
//! bounded, and always returns to the top of its tick to drain events.
//!
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};

use fflocate::applier::{Applier, FsView, RealFs, Shared};
use fflocate::event::{FsEvent, RescanCause};
use fflocate::index::{EntryFlags, Index, ROOT_ID};
use fflocate::mount;
use fflocate::persist::Store;
use fflocate::roots::Root;
use fflocate::server::{Control, ControlSender, ServerCtx};
use fflocate::sync::SyncRegistry;
#[cfg(target_os = "linux")]
use fflocate::watch::InotifyBackend;
use fflocate::watch::{FakeBackend, WatchBackend};

use crate::config::{BackendChoice, Config};
use std::collections::BTreeMap;

/// Root-dir ids needing a drive-change rescan: a drive was recorded for the
/// path, a drive probes now, and they disagree. Pure for testability; `build`
/// wires it up. Foreign roots are never touched here (their entries belong
/// to another OS), and unknown drives on either side mean "cannot tell",
/// never "changed".
fn drive_changed_rids(
    recorded: &BTreeMap<String, fflocate::persist::RecordedRoot>,
    roots: &[Root],
    index: &Index,
) -> Vec<u32> {
    let mut out = Vec::new();
    for root in roots.iter().filter(|r| r.is_local()) {
        let rec = recorded.get(&root.key()).and_then(|r| r.drive.clone());
        match (rec, root.drive.clone()) {
            (Some(a), Some(b)) if !a.matches(&b) => {
                let rid = fflocate::applier::root_dir_id(index, root);
                if rid != ROOT_ID {
                    out.push(rid);
                }
            }
            _ => {}
        }
    }
    out
}

/// Stored roots made redundant by configured ones: same filesystem (equal,
/// nonzero device ids) with the configured root strictly above the stored
/// one. Pure for testability; `build` probes the device ids. Returns the
/// stored paths to drop (records and subtrees).
///
/// This is the defaults migration (yesterday's `$HOME` root under today's
/// `/home` filesystem root) and the general same-filesystem nesting rule —
/// without it the subtree would be indexed twice. Only provable coverage
/// prunes: unknown device ids never count, and a stored root on its own
/// filesystem (a mount point) is never covered, only adopted or kept.
fn covered_root_paths(stored: &[(String, u64)], configured: &[(String, u64)]) -> Vec<String> {
    let norm = |p: &str| {
        let t = p.trim_end_matches('/');
        if t.is_empty() {
            "/".to_string()
        } else {
            t.to_string()
        }
    };
    let mut out = Vec::new();
    for (spath, sdev) in stored {
        if *sdev == 0 {
            continue; // unprobed: cannot prove coverage, keep
        }
        let spath = norm(spath);
        for (cpath, cdev) in configured {
            if *cdev == 0 || *cdev != *sdev {
                continue;
            }
            let cpath = norm(cpath);
            if cpath != spath && is_ancestor_path(&cpath, &spath) {
                out.push(spath.clone());
                break;
            }
        }
    }
    out
}

/// Is `parent` a strict ancestor directory of `path` (component-wise, not a
/// string prefix: `/home` covers `/home/me` but not `/home2`)? Both already
/// slash-normalized; `/` covers everything below it.
fn is_ancestor_path(parent: &str, path: &str) -> bool {
    if parent == "/" {
        return path.len() > 1 && path.starts_with('/');
    }
    path.len() > parent.len() && path.starts_with(parent) && path.as_bytes()[parent.len()] == b'/'
}

/// The backend shared between the watcher thread (poll) and the applier loop
/// (install watches).
pub type SharedBackend = Arc<Mutex<Box<dyn WatchBackend>>>;

/// A watch that could not be installed. Reported from the watcher thread to the
/// applier so the root can be marked degraded — a failed watch is never silent.
#[derive(Clone, Debug)]
pub struct WatchFailure {
    pub dir: u32,
    pub reason: String,
}

/// Build the configured backend, returning it alongside the **resolved**
/// concrete choice so callers can report what is actually running (an `auto`
/// config must not surface as `"auto"` in `ff stats`).
///
/// `Auto` picks the best available for this system (§4). A backend that is asked
/// for explicitly but unavailable is a hard error, not a silent downgrade: if
/// you asked for fanotify and the kernel cannot do it, you want to know.
pub fn make_backend(choice: BackendChoice) -> anyhow::Result<(SharedBackend, BackendChoice)> {
    let resolved = choice
        .resolve()
        .ok_or_else(|| anyhow::anyhow!("no usable watcher backend on this platform"))?;
    let b: Box<dyn WatchBackend> = match resolved {
        #[cfg(target_os = "linux")]
        BackendChoice::Inotify => match InotifyBackend::new() {
            Ok(b) => Box::new(b),
            Err(e) => anyhow::bail!("inotify unavailable: {e}"),
        },
        // No kernel watcher yet on this platform (FSEvents is v1.5, §17);
        // fail loudly rather than silently running unwatched.
        #[cfg(not(target_os = "linux"))]
        BackendChoice::Inotify => {
            anyhow::bail!("backend `inotify` is Linux-only; use `fake` on this platform")
        }
        #[cfg(target_os = "linux")]
        BackendChoice::Fanotify => match fflocate::watch::FanotifyBackend::new() {
            Ok(b) => Box::new(b),
            Err(e) => anyhow::bail!(
                "fanotify unavailable: {e} (needs kernel >= 5.13 for FAN_REPORT_DFID_NAME)"
            ),
        },
        #[cfg(not(target_os = "linux"))]
        BackendChoice::Fanotify => {
            anyhow::bail!("backend `fanotify` is Linux-only")
        }
        BackendChoice::Fake => Box::new(FakeBackend::new(fflocate::watch::fake::FakeHandle::new())),
        // `resolve` never yields Auto.
        BackendChoice::Auto => unreachable!("Auto is resolved before this match"),
    };
    Ok((Arc::new(Mutex::new(b)), resolved))
}

/// A live daemon.
pub struct Daemon {
    pub shared: Arc<Shared>,
    pub config: Config,
    pub sync: Arc<SyncRegistry>,
    applier: Applier,
    backend: SharedBackend,
    /// The concrete backend in use, after `auto` resolution.
    resolved_backend: BackendChoice,
    /// Dirs already asked to be watched (dedup).
    watched_dirs: std::collections::HashSet<u32>,
    /// Watch requests from the applier to the watcher thread (new dirs).
    watch_tx: Sender<(u32, PathBuf)>,
    /// Watch failures from the watcher thread back to the applier.
    watch_fail_tx: Sender<WatchFailure>,
    watch_fail_rx: Receiver<WatchFailure>,
    incoming_rx: Receiver<Vec<FsEvent>>,
    incoming_tx: Sender<Vec<FsEvent>>,
    control_rx: Receiver<Control>,
    control_tx: ControlSender,
    applier_stop: Arc<AtomicBool>,
    watcher_stop: Arc<AtomicBool>,
    watcher_thread: Option<std::thread::JoinHandle<()>>,
    /// Shared handle to the accept-loop supervisor (§15 deaf-daemon watchdog).
    /// Installed by `main` after it builds the supervisor — `None` in tests
    /// and harnesses that never serve the socket. When present, the watcher
    /// ticks it ~20×/s alongside its poll loop, so *either* surviving thread
    /// (applier via `main`, watcher from here) revives a dead listener.
    accept_supervisor: Option<crate::net::SharedSupervisor>,
    /// Last time the mount watchdog ran (ms since epoch).
    last_mount_check_ms: u64,
    /// Kept until `start()` moves it into the watcher thread.
    watch_rx: Option<Receiver<(u32, PathBuf)>>,
}

impl Daemon {
    /// Build a daemon from a config. Loads the snapshot + replays the journal.
    pub fn build(config: Config) -> anyhow::Result<Daemon> {
        let mut store = Store::open(&config.state_dir)?;
        let this_os = fflocate::persist::current_os();

        // Store gates, in order (§15): version first — a store from a newer
        // format refuses to start rather than recrawling over data it cannot
        // read — then ownership (a foreign store needs --shared-store).
        let stamp = Store::read_stamp(&config.state_dir);
        let snap_version = Store::peek_snapshot_version(&config.state_dir)?;
        let handoff = match fflocate::persist::check_gate(
            stamp.as_ref(),
            snap_version,
            this_os,
            config.shared_store,
        ) {
            fflocate::persist::OpenGate::Open { handoff } => {
                if handoff {
                    eprintln!(
                        "fflocated: store last written by '{}'; shared mode, adopting (generation bump, local roots rescanned)",
                        stamp.as_ref().map(|s| s.last_writer_os.as_str()).unwrap_or("?"),
                    );
                }
                handoff
            }
            fflocate::persist::OpenGate::Refuse(msg) => {
                anyhow::bail!("{msg}");
            }
        };

        let loaded = store.load_snapshot()?;
        let had_snapshot = loaded.is_some();
        let (mut index, mut roots) = match loaded {
            Some(l) => (l.index, l.roots),
            None => (Index::new(), Vec::new()),
        };

        // Merge configured roots into the roots table. New records are owned
        // by this OS; stored records keep their recorded owner (foreign roots
        // stay visible but are never touched here).
        for p in &config.roots {
            if !roots.iter().any(|r| r.path == *p) {
                // max-plus-one, not len: pruned roots below leave holes, and
                // ids must never be reused within a table.
                let id = roots.iter().map(|r| r.id + 1).max().unwrap_or(0);
                let mut root = Root::new(id, p.clone());
                root.owner_os = Some(this_os.to_string());
                root.caps = mount::probe_root(p);
                roots.push(root);
            }
        }
        // Drop stored roots a configured root now covers (same filesystem,
        // strictly above): without this yesterday's `$HOME` root would
        // double-index everything today's `/home` filesystem root covers.
        // Records and subtrees go; the covering crawl re-adds the entries.
        {
            let configured: Vec<(String, u64)> = config
                .roots
                .iter()
                .map(|p| {
                    (
                        p.to_string_lossy().into_owned(),
                        mount::probe_dev(p).unwrap_or(0),
                    )
                })
                .collect();
            let stored: Vec<(String, u64)> = roots
                .iter()
                .filter(|r| !config.roots.iter().any(|c| c == &r.path))
                .map(|r| (r.key(), mount::probe_dev(&r.path).unwrap_or(0)))
                .collect();
            for gone in covered_root_paths(&stored, &configured) {
                if let Some(pos) = roots.iter().position(|r| r.key() == gone) {
                    let root = roots.remove(pos);
                    let rid = fflocate::applier::root_dir_id(&index, &root);
                    if rid != ROOT_ID {
                        let mut ids = vec![rid];
                        ids.extend(fflocate::applier::descendants(&index, rid));
                        for id in ids.into_iter().rev() {
                            index.remove(id);
                        }
                    }
                    eprintln!(
                        "fflocated: root {} is covered by a configured root on the same filesystem; dropping its redundant entries",
                        root.key()
                    );
                }
            }
        }
        // Assign each root a root-dir entry in the index (name = full path,
        // which cannot collide across roots). A legacy store may already hold
        // a basename entry for the root: `root_dir_id` resolves either, and
        // only a root with *neither* gets a fresh entry — the legacy one is
        // adopted, never duplicated.
        for root in roots.iter_mut() {
            if fflocate::applier::root_dir_id(&index, root) == ROOT_ID {
                index.insert(
                    ROOT_ID,
                    &fflocate::roots::root_entry_name(&root.path),
                    EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
                );
            }
            root.caps = mount::probe_root(&root.path);
            root.dev = mount::probe_dev(&root.path).unwrap_or(0);
            root.drive = fflocate::drive::probe_drive(&root.path);
        }

        // §9 background replay: decode the journal tail *without* applying
        // it. The daemon serves the snapshot immediately below; the applier
        // drains the tail in bounded chunks on its tick (see `drain`), so a
        // large tail no longer blocks startup or the first query. Records
        // move out of the decode — no cloned Vec. A torn tail stops at the
        // first bad record exactly as before (`corrupt_tail`); the applied
        // prefix is replayed from the snapshot, and the suffix is healed by
        // the JournalCorrupt rescans queued below.
        let snapshot_lsn = index.last_lsn();
        let (tail, replay) = store.read_tail(snapshot_lsn)?;
        let tail_len = tail.len() as u64;

        // Cross-OS handoff: old cursors must not silently apply to a new
        // writer's history — bump the generation (§9) and re-reconcile.
        if handoff {
            index.bump_generation();
        }

        // No build-time compaction anymore: it renumbered ids while the
        // state was still local, but background replay needs those ids
        // stable across ticks (pending work, watch requests, and the tail
        // being applied all name them). Instead the tail's record count is
        // stashed as compaction credit on the applier, which folds it into
        // `records_since_compaction` when the last replay chunk drains —
        // one deferred compaction via the normal `maybe_compact` path, after
        // the tail is live and serving. Skipped when the tail is suspect
        // (corrupt → rescan first) or a handoff queued rescans (compact
        // after they drain, same path).
        let defer_compaction =
            tail_len >= config.compaction_threshold && !replay.corrupt_tail && !handoff;

        let shared = Shared::new(index, roots);
        {
            let mut h = shared.health.write().unwrap();
            h.journal_corrupt = replay.corrupt_tail;
            // `catching_up` stays true while replay remains OR the initial
            // crawl hasn't drained — `tick` clears it only when the tail is
            // empty, the pending queue is drained, and the queue was
            // non-trivially exercised (see `tick`). Honest from the first
            // served query: snapshot answers carry the flag until the tail
            // lands, and `ff --sync` is the settle barrier.
            h.catching_up = tail_len > 0 || !had_snapshot || replay.corrupt_tail || handoff;
        }

        let sync = SyncRegistry::new();
        let fs: Arc<dyn FsView> = Arc::new(RealFs);
        let mut applier = Applier::new(shared.clone(), store, fs, sync.clone());
        applier.compaction_threshold = config.compaction_threshold;
        // §4/§6: exclusions must be installed before the initial crawl, so an
        // excluded subtree is never enumerated (and, for a whole-mount backend
        // that cannot decline the watch, never inserted either).
        if !config.excludes.is_empty() {
            applier.set_excludes(config.excludes.clone());
        }
        // §4: mount boundaries for the mount-aware crawl, from this boot's
        // mount table (each filesystem indexed through its own root).
        applier.set_mountpoints(mount::mountpoint_set(&mount::read_mountinfo()));
        // The decoded tail becomes the applier's replay queue (moved, not
        // cloned). The daemon serves the snapshot from here on; the tail
        // drains in bounded chunks on the tick. Credit folds into the
        // compaction counter when the last chunk lands (see `drain`).
        applier.set_pending_replay(tail, if defer_compaction { tail_len } else { 0 });
        let (backend, resolved_backend) = make_backend(config.backend)?;

        // Reconcile queued at open: swapped drives re-verified, a torn
        // journal tail re-verified, and on a cross-OS handoff every local
        // root is re-reconciled unconditionally (a crawl only adds;
        // deletions need the diff). All three wait behind the replay tail:
        // `drain` applies replay chunks before popping any pending work, so
        // these diffs always see post-replay ids (no duplicates) — and a
        // deferred compaction (credit above) only fires via `maybe_compact`
        // after the tail drains, when its id remap can fix the live tables.
        for rid in drive_changed_rids(
            &stamp.as_ref().map(|s| s.drives.clone()).unwrap_or_default(),
            &shared.roots.read().unwrap(),
            &shared.index.read().unwrap(),
        ) {
            applier
                .pending_mut()
                .enqueue_rescan(rid, RescanCause::MountEvent);
        }
        if handoff {
            let ix = shared.index.read().unwrap();
            let roots = shared.roots.read().unwrap();
            for r in roots.iter().filter(|r| r.is_local()) {
                let rid = fflocate::applier::root_dir_id(&ix, r);
                if rid != ROOT_ID {
                    applier
                        .pending_mut()
                        .enqueue_rescan(rid, RescanCause::OsHandoff);
                }
            }
        }
        // A torn tail stops at the first bad record by construction
        // (`Journal::replay` never resyncs past one), so everything after it
        // is unreachable — including valid records. The applied prefix is
        // already converging via replay; the suffix heals by diff: enqueue a
        // rescan of every local root, automatically. They queue behind the
        // replay tail (never ahead of it), carry the honest cause, and are
        // never dropped — `catching_up` stays true until they drain too.
        if replay.corrupt_tail {
            let ix = shared.index.read().unwrap();
            let roots = shared.roots.read().unwrap();
            for r in roots.iter().filter(|r| r.is_local()) {
                let rid = fflocate::applier::root_dir_id(&ix, r);
                if rid != ROOT_ID {
                    applier
                        .pending_mut()
                        .enqueue_rescan(rid, RescanCause::JournalCorrupt);
                }
            }
        }

        // Record this boot: ownership for new records, fresh drive probes
        // where available (never clobbering with unknowns), us as writer.
        {
            let roots = shared.roots.read().unwrap();
            let created_by_os = stamp
                .as_ref()
                .map(|s| s.created_by_os.clone())
                .unwrap_or_else(|| this_os.to_string());
            let mut drives = stamp.map(|s| s.drives).unwrap_or_default();
            for r in roots.iter() {
                let entry = drives.entry(r.key()).or_default();
                if r.drive.is_some() {
                    entry.drive = r.drive.clone();
                }
                if entry.owner_os.is_none() {
                    entry.owner_os = r.owner_os.clone();
                }
            }
            let next = fflocate::persist::StoreStamp {
                format_version: fflocate::config_paths::FORMAT_VERSION,
                created_by_os,
                last_writer_os: this_os.to_string(),
                backend: resolved_backend.as_str().to_string(),
                shared: config.shared_store,
                drives,
            };
            Store::write_stamp(&config.state_dir, &next)?;
        }

        let (incoming_tx, incoming_rx) = bounded::<Vec<FsEvent>>(1000);
        let (control_tx, control_rx) = bounded::<Control>(64);
        let (watch_tx, watch_rx) = bounded::<(u32, PathBuf)>(4096);
        let (watch_fail_tx, watch_fail_rx) = bounded::<WatchFailure>(1024);

        Ok(Daemon {
            shared,
            config,
            sync,
            applier,
            backend,
            resolved_backend,
            watched_dirs: std::collections::HashSet::new(),
            watch_tx,
            watch_fail_tx,
            watch_fail_rx,
            incoming_rx,
            incoming_tx,
            control_rx,
            control_tx,
            applier_stop: Arc::new(AtomicBool::new(false)),
            watcher_stop: Arc::new(AtomicBool::new(false)),
            watcher_thread: None,
            accept_supervisor: None,
            last_mount_check_ms: 0,
            watch_rx: Some(watch_rx),
        })
    }

    /// Warm the trigram accelerator on its own thread, once the index has settled.
    ///
    /// ## Why a thread, and why the waiting
    ///
    /// The accelerator is built lazily on first use and costs ~0.7 s on a 325 k
    /// entry index (1.9 s at 1 M). Left alone, that lands on whoever issues the
    /// first query — the worst possible moment, and in a GUI it looks like the
    /// search box has hung.
    ///
    /// It cannot simply be built at startup, because the initial crawl is still
    /// mutating the index then: the cache is keyed by generation, and anything
    /// built mid-crawl is invalidated by the next batch of inserts. Nor can it hang
    /// off the applier tick: on a large root `Applier::drain` does not return until
    /// the crawl has finished, so the tick is not a place that runs promptly.
    ///
    /// So: a thread that polls the index for a stable shape and then builds. It
    /// never blocks startup, never blocks the applier, and if it gives up the query
    /// path still builds on demand — warming is an optimisation, never a
    /// prerequisite.
    fn spawn_trigram_warmer(&self) {
        #[cfg(feature = "trigram")]
        {
            let shared = self.shared.clone();
            std::thread::Builder::new()
                .name("trigram-warm".into())
                .spawn(move || {
                    // Wait for the index to stop growing. A rebuild mid-crawl is
                    // wasted, so this is patience rather than readiness.
                    let mut last = (0u64, 0usize);
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
                    loop {
                        let (gen, entries) = {
                            let ix = shared.index.read().unwrap();
                            (ix.generation(), ix.entry_count())
                        };
                        if (gen, entries) == last && entries > 1 {
                            break; // stable for a whole poll interval
                        }
                        last = (gen, entries);
                        if std::time::Instant::now() >= deadline {
                            break; // warm anyway; correctness does not depend on it
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    let t = std::time::Instant::now();
                    let tg = shared.trigram_index();
                    eprintln!(
                        "fflocated: trigram index warmed in {:.0} ms ({} trigrams); \
                         the first search will not pay for it",
                        t.elapsed().as_secs_f64() * 1000.0,
                        tg.read().unwrap().trigram_count()
                    );
                })
                .ok();
        }
        #[cfg(not(feature = "trigram"))]
        {
            // Nothing to warm; the query path is a plain scan.
        }
    }

    /// Install the shared accept-loop supervisor (§15). Called by `main` once
    /// it builds the supervisor around the startup listener; the watcher
    /// thread picks it up in `start` and ticks it alongside its poll loop.
    pub fn set_accept_supervisor(&mut self, sup: crate::net::SharedSupervisor) {
        self.accept_supervisor = Some(sup);
    }

    /// Install watches on the root dirs and start the watcher thread.
    pub fn start(&mut self) -> anyhow::Result<()> {
        self.applier.rebuild_dir_cache();

        // Warm the trigram accelerator off the user's critical path (§10, §17).
        // The thread waits for the index to stop growing before building, because a
        // build mid-crawl is invalidated by the next insert; see the method for why
        // neither startup nor the applier tick can host it directly.
        self.spawn_trigram_warmer();

        // Watch the roots first, then enumerate (§5).
        let roots: Vec<(u32, PathBuf)> = {
            let ix = self.shared.index.read().unwrap();
            let roots = self.shared.roots.read().unwrap();
            roots
                .iter()
                .filter_map(|r| {
                    let rid = fflocate::applier::root_dir_id(&ix, r);
                    (rid != ROOT_ID).then(|| (rid, r.path.clone()))
                })
                .collect()
        };
        for (id, path) in roots {
            self.watch_dir(id, &path);
            self.applier.pending_mut().enqueue_crawl(id);
        }

        // Watcher thread: poll the backend and forward events, and service
        // watch requests for directories discovered during the crawl.
        let stop = self.watcher_stop.clone();
        let tx = self.incoming_tx.clone();
        let backend = self.backend.clone();
        let roots_snapshot = self.shared.roots.read().unwrap().clone();
        let watch_rx = self.watch_rx.take().expect("watch_rx taken twice");
        let fail_tx = self.watch_fail_tx.clone();
        // Mutual supervision (§15): the watcher ticks the accept supervisor
        // alongside its poll loop, so a dead listener is revived even when
        // the applier is wedged in a minutes-long `diff_subtree`. The tick
        // only takes the supervisor lock for a liveness check on the fast
        // path — a re-bind spans ticks, never holds the lock across one.
        let accept_sup = self.accept_supervisor.clone();
        self.watcher_thread = Some(std::thread::spawn(move || {
            // Install every pending watch request, reporting failures. This is
            // the watcher's consume step for the watch channel, and it is
            // called from every wait below — that is what breaks the
            // channel cycle (see the loop comment).
            let install_pending = |backend: &SharedBackend| -> bool {
                let mut busy = false;
                while let Ok((id, path)) = watch_rx.try_recv() {
                    busy = true;
                    let mut b = backend.lock().unwrap();
                    if let Err(e) = b.watch_dir(id, &path) {
                        // A failure is *never* silent (§7, §15): it is
                        // reported so the applier can degrade the root
                        // loudly. But the report itself must never block:
                        // the applier drains `watch_fail_rx` only at the top
                        // of its tick, so a blocking send on a saturated
                        // report channel would park the watcher, stop the
                        // watch-request servicing, and re-create the exact
                        // deadlock this loop exists to prevent. Saturated =
                        // drop loudly to the daemon log; the first 1024
                        // reports have already degraded every root in
                        // trouble.
                        let f = WatchFailure { dir: id, reason: e.to_string() };
                        if fail_tx.try_send(f).is_err() {
                            eprintln!(
                                "fflocated: watch failure report dropped (report channel saturated): dir {id}: {e}"
                            );
                        }
                    }
                }
                busy
            };

            {
                let mut b = backend.lock().unwrap();
                for r in &roots_snapshot {
                    let _ = b.add_root(r);
                }
            }
            // The watcher never blocks indefinitely on `tx.send`.
            //
            // Both channels here are bounded, and each thread's blocking
            // send is exactly the other's consume step — a textbook ABBA
            // cycle, observed live on a 1.4M-entry index (both threads
            // futex-parked forever, stacks recovered from /proc):
            //
            //   applier:  one long work item (a full-root `diff_subtree`;
            //             DRAIN_TIME_BUDGET paces *between* items, not
            //             within one) goes minutes without draining
            //             `incoming_rx`; a live event flood fills the
            //             1000-batch event channel and the watcher parks in
            //             its send — and while parked stops servicing watch
            //             requests. The applier then finishes its item
            //             holding one watch request per surviving directory
            //             (far more than the 4096-channel after a
            //             full-tree pass) and blocks in `watch_dir`'s
            //             `watch_tx.send` — never returning to the top of
            //             its tick to drain `incoming_rx`. Neither thread
            //             ever reaches its consume step again.
            //
            // The cycle breaks here, in the watcher: every event-batch send
            // carries a deadline, watch requests are serviced between
            // retries, and the stop flag is re-checked on every retry. The
            // applier's `watch_dir` send is therefore parked for at most
            // ~one retry interval, always returns to the top of its tick,
            // and always drains `incoming_rx`. Neither events nor watch
            // requests are ever dropped; shutdown stays prompt.
            while !stop.load(Ordering::Relaxed) {
                let mut busy = install_pending(&backend);
                let evs = {
                    let mut b = backend.lock().unwrap();
                    b.poll(Duration::from_millis(50)).unwrap_or_default()
                };
                if !evs.is_empty() {
                    busy = true;
                    let mut batch = Some(evs);
                    while let Some(evs) = batch {
                        if stop.load(Ordering::Relaxed) {
                            // Shutdown: drop the in-flight batch. Events not
                            // yet applied are lost on shutdown by
                            // construction today (the applier stops ticking
                            // first); §7 rescans heal the gap.
                            break;
                        }
                        busy |= install_pending(&backend);
                        match tx.send_timeout(evs, Duration::from_millis(50)) {
                            Ok(()) => batch = None,
                            Err(crossbeam_channel::SendTimeoutError::Timeout(evs)) => {
                                batch = Some(evs);
                            }
                            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                                // The applier side is gone: the daemon is
                                // stopping. Nothing further to deliver.
                                return;
                            }
                        }
                    }
                }
                // Drain watch requests again promptly so a new dir is watched
                // before its contents change.
                busy |= install_pending(&backend);
                // Mutual supervision (§15): a dead accept loop is revived
                // from here as well as from the applier — whichever thread
                // is still alive recovers service. Ticked every pass
                // (~20x/s idle), so a dead listener is noticed within ~50
                // ms even when the applier is wedged in a long drain. The
                // tick holds the supervisor lock, but never blocks
                // indefinitely under it: the common case is one liveness
                // check, a reap joins an already-dead thread, and a re-bind
                // is fast syscalls plus a spawn (the 1 s backoff spans
                // ticks, never sleeps under the lock). A poisoned mutex
                // (the applier panicked mid-tick) skips supervision rather
                // than panicking the watcher too.
                if let Some(sup) = accept_sup.as_ref() {
                    if let Ok(mut s) = sup.lock() {
                        s.tick();
                    }
                }
                // Kernel backends block in `poll` for up to the timeout, but
                // the fake backend drains a queue and returns instantly — so
                // on platforms without a kernel watcher (macOS/Windows) this
                // loop would spin a core at 100%. Sleep when idle, matching
                // the poll budget; the stop flag still lands within ~50 ms.
                if !busy {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }));
        Ok(())
    }

    fn watch_dir(&mut self, id: u32, path: &Path) {
        if self.watched_dirs.insert(id) {
            let _ = self.watch_tx.send((id, path.to_path_buf()));
        }
    }

    /// The applier loop. Blocks until `stop()` is called.
    pub fn run(&mut self) -> anyhow::Result<()> {
        while !self.applier_stop.load(Ordering::Relaxed) {
            let idle = self.tick();
            if idle {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }

    /// One applier pass. Returns true when there was nothing to do (idle).
    pub fn tick(&mut self) -> bool {
        let mut work_done = false;

        // Handle control requests *first*: a cookie must be registered before we
        // drain the event that announces it, or the marker is missed.
        while let Ok(ctl) = self.control_rx.try_recv() {
            match ctl {
                Control::Cookie { token, root } => {
                    self.applier.register_cookie_existing(token, root);
                }
                Control::RescanRoot { root } => {
                    self.applier
                        .pending_mut()
                        .enqueue_rescan(root, RescanCause::Manual);
                }
            }
            work_done = true;
        }

        while let Ok(batch) = self.incoming_rx.try_recv() {
            for ev in batch {
                self.applier.push(ev);
            }
            work_done = true;
        }

        // Watch failures degrade the owning root loudly (§7, §15): queries keep
        // answering from what is known, and every response meta lists the root.
        while let Ok(f) = self.watch_fail_rx.try_recv() {
            self.applier.note_watch_failure(f.dir, &f.reason);
            work_done = true;
        }

        // Mount watchdog. `IN_UNMOUNT` is not delivered on every platform (it is
        // absent under WSL2), so we detect a mount change portably: compare each
        // root's device id against the probed value. A change means a remount or
        // a different filesystem on the same point → rescan (cause=mount_event).
        work_done |= self.mount_watchdog();

        let applied = self.applier.drain();
        work_done |= applied > 0;
        // Install watches for directories discovered during the crawl. The
        // applier records them via `take_watch_requests`.
        for (id, path) in self.applier.take_watch_requests() {
            self.watch_dir(id, &path);
        }

        // Lag behind the filesystem: the arrival time of the oldest
        // still-unapplied change. Mirrored raw (wall ms) so `Meta.lag_ms` can
        // age it at query time rather than reporting a stale age; `0` is the
        // "caught up" sentinel.
        //
        // Journal-replay backlog counts as unapplied change by definition, so
        // a still-draining tail contributes its replay start as a lag floor —
        // otherwise a replaying daemon would report "caught up" while it was
        // demonstrably behind.
        //
        // This is mirrored BEFORE the `catching_up` clear below, because that
        // clear is gated on the lag: a daemon whose queues drained and whose
        // tail finished is the only thing that may claim it is caught up.
        let oldest = match (
            self.applier.pending().oldest_queued_ms(),
            self.applier.pending_replay_started_ms(),
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
        .unwrap_or(0);
        {
            let mut h = self.shared.health.write().unwrap();
            h.oldest_queued_at_ms = oldest;
            h.queue_depth =
                self.applier.pending().depth() as u64 + self.applier.pending_replay_len() as u64;
            h.rescans_collapsed = self.applier.pending().rescans_collapsed();

            // What the daemon is doing, in its own words. Derived from what is
            // actually outstanding — the replay tail, the crawl queue, the
            // rescan queue — so the phase cannot disagree with the work the
            // applier is really doing. `crawl_entries` is the live index count:
            // the honest crawl-progress numerator, since a streaming crawl has
            // no known total up front (it *discovers* the tree).
            let replay = self.applier.pending_replay_len();
            let crawl_dirs = self.applier.pending().crawl_queued_len();
            let rescans = self.applier.pending().rescans_queued_len();
            h.phase = fflocate::health::classify_phase(replay, crawl_dirs, rescans);
            h.crawl_entries = self.shared.index.read().unwrap().live_count();
            h.crawl_dirs_pending = crawl_dirs as u64;
            h.rescans_pending = rescans as u64;

            // Clear `catching_up` once the replay tail has drained, the
            // initial crawl has caught up, and the queue drains. All three,
            // not just the queue: the tail is crash-recovery history applied
            // in chunks (a drained queue with an unapplied tail is still
            // stale), and the initial crawl's work items only exist after
            // `start` enqueues them.
            //
            // The lag is the arbiter rather than a second opinion: if anything
            // is still unapplied (`oldest != 0`), the daemon IS behind and must
            // keep saying so. Reading the same value that `Meta.lag_ms` serves
            // means the flag and the seconds can never disagree — which is
            // exactly how a daemon with a 51k-deep backlog used to report
            // "caught up" the moment its two queues briefly emptied between
            // drain chunks.
            if oldest == 0 {
                h.catching_up = false;
            }
        }
        self.applier.maybe_compact();
        !work_done
    }

    /// Process pending items once (used by tests and by `run`).
    pub fn drain_once(&mut self) -> usize {
        self.applier.drain()
    }

    /// Periodically compare each root's device id with the probed value. A
    /// change (or a vanished root path) means the mount was replaced underneath
    /// us. This is the portable mount-event detection, because `IN_UNMOUNT` is
    /// not delivered on every platform. Returns true if it did anything.
    fn mount_watchdog(&mut self) -> bool {
        let now = fflocate::applier::now_ms();
        if now.saturating_sub(self.last_mount_check_ms) < 1_000 {
            return false;
        }
        self.last_mount_check_ms = now;
        // Keep the crawl's mount-boundary set current: mounts come and go
        // while a crawl is in flight, and a stale set errs safe in both
        // directions (a missed new mount is healed by the next rescan, a
        // stale entry merely skips an already-gone path).
        self.applier
            .set_mountpoints(mount::mountpoint_set(&mount::read_mountinfo()));
        let mut changed: Vec<(u32, u64, Option<u64>, String)> = Vec::new();
        {
            let ix = self.shared.index.read().unwrap();
            let roots = self.shared.roots.read().unwrap();
            for r in roots.iter() {
                if !r.enabled {
                    continue;
                }
                let rid = fflocate::applier::root_dir_id(&ix, r);
                let cur = mount::probe_dev(&r.path);
                // A vanished path alarms even with no baseline dev: on
                // platforms without device IDs (Windows) this existence
                // check is the *only* root-gone signal; elsewhere it agrees
                // with the dev comparison (a gone root probes `None`).
                let gone = cur.is_none() && !r.path.exists();
                if (cur != Some(r.dev) && r.dev != 0) || gone {
                    changed.push((rid, r.dev, cur, r.path.to_string_lossy().into_owned()));
                }
            }
        }
        if changed.is_empty() {
            return false;
        }
        for (rid, old_dev, cur, path) in changed {
            self.shared.health.write().unwrap().mount_events += 1;
            if cur.is_none() {
                // The root is gone entirely: drop it and mark it degraded.
                if rid != ROOT_ID {
                    self.applier.pending_mut().enqueue_drop(rid);
                }
                self.applier.note_mount_gone(rid, &path);
            } else {
                // A different filesystem is now mounted: rescan the root.
                if rid != ROOT_ID {
                    self.applier
                        .pending_mut()
                        .enqueue_rescan(rid, RescanCause::MountEvent);
                }
                self.applier
                    .note_mount_remounted(rid, old_dev, cur.unwrap_or(0));
            }
        }
        true
    }

    /// Wait until the pending queue is empty and the initial crawl reports
    /// caught-up, or `timeout` elapses. Returns true when quiescent.
    pub fn settle(&mut self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            self.tick();
            if self.applier.pending().is_drained()
                && !self.shared.health.read().unwrap().catching_up
            {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn control_sender(&self) -> ControlSender {
        self.control_tx.clone()
    }

    pub fn server_ctx(&self) -> ServerCtx {
        ServerCtx {
            shared: self.shared.clone(),
            sync: self.sync.clone(),
            backend: self.resolved_backend.as_str().to_string(),
            control: Some(self.control_tx.clone()),
            // Per-user daemon: no ownership filter. The system daemon builds a
            // context per connection instead, with the caller from SO_PEERCRED.
            caller: None,
        }
    }

    /// A context for a **system** instance, where every query is answered as the
    /// connecting user (§3).
    pub fn server_ctx_for(&self, caller: Option<fflocate::ownership::Caller>) -> ServerCtx {
        let mut ctx = self.server_ctx();
        ctx.caller = caller;
        ctx
    }

    /// Is this the privileged system instance?
    pub fn is_system(&self) -> bool {
        self.config.system
    }

    /// The backend name in use (after `auto` resolution).
    pub fn backend_name(&self) -> String {
        self.resolved_backend.as_str().to_string()
    }

    /// Turn on per-entry ownership capture. Separate from `build` so the cost is
    /// visible at the call site rather than buried: this is the `lstat` per file.
    pub fn enable_ownership_tracking(&mut self) {
        self.applier.track_ownership(true);
    }

    pub fn applier_mut(&mut self) -> &mut Applier {
        &mut self.applier
    }

    pub fn stop(&mut self) {
        self.applier_stop.store(true, Ordering::Relaxed);
        self.watcher_stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.watcher_thread.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fflocate::drive::DriveId;
    use fflocate::persist::RecordedRoot;

    fn root_with(id: u32, path: &str, drive: Option<DriveId>, owner: Option<&str>) -> Root {
        let mut r = Root::new(id, PathBuf::from(path));
        r.drive = drive;
        r.owner_os = owner.map(str::to_string);
        r
    }

    fn index_with_root_entry(name: &[u8]) -> Index {
        let mut ix = Index::new();
        ix.insert(
            ROOT_ID,
            name,
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        ix
    }

    fn disk(serial: &str) -> DriveId {
        DriveId {
            disk_serial: Some(serial.to_string()),
            volume_uuid: None,
        }
    }

    #[test]
    fn drive_change_needs_agreeing_mismatch() {
        let ix = index_with_root_entry(b"data");
        // `drive_changed_rids` only considers local roots (`is_local` checks
        // the host OS), so the test roots must be owned by *this* OS — a
        // hardcoded "linux" reads as foreign on macOS/Windows and the first
        // assertion fails there.
        let this_os = std::env::consts::OS;
        let mk = |drive: Option<DriveId>| root_with(0, "/mnt/data", drive, Some(this_os));
        let rec = |drive: Option<DriveId>| RecordedRoot {
            drive,
            owner_os: Some(this_os.into()),
        };

        // Recorded A, probes B → rescan.
        let mut map = BTreeMap::new();
        map.insert("/mnt/data".to_string(), rec(Some(disk("A"))));
        let roots = vec![mk(Some(disk("B")))];
        assert_eq!(drive_changed_rids(&map, &roots, &ix).len(), 1);

        // Agreeing (case-insensitively) → quiet.
        let roots = vec![mk(Some(disk("a")))];
        assert!(drive_changed_rids(&map, &roots, &ix).is_empty());

        // Either side unknown → cannot tell, never "changed".
        let roots = vec![mk(None)];
        assert!(drive_changed_rids(&map, &roots, &ix).is_empty());
        let map_empty: BTreeMap<String, RecordedRoot> = BTreeMap::new();
        let roots = vec![mk(Some(disk("B")))];
        assert!(drive_changed_rids(&map_empty, &roots, &ix).is_empty());

        // Foreign roots are never touched, even on mismatch: pick an OS that
        // is not this one, so the test is foreign on every platform.
        let other_os = if this_os == "windows" {
            "linux"
        } else {
            "windows"
        };
        let mut foreign = mk(Some(disk("B")));
        foreign.owner_os = Some(other_os.into());
        let roots = vec![foreign];
        assert!(drive_changed_rids(&map, &roots, &ix).is_empty());

        // Legacy (unowned) roots behave like local ones.
        let mut legacy = mk(Some(disk("B")));
        legacy.owner_os = None;
        let roots = vec![legacy];
        assert_eq!(drive_changed_rids(&map, &roots, &ix).len(), 1);
    }

    #[test]
    fn ancestor_matching_is_component_wise() {
        assert!(is_ancestor_path("/home", "/home/me"));
        assert!(is_ancestor_path("/", "/home/me"));
        assert!(
            !is_ancestor_path("/", "/"),
            "strict even for the filesystem root"
        );
        assert!(
            !is_ancestor_path("/home", "/home"),
            "strict: a path does not cover itself"
        );
        assert!(
            !is_ancestor_path("/home", "/home2"),
            "prefix is not ancestry"
        );
        assert!(
            !is_ancestor_path("/home/me", "/home"),
            "a child never covers its parent"
        );
    }

    #[test]
    fn covered_roots_need_same_fs_and_strict_nesting() {
        let stored = vec![
            ("/home/me".to_string(), 11u64),  // yesterday's $HOME root
            ("/mnt/data".to_string(), 22u64), // its own filesystem
            ("/gone".to_string(), 0u64),      // unprobed: unknowable
        ];
        // Configured `/home` (same fs as the $HOME root) covers it; `/`
        // (another fs) covers nothing here.
        let configured = vec![("/home".to_string(), 11u64), ("/".to_string(), 99u64)];
        assert_eq!(
            covered_root_paths(&stored, &configured),
            vec!["/home/me".to_string()],
            "only provable same-filesystem nesting prunes"
        );
        // Same paths, unknown devices: nothing is provable, nothing prunes.
        let stored_unknown = vec![("/home/me".to_string(), 0u64)];
        assert!(covered_root_paths(&stored_unknown, &configured).is_empty());
        // Self-coverage is not coverage (dedup by path happens at merge).
        let self_cover = vec![("/home".to_string(), 11u64)];
        assert!(covered_root_paths(&self_cover, &configured).is_empty());
        // Trailing-slash spellings still match.
        let slashed = vec![("/home/me/".to_string(), 11u64)];
        let cfg_slashed = vec![("/home/".to_string(), 11u64)];
        assert_eq!(
            covered_root_paths(&slashed, &cfg_slashed),
            vec!["/home/me".to_string()]
        );
    }

    /// Regression for the watcher<->applier channel deadlock (see the watcher
    /// loop in `start`): with the event channel saturated *and* events still
    /// flowing, the applier must still be able to flush more watch requests
    /// than the watch channel holds -- i.e. the watcher must keep servicing
    /// watch requests while its event send is parked. Before the fix the
    /// watcher blocked in a plain `send` here, the applier blocked in
    /// `watch_dir`'s send, and neither thread ever reached its consume step
    /// again (observed live: both threads in futex waits, index frozen).
    ///
    /// The scenario runs on a worker thread with a watchdog: a hang here is
    /// the deadlock, and the watchdog turns it into a test failure instead.
    #[test]
    fn watcher_services_watch_requests_while_events_back_up() {
        use crate::config::{BackendChoice, Config};
        use fflocate::watch::fake::FakeBackend;

        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("tree");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = Config {
            socket_path: d.path().join("fflocate.sock"),
            pidfile: d.path().join("fflocate.pid"),
            state_dir: d.path().join("state"),
            roots: vec![root.clone()],
            backend: BackendChoice::Fake,
            compaction_threshold: 1_000_000,
            sync_timeout_ms: 500,
            excludes: fflocate::excludes::Excludes::new(),
            system: false,
            system_group: 0,
            shared_store: false,
        };
        let mut daemon = Daemon::build(cfg).unwrap();
        daemon.start().unwrap();

        // The fake backend's injection handle: upcast the boxed trait object
        // to `dyn Any` and downcast back to the concrete fake, so the test
        // can keep events flowing through a backend the daemon built
        // internally.
        let handle = {
            let b = daemon.backend.lock().unwrap();
            let backend: &dyn WatchBackend = b.as_ref();
            let any: &dyn std::any::Any = backend;
            any.downcast_ref::<FakeBackend>()
                .expect("test config asks for the fake backend")
                .handle()
        };

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let injecting = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let injector_handle = handle.clone();
        let injector_flag = injecting.clone();
        // Keep events flowing for the whole scenario, like a live filesystem
        // under load: every watcher poll must find a non-empty queue, so the
        // parked-send condition persists (one static batch is not enough —
        // the fake backend drains its queue in a single poll, and the
        // watcher's pre-poll servicing would let the flush slip through).
        let injector = std::thread::spawn(move || {
            let mut i = 0u32;
            while injector_flag.load(Ordering::Relaxed) {
                injector_handle.inject(fflocate::event::FsEvent::upsert(
                    fflocate::event::DirKey::new(900_000 + (i % 64)),
                    format!("filler_{i}").into_bytes(),
                    false,
                ));
                i += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let scenario = std::thread::spawn(move || {
            // 1. Saturate the event channel, as a long applier work item
            //    would (nothing drains it until the tick at the end). The
            //    loop's exit on `Full` is the saturation guarantee — the
            //    exact count is not asserted, because the watcher may have
            //    delivered batches of its own in the meantime.
            let mut filled = 0usize;
            loop {
                match daemon.incoming_tx.try_send(Vec::new()) {
                    Ok(()) => filled += 1,
                    Err(crossbeam_channel::TrySendError::Full(_)) => break,
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                        panic!("event channel disconnected before saturation")
                    }
                }
            }

            // 1b. Let the watcher reach its parked state: with the channel
            //     full and the injector feeding it, the watcher's next poll
            //     cycle finds a non-empty batch and parks in its event send.
            //     Without this wait the entire scenario can race past
            //     before the watcher wakes from its 50 ms idle sleep, and
            //     the parked-send condition never forms.
            std::thread::sleep(Duration::from_millis(250));

            // 2. Flush more watch requests than the watch channel's capacity.
            //    With the fix these complete (the watcher services between
            //    send retries); without it, this blocks forever at request
            //    #4097 — the watcher is parked sending events into the
            //    saturated channel and never reaches its servicing pass.
            for i in 0..6000u32 {
                daemon.watch_dir(i + 1, &root);
            }

            // 3. The tick must return: it drains the event channel at its
            //    top, which unparks the watcher.
            daemon.tick();
            let _ = done_tx.send(filled);
        });

        let filled = done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("watcher-applier deadlock: the watch flush never completed");
        assert!(filled > 0, "the fill loop should have delivered batches: {filled}");
        injecting.store(false, Ordering::Relaxed);
        injector.join().unwrap();
        // The scenario thread owns the daemon; on success it drops (and so
        // stops) it. Join to surface any panic it hit.
        scenario.join().unwrap();
    }
}

#[cfg(test)]
mod replay_integration_tests {
    use super::*;
    use crate::config::{BackendChoice, Config};

    fn test_config(dir: &std::path::Path, root: PathBuf) -> Config {
        Config {
            socket_path: dir.join("fflocate.sock"),
            pidfile: dir.join("fflocate.pid"),
            state_dir: dir.join("state"),
            roots: vec![root],
            backend: BackendChoice::Fake,
            compaction_threshold: 1_000_000,
            sync_timeout_ms: 500,
            excludes: fflocate::excludes::Excludes::new(),
            system: false,
            system_group: 0,
            shared_store: false,
        }
    }

    /// A torn journal tail heals automatically: `build` serves the snapshot,
    /// marks the store corrupt, and enqueues a JournalCorrupt rescan for
    /// every local root — which drains behind the replay tail and converges
    /// without any manual `rescan`.
    #[test]
    fn corrupt_tail_enqueues_journal_corrupt_rescans() {
        use fflocate::journal::Record;
        use fflocate::persist::Store;

        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("tree");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("keep.txt"), b"x").unwrap();
        // Enough files that the tail is well past one record: the chop
        // below must land mid-record, not truncate the whole journal.
        for i in 0..20 {
            std::fs::write(root.join(format!("f{i:02}.txt")), b"x").unwrap();
        }

        // Phase 1: journal something, then tear the tail mid-record.
        let cfg = test_config(d.path(), root.clone());
        {
            let mut daemon = Daemon::build(cfg).unwrap();
            daemon.start().unwrap();
            daemon.settle(Duration::from_secs(2));
            daemon.stop();
            // Sanity: something was journaled.
            assert!(
                daemon.applier_mut().store().journal.count() > 0,
                "crawl must journal"
            );
        }
        let journal_path = d.path().join("state").join("journal.log");
        let size = std::fs::metadata(&journal_path).unwrap().len();
        assert!(size > 32, "need a non-trivial journal, got {size}");
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&journal_path)
            .unwrap();
        // Chop 37 bytes off: lands mid-record for any realistic tail.
        f.set_len(size - 37).unwrap();
        drop(f);

        // Phase 2: rebuild. Snapshot serves, tail stashed, corrupt flagged,
        // and a JournalCorrupt rescan queued per local root — automatically.
        let cfg = test_config(d.path(), root.clone());
        let mut daemon = Daemon::build(cfg).unwrap();
        assert!(
            daemon.shared.health.read().unwrap().journal_corrupt,
            "torn tail must be reported"
        );
        assert!(
            daemon.shared.health.read().unwrap().catching_up,
            "corrupt tail keeps catching_up until rescans drain"
        );
        // The rescan is queued even though `start` (which enqueues crawls)
        // never ran: build did it.
        let has_rescan = {
            let p = daemon.applier_mut().pending_mut();
            // Drain the queue handle: pop one work item and check its cause.
            // (Pending has no iterator; popping is the honest probe.)
            let mut found = false;
            while let Some(w) = p.pop_work() {
                if matches!(
                    w,
                    fflocate::pending::WorkItem::Rescan {
                        cause: RescanCause::JournalCorrupt,
                        ..
                    }
                ) {
                    found = true;
                    break;
                }
            }
            found
        };
        assert!(has_rescan, "JournalCorrupt rescan must be auto-enqueued");
        daemon.stop();
    }

    /// `read_tail` decodes without applying and never clones through a Vec:
    /// the snapshot is untouched by the decode, and records come back with
    /// their file LSNs in order.
    #[test]
    fn read_tail_decodes_without_applying() {        use fflocate::journal::Record;
        use fflocate::persist::Store;

        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("tree");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"x").unwrap();

        let cfg = test_config(d.path(), root);
        let mut daemon = Daemon::build(cfg).unwrap();
        daemon.start().unwrap();
        daemon.settle(Duration::from_secs(2));
        let journal_count = daemon.applier_mut().store().journal.count();
        assert!(journal_count > 0);
        daemon.stop();

        // Decode the tail against the snapshot LSN (0: no snapshot written
        // yet in this path — the crawl journaled everything).
        let mut store = Store::open(&d.path().join("state")).unwrap();
        let snap_lsn = store
            .load_snapshot()
            .unwrap()
            .map(|l| l.index.last_lsn())
            .unwrap_or(0);
        let (tail, out) = store.read_tail(snap_lsn).unwrap();
        assert!(!out.corrupt_tail, "clean journal must decode clean");
        assert_eq!(tail.len() as u64, out.replayed);
        assert_eq!(tail.len() as u64, journal_count);
        // LSNs strictly ascend from the snapshot high-water.
        for (i, (lsn, _)) in tail.iter().enumerate() {
            assert_eq!(*lsn, snap_lsn + i as u64 + 1, "lsn {i} must be ordered");
        }
        let _ = Record::Fence;
    }

    /// The daemon's own phase, end to end through a real `tick`: a fresh daemon
    /// with an initial crawl enqueued is `Crawling` (with a growing entry
    /// count and a draining dir count); once it settles it is `Idle`. This is
    /// the wiring the GUI's status line depends on — the phase must come from
    /// the queues, not be guessed.
    #[test]
    fn tick_reports_crawling_then_idle() {
        use crate::config::{BackendChoice, Config};
        use fflocate::health::Phase;

        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("tree");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"x").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"x").unwrap();
        let cfg = Config {
            socket_path: d.path().join("fflocate.sock"),
            pidfile: d.path().join("fflocate.pid"),
            state_dir: d.path().join("state"),
            roots: vec![root.clone()],
            backend: BackendChoice::Fake,
            compaction_threshold: 1_000_000,
            sync_timeout_ms: 500,
            excludes: fflocate::excludes::Excludes::new(),
            system: false,
            system_group: 0,
            shared_store: false,
        };
        let mut daemon = Daemon::build(cfg).unwrap();
        daemon.start().unwrap();

        // Before the first tick, Health holds its default (Idle).
        assert_eq!(daemon.shared.health.read().unwrap().phase, Phase::Idle);

        // A tick enqueues/consumes crawl work; with the crawl outstanding the
        // daemon must call itself Crawling and report its progress.
        daemon.tick();
        let h = daemon.shared.health.read().unwrap().clone();
        // It is either still crawling (dirs pending) or has just drained the
        // tiny tree (idle) on this one tick — both are honest. Assert the
        // invariants that must hold either way.
        match h.phase {
            Phase::Crawling | Phase::Idle => {}
            other => panic!("a just-started crawl must read crawling or idle, got {other:?}"),
        }
        // `crawl_entries` is the live index count and is always populated.
        assert_eq!(
            h.crawl_entries,
            daemon.shared.index.read().unwrap().live_count(),
            "crawl_entries must mirror the live index count"
        );

        // Settle fully: the daemon must end up Idle with both queues empty and
        // the counters consistent.
        assert!(daemon.settle(Duration::from_secs(5)), "crawl must settle");
        let h = daemon.shared.health.read().unwrap().clone();
        assert_eq!(h.phase, Phase::Idle, "a settled daemon is idle");
        assert_eq!(h.crawl_dirs_pending, 0);
        assert_eq!(h.rescans_pending, 0);
        assert!(!h.catching_up, "caught up must clear the flag");
        assert!(h.crawl_entries >= 3, "root + sub + two files, got {}", h.crawl_entries);
        daemon.stop();
    }
}
