//! Typing → debounce → request → rows, on GTK, through the app's own client.
//!
//! The loop is the one `main.rs` runs: the entry's GTK `changed` signal (what
//! `Entry::connect_changed` registers, and what a keystroke emits) arms
//! `ff_gui::Debounce`; the timer tick asks `Debounce::due`, reads the box,
//! issues the request and paints the answer. The request is not a closure
//! standing in for a backend: it is `DaemonClient::query_request` →
//! `encode_request` → `PendingRequest` over a real socket, and the rows are
//! `outcome_from_records` → `rows_from_paths` → `TreeView::set_rows` — the calls
//! the GUI makes, in the order it makes them.
//!
//! The backend is a real `fflocated` when this build produced one: it indexes a
//! throwaway root holding the fixture files, as the macOS probe's daemon does.
//! Where no daemon binary is available — `cargo test -p ff-gui` alone does not
//! build another crate's binary — the endpoint is a protocol server on a real
//! socket answering `Response::Hit`/`Response::Done` for the same files. Either
//! way the client half is the app's code; no rows are fabricated inside it.
//!
//! Needs a display with GTK3 (the entry's `changed` is a real GTK signal), so a
//! headless run returns early; CI runs it under Xvfb.

#![cfg(unix)]

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use ff_gui::{
    box_change_clears, encode_request, rows_from_paths, DaemonClient, Debounce, PendingRequest,
    Progress, SearchOptions,
};
use rswidgets::compat::{CellValue, TreeView};

/// The query every fixture file matches.
const QUERY: &str = "typing";

/// The fixture files, relative to the backend's root.
const FIXTURES: [&str; 3] = ["alpha-typing.txt", "beta-typing.md", "sub/gamma-typing.rs"];

