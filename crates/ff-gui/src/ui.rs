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

// The results list, the advanced-search foldout and the row context menu come
// from rswidgets' *compat* layer: the merged backends dropped `TreeView`,
// `Expander` and `ContextMenu`, and compat rebuilds them on the portable
// (Canvas/Box/Button) surface so one implementation serves GTK and AppKit.
// `CellValue`/`ScrollDirection` are the cell and wheel types those widgets use.
use rswidgets::compat::{CellValue, ContextMenu, ScrollDirection};

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::{
    apply_created_range, box_change_clears, containing_dir, created_request_range, row_menu_plan,
    created_ui, drag_payload, effective_query, encode_request, render_count, streaming_rows,
    rows_from_paths, scan_percent, search_label, searching_status_line, step_max_distance,
    streaming_status_line, CreatedRange, DaemonClient, GuiError, MenuAction, PendingRequest,
    Progress, RowAction, SearchGate, SearchOptions, SearchOutcome, StartupPoll, StartupProbe,
    FullSet, MAX_SORTED_ROWS, WheelStep, WireResponse, ADVANCED_SYNTAX_HELP, DEFAULT_MAX_DISTANCE,
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

/// Column index of the full path in the results list: icon, name, path.
///
/// Module scope because `step_sort_load` reads the displayed order back, which
/// needs the same column the render path uses.
const PATH_COLUMN: usize = 2;

/// How many rows to render at most from a first page (the daemon may match
/// millions; sorting the whole set loads the rest, and the list draws only
/// the rows on screen, so every one of them scrolls).
const MAX_ROWS: usize = 200;
/// How many hits to ask the daemon for (rows plus a little slack for the
/// "of N matches" line).
const FETCH_LIMIT: usize = MAX_ROWS + 1;
/// How long startup waits for a freshly spawned daemon's socket. The daemon
/// probes mounts and starts threads before binding, so the socket typically
/// appears a moment after the child exists; past this the status just says
/// it is still starting.
const SPAWN_STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Run the GUI and return the process exit code.
///
/// The multi-call binary (`ffbusy`) dispatches here when invoked as `ff-gui`;
/// the standalone binary's `main` does the same.
pub fn main_entry() -> std::process::ExitCode {
    if let Err(e) = run() {
        eprintln!("ff-gui: {e}");
        return std::process::ExitCode::from(2);
    }
    std::process::ExitCode::SUCCESS
}

/// Print a construction step to stderr when `FF_GUI_TRACE` is set.
///
/// Building the AppKit UI means raw message sends. A send to a selector that
/// does not exist raises an Objective-C exception, which unwinds into the Rust
/// frame that sent it and aborts the process (`Rust cannot catch foreign
/// exceptions`) with no Rust backtrace and, since stdout is block-buffered,
/// often with no output either. The last step printed is then the one that
/// threw, which is the only handle a headless CI run has on such a crash.
macro_rules! trace_step {
    ($($arg:tt)*) => {
        if trace_enabled() {
            eprintln!("ff-gui: {}", format!($($arg)*));
        }
    };
}

fn trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FF_GUI_TRACE").is_some())
}

/// Whether this build's label backend parses Pango markup.
///
/// GTK does. AppKit and iOS flatten markup to its words themselves, so either
/// path reads correctly there. `zork`, NWG, Android and the curses fallbacks
/// store or draw the string as-is, so a terminal or a Win32 `STATIC` would show
/// the tags literally — those get the plain wording instead.
const LABELS_PARSE_MARKUP: bool = cfg!(all(
    feature = "gtk",
    target_os = "linux",
    not(feature = "zork")
));

