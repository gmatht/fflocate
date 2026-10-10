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
    effective_query, row_at_line, DaemonClient, GuiError, RowAction, SearchOutcome,
    ADVANCED_SYNTAX_HELP,
};

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

    let query_row = app.create_box(Orientation::Horizontal, 2)?;
    let query = app.create_entry()?;
    query.set_text("");
    let search = app.create_button("Search")?;
    let status = app.create_label("type a query and press Search")?;
    // A TextView, not a Label: it renders the hits as a scrollable list
    // instead of one wrapping paragraph.
    let results = app.create_textview()?;
    // A TextView is a scrolling, line-per-row list on gtk. The display-free
    // zork shim implements only set_text/get_text/set_wrap_mode, so the
    // sizing calls are gtk-only.
    #[cfg(feature = "gtk")]
    {
        results.set_vexpand(true);
        results.set_wrap_mode(0); // no wrapping: one hit per line
    }

    // The advanced-search foldout: a native disclosure holding a free-form
    // query field (with the full syntax) and a reminder of that syntax. Its
    // contents are merged into the effective query, so what runs is always one
    // readable string.
    let advanced = app.create_expander("Advanced search")?;
    let advanced_query = app.create_entry()?;
    advanced_query.set_text("");
    let advanced_help = app.create_label(ADVANCED_SYNTAX_HELP)?;
    let advanced_box = app.create_box(Orientation::Vertical, 2)?;
    advanced_box.append(&advanced_query);
    advanced_box.append(&advanced_help);
    advanced.set_child(&advanced_box);
    advanced.set_expanded(false);

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

    // The search runs synchronously: the protocol is request/response, so there
    // is nothing to await. (A push feed would use the `subscribe` op, which is
    // still a v1.5 preview in the daemon.) The same closure backs both the
    // Search button and the Return key, so the two cannot drift apart.
    let run_search: Rc<RefCell<Box<dyn FnMut()>>> = {
        let query = query.clone();
        let advanced_query = advanced_query.clone();
        let status = status.clone();
        let results = results.clone();
        let current = current.clone();
        Rc::new(RefCell::new(Box::new(move || {
            let quick = query.get_text().unwrap_or_default();
            let adv = advanced_query.get_text().unwrap_or_default();
            let (q, shadowed) = effective_query(&quick, &adv);
            match run_query(&q) {
                Ok(outcome) => {
                    let mut line = outcome.status_line(outcome.paths.len().min(MAX_ROWS));
                    if shadowed {
                        // Say which query ran: the two boxes disagreeing must
                        // not be silent.
                        line.push_str("  [advanced query]");
                    }
                    status.set_text(&line);
                    results.set_text(&render_rows(&outcome));
                    // Record only the rows actually shown (a "no matches" view
                    // shows a placeholder line, which is not a path).
                    *current.borrow_mut() = outcome.paths.iter().take(MAX_ROWS).cloned().collect();
                }
                Err(e) => {
                    status.set_text(&format!("{e}"));
                    results.set_text("");
                    current.borrow_mut().clear();
                }
            }
        })))
    };

    {
        let run_search = run_search.clone();
        search.on_click(move || {
            if let Ok(mut f) = run_search.try_borrow_mut() {
                f();
            }
        })?;
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
            // time `drag-data-get` runs.)
            if button == 1 {
                let rows = current.borrow();
                *drag_row.borrow_mut() = cb_results
                    .line_at(x, y)
                    .and_then(|line| row_at_line(&rows, line))
                    .map(|p| p.to_string());
                return;
            }
            if button != 3 {
                return;
            }
            let Some(line) = cb_results.line_at(x, y) else {
                return;
            };
            // `row_at_line` returns None past the last row, so a click on
            // blank space (or the "… N more" trailer) does nothing rather than
            // acting on a neighbour.
            let path = match row_at_line(&current.borrow(), line) {
                Some(p) => p.to_string(),
                None => return,
            };
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
        if let Err(e) = results.enable_uri_drag_source(move || {
            drag_row
                .borrow()
                .as_deref()
                .map(ff_gui::drag_payload)
        }) {
            // Not fatal: the list still works, only dragging is unavailable.
            status.set_text(&format!("drag and drop unavailable: {e}"));
        }
    }

    query_row.append(&query);
    query_row.append(&search);

    let layout = app.create_box(Orientation::Vertical, 1)?;
    layout.append(&query_row);
    layout.append(&advanced);
    layout.append(&status);
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

/// Run one search against the daemon.
fn run_query(query: &str) -> Result<SearchOutcome, GuiError> {
    let client = DaemonClient::new();
    client.search(query, FETCH_LIMIT)
}

/// Render the result list: one path per line.
///
/// A list, not prose: the hits go into a TextView, so each line is a row. The
/// daemon already returns absolute paths, so they are shown verbatim (a
/// basename-first `name — dir` split reads as prose and wastes the column).
fn render_rows(outcome: &SearchOutcome) -> String {
    if outcome.paths.is_empty() {
        return "(no matches)".to_string();
    }
    let mut lines: Vec<String> = outcome
        .paths
        .iter()
        .take(MAX_ROWS)
        .cloned()
        .collect();
    if outcome.total as usize > lines.len() {
        lines.push(format!("… {} more", outcome.total as usize - lines.len()));
    }
    lines.join("\n")
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
    fn render_empty_is_explicit() {
        let o = SearchOutcome::default();
        assert_eq!(render_rows(&o), "(no matches)");
    }

    #[test]
    fn render_is_a_list_one_path_per_line() {
        let o = SearchOutcome {
            paths: vec!["/a/x.rs".into(), "/b/y.rs".into(), "/c/z.rs".into()],
            total: 3,
            fresh: true,
            ..Default::default()
        };
        assert_eq!(render_rows(&o), "/a/x.rs\n/b/y.rs\n/c/z.rs");
    }

    #[test]
    fn render_truncates_and_counts_the_remainder() {
        let paths: Vec<String> = (0..MAX_ROWS + 50).map(|i| format!("/t/f{i}.rs")).collect();
        let total = paths.len() as u64;
        let o = SearchOutcome { paths, total, fresh: true, ..Default::default() };
        let text = render_rows(&o);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), MAX_ROWS + 1);
        assert!(lines.last().unwrap().contains("50 more"), "{:?}", lines.last());
    }
}
