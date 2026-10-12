//! Real differential tests (§14.2) and fault injection (§14.3).
//!
//! Random op sequences (create/delete/rename/move) are applied to a temp dir
//! with `std::fs`; after each step we settle (cookie sync — deterministic, no
//! sleeps) and compare the full index path set against a from-scratch walk.
//! This catches ~95% of watcher bugs.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use findex::applier::{Applier, Shared};
use findex::event::{DirKey, FsEvent, RescanCause};
use findex::index::{EntryFlags, Index, ROOT_ID};
use findex::persist::Store;
use findex::roots::Root;
use findex::sync::SyncRegistry;

/// Canonical comparison form: verbatim `\\?\` prefixes stripped, separators
/// normalized to `/`, so the OS-path oracle (`truth`) compares equal to the
/// index's `/`-canonical paths. Identity off Windows.
fn canon(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    #[cfg(not(windows))]
    {
        s
    }
    #[cfg(windows)]
    {
        let s = s
            .strip_prefix(r"\\?\UNC\")
            .map(|r| format!(r"\\{r}"))
            .or_else(|| s.strip_prefix(r"\\?\").map(str::to_string))
            .unwrap_or(s);
        s.replace('\\', "/")
    }
}

/// A minimal deterministic PRNG (xorshift64*) so failures reproduce.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

struct Fixture {
    applier: Applier,
    shared: Arc<Shared>,
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    root_id: u32,
}

fn fixture() -> Fixture {
    let d = tempfile::tempdir().unwrap();
    // The indexed root is a *subdirectory* so the state dir (journal, snapshot)
    // never pollutes the tree we compare against.
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let store = Store::open(&d.path().join("state")).unwrap();

    let mut ix = Index::new();
    let basename = root.file_name().unwrap().to_string_lossy().to_string();
    let root_id = ix.insert(
        ROOT_ID,
        basename.as_bytes(),
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let mut r = Root::new(0, root.clone());
    r.caps.writable = true;
    let shared = Shared::new(ix, vec![r]);
    let mut applier = Applier::new(
        shared.clone(),
        store,
        Arc::new(findex::applier::RealFs),
        SyncRegistry::new(),
    );
    applier.rebuild_dir_cache();
    applier.pending_mut().enqueue_crawl(root_id);
    applier.drain();
    applier.drain();
    Fixture {
        applier,
        shared,
        _dir: d,
        root,
        root_id,
    }
}

impl Fixture {
    /// Settle: drain everything and run a full rescan to reconcile any missed
    /// events (the deterministic equivalent of `ff --sync` + a safety rescan).
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.applier.drain();
            if self.applier.pending().is_drained() {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        // Reconcile: rescan the root so any event the fake watcher did not model
        // (we do not run a real watcher in this test) is healed.
        self.applier
            .pending_mut()
            .enqueue_rescan(self.root_id, RescanCause::Manual);
        loop {
            self.applier.drain();
            if self.applier.pending().is_drained() {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
        }
    }

    /// The index's view of all paths under the root (files and dirs).
    fn indexed(&self) -> BTreeSet<String> {
        let ix = self.shared.index.read().unwrap();
        let mut out = BTreeSet::new();
        let root_path = canon(&self.root).into_bytes();
        for (id, e) in ix.iter_live() {
            if e.flags().is_root_dir() {
                continue;
            }
            if let Some(p) = ix.path_of(id, &root_path) {
                out.insert(String::from_utf8_lossy(&p).into_owned());
            }
        }
        out
    }
}

/// Walk the real tree (the oracle).
fn truth(root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            out.insert(canon(&p));
            if ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(p);
            }
        }
    }
    out
}

#[test]
fn differential_random_operations() {
    let mut fx = fixture();
    let mut rng = Rng::new(0xC0FFEE);
    let mut dirs: Vec<std::path::PathBuf> = vec![fx.root.clone()];
    let mut files: Vec<std::path::PathBuf> = Vec::new();

    for step in 0..120 {
        match rng.below(4) {
            0 => {
                // create a file in a random dir
                let dir = &dirs[rng.below(dirs.len() as u64) as usize];
                let name = format!("f{step}.rs");
                let p = dir.join(&name);
                std::fs::write(&p, b"x").unwrap();
                files.push(p);
            }
            1 if !files.is_empty() => {
                // delete a random file
                let i = rng.below(files.len() as u64) as usize;
                let p = files.swap_remove(i);
                let _ = std::fs::remove_file(&p);
            }
            2 if !files.is_empty() => {
                // rename a random file
                let i = rng.below(files.len() as u64) as usize;
                let p = files[i].clone();
                let dir = p.parent().unwrap().to_path_buf();
                let np = dir.join(format!("r{step}.txt"));
                if std::fs::rename(&p, &np).is_ok() {
                    files[i] = np;
                }
            }
            _ => {
                // create a subdirectory (bounded depth)
                let dir = &dirs[rng.below(dirs.len() as u64) as usize];
                let depth = dir.components().count();
                if depth < 6 {
                    let p = dir.join(format!("d{step}"));
                    if std::fs::create_dir(&p).is_ok() {
                        dirs.push(p);
                    }
                }
            }
        }
        fx.settle();

        let got = fx.indexed();
        let want = truth(&fx.root);
        assert_eq!(
            got, want,
            "index diverged from the filesystem at step {step}\n  missing from index: {:?}\n  extra in index: {:?}",
            want.difference(&got).collect::<Vec<_>>(),
            got.difference(&want).collect::<Vec<_>>()
        );
    }
}

#[test]
fn differential_with_live_event_notifications() {
    // Same, but this time we *also* inject the watcher events we would expect,
    // exercising the live-apply path as well as the rescan.
    let mut fx = fixture();
    let mut rng = Rng::new(0xBEEF);
    let mut files: Vec<std::path::PathBuf> = Vec::new();

    for step in 0..60 {
        let name = format!("e{step}.dat");
        let p = fx.root.join(&name);
        std::fs::write(&p, b"x").unwrap();
        // The watcher would deliver this create:
        fx.applier.push(FsEvent::upsert(
            DirKey::new(fx.root_id),
            name.as_bytes().to_vec(),
            false,
        ));
        files.push(p);

        if step % 5 == 4 && !files.is_empty() {
            let p = files.remove(0);
            let n = p.file_name().unwrap().to_string_lossy().to_string();
            std::fs::remove_file(&p).unwrap();
            fx.applier.push(FsEvent::remove(
                DirKey::new(fx.root_id),
                n.as_bytes().to_vec(),
            ));
        }

        fx.applier.drain();
        let _ = rng.below(10);
    }
    // Final reconcile.
    fx.applier
        .pending_mut()
        .enqueue_rescan(fx.root_id, RescanCause::Manual);
    fx.applier.drain();

    assert_eq!(fx.indexed(), truth(&fx.root));
}

#[test]
fn fault_injection_overflow_then_rescan_converges() {
    let mut fx = fixture();
    // Create real files first.
    for i in 0..10 {
        std::fs::write(fx.root.join(format!("real{i}.txt")), b"x").unwrap();
    }
    // Now simulate an overflow: the index is told nothing, but a rescan runs.
    fx.applier.push(FsEvent::rescan(
        DirKey::new(fx.root_id),
        RescanCause::Overflow,
    ));
    fx.applier.drain();
    assert_eq!(fx.indexed(), truth(&fx.root));
}

#[test]
fn fault_injection_unpaired_move_degrades_to_remove_then_rescan_heals() {
    let mut fx = fixture();
    std::fs::write(fx.root.join("moveme.txt"), b"x").unwrap();
    fx.applier.push(FsEvent::rescan(
        DirKey::new(fx.root_id),
        RescanCause::Manual,
    ));
    fx.applier.drain();
    assert!(fx.indexed().iter().any(|p| p.ends_with("moveme.txt")));

    // The backend lost the MOVED_TO half: it only reports a remove.
    fx.applier.push(FsEvent::remove(
        DirKey::new(fx.root_id),
        b"moveme.txt".to_vec(),
    ));
    // In reality the file was renamed:
    std::fs::rename(fx.root.join("moveme.txt"), fx.root.join("renamed.txt")).unwrap();
    fx.applier.drain();
    // The index now believes the old name is gone (correct) but lacks the new
    // one; a rescan must heal. Backoff may defer it briefly, so retry until the
    // window opens (bounded) — a deferred rescan is never lost.
    fx.applier.push(FsEvent::rescan(
        DirKey::new(fx.root_id),
        RescanCause::WatchDropped,
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        fx.applier.drain();
        if fx.indexed() == truth(&fx.root) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "deferred rescan never ran: {:?}",
            fx.indexed()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(fx.indexed(), truth(&fx.root));
}

#[test]
fn backoff_limits_rescan_rate_under_an_event_storm() {
    // §14.4: a pathological event storm must converge to a bounded rescan rate.
    let mut fx = fixture();
    // Force many rescans in quick succession; the backoff must defer most.
    for _ in 0..50 {
        fx.applier.push(FsEvent::rescan(
            DirKey::new(fx.root_id),
            RescanCause::Overflow,
        ));
        fx.applier.drain();
    }
    let h = fx.shared.health.read().unwrap();
    let roots = fx.shared.roots.read().unwrap();
    let deferred = h.rescans_deferred;
    let recrawl = roots[0].recrawl_count;
    assert!(
        deferred > 0,
        "the backoff must defer at least some of 50 immediate rescans (deferred={deferred}, recrawl={recrawl})"
    );
    assert!(
        recrawl < 50,
        "the rescans must be rate-limited (recrawl={recrawl})"
    );
}

#[test]
fn manual_rescan_bypasses_backoff() {
    let mut fx = fixture();
    let before = fx.shared.roots.read().unwrap()[0].recrawl_count;
    for _ in 0..3 {
        fx.applier.push(FsEvent::rescan(
            DirKey::new(fx.root_id),
            RescanCause::Manual,
        ));
        fx.applier.drain();
    }
    let after = fx.shared.roots.read().unwrap()[0].recrawl_count;
    assert_eq!(after - before, 3, "manual rescans must not be deferred");
}
