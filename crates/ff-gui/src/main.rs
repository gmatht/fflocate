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

use ff_gui::{DaemonClient, GuiError, SearchOutcome};

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

    // The Search button runs the query synchronously: the protocol is
    // request/response, so there is nothing to await. (A push feed would use the
    // `subscribe` op, which is still a v1.5 preview in the daemon.)
    {
        let query = query.clone();
        let status = status.clone();
        let results = results.clone();
        search.on_click(move || {
            let q = query.get_text().unwrap_or_default();
            match run_query(&q) {
                Ok(outcome) => {
                    status.set_text(&outcome.status_line(outcome.paths.len().min(MAX_ROWS)));
                    results.set_text(&render_rows(&outcome));
                }
                Err(e) => {
                    status.set_text(&format!("{e}"));
                    results.set_text("");
                }
            }
        })?;
    }

    query_row.append(&query);
    query_row.append(&search);

    let layout = app.create_box(Orientation::Vertical, 1)?;
    layout.append(&query_row);
    layout.append(&status);
    layout.append(&results);

    win.set_child(&layout);
    win.present();

    println!("ff-gui — findex search browser (backend: {BACKEND})");
    println!("Enter a query in the entry, then invoke Search.");
    app.run().map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
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
