//! Created from–to, end to end on GTK, through the app's own widgets and code.
//!
//! The field is only real if typing dates into the Advanced foldout re-runs the
//! search and the rows on screen are the ones inside the range. So this test
//! does not model the foldout: it calls `ff_gui::created_ui::build` — the same
//! function `main.rs` calls — and wires it with `created_ui::wire`, the same
//! handler the app connects to both entries' `changed` signal. Typing is a real
//! GTK `set_text`, which emits the signal a keystroke emits; the pump delivers
//! it; the handler arms a real `ff_gui::Debounce`, whose `due` gates a query.
//!
//! The query goes over a real socket to a protocol server answering with the
//! fixture files, so the rows come from `DaemonClient` → `outcome_from_records`
//! → `rows_from_paths` → `TreeView::set_rows`, and the range is applied by
//! `ff_gui::apply_created_range`, the function `main.rs`'s renderer calls. The
//! files are real files, so their creation times are the ones the filter reads.
//!
//! What is asserted:
//!   * the row the foldout builds has the "created from:" / "to:" labels and
//!     two empty date fields;
//!   * a same-day range keeps the rows created today (the `to` day is inclusive
//!     to its last second, so a file made minutes ago still matches);
//!   * a `to` of yesterday drops them, and a `from` of tomorrow drops them —
//!     both bounds bite, not just one;
//!   * the coverage note (`0 of 3 matched`) reaches the status line;
//!   * a bad date and an inverted range are refused by `created_request_range`
//!     with the offending field named, and the change handler does not search;
//!   * the as-you-type partial paint filters by the same rule;
//!   * a truncated reply says how many matches it did not check.
//!
//! Needs a display with GTK3 (the `changed` signal is GTK's), so a headless run
//! returns early; CI runs the Linux interaction step under Xvfb.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ff_gui::{
    apply_created_range, created_request_range, created_ui, filter_partial_paths, format_day,
    rows_from_paths, DaemonClient, Debounce, SearchOptions,
};
use fflocate::proto::{Request, Response};
use rswidgets::compat::{CellValue, TreeView};
use rswidgets::prelude::*;

/// The query every fixture file matches.
const QUERY: &str = "created";

/// The fixture files, relative to the backend's root.
const FIXTURES: [&str; 3] = ["alpha-created.txt", "beta-created.rs", "sub/gamma-created.md"];

/// `main.rs`'s `FETCH_LIMIT`: the first page a search asks for.
const FETCH_LIMIT: usize = 201;

/// Pump GTK's main loop so a `changed` signal queued by `set_text` is
/// delivered, as `typing_gtk` and `menubar_gtk` do.
fn pump(loader: &gtk_dynamic_loader::Loader, iterations: usize) {
    let Some(gtk) = loader.libs.get("libgtk") else {
        return;
    };
    unsafe {
        // `gtk_main_iteration_do(FALSE)`: non-blocking, the real signature.
        let it = gtk.get::<unsafe extern "C" fn(i32) -> i32>(b"gtk_main_iteration_do");
        if let Ok(f) = it {
            for _ in 0..iterations {
                f(0);
            }
        }
    }
}

/// The three-column cells the app builds for a list of paths.
fn cells(paths: &[String]) -> Vec<Vec<CellValue>> {
    rows_from_paths(paths)
        .into_iter()
        .map(|r| {
            vec![
                CellValue::Icon {
                    name: r.icon,
                    fallback: "-".to_string(),
                },
                CellValue::Text(r.name),
                CellValue::Text(r.path),
            ]
        })
        .collect()
}

/// One column of every row, read back out of the list's own model.
fn column(list: &TreeView, col: usize) -> Vec<String> {
    (0..).map_while(|row| list.cell_text(row, col)).collect()
}

/// A protocol server on a real socket, answering every query with the fixture
/// paths and honouring `limit`, so the client half is the app's own.
struct FakeDaemon {
    socket: PathBuf,
    requests: Arc<AtomicUsize>,
    /// The socket's directory; kept alive for the life of the backend.
    _dir: tempfile::TempDir,
}

