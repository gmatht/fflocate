//! The findexd daemon (§2, §5–§9, §13, §15).
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
//!   handles a request via `findex::server`.
//!
//! Startup is *load → replay → serve immediately*, with `catching_up: true` in
//! meta until the initial crawl lands (§9/§15). Watch first, then enumerate
//! (§5): watches are installed on every directory before it is crawled.


use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, Sender};

use findex::applier::{Applier, FsView, RealFs, Shared};
use findex::event::{FsEvent, RescanCause};
use findex::index::{EntryFlags, Index, ROOT_ID};
use findex::mount;
use findex::persist::Store;
use findex::journal::Record;
use findex::roots::Root;
use findex::server::{Control, ControlSender, ServerCtx};
use findex::sync::SyncRegistry;
use findex::watch::{FakeBackend, WatchBackend};
#[cfg(target_os = "linux")]
use findex::watch::InotifyBackend;

use crate::config::{BackendChoice, Config};
use std::collections::BTreeMap;

/// Root-dir ids needing a drive-change rescan: a drive was recorded for the
/// path, a drive probes now, and they disagree. Pure for testability; `build`
/// wires it up. Foreign roots are never touched here (their entries belong
/// to another OS), and unknown drives on either side mean "cannot tell",
/// never "changed".
fn drive_changed_rids(
    recorded: &BTreeMap<String, findex::persist::RecordedRoot>,
    roots: &[Root],
    index: &Index,
) -> Vec<u32> {
    let mut out = Vec::new();
    for root in roots.iter().filter(|r| r.is_local()) {
        let rec = recorded.get(&root.key()).and_then(|r| r.drive.clone());
        match (rec, root.drive.clone()) {
            (Some(a), Some(b)) if !a.matches(&b) => {
                let rid = findex::applier::root_dir_id(index, root);
                if rid != ROOT_ID {
                    out.push(rid);
                }
            }
            _ => {}
        }
    }
    out
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
        BackendChoice::Fanotify => match findex::watch::FanotifyBackend::new() {
            Ok(b) => Box::new(b),
            Err(e) => anyhow::bail!(
                "fanotify unavailable: {e} (needs kernel >= 5.13 for FAN_REPORT_DFID_NAME)"
            ),
        },
        #[cfg(not(target_os = "linux"))]
        BackendChoice::Fanotify => {
            anyhow::bail!("backend `fanotify` is Linux-only")
        }
        BackendChoice::Fake => Box::new(FakeBackend::new(findex::watch::fake::FakeHandle::new())),
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
    /// Last time the mount watchdog ran (ms since epoch).
    last_mount_check_ms: u64,
    /// Kept until `start()` moves it into the watcher thread.
    watch_rx: Option<Receiver<(u32, PathBuf)>>,
}

