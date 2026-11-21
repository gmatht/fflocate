//! The app's interaction loop on GTK, without a window: typing into an entry
//! arms the debounce, the elapsed window issues a request to a *recording*
//! backend, its rows land in the list, and a click / header click / double
//! click reach the list's own press handling.
//!
//! The macOS probe proves the same facts through AppKit's real text and mouse
//! plumbing; this is the Linux counterpart its mission asks for. The debounce
//! is `ff_gui::Debounce`, the type the GUI's pump itself ticks, and the clicks
//! go through `TreeView::click_at`/`double_click_at`, which are the functions
//! the canvas' button and double-click signals call.
//!
//! Needs a display with GTK3 (the entry's `changed` signal is a real GTK
//! signal), so a headless run returns early; CI runs it under Xvfb.

#![cfg(unix)]

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::rc::Rc;
use std::time::Duration;

use ff_gui::Debounce;
use rswidgets::compat::{CellValue, TreeView};

/// The three paths the recording backend answers with.
fn answer() -> [&'static str; 3] {
    ["/rec/alpha.rs", "/rec/beta.md", "/rec/gamma.txt"]
}

fn cells(paths: &[String]) -> Vec<Vec<CellValue>> {
    paths
        .iter()
        .map(|p| {
            let name = p.rsplit('/').next().unwrap_or(p).to_string();
            vec![
                CellValue::Icon {
                    name: "text-x-generic".to_string(),
                    fallback: "-".to_string(),
                },
                CellValue::Text(name),
                CellValue::Text(p.clone()),
            ]
        })
        .collect()
}

/// The Name column of every row, read back from the list's own model.
fn names(list: &TreeView) -> Vec<String> {
    (0..)
        .map_while(|row| list.cell_text(row, 1))
        .collect()
}

/// Pump GTK's main loop, the way `menubar_gtk` does, so a `changed` signal and
/// the timer-driven pump can run.
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

