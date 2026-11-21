//! A right-click on a **row** of the real results list, on GTK.
//!
//! The other row-menu test builds a menu directly and fires its items; this one
//! starts where a user does. It puts real result rows in a real `TreeView`,
//! installs the app's right-click handler shape on the list's own press
//! plumbing, and feeds a button-3 press at a specific row through
//! `TreeView::click_at` — the same call the canvas' button signal makes. From
//! there the assertions are about the menu that appears: built for the row
//! under the pointer, with the app's actions, all sensitive, shown through
//! GTK's own popup. Choosing Copy path writes the row's path through
//! `compat::clipboard_set_text`, and the test reads it back through
//! `compat::clipboard_get_text` — the app's own clipboard path — and through
//! GDK's independent waiting read. Each of Open, Open containing folder and
//! Open in terminal is then chosen from a fresh right-click and must dispatch
//! the real launcher for that same row; their helper programs are stub scripts
//! on a private PATH, so the test records what was launched instead of opening
//! windows on the machine running it.
//!
//! Needs a display with GTK3, so a headless run returns early; CI runs it under
//! Xvfb. A build without GTK (the macOS run) skips before touching AppKit.

#![cfg(unix)]

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ff_gui::{
    context_menu_target, run_row_action, RowAction, FOLDER_OPENER_CANDIDATES,
    TERMINAL_CANDIDATES,
};
use rswidgets::compat::{CellValue, ContextMenu};
use rswidgets::prelude::Orientation;

/// The four actions the mission names.
const NAMED: [RowAction; 4] = [
    RowAction::Open,
    RowAction::OpenContainingFolder,
    RowAction::CopyPath,
    RowAction::OpenInTerminal,
];

/// Pump GTK's main loop so the menu's popup and signals run, as
/// `menubar_gtk` and `created_range_gtk` do.
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

/// Read the CLIPBOARD selection with GDK's own waiting read.
///
/// Deliberately independent of `compat::clipboard_get_text`: the test checks
/// both, so a compat read that echoed its own write could not fake the copy.
fn read_clipboard_gdk(loader: &gtk_dynamic_loader::Loader) -> Option<String> {
    let syms = &loader.symbols;
    let intern = syms.gdk_atom_intern?;
    let get = syms.gtk_clipboard_get?;
    let wait = syms.gtk_clipboard_wait_for_text?;
    // SAFETY: GDK3's own selection calls; every pointer is null-checked.
    unsafe {
        let atom = intern(b"CLIPBOARD\0".as_ptr(), 0);
        if atom.is_null() {
            return None;
        }
        let clipboard = get(atom);
        if clipboard.is_null() {
            return None;
        }
        let text = wait(clipboard);
        if text.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(text as *const std::ffi::c_char)
                .to_string_lossy()
                .into_owned(),
        )
    }
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