/// Put one of the alert lines on a label: bold red where the backend parses
/// Pango markup, the plain words where it would otherwise show the tags.
macro_rules! set_alert {
    ($label:expr, $text:expr) => {
        if LABELS_PARSE_MARKUP {
            $label.set_markup(&crate::alert_markup($text));
        } else {
            $label.set_text($text);
        }
    };
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("ff-gui {} (backend: {BACKEND})", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // The icon's theme files must be on disk before GTK starts: GTK reads the
    // icon theme while initialising, so an icon installed later would not be
    // found by name. See `crate::icon::prepare`.
    //
    // CFG: only GTK and macOS have an icon to set, so only they get a call —
    // the other backends have no such method.
    #[cfg(all(unix, not(target_os = "macos"), not(feature = "zork")))]
    let icon_path = crate::icon::prepare();

    trace_step!("app init");
    let app = App::init().map_err(|e| GuiError::Backend(e.to_string()))?;
    let socket_path = DaemonClient::new().socket().to_path_buf();

    trace_step!("create window");
    let win = app.create_window()?;
    trace_step!("window title");
    win.set_title("fflocate");
    // The fflocate mark: the window/taskbar icon on GTK, the Dock icon on
    // macOS. Best-effort, and before `present()` — GTK applies the icon it
    // holds when the window is mapped, so one call here is enough.
    #[cfg(all(unix, not(target_os = "macos"), not(feature = "zork")))]
    win.set_icon(&icon_path, crate::icon::ICON_NAME);
    #[cfg(all(target_os = "macos", not(feature = "zork")))]
    win.set_icon(crate::icon::ICON_PNG);

    // Roomy enough that all three columns are visible on launch.
    win.set_default_size(DEFAULT_WINDOW_W, DEFAULT_WINDOW_H);

    trace_step!("query row box");
    let query_row = app.create_box(Orientation::Horizontal, 2)?;
    trace_step!("query entry");
    let query = app.create_entry()?;
    query.set_text("");
    // The Basic box is literal-only (quoted before sending): say so, so a
    // user reaching for ext:/regex: learns where that syntax lives.
    trace_step!("query placeholder");
    query.set_placeholder_text("filename — literal, no syntax (ext:/regex: go in Advanced search)");
    expand_query_entry(&query);
    trace_step!("search button");
    let search = app.create_button("Search")?;
    trace_step!("query row append");
    query_row.append(&query);
    query_row.append(&search);
    query_row.set_child_hexpand(&query, true);
    trace_step!("status label");
    let status = app.create_label("type a query and press Search")?;
    // The results are a three-column list: icon, name, path. The compat
    // `TreeView` draws the header and rows on a canvas, hit-tests clicks and
    // tracks a selection on every backend, so the three columns align and the
    // same code serves GTK and AppKit. (title, is_icon, sortable): clicking a
    // sortable header sorts by it and a second click reverses the order. All
    // three are sortable, the icon column included: its pictogram groups the
    // rows by kind, which is how you collect the folders at one end.
    trace_step!("results treeview");
    let results = app.create_treeview(&[
        ("", true, true),
        ("Name", false, true),
        ("Path", false, true),
    ])?;
    // `PATH_COLUMN` (module scope) is the column Copy / Open act on.

    // The list scrolls, and the scrolled container must expand with the window —
    // otherwise the results stay at their minimum height while the window grows.
    let results_scroll = {
        trace_step!("results scrolled window");
        let sw = results.clone().into_scrolled()?;
        // The list takes the leftover height. Only the vertical flag is set:
        // `set_hexpand` belonged to the GTK adapter's scrolled window and is not
        // part of the portable layer, so the width is whatever the enclosing box
        // gives it.
        sw.set_vexpand(true);
        sw
    };

    // The advanced-search foldout: a native disclosure holding a free-form
    // query field (with the full syntax) plus real controls. Every control maps
    // to a daemon field — Case sensitive (case_sensitive), Fuzzy (fuzzy), Count
    // only (count), and the fuzzy maximum edit distance (max_distance).
    trace_step!("advanced expander");
    let advanced = app.create_expander("Advanced search")?;
    trace_step!("advanced entry");
    let advanced_query = app.create_entry()?;
    advanced_query.set_text("");
    // The syntax reminder lives inside the field as grey hint text: visible
    // while the field is empty and unfocused, gone as soon as it holds text
    // or is selected for typing.
    advanced_query.set_placeholder_text(ADVANCED_SYNTAX_HELP);

    // The created-date range: two inclusive `YYYY-MM-DD` fields, either blank
    // for unbounded. Each hit carries the entry's creation minute as the daemon
    // stored it, so the GUI filters the rows it gets without a `stat` per row
    // (see `crate::apply_created_range`); a row the daemon could not date falls
    // back to `crate::created_secs`. The status line says exactly what was
    // checked, and a file whose filesystem reports no birth time never matches.
    //
    // The row is built by `crate::created_ui` rather than here: `main.rs` is a
    // binary, so the GTK regression test can only drive the same widgets if
    // the construction lives in the library.
    trace_step!("created row");
    let created = created_ui::build(&app)?;
    let created_from = created.from.clone();
    let created_to = created.to.clone();

    trace_step!("case checkbutton");
    let opt_case = app.create_checkbutton("Case sensitive")?;
    opt_case.set_active(false);
    trace_step!("fuzzy checkbutton");
    let opt_fuzzy = app.create_checkbutton("Fuzzy matching")?;
    opt_fuzzy.set_active(false);
    trace_step!("count checkbutton");
    let opt_count = app.create_checkbutton("Count only (skip the list)")?;
    opt_count.set_active(false);

    // Live mode: a `subscribe` feed on its own connection, stepped by the same
    // pump as the query socket. The toggle only records intent here — the pump
    // opens the feed, because only the main thread may read the boxes the
    // subscription is built from.
    trace_step!("live checkbutton");
    let live = crate::live_ui::build(&app)?;
    let live_wanted: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let live_run: Rc<RefCell<crate::LiveRunner>> =
        Rc::new(RefCell::new(crate::LiveRunner::new()));

    // The max edit distance only means anything with Fuzzy on, so its label
    // says so until then rather than leaving a value that reads as active. The
    // row (label, `-`, field, `+`) is built by the library so a test can drive
    // the very widgets the app shows; the buttons are what makes the value
    // changeable on macOS, where AppKit's entry wires no wheel to the field
    // and keystrokes may not reach it inside a stack view (see
    // `crate::distance_ui`).
    trace_step!("distance row");
    let distance = crate::distance_ui::build(&app, DEFAULT_MAX_DISTANCE)?;
    let opt_distance = distance.field.clone();
    let distance_row = distance.row.clone();

    trace_step!("advanced box");
    let advanced_box = app.create_box(Orientation::Vertical, 2)?;
    advanced_box.append(&advanced_query);
    advanced_box.append(&created.row);
    advanced_box.append(&opt_case);
    advanced_box.append(&opt_fuzzy);
    advanced_box.append(&distance_row);
    advanced_box.append(&opt_count);
    advanced_box.append(&live.button);
    trace_step!("sensitive toggles");
    advanced.set_child(&advanced_box);
    // Collapsed, and it stays collapsed: `set_expanded(false)` is applied after
    // `present()` below, because GTK's show_all would otherwise re-show the
    // body (the expander hides it by hiding the box it lives in).

    // Fuzzy off => the distance means nothing, and the label says `(fuzzy
    // off)` so that is clear rather than silently ignored. The field itself
    // stays typeable and the buttons stay clickable: a disabled `NSTextField`
    // drops input, and someone who prefers preparing the value before ticking
    // Fuzzy must still be able to set it (`gather_options` ignores it until
    // then).
    {
        let controls = distance.clone();
        controls.set_controls_sensitive(false);
        let f = opt_fuzzy.clone();
        opt_fuzzy
            .connect_toggled(move || {
                controls.set_controls_sensitive(f.is_active());
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

    // ------------------------------------------------------------------
    // Sorting the **whole** result set.
    //
    // A search asks for `FETCH_LIMIT` (201) hits, so a header click can only
    // sort those. Sorting every match means asking the daemon for everything
    // (`limit == 0`, as `ff -l 0` documents) and merging the stream into the
    // list: one scan, one snapshot, hits arriving while it runs.
    // ------------------------------------------------------------------
    // The load in flight, if any.
    let sort_load: Rc<RefCell<Option<SortLoad>>> = Rc::new(RefCell::new(None));
    // The (query, options) of the last search, which a load has to re-ask.
    let last_search: Rc<RefCell<Option<(String, SearchOptions)>>> = Rc::new(RefCell::new(None));
    // Matches the last reply reported: a load is only worth starting when there
    // are more of them than the list holds.
    let last_total: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    // "Fetch the whole set for the active sort." Set by a header click and by a
    // finished search that is sorted; consumed by the pump, so a burst of
    // clicks starts at most one load.
    let want_full: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    // Which column the set now in the list was loaded for. Reversing that sort
    // re-sorts locally instead of asking the daemon for the same 600 k paths
    // again; a *different* column does need the set re-fetched, because the
    // rows kept are only the top 1 000 of the previous order.
    let loaded_column: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
    // The created-date range in force, parsed once per search in
    // `build_request`. The renderer reads it back instead of re-reading the
    // boxes, so a reply is never filtered by a half-typed date, and the pump's
    // streaming rows are filtered by the same range the final render uses.
    let created_range: Rc<RefCell<Option<CreatedRange>>> = Rc::new(RefCell::new(None));
    // True while the reply being rendered is the fuzzy follow-up a zero-hit
    // exact search asked for (`crate::wants_suggestion`): the status line then
    // carries the alert line (`crate::suggestion_line`) instead of a count.
    let suggesting: Rc<Cell<bool>> = Rc::new(Cell::new(false));

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
        let created_from = created_from.clone();
        let created_to = created_to.clone();
        let created_range = created_range.clone();
        let last_search = last_search.clone();
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
            // A bad or inverted date refuses the search here, so the status
            // says which field is wrong instead of the list quietly ignoring
            // the range.
            let created = created_request_range(
                &created_from.get_text().unwrap_or_default(),
                &created_to.get_text().unwrap_or_default(),
            )?;
            let req = DaemonClient::query_request(&q, FETCH_LIMIT, &opts);
            *created_range.borrow_mut() = created;
            // Remember exactly what was asked, so a whole-set load asks the
            // same question with no limit rather than re-reading the boxes (the
            // user may have typed on by the time it starts).
            *last_search.borrow_mut() = Some((q.clone(), opts));
            encode_request(&req)
        })
    };

    // Each root's health as the daemon last reported it, filled by the startup
    // probe's `stats` (which asks with roots enabled). Shared with the pump and
    // the render so a `[stale roots: …]` suffix can carry the daemon's own
    // reason instead of leaving the user with a path and no cause.
    let roots_health: Rc<RefCell<Vec<crate::RootHealth>>> = Rc::new(RefCell::new(Vec::new()));

    let render: RenderFn = {
        let status = status.clone();
        let results = results.clone();
        let current = current.clone();
        let roots_health = roots_health.clone();
        let opt_count = opt_count.clone();
        let last_total = last_total.clone();
        let want_full = want_full.clone();
        let created_range = created_range.clone();
        let suggesting = suggesting.clone();
        Rc::new(move |outcome| match outcome {
            Ok(mut outcome) => {
                // A reply names the stale roots, never why they are stale; the
                // daemon's own state words come from the `stats` the startup
                // probe already ran.
                outcome.roots_health = roots_health.borrow().clone();
                let count_only = opt_count.is_active();
                last_total.set(outcome.total);
                // The created range is applied to the page the daemon returned,
                // using the creation minute each hit carries (protocol v5) — no
                // per-row `stat` unless the daemon could not date the entry. The
                // suffix then states how much of that page it hid, so the
                // filtered count never passes for the daemon's total.
                let range = created_range.borrow().clone();
                let note = apply_created_range(&mut outcome, range.clone(), count_only);
                let mut line =
                    outcome.status_line_opts(outcome.paths.len().min(MAX_ROWS), count_only);
                line.push_str(&note);
                if suggesting.get() {
                    // The rows below are the fuzzy follow-up, so the line says
                    // why they are not the query: bold red, as asked for. A
                    // follow-up that found nothing either says so plainly
                    // instead of asking "did you mean…" over an empty list.
                    set_alert!(status, crate::suggestion_line(&outcome));
                } else {
                    status.set_text(&line);
                }
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
                    // Read the rows back rather than copying `shown`: the list
                    // sorts its own rows once a header has been clicked, so
                    // the order on screen is not the daemon's order.
                    *current.borrow_mut() = results.displayed_column(PATH_COLUMN);
                    // Sorted, but only the page on screen is in the list: ask
                    // for the rest so the sort really does cover every match.
                    // A created range is a client-side filter over this page;
                    // the whole-set load would replace the rows with the
                    // unfiltered set, so it stays off while one is active.
                    if range.is_none()
                        && results.sort_state().is_some()
                        && outcome.total > shown.len() as u64
                    {
                        want_full.set(true);
                    }
                }
            }
            Err(e) => {
                status.set_text(&format!("{e}"));
                results.set_rows(&[]);
                current.borrow_mut().clear();
            }
        })
    };

    // The same flag backs the Search button, the Return key and the as-you-type
    // debounce, so the three cannot drift apart. Nothing here runs a query: it
    // asks the pump to start one *now* (bypassing the debounce), so even an
    // explicit click cannot block the window on a slow daemon.
    //
    // An atomic rather than the obvious `Rc<Cell<bool>>`: the AppKit button's
    // click handler must be `Send` (its callback registry is process-global),
    // while every other widget here stays on the main thread. One flag both can
    // share keeps the button on the same path as the keyboard and the menubar.
    let start_now: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    // The keyboard/menubar path: a plain main-thread closure that sets the flag.
    let run_search: Rc<RefCell<Box<dyn FnMut()>>> = Rc::new(RefCell::new(Box::new(|| {})));

    {
        let start_now = start_now.clone();
        search.on_click(move || start_now.store(true, Ordering::SeqCst))?;
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
        // The keystroke window lives in `crate::Debounce`, the same type the
        // macOS probe and the Linux interaction test drive.
        let debounce = Rc::new(crate::Debounce::new(std::time::Duration::from_millis(
            DEBOUNCE_MS,
        )));
        // The gate is what stops a slow query from being overwritten by a newer one
        // and keeps a burst of keystrokes from queuing a query each. It is plain
        // logic, tested in `crate::asyou_type_tests`.
        let gate = Rc::new(RefCell::new(SearchGate::new()));

        const DEBOUNCE_MS: u64 = 150;
        const PUMP_MS: u32 = 30;

        let arm = {
            let debounce = debounce.clone();
            move || debounce.arm()
        };

        // Set by the Search button / Return: "start the next request without waiting
        // for the debounce". They share the pump rather than making their own request
        // so that every path through the GUI does the I/O the same non-blocking way.
        {
            let start_now = start_now.clone();
            *run_search.borrow_mut() = Box::new(move || start_now.store(true, Ordering::SeqCst));
        }

        let pump = {
            let debounce = debounce.clone();
            let gate = gate.clone();
            let render = render.clone();
            let build_request = build_request.clone();
            let startup = startup.clone();
            let startup_status = status.clone();
            let quick_box = query.clone();
            let adv_box = advanced_query.clone();
            let sort_load = sort_load.clone();
            let last_search = last_search.clone();
            let last_total = last_total.clone();
            let want_full = want_full.clone();
            let loaded_column = loaded_column.clone();
            let created_range = created_range.clone();
            let suggesting = suggesting.clone();
            // Live mode: the runner, the user's intent, and the controls the
            // subscription key is built from. The socket is resolved once, so a
            // changed FFLOCATE_SOCKET cannot leave the feed pointing elsewhere
            // than the query socket.
            let live_run = live_run.clone();
            let live_wanted = live_wanted.clone();
            let live_button = live.button.clone();
            let live_socket = DaemonClient::new().socket().to_path_buf();
            let live_case = opt_case.clone();
            let live_fuzzy = opt_fuzzy.clone();
            let live_count = opt_count.clone();
            let live_distance = opt_distance.clone();
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
                        // Stepped into a binding first: a `match` on the borrow
                        // holds the `RefMut` for every arm, and the Done arm
                        // reads the probe again for the roots it parsed.
                        let polled = startup.borrow_mut().poll();
                        match polled {
                            StartupPoll::Pending => {}
                            StartupPoll::Note(line) => startup_status.set_text(&line),
                            StartupPoll::Done(line) => {
                                startup_status.set_text(&line);
                                *roots_health.borrow_mut() = startup.borrow().root_health().to_vec();
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
                        // A zero-hit exact search is not a dead end: the same
                        // question runs again fuzzy at one edit, and the status
                        // says so in bold red (`crate::wants_suggestion`).
                        // Decided before the render so the red line replaces
                        // "0 matches", and issued here because this is where
                        // the pump owns the gate and the socket.
                        let was_suggestion = suggesting.get();
                        let mut suggest: Option<(String, SearchOptions)> = None;
                        if let Some(outcome) = gate.borrow_mut().finish(seq, outcome) {
                            if !was_suggestion {
                                if let (Ok(o), Some((q, opts))) =
                                    (&outcome, last_search.borrow().clone())
                                {
                                    if crate::wants_suggestion(o, opts) {
                                        suggest = Some((q, crate::suggestion_options(opts)));
                                    }
                                }
                            }
                            render(outcome);
                        }
                        suggesting.set(false);
                        if let Some((q, opts)) = suggest {
                            // A whole-set load belongs to the query that just
                            // answered with nothing.
                            *sort_load.borrow_mut() = None;
                            loaded_column.set(None);
                            want_full.set(false);
                            // Remember the follow-up, not the exact question: a
                            // header click's full-set load must re-ask what is
                            // on screen.
                            *last_search.borrow_mut() = Some((q.clone(), opts));
                            if let Some(ticket) = gate.borrow_mut().begin() {
                                let req = DaemonClient::query_request(&q, FETCH_LIMIT, &opts);
                                match encode_request(&req).and_then(|line| {
                                    let stream = connect_stream()?;
                                    PendingRequest::start(stream, &line)
                                }) {
                                    Ok(p) => {
                                        suggesting.set(true);
                                        set_alert!(startup_status, crate::NO_EXACT_MATCHES);
                                        *pending.borrow_mut() = Some((
                                            ticket.seq,
                                            p,
                                            std::time::Instant::now(),
                                            q,
                                            0,
                                            None,
                                            None,
                                            None,
                                            None,
                                            None,
                                        ));
                                    }
                                    Err(e) => {
                                        // The follow-up never started, so it can
                                        // never finish: release the gate's slot.
                                        if let Some(outcome) =
                                            gate.borrow_mut().finish(ticket.seq, Err(e))
                                        {
                                            render(outcome);
                                        }
                                    }
                                }
                            }
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
                            // Filter and bound the streaming rows by the same
                            // rule the final render uses, so rows outside the
                            // range never flash up and then vanish — and so a
                            // mid-stream right-click resolves the rows this
                            // list actually paints (`streaming_rows` is the
                            // function the row-menu tests paint through).
                            let (kept, partial_shown) =
                                streaming_rows(&hits, created_range.borrow().clone(), MAX_ROWS);
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
                                let fresh_hits = kept.clone();
                                drop(pending_ref);
                                if gate.borrow().is_current(seq) {
                                    // Paint only the rows on screen (MAX_ROWS),
                                    // bounded above; the status counts every Hit
                                    // so far.
                                    partial_results.set_rows(&rows_to_cells(&partial_shown));
                                    *partial_current.borrow_mut() =
                                        partial_results.displayed_column(PATH_COLUMN);
                                    // The streaming line: hits are on screen,
                                    // the total is still being counted. No
                                    // invented "starting" wording — the daemon's
                                    // phase, if it has one, speaks for itself.
                                    let _ = (phase, crawl_entries, crawl_dirs_pending);
                                    // The suggestion's own progress stays behind
                                    // its red line: the words are the answer to
                                    // "no exact matches", not a search log.
                                    if !suggesting.get() {
                                        startup_status.set_text(&streaming_status_line(
                                            fresh_hits.len(),
                                            elapsed,
                                            pct,
                                        ));
                                    }
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
                                // Same rule as above: a suggestion's reply keeps
                                // the red line rather than narrating itself.
                                if !suggesting.get() {
                                    startup_status.set_text(&line);
                                }
                            }
                        }
                    }

                    // 1c. The whole-set sort load. Clicking a header sorts the
                    //     rows the list holds — the first page. This issues the
                    //     same query with `limit = 0` (everything, as `ff -l 0`
                    //     documents) and merges the stream into the list as it
                    //     arrives, so the sort covers every match instead of
                    //     the first 201. Bounded work per tick: a fast daemon
                    //     can deliver hundreds of thousands of hits between
                    //     ticks, and building that many rows at once would
                    //     stall the window just as surely as a blocking read.
                    {
                        let mut load_ref = sort_load.borrow_mut();
                        if let Some(load) = load_ref.as_mut() {
                            let (over, line) =
                                step_sort_load(load, &partial_results, &partial_current);
                            if !line.is_empty() {
                                startup_status.set_text(&line);
                            }
                            if over {
                                // Done, or stopped at the ceiling: either way
                                // there is nothing further to fetch for this
                                // query and column, so a reverse click on the
                                // same header re-sorts locally.
                                if !load.set.failed() {
                                    loaded_column.set(Some(load.column));
                                }
                                *load_ref = None;
                            }
                        } else if want_full.get() {
                            want_full.set(false);
                            // Nothing to do when no header is sorted, when the
                            // set is already complete, or when the list
                            // already holds every match.
                            let sorted_column = partial_results.sort_state().map(|(c, _)| c);
                            let worth_it = created_range.borrow().is_none()
                                && sorted_column.is_some()
                                && loaded_column.get() != sorted_column
                                && last_total.get() > partial_current.borrow().len() as u64;
                            if worth_it {
                                if let Some((q, opts)) = last_search.borrow().clone() {
                                    let set = FullSet::new(
                                        q,
                                        opts,
                                        last_total.get(),
                                        MAX_SORTED_ROWS,
                                    );
                                    let started = encode_request(&set.request()).and_then(|line| {
                                        let stream = connect_stream()?;
                                        PendingRequest::start(stream, &line)
                                    });
                                    match started {
                                        Ok(pending) => {
                                            // The reply starts at hit 0 and
                                            // carries no offset, so the rows
                                            // on screen cannot be kept without
                                            // showing hits twice: clear them
                                            // and let the first hits
                                            // repopulate (one tick later).
                                            partial_results.set_rows(&[]);
                                            partial_current.borrow_mut().clear();
                                            startup_status.set_text(&set.status_line());
                                            *load_ref = Some(SortLoad {
                                                column: sorted_column.unwrap_or(PATH_COLUMN),
                                                set,
                                                pending,
                                                consumed: 0,
                                                staged: Vec::new(),
                                                painted: std::time::Instant::now(),
                                            });
                                        }
                                        Err(e) => {
                                            startup_status.set_text(&format!(
                                                "sort load failed: {e}"
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // 1c. Live mode: keep the feed subscribed to the boxes on
                    //     screen, and fold a burst of daemon change events into
                    //     one re-query. Off means the socket is dropped here, so
                    //     the daemon sees the disconnect and stops resolving
                    //     paths for us.
                    if !live_wanted.get() && live_run.borrow().is_active() {
                        live_run.borrow_mut().stop();
                    }
                    if live_wanted.get() {
                        let quick = quick_box.get_text().unwrap_or_default();
                        let adv = adv_box.get_text().unwrap_or_default();
                        if let Some(q) = effective_query(&quick, &adv) {
                            let opts = gather_options(
                                &live_case,
                                &live_fuzzy,
                                &live_count,
                                &live_distance,
                            );
                            let key = crate::LiveKey::new(&q, &opts);
                            // Stepped into a binding first: a `match` on the
                            // borrow itself holds the `RefMut` for every arm,
                            // and the Refresh arm needs to borrow the runner
                            // again to take the changes (`RefCell` panics).
                            let step = live_run.borrow_mut().step(
                                &live_socket,
                                &key,
                                std::time::Instant::now(),
                            );
                            match step {
                                crate::LiveStep::Refresh => {
                                    // Apply what the daemon sent instead of
                                    // re-running the query. The feed's events
                                    // are already filtered by this same query
                                    // (§11), so a create is a row to add and a
                                    // delete a row to remove: the list updates
                                    // in place, keeping its scroll and the
                                    // selected file. A `resync` (ring overflow,
                                    // generation bump) — or a query whose
                                    // matching the feed does not carry (fuzzy,
                                    // a created-date range, count-only) — falls
                                    // back to the whole-query path below.
                                    let changes = live_run.borrow_mut().take_changes();
                                    let local = !live_fuzzy.is_active()
                                        && !live_count.is_active()
                                        && created_range.borrow().is_none();
                                    if !(local
                                        && !changes.is_empty()
                                        && apply_live_changes(
                                            &changes,
                                            &partial_results,
                                            &partial_current,
                                        ))
                                    {
                                        // Same path as Return/Search: the next
                                        // tick issues the query, so the rows and
                                        // the status line come from one place.
                                        start_now.store(true, Ordering::SeqCst);
                                    }
                                }
                                crate::LiveStep::Unsupported => {
                                    live_wanted.set(false);
                                    live_button.set_active(false);
                                    startup_status.set_text(
                                        "live mode needs daemon protocol 6 — this daemon cannot stream changes",
                                    );
                                }
                                crate::LiveStep::Failed(e) => {
                                    live_wanted.set(false);
                                    live_button.set_active(false);
                                    startup_status.set_text(&format!("live: {e}"));
                                }
                                crate::LiveStep::Idle => {}
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
                    let immediate = start_now.swap(false, Ordering::SeqCst);
                    if !immediate {
                        // No keystroke pending, or still typing: either way
                        // there is nothing to issue on this tick. (A Search
                        // press while a scan is in flight is `immediate`,
                        // handled above.)
                        if !debounce.due() {
                            return true;
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
                        debounce.clear();
                        // An emptied box shows no results, but the daemon may
                        // still be behind — say so rather than rendering a
                        // clean-looking "0 matches" from a stale view.
                        let remembered = *last_lag.borrow();
                        // An emptied box abandons a whole-set load: its hits
                        // belong to a query that no longer exists.
                        *sort_load.borrow_mut() = None;
                        loaded_column.set(None);
                        want_full.set(false);
                        suggesting.set(false);
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
                    debounce.clear();
                    // A fresh query is not a suggestion; the red line goes away.
                    suggesting.set(false);

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
                            // A new query makes any whole-set load stale: its
                            // hits are the previous query's. Keep the rows it
                            // already loaded; drop the socket.
                            *sort_load.borrow_mut() = None;
                            loaded_column.set(None);
                            want_full.set(false);
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
            let suggesting = suggesting.clone();
            entry
                .connect_changed(move || {
                    let quick = quick_box.get_text().unwrap_or_default();
                    let adv = adv_box.get_text().unwrap_or_default();
                    if box_change_clears(&quick, &adv) {
                        suggesting.set(false);
                        render(Ok(SearchOutcome::default()));
                        return;
                    }
                    arm();
                })
                .ok();
        }

        // The date fields re-run the search once the pair parses (or both are
        // blank). A half-typed date is not an error yet: Search/Return reports
        // it, so typing `2026-0` does not flash an error on every keystroke.
        // The handler lives in `created_ui` so the GTK test exercises it too.
        created_ui::wire(&created, arm.clone());

        // The max-distance `-`/`+` buttons are the one way to change the edit
        // budget that does not depend on the field taking keystrokes or the
        // wheel (AppKit wires neither here — see `crate::distance_ui`). A
        // click is a deliberate edit like a keystroke, so it arms the same
        // debounce; with Fuzzy unticked the value changes but the search it
        // would affect is off anyway, and `gather_options` ignores it.
        crate::distance_ui::wire(&distance, arm.clone());

        // A setting that shapes the request refreshes the rows it shaped:
        // ticking Fuzzy (or Case sensitive, or Count only) with a query on
        // screen re-asks that query through the same debounce a keystroke arms,
        // instead of leaving rows computed without the option until the next
        // keypress. With both boxes empty there is nothing to re-ask, and an
        // empty query would match everything (`crate::setting_change_refreshes`).
        crate::settings_ui::wire(
            &[&opt_case, &opt_fuzzy, &opt_count],
            &query,
            &advanced_query,
            arm.clone(),
        );
    }

    // The Live toggle records intent only; the pump above owns the socket, so
    // turning it on subscribes with whatever the boxes say on the next tick and
    // turning it off drops the feed.
    {
        let wanted = live_wanted.clone();
        let button = live.button.clone();
        let status = status.clone();
        live.button
            .connect_toggled(move || {
                let on = button.is_active();
                wanted.set(on);
                status.set_text(if on { crate::LIVE_STATUS } else { "live: off" });
            })
            .ok();
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
        let want_full = want_full.clone();
        if let Err(e) = results.connect_button_press_event(move |button, x, y, event| {
            // A header click has just re-sorted the rows inside the list, so
            // the row order `current` was built from is stale. Re-read it
            // before any index is used. (Buttons arrive normalized
            // (`normalize_press_button`): a middle-press is already button 1,
            // so only the secondary button needs handling below — a primary
            // click selects its row inside the compat list itself.)
            *current.borrow_mut() = cb_results.displayed_column(PATH_COLUMN);
            // A header click re-sorted the rows the list holds, which is only
            // the first page. Ask for the whole match set so the sort covers
            // every hit; the pump starts it, and knows when the set is already
            // loaded so that a reverse click just re-sorts locally.
            if button == 1 && y < cb_results.header_height() {
                want_full.set(true);
            }
            if button != 3 {
                return;
            }
            // Right-click acts on the row under the pointer, and the menu is
            // the shared model's output rather than a second list that has to
            // agree with it: hit test → `row_menu_plan` → native items. A press
            // with no row under it opens nothing, and an action the resolved row
            // cannot support (its file is gone since the index dated it) comes
            // out insensitive and dispatches nothing.
            let row = cb_results.row_at(x, y);
            let (path, items) = match row_menu_plan(
                button,
                row.map(|r| r as i32),
                &current.borrow(),
                |p| std::path::Path::new(p).exists(),
            ) {
                Some(plan) => plan,
                None => return,
            };
            // Show the selection too, so it is obvious which row the menu is for.
            // A plan exists, so `row` held a valid in-bounds row.
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
            for item in items {
                let action = item.action;
                let enabled = item.enabled;
                let path = path.clone();
                let status = status.clone();
                let results = cb_results.clone();
                let menu_fired = menu_fired.clone();
                // The item keeps the model's sensitivity: a greyed-out item is
                // not a silent item, and the guard below means a host that
                // delivered the choice anyway would still not act on it.
                menu.append_item_enabled(
                    item.label,
                    enabled,
                    Box::new(move || {
                        if !enabled || *menu_fired.borrow() {
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

    // Dragging a row out of the window hands its path to whatever is under the
    // pointer: a `file://` URI for a terminal or file manager, and the plain
    // path as text. The payload is read at drop time, so it follows a header
    // click's re-sort and the current selection.
    {
        let drag_results = results.clone();
        if let Err(e) = results.enable_uri_drag_source(
            move || {
                drag_results
                    .selected_path(PATH_COLUMN)
                    .map(|path| drag_payload(&path))
            },
            || None,
        ) {
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
                            let mut body = format!("connected ({entries} entries, {backend})");
                            // Per-root health, with the daemon's reason: this is
                            // where to look when the status line says a root is
                            // stale and the user asks what that means.
                            if let Ok(roots) = DaemonClient::new().roots_health() {
                                for root in &roots {
                                    match root.degraded_reason() {
                                        Some(why) => body.push_str(&format!(
                                            "\n{}: degraded — {why}",
                                            root.path
                                        )),
                                        None => body.push_str(&format!("\n{}: ok", root.path)),
                                    }
                                }
                            }
                            body
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
    trace_step!("menubar");
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
        for (menu_label, items) in crate::menubar_model() {
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

    trace_step!("layout");
    let layout = app.create_box(Orientation::Vertical, 1)?;
    layout.set_vexpand(true);
    layout.set_hexpand(true);
    layout.append(&menubar);
    layout.append(&query_row);
    layout.append(&advanced);
    layout.append(&status);
    layout.append(&results_scroll);

    trace_step!("attach content view");
    win.set_child(&layout);
    trace_step!("present");
    win.present();
    // After present, not before: presenting runs `show_all` on GTK, which would
    // show the advanced-search body again. The foldout opens from the Search
    // menu ("Advanced search"), which toggles this same call.
    advanced.set_expanded(false);
    trace_step!("grab focus");

    // The Basic Search box owns the keyboard from the start: the window opens
    // ready to type, with no click needed. After `present()` — grabbing focus
    // on an unmapped widget is a silent no-op in GTK, so this must come after.
    query.grab_focus();

    // A picture of the AppKit window, for CI: a macOS runner can start this
    // process but cannot see it, and `screencapture` photographs the desktop
    // rather than this view. Env-gated like `FF_GUI_TRACE`, so it costs a
    // normal run nothing, and asked for only after `present()` — the tree has
    // been laid out by then, which is what makes the image match the window.
    #[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
    if let Ok(path) = std::env::var("FF_GUI_SCREENSHOT") {
        // Real search results, not a fixture: ask the daemon the question a
        // user's first search would and render what it answers, so this picture
        // is the daemon -> rows -> pixels path. A host with no daemon captures
        // the empty list it really has, and says so, instead of inventing rows.
        let query =
            std::env::var("FF_GUI_SCREENSHOT_QUERY").unwrap_or_else(|_| "ff".to_string());
        let client = DaemonClient::new();
        let waited = std::time::Instant::now();
        let mut answered = None;
        let mut last_error = String::new();
        // The daemon binds before it has finished indexing, so ask again until
        // it has hits to show; a missing daemon fails on this deadline instead
        // of hanging the run.
        while waited.elapsed() < std::time::Duration::from_secs(30) {
            match client.search(&query, MAX_ROWS) {
                Ok(outcome) if !outcome.paths.is_empty() => {
                    answered = Some(outcome);
                    break;
                }
                Ok(outcome) => answered = Some(outcome),
                Err(e) => last_error = e.to_string(),
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        match &answered {
            Some(outcome) => {
                let shown = outcome.paths.len().min(MAX_ROWS);
                results.set_rows(&rows_to_cells(&outcome.paths[..shown]));
                status.set_text(&format!(
                    "{shown} of {} matches for {query:?} (screenshot from the daemon)",
                    outcome.total
                ));
                eprintln!(
                    "ff-gui: screenshot rows: {shown} of {} from the daemon for {query:?} at {}",
                    outcome.total,
                    client.socket().display()
                );
            }
            None => {
                results.set_rows(&[]);
                status.set_text(&format!(
                    "0 matches for {query:?} (screenshot: no daemon at {}: {last_error})",
                    client.socket().display()
                ));
                eprintln!(
                    "ff-gui: screenshot rows: 0 (no daemon at {}: {last_error})",
                    client.socket().display()
                );
            }
        }
        // Same window size as the Linux capture, when asked for, so the two
        // PNGs can be compared directly.
        let size = std::env::var("FF_GUI_SCREENSHOT_SIZE")
            .ok()
            .and_then(|spec| {
                let (w, h) = spec.split_once('x')?;
                Some((w.trim().parse::<f64>().ok()?, h.trim().parse::<f64>().ok()?))
            });
        // Overlay scrollers fade out, so a fresh window would show no scroller
        // at all and "the scrollbar works" could not be seen in the pixels. Only
        // this path asks for the persistent style; a normal run keeps AppKit's.
        rswidgets::backends_macos_adapter::use_legacy_scrollers(win.raw_handle());
        match rswidgets::backends_macos_adapter::write_view_png(win.raw_handle(), &path, size) {
            Some((w, h, bytes)) => {
                eprintln!("ff-gui: screenshot {path} ({w}x{h}, {bytes} bytes)");
            }
            None => eprintln!("ff-gui: screenshot {path} failed"),
        }
    }

    trace_step!("run loop");
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
/// [`crate::open_in_terminal_command`]).
fn run_row_action(action: RowAction, path: &str, results: &impl TextViewLike) -> String {
    // "Open in terminal" carries host-specific prefix flags: when the
    // alternatives link resolves to zutty — whose `9x18` default aborts when
    // `xfonts-base` is not installed — splice `-font <installed monospace>`
    // in front of `-e` so the emulator starts at all. Every other action
    // launches exactly what `RowAction::command` says.
    let (program, args) = crate::launch_command(action, path);
    if program.is_empty() {
        // Clipboard actions need no external command; they differ only in what
        // they copy.
        let text = match action {
            RowAction::CopyFolderPath => containing_dir(path),
            _ => path.to_string(),
        };
        return match results.clipboard_set_text(&text) {
            // Name what is now on the clipboard: the whole path, because
            // "copied notes.txt" says nothing about *which* notes.txt when a
            // search returns several.
            Ok(()) => format!("copied {text}"),
            Err(e) => format!("copy failed: {e}"),
        };
    }
    // The children own their own windows and are never read from: detaching
    // stdio keeps them from holding this process's pipes open (a `dolphin` or
    // `kde-open` that outlives the app would otherwise keep a shell waiting on
    // the pipe for as long as the window is up) and keeps their diagnostics out
    // of the app's output.
    match std::process::Command::new(&program)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        // Name the program: "Open containing folder → photo.png (dolphin)"
        // says which helper actually ran, which is the difference between
        // "the app did nothing" and "the app started a file manager".
        Ok(_) => format!(
            "{} → {} ({program})",
            action.label(),
            short_path(path)
        ),
        Err(e) => format!("{} failed to start {program}: {e}", action.label()),
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
    // The mapping itself lives in the library (`crate::gather_options`) so a
    // test can drive the foldout's real widgets through it; this is only the
    // widget-side half: read each control, then hand over plain values.
    let distance_text = distance.get_text().unwrap_or_default();
    crate::gather_options(case.is_active(), fuzzy.is_active(), count.is_active(), &distance_text)
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
    use super::{Active, Expandable, TextValue};

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
}

// Keyed on the feature alone, not on `not(feature = "gtk")`: zork wins backend
// selection whenever its feature is on (every other `App::create_*` arm
// excludes `zork`), so `--features zork` on top of the defaults still hands the
// app zork's adapter types. Requiring GTK to be *off* left those types with no
// trait impls at all in exactly that build.
#[cfg(feature = "zork")]
mod widget_traits {
    use super::{Active, Expandable, TextValue};

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
}

// The other targets build the same UI from their own adapter types. These are
// not built on this host, but must compile there.
#[cfg(all(windows, not(feature = "zork")))]
mod widget_traits {
    use super::{Active, Expandable, TextValue};
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
}

#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
mod widget_traits {
    use super::{Active, Expandable, TextValue};
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

// macOS builds the same UI from the AppKit adapter types: the checkbox and
// entry traits only. The results list — and with it the clipboard hook — is
// the compat layer's on every backend (see below).
#[cfg(all(target_os = "macos", not(feature = "zork")))]
mod widget_traits {
    use super::{Active, Expandable, TextValue};
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
}

// The results list is the compat `TreeView` on *every* backend, so the
// clipboard hook is one implementation rather than one per adapter. The compat
// layer routes it to the running toolkit's own clipboard (GTK's CLIPBOARD
// selection, AppKit's NSPasteboard); only a build with no desktop toolkit has
// no clipboard to write to, and it reports that instead of pretending the copy
// happened.
impl TextViewLike for rswidgets::compat::TreeView {
    fn clipboard_set_text(&self, text: &str) -> Result<(), rswidgets::Error> {
        rswidgets::compat::TreeView::clipboard_set_text(self, text)
    }
}

/// The character an icon name falls back to when there is no canvas to paint on.
///
/// The results list does **not** use these: `rswidgets::compat::TreeView` paints
/// a pictogram per file kind (`paint_icon`). This is what a text backend — the
/// `zork` fallback, which renders the list as aligned text — prints in the icon
/// column, and it is why the glyphs are one ASCII character each.
///
/// ASCII only, deliberately. The canvas draws with the base sans font and has
/// no glyph fallback, so a symbol the font happens to lack (Noto Sans has no
/// `\u{2261}`, for one) comes out as a blank box. A letter always renders.
fn icon_glyph(icon: &str) -> &'static str {
    match icon {
        "folder" => "d",
        "text-x-rust" => "R",
        "text-x-python" => "P",
        "text-x-javascript" | "application-json" => "J",
        "text-x-typescript" => "T",
        "text-x-csrc" | "text-x-c++src" => "C",
        "text-x-go" => "G",
        "text-x-java" => "J",
        "text-x-ruby" => "r",
        "text-x-script" => "$",
        "text-markdown" => "M",
        "text-html" => "H",
        "text-css" => "s",
        "image-x-generic" => "i",
        "audio-x-generic" => "a",
        "video-x-generic" => "v",
        "x-office-spreadsheet" => "t",
        "application-x-executable" => "x",
        "application-x-object" => "o",
        "application-pdf" => "p",
        "package-x-generic" => "z",
        _ => "-",
    }
}

/// A whole-set sort load in flight.
struct SortLoad {
    /// The column this set was loaded for, so a later click on the same header
    /// (a reverse) can re-sort locally rather than fetch the set again.
    column: usize,
    /// The query state machine: what to ask, and how far it has got.
    set: FullSet,
    /// The reply being stepped from the 30 ms pump.
    pending: PendingRequest,
    /// Hits absorbed but not yet merged into the list.
    staged: Vec<String>,
    /// Reply records already folded in: `records()` keeps the whole reply, so
    /// re-reading all of it every tick would be quadratic.
    consumed: usize,
    /// When the list was last repainted, which throttles the merging to ~4/s.
    painted: std::time::Instant,
}

/// Step a whole-set sort load by one tick.
///
/// Returns `(over, status line)`. `over` means the reply has ended *and* every
/// staged hit has been merged, so the caller may drop the load. Merging is
/// bounded per call: hits are handed to `append_rows`, which sorts the batch and
/// merges it into the rows already on screen (keeping the active sort). The
/// list draws only the rows on screen, so holding six figures of rows costs
/// nothing per frame.
fn step_sort_load(
    load: &mut SortLoad,
    results: &rswidgets::compat::TreeView,
    current: &Rc<RefCell<Vec<String>>>,
) -> (bool, String) {
    /// Hits merged per tick: big enough that a 600 k-hit reply finishes in a
    /// handful of ticks, small enough that one tick stays a few milliseconds.
    const MERGE_BATCH: usize = 50_000;
    /// How often the list and the status line are refreshed while loading.
    const REPAINT: std::time::Duration = std::time::Duration::from_millis(250);

    let reply_over = match load.pending.advance() {
        Ok(Progress::Finished) => true,
        Ok(_) => false,
        Err(e) => {
            // The reply never reached Done. Say why in the status line and keep
            // whatever rows already made it onto the list.
            load.set.fail(&format!("{e}"));
            true
        }
    };

    // Fold in only the records that arrived since the last tick.
    let consumed = load.consumed;
    let arrived = load.pending.records().len();
    if arrived > consumed {
        let fresh = load.set.absorb(&load.pending.records()[consumed..]);
        load.staged.extend(fresh);
        load.consumed = arrived;
    }

    let due = reply_over || load.painted.elapsed() >= REPAINT || load.staged.len() >= MERGE_BATCH;
    if due && !load.staged.is_empty() {
        let take = MERGE_BATCH.min(load.staged.len());
        let batch: Vec<String> = load.staged.drain(..take).collect();
        results.append_rows(&rows_to_cells(&batch));
        // The list sorted the batch into place, so read the order back rather
        // than assuming it.
        *current.borrow_mut() = results.displayed_column(PATH_COLUMN);
        load.painted = std::time::Instant::now();
    }

    let over = reply_over && load.staged.is_empty();
    // Repaint the status line at the same ~4/s rate as the rows: the pump runs
    // every 30 ms and rewriting an unchanged line that often is pure churn.
    let line = if due || over {
        load.set.status_line()
    } else {
        String::new()
    };
    (over, line)
}

/// Apply a live feed's `create`/`delete` events to the rows already on screen.
///
/// The daemon filtered these against the subscription's own query (§11), so no
/// row is guessed: a create is the row to add and a delete the row to remove.
/// Only those rows are touched — the rest keep their order, the scroll offset
/// and the selected file, which is the difference between a live list and one
/// rebuilt from scratch on every change.
///
/// Returns `false`, with nothing applied, when the batch carries a `resync`
/// (the daemon's feed has a gap only a fresh query can close), so the caller
/// falls back to re-running the query.
fn apply_live_changes(
    changes: &[crate::LiveChange],
    results: &rswidgets::compat::TreeView,
    current: &Rc<RefCell<Vec<String>>>,
) -> bool {
    use crate::LiveChange;
    if changes.iter().any(|c| matches!(c, LiveChange::Resync)) {
        return false;
    }
    // A working copy of what the list shows, so a batch with several creates
    // counts and de-duplicates against the rows this same batch added.
    let mut shown = current.borrow().clone();
    for change in changes {
        match change {
            LiveChange::Created(path) => {
                // A full page stays full: a new match sorted past the last row
                // would sit outside the page the daemon answered, so the list
                // never grows beyond what a search would have shown.
                if shown.len() >= MAX_ROWS || shown.iter().any(|p| p == path) {
                    continue;
                }
                results.append_rows(&rows_to_cells(std::slice::from_ref(path)));
                shown.push(path.clone());
            }
            LiveChange::Deleted(path) => {
                results.remove_rows_matching(PATH_COLUMN, path);
                shown.retain(|p| p != path);
            }
            LiveChange::Resync => unreachable!("checked above"),
        }
    }
    *current.borrow_mut() = results.displayed_column(PATH_COLUMN);
    true
}

/// Build the three-column cells (icon, name, path) for a list of paths.
///
/// The icon is a theme name plus the glyph the canvas draws in its place.
fn rows_to_cells(paths: &[String]) -> Vec<Vec<CellValue>> {
    rows_from_paths(paths)
        .into_iter()
        .map(|r| {
            let glyph = icon_glyph(&r.icon);
            vec![
                CellValue::Icon {
                    name: r.icon,
                    fallback: glyph.to_string(),
                },
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
        // The status names the exact text that is on the clipboard now.
        assert_eq!(msg, "copied /a/b/notes.txt");

        // "Copy folder path" copies the directory, and says so.
        let view = FakeView(RefCell::new(None));
        let msg = run_row_action(RowAction::CopyFolderPath, "/a/b/notes.txt", &view);
        assert_eq!(view.0.borrow().as_deref(), Some("/a/b"));
        assert_eq!(msg, "copied /a/b");

        // A copy through the real toolkit must not answer with the "not wired
        // up" fallback: that text belongs to a backend with no clipboard at
        // all, and a GTK/macOS build reaching it would be a routing bug.
        for msg in [
            run_row_action(RowAction::CopyPath, "/a/b/notes.txt", &FakeView(RefCell::new(None))),
            run_row_action(RowAction::CopyFolderPath, "/a/b/notes.txt", &FakeView(RefCell::new(None))),
        ] {
            assert!(
                !msg.contains("not wired up"),
                "the copy path fell through to the no-clipboard fallback: {msg}"
            );
        }
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
        // Exactly three columns — icon, name, path. The canvas list draws the
        // first cell, so a fourth hidden value would be dead weight (and the
        // hidden extension column went with the `GtkListStore`).
        assert_eq!(row.len(), 3);
        match &row[0] {
            CellValue::Icon { name, fallback } => {
                assert_eq!(name, "text-x-rust");
                // The theme name travels with the cell; the canvas draws the
                // glyph, which is what the column actually shows.
                assert_eq!(fallback, "R");
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
                // A folder draws its own glyph, not the generic-file one.
                assert_eq!(fallback, "d");
            }
            _ => panic!("expected an icon"),
        }
    }

    #[test]
    fn icon_glyphs_are_one_ascii_character_each() {
        // The icon column is a fixed narrow fraction of the list, so a glyph
        // that is more than one character wide would be clipped. ASCII only:
        // the canvas has no glyph fallback, so a symbol the base font lacks
        // (\u{2261}, \u{25b8}, \u{25a3}) draws as an empty box.
        for icon in [
            "folder",
            "text-x-rust",
            "text-x-python",
            "text-x-script",
            "text-x-json",
            "text-x-generic",
            "image-x-generic",
            "audio-x-generic",
            "video-x-generic",
            "x-office-spreadsheet",
            "application-x-executable",
            "application-x-object",
            "package-x-generic",
            "application-pdf",
            "something-new",
        ] {
            let glyph = icon_glyph(icon);
            assert_eq!(glyph.chars().count(), 1, "{icon} -> {glyph:?}");
            assert!(glyph.is_ascii(), "{icon} -> {glyph:?} is not ASCII");
        }
    }

    #[test]
    fn rows_to_cells_is_empty_for_no_paths() {
        assert!(rows_to_cells(&[]).is_empty());
    }
}