impl FakeDaemon {
    fn start(hits: Vec<String>) -> FakeDaemon {
        let dir = tempfile::tempdir().expect("socket dir");
        let socket = dir.path().join("ff.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let Ok(reader) = stream.try_clone() else {
                    continue;
                };
                let mut line = String::new();
                if BufReader::new(reader).read_line(&mut line).is_err() {
                    continue;
                }
                let Ok(req) = Request::from_line(line.trim()) else {
                    continue;
                };
                let limit = match req {
                    Request::Query { limit, .. } => limit,
                    _ => 0,
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let meta = Response::Meta {
                    gen: 1,
                    lsn: 0,
                    fresh: true,
                    elapsed_ms: 0.0,
                    stale_roots: Vec::new(),
                    catching_up: None,
                    lag_ms: None,
                    phase: Some("idle".to_string()),
                    crawl_entries: None,
                    crawl_dirs_pending: None,
                    backend: None,
                    entries: None,
                };
                let _ = writeln!(stream, "{}", meta.to_line());
                let take = if limit == 0 {
                    hits.len()
                } else {
                    limit.min(hits.len())
                };
                for (i, path) in hits.iter().take(take).enumerate() {
                    let hit = Response::Hit {
                        path: path.clone(),
                        id: i as u32,
                        // This fake daemon is deliberately old-style here: no
                        // creation minute, so the client must fall back to its
                        // own `stat` (which is what this test exercises).
                        created: None,
                    };
                    let _ = writeln!(stream, "{}", hit.to_line());
                }
                let done = Response::Done {
                    count: hits.len() as u64,
                    cursor: "c:1:0".to_string(),
                };
                let _ = writeln!(stream, "{}", done.to_line());
                let _ = stream.flush();
            }
        });
        FakeDaemon {
            socket,
            requests,
            _dir: dir,
        }
    }

    fn client(&self) -> DaemonClient {
        DaemonClient::with_socket(self.socket.clone())
    }
}

