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
use findex::watch::{FakeBackend, InotifyBackend, WatchBackend};

use crate::config::{BackendChoice, Config};

/// The backend shared between the watcher thread (poll) and the applier loop
/// (install watches).
pub type SharedBackend = Arc<Mutex<Box<dyn WatchBackend>>>;

/// Build the configured backend.
pub fn make_backend(choice: BackendChoice) -> anyhow::Result<SharedBackend> {
    let b: Box<dyn WatchBackend> = match choice {
        BackendChoice::Inotify => match InotifyBackend::new() {
            Ok(b) => Box::new(b),
            Err(e) => anyhow::bail!("inotify unavailable: {e}"),
        },
        BackendChoice::Fake => Box::new(FakeBackend::new(findex::watch::fake::FakeHandle::new())),
    };
    Ok(Arc::new(Mutex::new(b)))
}

/// A live daemon.
pub struct Daemon {
    pub shared: Arc<Shared>,
    pub config: Config,
    pub sync: Arc<SyncRegistry>,
    applier: Applier,
    backend: SharedBackend,
    /// Dirs already asked to be watched (dedup).
    watched_dirs: std::collections::HashSet<u32>,
    /// Watch requests from the applier to the watcher thread (new dirs).
    watch_tx: Sender<(u32, PathBuf)>,
    incoming_rx: Receiver<Vec<FsEvent>>,
    incoming_tx: Sender<Vec<FsEvent>>,
    control_rx: Receiver<Control>,
    control_tx: ControlSender,
    applier_stop: Arc<AtomicBool>,
    watcher_stop: Arc<AtomicBool>,
    watcher_thread: Option<std::thread::JoinHandle<()>>,
    /// Kept until `start()` moves it into the watcher thread.
    watch_rx: Option<Receiver<(u32, PathBuf)>>,
}

impl Daemon {
    /// Build a daemon from a config. Loads the snapshot + replays the journal.
    pub fn build(config: Config) -> anyhow::Result<Daemon> {
        let mut store = Store::open(&config.state_dir)?;

        let loaded = store.load_snapshot()?;
        let had_snapshot = loaded.is_some();
        let (mut index, mut roots) = match loaded {
            Some(l) => (l.index, l.roots),
            None => (Index::new(), Vec::new()),
        };

        // Merge configured roots into the roots table.
        for p in &config.roots {
            if !roots.iter().any(|r| r.path == *p) {
                let id = roots.len() as u32;
                let mut root = Root::new(id, p.clone());
                root.caps = mount::probe_root(p);
                roots.push(root);
            }
        }
        // Assign each root a root-dir entry in the index (name = basename).
        for root in roots.iter_mut() {
            if let Some(base) = root.path.file_name() {
                let base_bytes = base.to_string_lossy().as_bytes().to_vec();
                if index.lookup(ROOT_ID, &base_bytes).is_none() {
                    index.insert(
                        ROOT_ID,
                        &base_bytes,
                        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
                    );
                }
            }
            root.caps = mount::probe_root(&root.path);
        }

        // Replay the journal tail. `replay` reports the records; we apply them
        // to the index ourselves (the same `apply_record` the applier uses).
        let mut tail: Vec<Record> = Vec::new();
        let replay = store.replay(&mut index, |_lsn, rec| tail.push(rec.clone()))?;
        for rec in &tail {
            findex::persist::apply_record(&mut index, rec);
        }

        let shared = Shared::new(index, roots);
        {
            let mut h = shared.health.write().unwrap();
            h.journal_corrupt = replay.corrupt_tail;
            h.catching_up = !had_snapshot || replay.corrupt_tail;
        }

        let sync = SyncRegistry::new();
        let fs: Arc<dyn FsView> = Arc::new(RealFs);
        let applier = Applier::new(shared.clone(), store, fs, sync.clone());
        let backend = make_backend(config.backend)?;

        let (incoming_tx, incoming_rx) = bounded::<Vec<FsEvent>>(1000);
        let (control_tx, control_rx) = bounded::<Control>(64);
        let (watch_tx, watch_rx) = bounded::<(u32, PathBuf)>(4096);

        Ok(Daemon {
            shared,
            config,
            sync,
            applier,
            backend,
            watched_dirs: std::collections::HashSet::new(),
            watch_tx,
            incoming_rx,
            incoming_tx,
            control_rx,
            control_tx,
            applier_stop: Arc::new(AtomicBool::new(false)),
            watcher_stop: Arc::new(AtomicBool::new(false)),
            watcher_thread: None,
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
        self.watcher_thread = Some(std::thread::spawn(move || {
            {
                let mut b = backend.lock().unwrap();
                for r in &roots_snapshot {
                    let _ = b.add_root(r);
                }
            }
            while !stop.load(Ordering::Relaxed) {
                // Service pending watch additions (non-blocking).
                while let Ok((id, path)) = watch_rx.try_recv() {
                    let mut b = backend.lock().unwrap();
                    let _ = b.watch_dir(id, &path);
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
                    let _ = b.watch_dir(id, &path);
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
            backend: self.config.backend.as_str().to_string(),
            control: Some(self.control_tx.clone()),
        }
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