#[test]
fn typing_debounces_into_a_request_and_the_rows_repaint_then_clicks_land() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("interaction: no display, skipping");
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => {
            eprintln!("interaction: no GTK loader, skipping");
            return;
        }
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        eprintln!("interaction: not GTK3, skipping");
        return;
    }
    let app = match rswidgets::core::App::init() {
        Ok(app) => app,
        Err(_) => {
            eprintln!("interaction: GTK did not initialise, skipping");
            return;
        }
    };

    let entry = app.create_entry().expect("entry");
    // The app's own columns: all three headers sort, the icon column included.
    let list = TreeView::new(&[("", true, true), ("Name", false, true), ("Path", false, true)])
        .expect("the results list could not be created");

    // The app's own debounce window, shortened so the test does not sleep for
    // the GUI's 150 ms; the type and the semantics are the GUI's.
    let debounce = Rc::new(Debounce::new(Duration::from_millis(120)));
    // The recording backend: every request is appended here, and answers with
    // the three fixed paths above.
    let requests: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let debounce = debounce.clone();
        entry
            .connect_changed(move || debounce.arm())
            .expect("the entry has no changed signal");
    }

    // What the pump does when the deadline elapses: read the query, ask the
    // backend, paint the answer into the list.
    let issue = |query: &str| {
        requests.borrow_mut().push(query.to_string());
        let paths: Vec<String> = answer().iter().map(|p| (*p).to_string()).collect();
        list.set_rows(&cells(&paths));
        debounce.clear();
    };

    // Typing: the change signal must arm the debounce and must not send a
    // request on its own.
    entry.set_text("rust");
    pump(&loader, 20);
    assert!(
        debounce.pending(),
        "typing did not arm the debounce: the window would never elapse"
    );
    assert!(
        requests.borrow().is_empty(),
        "a request went out while the user was still typing: {:?}",
        requests.borrow()
    );

    // The pump's ticks: nothing until the window passes, then exactly one
    // request carrying the text that is in the box.
    let mut issued = false;
    for _ in 0..200 {
        if debounce.due() {
            let query = entry.get_text().unwrap_or_default();
            issue(&query);
            issued = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
        pump(&loader, 1);
    }
    assert!(issued, "the debounce never elapsed, so no request was issued");
    assert_eq!(
        *requests.borrow(),
        vec!["rust".to_string()],
        "the request did not carry the typed query exactly once"
    );
    assert!(!debounce.pending(), "the debounce stayed armed after issuing");

    // The rows are the backend's answer, read back out of the list's own
    // model rather than from the closure that set them.
    assert_eq!(
        names(&list),
        vec!["alpha.rs".to_string(), "beta.md".to_string(), "gamma.txt".to_string()],
        "the list does not hold the backend's rows"
    );
    assert_eq!(list.cell_text(2, 2).as_deref(), Some("/rec/gamma.txt"));
    println!(
        "interaction: typed \"rust\" -> debounce -> request {:?} -> rows {:?}",
        requests.borrow(),
        names(&list)
    );

    // A click on the second row selects it...
    let row_y = 22.0 + 20.0 + 10.0; // header band + one row + half the next
    assert_eq!(list.click_at(60.0, row_y, 1), Some(1), "the click missed row 1");
    assert_eq!(
        list.selected_path(2).as_deref(),
        Some("/rec/beta.md"),
        "the clicked row is not the selected one"
    );

    // ...a click on the Name header sorts by it, and again reverses...
    assert_eq!(list.sort_state(), None, "the list started sorted");
    // 320 of 640: inside the Name column in both column layouts (fixed thirds,
    // and the icon-share layout the current list uses).
    assert_eq!(list.click_at(320.0, 6.0, 1), None, "a header click is not a row");
    assert_eq!(list.sort_state(), Some((1, true)), "the header click did not sort");
    assert_eq!(
        names(&list),
        vec!["alpha.rs".to_string(), "beta.md".to_string(), "gamma.txt".to_string()]
    );
    list.click_at(320.0, 6.0, 1);
    assert_eq!(list.sort_state(), Some((1, false)), "the second click did not reverse");
    assert_eq!(
        names(&list),
        vec!["gamma.txt".to_string(), "beta.md".to_string(), "alpha.rs".to_string()],
        "reversing did not reorder the rows"
    );

    // ...and a double click opens the row under the pointer, through the
    // app's own handler (`connect_open_on_double_click`), which reads the
    // clicked row's path out of the list and hands it to the Open action. The
    // opener is a stub on a private PATH, so the test records the path it was
    // asked to open instead of starting a file manager on this machine.
    let opener_dir = tempfile::tempdir().expect("no temp dir");
    let open_log = opener_dir.path().join("open.log");
    let opener = opener_dir.path().join("xdg-open");
    std::fs::write(
        &opener,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FF_GUI_OPEN_LOG\"\n",
    )
    .expect("stub opener");
    std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755))
        .expect("stub executable");
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{old_path}", opener_dir.path().display()));
    std::env::set_var("FF_GUI_OPEN_LOG", &open_log);
    std::env::remove_var("XDG_CURRENT_DESKTOP");
    std::env::remove_var("TERMINAL");

    let reported: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let reported = reported.clone();
        ff_gui::connect_open_on_double_click(&list, 2, move |line| {
            reported.borrow_mut().push(line)
        });
    }
    assert_eq!(list.double_click_at(row_y), Some(1), "the double click missed row 1");
    assert_eq!(
        reported.borrow().len(),
        1,
        "the double click did not dispatch Open: {:?}",
        reported.borrow()
    );
    assert!(
        reported.borrow()[0].starts_with("Open \u{2192} beta.md"),
        "Open reported {:?} for the clicked row, not that row's path",
        reported.borrow()
    );
    // The launcher is what actually opens something: it must have been handed
    // the clicked row's path, and no other row's. The child runs detached, so
    // poll the log rather than assuming it has already written.
    let start = std::time::Instant::now();
    let mut launched = String::new();
    while start.elapsed() < Duration::from_secs(5) {
        launched = std::fs::read_to_string(&open_log).unwrap_or_default();
        if !launched.trim().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        launched.trim(),
        "/rec/beta.md",
        "Open was handed {:?}, not the double-clicked row's path",
        launched.trim()
    );
    println!(
        "interaction: click selected {:?}; header sorted {:?} then {:?}; double click reported {:?} and opened {:?}",
        list.selected_path(2),
        Some((1, true)),
        list.sort_state(),
        reported.borrow(),
        launched.trim()
    );

    // The leftmost (icon) header sorts too, by the kind its glyph shows: a
    // folder and a file arrive in name order here, and the click puts the file
    // first because its glyph sorts before the folder's.
    let mut mixed = cells(&["/rec/one.rs".to_string(), "/rec/two.md".to_string()]);
    if let CellValue::Icon { name, fallback } = &mut mixed[1][0] {
        *name = "folder".to_string();
        *fallback = "d".to_string();
    }
    list.set_rows(&mixed);
    assert_eq!(
        names(&list),
        vec!["two.md".to_string(), "one.rs".to_string()],
        "the rows did not arrive in name order (sort state {:?})",
        list.sort_state()
    );
    assert_eq!(
        list.click_at(3.0, 6.0, 1),
        None,
        "a header click is not a row"
    );
    assert_eq!(
        list.sort_state(),
        Some((0, true)),
        "a click on the leftmost header did not sort by the icon column"
    );
    assert_eq!(
        names(&list),
        vec!["one.rs".to_string(), "two.md".to_string()],
        "the icon header sorted, but the rows did not reorder by kind"
    );
    println!(
        "interaction: icon header (x=3) sorted by kind -> {:?}",
        names(&list)
    );
}
