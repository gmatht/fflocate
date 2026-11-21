//! A right-click on a **row** of the real results list, on AppKit.
//!
//! The macOS sibling of `tests/row_menu_row_gtk.rs`, and the row-level
//! counterpart of `tests/row_menu_interaction_mac.rs` (which builds the menu
//! directly). It starts where a user does: real result rows in a real
//! `TreeView`, the app's right-click handler installed on the list's own press
//! path, and an `NSRightMouseDown` delivered to the list's `CorroSheetView` at a
//! specific row. The press reaches `-[NSView rightMouseDown:]`, which is what a
//! real secondary click runs on this backend, and everything asserted after it
//! is read back from AppKit: the `NSMenu` the popup built, its `NSMenuItem`
//! titles and `isEnabled`, the pasteboard text, and the helper each action
//! launched (stubbed on a private `PATH`, so the test opens nothing on the
//! machine running it).
//!
//! AppKit needs the main thread, which the normal test harness cannot give a
//! test, so this target has `harness = false` and its own `fn main` — see
//! `[[test]]` in `Cargo.toml`.

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::os::unix::fs::PermissionsExt;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use ff_gui::{context_menu_target, run_row_action, RowAction};
    use rswidgets::compat::{CellValue, ContextMenu};
    use rswidgets::prelude::*;

    // --- The few AppKit sends this test needs ---
    //
    // The press, the event it carries and the pasteboard read are AppKit's own
    // calls, not the adapter's helpers: the point is to make the backend do what
    // a secondary click makes it do.
    fn send0(obj: *mut c_void, sel: &str) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send0i(obj: *mut c_void, sel: &str) -> isize {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send1p(obj: *mut c_void, sel: &str, arg: *mut c_void) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel), arg) }
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

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }

    /// The pasteboard's text, read with `NSPasteboard`'s own call.
    fn pasteboard_text() -> Option<String> {
        let pasteboard = send0(rswidgets::backends::apple::cls("NSPasteboard"), "generalPasteboard");
        if pasteboard.is_null() {
            return None;
        }
        let kind = rswidgets::backends::apple::nsstring("public.utf8-plain-text");
        rswidgets::backends::apple::nsstring_to_rust(send1p(pasteboard, "stringForType:", kind))
    }

    /// Queue an Escape on the application's event queue.
    ///
    /// AppKit's context-menu tracking loop is modal: `popUpContextMenu:` does
    /// not return until the menu is dismissed. The probe uses this same trick;
    /// queued ahead of the popup, the Escape closes it and the press returns.
    unsafe fn post_escape(app: *mut c_void, window_number: isize) {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, bool) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        let make: unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            usize,
            Point,
            usize,
            f64,
            isize,
            *mut c_void,
            *mut c_void,
            *mut c_void,
            bool,
            u16,
        ) -> *mut c_void = unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        let chars = rswidgets::backends::apple::nsstring("\u{1b}");
        let event = unsafe {
            make(
                rswidgets::backends::apple::cls("NSEvent"),
                rswidgets::backends::apple::selector(
                    "keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:",
                ),
                // NSKeyDown, no modifiers, Escape.
                10,
                Point { x: 0.0, y: 0.0 },
                0,
                0.0,
                window_number,
                std::ptr::null_mut(),
                chars,
                chars,
                false,
                53,
            )
        };
        unsafe {
            f(
                app,
                rswidgets::backends::apple::selector("postEvent:atStart:"),
                event,
                true,
            )
        }
    }

    /// Build an `NSRightMouseDown` at a point in `view`'s own coordinates.
    unsafe fn right_mouse_event(view: *mut c_void, x: f64, y: f64) -> *mut c_void {
        let convert: unsafe extern "C" fn(*mut c_void, *mut c_void, f64, f64, *mut c_void) -> Point =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        let in_window = unsafe {
            convert(
                view,
                rswidgets::backends::apple::selector("convertPoint:toView:"),
                x,
                y,
                std::ptr::null_mut(),
            )
        };
        let window = send0(view, "window");
        if window.is_null() {
            return std::ptr::null_mut();
        }
        let number = send0i(window, "windowNumber");
        let make: unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            usize,
            Point,
            usize,
            f64,
            isize,
            *mut c_void,
            isize,
            isize,
            f32,
        ) -> *mut c_void = unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe {
            make(
                rswidgets::backends::apple::cls("NSEvent"),
                rswidgets::backends::apple::selector(
                    "mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:",
                ),
                // NSEventTypeRightMouseDown.
                3,
                in_window,
                0,
                0.0,
                number,
                std::ptr::null_mut(),
                0,
                1,
                1.0,
            )
        }
    }

    let app = App::init()?;
    let win = app.create_window()?;
    win.set_title("ff-gui row menu on a row test");

    // --- Three rows, each in its own directory so a launch names its row ---
    let root = tempfile::tempdir()?;
    let mut paths = Vec::new();
    for (index, name) in ["row-zero.txt", "row-one.rs", "row-two.md"].iter().enumerate() {
        let row_dir = root.path().join(index.to_string());
        std::fs::create_dir_all(&row_dir)?;
        let file = row_dir.join(name);
        std::fs::write(&file, "row menu\n")?;
        paths.push(file.to_string_lossy().into_owned());
    }

    // --- A recording stub for the launcher, on a private PATH ---
    //
    // AppKit's row actions all go through `open` (`open <path>`, `open -R`, and
    // `open -a Terminal <dir>`), so one stub records every launch instead of
    // opening a document, Finder or Terminal on the machine running the test.
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin)?;
    let log = root.path().join("launch.log");
    let stub = bin.join("open");
    std::fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s %s\\n' \"$(basename \"$0\")\" \"$*\" >> \"$FF_GUI_LAUNCH_LOG\"\n",
    )?;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))?;
    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{}:{old_path}", bin.display()));
    std::env::set_var("FF_GUI_LAUNCH_LOG", &log);
    std::env::remove_var("TERMINAL");

    // --- The app's results list and status line ---
    let list = app.create_treeview(&[("", true, true), ("Name", false, true), ("Path", false, true)])?;
    let rows: Vec<Vec<CellValue>> = paths
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
        .collect();
    list.set_rows(&rows);
    let painted = list.clone().into_scrolled()?;
    let status = app.create_label("")?;
    let column = app.create_box(Orientation::Vertical, 2)?;
    column.append(&painted);
    column.append(&status);
    win.set_child(&column);
    win.present();
    // Lay the list out at a real size: the press is hit-tested against the
    // width the list was last drawn at.
    let view = win.raw_handle();
    let _ = rswidgets::backends_macos_adapter::capture(view, Some((900.0, 560.0)));
    send0v(view, "layoutSubtreeIfNeeded");

    let canvas_id = rswidgets::backends_macos_adapter::Canvas(list.raw_handle()).canvas_id();
    assert_ne!(
        canvas_id, 0,
        "the list's canvas has no id, so no press can reach it"
    );

    // --- The app's right-click handler shape, on the list's press path ---
    let current: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(list.displayed_column(2)));
    let live: Rc<RefCell<Option<ContextMenu>>> = Rc::new(RefCell::new(None));
    let dispatched: Rc<RefCell<Vec<(RowAction, String)>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let app_handle = app.clone();
        let press_list = list.clone();
        let current = current.clone();
        let live = live.clone();
        let status = status.clone();
        let dispatched = dispatched.clone();
        list.connect_button_press_event(move |button, x, y, event| {
            // The list re-sorts on a header click, so the order `current` holds
            // is re-read before any index is used.
            *current.borrow_mut() = press_list.displayed_column(2);
            if button != 3 {
                return;
            }
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
        })?;
    }

    // The y that lands inside a row, found through the list's own hit test so a
    // change to the row height cannot silently aim at a neighbour.
    let y_of = |row: usize| -> f64 {
        let mut y = 1.0;
        while y < 400.0 {
            if list.row_at(60.0, y) == Some(row) {
                return y;
            }
            y += 1.0;
        }
        panic!("row {row} is not on screen");
    };

    let clicked = 1usize;
    let marked = format!("/{clicked}");

    // Right-click `row` through AppKit and leave its menu in `live`.
    //
    // The Escape is queued first (three of them: the first can be consumed
    // before tracking starts), so the modal loop always ends — a menu that
    // never closes fails on the missing assertions instead of hanging the job.
    let right_click = |row: usize| -> usize {
        live.borrow_mut().take();
        let canvas = list.raw_handle();
        let window = send0(canvas, "window");
        let number = if window.is_null() { 0 } else { send0i(window, "windowNumber") };
        let nsapp = send0(rswidgets::backends::apple::cls("NSApplication"), "sharedApplication");
        for _ in 0..3 {
            unsafe { post_escape(nsapp, number) };
        }
        let event = unsafe { right_mouse_event(canvas, 60.0, y_of(row)) };
        assert!(!event.is_null(), "no NSRightMouseDown could be built");
        send1v(canvas, "rightMouseDown:", event);
        live.borrow_mut()
            .as_ref()
            .map(|m| m.handle())
            .expect("the right-click produced no menu")
    };

    // --- Right-click row 1: a menu appears, for that row ---
    let handle = right_click(clicked);
    assert_ne!(handle, 0, "the row menu was not built");
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

    let items = ContextMenu::items_of(handle);
    let labels: Vec<String> = items.iter().map(|(l, _)| l.clone()).collect();
    let expected: Vec<String> = RowAction::all().iter().map(|a| a.label().to_string()).collect();
    assert_eq!(
        labels, expected,
        "the NSMenu on a row is not the app's action list; got {items:?}"
    );
    for label in [
        RowAction::Open,
        RowAction::OpenContainingFolder,
        RowAction::CopyPath,
        RowAction::OpenInTerminal,
    ]
    .iter()
    .map(|a| a.label())
    {
        assert!(
            labels.iter().any(|l| l == label),
            "the row menu does not offer {label:?}: {labels:?}"
        );
    }
    for (label, enabled) in &items {
        assert!(
            *enabled,
            "the NSMenu item {label:?} is disabled (AppKit says it is)"
        );
    }
    assert!(
        !labels.iter().any(|l| l.contains("not available")),
        "the menu still claims the feature is unavailable: {labels:?}"
    );
    println!("row menu on a row: NSMenu items {items:?}");

    // --- Choosing an item: a fresh right-click, then AppKit's own dispatch ---
    let choose = |row: usize, action: RowAction| -> (usize, String, String) {
        let handle = right_click(row);
        let items = ContextMenu::items_of(handle);
        let index = items
            .iter()
            .position(|(l, _)| l == action.label())
            .unwrap_or_else(|| panic!("the menu has no {:?} item", action.label()));
        let before = dispatched.borrow().len();
        assert!(
            ContextMenu::fire(handle, index),
            "choosing {:?} did not reach AppKit's performActionForItemAtIndex:",
            action.label()
        );
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
        let path = dispatched.borrow().last().expect("a dispatch").1.clone();
        (handle, path, status.get_text().unwrap_or_default())
    };

    // Copy path: AppKit's dispatch runs the app's runner, and the path it
    // copied is the row's — readable through the app's own clipboard call and
    // through NSPasteboard directly.
    let (_handle, path, message) = choose(clicked, RowAction::CopyPath);
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
        "the pasteboard does not hold the right-clicked row's path"
    );
    assert_eq!(
        pasteboard_text().as_deref(),
        Some(paths[clicked].as_str()),
        "NSPasteboard's own read does not agree with the app's"
    );

    // The other three: each is chosen from its own right-click, must run for
    // the same row, and must really launch the helper on that row's path. The
    // helper is a stub, so the log records the launch instead of a window
    // opening.
    for action in [
        RowAction::Open,
        RowAction::OpenContainingFolder,
        RowAction::OpenInTerminal,
    ] {
        let launches_before = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .count();
        let (_handle, path, message) = choose(clicked, action);
        assert_eq!(path, paths[clicked], "{:?} ran for the wrong row", action.label());
        assert!(
            message.starts_with(action.label()),
            "{:?} did not report its own action, got {message:?}",
            action.label()
        );
        assert!(
            !message.contains("not available")
                && !message.contains("no native menu")
                && !message.contains("failed to start"),
            "{:?} did not launch: {message:?}",
            action.label()
        );
        let start = Instant::now();
        let launch = loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            if lines.len() > launches_before {
                break lines[lines.len() - 1].to_string();
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "no launcher ran for {:?}",
                action.label()
            );
            std::thread::sleep(Duration::from_millis(20));
        };
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
        println!("row menu on a row: {:?} launched {launch}", action.label());
    }

    // A right-click on blank space below the rows offers nothing: the menu is
    // for a row, not for whatever was last selected.
    live.borrow_mut().take();
    {
        let canvas = list.raw_handle();
        let window = send0(canvas, "window");
        let number = if window.is_null() { 0 } else { send0i(window, "windowNumber") };
        let nsapp = send0(rswidgets::backends::apple::cls("NSApplication"), "sharedApplication");
        for _ in 0..3 {
            unsafe { post_escape(nsapp, number) };
        }
        let event = unsafe { right_mouse_event(canvas, 60.0, y_of(paths.len() - 1) + 400.0) };
        assert!(!event.is_null(), "no NSRightMouseDown could be built");
        send1v(canvas, "rightMouseDown:", event);
    }
    assert!(
        live.borrow_mut().is_none(),
        "a right-click below the rows opened a row menu"
    );

    println!(
        "row menu on a row: right-clicked {}, NSMenu shown, Copy path put {} on the pasteboard, \
         Open / Open containing folder / Open in terminal each launched for that row",
        paths[clicked], paths[clicked]
    );
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    // The NSMenu and the pasteboard are AppKit's; the GTK counterpart is
    // `tests/row_menu_row_gtk.rs`.
    println!("row_menu_row_mac: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
