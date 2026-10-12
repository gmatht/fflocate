//! Live fanotify integration (§6, v1.5): events must converge the *index*.
//!
//! The unit test inside `watch/fanotify.rs` proves the event *stream* decodes.
//! This proves the other half: those events, pushed through the applier, make
//! the index reflect what actually happened on disk. That is the property that
//! matters — a backend that reports perfect events into a broken pipe is worth
//! nothing.
//!
//! ## The private tmpfs is not incidental
//!
//! `FAN_MARK_FILESYSTEM` watches a whole *mount*. Marking `/tmp` on this machine
//! would watch the entire root filesystem and the test would drown in other
//! processes' events (and could not tell its own from theirs). A freshly mounted
//! tmpfs is a mount containing only our tree, so every event is ours.
//!
//! Skips cleanly where fanotify or mount is unavailable — a test that fails on
//! an unsupported machine teaches nothing.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use findex::applier::{Applier, Shared};
use findex::event::{FsEvent, RescanCause};
use findex::index::{EntryFlags, Index, ROOT_ID};
use findex::mount::{fanotify_available, handle_of_path};
use findex::persist::Store;
use findex::roots::Root;
use findex::sync::SyncRegistry;
use findex::watch::fidmap::FidMap;
use findex::watch::{FanotifyBackend, WatchBackend};

/// Removes the tmpfs on drop even if the test panics.
struct TmpfsGuard(PathBuf);
impl Drop for TmpfsGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("umount").arg(&self.0).status();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Mount a private tmpfs, or return `None` if we cannot (unprivileged/no mount).
///
/// Unmounts any stale mount at the path first: a previous test run that panicked
/// before its guard ran can leave one (or several) stacked here, and then our
/// `FAN_MARK_FILESYSTEM` would watch a different layer than the one the test
/// writes to — an excellent way to get a mysteriously empty index.
fn mount_private_tmpfs(dir: &Path) -> Option<TmpfsGuard> {
    for _ in 0..8 {
        let unmounted = std::process::Command::new("umount")
            .arg(dir)
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !unmounted {
            break;
        }
    }
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).ok()?;
    let ok = std::process::Command::new("mount")
        .args(["-t", "tmpfs", "-o", "size=8M", "tmpfs"])
        .arg(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_dir_all(dir);
        return None;
    }
    Some(TmpfsGuard(dir.to_path_buf()))
}

/// Everything the test needs: an applier whose root is the tmpfs, and the
/// fanotify backend watching it, with the root's file handle already learned.
struct Harness {
    applier: Applier,
    shared: Arc<Shared>,
    backend: FanotifyBackend,
    _store: tempfile::TempDir,
    _tmpfs: TmpfsGuard,
    root_id: u32,
    root: PathBuf,
}

