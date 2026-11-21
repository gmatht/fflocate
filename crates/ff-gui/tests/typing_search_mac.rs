//! Typing → debounce → request → rows repaint, on AppKit, typed into the field.
//!
//! The macOS sibling of `tests/typing_gtk.rs`, and the counterpart of
//! `daemon_pixels_mac.rs` for the one path that starts at the keyboard.
//!
//! The keystrokes are real AppKit ones: the field is made the window's first
//! responder, its field editor (`currentEditor`, an `NSTextView`) is given text
//! with `insertText:replacementRange:`, and AppKit then reports the edit to the
//! field's `NSControlTextEditingDelegate`. That delegate is `CorroMacField`,
//! whose `controlTextDidChange:` lands in `dispatch_text_changed` → the
//! `Entry::connect_changed` handler — the same handler `main.rs` installs. The
//! handler arms `ff_gui::Debounce`; the pump asks `Debounce::due`, builds the
//! app's request (`DaemonClient::query_request` → `encode_request`), sends it
//! with `PendingRequest` over a socket to a real `fflocated`, and turns the
//! answer into rows with `rows_from_paths` → `TreeView::set_rows`.
//!
//! What it proves, and why each assertion is the loop rather than a stand-in:
//!
//! * per-keystroke edits are delivered (`changed` fires once per `insertText:`),
//!   and none of them issues a request;
//! * after the window, exactly one request goes out, carrying the whole typed
//!   query — a per-keystroke cascade or an early issue fails the request list;
//! * the rows on screen are the daemon's answer for that query, are real files,
//!   and are *different* from the rows the previous query painted (a narrowing
//!   query, not a query that happens to match everything);
//! * the window's own pixels changed where the rows are, so the list really
//!   repainted rather than only holding new cells;
//! * clearing the box empties the list at once and sends nothing.
//!
//! Needs AppKit on the process's main thread, so `harness = false` and its own
//! `fn main` — see `[[test]]` in `Cargo.toml`.

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    use std::io::{BufRead, BufReader, Write};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use ff_gui::{
        box_change_clears, encode_request, rows_from_paths, DaemonClient, Debounce, PendingRequest,
        Progress, SearchOptions,
    };
    use rswidgets::compat::CellValue;
    use rswidgets::prelude::*;

    // --- The AppKit sends this test needs ---
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct NSRange {
        location: usize,
        length: usize,
    }
    fn send0(obj: *mut c_void, sel: &str) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send0v(obj: *mut c_void, sel: &str) {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send1v(obj: *mut c_void, sel: &str, arg: *mut c_void) {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel), arg) }
    }
    /// `-[NSTextView insertText:replacementRange:]`: what typing into a field
    /// editor is. `NSNotFound` (for the range) means "insert at the caret".
    /// Falls back to the older `insertText:` when the editor has no ranged
    /// form — sending an object an unrecognised selector aborts the process,
    /// so the choice is made by asking, not by assuming.
    fn insert_text(editor: *mut c_void, text: &str, modern: bool) {
        if modern {
            type F = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, NSRange);
            let f: F = unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
            unsafe {
                f(
                    editor,
                    rswidgets::backends::apple::selector("insertText:replacementRange:"),
                    rswidgets::backends::apple::nsstring(text),
                    NSRange {
                        location: usize::MAX,
                        length: 0,
                    },
                )
            }
        } else {
            send1v(editor, "insertText:", rswidgets::backends::apple::nsstring(text));
        }
    }
    fn responds(obj: *mut c_void, sel: &str) -> bool {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i8 =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe {
            f(
                obj,
                rswidgets::backends::apple::selector("respondsToSelector:"),
                rswidgets::backends::apple::selector(sel),
            ) != 0
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
    fn column(list: &rswidgets::compat::TreeView, col: usize) -> Vec<String> {
        (0..).map_while(|row| list.cell_text(row, col)).collect()
    }

    /// Issue one query the way the GUI's pump does: the app's request, the
    /// app's transport, the app's reply parsing.
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

    /// The daemon binary this build produced, if there is one. A harness-free
    /// test binary does not live in `target/debug` itself, so walk up.
    fn daemon_binary() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..8 {
            let Some(d) = dir else { break };
            let candidate = d.join(format!("fflocated{}", std::env::consts::EXE_SUFFIX));
            if candidate.is_file() {
                return Some(candidate);
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
        None
    }

    /// A backend on a socket: a real `fflocated` when the build has one,
    /// otherwise a protocol responder so the client half is still the app's.
    struct Backend {
        socket: PathBuf,
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
                match client.search("typing", 20) {
                    Ok(outcome) if outcome.paths.len() == fixture_paths.len() => {
                        println!(
                            "typing: {} indexed {} and answered {} hits for \"typing\" in {}ms",
                            bin.display(),
                            root.display(),
                            outcome.paths.len(),
                            start.elapsed().as_millis()
                        );
                        return Backend {
                            socket,
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
        // No daemon binary: a protocol endpoint on a real socket, matching the
        // query as a case-insensitive substring the way the fake backend does.
        let listener = fflocate::transport::Listener::bind(&socket).expect("bind the test socket");
        let answers: Vec<String> = fixture_paths.to_vec();
        std::thread::spawn(move || {
            for _ in 0..16 {
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
                let query = request["q"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let hits: Vec<&String> = answers
                    .iter()
                    .filter(|p| p.to_ascii_lowercase().contains(&query))
                    .collect();
                for (id, path) in hits.iter().enumerate() {
                    let resp = serde_json::to_string(&fflocate::proto::Response::Hit {
                        path: (*path).clone(),
                        id: id as u32,
                        created: None,
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
            kind: "protocol endpoint",
            child: None,
        }
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

    // --- Fixtures: a query that matches all four, and one that narrows to one ---
    let fixture = tempfile::tempdir().expect("no temp dir");
    let root = fixture.path().join("root");
    let files = [
        "typing-alpha.txt",
        "typing-beta.md",
        "other-typing.rs",
        "sub/typing-gamma.rs",
        // Matches neither query: four rows for "typing", one for "typing-a".
        "unrelated.rs",
    ];
    for rel in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().expect("fixture parent")).expect("fixture dir");
        std::fs::write(&p, "typing fixture\n").expect("fixture file");
    }
    let broad: Vec<String> = ["typing-alpha.txt", "typing-beta.md", "other-typing.rs", "sub/typing-gamma.rs"]
        .iter()
        .map(|rel| root.join(rel).to_string_lossy().into_owned())
        .collect();
    let narrow: String = root
        .join("typing-alpha.txt")
        .to_string_lossy()
        .into_owned();

    let backend = start_backend(&root, &fixture.path().join("run"), &broad);

    // --- The app's widgets: the query entry and the results list ---
    let app = App::init()?;
    let win = app.create_window()?;
    win.set_title("ff-gui typing and search test");
    let entry = app.create_entry()?;
    let list = app.create_treeview(&[("", true, true), ("Name", false, true), ("Path", false, true)])?;
    let painted = list.clone().into_scrolled()?;
    let body = app.create_box(Orientation::Vertical, 2)?;
    body.append(&entry);
    body.append(&painted);
    win.set_child(&body);
    win.present();
    let view = win.raw_handle();
    // Lay the window out at a real size: the list's rows (and the capture
    // below) need a viewport, and the entry needs a field editor.
    let _ = rswidgets::backends_macos_adapter::capture(view, Some((900.0, 560.0)));
    send0v(view, "layoutSubtreeIfNeeded");

    // --- The app's handler, on the field's own change signal ---
    let debounce = Rc::new(Debounce::new(Duration::from_millis(300)));
    let changes = Rc::new(Cell::new(0usize));
    {
        let debounce = debounce.clone();
        let changes = changes.clone();
        let boxed = entry.clone();
        let cleared = list.clone();
        entry
            .connect_changed(move || {
                changes.set(changes.get() + 1);
                let text = boxed.get_text().unwrap_or_default();
                if box_change_clears(&text, "") {
                    cleared.set_rows(&Vec::new());
                    return;
                }
                debounce.arm();
            })
            .expect("the entry has no changed signal");
    }

    let requests: RefCell<Vec<String>> = RefCell::new(Vec::new());

    // --- Start the edit the way a click or Tab would: the field editor ---
    //
    // `makeFirstResponder:` is the window's: sending it to the content view is
    // an unrecognised selector in AppKit, which aborts the process rather than
    // returning an error.
    let window = send0(entry.raw_handle(), "window");
    assert!(!window.is_null(), "the entry is not in a window");
    send1v(window, "makeFirstResponder:", entry.raw_handle());
    let editor = send0(entry.raw_handle(), "currentEditor");
    assert!(
        !editor.is_null(),
        "the entry has no field editor, so AppKit never started an edit and nothing typed could reach it"
    );
    let modern_insert = responds(editor, "insertText:replacementRange:");
    assert!(
        modern_insert || responds(editor, "insertText:"),
        "the field editor accepts no text insertion, so typing cannot be delivered"
    );
    println!(
        "typing: the entry is first responder and its field editor is open ({})",
        if modern_insert {
            "insertText:replacementRange:"
        } else {
            "insertText:"
        }
    );

    // One pump tick, exactly as `main.rs` decides: nothing due while typing,
    // otherwise read the box, build the app's request, send it, paint the rows.
    macro_rules! tick {
        () => {{
            if !debounce.due() {
                Step::Idle
            } else {
                let query = entry.get_text().unwrap_or_default();
                if box_change_clears(&query, "") {
                    debounce.clear();
                    list.set_rows(&Vec::new());
                    Step::Cleared
                } else {
                    let paths = issue(&backend.socket, &query);
                    debounce.clear();
                    requests.borrow_mut().push(query.clone());
                    list.set_rows(&cells(&paths));
                    Step::Issued(query)
                }
            }
        }};
    }

    // --- Type "typing" one character at a time, as a keyboard does ---
    let mut typed_changes = 0usize;
    for ch in ["t", "y", "p", "i", "n", "g"] {
        let before = changes.get();
        insert_text(editor, ch, modern_insert);
        assert_eq!(
            changes.get(),
            before + 1,
            "typing {ch:?} did not deliver exactly one change to Rust (AppKit delegate not wired)"
        );
        typed_changes += 1;
        assert_eq!(
            tick!(),
            Step::Idle,
            "a request was issued while the user was still typing {ch:?}"
        );
        assert!(
            requests.borrow().is_empty(),
            "a request went out per character: {:?}",
            requests.borrow()
        );
        assert!(
            column(&list, 1).is_empty(),
            "the list was painted while the user was still typing: {:?}",
            column(&list, 1)
        );
    }
    assert_eq!(
        entry.get_text().unwrap_or_default(),
        "typing",
        "the field does not hold the typed text"
    );
    assert!(
        debounce.pending(),
        "typing did not arm the debounce, so nothing would ever be sent"
    );
    println!(
        "typing: typed {typed_changes} char(s) into the AppKit field -> {} changed signal(s), entry {:?}, 0 request(s), debounce armed",
        changes.get(),
        entry.get_text().unwrap_or_default()
    );

    // --- The window elapses: one request carrying the whole query ---
    let start = Instant::now();
    let mut issued = Vec::new();
    while start.elapsed() < Duration::from_secs(20) {
        match tick!() {
            Step::Idle => std::thread::sleep(Duration::from_millis(5)),
            other => {
                issued.push(other);
                break;
            }
        }
    }
    assert_eq!(
        issued,
        vec![Step::Issued("typing".to_string())],
        "the window did not collapse the keystrokes into one request"
    );
    assert_eq!(
        *requests.borrow(),
        vec!["typing".to_string()],
        "the request did not carry the typed query exactly once: {:?}",
        requests.borrow()
    );
    assert!(!debounce.pending(), "the debounce stayed armed after issuing");
    let mut names = column(&list, 1);
    let mut paths = column(&list, 2);
    names.sort();
    paths.sort();
    let mut expected_broad: Vec<String> = broad.iter().map(|p| p.rsplit('/').next().unwrap().to_string()).collect();
    expected_broad.sort();
    let mut expected_broad_paths = broad.clone();
    expected_broad_paths.sort();
    println!(
        "typing: debounce -> request {:?} -> {} row(s) {names:?} (backend: {})",
        requests.borrow(),
        names.len(),
        backend.kind
    );
    assert_eq!(
        names, expected_broad,
        "the list does not hold the rows the daemon returned for \"typing\""
    );
    assert_eq!(
        paths, expected_broad_paths,
        "the rows on screen are not the daemon's paths"
    );
    let before_shot = rswidgets::backends_macos_adapter::capture(view, Some((900.0, 560.0)))
        .ok_or("the window produced no capture before typing the narrower query")?;
    assert!(
        before_shot.content_ink > 100,
        "the four rows painted {} px: nothing reached the bitmap",
        before_shot.content_ink
    );

    // --- Type a query that narrows the list: "-a" ---
    for ch in ["-", "a"] {
        let before = changes.get();
        insert_text(editor, ch, modern_insert);
        assert_eq!(
            changes.get(),
            before + 1,
            "typing {ch:?} into the existing query did not deliver one change"
        );
        assert_eq!(
            tick!(),
            Step::Idle,
            "a request was issued before the debounce elapsed after {ch:?}"
        );
        assert_eq!(
            *requests.borrow(),
            vec!["typing".to_string()],
            "typing {ch:?} reached the backend early: {:?}",
            requests.borrow()
        );
        // The list keeps the daemon's order, so compare as sorted sets: the
        // point is that these are still the previous query's rows.
        let mut shown = column(&list, 1);
        shown.sort();
        assert_eq!(
            shown, expected_broad,
            "the rows were replaced before the debounce elapsed"
        );
    }
    assert_eq!(
        entry.get_text().unwrap_or_default(),
        "typing-a",
        "the field does not hold the narrowed query"
    );
    println!(
        "typing: typed \"-a\" -> entry {:?}, {} change(s) total, request(s) still {:?}, rows still {}",
        entry.get_text().unwrap_or_default(),
        changes.get(),
        requests.borrow(),
        column(&list, 1).len()
    );

    let start = Instant::now();
    let mut narrowed = Vec::new();
    while start.elapsed() < Duration::from_secs(20) {
        match tick!() {
            Step::Idle => std::thread::sleep(Duration::from_millis(5)),
            other => {
                narrowed.push(other);
                break;
            }
        }
    }
    assert_eq!(
        narrowed,
        vec![Step::Issued("typing-a".to_string())],
        "the narrowed query did not issue exactly once after its own window"
    );
    assert_eq!(
        *requests.borrow(),
        vec!["typing".to_string(), "typing-a".to_string()],
        "the typed queries did not reach the backend one at a time: {:?}",
        requests.borrow()
    );
    let narrowed_names = column(&list, 1);
    let narrowed_paths = column(&list, 2);
    println!(
        "typing: debounce -> request {:?} -> {} row(s) {narrowed_names:?} paths {narrowed_paths:?}",
        requests.borrow(),
        narrowed_names.len(),
    );
    assert_eq!(
        narrowed_names,
        vec!["typing-alpha.txt".to_string()],
        "the rows are not the one file \"typing-a\" matches: {narrowed_names:?}"
    );
    assert_eq!(
        narrowed_paths,
        vec![narrow.clone()],
        "the painted row is not the narrowed query's file"
    );
    assert!(
        std::fs::metadata(&narrowed_paths[0])
            .map(|m| m.is_file())
            .unwrap_or(false),
        "the painted row {:?} is not a file on disk",
        narrowed_paths[0]
    );
    assert!(
        !narrowed_names.iter().any(|n| expected_broad.contains(n) && n != "typing-alpha.txt"),
        "the list still holds rows from the broader query: {narrowed_names:?}"
    );

    // --- The list really repainted: the window's pixels changed ---
    let after_shot = rswidgets::backends_macos_adapter::capture(view, Some((900.0, 560.0)))
        .ok_or("the window produced no capture after the narrowed rows")?;
    assert_eq!(
        (after_shot.width, after_shot.height),
        (before_shot.width, before_shot.height),
        "the repaint changed the capture's size"
    );
    let repainted = before_shot
        .rgba
        .iter()
        .zip(after_shot.rgba.iter())
        .filter(|(a, b)| a != b)
        .count();
    println!(
        "typing: capture {}x{}: 4 rows ink {} px -> 1 row ink {} px, {} px repainted",
        after_shot.width, after_shot.height, before_shot.content_ink, after_shot.content_ink, repainted
    );
    assert!(
        after_shot.content_ink > 100,
        "the narrowed row painted {} px: nothing reached the bitmap",
        after_shot.content_ink
    );
    assert!(
        repainted > 500,
        "replacing four rows with one changed only {repainted} px: the list did not repaint"
    );

    // --- Clearing the box: no rows, no request ---
    send0v(editor, "selectAll:");
    send0v(editor, "deleteBackward:");
    assert_eq!(
        entry.get_text().unwrap_or_default(),
        "",
        "deleting the query left text in the field"
    );
    assert!(
        column(&list, 1).is_empty(),
        "clearing the box left rows on screen: {:?}",
        column(&list, 1)
    );
    assert_eq!(
        *requests.borrow(),
        vec!["typing".to_string(), "typing-a".to_string()],
        "clearing the box sent a request: {:?}",
        requests.borrow()
    );
    assert!(
        !debounce.pending(),
        "clearing the box left the debounce armed"
    );
    println!(
        "typing: box cleared -> {} row(s), requests {:?}, no deadline",
        column(&list, 1).len(),
        requests.borrow()
    );

    println!(
        "typing: ok — AppKit keystrokes debounced into one request, the rows are its answer, and the repaint changed {} px",
        repainted
    );
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    // The keystroke goes through AppKit's field editor and delegate; the GTK
    // counterpart is `tests/typing_gtk.rs`.
    println!("typing_search_mac: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
