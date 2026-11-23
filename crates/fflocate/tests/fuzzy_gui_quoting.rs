//! Fuzzy search: what it may and may not do with the query it was given.
//!
//! Three properties, each one a bug that shipped and was found by asking the
//! daemon the same question the GUI asks:
//!
//! * **The GUI's quoting must not cost edits.** The Basic box quotes what the
//!   user types, so fuzzy arrives as `"FreeBiff"`. Taken literally, the two
//!   quote characters used the whole budget of a default query and the answer
//!   was empty; unquoted, the same word found the file. Quoted and bare must
//!   agree at every budget.
//! * **Smart case is a filter, not a distance.** `FreeBiff` has a capital, so
//!   plain search treats it case-sensitively — and fuzzy used to score it that
//!   way too, which (a) charged the `B`/`b` as an edit and (b) asked the trigram
//!   generator for trigrams no name contains (`bif`, `iff`), so it answered "no
//!   candidates" at a positive budget. `freebiff` found the file and `FreeBiff`
//!   did not. Both must find it, at the same distance.
//! * **The accelerator may never narrow the answer.** With a budget too wide for
//!   the trigram bound, or a trigram the index does not hold, the honest answer
//!   is "scan everything" — never "no matches".
//!
//! The fixture index is a real `Shared` with a real tree on disk, so hits carry
//! real paths and the whole `handle_request` path runs, exactly as it does for
//! a socket client.

use std::path::PathBuf;

use fflocate::index::{EntryFlags, Index, ROOT_ID};
use fflocate::proto::{Request, Response};
use fflocate::roots::Root;
use fflocate::applier::Shared;
use fflocate::server::{handle_request, ServerCtx};
use fflocate::sync::SyncRegistry;

