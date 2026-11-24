//! The foldout's controls, the request the GUI builds from them, and the
//! daemon's answer to it: `FreeBiff` must find `FreeBuff` with the foldout in
//! the state the app ships.
//!
//! Why this exists next to `fflocate`'s `fuzzy_gui_quoting` and the GUI's
//! `distance_row_*` tests: those prove the daemon's fuzzy answer and the row's
//! own buttons respectively, and neither notices a widget that reports the
//! wrong *value* into `SearchOptions`. That is the failure a user saw — an
//! empty list on macOS for a typo the CLI found — so this drives the whole
//! chain from the boxes a user touches:
//!
//! 1. the foldout's `Fuzzy matching` checkbox and the max-distance row, built
//!    the way `main.rs` builds them (Fuzzy unticked, the row seeded with
//!    `DEFAULT_MAX_DISTANCE`);
//! 2. the values read back out of those widgets;
//! 3. [`ff_gui::gather_options`] + [`ff_gui::effective_query`] +
//!    `DaemonClient::query_request` — the request a search sends;
//! 4. a real `ServerCtx` answering it, through `handle_request`, exactly as a
//!    socket client's query is answered.
//!
//! Everything but the socket is production code, and the widgets are the
//! foldout's real ones on whichever backend this build has (`zork` headless in
//! CI, GTK under Xvfb). The failure modes are pinned as failures too: Fuzzy
//! unticked sends no fuzzy query, and a budget of 0 cannot match a two-edit
//! typo — so a future regression reports *which* one came back.

use fflocate::applier::Shared;
use fflocate::index::{EntryFlags, Index, ROOT_ID};
use fflocate::proto::{Request, Response};
use fflocate::roots::Root;
use fflocate::server::{handle_request, ServerCtx};
use fflocate::sync::SyncRegistry;
use ff_gui::{
    effective_query, gather_options, DaemonClient, SearchOptions, DEFAULT_MAX_DISTANCE,
    DEFAULT_MAX_DISTANCE_VALUE,
};

/// Whether the foldout's widgets cannot be built in this run.
///
/// The `zork` build is headless and is the one CI runs here; the default build
/// on Linux is GTK, which aborts inside the toolkit when there is no display,
/// so the `cargo test -p ff-gui` step (no Xvfb) has to skip instead.
fn no_display() -> bool {
    if cfg!(all(feature = "gtk", target_os = "linux", not(feature = "zork"))) {
        return std::env::var_os("DISPLAY").is_none()
            && std::env::var_os("WAYLAND_DISPLAY").is_none();
    }
    false
}

/// A daemon whose only interesting entry is the name the typo is aimed at.
fn daemon_with(dir: &std::path::Path) -> ServerCtx {
    std::fs::create_dir_all(dir.join("FreeBuff")).expect("create FreeBuff");
    let mut ix = Index::new();
    let base = dir
        .file_name()
        .expect("tempdir has a name")
        .to_string_lossy()
        .to_string();
    let rid = ix.insert(
        ROOT_ID,
        base.as_bytes(),
        EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
    );
    ix.insert(rid, b"FreeBuff", EntryFlags::EMPTY.with_dir(true));
    let mut root = Root::new(0, dir.to_path_buf());
    root.caps.writable = true;
    ServerCtx {
        shared: Shared::new(ix, vec![root]),
        sync: SyncRegistry::new(),
        backend: "fake".into(),
        control: None,
        caller: None,
    }
}

/// The request a search sends for Basic-box text `typed` and these options —
/// the same three calls `main.rs`'s `build_request` makes.
fn request_for(typed: &str, opts: &SearchOptions) -> Request {
    let q = effective_query(typed, "").expect("the Basic box can quote this text");
    DaemonClient::query_request(&q, 0, opts)
}

/// The paths the daemon answers with, or the error it sent.
fn ask(ctx: &ServerCtx, req: &Request) -> Result<Vec<String>, String> {
    let mut hits = Vec::new();
    for r in handle_request(ctx, req) {
        match r {
            Response::Hit { path, .. } => hits.push(path),
            Response::Error { message } => return Err(message),
            _ => {}
        }
    }
    Ok(hits)
}

