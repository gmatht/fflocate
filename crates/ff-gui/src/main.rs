//! `ff-gui` — search the findex daemon through a rustxWidgets window.
//!
//! Layout:
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
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
//! The backend is rustxWidgets' `zork` model, which is headless: the same widget
//! calls work in a terminal (REPL), over ssh, or against GTK when that backend is
//! available. The daemon does all the indexing and matching; this only collects
//! input and renders rows.

use rustxwidgets::prelude::*;

use std::cell::RefCell;
use std::rc::Rc;

use ff_gui::{
    effective_query, encode_request, query_is_searchable, render_count, rows_from_paths,
    DaemonClient, GuiError, PendingRequest, Progress, RowAction, SearchGate,
    SearchOptions, SearchOutcome, WireResponse, ADVANCED_SYNTAX_HELP, DEFAULT_MAX_DISTANCE,
};

/// A search's result, plus whether the advanced box's query was the one that ran.
type SearchResult = Result<(SearchOutcome, bool), GuiError>;

/// Applies a result to the widgets. Main-thread only: the widgets are not `Send`.
type RenderFn = Rc<dyn Fn(SearchResult)>;

/// Which backend this binary was compiled with.
///
/// `ff --gui` locates `ff-gui` next to itself, and `cargo run --bin ff` builds
/// *only* `ff` -- so a stale sibling built with different features (e.g.
/// `--no-default-features --features zork`) can be launched instead. Naming the
/// backend makes that mismatch visible rather than mysterious.
const BACKEND: &str = if cfg!(feature = "gtk") {
    "gtk"
} else if cfg!(feature = "zork") {
    "zork"
} else {
    "unknown"
};

/// Default window size, big enough for the three columns to be readable.
const DEFAULT_WINDOW_W: i32 = 1000;
const DEFAULT_WINDOW_H: i32 = 700;

