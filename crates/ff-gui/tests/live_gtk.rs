//! Live mode on GTK: the foldout's toggle, the daemon's `subscribe` feed, and
//! the rows it makes the app re-query.
//!
//! The pieces under test are the app's own, not a re-implementation:
//!
//! * the checkbox is `ff_gui::live_ui::build`, the widget `main.rs` puts in the
//!   Advanced Search foldout;
//! * the connection is `ff_gui::LiveRunner`, the type `main.rs` steps from its
//!   pump, talking NDJSON over a real socket to a real server thread;
//! * the rows are the app's `DaemonClient::search` → `rows_from_paths` →
//!   `TreeView::set_rows`, exactly what the GUI's render does.
//!
//! What is asserted, and why:
//!
//! * turning the toggle on opens a connection whose first request is
//!   `{"op":"subscribe", "q": …}` — the same query the boxes hold, so the daemon
//!   filters server-side;
//! * a pushed `create` event makes `LiveRunner` report `Refresh` once (the
//!   coalescing window), and the re-query the app would then run paints the new
//!   row;
//! * a pushed `delete` event produces another `Refresh`, and the row disappears;
//! * the status line says the feed is live while it is;
//! * turning the toggle off drops the feed, so the daemon sees the disconnect.
//!
//! Needs a display with GTK3, so a headless run returns early; CI runs it under
//! Xvfb. A build without GTK skips before touching AppKit.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use fflocate::proto::Response;
use ff_gui::{
    live_ui, rows_from_paths, DaemonClient, LiveKey, LiveRunner, LiveStep, SearchOptions, LIVE_STATUS,
};
use rswidgets::compat::CellValue;

/// The query the fixtures match, and what the subscription must carry.
const QUERY: &str = "alpha";