#[test]
fn the_foldout_as_built_finds_freebuff_for_freebiff() {
    if no_display() {
        println!("foldout fuzzy request: skipped (no display)");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = daemon_with(dir.path());

    // --- The foldout as the app builds it ---
    let app = rswidgets::prelude::App::init().expect("a widget backend");
    let fuzzy = app.create_checkbutton("Fuzzy matching").expect("checkbox");
    fuzzy.set_active(false);
    let distance = ff_gui::distance_ui::build(&app, DEFAULT_MAX_DISTANCE).expect("distance row");
    distance.set_controls_sensitive(false);

    // Exactly the values `gather_options` is handed in `main.rs`: each control
    // read back, the distance as text.
    let read = |fuzzy_on: bool| {
        let text = distance.field.get_text().unwrap_or_default();
        gather_options(false, fuzzy_on, false, &text)
    };

    let launched = read(fuzzy.is_active());
    assert!(
        !launched.fuzzy,
        "the foldout's Fuzzy box starts ticked; the app ships it unticked"
    );

    // What a user does: tick Fuzzy, then search the typo.
    //
    // A backend whose widgets were never built is indistinguishable from an
    // unticked box at the assertion below, so say what the control reads at
    // each step: on macOS this is the difference between "the app's request is
    // wrong" and "nothing was built to tick".
    println!(
        "foldout fuzzy request: at launch fuzzy {} (box reads {}), distance {:?}",
        launched.fuzzy,
        fuzzy.is_active(),
        distance.field.get_text()
    );
    fuzzy.set_active(true);
    let opts = read(fuzzy.is_active());
    println!(
        "foldout fuzzy request: after set_active(true) the box reads {} -> options fuzzy {} max distance {:?}",
        fuzzy.is_active(),
        opts.fuzzy,
        opts.max_distance
    );
    println!(
        "foldout: fuzzy {} max distance {:?} -> {:?}",
        opts.fuzzy, opts.max_distance, request_for("FreeBiff", &opts)
    );
    assert!(opts.fuzzy, "a ticked Fuzzy box must send a fuzzy query");
    assert_eq!(
        opts.max_distance,
        Some(DEFAULT_MAX_DISTANCE_VALUE),
        "the row the app builds must reach the request as its default distance, not as `None`"
    );

    let hits = ask(&ctx, &request_for("FreeBiff", &opts)).expect("no error");
    println!("foldout: FreeBiff, Fuzzy, k={:?} -> {hits:?}", opts.max_distance);
    assert!(
        hits.iter().any(|p| p.ends_with("/FreeBuff")),
        "the foldout's own widget values did not find FreeBuff for FreeBiff: {hits:?}"
    );

    // --- The two ways the same window comes back empty, pinned as empty ---
    //
    // Fuzzy off is an exact search: a typo finds nothing. A budget of 0 relaxes
    // nothing: two edits are out of reach. Both are *expected* to be empty, so
    // a future regression that lands here is named, not mistaken for a fix.
    let exact = ask(&ctx, &request_for("FreeBiff", &read(false))).expect("no error");
    println!("foldout: Fuzzy unticked -> {exact:?}");
    assert!(
        exact.is_empty(),
        "an exact search for FreeBiff found {exact:?}: this test no longer shows that \
         unticking Fuzzy is the empty list"
    );

    let zero = SearchOptions {
        fuzzy: true,
        max_distance: Some(0),
        ..Default::default()
    };
    let no_edits = ask(&ctx, &request_for("FreeBiff", &zero)).expect("no error");
    println!("foldout: budget 0 -> {no_edits:?}");
    assert!(
        no_edits.is_empty(),
        "a budget of 0 matched {no_edits:?}: at 0 nothing is relaxed, so this test no longer \
         shows that a field reading 0 is the empty list"
    );
}

#[test]
fn a_wider_budget_still_finds_the_typo_through_the_request() {
    if no_display() {
        println!("foldout fuzzy request: skipped (no display)");
        return;
    }
    // The row's buttons are how a macOS user changes the budget without a
    // keyboard (AppKit's entry may take no keystrokes inside a stack view and
    // wires no wheel), so the value they produce must also survive the request
    // path — not just step the text. `step_max_distance` is the buttons' own
    // arithmetic, so this is what a click writes into the field.
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = daemon_with(dir.path());
    let stepped = ff_gui::step_max_distance(DEFAULT_MAX_DISTANCE, ff_gui::WheelStep::Up);
    let opts = gather_options(false, true, false, &stepped);
    println!("foldout: stepped {stepped:?} -> {:?}", opts.max_distance);
    assert_eq!(
        opts.max_distance,
        Some(DEFAULT_MAX_DISTANCE_VALUE + 1),
        "`+` must widen the budget the request carries"
    );
    let hits = ask(&ctx, &request_for("FreeBiff", &opts)).expect("no error");
    assert!(
        hits.iter().any(|p| p.ends_with("/FreeBuff")),
        "a widened budget did not find FreeBuff: {hits:?}"
    );
}
