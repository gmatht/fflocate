//! A store written by the previous format, opened where it already sits,
//! migrated in place twice, and queried before and after — with rows written
//! after the upgrade beside the migrated ones.
//!
//! The unit tests in `snapshot` and `persist` pin the encoding and the
//! directory choice separately. This drives the whole chain the daemon takes:
//! the bytes a format-3 build wrote (real minutes, `0` for unknown), the
//! state-directory resolver finding that directory, `Store::load_snapshot`
//! reading it, `Store::compact` rewriting it in place as the current format,
//! and the query engine answering `since … oclock` from the stored minutes.
//!
//! What has to survive is not "some date": a migrated row keeps its exact
//! minute, an undated row stays unknown (never 1970), no row is lost, and the
//! second migration changes nothing. Minute `0` is the value format 3 could not
//! express, so it is written *after* the upgrade and queried across that
//! boundary.

use std::path::Path;
use std::time::UNIX_EPOCH;

use fflocate::config_paths::resolve_state_dir;
use fflocate::expr::{ClockSpec, Expr, SinceField};
use fflocate::index::{
    created_minutes_at, EntryFlags, Index, CREATED_LATEST_MINUTE, CREATED_UNKNOWN, ROOT_ID,
};
use fflocate::journal::Record;
use fflocate::persist::{apply_record, Store};
use fflocate::query::execute_expr;
use fflocate::roots::Root;

/// 2026-01-02T00:00:00Z in whole minutes since the epoch.
const JAN2: u32 = 29_455_200;

/// `since <ts> oclock`, the creation-clock lower bound (`otime`).
fn since(ts: i64) -> Expr {
    Expr::Since {
        spec: ClockSpec::Timestamp(ts),
        field: SinceField::Oclock,
    }
}

/// The creation-time window `[from, to)`.
fn range(from: i64, to: i64) -> Expr {
    Expr::AllOf(vec![since(from), Expr::Not(Box::new(since(to)))])
}

/// The paths a query returns, in the order the engine emitted them.
fn hits(ix: &Index, roots: &[(u32, Vec<u8>)], expr: &Expr) -> Vec<String> {
    execute_expr(ix, roots, b"", expr, 0, false)
        .hits
        .iter()
        .map(|h| String::from_utf8_lossy(&h.path).into_owned())
        .collect()
}

/// Write `ix` as a format-3 snapshot: version 3, and `0` (that format's
/// "unknown") wherever the index has no creation minute.
fn write_v3(dir: &Path, ix: &Index, roots: &[Root]) {
    let bytes = fflocate::snapshot::encode(ix, roots);
    let n = bytes.len();
    let mut body = bytes[..n - 4].to_vec();
    let created_start = n - 4 - ix.len() * 4;
    for id in 0..ix.len() as u32 {
        let stored = ix.created_min(id).unwrap_or(0);
        let at = created_start + id as usize * 4;
        body[at..at + 4].copy_from_slice(&stored.to_le_bytes());
    }
    body[4..8].copy_from_slice(&3u32.to_le_bytes());
    let sum = fflocate::util::crc32(&body);
    body.extend_from_slice(&sum.to_le_bytes());
    std::fs::write(Store::snapshot_path(dir), &body).unwrap();
}

/// The tree both formats index: a root directory, one subdirectory, and files
/// straddling the epoch, a normal date, and the top of the minute range.
struct Fixture {
    ix: Index,
    root_id: u32,
    main: u32,
    lib: u32,
    readme: u32,
    notes: u32,
    future: u32,
}