/// How many rows to render at most (the daemon may match millions).
const MAX_ROWS: usize = 200;
/// How many hits to ask the daemon for (rows plus a little slack for the
/// "of N matches" line).
const FETCH_LIMIT: usize = MAX_ROWS + 1;

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
    let client = DaemonClient::new();

    let win = app.create_window()?;
    win.set_title("ff-gui — findex search");
    // Roomy enough that all three columns are visible on launch.
    win.set_default_size(DEFAULT_WINDOW_W, DEFAULT_WINDOW_H);

    let query_row = app.create_box(Orientation::Horizontal, 2)?;
    let query = app.create_entry()?;
    query.set_text("");
    let search = app.create_button("Search")?;
    let status = app.create_label("type a query and press Search")?;
    // The results are a three-column list: icon, name, path. On GTK this is a
    // real GtkTreeView (so the columns align and the icon column shows a themed
    // icon); the display-free fallbacks render the same three columns as text.
    let results = app.create_treeview(&[("", true), ("Name", false), ("Path", false)])?;
    // Column index of the full path, which Copy / Open / drag act on.
    const PATH_COLUMN: usize = 2;

    // On GTK the tree view needs its own scrolled container to scroll, and that
    // container must expand with the window — otherwise the results stay at
    // their minimum height while the window grows.
    #[cfg(feature = "gtk")]
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
    let advanced_help = app.create_label(ADVANCED_SYNTAX_HELP)?;

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
    advanced_box.append(&advanced_help);
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

    // A live connection check up front: an empty list from an absent daemon and
    // an empty list from a real "no matches" must not look the same.
    let initial_status = if client.hello() {
        match client.stats() {
            Ok(s) => {
                let entries = s.get("entries").and_then(|v| v.as_u64()).unwrap_or(0);
                let backend = s.get("backend").and_then(|v| v.as_str()).unwrap_or("?");
                format!("connected to findexd at {} ({entries} entries, {backend})", client.socket().display())
            }
            Err(e) => format!("{e}"),
        }
    } else {
        format!("no daemon at {} — run `ff` once to spawn one", client.socket().display())
    };
    status.set_text(&initial_status);

    // The search. Two halves, deliberately separated:
    //
    // * `fetch` runs the query and returns a plain result;
    // * `render` applies that result to the widgets.
    //
    // Splitting them is what lets the debounce reuse the same query code as the
    // Search button and the Return key, so the three cannot drift apart. (It also
    // means the expensive half *could* move to a worker thread, but it is 0.00 s
    // against a warm daemon, so it does not: see the note at the as-you-type code.)
    // Turn the current widgets into the pair (request line, shadowed?). This is the
    // *only* part of a search that needs the widgets, so it is the only part that
    // must run on the main thread — the I/O below is stepped from the pump.
    let build_request: Rc<dyn Fn() -> Result<(String, bool), GuiError>> = {
        let query = query.clone();
        let advanced_query = advanced_query.clone();
        let opt_case = opt_case.clone();
        let opt_fuzzy = opt_fuzzy.clone();
        let opt_count = opt_count.clone();
        let opt_distance = opt_distance.clone();
        Rc::new(move || {
            let quick = query.get_text().unwrap_or_default();
            let adv = advanced_query.get_text().unwrap_or_default();
            let (q, shadowed) = effective_query(&quick, &adv);
            let opts = gather_options(&opt_case, &opt_fuzzy, &opt_count, &opt_distance);
            let req = DaemonClient::query_request(&q, FETCH_LIMIT, &opts);
            encode_request(&req).map(|line| (line, shadowed))
        })
    };

    let render: RenderFn = {
        let status = status.clone();
        let results = results.clone();
        let current = current.clone();
        let opt_count = opt_count.clone();
        Rc::new(move |outcome| match outcome {
            Ok((outcome, shadowed)) => {
                let count_only = opt_count.is_active();
                let mut line = outcome.status_line_opts(outcome.paths.len().min(MAX_ROWS), count_only);
                if shadowed {
                    // Say which query ran: the two boxes disagreeing must not be
                    // silent.
                    line.push_str("  [advanced query]");
                }
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
                    let shown: Vec<String> =
                        outcome.paths.iter().take(MAX_ROWS).cloned().collect();
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

        // Which issued request was the one whose query came from the *advanced* box,
        // so the status line can say so when the reply finally lands. Keyed by the
        // gate's sequence number, because the widgets may have changed by then.
        let shadowed_by_seq: Rc<RefCell<std::collections::HashMap<u64, bool>>> =
            Rc::new(RefCell::new(std::collections::HashMap::new()));

        const DEBOUNCE_MS: u64 = 150;
        const PUMP_MS: u32 = 30;

        let arm = {
            let due_at = due_at.clone();
            move || {
                *due_at.borrow_mut() = Some(
                    std::time::Instant::now() + std::time::Duration::from_millis(DEBOUNCE_MS),
                );
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
            // The request currently in flight, if any. Advancing it is what the
            // pump does: it never blocks, so a slow or wedged daemon leaves the
            // window fully responsive (the pump simply keeps returning `true`).
            let pending: Rc<RefCell<Option<(u64, PendingRequest)>>> =
                Rc::new(RefCell::new(None));
            App::timeout_add(PUMP_MS, Box::new(move || {
                // 1. Advance a request in flight. `WouldBlock` is normal, not an
                //    error: it means "come back next tick", which is exactly what
                //    keeps the loop live.
                let mut finished: Option<(u64, Result<Vec<WireResponse>, GuiError>)> = None;
                if let Some((seq, p)) = pending.borrow_mut().as_mut() {
                    let seq = *seq;
                    match p.advance() {
                        Ok(Progress::Finished) => {
                            finished = Some((seq, Ok(p.take_records())));
                        }
                        Ok(_) => {}
                        Err(e) => finished = Some((seq, Err(e))),
                    }
                }
                if finished.is_some() {
                    *pending.borrow_mut() = None;
                }
                if let Some((seq, result)) = finished {
                    let outcome = result
                        .and_then(|recs| DaemonClient::new().outcome_from_records(&recs))
                        .map(|o| (o, take_shadowed(&shadowed_by_seq, seq)));
                    if let Some(outcome) = gate.borrow_mut().finish(seq, outcome) {
                        render(outcome);
                    }
                }

                // 2. Start a new request if the debounce has elapsed (or the Search
                //    button asked for one now) and nothing is in flight. `begin`
                //    refuses while one is out, so a burst of keystrokes cannot queue
                //    up round-trips.
                if pending.borrow().is_some() {
                    return true;
                }
                let immediate = std::mem::replace(&mut *start_now.borrow_mut(), false);
                if !immediate {
                    let Some(t) = *due_at.borrow() else { return true };
                    if std::time::Instant::now() < t {
                        return true; // still typing
                    }
                }
                let Some(ticket) = gate.borrow_mut().begin() else {
                    return true;
                };
                *due_at.borrow_mut() = None;

                match build_request().and_then(|(line, shadowed)| {
                    let stream = connect_stream()?;
                    PendingRequest::start(stream, &line).map(|p| (p, shadowed))
                }) {
                    Ok((p, shadowed)) => {
                        shadowed_by_seq.borrow_mut().insert(ticket.seq, shadowed);
                        *pending.borrow_mut() = Some((ticket.seq, p));
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
            }))
        };
        let _ = pump;

        // Arm the debounce on every change to either box. An emptied box clears the
        // list at once rather than waiting, since "show everything again" is the
        // absence of a query, not a query.
        for entry in [&query, &advanced_query] {
            let arm = arm.clone();
            let e = entry.clone();
            let render = render.clone();
            entry
                .connect_changed(move || {
                    if !query_is_searchable(&e.get_text().unwrap_or_default()) {
                        render(Ok((SearchOutcome::default(), false)));
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

    // Warn live when both boxes hold text: the advanced one will run and the
    // quick one will be ignored. Better to say so as the user types than to
    // surprise them after pressing Search.
    {
        let q1 = query.clone();
        let a1 = advanced_query.clone();
        let s1 = status.clone();
        advanced_query
            .connect_changed(move || {
                let quick = q1.get_text().unwrap_or_default();
                let adv = a1.get_text().unwrap_or_default();
                if !quick.trim().is_empty() && !adv.trim().is_empty() {
                    s1.set_text("both boxes have text — Search will run the advanced query");
                }
            })
            .ok();
        let q2 = query.clone();
        let a2 = advanced_query.clone();
        let s2 = status.clone();
        query
            .connect_changed(move || {
                let quick = q2.get_text().unwrap_or_default();
                let adv = a2.get_text().unwrap_or_default();
                if !quick.trim().is_empty() && !adv.trim().is_empty() {
                    s2.set_text("both boxes have text — Search will run the advanced query");
                }
            })
            .ok();
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
            // Left-press starts a potential drag: remember the row grabbed.
            // (button-press fires before the drag begins, so this is set by the
            // time `drag-data-get` runs.) The row is the one under the pointer.
            if button == 1 {
                let row = cb_results.row_at(x, y);
                let path = row
                    .and_then(|r| current.borrow().get(r.max(0) as usize).cloned());
                *drag_row.borrow_mut() = path;
                return;
            }
            if button != 3 {
                return;
            }
            // Right-click acts on the row under the pointer; a click on blank
            // space below the rows does nothing rather than acting on a
            // neighbour.
            let Some(row) = cb_results.row_at(x, y) else {
                return;
            };
            let path = match current.borrow().get(row.max(0) as usize) {
                Some(p) => p.clone(),
                None => return,
            };
            // Show the selection too, so it is obvious which row the menu is for.
            cb_results.select_row(row);
            let mut menu = match app_handle.new_context_menu() {
                Ok(m) => m,
                Err(e) => {
                    status.set_text(&format!("context menu unavailable: {e}"));
                    return;
                }
            };
            for action in [
                RowAction::CopyPath,
                RowAction::Open,
                RowAction::OpenContainingFolder,
            ] {
                let path = path.clone();
                let status = status.clone();
                let results = cb_results.clone();
                menu.append_item(action.label(), Box::new(move || {
                    let msg = run_row_action(action, &path, &results);
                    status.set_text(&msg);
                }));
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
    // drop time from the row recorded on left-press.
    {
        let results = results.clone();
        let status = status.clone();
        let drag_row = drag_row.clone();
        let cb_results = results.clone();
        if let Err(e) = results.enable_uri_drag_source(move || {
            // Prefer the row recorded on left-press; fall back to the widget's
            // own selection (which is what the drop target sees).
            let recorded = drag_row.borrow().clone();
            let path = recorded.or_else(|| cb_results.selected_path(PATH_COLUMN));
            path.as_deref().map(ff_gui::drag_payload)
        }) {
            // Not fatal: the list still works, only dragging is unavailable.
            status.set_text(&format!("drag and drop unavailable: {e}"));
        }
    }

    query_row.append(&query);
    query_row.append(&search);

    let layout = app.create_box(Orientation::Vertical, 1)?;
    layout.set_vexpand(true);
    layout.set_hexpand(true);
    layout.append(&query_row);
    layout.append(&advanced);
    layout.append(&status);
    // On GTK the tree view lives in its own scrolled container so it scrolls;
    // elsewhere the widget itself is the list.
    #[cfg(feature = "gtk")]
    layout.append(&results_scroll);
    #[cfg(not(feature = "gtk"))]
    layout.append(&results);

    win.set_child(&layout);
    win.present();

    println!("ff-gui — findex search browser (backend: {BACKEND})");
    println!("Enter a query in the entry, then invoke Search.");
    app.run().map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
}

/// Perform a context-menu action on `path`, returning the message for the
/// status line.
///
/// Copy is done in-process through the widget clipboard; Open and
/// Open-containing-folder spawn `xdg-open`.
fn run_row_action(action: RowAction, path: &str, results: &impl TextViewLike) -> String {
    let (program, args) = action.command(path);
    if program.is_empty() {
        // Copy: no external command.
        return match results.clipboard_set_text(path) {
            Ok(()) => format!("copied {}", short_path(path)),
            Err(e) => format!("copy failed: {e}"),
        };
    }
    match std::process::Command::new(program).args(&args).spawn() {
        Ok(_) => format!("{} → {}", action.label(), short_path(path)),
        Err(e) => format!("{} failed: {e}", action.label()),
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
    fn clipboard_set_text(&self, text: &str) -> Result<(), rustxwidgets::Error>;
}

impl TextViewLike for TextView {
    fn clipboard_set_text(&self, text: &str) -> Result<(), rustxwidgets::Error> {
        TextView::clipboard_set_text(self, text)
    }
}

/// Open a non-blocking connection for a query.
fn connect_stream() -> Result<findex::transport::Stream, GuiError> {
    let path = DaemonClient::new().socket().to_path_buf();
    findex::transport::connect(&path)
        .map_err(|e| GuiError::Connect(format!("{}: {e}", path.display())))
}

/// Look up (and forget) whether a finished request's query came from the advanced
/// box. Forgetting matters: the map would otherwise grow for the life of the app.
fn take_shadowed(
    map: &Rc<RefCell<std::collections::HashMap<u64, bool>>>,
    seq: u64,
) -> bool {
    map.borrow_mut().remove(&seq).unwrap_or(false)
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

// These target the adapter types the app actually builds. The prelude also
// re-exports `common` wrappers of the same name, so name the concrete type
// explicitly — and per backend, since only one adapter exists in a given build.
#[cfg(feature = "gtk")]
mod widget_traits {
    use super::{Active, TextViewLike, TextValue};

    impl Active for rustxwidgets::backends_gtk_adapter::CheckButton {
        fn is_active(&self) -> bool {
            rustxwidgets::backends_gtk_adapter::CheckButton::is_active(self)
        }
    }
    impl TextValue for rustxwidgets::backends_gtk_adapter::Entry {
        fn get_text(&self) -> Option<String> {
            rustxwidgets::backends_gtk_adapter::Entry::get_text(self)
        }
    }
    impl TextViewLike for rustxwidgets::backends_gtk_adapter::TreeView {
        fn clipboard_set_text(&self, text: &str) -> Result<(), rustxwidgets::Error> {
            // The tree view is a widget; the clipboard is process-wide.
            rustxwidgets::backends_gtk_adapter::clipboard_set_text(text)
        }
    }
}

#[cfg(all(feature = "zork", not(feature = "gtk")))]
mod widget_traits {
    use super::{Active, TextViewLike, TextValue};

    impl Active for rustxwidgets::backends_zork_adapter::CheckButton {
        fn is_active(&self) -> bool {
            rustxwidgets::backends_zork_adapter::CheckButton::is_active(self)
        }
    }
    impl TextValue for rustxwidgets::backends_zork_adapter::Entry {
        fn get_text(&self) -> Option<String> {
            rustxwidgets::backends_zork_adapter::Entry::get_text(self)
        }
    }
    impl TextViewLike for rustxwidgets::backends_zork_adapter::TreeView {
        fn clipboard_set_text(&self, text: &str) -> Result<(), rustxwidgets::Error> {
            // The REPL copies with the terminal clipboard escape.
            rustxwidgets::backends_zork_adapter::clipboard_set_text(text)
        }
    }
}

// The other targets build the same UI from their own adapter types. These are
// not built on this host, but must compile there.
#[cfg(all(windows, not(feature = "zork")))]
mod widget_traits {
    use super::{Active, TextViewLike, TextValue};
    impl Active for rustxwidgets::backends_nwg_adapter::CheckButton {
        fn is_active(&self) -> bool {
            rustxwidgets::backends_nwg_adapter::CheckButton::is_active(self)
        }
    }
    impl TextValue for rustxwidgets::backends_nwg_adapter::Entry {
        fn get_text(&self) -> Option<String> {
            rustxwidgets::backends_nwg_adapter::Entry::get_text(self)
        }
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
mod widget_traits {
    use super::{Active, TextViewLike, TextValue};
    impl Active for rustxwidgets::backends_wasm_adapter::CheckButton {
        fn is_active(&self) -> bool {
            rustxwidgets::backends_wasm_adapter::CheckButton::is_active(self)
        }
    }
    impl TextValue for rustxwidgets::backends_wasm_adapter::Entry {
        fn get_text(&self) -> Option<String> {
            rustxwidgets::backends_wasm_adapter::Entry::get_text(self)
        }
    }
}

/// Render the result list: one path per line.
/// Build the three-column cells (icon, name, path) for a list of paths.
///
/// The icon is given as a theme name plus a generic fallback, since a minimal
/// icon theme may not carry the specific name.
fn rows_to_cells(paths: &[String]) -> Vec<Vec<CellValue>> {
    rows_from_paths(paths)
        .into_iter()
        .map(|r| {
            let fallback = if r.icon == "folder" { "folder" } else { "text-x-generic" };
            vec![
                CellValue::Icon { name: r.icon, fallback: fallback.to_string() },
                CellValue::Text(r.name),
                CellValue::Text(r.path),
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
        fn clipboard_set_text(&self, text: &str) -> Result<(), rustxwidgets::Error> {
            *self.0.borrow_mut() = Some(text.to_string());
            Ok(())
        }
    }

    struct FakeActive(bool);
    impl Active for FakeActive {
        fn is_active(&self) -> bool { self.0 }
    }
    struct FakeText(String);
    impl TextValue for FakeText {
        fn get_text(&self) -> Option<String> { Some(self.0.clone()) }
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
            fn clipboard_set_text(&self, _t: &str) -> Result<(), rustxwidgets::Error> {
                Err(rustxwidgets::Error::Backend("no terminal".into()))
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
        // 2 the full path.
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