#[test]
fn the_created_fields_filter_the_rows_and_report_what_they_checked() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        println!("created range: skipped (no display)");
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => {
            println!("created range: skipped (no GTK)");
            return;
        }
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        println!("created range: skipped (not GTK3)");
        return;
    }
    let Ok(app) = App::init() else {
        println!("created range: skipped (App::init failed)");
        return;
    };

    // --- The fixture files, which the filter stats for real ---
    let root = tempfile::tempdir().expect("fixture root");
    let mut paths = Vec::new();
    for rel in FIXTURES {
        let path = root.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture dir");
        }
        std::fs::write(&path, b"created fixture\n").expect("fixture file");
        paths.push(path.to_string_lossy().into_owned());
    }
    let daemon = FakeDaemon::start(paths.clone());
    let client = daemon.client();

    // --- The foldout exactly as `run()` builds it ---
    let win = app.create_window().expect("window");
    win.set_title("ff-gui created range test");
    let list = app
        .create_treeview(&[("", true, true), ("Name", false, true), ("Path", false, true)])
        .expect("results list");
    let status = app.create_label("").expect("status");
    let advanced_query = app.create_entry().expect("advanced entry");
    advanced_query.set_text(QUERY);
    let created = created_ui::build(&app).expect("the created row");

    // The row is the foldout's, not a look-alike: its labels are the ones on
    // screen and its fields start empty.
    assert_eq!(
        created.from_label.get_text().as_deref(),
        Some(created_ui::FROM_LABEL),
        "the row must label its first field"
    );
    assert_eq!(
        created.to_label.get_text().as_deref(),
        Some(created_ui::TO_LABEL),
        "the row must label its second field"
    );
    assert_eq!(created.from.get_text().unwrap_or_default(), "");
    assert_eq!(created.to.get_text().unwrap_or_default(), "");

    // The age dropdowns are real controls on the row, not labels: a real GTK
    // combo box reports a selected index, and both start on the placeholder.
    assert_eq!(
        created.from_ages.get_active(),
        0,
        "the from dropdown must exist and start on the placeholder row"
    );
    assert_eq!(
        created.to_ages.get_active(),
        0,
        "the to dropdown must exist and start on the placeholder row"
    );
    assert!(
        created_ui::PLACEHOLDER.contains("HH:MM"),
        "the hint must advertise the clock-time form, got {:?}",
        created_ui::PLACEHOLDER
    );

    let foldout_box = app
        .create_box(Orientation::Vertical, 2)
        .expect("foldout box");
    foldout_box.append(&advanced_query);
    foldout_box.append(&created.row);
    let foldout = app
        .create_expander("Advanced search")
        .expect("advanced expander");
    foldout.set_child(&foldout_box);
    let layout = app
        .create_box(Orientation::Vertical, 2)
        .expect("window layout");
    // The list lives in a scroller in the app; `into_scrolled` consumes the
    // list, so keep a handle for the row reads.
    let list_scroll = list.clone().into_scrolled().expect("results scroller");
    layout.append(&foldout);
    layout.append(&list_scroll);
    layout.append(&status);
    win.set_child(&layout);
    win.present();
    pump(&loader, 50);

    // --- The app's own change handler, and a real debounce ---
    let debounce = std::rc::Rc::new(Debounce::new(Duration::from_millis(20)));
    {
        let d = debounce.clone();
        created_ui::wire(&created, move || d.arm());
    }

    let today = {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        format_day(now)
    };
    let yesterday = {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        format_day(now - 86_400)
    };
    let tomorrow = {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        format_day(now + 86_400)
    };

    // Type a date into a field and let GTK deliver `changed`, the way a
    // keystroke does.
    let type_dates = |from: &str, to: &str| {
        created.from.set_text(from);
        created.to.set_text(to);
        pump(&loader, 20);
        std::thread::sleep(Duration::from_millis(30));
        pump(&loader, 5);
    };

    // The status line the app composes: the daemon's own line, then the
    // created-range note. (`main.rs`'s renderer does exactly this, with the
    // same FETCH_LIMIT-sized page.)
    let search_and_paint = |limit: usize| {
        let range = created_request_range(
            &created.from.get_text().unwrap_or_default(),
            &created.to.get_text().unwrap_or_default(),
        )
        .expect("the typed dates parse");
        let mut outcome = client
            .search_with(QUERY, limit, &SearchOptions::default())
            .expect("the daemon answers");
        let note = apply_created_range(&mut outcome, range, false);
        let mut line = outcome.status_line_opts(outcome.paths.len().min(200), false);
        line.push_str(&note);
        status.set_text(&line);
        list.set_rows(&cells(&outcome.paths));
        line
    };

    // --- A same-day range keeps what was created today ---
    type_dates(&today, &today);
    assert!(
        debounce.due(),
        "typing a complete date must arm the search"
    );
    debounce.clear();
    let line = search_and_paint(FETCH_LIMIT);
    let rows = column(&list, 2);
    assert_eq!(
        rows, paths,
        "a range covering today must keep today's rows; status was {line:?}"
    );
    assert!(
        line.ends_with(&format!("[created {today}–{today}]")),
        "the status line must state the range that was applied, got {line:?}"
    );
    assert!(
        !line.contains("matched"),
        "nothing was filtered out, so the note must not claim it was: {line:?}"
    );

    // --- Yesterday as the upper bound drops them, and the note says so ---
    type_dates("", &yesterday);
    assert!(debounce.due(), "a changed upper bound must re-search");
    debounce.clear();
    let line = search_and_paint(FETCH_LIMIT);
    assert!(
        column(&list, 2).is_empty(),
        "files created today are outside a range ending yesterday; status was {line:?}"
    );
    assert!(
        line.contains(&format!("[created until {yesterday}: 0 of 3 matched]")),
        "the note must say what the bound hid, got {line:?}"
    );

    // --- Tomorrow as the lower bound drops them too: both ends bite ---
    type_dates(&tomorrow, "");
    assert!(debounce.due(), "a changed lower bound must re-search");
    debounce.clear();
    let line = search_and_paint(FETCH_LIMIT);
    assert!(
        column(&list, 2).is_empty(),
        "files created before tomorrow are outside a range starting tomorrow; \
         status was {line:?}"
    );
    assert!(
        line.contains(&format!("[created since {tomorrow}: 0 of 3 matched]")),
        "the note must name the open-ended lower bound, got {line:?}"
    );

    // --- The coverage note on a truncated page ---
    type_dates(&today, &today);
    assert!(debounce.due());
    debounce.clear();
    let line = search_and_paint(1);
    assert_eq!(
        column(&list, 2).len(),
        1,
        "a one-hit page holds one row; status was {line:?}"
    );
    assert!(
        line.contains("2 more matches not checked"),
        "a truncated page must say what it did not examine, got {line:?}"
    );

    // --- A bad date refuses, naming the field, and does not search ---
    let before = daemon.requests.load(Ordering::SeqCst);
    type_dates("yesterday", "");
    assert!(
        !debounce.due(),
        "a half-typed date must not search behind the user's back"
    );
    let err = created_request_range("yesterday", "").expect_err("a bad date is refused");
    let msg = format!("{err}");
    assert!(
        msg.starts_with("protocol: created from:"),
        "the refusal must name the field, got {msg:?}"
    );
    assert!(
        msg.contains("YYYY-MM-DD"),
        "the refusal must say what a date looks like, got {msg:?}"
    );
    assert_eq!(
        daemon.requests.load(Ordering::SeqCst),
        before,
        "a refused date must not reach the daemon"
    );

    // --- An inverted range refuses, naming both days ---
    let err = created_request_range(&today, "2020-01-01").expect_err("an inverted range is refused");
    assert_eq!(
        format!("{err}"),
        format!("protocol: created range: from {today} is after to 2020-01-01")
    );

    // --- The as-you-type paint uses the same rule ---
    let kept_today = filter_partial_paths(&paths, created_request_range(&today, &today).unwrap());
    assert_eq!(
        kept_today, paths,
        "the streaming paint must keep rows inside today's range"
    );
    let kept_past =
        filter_partial_paths(&paths, created_request_range("", &yesterday).unwrap());
    assert!(
        kept_past.is_empty(),
        "the streaming paint must drop rows outside the range before they are shown"
    );

    println!(
        "created range: {} rows from the daemon, today's range kept {}, yesterday's kept {}, \
         bad date refused by field, truncated note and streaming filter asserted",
        paths.len(),
        kept_today.len(),
        kept_past.len()
    );
}