/// A tree with a `FreeBuff` directory, its contents, and a couple of names to
/// keep the trigram index honest.
fn fixture() -> (ServerCtx, tempfile::TempDir) {
    let d = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(d.path().join("FreeBuff")).expect("dir");
    std::fs::write(d.path().join("FreeBuff/notes.txt"), b"notes").expect("file");
    std::fs::write(d.path().join("freebuff.conf"), b"conf").expect("file");
    std::fs::write(d.path().join("README.md"), b"readme").expect("file");

    let mut ix = Index::new();
    let basename = d.path().file_name().unwrap().to_string_lossy().to_string();
    let rid = ix.insert(
        ROOT_ID,
        basename.as_bytes(),
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    let dir = ix.insert(rid, b"FreeBuff", EntryFlags::EMPTY.with_dir(true));
    ix.insert(dir, b"notes.txt", EntryFlags::EMPTY);
    ix.insert(rid, b"freebuff.conf", EntryFlags::EMPTY);
    ix.insert(rid, b"README.md", EntryFlags::EMPTY);
    let mut root = Root::new(0, d.path().to_path_buf());
    root.caps.writable = true;
    let shared = Shared::new(ix, vec![root]);
    let ctx = ServerCtx {
        shared,
        sync: SyncRegistry::new(),
        backend: "fake".into(),
        control: None,
        caller: None,
    };
    (ctx, d)
}

/// Ask a query and return the hit paths (the last component only, so the
/// tempdir prefix does not get in the way of readable assertions).
fn ask(ctx: &ServerCtx, q: &str, fuzzy: bool, max_distance: Option<u32>) -> Vec<String> {
    let out = handle_request(
        ctx,
        &Request::Query {
            q: q.into(),
            limit: 0,
            json: false,
            count: false,
            expression: None,
            glob: None,
            glob_includedotfiles: None,
            glob_noescape: None,
            suffix: None,
            path: None,
            relative_root: None,
            case_sensitive: None,
            fuzzy,
            max_distance,
            progress: false,
        },
    );
    let mut hits: Vec<String> = out
        .iter()
        .filter_map(|r| match r {
            Response::Hit { path, .. } => Some(
                PathBuf::from(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone()),
            ),
            Response::Error { message } => panic!("daemon error for {q:?}: {message}"),
            _ => None,
        })
        .collect();
    hits.sort();
    hits
}

/// The budgets a caller actually uses: the daemon default (`None`), an exact
/// one, and a few that a person would try while hunting for a typo.
const BUDGETS: [Option<u32>; 5] = [None, Some(0), Some(1), Some(2), Some(3)];

#[test]
fn the_gui_quoting_costs_no_edits() {
    let (ctx, _d) = fixture();
    for raw in ["FreeBuff", "FreeBiff", "freebuff", "Free", "Biff"] {
        for k in BUDGETS {
            let quoted = ask(&ctx, &format!("\"{raw}\""), true, k);
            let bare = ask(&ctx, raw, true, k);
            assert_eq!(
                quoted, bare,
                "quoting {raw:?} at k={k:?} changed the fuzzy answer: {quoted:?} vs {bare:?}"
            );
        }
    }
}

#[test]
fn smart_case_is_a_filter_not_a_distance() {
    let (ctx, _d) = fixture();
    // The same letters, in every case a person types them: fuzzy must agree.
    let folded = ask(&ctx, "freebiff", true, None);
    assert!(
        folded.contains(&"FreeBuff".to_string()),
        "even the lower-case spelling must find FreeBuff at the default budget, got {folded:?}"
    );
    for spelling in ["FreeBiff", "\"FreeBiff\"", "FREEBiFF", "freeBiff"] {
        assert_eq!(
            ask(&ctx, spelling, true, None),
            folded,
            "{spelling:?} must fuzzy-match exactly what `freebiff` does"
        );
    }
    // The same name, spelled exactly, is found too — this is the bug that made
    // `freebuff` match and `FreeBuff` not.
    for spelling in ["FreeBuff", "freebuff", "\"FreeBuff\""] {
        assert!(
            ask(&ctx, spelling, true, None).contains(&"FreeBuff".to_string()),
            "{spelling:?} must be found by a fuzzy query"
        );
    }
    // At distance 0 nothing is relaxed: an exact name is found, a typo is not.
    // (The smart-case relaxation only widens the *filter*; the distance still
    // decides, so k=0 is exact.)
    assert!(ask(&ctx, "FreeBuff", true, Some(0)).contains(&"FreeBuff".to_string()));
    assert!(
        !ask(&ctx, "FreeBiff", true, Some(0)).contains(&"FreeBuff".to_string()),
        "k=0 must not approximate"
    );
}

#[test]
fn a_plain_query_is_unaffected() {
    let (ctx, _d) = fixture();
    // Non-fuzzy search keeps smart case: `FreeBuff` is exact-and-sensitive, so
    // a typo finds nothing and the lower-case spelling is case-insensitive.
    assert!(ask(&ctx, "FreeBuff", false, None).contains(&"FreeBuff".to_string()));
    assert!(ask(&ctx, "freebuff", false, None).contains(&"FreeBuff".to_string()));
    assert!(
        ask(&ctx, "FreeBiff", false, None).is_empty(),
        "plain search must not approximate"
    );
    assert!(
        ask(&ctx, "\"FreeBuff\"", false, None).contains(&"FreeBuff".to_string()),
        "plain search already strips the GUI's quoting"
    );
}

#[test]
fn the_accelerator_never_narrows_the_answer() {
    let (ctx, _d) = fixture();
    // Every budget, including ones wide enough that the trigram bound says
    // nothing, and needles whose trigrams no name holds: the answer must still
    // contain everything the folded needle matches.
    for k in [Some(2), Some(5), Some(50)] {
        for raw in ["FreeBiff", "zzzFreeBu", "FreeB", "F"] {
            let hits = ask(&ctx, raw, true, k);
            let folded = raw.to_ascii_lowercase();
            let expected = ask(&ctx, &folded, true, k);
            assert_eq!(
                hits, expected,
                "{raw:?} at k={k:?} disagreed with its folded spelling"
            );
        }
    }
}