/// Pump GTK's main loop, the way `interaction_gtk` and `menubar_gtk` do, so a
/// `changed` signal queued by GTK gets delivered.
fn pump(loader: &gtk_dynamic_loader::Loader, iterations: usize) {
    let Some(gtk) = loader.libs.get("libgtk") else {
        return;
    };
    unsafe {
        let it = gtk.get::<unsafe extern "C" fn()>(b"gtk_main_iteration_do");
        if let Ok(f) = it {
            for _ in 0..iterations {
                f();
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

/// A backend on a socket: a real `fflocated` when one is available, otherwise a
/// protocol responder over the same transport.
struct Backend {
    socket: PathBuf,
    /// The paths the backend answered with before typing began — what the typed
    /// query must reproduce.
    paths: Vec<String>,
    kind: &'static str,
    child: Option<std::process::Child>,
}

impl Drop for Backend {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The daemon binary this build produced, if there is one.
///
/// `$FFLOCATED_BIN` wins (the app honours it too); otherwise the binary sits
/// next to this test executable: `target/debug/deps/typing_gtk-…` → `../`.
fn daemon_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FFLOCATED_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    // Cargo puts integration-test binaries under `target/debug/deps`, and this
    // crate's build script copies them into `target/debug/build/<pkg>/<hash>/out`,
    // so look up the chain rather than assuming one layout.
    let exe = std::env::current_exe().ok()?;
    let name = format!("fflocated{}", std::env::consts::EXE_SUFFIX);
    // Shallowest wins: `target/debug/fflocated` before anything a build script
    // left under `target/debug/build/`. `is_file` matters too — `build/fflocated`
    // is a directory named after that crate, and spawning it is EACCES.
    let mut best: Option<PathBuf> = None;
    for dir in exe.ancestors().skip(1).take(8) {
        let candidate = dir.join(&name);
        let depth = |p: &PathBuf| p.components().count();
        if candidate.is_file() && best.as_ref().map_or(true, |b| depth(&candidate) < depth(b)) {
            best = Some(candidate);
        }
    }
    best
}

/// Start a backend rooted at `root`, with its socket under `run_dir`.
fn start_backend(root: &Path, run_dir: &Path, fixture_paths: &[String]) -> Backend {
    std::fs::create_dir_all(run_dir).expect("socket dir");
    let socket = run_dir.join("ff.sock");
    if let Some(bin) = daemon_binary() {
        let child = std::process::Command::new(&bin)
            .arg("--foreground")
            .arg("--backend")
            .arg("fake")
            .arg("--root")
            .arg(root)
            .arg("--state-dir")
            .arg(run_dir.join("state"))
            .arg("--socket")
            .arg(&socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn fflocated");
        let client = DaemonClient::with_socket(socket.clone());
        let start = Instant::now();
        let mut last = String::new();
        while start.elapsed() < Duration::from_secs(30) {
            match client.search(QUERY, 10) {
                Ok(outcome) if outcome.paths.len() == fixture_paths.len() => {
                    println!(
                        "typing: {} indexed {} from {} and answered {} hit(s) for {QUERY:?} in {}ms",
                        bin.display(),
                        root.display(),
                        run_dir.display(),
                        outcome.paths.len(),
                        start.elapsed().as_millis()
                    );
                    return Backend {
                        socket,
                        paths: outcome.paths,
                        kind: "fflocated",
                        child: Some(child),
                    };
                }
                Ok(_) => {}
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the daemon answered nothing within 30s (last error {last:?})");
    }

    // No daemon binary: a protocol endpoint on a real socket. The client still
    // sends the app's encoded request and parses the reply with the app's own
    // `outcome_from_records`; only the index behind it is canned.
    let listener = fflocate::transport::Listener::bind(&socket).expect("bind the test socket");
    let answers: Vec<String> = fixture_paths.to_vec();
    std::thread::spawn(move || {
        for _ in 0..8 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().expect("clone the stream"));
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                return;
            }
            let request: serde_json::Value =
                serde_json::from_str(&line).unwrap_or(serde_json::Value::Null);
            let query = request["q"].as_str().unwrap_or_default().to_ascii_lowercase();
            let hits: Vec<&String> = answers
                .iter()
                .filter(|p| {
                    p.rsplit('/')
                        .next()
                        .unwrap_or(p)
                        .to_ascii_lowercase()
                        .contains(&query)
                })
                .collect();
            for (id, path) in hits.iter().enumerate() {
                let resp = serde_json::to_string(&fflocate::proto::Response::Hit {
                    path: (*path).clone(),
                    id: id as u32,
                })
                .expect("serialise a hit");
                let _ = writeln!(stream, "{resp}");
            }
            let done = serde_json::to_string(&fflocate::proto::Response::Done {
                count: hits.len() as u64,
                cursor: String::new(),
            })
            .expect("serialise done");
            let _ = writeln!(stream, "{done}");
            let _ = stream.flush();
        }
    });
    println!(
        "typing: no fflocated binary in this build; protocol endpoint on {} answering {} fixture path(s)",
        socket.display(),
        fixture_paths.len()
    );
    Backend {
        socket,
        paths: fixture_paths.to_vec(),
        kind: "protocol endpoint",
        child: None,
    }
}

/// Issue one query the way the GUI's pump does: build the app's request, send it
/// over `PendingRequest`, and turn the reply into paths.
fn issue(socket: &Path, query: &str) -> Vec<String> {
    let req = DaemonClient::query_request(query, 100, &SearchOptions::default());
    let line = encode_request(&req).expect("encode the request");
    let stream = fflocate::transport::connect(socket).expect("connect to the backend");
    let mut pending = PendingRequest::start(stream, &line).expect("start the request");
    let start = Instant::now();
    loop {
        match pending.advance().expect("the request failed") {
            Progress::Finished => break,
            Progress::Advanced => {}
            Progress::WouldBlock => {
                assert!(
                    start.elapsed() < Duration::from_secs(30),
                    "the backend never finished the request for {query:?}"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
    let client = DaemonClient::with_socket(socket.to_path_buf());
    client
        .outcome_from_records(pending.records())
        .expect("the reply did not parse")
        .paths
}

/// What one pump tick decided.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Nothing was due: the user is still typing.
    Idle,
    /// A request went out for the query in the box.
    Issued(String),
    /// The box was empty, so the list was cleared without a request.
    Cleared,
}

#[test]
fn typing_debounces_into_one_request_and_the_rows_are_its_answer() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("typing: no display, skipping");
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => {
            eprintln!("typing: no GTK loader, skipping");
            return;
        }
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        eprintln!("typing: not GTK3, skipping");
        return;
    }
    let app = match rswidgets::core::App::init() {
        Ok(app) => app,
        Err(_) => {
            eprintln!("typing: GTK did not initialise, skipping");
            return;
        }
    };

    // A real tree on disk, so "the rows on screen" are real files.
    let tmp = tempfile::tempdir().expect("no temp dir");
    let root = tmp.path().join("root");
    let fixtures: Vec<String> = FIXTURES
        .iter()
        .map(|rel| root.join(rel).to_string_lossy().into_owned())
        .collect();
    for path in &fixtures {
        let p = Path::new(path);
        std::fs::create_dir_all(p.parent().expect("fixture parent")).expect("fixture dir");
        std::fs::write(p, "typing fixture\n").expect("fixture file");
    }
    // A file the query must not match: three hits, not four.
    std::fs::write(root.join("unrelated.rs"), "no match here\n").expect("decoy file");

    let backend = start_backend(&root, &tmp.path().join("run"), &fixtures);
    let mut expected_names: Vec<String> = backend
        .paths
        .iter()
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
        .collect();
    let mut expected_paths = backend.paths.clone();
    expected_names.sort();
    expected_paths.sort();

    // The app's widgets: the search entry and the results list with its columns.
    let entry = app.create_entry().expect("entry");
    let list = TreeView::new(&[("", true, true), ("Name", false, true), ("Path", false, true)])
        .expect("the results list could not be created");

    // The app's debounce window, shortened; the type and the semantics are the
    // GUI's.
    let debounce = Rc::new(Debounce::new(Duration::from_millis(80)));
    let changes = Rc::new(RefCell::new(0usize));
    {
        // main.rs' handler, in miniature: count the edit, read the box, and —
        // only when the combined query is still worth searching — arm. An
        // emptied box renders nothing at once and arms nothing. (The deadline a
        // previous keystroke armed is deliberately left alone: the pump is what
        // must not issue for it, which is the case asserted below.)
        let changes = changes.clone();
        let debounce = debounce.clone();
        let boxed = entry.clone();
        let cleared_list = list.clone();
        entry
            .connect_changed(move || {
                *changes.borrow_mut() += 1;
                let text = boxed.get_text().unwrap_or_default();
                if box_change_clears(&text, "") {
                    cleared_list.set_rows(&Vec::new());
                    return;
                }
                debounce.arm();
            })
            .expect("the entry has no changed signal");
    }

    let requests: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let tick = || -> Step {
        if !debounce.due() {
            return Step::Idle;
        }
        let query = entry.get_text().unwrap_or_default();
        if box_change_clears(&query, "") {
            debounce.clear();
            list.set_rows(&Vec::new());
            return Step::Cleared;
        }
        let paths = issue(&backend.socket, &query);
        // main.rs retires the deadline as it issues, so a second tick cannot
        // send the same query again.
        debounce.clear();
        requests.borrow_mut().push(query.clone());
        list.set_rows(&cells(&paths));
        Step::Issued(query)
    };

    // --- Rapid keystrokes: six edits, no request, one armed deadline ---
    for text in ["t", "ty", "typ", "typi", "typin", QUERY] {
        entry.set_text(text);
        pump(&loader, 5);
    }
    let typed = entry.get_text().unwrap_or_default();
    assert_eq!(typed, QUERY, "the entry does not hold the keystrokes");
    assert!(
        requests.borrow().is_empty(),
        "a request went out per character: {:?}",
        requests.borrow()
    );
    assert!(
        debounce.pending(),
        "typing did not arm the debounce, so nothing would ever be sent"
    );
    println!(
        "typing: {} change(s) -> entry holds {typed:?}, {} request(s) so far, debounce armed",
        *changes.borrow(),
        requests.borrow().len()
    );

    // --- The window elapses: exactly one request, carrying the last text ---
    let start = Instant::now();
    let mut issued = Vec::new();
    while start.elapsed() < Duration::from_secs(10) {
        pump(&loader, 1);
        match tick() {
            Step::Idle => std::thread::sleep(Duration::from_millis(5)),
            other => {
                issued.push(other);
                break;
            }
        }
    }
    assert_eq!(
        issued,
        vec![Step::Issued(QUERY.to_string())],
        "the window did not collapse the keystrokes into one request"
    );
    assert_eq!(
        *requests.borrow(),
        vec![QUERY.to_string()],
        "the request did not carry the typed query exactly once: {:?}",
        requests.borrow()
    );
    assert!(!debounce.pending(), "the debounce stayed armed after issuing");
    let painted = column(&list, 1);
    let painted_paths = column(&list, 2);
    println!(
        "typing: debounce -> request {:?} -> rows {painted:?} paths {painted_paths:?} (backend: {})",
        requests.borrow(),
        backend.kind
    );
    let mut painted_sorted = painted.clone();
    painted_sorted.sort();
    let mut paths_sorted = painted_paths.clone();
    paths_sorted.sort();
    assert_eq!(
        painted_sorted, expected_names,
        "the list does not hold the rows the backend returned"
    );
    assert_eq!(
        paths_sorted, expected_paths,
        "the rows on screen are not the backend's paths"
    );
    assert_eq!(
        painted_paths.len(),
        FIXTURES.len(),
        "{} row(s) for {} fixture files: {painted_paths:?}",
        painted_paths.len(),
        FIXTURES.len()
    );
    assert!(
        painted_paths
            .iter()
            .all(|p| std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)),
        "the painted rows are not real files: {painted_paths:?}"
    );

    // --- Clearing the box: no rows, no request ---
    entry.set_text("");
    pump(&loader, 5);
    assert!(
        column(&list, 1).is_empty(),
        "clearing the box left rows on screen: {:?}",
        column(&list, 1)
    );
    assert_eq!(
        *requests.borrow(),
        vec![QUERY.to_string()],
        "clearing the box sent a request: {:?}",
        requests.borrow()
    );
    assert!(
        !debounce.pending(),
        "clearing the box left the debounce armed"
    );
    println!(
        "typing: box cleared -> rows {:?}, requests {:?}, no deadline",
        column(&list, 1),
        requests.borrow()
    );

    // --- Typing, then erasing before the window: nothing may repaint ---
    entry.set_text("zzz");
    pump(&loader, 5);
    assert!(debounce.pending(), "typing again did not arm the debounce");
    entry.set_text("");
    pump(&loader, 5);
    assert!(
        column(&list, 1).is_empty(),
        "erasing the query left rows for it: {:?}",
        column(&list, 1)
    );
    let start = Instant::now();
    let mut after_erase = Vec::new();
    while start.elapsed() < Duration::from_secs(10) {
        pump(&loader, 1);
        match tick() {
            Step::Idle => std::thread::sleep(Duration::from_millis(5)),
            other => {
                after_erase.push(other);
                break;
            }
        }
    }
    assert_eq!(
        after_erase,
        vec![Step::Cleared],
        "an erased query still issued a request"
    );
    assert_eq!(
        *requests.borrow(),
        vec![QUERY.to_string()],
        "the erased query reached the backend: {:?}",
        requests.borrow()
    );
    assert!(
        column(&list, 1).is_empty(),
        "the list was repainted for a query the user erased: {:?}",
        column(&list, 1)
    );
    assert!(
        !debounce.pending(),
        "the debounce is still armed after erasing"
    );
    println!(
        "typing: typed \"zzz\" then erased before the window -> {:?}, requests {:?} (backend: {})",
        after_erase,
        requests.borrow(),
        backend.kind
    );
}
