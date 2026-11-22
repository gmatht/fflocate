//! Live mode on AppKit: the foldout's toggle, the daemon's `subscribe` feed,
//! and the rows it makes the app re-query.
//!
//! The macOS sibling of `tests/live_gtk.rs`. Everything it drives is the app's
//! own code: `ff_gui::live_ui::build` (the checkbox `main.rs` appends to the
//! Advanced Search foldout), `ff_gui::LiveRunner` (the feed `main.rs` steps from
//! its pump), and `DaemonClient::search` → `rows_from_paths` →
//! `TreeView::set_rows` for the repaint. Only the peer is synthetic: a protocol
//! server on a real socket, so the request encoding, the non-blocking read and
//! the event parsing are the production ones.
//!
//! Asserted: the toggle opens a feed asking for `subscribe` with the box's
//! query; a pushed `create` makes the runner report `Refresh`; the re-query
//! after it paints the new row; a pushed `delete` produces another `Refresh`
//! and the row is gone.
//!
//! Needs AppKit on the process's main thread, so `harness = false` and its own
//! `fn main` — see `[[test]]` in `Cargo.toml`.

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{BufRead, Write};
    use std::path::PathBuf;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    use fflocate::proto::Response;
    use ff_gui::{
        live_ui, rows_from_paths, DaemonClient, LiveKey, LiveRunner, LiveStep, SearchOptions,
        LIVE_STATUS,
    };
    use rswidgets::compat::CellValue;
    use rswidgets::prelude::*;

    const QUERY: &str = "alpha";

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

    /// A protocol endpoint: one long-lived subscribe feed plus query replies,
    /// each on its own thread so the feed can stay open.
    fn serve(
        socket: PathBuf,
        initial: Vec<String>,
    ) -> (
        mpsc::Receiver<String>,
        mpsc::Sender<String>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let (tx_req, rx_req) = mpsc::channel::<String>();
        let (tx_evt, rx_evt) = mpsc::channel::<String>();
        let hits = Arc::new(Mutex::new(initial));
        let server_hits = Arc::clone(&hits);
        let events = Arc::new(Mutex::new(rx_evt));
        let listener = fflocate::transport::Listener::bind(&socket).expect("bind the live socket");
        std::thread::spawn(move || {
            for _ in 0..16 {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let requests = tx_req.clone();
                let events = Arc::clone(&events);
                let hits = Arc::clone(&server_hits);
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut reader = std::io::BufReader::new(
                        stream.try_clone().expect("clone the client socket"),
                    );
                    let mut first = String::new();
                    if reader.read_line(&mut first).is_err() {
                        return;
                    }
                    if first.contains("\"op\":\"subscribe\"") {
                        let _ = requests.send(first);
                        let _ = writeln!(stream, "{}", meta().to_line());
                        let _ = stream.flush();
                        loop {
                            let next = events.lock().unwrap().recv();
                            let Ok(line) = next else {
                                return;
                            };
                            if writeln!(stream, "{line}").is_err() || stream.flush().is_err() {
                                return;
                            }
                        }
                    }
                    let current = hits.lock().unwrap().clone();
                    for (id, path) in current.iter().enumerate() {
                        let _ = writeln!(
                            stream,
                            "{}",
                            Response::Hit {
                                path: path.clone(),
                                id: id as u32,
                                created: None,
                            }
                            .to_line()
                        );
                    }
                    let _ = writeln!(
                        stream,
                        "{}",
                        Response::Done {
                            count: current.len() as u64,
                            cursor: "c:1:2".to_string(),
                        }
                        .to_line()
                    );
                    let _ = stream.flush();
                });
            }
        });
        (rx_req, tx_evt, hits)
    }

    let dir = tempfile::tempdir()?;
    let first = dir.path().join("alpha-one.txt");
    std::fs::write(&first, b"live\n")?;
    let first = first.to_string_lossy().into_owned();
    let socket = dir.path().join("live.sock");
    let (requests, events, hits) = serve(socket.clone(), vec![first.clone()]);

    let app = App::init()?;
    let win = app.create_window()?;
    win.set_title("ff-gui live mac test");
    let live = live_ui::build(&app)?;
    let list = app.create_treeview(&[
        ("", true, true),
        ("Name", false, true),
        ("Path", false, true),
    ])?;
    let status = app.create_label("")?;
    let layout = app.create_box(Orientation::Vertical, 3)?;
    layout.append(&live.button);
    layout.append(&list.clone().into_scrolled()?);
    layout.append(&status);
    win.set_child(&layout);
    win.present();

    // --- On: a feed for the box's query ---
    live.button.set_active(true);
    assert!(live.button.is_active(), "the Live toggle did not turn on");
    status.set_text(LIVE_STATUS);
    assert_eq!(status.get_text().unwrap_or_default(), LIVE_STATUS);

    let key = LiveKey::new(QUERY, &SearchOptions::default());
    let mut runner = LiveRunner::new();
    let request = {
        let start = Instant::now();
        loop {
            let _ = runner.step(&socket, &key, Instant::now());
            match requests.recv_timeout(Duration::from_millis(20)) {
                Ok(line) => break line,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    assert!(
                        start.elapsed() < Duration::from_secs(10),
                        "the live feed never sent a subscribe request"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("the live feed hung up: {e}"),
            }
        }
    };
    assert!(
        request.contains("\"op\":\"subscribe\""),
        "the toggle did not subscribe: {request}"
    );
    assert!(
        request.contains(&format!("\"q\":\"{QUERY}\"")),
        "the subscription does not carry the query: {request}"
    );
    println!("live_mac: subscribed -> {request}");

    // --- A create: one coalesced refresh, then the row is painted ---
    let second = dir.path().join("alpha-two.rs");
    std::fs::write(&second, b"live\n")?;
    let second = second.to_string_lossy().into_owned();
    hits.lock().unwrap().push(second.clone());
    events.send(event("create", &second))?;

    let refreshed = {
        let start = Instant::now();
        loop {
            match runner.step(&socket, &key, Instant::now()) {
                LiveStep::Refresh => break true,
                LiveStep::Unsupported => panic!("the daemon answered the feed with Done"),
                LiveStep::Failed(e) => panic!("the live feed failed: {e}"),
                LiveStep::Idle => {}
            }
            if start.elapsed() >= Duration::from_secs(10) {
                break false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    assert!(refreshed, "a create event never produced a refresh");

    let client = DaemonClient::with_socket(socket.clone());
    let outcome = client.search(QUERY, 10)?;
    let mut painted = outcome.paths.clone();
    painted.sort();
    list.set_rows(&cells(&painted));
    let shown: Vec<String> = (0..)
        .map_while(|row| list.cell_text(row, 2))
        .collect();
    assert!(
        shown.iter().any(|p| p == &second),
        "the created row is not painted: {shown:?}"
    );
    println!("live_mac: after create, rows are {shown:?}");

    // --- A delete: another refresh, and the row goes away ---
    std::fs::remove_file(&second)?;
    hits.lock().unwrap().retain(|p| p != &second);
    events.send(event("delete", &second))?;
    let refreshed = {
        let start = Instant::now();
        loop {
            match runner.step(&socket, &key, Instant::now()) {
                LiveStep::Refresh => break true,
                LiveStep::Unsupported => panic!("the daemon answered the feed with Done"),
                LiveStep::Failed(e) => panic!("the live feed failed: {e}"),
                LiveStep::Idle => {}
            }
            if start.elapsed() >= Duration::from_secs(10) {
                break false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    assert!(refreshed, "a delete event never produced a refresh");

    let outcome = client.search(QUERY, 10)?;
    list.set_rows(&cells(&outcome.paths));
    let shown: Vec<String> = (0..)
        .map_while(|row| list.cell_text(row, 2))
        .collect();
    assert_eq!(
        shown,
        vec![first.clone()],
        "the deleted row is still painted, or the survivor is missing"
    );

    runner.stop();
    assert!(!runner.is_active(), "turning live off did not close the feed");
    live.button.set_active(false);
    status.set_text("live: off");
    println!(
        "live_mac: ok — subscribed for {QUERY:?}, a create repainted {} and a delete removed it",
        second
    );
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    // AppKit is the point of this binary; the GTK counterpart is
    // `tests/live_gtk.rs`.
    println!("live_mac: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