impl Daemon {
    /// Build a daemon from a config. Loads the snapshot + replays the journal.
    pub fn build(config: Config) -> anyhow::Result<Daemon> {
        let mut store = Store::open(&config.state_dir)?;
        let this_os = findex::persist::current_os();

        // Store gates, in order (§15): version first — a store from a newer
        // format refuses to start rather than recrawling over data it cannot
        // read — then ownership (a foreign store needs --shared-store).
        let stamp = Store::read_stamp(&config.state_dir);
        let snap_version = Store::peek_snapshot_version(&config.state_dir)?;
        let handoff = match findex::persist::check_gate(
            stamp.as_ref(),
            snap_version,
            this_os,
            config.shared_store,
        ) {
            findex::persist::OpenGate::Open { handoff } => {
                if handoff {
                    eprintln!(
                        "findexd: store last written by '{}'; shared mode, adopting (generation bump, local roots rescanned)",
                        stamp.as_ref().map(|s| s.last_writer_os.as_str()).unwrap_or("?"),
                    );
                }
                handoff
            }
            findex::persist::OpenGate::Refuse(msg) => {
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
                let id = roots.len() as u32;
                let mut root = Root::new(id, p.clone());
                root.owner_os = Some(this_os.to_string());
                root.caps = mount::probe_root(p);
                roots.push(root);
            }
        }
        // Assign each root a root-dir entry in the index (name = basename,
        // or the drive letter for a Windows drive root, which has none).
        for root in roots.iter_mut() {
            if let Some(base_bytes) = findex::roots::root_entry_name(&root.path) {
                if index.lookup(ROOT_ID, &base_bytes).is_none() {
                    index.insert(
                        ROOT_ID,
                        &base_bytes,
                        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
                    );
                }
            }
            root.caps = mount::probe_root(&root.path);
            root.dev = mount::probe_dev(&root.path).unwrap_or(0);
            root.drive = findex::drive::probe_drive(&root.path);
        }

        // A drive that changed under a known path (swapped removable media,
        // or the other OS's view of a shared drive) must be re-reconciled:
        // entries proved against the old drive are suspect.
        let drive_changed = drive_changed_rids(
            &stamp.as_ref().map(|s| s.drives.clone()).unwrap_or_default(),
            &roots,
            &index,
        );

        // Replay the journal tail. `replay` reports the records; we apply them
        // to the index ourselves (the same `apply_record` the applier uses).
        let mut tail: Vec<Record> = Vec::new();
        let replay = store.replay(&mut index, |_lsn, rec| tail.push(rec.clone()))?;
        for rec in &tail {
            findex::persist::apply_record(&mut index, rec);
        }

        // Cross-OS handoff: old cursors must not silently apply to a new
        // writer's history — bump the generation (§9) and re-reconcile.
        if handoff {
            index.bump_generation();
        }

        let shared = Shared::new(index, roots);
        {
            let mut h = shared.health.write().unwrap();
            h.journal_corrupt = replay.corrupt_tail;
            h.catching_up = !had_snapshot || replay.corrupt_tail || handoff;
        }

        let sync = SyncRegistry::new();
        let fs: Arc<dyn FsView> = Arc::new(RealFs);
        let mut applier = Applier::new(shared.clone(), store, fs, sync.clone());
        // §4/§6: exclusions must be installed before the initial crawl, so an
        // excluded subtree is never enumerated (and, for a whole-mount backend
        // that cannot decline the watch, never inserted either).
        if !config.excludes.is_empty() {
            applier.set_excludes(config.excludes.clone());
        }
        let (backend, resolved_backend) = make_backend(config.backend)?;

        // Reconcile queued at open: swapped drives re-verified, and on a
        // cross-OS handoff every local root is re-reconciled unconditionally
        // (a crawl only adds; deletions need the diff).
        for rid in drive_changed {
            applier.pending_mut().enqueue_rescan(rid, RescanCause::MountEvent);
        }
        if handoff {
            let ix = shared.index.read().unwrap();
            let roots = shared.roots.read().unwrap();
            for r in roots.iter().filter(|r| r.is_local()) {
                let rid = findex::applier::root_dir_id(&ix, r);
                if rid != ROOT_ID {
                    applier
                        .pending_mut()
                        .enqueue_rescan(rid, RescanCause::OsHandoff);
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
            let next = findex::persist::StoreStamp {
                format_version: findex::config_paths::FORMAT_VERSION,
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
            last_mount_check_ms: 0,
            watch_rx: Some(watch_rx),
        })
    }

    /// Install watches on the root dirs and start the watcher thread.
    pub fn start(&mut self) -> anyhow::Result<()> {
        self.applier.rebuild_dir_cache();

        // Watch the roots first, then enumerate (§5).
        let roots: Vec<(u32, PathBuf)> = {
            let ix = self.shared.index.read().unwrap();
            let roots = self.shared.roots.read().unwrap();
            roots
                .iter()
                .filter_map(|r| {
                    let rid = findex::applier::root_dir_id(&ix, r);
                    (rid != ROOT_ID).then(|| (rid, r.path.clone()))
                })
                .collect()
        };
        for (id, path) in roots {
            self.watch_dir(id, &path);
            self.applier
                .pending_mut()
                .enqueue_crawl(id);
        }

        // Watcher thread: poll the backend and forward events, and service
        // watch requests for directories discovered during the crawl.
        let stop = self.watcher_stop.clone();
        let tx = self.incoming_tx.clone();
        let backend = self.backend.clone();
        let roots_snapshot = self.shared.roots.read().unwrap().clone();
        let watch_rx = self.watch_rx.take().expect("watch_rx taken twice");
        let fail_tx = self.watch_fail_tx.clone();
        self.watcher_thread = Some(std::thread::spawn(move || {
            {
                let mut b = backend.lock().unwrap();
                for r in &roots_snapshot {
                    let _ = b.add_root(r);
                }
            }
            while !stop.load(Ordering::Relaxed) {
                // Service pending watch additions (non-blocking). A failure is
                // *never* silent: it is reported so the applier can degrade the
                // root loudly (§7, §15).
                while let Ok((id, path)) = watch_rx.try_recv() {
                    let mut b = backend.lock().unwrap();
                    if let Err(e) = b.watch_dir(id, &path) {
                        let _ = fail_tx.send(WatchFailure { dir: id, reason: e.to_string() });
                    }
                }
                let evs = {
                    let mut b = backend.lock().unwrap();
                    b.poll(Duration::from_millis(50)).unwrap_or_default()
                };
                if !evs.is_empty() && tx.send(evs).is_err() {
                    break;
                }
                // Drain watch requests again promptly so a new dir is watched
                // before its contents change.
                while let Ok((id, path)) = watch_rx.try_recv() {
                    let mut b = backend.lock().unwrap();
                    if let Err(e) = b.watch_dir(id, &path) {
                        let _ = fail_tx.send(WatchFailure { dir: id, reason: e.to_string() });
                    }
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
                    self.applier.pending_mut().enqueue_rescan(root, RescanCause::Manual);
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

        // Clear `catching_up` once the crawl has caught up and the queue drains.
        if self.applier.pending().is_drained() {
            let mut h = self.shared.health.write().unwrap();
            if h.catching_up {
                h.catching_up = false;
            }
        }
        self.applier.maybe_compact();
        self.shared.health.write().unwrap().queue_depth = self.applier.pending().depth() as u64;
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
        let now = findex::applier::now_ms();
        if now.saturating_sub(self.last_mount_check_ms) < 1_000 {
            return false;
        }
        self.last_mount_check_ms = now;
        let mut changed: Vec<(u32, u64, Option<u64>, String)> = Vec::new();
        {
            let ix = self.shared.index.read().unwrap();
            let roots = self.shared.roots.read().unwrap();
            for r in roots.iter() {
                if !r.enabled {
                    continue;
                }
                let rid = findex::applier::root_dir_id(&ix, r);
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
                    self.applier.pending_mut().enqueue_rescan(rid, RescanCause::MountEvent);
                }
                self.applier.note_mount_remounted(rid, old_dev, cur.unwrap_or(0));
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
            if self.applier.pending().is_drained() && !self.shared.health.read().unwrap().catching_up
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
    pub fn server_ctx_for(&self, caller: Option<findex::ownership::Caller>) -> ServerCtx {
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
    use findex::drive::DriveId;
    use findex::persist::RecordedRoot;

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
        DriveId { disk_serial: Some(serial.to_string()), volume_uuid: None }
    }

    #[test]
    fn drive_change_needs_agreeing_mismatch() {
        let ix = index_with_root_entry(b"data");
        let mk = |drive: Option<DriveId>| root_with(0, "/mnt/data", drive, Some("linux"));
        let rec = |drive: Option<DriveId>| RecordedRoot { drive, owner_os: Some("linux".into()) };

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

        // Foreign roots are never touched, even on mismatch.
        let mut foreign = mk(Some(disk("B")));
        foreign.owner_os = Some("windows".into());
        let roots = vec![foreign];
        assert!(drive_changed_rids(&map, &roots, &ix).is_empty());

        // Legacy (unowned) roots behave like local ones.
        let mut legacy = mk(Some(disk("B")));
        legacy.owner_os = None;
        let roots = vec![legacy];
        assert_eq!(drive_changed_rids(&map, &roots, &ix).len(), 1);
    }
}