fn fixture() -> Fixture {
    let mut ix = Index::new();
    let root_id = ix.insert(
        ROOT_ID,
        b"proj",
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let src = ix.insert(root_id, b"src", EntryFlags::EMPTY.with_dir(true));
    let main = ix.insert(src, b"main.rs", EntryFlags::EMPTY);
    let lib = ix.insert(src, b"lib.rs", EntryFlags::EMPTY);
    let readme = ix.insert(root_id, b"README.md", EntryFlags::EMPTY);
    let notes = ix.insert(root_id, b"Notes.TXT", EntryFlags::EMPTY);
    let future = ix.insert(root_id, b"future.txt", EntryFlags::EMPTY);

    // One minute after the epoch (format 3 has no other way to say "1970" —
    // its `0` already means unknown), a normal date, the next minute, and the
    // top of the representable range. README is left undated on purpose.
    assert_eq!(
        created_minutes_at(UNIX_EPOCH + std::time::Duration::from_secs(60)),
        1,
        "minute 1 is the first minute a v3 store can carry as a real time"
    );
    ix.set_created(main, 1);
    ix.set_created(lib, JAN2);
    ix.set_created(notes, JAN2 + 1);
    ix.set_created(future, CREATED_LATEST_MINUTE);
    Fixture {
        ix,
        root_id,
        main,
        lib,
        readme,
        notes,
        future,
    }
}

#[test]
fn a_dated_v3_store_migrates_in_place_and_keeps_every_created_minute() {
    let tmp = tempfile::tempdir().unwrap();
    // What a 0.1.1-beta1 (format 3) daemon left behind: a version-named store
    // under the state root.
    let state_root = tmp.path().join("state");
    let existing = state_root.join("3");
    std::fs::create_dir_all(&existing).unwrap();
    let tree = tmp.path().join("proj");
    std::fs::create_dir_all(&tree).unwrap();

    let fx = fixture();
    let roots = vec![Root::new(fx.root_id, tree.clone())];
    let root_paths = vec![(fx.root_id, tree.to_string_lossy().into_owned().into_bytes())];
    write_v3(&existing, &fx.ix, &roots);

    // The daemon's own resolution: the existing store's directory, not a
    // sibling named after the format this build writes.
    let resolved = resolve_state_dir(&state_root);
    assert_eq!(resolved, existing, "the store's directory, not a sibling");
    assert_eq!(Store::peek_snapshot_version(&resolved).unwrap(), Some(3));

    let mut store = Store::open(&resolved).unwrap();
    let loaded = store.load_snapshot().unwrap().expect("the v3 snapshot loads");
    let ix2 = loaded.index;

    // --- Nothing lost, nothing shifted ---
    assert_eq!(ix2.len(), fx.ix.len(), "every entry intact");
    assert_eq!(ix2.entries, fx.ix.entries);
    assert_eq!(ix2.names, fx.ix.names);
    for id in 0..fx.ix.len() as u32 {
        assert_eq!(
            ix2.created_min(id),
            fx.ix.created_min(id),
            "entry {id} changed its creation minute on load"
        );
    }
    assert_eq!(ix2.created_min(fx.main), Some(1), "minute 1 survives");
    assert_eq!(ix2.created_min(fx.lib), Some(JAN2));
    assert_eq!(ix2.created_min(fx.notes), Some(JAN2 + 1));
    assert_eq!(
        ix2.created_min(fx.future),
        Some(CREATED_LATEST_MINUTE),
        "the top of the representable range survives, below the unknown sentinel"
    );
    assert_eq!(
        ix2.created_min(fx.readme),
        None,
        "format 3's 0 is unknown, not 1970"
    );

    // --- The queries, before the migration ---
    let dated_before: Vec<String> = ["/src/main.rs", "/src/lib.rs", "/Notes.TXT", "/future.txt"]
        .into_iter()
        .map(|p| format!("{}{p}", tree.display()))
        .collect();
    assert_eq!(
        hits(&ix2, &root_paths, &since(0)),
        dated_before,
        "the wide bound keeps every dated row (in index order) and drops the undated one"
    );
    let jan2_start = i64::from(JAN2) * 60;
    assert_eq!(
        hits(&ix2, &root_paths, &range(jan2_start, jan2_start + 60)),
        vec![format!("{}/src/lib.rs", tree.display())],
        "a one-minute window selects exactly the row stored in that minute"
    );

    // --- Migrate in place, onto the same directory ---
    store.compact(&ix2, &roots).unwrap();
    assert_eq!(
        Store::peek_snapshot_version(&existing).unwrap(),
        Some(fflocate::snapshot::VERSION),
        "the same directory now holds the current format"
    );
    let mut siblings: Vec<String> = std::fs::read_dir(&state_root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    siblings.sort();
    assert_eq!(siblings, vec!["3".to_string()], "no sibling appeared");

    // --- The migrated store reads back identically, and answers identically ---
    let mut store = Store::open(&resolved).unwrap();
    let migrated = store.load_snapshot().unwrap().unwrap().index;
    assert_eq!(migrated.entries, fx.ix.entries);
    for id in 0..fx.ix.len() as u32 {
        assert_eq!(migrated.created_min(id), fx.ix.created_min(id));
    }
    assert_eq!(hits(&migrated, &root_paths, &since(0)), dated_before);
    assert_eq!(
        hits(&migrated, &root_paths, &range(jan2_start, jan2_start + 60)),
        vec![format!("{}/src/lib.rs", tree.display())]
    );

    // --- Re-running the migration is a no-op ---
    store.compact(&migrated, &roots).unwrap();
    let mut store = Store::open(&resolved).unwrap();
    let twice = store.load_snapshot().unwrap().unwrap().index;
    assert_eq!(twice.entries, migrated.entries);
    for id in 0..migrated.len() as u32 {
        assert_eq!(twice.created_min(id), migrated.created_min(id));
    }
    assert_eq!(hits(&twice, &root_paths, &since(0)), dated_before);

    // --- A row written after the upgrade, at the boundary format 3 could not
    // express: minute 0 is a real time now, and must stay one across a reload.
    let epoch_id = twice.next_id();
    let late_id = epoch_id + 1;
    let mut with_new = twice.clone();
    apply_record(
        &mut with_new,
        &Record::Add {
            id: epoch_id,
            parent: fx.root_id,
            name: b"epoch.txt".to_vec(),
            flags: 0,
            created: 0,
        },
    );
    apply_record(
        &mut with_new,
        &Record::Add {
            id: late_id,
            parent: fx.root_id,
            name: b"late.txt".to_vec(),
            flags: 0,
            created: JAN2 + 2,
        },
    );
    store.compact(&with_new, &roots).unwrap();

    let store = Store::open(&resolved).unwrap();
    let now = store.load_snapshot().unwrap().unwrap().index;
    assert_eq!(
        now.created_min(epoch_id),
        Some(0),
        "minute 0 must survive a v4 write and read as a real time"
    );
    assert_eq!(now.created_min(late_id), Some(JAN2 + 2));
    assert_ne!(now.created_min(fx.readme), Some(0), "undated is still unknown");

    // Both the migrated and the new rows answer the same questions.
    let mut expected = dated_before.clone();
    expected.push(format!("{}/epoch.txt", tree.display()));
    expected.push(format!("{}/late.txt", tree.display()));
    assert_eq!(hits(&now, &root_paths, &since(0)), expected);

    // The epoch row is in the first minute and nowhere later: a bound at 60 s
    // is the first real boundary, and the migrated minute-1 row (whose window
    // runs to 1:59) survives it while minute 0 does not.
    let from_minute_1 = hits(&now, &root_paths, &since(60));
    assert!(
        !from_minute_1.iter().any(|p| p.ends_with("/epoch.txt")),
        "minute 0 must not satisfy a bound at 60 s: {from_minute_1:?}"
    );
    assert!(
        from_minute_1.iter().any(|p| p.ends_with("/main.rs")),
        "the migrated minute-1 row must satisfy a bound at 60 s: {from_minute_1:?}"
    );

    // A range that ends at minute JAN2+2 excludes the row stored there, and
    // keeps the migrated row in JAN2 — both sides of the same integer compare.
    assert_eq!(
        hits(&now, &root_paths, &range(jan2_start, jan2_start + 120)),
        vec![
            format!("{}/src/lib.rs", tree.display()),
            format!("{}/Notes.TXT", tree.display())
        ]
    );
}

/// The other half of the boundary: a time outside what a `u32` of minutes can
/// hold is rejected into the documented sentinel, never truncated into a small
/// number that a query would then match.
#[test]
fn out_of_range_timestamps_never_become_plausible_minutes() {
    assert_eq!(
        created_minutes_at(UNIX_EPOCH - std::time::Duration::from_secs(1)),
        CREATED_UNKNOWN,
        "before the epoch is unknown, not a wrapped giant or 1970"
    );
    let beyond = UNIX_EPOCH
        + std::time::Duration::from_secs(u64::from(u32::MAX) * 60 + 60 * 60 * 24 * 365 * 1000);
    assert_eq!(
        created_minutes_at(beyond),
        CREATED_LATEST_MINUTE,
        "past the last representable minute saturates below the unknown sentinel"
    );
    assert_eq!(
        created_minutes_at(UNIX_EPOCH + std::time::Duration::from_secs(u64::MAX / 2)),
        CREATED_LATEST_MINUTE
    );
}