fn harness(tag: &str) -> Option<Harness> {
    if !fanotify_available() {
        eprintln!("skipping: fanotify not available on this system");
        return None;
    }
    let dir = PathBuf::from(format!("/tmp/findex-fanotify-live-{tag}"));
    let tmpfs = mount_private_tmpfs(&dir)?;

    let store_dir = tempfile::tempdir().ok()?;
    let store = Store::open(&store_dir.path().join("state")).ok()?;

    let mut ix = Index::new();
    // The root entry's name is the path's basename — that is how `root_dir_id`
    // finds it, and it is what the daemon does. Getting this wrong leaves the
    // path cache un-primed and nothing is crawled.
    let basename = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let root_id = ix.insert(
        ROOT_ID,
        basename.as_bytes(),
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let mut root = Root::new(0, dir.clone());
    root.caps.writable = true;
    root.caps.file_handles = true;
    let shared = Shared::new(ix, vec![root]);
    let fs: Arc<dyn findex::applier::FsView> = Arc::new(findex::applier::RealFs);
    let mut applier = Applier::new(shared.clone(), store, fs, SyncRegistry::new());
    applier.rebuild_dir_cache();

    let mut backend = FanotifyBackend::new().ok()?;
    // Learn the tmpfs root's handle exactly as a crawl would (this is the
    // FID→dir-id table the resolver consults for every event).
    let handle = handle_of_path(&dir)?;
    let mut fids = FidMap::new();
    fids.learn_root(&handle, root_id);
    backend.learn_fid(
        findex::watch::fanotify::FidIdentity {
            fsid: [0, 0],
            handle_bytes: handle.handle.len() as u32,
            handle_type: handle.handle_type,
            handle: handle.handle.clone(),
        },
        root_id,
    );
    backend
        .add_root(shared.roots.read().unwrap().first().unwrap())
        .ok()?;

    Some(Harness {
        applier,
        shared,
        backend,
        _store: store_dir,
        _tmpfs: tmpfs,
        root_id,
        root: dir,
    })
}

impl Harness {
    /// Pump the backend for a while, translating its events into the applier.
    fn pump(&mut self, for_ms: u64) {
        let deadline = std::time::Instant::now() + Duration::from_millis(for_ms);
        while std::time::Instant::now() < deadline {
            let evs = self.backend.poll(Duration::from_millis(25)).unwrap();
            for e in evs {
                self.applier.push(e);
            }
            self.applier.drain();
            // The daemon's bridge: teach the backend the file handle of every
            // directory the applier discovered, so events naming that directory
            // as a parent can resolve. Without this, a directory created at
            // runtime is indexed but its contents are never seen.
            for (id, path) in self.applier.take_watch_requests() {
                let _ = self.backend.watch_dir(id, &path);
            }
        }
    }

    fn index_paths(&self) -> Vec<String> {
        let ix = self.shared.index.read().unwrap();
        let mut out: Vec<String> = ix
            .iter_live()
            .filter(|(_, e)| !e.flags().is_root_dir())
            .filter_map(|(id, _)| ix.path_of(id, self.root.to_string_lossy().as_bytes()))
            .map(|p| String::from_utf8_lossy(&p).into_owned())
            .collect();
        out.sort();
        out
    }
}

#[test]
fn fanotify_events_converge_the_index() {
    let Some(mut h) = harness("converge") else {
        return;
    };
    // Seed the index with the (empty) root's real contents, then watch.
    h.applier.pending_mut().enqueue_crawl(h.root_id);
    h.applier.drain();

    // Space the operations: a burst that races the mark loses the earliest op,
    // and the point here is the index, not fanotify's burst behaviour.
    let script = format!(
        "sleep 0.4; mkdir {d}/sub; sleep 0.4; echo hi > {d}/sub/a.rs; sleep 0.4; mv {d}/sub/a.rs {d}/sub/b.rs; sleep 0.4; rm -f {d}/sub/b.rs; sleep 0.2",
        d = h.root.display()
    );

    // Run the shell in a thread so we can pump events *while* it works.
    let root = h.root.clone();
    let worker = std::thread::spawn(move || {
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .status();
        let _ = root;
    });
    h.pump(2600);
    let _ = worker.join();
    // One more pump to catch the tail after the shell exits.
    h.pump(400);
    let paths = h.index_paths();
    // The sequence was mkdir → create a.rs → rename to b.rs → delete b.rs, so
    // the *final* state is just the directory. Asserting every intermediate
    // state would be asserting on timing, not on convergence.
    assert!(
        paths.iter().any(|p| p.ends_with("/sub")),
        "mkdir must be indexed; got {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.ends_with("/sub/a.rs")),
        "a.rs was renamed away and must be gone; got {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.ends_with("/sub/b.rs")),
        "b.rs was deleted and must be gone; got {paths:?}"
    );
    // And the resolver never had to guess — every event, including the ones
    // inside the directory created *after* we started watching, resolved.
    assert_eq!(
        h.backend.unresolved_events, 0,
        "every event's parent handle was learned"
    );
}

#[test]
fn fanotify_index_survives_a_rescan_after_events() {
    // Events give a *live* view; a rescan must agree with it. If the two
    // disagree, one of them is lying, and the whole "diffs over replacements"
    // contract (§7) rests on them agreeing.
    let Some(mut h) = harness("rescan") else {
        return;
    };
    h.applier.pending_mut().enqueue_crawl(h.root_id);
    h.applier.drain();

    // Create files, then settle.
    let root = h.root.clone();
    let worker = std::thread::spawn(move || {
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "sleep 0.4; echo a > {d}/one.txt; sleep 0.3; echo b > {d}/two.txt; sleep 0.2",
                d = root.display()
            ))
            .status();
    });
    h.pump(2000);
    let _ = worker.join();
    h.pump(300);

    let after_events = h.index_paths();
    assert!(
        after_events.iter().any(|p| p.ends_with("/one.txt")),
        "got {after_events:?}"
    );
    assert!(
        after_events.iter().any(|p| p.ends_with("/two.txt")),
        "got {after_events:?}"
    );

    // A full rescan must converge to the same set (not replace it with something
    // else, and not lose the files the events added).
    h.applier
        .pending_mut()
        .enqueue_rescan(h.root_id, RescanCause::Manual);
    h.applier.drain();
    let after_rescan = h.index_paths();
    assert_eq!(after_events, after_rescan, "events and a rescan must agree");
}

#[test]
fn fanotify_ignores_events_outside_the_root() {
    // A whole-mount watch reports the whole mount. An event whose parent handle
    // we never learned must be dropped, not attributed to a root we do know.
    let Some(mut h) = harness("foreign") else {
        return;
    };
    h.applier.drain();

    // Fabricate an event with an unknown parent id: the resolver cannot have a
    // mapping for it, so nothing may be indexed.
    let before = h.index_paths();
    h.applier.push(FsEvent::upsert(
        findex::event::DirKey::new(9999),
        b"stranger.rs".to_vec(),
        false,
    ));
    h.applier.drain();
    let after = h.index_paths();
    assert_eq!(before, after, "an unknown parent must not create entries");
}
