//! Store ownership gates for dual-boot sharing (§15).
//!
//! All hermetic (tempdirs, fake backend, fictitious OS names where the gate
//! logic is concerned): the one thing these tests cannot do is boot another
//! OS, so cross-OS paths are exercised through stamps written by hand.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use fflocate::persist::{RecordedRoot, Store, StoreStamp};
use fflocated::config::{BackendChoice, Config};
use fflocated::daemon::Daemon;

const THIS_OS: &str = std::env::consts::OS;

fn other_os() -> &'static str {
    if std::env::consts::OS == "linux" {
        "windows"
    } else {
        "linux"
    }
}

fn test_config(dir: &std::path::Path, root: PathBuf, shared: bool) -> Config {
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
        shared_store: shared,
    }
}

fn stamp(writer_os: &str, shared: bool) -> StoreStamp {
    StoreStamp {
        format_version: fflocate::config_paths::FORMAT_VERSION,
        created_by_os: writer_os.to_string(),
        last_writer_os: writer_os.to_string(),
        backend: "fake".into(),
        shared,
        drives: BTreeMap::new(),
    }
}

fn tree_with(d: &tempfile::TempDir, name: &str) -> PathBuf {
    let root = d.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(name), b"x").unwrap();
    root
}

#[test]
fn legacy_store_adopts_and_stamps() {
    let d = tempfile::tempdir().unwrap();
    let root = tree_with(&d, "a.txt");
    let cfg = test_config(d.path(), root, false);
    let state = cfg.state_dir.clone();
    let mut daemon = Daemon::build(cfg).unwrap();
    // No handoff on a fresh store: no rescans queued at open.
    assert_eq!(daemon.applier_mut().pending().work_len(), 0);
    drop(daemon);
    // ...but the boot stamps its ownership for the next OS to see.
    let s = Store::read_stamp(&state).expect("stamp written at build");
    assert_eq!(s.last_writer_os, THIS_OS);
    assert_eq!(s.created_by_os, THIS_OS);
    assert!(!s.shared);
}

#[test]
fn newer_snapshot_refuses_before_any_mutation() {
    let d = tempfile::tempdir().unwrap();
    let root = tree_with(&d, "a.txt");
    let state = d.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    // A snapshot from a newer format: bump the version, fix the checksum so
    // only the version gate can fire.
    let ix = fflocate::index::Index::new();
    let mut bytes = fflocate::snapshot::encode(&ix, &[]);
    let newer = fflocate::config_paths::FORMAT_VERSION + 1;
    bytes[4..8].copy_from_slice(&newer.to_le_bytes());
    let sum = fflocate::util::crc32(&bytes[..bytes.len() - 4]);
    let n = bytes.len();
    bytes[n - 4..].copy_from_slice(&sum.to_le_bytes());
    std::fs::write(Store::snapshot_path(&state), &bytes).unwrap();

    let cfg = test_config(d.path(), root, true); // flag must not save it
    let Err(err) = Daemon::build(cfg) else {
        panic!("expected refusal")
    };
    let err = err.to_string();
    assert!(
        err.contains("newer"),
        "version gate must refuse, got: {err}"
    );
}

#[test]
fn foreign_store_refuses_without_the_flag() {
    let d = tempfile::tempdir().unwrap();
    let root = tree_with(&d, "a.txt");
    let state = d.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let mut s = stamp(other_os(), false);
    s.drives.insert(
        root.to_string_lossy().into_owned(),
        RecordedRoot {
            drive: None,
            owner_os: Some(other_os().into()),
        },
    );
    Store::write_stamp(&state, &s).unwrap();

    let cfg = test_config(d.path(), root, false);
    let Err(err) = Daemon::build(cfg) else {
        panic!("expected refusal")
    };
    let err = err.to_string();
    assert!(err.contains("foreign"), "must refuse, got: {err}");
    assert!(
        err.contains("--shared-store"),
        "must point at the flag, got: {err}"
    );
}

#[test]
fn foreign_store_opens_with_flag_and_handoffs() {
    // Baseline generation from a same-OS store: the handoff must bump
    // exactly one past it (old cursors go fresh, §9).
    let d0 = tempfile::tempdir().unwrap();
    let root0 = tree_with(&d0, "a.txt");
    let base_gen = Daemon::build(test_config(d0.path(), root0, false))
        .unwrap()
        .shared
        .generation();

    let d = tempfile::tempdir().unwrap();
    let root = tree_with(&d, "a.txt");
    let state = d.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    Store::write_stamp(&state, &stamp(other_os(), true)).unwrap();

    let cfg = test_config(d.path(), root.clone(), true);
    let mut daemon = Daemon::build(cfg).unwrap();
    // start() builds the dir-path cache rescans resolve through (production
    // always runs build → start; without it the rescan finds nothing).
    daemon.start().unwrap();
    // Handoff: generation bumped (old cursors go fresh, §9) ...
    assert_eq!(daemon.shared.generation(), base_gen + 1);
    // ... the daemon knows it is catching up ...
    assert!(daemon.shared.health.read().unwrap().catching_up);
    // ... and a handoff rescan is queued for the local root before serving.
    assert!(
        daemon.applier_mut().pending().work_len() >= 1,
        "handoff must enqueue rescans"
    );
    // Draining runs it: the cause is recorded honestly, and the file the
    // other OS never saw is indexed.
    assert!(daemon.settle(Duration::from_secs(10)), "must settle");
    let roots = daemon.shared.roots.read().unwrap();
    assert_eq!(
        roots[0].last_rescan_cause,
        Some(fflocate::event::RescanCause::OsHandoff)
    );
    drop(roots);
    let ctx = daemon.server_ctx();
    let out = fflocate::server::handle_request(
        &ctx,
        &fflocate::proto::Request::query("a.txt", 0, false, false),
    );
    assert!(
        out.iter().any(
            |r| matches!(r, fflocate::proto::Response::Hit { path, .. } if path.ends_with("a.txt"))
        ),
        "handoff rescan must index the tree"
    );
    // ... and the stamp now names this OS the writer.
    let s = Store::read_stamp(&state).unwrap();
    assert_eq!(s.last_writer_os, THIS_OS);
    assert!(s.shared, "flag persists in the stamp");
}