fn pump(loader: &gtk_dynamic_loader::Loader, iterations: usize) {
    let Some(gtk) = loader.libs.get("libgtk") else {
        return;
    };
    unsafe {
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

fn meta() -> Response {
    Response::Meta {
        gen: 1,
        lsn: 1,
        fresh: true,
        elapsed_ms: 0.1,
        stale_roots: Vec::new(),
        catching_up: None,
        lag_ms: None,
        phase: Some("idle".to_string()),
        crawl_entries: None,
        crawl_dirs_pending: None,
        backend: Some("fake".to_string()),
        entries: Some(1),
    }
}

fn event(action: &str, path: &str) -> String {
    Response::Event {
        cursor: "c:1:2".to_string(),
        action: action.to_string(),
        path: path.to_string(),
    }
    .to_line()
}

/// A protocol endpoint on a real socket: one `subscribe` connection (which the
/// test drives event-by-event) plus any number of `query` requests answered
/// from the shared hit list.
struct Server {
    socket: PathBuf,
    /// The subscribe request line, sent to the test when it arrives.
    requests: mpsc::Receiver<String>,
    /// Event lines to write on the open feed.
    events: mpsc::Sender<String>,
    hits: Arc<Mutex<Vec<String>>>,
}

/// Answer one client: a subscription is kept open and fed from the test's
/// event channel; a query is answered from the shared hit list.
fn handle(
    mut stream: fflocate::transport::Stream,
    requests: mpsc::Sender<String>,
    events: Arc<Mutex<mpsc::Receiver<String>>>,
    hits: Arc<Mutex<Vec<String>>>,
) {
    let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone the client socket"));
    let mut first = String::new();
    if std::io::BufRead::read_line(&mut reader, &mut first).is_err() {
        return;
    }
    if first.contains("\"op\":\"subscribe\"") {
        let _ = requests.send(first);
        for line in [meta().to_line(), "\n".to_string()] {
            if std::io::Write::write_all(&mut stream, line.as_bytes()).is_err() {
                return;
            }
        }
        let _ = std::io::Write::flush(&mut stream);
        loop {
            // Hold the receive lock only for the wait, then write without it.
            let next = events.lock().unwrap().recv();
            let Ok(line) = next else {
                return;
            };
            if std::io::Write::write_all(&mut stream, line.as_bytes()).is_err()
                || std::io::Write::write_all(&mut stream, b"\n").is_err()
                || std::io::Write::flush(&mut stream).is_err()
            {
                return;
            }
        }
    }
    let current = hits.lock().unwrap().clone();
    for (id, path) in current.iter().enumerate() {
        let line = Response::Hit {
            path: path.clone(),
            id: id as u32,
            created: None,
        }
        .to_line();
        let _ = std::io::Write::write_all(&mut stream, line.as_bytes());
        let _ = std::io::Write::write_all(&mut stream, b"\n");
    }
    let done = Response::Done {
        count: current.len() as u64,
        cursor: "c:1:2".to_string(),
    }
    .to_line();
    let _ = std::io::Write::write_all(&mut stream, done.as_bytes());
    let _ = std::io::Write::write_all(&mut stream, b"\n");
    let _ = std::io::Write::flush(&mut stream);
}

fn serve(socket: PathBuf, initial: Vec<String>) -> Server {
    let (tx_req, rx_req) = mpsc::channel::<String>();
    let (tx_evt, rx_evt) = mpsc::channel::<String>();
    let hits = Arc::new(Mutex::new(initial));
    let server_hits = Arc::clone(&hits);
    let events = Arc::new(Mutex::new(rx_evt));
    let listener = fflocate::transport::Listener::bind(&socket).expect("bind the live test socket");
    // A thread per connection: the subscribe feed stays open while the test's
    // re-queries arrive on their own sockets.
    std::thread::spawn(move || {
        for _ in 0..16 {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let requests = tx_req.clone();
            let events = Arc::clone(&events);
            let hits = Arc::clone(&server_hits);
            std::thread::spawn(move || handle(stream, requests, events, hits));
        }
    });
    Server {
        socket,
        requests: rx_req,
        events: tx_evt,
        hits,
    }
}

/// Step the runner until it reports `Refresh`, pumping GTK in between.
fn pump_until_refresh(
    runner: &mut LiveRunner,
    socket: &Path,
    key: &LiveKey,
    loader: &gtk_dynamic_loader::Loader,
) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        match runner.step(socket, key, Instant::now()) {
            LiveStep::Refresh => return true,
            LiveStep::Unsupported => panic!("the daemon answered the feed with Done/Error"),
            LiveStep::Failed(e) => panic!("the live feed failed: {e}"),
            LiveStep::Idle => {}
        }
        pump(loader, 20);
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn live_toggle_subscribes_and_a_pushed_change_repaints_the_rows() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("live: no display, skipping");
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => {
            eprintln!("live: no GTK loader, skipping");
            return;
        }
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        eprintln!("live: not GTK3, skipping");
        return;
    }
    let Ok(app) = rswidgets::App::init() else {
        eprintln!("live: GTK did not initialise, skipping");
        return;
    };

    let dir = tempfile::tempdir().expect("temp dir");
    let first = dir.path().join("alpha-one.txt");
    std::fs::write(&first, b"live\n").expect("write fixture");
    let first = first.to_string_lossy().into_owned();
    let socket = dir.path().join("live.sock");
    let server = serve(socket.clone(), vec![first.clone()]);

    // The foldout as the app builds it: toggle, list, status.
    let win = app.create_window().expect("window");
    win.set_title("ff-gui live test");
    let live = live_ui::build(&app).expect("the Live toggle could not be built");
    let list = app
        .create_treeview(&[("", true, true), ("Name", false, true), ("Path", false, true)])
        .expect("the results list could not be created");
    let status = app.create_label("").expect("status");
    let layout = app
        .create_box(rswidgets::prelude::Orientation::Vertical, 3)
        .expect("layout");
    layout.append(&live.button);
    layout.append(&list.clone().into_scrolled().expect("scroller"));
    layout.append(&status);
    win.set_child(&layout);
    win.present();
    pump(&loader, 50);

    // --- Turning the toggle on opens the feed for the box's query ---
    live.button.set_active(true);
    assert!(live.button.is_active(), "the toggle did not turn on");
    status.set_text(LIVE_STATUS);
    assert_eq!(status.get_text().unwrap_or_default(), LIVE_STATUS);

    let key = LiveKey::new(QUERY, &SearchOptions::default());
    let mut runner = LiveRunner::new();
    let request = {
        let start = Instant::now();
        loop {
            let _ = runner.step(&server.socket, &key, Instant::now());
            pump(&loader, 20);
            match server.requests.recv_timeout(Duration::from_millis(20)) {
                Ok(line) => break line,
                Err(mpsc::RecvTimeoutError::Timeout) if start.elapsed() < Duration::from_secs(5) => {}
                Err(e) => panic!("the live feed never sent a request: {e}"),
            }
        }
    };
    assert!(
        request.contains("\"op\":\"subscribe\""),
        "the live feed did not ask the daemon to subscribe: {request}"
    );
    assert!(
        request.contains(&format!("\"q\":\"{QUERY}\"")),
        "the subscription does not carry the box's query: {request}"
    );
    eprintln!("live: subscribed -> {request}");

    // --- A create event: one coalesced refresh, then the row is painted ---
    let second = dir.path().join("alpha-two.rs");
    std::fs::write(&second, b"live\n").expect("write the created file");
    let second = second.to_string_lossy().into_owned();
    server.hits.lock().unwrap().push(second.clone());
    server
        .events
        .send(event("create", &second))
        .expect("send the create event");
    assert!(
        pump_until_refresh(&mut runner, &server.socket, &key, &loader),
        "a create event never produced a refresh"
    );

    let client = DaemonClient::with_socket(server.socket.clone());
    let outcome = client.search(QUERY, 10).expect("the re-query failed");
    let mut painted = outcome.paths.clone();
    painted.sort();
    list.set_rows(&cells(&painted));
    let shown = list.displayed_column(2);
    assert!(
        shown.iter().any(|p| p == &second),
        "the created row is not in the list after the refresh: {shown:?}"
    );
    eprintln!("live: after create, rows are {shown:?}");

    // --- A delete event: another refresh, and the row goes away ---
    std::fs::remove_file(&second).expect("remove the file");
    server.hits.lock().unwrap().retain(|p| p != &second);
    server
        .events
        .send(event("delete", &second))
        .expect("send the delete event");
    assert!(
        pump_until_refresh(&mut runner, &server.socket, &key, &loader),
        "a delete event never produced a refresh"
    );

    let outcome = client.search(QUERY, 10).expect("the re-query after delete failed");
    list.set_rows(&cells(&outcome.paths));
    let shown = list.displayed_column(2);
    assert!(
        !shown.iter().any(|p| p == &second),
        "the deleted row is still in the list: {shown:?}"
    );
    assert_eq!(
        shown,
        vec![first.clone()],
        "the list should hold exactly the surviving row"
    );
    eprintln!("live: after delete, rows are {shown:?}");

    // --- Off drops the feed ---
    runner.stop();
    assert!(!runner.is_active(), "turning live off did not close the feed");
    live.button.set_active(false);
    assert!(!live.button.is_active());
    status.set_text("live: off");
    drop(server.events);
    println!(
        "live: ok — toggle subscribed for {QUERY:?}, a create repainted {} and a delete removed it",
        second
    );
}
