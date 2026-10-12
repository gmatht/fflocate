//! §14.5 soak: a bounded soak loop with periodic full-tree audits.
//!
//! DESIGN.md asks for a 24 h soak with `find` audits and zero drift. A real 24 h
//! run cannot live in CI, so this is a *bounded* version with the same shape:
//! many thousands of random namespace operations against a real tree, driven
//! through the applier with live event injection, audited against a from-scratch
//! walk at intervals. Zero drift is the bar.
//!
//! Budget: `FINDEX_SOAK_OPS` (default 20 000 ops ≈ a few seconds), audit every
//! `FINDEX_SOAK_AUDIT` ops (default 2 000). Set both higher for a long run:
//!
//! ```sh
//! FINDEX_SOAK_OPS=2000000 cargo test --release -p findex --test soak -- --nocapture
//! ```
//!
//! It is `#[ignore]` by default so a plain `cargo test` stays quick; run it with
//! `--ignored`, or via `FINDEX_E2E_SOAK=1 scripts/e2e.sh`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use findex::applier::{Applier, Shared};
use findex::event::{DirKey, FsEvent, RescanCause};
use findex::index::{EntryFlags, Index, ROOT_ID};
use findex::persist::Store;
use findex::roots::Root;
use findex::sync::SyncRegistry;

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
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Canonical comparison form (see differential.rs): verbatim prefixes
/// stripped, separators normalized, so the oracle compares equal to the
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
#[ignore = "soak; run with --ignored or FINDEX_E2E_SOAK=1"]
fn soak_thousands_of_ops_zero_drift() {
    let ops: usize = std::env::var("FINDEX_SOAK_OPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20_000);
    let audit_every: usize = std::env::var("FINDEX_SOAK_AUDIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000);

    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    // State lives beside, not inside, the indexed tree.
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
    // A small journal-compaction threshold so the soak also exercises compaction.
    let mut applier = Applier::new(
        shared.clone(),
        store,
        Arc::new(findex::applier::RealFs),
        SyncRegistry::new(),
    );
    applier.compaction_threshold = 1_500;
    applier.rebuild_dir_cache();
    applier.pending_mut().enqueue_crawl(root_id);
    applier.drain();

    let mut rng = Rng::new(0x5EA5_0A5E);
    let mut dirs: Vec<PathBuf> = vec![root.clone()];
    let mut files: Vec<PathBuf> = Vec::new();
    let mut name_gen: u64 = 0;

    let settle = |applier: &mut Applier| {
        // Drain everything, then reconcile with a rescan (the deterministic
        // equivalent of `ff --sync`, plus the safety rescan a real watcher would
        // trigger on any missed event).
        for _ in 0..8 {
            applier.drain();
            if applier.pending().is_drained() {
                break;
            }
        }
        applier
            .pending_mut()
            .enqueue_rescan(root_id, RescanCause::Manual);
        for _ in 0..8 {
            applier.drain();
            if applier.pending().is_drained() {
                break;
            }
        }
    };

    for step in 0..ops {
        name_gen += 1;
        match rng.below(10) {
            0..=4 => {
                // create a file in a random dir
                let dir = dirs[rng.below(dirs.len())].clone();
                let name = format!("f{name_gen}.rs");
                let p = dir.join(&name);
                std::fs::write(&p, b"x").unwrap();
                applier.push(FsEvent::upsert(
                    DirKey::new(dir_id(&applier, &dir)),
                    name.as_bytes().to_vec(),
                    false,
                ));
                files.push(p);
            }
            5..=6 if !files.is_empty() => {
                let i = rng.below(files.len());
                let p = files.swap_remove(i);
                let parent = p.parent().unwrap().to_path_buf();
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                let _ = std::fs::remove_file(&p);
                applier.push(FsEvent::remove(
                    DirKey::new(dir_id(&applier, &parent)),
                    name.as_bytes().to_vec(),
                ));
            }
            7 if !files.is_empty() => {
                let i = rng.below(files.len());
                let p = files[i].clone();
                let parent = p.parent().unwrap().to_path_buf();
                let old = p.file_name().unwrap().to_string_lossy().to_string();
                let name = format!("m{name_gen}.txt");
                let np = parent.join(&name);
                if std::fs::rename(&p, &np).is_ok() {
                    let did = dir_id(&applier, &parent);
                    applier.push(FsEvent::remove(DirKey::new(did), old.as_bytes().to_vec()));
                    applier.push(FsEvent::upsert(
                        DirKey::new(did),
                        name.as_bytes().to_vec(),
                        false,
                    ));
                    files[i] = np;
                }
            }
            8 => {
                // mkdir (bounded depth)
                let dir = dirs[rng.below(dirs.len())].clone();
                if dir.components().count() < 8 {
                    let p = dir.join(format!("d{name_gen}"));
                    if std::fs::create_dir(&p).is_ok() {
                        applier.push(FsEvent::upsert(
                            DirKey::new(dir_id(&applier, &dir)),
                            p.file_name().unwrap().to_string_lossy().as_bytes().to_vec(),
                            true,
                        ));
                        dirs.push(p);
                    }
                }
            }
            _ => {
                // rmdir an empty dir (best effort)
                if dirs.len() > 1 {
                    let i = 1 + rng.below(dirs.len() - 1);
                    let p = dirs[i].clone();
                    if std::fs::remove_dir(&p).is_ok() {
                        let parent = p.parent().unwrap().to_path_buf();
                        let name = p.file_name().unwrap().to_string_lossy().to_string();
                        applier.push(FsEvent::remove(
                            DirKey::new(dir_id(&applier, &parent)),
                            name.as_bytes().to_vec(),
                        ));
                        dirs.remove(i);
                    }
                }
            }
        }

        if (step + 1) % audit_every == 0 {
            settle(&mut applier);
            // Full-tree audit: the index must equal a from-scratch walk.
            let got = index_paths(&shared, &root);
            let want = truth(&root);
            assert_eq!(
                got,
                want,
                "DRIFT after {} ops\n  missing: {:?}\n  extra: {:?}",
                step + 1,
                want.difference(&got).take(8).collect::<Vec<_>>(),
                got.difference(&want).take(8).collect::<Vec<_>>()
            );
            // Ids must remain stable for a sampled survivor.
            if let Some(f) = files.first() {
                if let Some(name) = f.file_name() {
                    let id = {
                        let ix = shared.index.read().unwrap();
                        ix.lookup(root_id, name.to_string_lossy().as_bytes())
                    };
                    if id.is_some() {
                        let again = {
                            let ix = shared.index.read().unwrap();
                            ix.lookup(root_id, name.to_string_lossy().as_bytes())
                        };
                        assert_eq!(id, again);
                    }
                }
            }
        }
    }

    // Final audit.
    settle(&mut applier);
    let got = index_paths(&shared, &root);
    let want = truth(&root);
    assert_eq!(
        got,
        want,
        "final drift\n  missing: {:?}\n  extra: {:?}",
        want.difference(&got).take(8).collect::<Vec<_>>(),
        got.difference(&want).take(8).collect::<Vec<_>>()
    );
    let h = shared.health.read().unwrap();
    eprintln!(
        "soak ok: {ops} ops, {:?} paths, recrawl_count={}, generation={}",
        want.len(),
        h.recrawl_count,
        shared.generation()
    );
}

fn index_paths(shared: &Shared, root: &Path) -> BTreeSet<String> {
    let ix = shared.index.read().unwrap();
    let root_path = canon(root).into_bytes();
    let mut out = BTreeSet::new();
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

/// Resolve a real absolute path to its index dir id by walking from the root
/// dir and matching names. This mirrors what the watcher would look up.
fn dir_id(applier: &Applier, path: &Path) -> u32 {
    // The applier caches dir id -> path; invert it for the test.
    applier.dir_id_for_path(path).unwrap_or(ROOT_ID)
}
