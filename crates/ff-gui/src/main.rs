//! `ff-gui` — search the fflocate daemon through a rustxWidgets window.
//!
//! Layout:
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │ File  Search  View  Help                          │
//! │ [ query box                              ][Search]│
//! ├──────────────────────────────────────────────────┤
//! │ /home/me/src/main.rs                              │
//! │ /home/me/src/lib.rs                               │
//! │ ...                                               │
//! ├──────────────────────────────────────────────────┤
//! │ 42 matches                       [status line]     │
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! Every menubar item reuses a handler something else already owns (the
//! search trigger, the option widgets, the row actions, the daemon client),
//! so the bar cannot drift from the rest of the UI. See `menubar_model` for
//! the bar's contents as testable data.
//!
//! The backend is rustxWidgets' `zork` model, which is headless: the same widget
//! calls work in a terminal (REPL), over ssh, or against GTK when that backend is
//! available. The daemon does all the indexing and matching; this only collects
//! input and renders rows.

use rswidgets::prelude::*;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ff_gui::{
    box_change_clears, containing_dir, context_menu_target, effective_query, encode_request,
    extension_sort_key, render_count, rows_from_paths, scan_percent, search_label, searching_status_line,
    step_max_distance, streaming_status_line, DaemonClient, GuiError,
    MenuAction, PendingRequest, Progress, RowAction, SearchGate, SearchOptions, SearchOutcome,
    StartupPoll, StartupProbe,
    WheelStep, WireResponse, ADVANCED_SYNTAX_HELP, DEFAULT_MAX_DISTANCE,
};

/// A search's result.
type SearchResult = Result<SearchOutcome, GuiError>;

/// Applies a result to the widgets. Main-thread only: the widgets are not `Send`.
type RenderFn = Rc<dyn Fn(SearchResult)>;

/// Which backend this binary was compiled with.
///
/// `ff --gui` locates `ff-gui` next to itself, and `cargo run --bin ff` builds
/// *only* `ff` -- so a stale sibling built with different features (e.g.
/// `--no-default-features --features zork`) can be launched instead. Naming the
/// backend makes that mismatch visible rather than mysterious.
const BACKEND: &str = if cfg!(feature = "zork") {
    "zork"
} else if cfg!(target_os = "macos") {
    "appkit"
} else if cfg!(windows) {
    // The `gtk` cargo feature is on everywhere (it is inert off Linux), so
    // the feature test below would mislabel a Windows build. The target
    // decides: on Windows rswidgets always drives the NWG backend.
    "nwg"
} else if cfg!(feature = "gtk") {
    "gtk"
} else {
    "unknown"
};

/// Default window size, big enough for the three columns to be readable.
const DEFAULT_WINDOW_W: i32 = 1000;
const DEFAULT_WINDOW_H: i32 = 700;
/// Width the window maps at on GTK: 1 px narrower than the default. The sortable
/// headers grow a little in the first frames (sort-arrow space arriving with
/// the theme), widening the results columns ~38 px past the viewport with no
/// compensating redistribution — queuing a resize does not help (GTK skips
/// no-change reallocations) and only a real allocation change or row arrival
/// redistributes. So the window starts 1 px narrow and steps out to the
/// default once settled (see below): that step *is* a real allocation change,
/// and the fixed-sized text columns snap to the viewport exactly. Later
/// resizes and row changes redistribute by themselves. An imperceptible
/// difference either way; the default above stays the documented size.
/// GTK-only: other backends map straight at the default (their headers do not
/// grow late, and nothing there steps the size back out).
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
const INITIAL_WINDOW_W: i32 = DEFAULT_WINDOW_W - 1;
/// How long after present the settle step runs. The header growth lands at
/// ~30 ms here; this has an order of magnitude of margin. A machine slower
/// than that keeps the bar until its first search or resize (which
/// redistribute by themselves) — accepted, and documented there.
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
const SETTLE_RESIZE_MS: u32 = 300;

/// How many rows to render at most (the daemon may match millions).
const MAX_ROWS: usize = 200;
/// How many hits to ask the daemon for (rows plus a little slack for the
/// "of N matches" line).
const FETCH_LIMIT: usize = MAX_ROWS + 1;
/// How long startup waits for a freshly spawned daemon's socket. The daemon
/// probes mounts and starts threads before binding, so the socket typically
/// appears a moment after the child exists; past this the status just says
/// it is still starting.
const SPAWN_STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

fn main() {
    if let Err(e) = run() {
        eprintln!("ff-gui: {e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("ff-gui {} (backend: {BACKEND})", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let app = App::init().map_err(|e| GuiError::Backend(e.to_string()))?;
    let socket_path = DaemonClient::new().socket().to_path_buf();

    let win = app.create_window()?;
    win.set_title("fflocate");
    // Roomy enough that all three columns are visible on launch. On GTK it
    // starts 1 px narrow (see INITIAL_WINDOW_W) and steps out once settled,
    // below; elsewhere it maps straight at the default.
    #[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
    win.set_default_size(INITIAL_WINDOW_W, DEFAULT_WINDOW_H);
    #[cfg(not(all(feature = "gtk", target_os = "linux", not(feature = "zork"))))]
    win.set_default_size(DEFAULT_WINDOW_W, DEFAULT_WINDOW_H);

    let query_row = app.create_box(Orientation::Horizontal, 2)?;
    let query = app.create_entry()?;
    query.set_text("");
    // The Basic box is literal-only (quoted before sending): say so, so a
    // user reaching for ext:/regex: learns where that syntax lives.
    query.set_placeholder_text("filename — literal, no syntax (ext:/regex: go in Advanced search)");
    expand_query_entry(&query);
    let search = app.create_button("Search")?;
    query_row.append(&query);
    query_row.append(&search);
    query_row.set_child_hexpand(&query, true);
    let status = app.create_label("type a query and press Search")?;
    // The results are a three-column list: icon, name, path. On GTK this is a
    // real GtkTreeView (so the columns align and the icon column shows a themed
    // icon); the display-free fallbacks render the same three columns as text.
    // (title, is_icon, sortable): every header sorts when clicked — Name and
    // Path by themselves, the icon column by file extension (see
    // `extension_sort_key`; a pixbuf has no order of its own).
    let results = app.create_treeview(&[
        ("", true, true),
        ("Name", false, true),
        ("Path", false, true),
    ])?;
    // Column index of the full path, which Copy / Open / drag act on.
    const PATH_COLUMN: usize = 2;

    // On GTK the tree view needs its own scrolled container to scroll, and that
    // container must expand with the window — otherwise the results stay at
    // their minimum height while the window grows.
    #[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
    let results_scroll = {
        let sw = results.into_scrolled()?;
        sw.set_vexpand(true);
        sw.set_hexpand(true);
        sw
    };

    // The advanced-search foldout: a native disclosure holding a free-form
    // query field (with the full syntax) plus real controls. Every control maps
    // to a daemon field — Case sensitive (case_sensitive), Fuzzy (fuzzy), Count
    // only (count), and the fuzzy maximum edit distance (max_distance).
    let advanced = app.create_expander("Advanced search")?;
    let advanced_query = app.create_entry()?;
    advanced_query.set_text("");
    // The syntax reminder lives inside the field as grey hint text: visible
    // while the field is empty and unfocused, gone as soon as it holds text
    // or is selected for typing.
    advanced_query.set_placeholder_text(ADVANCED_SYNTAX_HELP);

    let opt_case = app.create_checkbutton("Case sensitive")?;
    opt_case.set_active(false);
    let opt_fuzzy = app.create_checkbutton("Fuzzy matching")?;
    opt_fuzzy.set_active(false);
    let opt_count = app.create_checkbutton("Count only (skip the list)")?;
    opt_count.set_active(false);

    // The max edit distance only means anything with Fuzzy on, so it is
    // disabled until then rather than silently ignored.
    let opt_distance = app.create_entry()?;
    opt_distance.set_text(DEFAULT_MAX_DISTANCE);
    let distance_row = app.create_box(Orientation::Horizontal, 2)?;
    let distance_label = app.create_label("max distance:")?;
    distance_row.append(&distance_label);
    distance_row.append(&opt_distance);

    let advanced_box = app.create_box(Orientation::Vertical, 2)?;
    advanced_box.append(&advanced_query);
    advanced_box.append(&opt_case);
    advanced_box.append(&opt_fuzzy);
    advanced_box.append(&distance_row);
    advanced_box.append(&opt_count);
    advanced.set_child(&advanced_box);
    advanced.set_expanded(false);

    // Fuzzy off => the distance field is inert; grey it out so it is clear the
    // value has no effect rather than being silently ignored.
    {
        opt_distance.set_sensitive(false);
        distance_label.set_sensitive(false);
        let d = opt_distance.clone();
        let l = distance_label.clone();
        let f = opt_fuzzy.clone();
        opt_fuzzy
            .connect_toggled(move || {
                let on = f.is_active();
                d.set_sensitive(on);
                l.set_sensitive(on);
            })
            .ok();
    }

    // Wheel up/down over the max-distance field steps it by one (up
    // increments, like a spin button) — but only while the field is focused,
    // so scrolling the window or another control never edits it by accident.
    // Focus is tracked with the existing focus in/out events (returning 0
    // lets the default handler run, so the field still actually focuses).
    {
        let focused: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        {
            let focused = focused.clone();
            opt_distance
                .connect_focus_in_event(move |_| {
                    focused.set(true);
                    0
                })
                .ok();
        }
        {
            let focused = focused.clone();
            opt_distance
                .connect_focus_out_event(move |_| {
                    focused.set(false);
                    0
                })
                .ok();
        }
        let field = opt_distance.clone();
        opt_distance
            .connect_scroll_event(move |dir| {
                if !focused.get() {
                    return false;
                }
                let step = match dir {
                    ScrollDirection::Up => WheelStep::Up,
                    ScrollDirection::Down => WheelStep::Down,
                    _ => return false,
                };
                let current = field.get_text().unwrap_or_default();
                field.set_text(&step_max_distance(&current, step));
                // Handled: keep the tick from also scrolling a parent.
                true
            })
            .ok();
    }

    // The rows currently on screen, shared between the Search handler (which
    // fills them) and the right-click handler (which picks one). Also holds the
    // live context menu: GTK does not own a menu popped up this way, so we must
    // keep it alive until the user dismisses it.
    let current: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let live_menu: Rc<RefCell<Option<ContextMenu>>> = Rc::new(RefCell::new(None));
    // The row a drag would carry. Set on left-press (the drag's start), read
    // when a drop target asks for the data: `drag-data-get` carries no start
    // position of its own.
    let drag_row: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // The daemon check is deferred past first paint: it rides the timer pump
    // (see `StartupProbe`), so the window appears immediately even when the
    // daemon is missing and a fresh spawn takes seconds to bind. Until the
    // probe settles, the status says what is happening rather than claiming a
    // connection that has not been verified — an empty list from an absent
    // daemon and an empty list from a real "no matches" must not look the same.
    // Spawning runs at most once per process: later searches just report an
    // unreachable daemon.
    status.set_text("connecting to fflocated…");
    let startup = Rc::new(RefCell::new(StartupProbe::new(
        socket_path,
        SPAWN_STARTUP_GRACE,
        true,
    )));

    // The search. Two halves, deliberately separated:
    //
    // * `fetch` runs the query and returns a plain result;
    // * `render` applies that result to the widgets.
    //
    // Splitting them is what lets the debounce reuse the same query code as the
    // Search button and the Return key, so the three cannot drift apart. (It also
    // means the expensive half *could* move to a worker thread, but it is 0.00 s
    // against a warm daemon, so it does not: see the note at the as-you-type code.)
    // Turn the current widgets into the request line. This is the
    // *only* part of a search that needs the widgets, so it is the only part that
    // must run on the main thread — the I/O below is stepped from the pump.
    let build_request: Rc<dyn Fn() -> Result<String, GuiError>> = {
        let query = query.clone();
        let advanced_query = advanced_query.clone();
        let opt_case = opt_case.clone();
        let opt_fuzzy = opt_fuzzy.clone();
        let opt_count = opt_count.clone();
        let opt_distance = opt_distance.clone();
        Rc::new(move || {
            let quick = query.get_text().unwrap_or_default();
            let adv = advanced_query.get_text().unwrap_or_default();
            let q = effective_query(&quick, &adv);
            let q = q.ok_or_else(|| {
                GuiError::Protocol(
                    "that name contains a `\"`, which the Basic box cannot quote — \
                     type it in the Advanced search box instead"
                        .to_string(),
                )
            })?;
            let opts = gather_options(&opt_case, &opt_fuzzy, &opt_count, &opt_distance);
            let req = DaemonClient::query_request(&q, FETCH_LIMIT, &opts);
            encode_request(&req)
        })
    };

    let render: RenderFn = {
        let status = status.clone();
        let results = results.clone();
        let current = current.clone();
        let opt_count = opt_count.clone();
        Rc::new(move |outcome| match outcome {
            Ok(outcome) => {
                let count_only = opt_count.is_active();
                let line = outcome.status_line_opts(outcome.paths.len().min(MAX_ROWS), count_only);
                status.set_text(&line);
                if count_only {
                    // No paths come back: show the number, not an empty list that
                    // would read as "no matches".
                    results.set_rows(&[vec![
                        CellValue::Text(String::new()),
                        CellValue::Text(render_count(outcome.total)),
                        CellValue::Text(String::new()),
                    ]]);
                    current.borrow_mut().clear();
                } else {
                    let shown: Vec<String> = outcome.paths.iter().take(MAX_ROWS).cloned().collect();
                    results.set_rows(&rows_to_cells(&shown));
                    // Record only the rows actually shown.
                    *current.borrow_mut() = shown;
                }
            }
            Err(e) => {
                status.set_text(&format!("{e}"));
                results.set_rows(&[]);
                current.borrow_mut().clear();
            }
        })
    };

    // The same closure backs the Search button, the Return key and the as-you-type
    // debounce, so the three cannot drift apart. It does not run the query itself —
    // it asks the pump to start one *now* (bypassing the debounce), so even an
    // explicit click cannot block the window on a slow daemon.
    let run_search: Rc<RefCell<Box<dyn FnMut()>>> = Rc::new(RefCell::new(Box::new(|| {})));

    {
        let run_search = run_search.clone();
        search.on_click(move || {
            if let Ok(mut f) = run_search.try_borrow_mut() {
                f();
            }
        })?;
    }

    // ---------------------------------------------------------------------
    // Search as you type, over a **non-blocking** socket.
    //
    // Each keystroke arms a 150 ms deadline; a 30 ms timer (`App::timeout_add`, a
    // primitive added to the vendored rustxWidgets) issues the query once typing
    // pauses, so a burst of keystrokes collapses to one query.
    //
    // The socket is set non-blocking and the request is *stepped* from the pump
    // (`PendingRequest::advance`), never awaited. That is what keeps the window
    // responsive: a daemon that is slow, or wedged entirely, leaves the pump
    // returning `WouldBlock` each tick rather than sitting in `read`. A blocking
    // read — even one with a timeout — would freeze the window for the duration;
    // this does not freeze it at all.
    //
    // The reply is multi-record (Meta, Hit*, terminal), so a request spans many
    // ticks; `PendingRequest` remembers the partial line between them.
    //
    // Only two things must run on the main thread, because the widgets cannot cross
    // a thread boundary: reading the query out of the boxes, and writing the rows
    // back. Everything between is now pure I/O and could run anywhere.
    // ---------------------------------------------------------------------
    {
        let due_at: Rc<RefCell<Option<std::time::Instant>>> = Rc::new(RefCell::new(None));
        // The gate is what stops a slow query from being overwritten by a newer one
        // and keeps a burst of keystrokes from queuing a query each. It is plain
        // logic, tested in `ff_gui::asyou_type_tests`.
        let gate = Rc::new(RefCell::new(SearchGate::new()));

        const DEBOUNCE_MS: u64 = 150;
        const PUMP_MS: u32 = 30;

        let arm = {
            let due_at = due_at.clone();
            move || {
                *due_at.borrow_mut() =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(DEBOUNCE_MS));
            }
        };

        // Set by the Search button / Return: "start the next request without waiting
        // for the debounce". They share the pump rather than making their own request
        // so that every path through the GUI does the I/O the same non-blocking way.
        let start_now: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
        {
            let start_now = start_now.clone();
            *run_search.borrow_mut() = Box::new(move || *start_now.borrow_mut() = true);
        }

        let pump = {
            let due_at = due_at.clone();
            let gate = gate.clone();
            let render = render.clone();
            let build_request = build_request.clone();
            let startup = startup.clone();
            let startup_status = status.clone();
            let quick_box = query.clone();
            let adv_box = advanced_query.clone();
            // Partial renders paint rows directly (the `render` closure owns
            // its clones), so the pump holds its own.
            let partial_results = results.clone();
            let partial_current = current.clone();
            // Last lag the daemon reported, shared across renders: clearing
            // the query box must not make a behind daemon look caught up
            // (the stale-roots lesson: loudly stale beats silently wrong).
            let last_lag: Rc<RefCell<Option<u64>>> = Rc::new(RefCell::new(None));
            // The request currently in flight, if any: (gate seq, request,
            // when it was issued, display label for the status line). Advancing
            // it is what the pump does: it never blocks, so a slow or wedged
            // daemon leaves the window fully responsive (the pump simply keeps
            // returning `true`). While it is out, the status line says
            // "searching…" with elapsed time (see below) — a cold daemon's
            // first query can take seconds (crawl + trigram build), and that
            // must read as progress, not a frozen window.
            // (gate seq, request, when issued, display label, hits already
            // painted). `rendered` drives partial renders: each tick paints only
            // the Hits that arrived since the last paint, so the first page
            // fills while the daemon is still counting the total.
            // (gate seq, request, when issued, display label, hits already
            // painted, scan-fraction percent watermark). The watermark folds
            // every `Progress` record in and never rewinds — a rescanned
            // denominator could shrink mid-reply, and the line must not jump
            // backwards. Dropped with the ticket on supersede, like
            // `rendered`.
            // ...(gate seq, request, issued, label, rendered, percent
            // watermark, lag watermark, phase watermark). The phase watermark
            // is the last `Meta` phase/crawl triple seen on this reply — the
            // daemon's own "what am I doing", which the in-flight line renders
            // verbatim instead of the client inventing a cause.
            let pending: Rc<
                RefCell<
                    Option<(
                        u64,
                        PendingRequest,
                        std::time::Instant,
                        String,
                        usize,
                        Option<u8>,
                        Option<u64>,
                        Option<fflocate::health::Phase>,
                        Option<u64>,
                        Option<u64>,
                    )>,
                >,
            > = Rc::new(RefCell::new(None));
            App::timeout_add(
                PUMP_MS,
                Box::new(move || {
                    // 0. Settle the deferred startup check first: until it reports
                    //    Done the status line is provisional ("connecting…"), and
                    //    searches still work — they just report their own errors.
                    if !startup.borrow().is_done() {
                        match startup.borrow_mut().poll() {
                            StartupPoll::Pending => {}
                            StartupPoll::Note(line) | StartupPoll::Done(line) => {
                                startup_status.set_text(&line)
                            }
                        }
                    }
                    // 1. Advance a request in flight. `WouldBlock` is normal, not an
                    //    error: it means "come back next tick", which is exactly what
                    //    keeps the loop live.
                    let mut finished: Option<(u64, Result<Vec<WireResponse>, GuiError>)> = None;
                    let mut finished_started: Option<std::time::Instant> = None;
                    if let Some((seq, p, started, _, _, _, _, _, _, _)) = pending.borrow_mut().as_mut() {
                        let seq = *seq;
                        let started_at = *started;
                        match p.advance() {
                            Ok(Progress::Finished) => {
                                finished = Some((seq, Ok(p.take_records())));
                                finished_started = Some(started_at);
                            }
                            Ok(_) => {}
                            Err(e) => {
                                finished = Some((seq, Err(e)));
                                finished_started = Some(started_at);
                            }
                        }
                    }
                    if finished.is_some() {
                        *pending.borrow_mut() = None;
                    }
                    if let Some((seq, result)) = finished {
                        // Wall time replaces `Meta.elapsed_ms` for the slow-query
                        // suffix: Meta now goes out before the scan (elapsed ≈
                        // time-to-first-byte), so the client's own clock is the
                        // honest total. Partial rows are already on screen; this
                        // final render adds the exact total.
                        let wall = finished_started.map(|t| t.elapsed());
                        let outcome = result.and_then(|recs| {
                            DaemonClient::new().outcome_from_records(&recs).map(|mut o| {
                                if let Some(w) = wall {
                                    o.elapsed_ms = Some(w.as_secs_f64() * 1000.0);
                                }
                                // Remember this reply's lag so a later
                                // list-clearing render can still say the
                                // daemon is behind. `None` (caught up) clears
                                // the memory — the daemon is the authority on
                                // its own backlog, and it just said it has none.
                                *last_lag.borrow_mut() = o.lag_ms;
                                o
                            })
                        });
                        if let Some(outcome) = gate.borrow_mut().finish(seq, outcome) {
                            render(outcome);
                        }
                    }

                    // 1b. Streaming progress: the daemon flushes Meta+Hits as
                    //     found, so each tick paints newly arrived Hits and the
                    //     first page fills while the total is still being
                    //     counted. Before any Hit arrives the status says what
                    //     is happening and for how long (a slow first query —
                    //     cold daemon, trigram build on demand — must read as
                    //     progress, not a frozen window).
                    //
                    //     Gate-checked: a superseded ticket's partials never
                    //     paint (`is_current`), and `finish` still retires the
                    //     ticket — partials cannot overwrite a newer answer.
                    {
                        let mut pending_ref = pending.borrow_mut();
                        if let Some((
                            seq,
                            p,
                            started,
                            label,
                            rendered,
                            watermark,
                            lag,
                            phase_wm,
                            crawl_entries_wm,
                            crawl_dirs_wm,
                        )) = pending_ref.as_mut()
                        {
                            let seq = *seq;
                            let started_at = *started;
                            let hits: Vec<String> = p
                                .records()
                                .iter()
                                .filter_map(|r| match r {
                                    WireResponse::Hit { path, .. } => Some(path.clone()),
                                    _ => None,
                                })
                                .collect();
                            // Fold every Progress record into the watermark:
                            // monotone, never rewinds.
                            for r in p.records() {
                                if let WireResponse::Progress { scanned, total } = r {
                                    if let Some(pct) = scan_percent(*scanned, *total) {
                                        let w = watermark.get_or_insert(0);
                                        *w = (*w).max(pct);
                                    }
                                }
                                // The daemon's lag rides on Meta: keep the
                                // latest (a reply carries one Meta, and a
                                // re-issued ticket is a different tuple).
                                if let WireResponse::Meta {
                                    lag_ms,
                                    phase,
                                    crawl_entries,
                                    crawl_dirs_pending,
                                    ..
                                } = r
                                {
                                    if lag_ms.is_some() {
                                        *lag = *lag_ms;
                                    }
                                    // The phase is the daemon's own word for
                                    // what it is doing; keep the latest. An
                                    // old daemon sends none, so this stays
                                    // None and the line falls back to lag.
                                    if let Some(p) = phase
                                        .as_deref()
                                        .and_then(fflocate::health::Phase::parse)
                                    {
                                        *phase_wm = Some(p);
                                    }
                                    if crawl_entries.is_some() {
                                        *crawl_entries_wm = *crawl_entries;
                                    }
                                    if crawl_dirs_pending.is_some() {
                                        *crawl_dirs_wm = *crawl_dirs_pending;
                                    }
                                }
                            }
                            let pct = *watermark;
                            let lag_ms = *lag;
                            let phase = *phase_wm;
                            let crawl_entries = *crawl_entries_wm;
                            let crawl_dirs_pending = *crawl_dirs_wm;
                            if hits.len() > *rendered {
                                *rendered = hits.len();
                                let elapsed = started_at.elapsed();
                                let fresh_hits = hits.clone();
                                drop(pending_ref);
                                if gate.borrow().is_current(seq) {
                                    // Paint only the rows on screen (MAX_ROWS);
                                    // the status counts every Hit so far.
                                    let shown: Vec<String> =
                                        fresh_hits.iter().take(MAX_ROWS).cloned().collect();
                                    partial_results.set_rows(&rows_to_cells(&shown));
                                    *partial_current.borrow_mut() = shown;
                                    // The streaming line: hits are on screen,
                                    // the total is still being counted. No
                                    // invented "starting" wording — the daemon's
                                    // phase, if it has one, speaks for itself.
                                    let _ = (phase, crawl_entries, crawl_dirs_pending);
                                    startup_status.set_text(&streaming_status_line(
                                        hits.len(),
                                        elapsed,
                                        pct,
                                    ));
                                }
                            } else if hits.is_empty() {
                                let label_owned = label.clone();
                                let elapsed = started_at.elapsed();
                                let n = hits.len();
                                drop(pending_ref);
                                // No Hits yet: the in-flight line, carrying the
                                // daemon's own phase/crawl progress once its
                                // Meta has landed. No client-side "starting"
                                // guess, and no process-age number.
                                let line = searching_status_line(
                                    &label_owned,
                                    elapsed,
                                    n,
                                    false,
                                    pct,
                                    lag_ms,
                                    phase,
                                    crawl_entries,
                                    crawl_dirs_pending,
                                );
                                startup_status.set_text(&line);
                            }
                        }
                    }

                    // 2. Start a new request if the debounce has elapsed (or the Search
                    //    button asked for one now). A newer keystroke *supersedes*
                    //    the in-flight scan: drop its socket and retire its
                    //    ticket, then issue the fresh query on this same tick.
                    //    Without this a slow short-prefix scan (unaccelerated
                    //    <3-char literal) head-of-line-blocks the full string
                    //    behind it. Note the honesty bound: dropping the
                    //    socket only stops *this client* waiting — the
                    //    daemon-side scan runs to Done regardless (its writes
                    //    fail into a closed fd, but nothing cancels the scan
                    //    itself). The win is client latency, not daemon CPU:
                    //    the fresh query issues now instead of after the stale
                    //    4 s scan completes.
                    let immediate = std::mem::replace(&mut *start_now.borrow_mut(), false);
                    if !immediate {
                        let Some(t) = *due_at.borrow() else {
                            // No keystroke pending. But the user may have hit
                            // Search while a scan is in flight — `immediate`
                            // covers that above, so nothing to do here.
                            if pending.borrow().is_some() {
                                return true;
                            }
                            return true;
                        };
                        if std::time::Instant::now() < t {
                            return true; // still typing
                        }
                    }
                    if pending.borrow().is_some() {
                        // Supersede: dropping the PendingRequest closes its
                        // socket, so this client stops waiting. (The orphaned
                        // daemon-side scan runs to Done regardless — the win
                        // is issuing the fresh query now, not saving daemon
                        // CPU.) `supersede` retires the ticket so stragglers
                        // cannot paint and frees the gate for the reissue
                        // below — same tick, no extra latency.
                        *pending.borrow_mut() = None;
                        gate.borrow_mut().supersede();
                    }
                    // An explicit Search with no combined query clears the list
                    // instead of sending an empty query: empty matches
                    // everything, which would fill the list with entries that
                    // match neither box.
                    if box_change_clears(
                        &quick_box.get_text().unwrap_or_default(),
                        &adv_box.get_text().unwrap_or_default(),
                    ) {
                        *due_at.borrow_mut() = None;
                        // An emptied box shows no results, but the daemon may
                        // still be behind — say so rather than rendering a
                        // clean-looking "0 matches" from a stale view.
                        let remembered = *last_lag.borrow();
                        render(Ok(SearchOutcome {
                            catching_up: remembered.is_some(),
                            lag_ms: remembered,
                            ..Default::default()
                        }));
                        return true;
                    }
                    let Some(ticket) = gate.borrow_mut().begin() else {
                        return true;
                    };
                    *due_at.borrow_mut() = None;

                    // The display label is read now (main thread owns the boxes):
                    // by the time the reply lands the user may have typed on.
                    let label = search_label(
                        &quick_box.get_text().unwrap_or_default(),
                        &adv_box.get_text().unwrap_or_default(),
                    );
                    match build_request().and_then(|line| {
                        let stream = connect_stream()?;
                        PendingRequest::start(stream, &line)
                    }) {
                        Ok(p) => {
                            *pending.borrow_mut() = Some((
                                ticket.seq,
                                p,
                                std::time::Instant::now(),
                                label,
                                0,
                                None,
                                None,
                                None,
                                None,
                                None,
                            ));
                        }
                        Err(e) => {
                            // The request never started, so it can never finish: release
                            // the gate's slot or every later search would be refused.
                            if let Some(outcome) = gate.borrow_mut().finish(ticket.seq, Err(e)) {
                                render(outcome);
                            }
                        }
                    }
                    true // keep ticking for the life of the app
                }),
            )
        };
        let _ = pump;

        // Arm the debounce on every change to either box. Only when the
        // *combined* query is empty clear the list at once rather than
        // waiting, since "show everything again" is the absence of a query,
        // not a query. A box emptied while the other still holds text keeps
        // searching: the list shows only entries matching both boxes.
        for entry in [&query, &advanced_query] {
            let arm = arm.clone();
            let render = render.clone();
            let quick_box = query.clone();
            let adv_box = advanced_query.clone();
            entry
                .connect_changed(move || {
                    let quick = quick_box.get_text().unwrap_or_default();
                    let adv = adv_box.get_text().unwrap_or_default();
                    if box_change_clears(&quick, &adv) {
                        render(Ok(SearchOutcome::default()));
                        return;
                    }
                    arm();
                })
                .ok();
        }
    }

    // Return in either query field runs the same search.
    {
        // GDK_KEY_Return / GDK_KEY_KP_Enter.
        const KEYS: [u32; 2] = [0xFF0D, 0xFF8D];
        for entry in [&query, &advanced_query] {
            let run_search = run_search.clone();
            entry.on_key_raw(Box::new(move |keyval, _state| {
                if !KEYS.contains(&keyval) {
                    return false;
                }
                if let Ok(mut f) = run_search.try_borrow_mut() {
                    f();
                }
                // Claim the key so the entry does not also beep/act on it.
                true
            }));
        }
    }

    // Right-click on the results opens a context menu for the row under the
    // cursor. The row is found with GTK's own hit-test (`line_at`), so it stays
    // correct under scrolling and with any font.
    {
        let app_handle = app.clone();
        let results = results.clone();
        let status = status.clone();
        let current = current.clone();
        let live_menu = live_menu.clone();
        let cb_results = results.clone();
        let status_for_err = status.clone();
        let drag_row = drag_row.clone();
        if let Err(e) = results.connect_button_press_event(move |button, x, y, event| {
            // Buttons arrive normalized (`normalize_press_button`): a
            // middle-press is already button 1 here, so primary handling
            // covers it with no special case.
            // Primary press starts a potential drag: remember the row grabbed.
            // (button-press fires before the drag begins, so this is set by the
            // time `drag-data-get` runs.) The row is the one under the pointer.
            if button == 1 {
                let row = cb_results.row_at(x, y);
                let path = row.and_then(|r| current.borrow().get(r.max(0) as usize).cloned());
                *drag_row.borrow_mut() = path;
                return;
            }
            if button != 3 {
                return;
            }
            // Right-click acts on the row under the pointer; a click on blank
            // space below the rows does nothing rather than acting on a
            // neighbour (see `ff_gui::context_menu_target`).
            let row = cb_results.row_at(x, y);
            let path = match context_menu_target(button, row, &current.borrow()) {
                Some(p) => p,
                None => return,
            };
            // Show the selection too, so it is obvious which row the menu is for.
            // `path` is `Some`, so `row` held a valid in-bounds row.
            cb_results.select_row(row.unwrap_or(0));
            let mut menu = match app_handle.new_context_menu() {
                Ok(m) => m,
                Err(e) => {
                    status.set_text(&format!("context menu unavailable: {e}"));
                    return;
                }
            };
            // One-shot guard per menu: some hosts deliver the choice twice
            // (once via the popup return value, once via a command message).
            // Copy is idempotent but Open/terminal spawns are not — run once
            // per menu. Per-menu (not global): every new right-click arms a
            // fresh menu whose items must all fire once.
            let menu_fired: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
            for action in RowAction::all() {
                let path = path.clone();
                let status = status.clone();
                let results = cb_results.clone();
                let menu_fired = menu_fired.clone();
                menu.append_item(
                    action.label(),
                    Box::new(move || {
                        if *menu_fired.borrow() {
                            return;
                        }
                        *menu_fired.borrow_mut() = true;
                        let msg = run_row_action(action, &path, &results);
                        status.set_text(&msg);
                    }),
                );
            }
            let widget = cb_results.raw_handle();
            // Anchor the menu to the click: forwarding the real GdkEvent makes
            // GtkMenu position it under the pointer (a null event would leave
            // GTK with nothing to anchor to).
            if let Err(e) = menu.popup_at_pointer(widget, event) {
                status.set_text(&format!("context menu failed: {e}"));
                return;
            }
            *live_menu.borrow_mut() = Some(menu);
        }) {
            // Not fatal: the list still works, only the context menu is gone.
            // Say so rather than silently doing nothing on right-click.
            status_for_err.set_text(&format!("context menu unavailable: {e}"));
        }
    }

    // Dragging a row out of the window carries it as a file URI *and* as plain
    // text, so it can be dropped on a file manager (which wants the URI) or a
    // terminal/editor (which wants the path). The payload is read lazily at
    // drop time from the row recorded on left-press. The drag icon shows the
    // same row's themed icon under the cursor (via `drag-begin`): without it
    // GTK drags invisibly and there is no indication anything was picked up.
    {
        let results = results.clone();
        let status = status.clone();
        let drag_row = drag_row.clone();
        let cb_results = results.clone();
        let icon_row = drag_row.clone();
        let icon_results = results.clone();
        if let Err(e) = results.enable_uri_drag_source(
            move || {
                // Prefer the row recorded on left-press; fall back to the widget's
                // own selection (which is what the drop target sees).
                let recorded = drag_row.borrow().clone();
                let path = recorded.or_else(|| cb_results.selected_path(PATH_COLUMN));
                path.as_deref().map(ff_gui::drag_payload)
            },
            move || {
                // The icon for the row being dragged: the same row the payload
                // carries, mapped back through the icon table so the cursor
                // shows the file's own glyph (folder, Rust source, image…).
                // `None` keeps the default cursor rather than a wrong icon.
                let recorded = icon_row.borrow().clone();
                let path = recorded.or_else(|| icon_results.selected_path(PATH_COLUMN))?;
                let row = ff_gui::row_from_path(&path);
                // `row_from_path` already resolved the fallback the list
                // itself shows; recompute it the same way so the two agree.
                let fallback = if row.icon == "folder" {
                    "folder"
                } else {
                    "text-x-generic"
                };
                Some((row.icon, fallback.to_string()))
            },
        ) {
            // Not fatal: the list still works, only dragging is unavailable.
            status.set_text(&format!("drag and drop unavailable: {e}"));
        }
    }

    // Menubar (File/Search/View/Help). Every item reuses a handler something
    // else already owns — the search trigger, the checkbox/expander widgets,
    // the row actions, the daemon client — so the bar cannot drift from the
    // rest of the UI. Toggle items flip their widgets (which visibly hold the
    // state) rather than carrying checkmarks the toolkit cannot render. No
    // accelerators: the toolkit exposes none, so shortcuts wait on toolkit
    // support.
    //
    // One menu item's behaviour, as a closure factory: each box owns its
    // captures ('static, as `connect_activate` requires), cloned from
    // `run()` locals whose types stay inferred (adapter vs wrapper split).
    macro_rules! clone_all {
        ($($n:ident),*) => { $(let $n = $n.clone();)* };
    }
    let menu_item_callback = |action: MenuAction| -> Box<dyn FnMut()> {
        match action {
            MenuAction::SearchNow => {
                let run_search = run_search.clone();
                Box::new(move || {
                    if let Ok(mut f) = run_search.try_borrow_mut() {
                        f();
                    }
                })
            }
            MenuAction::FocusQuery => {
                let query = query.clone();
                Box::new(move || query.grab_focus())
            }
            MenuAction::ClearQuery => {
                let query = query.clone();
                Box::new(move || query.set_text(""))
            }
            MenuAction::ToggleCase => {
                let w = opt_case.clone();
                Box::new(move || w.set_active(!w.is_active()))
            }
            MenuAction::ToggleFuzzy => {
                let w = opt_fuzzy.clone();
                Box::new(move || w.set_active(!w.is_active()))
            }
            MenuAction::ToggleCount => {
                let w = opt_count.clone();
                Box::new(move || w.set_active(!w.is_active()))
            }
            MenuAction::ToggleAdvanced => {
                let a = advanced.clone();
                Box::new(move || a.set_expanded(!a.is_expanded()))
            }
            MenuAction::FilterAudio
            | MenuAction::FilterCompressed
            | MenuAction::FilterExecutable
            | MenuAction::FilterFolder
            | MenuAction::FilterPicture
            | MenuAction::FilterVideo => {
                // Type-filter presets are full query syntax (regex:/type:), so
                // they belong in the Advanced box — the Basic box is
                // literal-only and would quote them into inert text. Visible,
                // refinable, and as-you-type keeps working, unlike a hidden
                // filter. The preset ANDs with whatever the Basic box holds:
                // the list shows only entries matching both.
                let advanced_query = advanced_query.clone();
                let run_search = run_search.clone();
                let preset = action.preset_query().unwrap_or_default();
                Box::new(move || {
                    advanced_query.set_text(preset);
                    if let Ok(mut f) = run_search.try_borrow_mut() {
                        f();
                    }
                })
            }
            MenuAction::OpenContainingFolder | MenuAction::CopyPath => {
                clone_all!(results, status);
                let row_action = match action {
                    MenuAction::OpenContainingFolder => RowAction::OpenContainingFolder,
                    _ => RowAction::CopyPath,
                };
                Box::new(move || match results.selected_path(PATH_COLUMN) {
                    Some(path) => {
                        let msg = run_row_action(row_action, &path, &results);
                        status.set_text(&msg);
                    }
                    None => status.set_text("no row selected"),
                })
            }
            MenuAction::RefreshIndex => {
                clone_all!(status);
                Box::new(move || {
                    let client = DaemonClient::new();
                    let roots = match client.root_paths() {
                        Ok(r) => r,
                        Err(e) => {
                            status.set_text(&format!("refresh failed: {e}"));
                            return;
                        }
                    };
                    let mut failed = 0;
                    for r in &roots {
                        if client.rescan(r).is_err() {
                            failed += 1;
                        }
                    }
                    if failed == 0 {
                        let n = roots.len();
                        status.set_text(&format!(
                            "refresh requested on {} root{}",
                            n,
                            if n == 1 { "" } else { "s" }
                        ));
                    } else {
                        status.set_text(&format!("refresh failed on {failed} root(s)"));
                    }
                })
            }
            MenuAction::ShowSyntax => {
                clone_all!(app, status);
                Box::new(move || {
                    show_dialog(
                        &app,
                        "Query syntax",
                        ADVANCED_SYNTAX_HELP.to_string(),
                        |m| status.set_text(m),
                    )
                })
            }
            MenuAction::ShowDaemonStatus => {
                clone_all!(app, status);
                Box::new(move || {
                    let body = match DaemonClient::new().stats() {
                        Ok(s) => {
                            let entries = s.get("entries").and_then(|v| v.as_u64()).unwrap_or(0);
                            let backend = s.get("backend").and_then(|v| v.as_str()).unwrap_or("?");
                            format!("connected ({entries} entries, {backend})")
                        }
                        Err(e) => format!("{e}"),
                    };
                    show_dialog(&app, "Daemon status", body, |m| status.set_text(m))
                })
            }
            MenuAction::ShowAbout => {
                clone_all!(app, status);
                Box::new(move || {
                    let socket = DaemonClient::new().socket().to_path_buf();
                    show_dialog(
                        &app,
                        "About ff-gui",
                        format!(
                            "ff-gui {}\nbackend: {BACKEND}\ndaemon socket: {}",
                            env!("CARGO_PKG_VERSION"),
                            socket.display()
                        ),
                        |m| status.set_text(m),
                    )
                })
            }
            MenuAction::Quit => Box::new(|| {
                // No unsaved state anywhere (search frontend; the daemon is a
                // separate process), and the toolkit exposes no main-loop quit,
                // so exiting is exactly what closing the window does.
                std::process::exit(0);
            }),
        }
    };
    let menubar = {
        let err = |e: rswidgets::Error| GuiError::Backend(e.to_string());
        // The action group must exist BEFORE any action is registered on it:
        // `register_action` is a silent no-op until `ensure_action_group` has
        // run, so registering first dropped every action — leaving each
        // `app.<name>` item with nothing to resolve to, i.e. greyed out.
        // (The group is a plain `GSimpleActionGroup`, deliberately not a
        // `GtkApplication`: the application object resolved `app.<name>` only
        // on some displays. See `ensure_action_group`.)
        let group = app.ensure_action_group().map_err(err)?;
        let mut model = app.new_menu().map_err(err)?;
        for (menu_label, items) in ff_gui::menubar_model() {
            let mut sub = app.new_menu().map_err(err)?;
            for (label, action) in items {
                let mut cb = menu_item_callback(action);
                let act = app.new_simple_action(action.action_name()).map_err(err)?;
                act.connect_activate(move |_| cb()).map_err(err)?;
                app.register_action(&act).map_err(err)?;
                sub.append(label, &format!("app.{}", action.action_name()));
            }
            model.append_submenu(menu_label, &sub);
        }
        let menubar = app.new_menubar(&model, group).map_err(err)?;
        // The model names actions as `app.<name>`, so the group must be
        // visible under the name "app" from the menubar's place in the widget
        // hierarchy — otherwise GTK cannot resolve the items and renders every
        // one insensitive (greyed out). Insert on the window (the menubar's
        // eventual ancestor) and on the bar itself (which covers the GTK4
        // popover path, whose popovers are not window children).
        // SAFETY: `group` is the live GActionGroup pointer
        // `ensure_action_group` just returned, valid for the life of the app.
        unsafe {
            win.insert_action_group("app", group);
            menubar.insert_action_group("app", group);
        }
        menubar
    };

    let layout = app.create_box(Orientation::Vertical, 1)?;
    layout.set_vexpand(true);
    layout.set_hexpand(true);
    layout.append(&menubar);
    layout.append(&query_row);
    layout.append(&advanced);
    layout.append(&status);
    // On GTK the tree view lives in its own scrolled container so it scrolls;
    // elsewhere the widget itself is the list.
    #[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
    layout.append(&results_scroll);
    #[cfg(not(all(feature = "gtk", target_os = "linux", not(feature = "zork"))))]
    layout.append(&results);

    win.set_child(&layout);
    win.present();

    // The Basic Search box owns the keyboard from the start: the window opens
    // ready to type, with no click needed. After `present()` — grabbing focus
    // on an unmapped widget is a silent no-op in GTK, so this must come after.
    query.grab_focus();

    // The settle step for the results columns (see INITIAL_WINDOW_W): once the
    // late header growth has landed, step out to the documented width — but
    // only if the user has not resized meanwhile (a resize of their own
    // already redistributed, and yanking it back would steal their size).
    // One-shot; the height is read live so a height-only tweak of theirs is
    // preserved. Later resizes and row changes redistribute by themselves.
    #[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
    {
        let settle_win = win.clone();
        let _ = App::timeout_add(
            SETTLE_RESIZE_MS,
            Box::new(move || {
                let (w, h) = settle_win.get_size();
                if w == INITIAL_WINDOW_W {
                    settle_win.resize(DEFAULT_WINDOW_W, h);
                }
                false
            }),
        );
    }

    println!("ff-gui — fflocate search browser (backend: {BACKEND})");
    println!("Enter a query in the entry, then invoke Search.");
    app.run()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
}

/// Perform a context-menu action on `path`, returning the message for the
/// status line.
///
/// Copy is done in-process through the widget clipboard; Open,
/// Open-containing-folder, Open-with and Open-in-terminal spawn external
/// helpers (`xdg-open`, `mimeopen -a`, and `x-terminal-emulator` with the
/// file's directory as its working directory — plus a `-font` workaround
/// when the alternative target is zutty; see
/// [`ff_gui::open_in_terminal_command`]).
fn run_row_action(action: RowAction, path: &str, results: &impl TextViewLike) -> String {
    // "Open in terminal" carries host-specific prefix flags: when the
    // alternatives link resolves to zutty — whose `9x18` default aborts when
    // `xfonts-base` is not installed — splice `-font <installed monospace>`
    // in front of `-e` so the emulator starts at all. Every other action
    // launches exactly what `RowAction::command` says.
    let (program, args) = if action == RowAction::OpenInTerminal {
        ff_gui::open_in_terminal_command(path)
    } else {
        action.command(path)
    };
    if program.is_empty() {
        // Clipboard actions need no external command; they differ only in what
        // they copy.
        let text = match action {
            RowAction::CopyFolderPath => containing_dir(path),
            _ => path.to_string(),
        };
        return match results.clipboard_set_text(&text) {
            Ok(()) => format!("copied {}", short_path(&text)),
            Err(e) => format!("copy failed: {e}"),
        };
    }
    match std::process::Command::new(program).args(&args).spawn() {
        Ok(_) => format!("{} → {}", action.label(), short_path(path)),
        Err(e) => format!("{} failed: {e}", action.label()),
    }
}

/// Show a one-label dialog with a Close button. Failures go through `report`
/// (usually the status line), like every other UI error here. `report` keeps
/// this helper free of widget types, which differ per backend.
fn show_dialog(app: &App, title: &str, body: String, report: impl Fn(&str)) {
    let dlg = match app.new_dialog() {
        Ok(d) => d,
        Err(e) => {
            report(&format!("dialog unavailable: {e}"));
            return;
        }
    };
    dlg.set_title(title);
    match app.new_label(&body) {
        Ok(label) => dlg.append_content_area(&label),
        Err(e) => {
            report(&format!("dialog unavailable: {e}"));
            return;
        }
    }
    dlg.add_button("Close", 0);
    dlg.present();
    let closer = dlg.clone();
    if dlg
        .connect_response(move |_| {
            closer.close();
        })
        .is_err()
    {
        report("dialog close unavailable");
    }
}

/// A short label for `path`: the file name, or the path itself if it has none.
fn short_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// The bits of the results widget the action runner needs. A trait so the
/// logic can be unit-tested with a stub in place of a real `TextView`.
pub trait TextViewLike {
    fn clipboard_set_text(&self, text: &str) -> Result<(), rswidgets::Error>;
}

impl TextViewLike for TextView {
    fn clipboard_set_text(&self, text: &str) -> Result<(), rswidgets::Error> {
        TextView::clipboard_set_text(self, text)
    }
}

/// Open a non-blocking connection for a query.
fn connect_stream() -> Result<fflocate::transport::Stream, GuiError> {
    let path = DaemonClient::new().socket().to_path_buf();
    fflocate::transport::connect(&path)
        .map_err(|e| GuiError::Connect(format!("{}: {e}", path.display())))
}

/// Read the foldout's controls into a [`SearchOptions`].
///
/// The max-distance field is only consulted when Fuzzy is on; a blank or
/// unparseable value falls back to the daemon's own default (`None`).
fn gather_options(
    case: &impl Active,
    fuzzy: &impl Active,
    count: &impl Active,
    distance: &impl TextValue,
) -> SearchOptions {
    let fuzzy_on = fuzzy.is_active();
    SearchOptions {
        case_sensitive: case.is_active(),
        fuzzy: fuzzy_on,
        max_distance: if fuzzy_on {
            distance.get_text().and_then(|t| t.trim().parse().ok())
        } else {
            None
        },
        count_only: count.is_active(),
    }
}

/// The bits of a checkbox the option reader needs (lets the logic be tested
/// without a display).
pub trait Active {
    fn is_active(&self) -> bool;
}
/// The bits of a text field the option reader needs.
pub trait TextValue {
    fn get_text(&self) -> Option<String>;
}
/// The layout knob row layout needs: the widget takes leftover width. Lets
/// the row setup be tested without a display.
pub trait Expandable {
    fn set_hexpand(&self, expand: bool);
}

/// The query entry takes all leftover row width: without this the entry keeps
/// its minimum size and grey space pools to the right of the Search button.
fn expand_query_entry(entry: &impl Expandable) {
    entry.set_hexpand(true);
}

// These target the adapter types the app actually builds. The prelude also
// re-exports `common` wrappers of the same name, so name the concrete type
// explicitly — and per backend, since only one adapter exists in a given build.
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
mod widget_traits {
    use super::{Active, Expandable, TextValue, TextViewLike};

    impl Active for rswidgets::backends_gtk_adapter::CheckButton {
        fn is_active(&self) -> bool {
            rswidgets::backends_gtk_adapter::CheckButton::is_active(self)
        }
    }
    impl TextValue for rswidgets::backends_gtk_adapter::Entry {
        fn get_text(&self) -> Option<String> {
            rswidgets::backends_gtk_adapter::Entry::get_text(self)
        }
    }
    impl Expandable for rswidgets::backends_gtk_adapter::Entry {
        fn set_hexpand(&self, expand: bool) {
            rswidgets::backends_gtk_adapter::Entry::set_hexpand(self, expand)
        }
    }
    impl TextViewLike for rswidgets::backends_gtk_adapter::TreeView {
        fn clipboard_set_text(&self, text: &str) -> Result<(), rswidgets::Error> {
            // The tree view is a widget; the clipboard is process-wide.
            rswidgets::backends_gtk_adapter::clipboard_set_text(text)
        }
    }
}

#[cfg(all(feature = "zork", not(feature = "gtk")))]
mod widget_traits {
    use super::{Active, Expandable, TextValue, TextViewLike};

    impl Active for rswidgets::backends_zork_adapter::CheckButton {
        fn is_active(&self) -> bool {
            rswidgets::backends_zork_adapter::CheckButton::is_active(self)
        }
    }
    impl TextValue for rswidgets::backends_zork_adapter::Entry {
        fn get_text(&self) -> Option<String> {
            rswidgets::backends_zork_adapter::Entry::get_text(self)
        }
    }
    impl Expandable for rswidgets::backends_zork_adapter::Entry {
        fn set_hexpand(&self, expand: bool) {
            rswidgets::backends_zork_adapter::Entry::set_hexpand(self, expand)
        }
    }
    impl TextViewLike for rswidgets::backends_zork_adapter::TreeView {
        fn clipboard_set_text(&self, text: &str) -> Result<(), rswidgets::Error> {
            // The REPL copies with the terminal clipboard escape.
            rswidgets::backends_zork_adapter::clipboard_set_text(text)
        }
    }
}

// The other targets build the same UI from their own adapter types. These are
// not built on this host, but must compile there.
#[cfg(all(windows, not(feature = "zork")))]
mod widget_traits {
    use super::{Active, Expandable, TextValue, TextViewLike};
    impl Active for rswidgets::backends_nwg_adapter::CheckButton {
        fn is_active(&self) -> bool {
            rswidgets::backends_nwg_adapter::CheckButton::is_active(self)
        }
    }
    impl TextValue for rswidgets::backends_nwg_adapter::Entry {
        fn get_text(&self) -> Option<String> {
            rswidgets::backends_nwg_adapter::Entry::get_text(self)
        }
    }
    impl Expandable for rswidgets::backends_nwg_adapter::Entry {
        fn set_hexpand(&self, expand: bool) {
            rswidgets::backends_nwg_adapter::Entry::set_hexpand(self, expand)
        }
    }
    impl TextViewLike for rswidgets::backends_nwg_adapter::TreeView {
        fn clipboard_set_text(&self, text: &str) -> Result<(), rswidgets::Error> {
            // The list is text-backed; the clipboard is process-wide — and
            // unsupported on this backend, so Copy reports the failure
            // through the status line rather than silently doing nothing.
            rswidgets::backends_nwg_adapter::TreeView::clipboard_set_text(self, text)
        }
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
mod widget_traits {
    use super::{Active, Expandable, TextValue, TextViewLike};
    impl Active for rswidgets::backends_wasm_adapter::CheckButton {
        fn is_active(&self) -> bool {
            rswidgets::backends_wasm_adapter::CheckButton::is_active(self)
        }
    }
    impl TextValue for rswidgets::backends_wasm_adapter::Entry {
        fn get_text(&self) -> Option<String> {
            rswidgets::backends_wasm_adapter::Entry::get_text(self)
        }
    }
    impl Expandable for rswidgets::backends_wasm_adapter::Entry {
        fn set_hexpand(&self, expand: bool) {
            rswidgets::backends_wasm_adapter::Entry::set_hexpand(self, expand)
        }
    }
}

// macOS builds the same UI from the AppKit adapter types: checkbox/entry
// layout traits plus clipboard through the AppKit pasteboard. The results
// list is the TextView-backed TreeView stub (no click-to-select yet), so
// there is no TextViewLike arm — Copy/Open act on the selected path where
// the backend reports one.
#[cfg(all(target_os = "macos", not(feature = "zork")))]
mod widget_traits {
    use super::{Active, Expandable, TextValue, TextViewLike};
    impl Active for rswidgets::backends_macos_adapter::CheckButton {
        fn is_active(&self) -> bool {
            rswidgets::backends_macos_adapter::CheckButton::is_active(self)
        }
    }
    impl TextValue for rswidgets::backends_macos_adapter::Entry {
        fn get_text(&self) -> Option<String> {
            rswidgets::backends_macos_adapter::Entry::get_text(self)
        }
    }
    impl Expandable for rswidgets::backends_macos_adapter::Entry {
        fn set_hexpand(&self, expand: bool) {
            rswidgets::backends_macos_adapter::Entry::set_hexpand(self, expand)
        }
    }
    impl TextViewLike for rswidgets::backends_macos_adapter::TreeView {
        fn clipboard_set_text(&self, text: &str) -> Result<(), rswidgets::Error> {
            // The list is text-backed; the clipboard is process-wide.
            rswidgets::backends_macos_adapter::clipboard_set_text(text)
        }
    }
}

/// Render the result list: one path per line.
/// Build the three-column cells (icon, name, path) for a list of paths.
///
/// The icon is given as a theme name plus a generic fallback, since a minimal
/// icon theme may not carry the specific name. A fourth, hidden value carries
/// the file extension: on GTK it fills the store's hidden extension column,
/// which the (otherwise unorderable) icon column sorts by.
fn rows_to_cells(paths: &[String]) -> Vec<Vec<CellValue>> {
    rows_from_paths(paths)
        .into_iter()
        .map(|r| {
            let fallback = if r.icon == "folder" {
                "folder"
            } else {
                "text-x-generic"
            };
            let ext = extension_sort_key(&r.name);
            vec![
                CellValue::Icon {
                    name: r.icon,
                    fallback: fallback.to_string(),
                },
                CellValue::Text(r.name),
                CellValue::Text(r.path),
                CellValue::Text(ext),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the results widget so the action runner can be tested
    /// without a display or a real clipboard.
    struct FakeView(RefCell<Option<String>>);

    impl TextViewLike for FakeView {
        fn clipboard_set_text(&self, text: &str) -> Result<(), rswidgets::Error> {
            *self.0.borrow_mut() = Some(text.to_string());
            Ok(())
        }
    }

    struct FakeActive(bool);
    impl Active for FakeActive {
        fn is_active(&self) -> bool {
            self.0
        }
    }
    struct FakeText(String);
    impl TextValue for FakeText {
        fn get_text(&self) -> Option<String> {
            Some(self.0.clone())
        }
    }

    struct FakeExpandable(RefCell<bool>);
    impl Expandable for FakeExpandable {
        fn set_hexpand(&self, expand: bool) {
            *self.0.borrow_mut() = expand;
        }
    }

    #[test]
    fn query_entry_expands_to_fill_the_row() {
        // Regression test: without the expand flag the entry keeps its minimum
        // width and vast grey space pools to the right of the Search button.
        // Real widgets are not observable headless, so a recording fake pins
        // the layout helper `run()` calls for the query row.
        let entry = FakeExpandable(RefCell::new(false));
        expand_query_entry(&entry);
        assert!(
            *entry.0.borrow(),
            "query entry must take leftover row width"
        );
    }

    #[test]
    fn gather_options_maps_controls_to_daemon_fields() {
        let o = gather_options(
            &FakeActive(true),
            &FakeActive(true),
            &FakeActive(true),
            &FakeText("3".into()),
        );
        assert!(o.case_sensitive);
        assert!(o.fuzzy);
        assert!(o.count_only);
        assert_eq!(o.max_distance, Some(3));
    }

    #[test]
    fn gather_options_ignores_distance_when_fuzzy_is_off() {
        // The field is inert with Fuzzy off: whatever it holds must not leak
        // into the query.
        let o = gather_options(
            &FakeActive(false),
            &FakeActive(false),
            &FakeActive(false),
            &FakeText("9".into()),
        );
        assert_eq!(o.max_distance, None);
        assert!(!o.fuzzy);
    }

    #[test]
    fn gather_options_tolerates_a_blank_or_bad_distance() {
        let blank = gather_options(
            &FakeActive(false),
            &FakeActive(true),
            &FakeActive(false),
            &FakeText("  ".into()),
        );
        assert_eq!(blank.max_distance, None);
        let bad = gather_options(
            &FakeActive(false),
            &FakeActive(true),
            &FakeActive(false),
            &FakeText("abc".into()),
        );
        assert_eq!(bad.max_distance, None);
    }

    #[test]
    fn copy_action_puts_the_path_on_the_clipboard() {
        let view = FakeView(RefCell::new(None));
        let msg = run_row_action(RowAction::CopyPath, "/a/b/notes.txt", &view);
        assert_eq!(view.0.borrow().as_deref(), Some("/a/b/notes.txt"));
        // The status names the file, not the whole path.
        assert_eq!(msg, "copied notes.txt");
    }

    #[test]
    fn copy_reports_a_clipboard_failure() {
        struct Broken;
        impl TextViewLike for Broken {
            fn clipboard_set_text(&self, _t: &str) -> Result<(), rswidgets::Error> {
                Err(rswidgets::Error::Backend("no terminal".into()))
            }
        }
        let msg = run_row_action(RowAction::CopyPath, "/a/b.rs", &Broken);
        assert!(msg.starts_with("copy failed:"), "{msg}");
    }

    #[test]
    fn short_path_prefers_the_file_name() {
        assert_eq!(short_path("/a/b/notes.txt"), "notes.txt");
        // A trailing slash is normalised away: the name is still "b".
        assert_eq!(short_path("/a/b/"), "b");
        // "/" has no name at all, so the path itself is shown.
        assert_eq!(short_path("/"), "/");
    }

    #[test]
    fn rows_to_cells_builds_the_three_columns() {
        let paths = vec!["/a/b/main.rs".to_string()];
        let cells = rows_to_cells(&paths);
        assert_eq!(cells.len(), 1);
        let row = &cells[0];
        // Column 0 is the icon (with a sensible generic fallback), 1 the name,
        // 2 the full path, plus a hidden 4th value carrying the extension the
        // icon column sorts by.
        match &row[0] {
            CellValue::Icon { name, fallback } => {
                assert_eq!(name, "text-x-rust");
                assert_eq!(fallback, "text-x-generic");
            }
            _ => panic!("column 0 must be the icon"),
        }
        match &row[1] {
            CellValue::Text(t) => assert_eq!(t, "main.rs"),
            _ => panic!("column 1 must be the name"),
        }
        match &row[2] {
            CellValue::Text(t) => assert_eq!(t, "/a/b/main.rs"),
            _ => panic!("column 2 must be the path"),
        }
        match &row[3] {
            CellValue::Text(t) => assert_eq!(t, "rs"),
            _ => panic!("column 3 must be the extension sort key"),
        }
    }

    #[test]
    fn rows_to_cells_carries_an_empty_extension_for_directories() {
        // A directory has no extension: its hidden sort key is empty, so
        // directories group together ahead of any extension.
        let cells = rows_to_cells(&["/a/src/".to_string()]);
        match &cells[0][3] {
            CellValue::Text(t) => assert_eq!(t, ""),
            _ => panic!("column 3 must be the extension sort key"),
        }
    }

    #[test]
    fn rows_to_cells_gives_directories_the_folder_icon() {
        let cells = rows_to_cells(&["/a/src/".to_string()]);
        match &cells[0][0] {
            CellValue::Icon { name, fallback } => {
                assert_eq!(name, "folder");
                // A folder falls back to itself, not to a generic file icon.
                assert_eq!(fallback, "folder");
            }
            _ => panic!("expected an icon"),
        }
    }

    #[test]
    fn rows_to_cells_is_empty_for_no_paths() {
        assert!(rows_to_cells(&[]).is_empty());
    }
}