/// Put a recording stub for every helper the actions may launch on a private
/// PATH, so Open / containing folder / terminal run here without opening
/// windows, and append a line per launch to `log`.
fn stub_launchers(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("stub bin dir");
    let log = dir.join("launch.log");
    let script = "#!/bin/sh\nprintf '%s %s\\n' \"$(basename \"$0\")\" \"$*\" >> \"$FF_GUI_LAUNCH_LOG\"\n";
    for program in TERMINAL_CANDIDATES.iter().chain(FOLDER_OPENER_CANDIDATES.iter()) {
        let path = bin.join(program);
        std::fs::write(&path, script).expect("stub script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("stub executable");
    }
    (bin, log)
}

/// Wait for the launcher log to grow past `seen` lines and return the new one.
fn wait_for_launch(log: &std::path::Path, seen: usize) -> String {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() > seen {
            return lines[lines.len() - 1].to_string();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no launcher ran: {:?} did not grow past {seen} line(s)", log);
}

#[test]
fn right_clicking_a_row_opens_its_menu_and_copy_path_reaches_the_clipboard() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("row menu on a row: no display, skipping");
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => {
            eprintln!("row menu on a row: no GTK loader, skipping");
            return;
        }
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        eprintln!("row menu on a row: not GTK3, skipping");
        return;
    }
    let Ok(app) = rswidgets::App::init() else {
        eprintln!("row menu on a row: GTK did not initialise, skipping");
        return;
    };

    // --- Three rows, each in its own directory so a launch names its row ---
    let root = tempfile::tempdir().expect("no temp dir");
    let mut paths = Vec::new();
    for (index, name) in ["row-zero.txt", "row-one.rs", "row-two.md"].iter().enumerate() {
        let row_dir = root.path().join(index.to_string());
        std::fs::create_dir_all(&row_dir).expect("row dir");
        let file = row_dir.join(name);
        std::fs::write(&file, "row menu\n").expect("could not write the row's file");
        paths.push(file.to_string_lossy().into_owned());
    }
    let (bin, log) = stub_launchers(root.path());
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{old_path}", bin.display()));
    std::env::set_var("FF_GUI_LAUNCH_LOG", &log);
    std::env::remove_var("TERMINAL");
    std::env::remove_var("XDG_CURRENT_DESKTOP");

    // --- The app's results list and status line ---
    let win = app.create_window().expect("window");
    win.set_title("ff-gui row menu on a row test");
    let list = app
        .create_treeview(&[("", true, true), ("Name", false, true), ("Path", false, true)])
        .expect("the results list could not be created");
    list.set_rows(&cells(&paths));
    let status = app.create_label("").expect("status");
    let layout = app.create_box(Orientation::Vertical, 2).expect("layout");
    let scrolled = list.clone().into_scrolled().expect("scroller");
    layout.append(&scrolled);
    layout.append(&status);
    win.set_child(&layout);
    win.present();
    pump(&loader, 50);

    // --- The app's right-click handler shape, installed on the list's press ---
    let current: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(list.displayed_column(2)));
    let live: Rc<RefCell<Option<ContextMenu>>> = Rc::new(RefCell::new(None));
    // What each fired action received, so a dispatch can be traced to the row.
    let dispatched: Rc<RefCell<Vec<(RowAction, String)>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let app_handle = app.clone();
        let press_list = list.clone();
        let current = current.clone();
        let live = live.clone();
        let status = status.clone();
        let dispatched = dispatched.clone();
        list.connect_button_press_event(move |button, x, y, event| {
            // The row order the list shows is the one `current` must hold: a
            // header click would have re-sorted it since.
            *current.borrow_mut() = press_list.displayed_column(2);
            if button != 3 {
                return;
            }
            // Right-click acts on the row under the pointer; blank space below
            // the rows does nothing rather than acting on a neighbour.
            let row = press_list.row_at(x, y);
            let path = match context_menu_target(button, row.map(|r| r as i32), &current.borrow()) {
                Some(p) => p,
                None => return,
            };
            // Show the selection too, so it is obvious which row the menu is for.
            press_list.select_row(row.unwrap_or(0));
            let mut menu = match app_handle.new_context_menu() {
                Ok(m) => m,
                Err(e) => {
                    status.set_text(&format!("context menu unavailable: {e}"));
                    return;
                }
            };
            // One-shot guard per menu, exactly as the app arms it.
            let menu_fired: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
            for action in RowAction::all() {
                let path = path.clone();
                let status = status.clone();
                let menu_fired = menu_fired.clone();
                let dispatched = dispatched.clone();
                menu.append_item(
                    action.label(),
                    Box::new(move || {
                        if *menu_fired.borrow() {
                            return;
                        }
                        *menu_fired.borrow_mut() = true;
                        dispatched.borrow_mut().push((action, path.clone()));
                        let msg = run_row_action(action, &path, |text| {
                            rswidgets::compat::clipboard_set_text(text).map_err(|e| e.to_string())
                        });
                        status.set_text(&msg);
                    }),
                );
            }
            if let Err(e) = menu.popup_at_pointer(press_list.raw_handle(), event) {
                status.set_text(&format!("context menu failed: {e}"));
                return;
            }
            *live.borrow_mut() = Some(menu);
        })
        .expect("the list has no press signal");
    }

    // The y that lands inside row `row`, found through the list's own hit-test
    // so a change to the row height cannot silently aim at a neighbour.
    let y_of = |row: usize| -> f64 {
        let mut y = list.header_height() + 1.0;
        while y < list.header_height() + 400.0 {
            if list.row_at(0.0, y) == Some(row) {
                return y;
            }
            y += 1.0;
        }
        panic!("row {row} is not on screen");
    };

    // --- Right-click row 1: a menu appears, for that row ---
    let clicked = 1usize;
    // Each row lives in its own numbered directory, so a launch names its row;
    // "Open containing folder" passes the directory without a trailing slash.
    let marked = format!("/{clicked}");
    live.borrow_mut().take();
    let hit = list.click_at(40.0, y_of(clicked), 3);
    assert_eq!(hit, Some(clicked), "the press did not hit row {clicked}");
    pump(&loader, 20);

    assert_eq!(
        status.get_text().unwrap_or_default(),
        "",
        "the right-click reported a failure instead of opening a menu"
    );
    assert_eq!(
        list.selected_path(2).as_deref(),
        Some(paths[clicked].as_str()),
        "the right-clicked row was not selected"
    );

    let menu = live
        .borrow_mut()
        .as_ref()
        .map(|m| m.handle())
        .expect("the right-click produced no menu");
    assert_ne!(menu, 0, "the row menu was not built");
    assert!(
        gtk_dynamic_loader::context_menu_visible(&loader, menu as u64),
        "the row menu was built but not shown"
    );

    let items = ContextMenu::items_of(menu);
    let labels: Vec<String> = items.iter().map(|(l, _)| l.clone()).collect();
    let expected: Vec<String> = RowAction::all().iter().map(|a| a.label().to_string()).collect();
    assert_eq!(
        labels, expected,
        "the menu on a row is not the app's action list; got {items:?}"
    );
    for label in NAMED.iter().map(|a| a.label()) {
        assert!(
            labels.iter().any(|l| l == label),
            "the row menu does not offer {label:?}: {labels:?}"
        );
    }
    for (label, enabled) in &items {
        assert!(
            *enabled,
            "the row menu item {label:?} is greyed out (GTK says it is insensitive)"
        );
    }
    eprintln!("row menu on a row: items {items:?}, shown at the pointer");
    assert!(
        !labels.iter().any(|l| l.contains("not available")),
        "the menu still claims the feature is unavailable: {labels:?}"
    );

    // --- Choosing an item, from a fresh right-click on the same row ---
    let every_action = list.clone();
    let choose = |row: usize, action: RowAction| -> (String, String) {
        live.borrow_mut().take();
        let hit = every_action.click_at(40.0, y_of(row), 3);
        assert_eq!(hit, Some(row), "the press did not hit row {row}");
        pump(&loader, 20);
        let handle = live
            .borrow_mut()
            .as_ref()
            .map(|m| m.handle())
            .expect("the right-click produced no menu");
        let items = ContextMenu::items_of(handle);
        let index = items
            .iter()
            .position(|(l, _)| l == action.label())
            .unwrap_or_else(|| panic!("the menu has no {:?} item", action.label()));
        let before = dispatched.borrow().len();
        assert!(
            ContextMenu::fire(handle, index),
            "choosing {:?} did not reach GTK's activate signal",
            action.label()
        );
        pump(&loader, 20);
        {
            let fired = dispatched.borrow();
            assert_eq!(
                fired.len(),
                before + 1,
                "choosing {:?} did not dispatch exactly once",
                action.label()
            );
            let (fired_action, fired_path) = fired.last().expect("a dispatch").clone();
            assert_eq!(fired_action, action, "the wrong handler ran");
            assert_eq!(
                fired_path, paths[row],
                "{:?} ran for the wrong row",
                action.label()
            );
        }
        (
            dispatched.borrow().last().expect("a dispatch").1.clone(),
            status.get_text().unwrap_or_default(),
        )
    };

    // Copy path: the handler is the app's runner, and the path it copied is
    // the row's, readable through the app's own clipboard call.
    let (path, message) = choose(clicked, RowAction::CopyPath);
    assert_eq!(path, paths[clicked], "Copy path ran for the wrong row");
    assert_eq!(
        message,
        format!("copied {}", paths[clicked]),
        "the status line does not say what was copied"
    );
    let through_app = rswidgets::compat::clipboard_get_text()
        .expect("the app's clipboard read failed after a copy");
    assert_eq!(
        through_app, paths[clicked],
        "the clipboard does not hold the right-clicked row's path"
    );
    assert_eq!(
        read_clipboard_gdk(&loader).as_deref(),
        Some(paths[clicked].as_str()),
        "GDK's own read of CLIPBOARD does not agree with the app's"
    );

    // The other three: each is chosen from its own right-click, must run for
    // the same row, and must really launch the helper on that row's path. The
    // helpers are stubs on the test's PATH, so the log records the launch
    // instead of a window opening.
    for action in [
        RowAction::Open,
        RowAction::OpenContainingFolder,
        RowAction::OpenInTerminal,
    ] {
        let launches_before = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .count();
        let (path, message) = choose(clicked, action);
        assert_eq!(path, paths[clicked], "{:?} ran for the wrong row", action.label());
        assert!(
            message.starts_with(action.label()),
            "{:?} did not report its own action, got {message:?}",
            action.label()
        );
        assert!(
            !message.contains("not available") && !message.contains("not wired up"),
            "{:?} still reports the feature as missing: {message:?}",
            action.label()
        );
        let launch = wait_for_launch(&log, launches_before);
        assert!(
            launch.contains(&marked),
            "{:?} launched {launch:?}, which does not name the right-clicked row {marked}",
            action.label()
        );
        for other in 0..paths.len() {
            if other != clicked {
                assert!(
                    !launch.contains(&format!("/{other}")),
                    "{:?} launched {launch:?}, which names another row",
                    action.label()
                );
            }
        }
        eprintln!("row menu on a row: {:?} launched {launch}", action.label());
    }

    // A right-click on blank space below the rows offers nothing: the menu is
    // for a row, not for whatever was last selected.
    live.borrow_mut().take();
    list.click_at(40.0, y_of(paths.len() - 1) + 400.0, 3);
    pump(&loader, 10);
    assert!(
        live.borrow_mut().is_none(),
        "a right-click below the rows opened a row menu"
    );

    println!(
        "row menu on a row: right-clicked {}, menu {:?} shown, Copy path put {} on the clipboard, \
         Open / Open containing folder / Open in terminal each launched for that row",
        paths[clicked],
        NAMED.iter().map(|a| a.label()).collect::<Vec<_>>(),
        paths[clicked]
    );
}
