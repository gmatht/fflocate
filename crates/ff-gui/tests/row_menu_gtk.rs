//! The row context menu on GTK: the four actions, each one running, the popup
//! going up at the pointer, and Copy path leaving the row's path on the system
//! clipboard where a second, different GTK call reads it back.
//!
//! The menu is built through `ContextMenu::build`, which is the popup's first
//! step, and its items are fired through GTK's own `activate` signal — the same
//! signal a click in the menu emits. The popup itself is `gtk_menu_popup`, the
//! call the app's right-click handler reaches, and it is not modal on GTK, so it
//! can be shown and left to the display. Needs a display with GTK3, so on a
//! headless run it returns early; CI runs it under Xvfb.

#![cfg(unix)]

use ff_gui::RowAction;
// `Window::raw_handle` comes from the widget trait, as it does in the app.
use rswidgets::core::Widget as _;

/// The four actions a right-click offers, in menu order.
fn row_actions() -> [RowAction; 4] {
    [
        RowAction::Open,
        RowAction::OpenContainingFolder,
        RowAction::CopyPath,
        RowAction::OpenInTerminal,
    ]
}

/// Read the CLIPBOARD selection through GDK's own waiting read.
///
/// Deliberately not `compat::clipboard_get_text`: the copy side goes through
/// `gtk_clipboard_set_text`, so reading with `gtk_clipboard_wait_for_text` asks
/// GTK to serve the selection back the way another application would, instead
/// of trusting the same call that wrote it.
fn read_clipboard_directly(loader: &gtk_dynamic_loader::Loader) -> Option<String> {
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

#[test]
fn row_menu_items_run_and_copy_reaches_the_clipboard() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("row menu: no display, skipping");
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => {
            eprintln!("row menu: no GTK loader, skipping");
            return;
        }
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        eprintln!("row menu: not GTK3, skipping");
        return;
    }
    let Ok(app) = rswidgets::App::init() else {
        eprintln!("row menu: GTK did not initialise, skipping");
        return;
    };

    // A real file on disk, so the copy round trip is a real path rather than a
    // string nothing would find.
    let dir = tempfile::tempdir().expect("no temp dir");
    let file = dir.path().join("ff-gui-row-menu.txt");
    std::fs::write(&file, "row menu\n").expect("could not write the row's file");
    let path = file.to_string_lossy().into_owned();

    let ran: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let mut menu = rswidgets::compat::ContextMenu::new();
    for action in row_actions() {
        let ran = ran.clone();
        let path = path.clone();
        menu.append_item(
            action.label(),
            Box::new(move || {
                let line = ff_gui::run_row_action(action, &path, |text| {
                    rswidgets::compat::clipboard_set_text(text).map_err(|e| e.to_string())
                });
                ran.borrow_mut().push(line);
            }),
        );
    }

    let handle = menu.build().expect("the row menu could not be built");
    let items = rswidgets::compat::ContextMenu::items_of(handle);
    let labels: Vec<String> = items.iter().map(|(l, _)| l.clone()).collect();
    eprintln!("row menu: items {items:?}");
    assert_eq!(items.len(), 4, "the row menu does not offer four actions");
    assert_eq!(
        labels,
        row_actions()
            .iter()
            .map(|a| a.label().to_string())
            .collect::<Vec<_>>(),
        "the row menu labels are not the four row actions"
    );
    for (label, enabled) in &items {
        assert!(*enabled, "the row menu item {label:?} is disabled");
    }

    for index in 0..labels.len() {
        assert!(
            rswidgets::compat::ContextMenu::fire(handle, index),
            "item {index} ({}) could not be fired",
            labels[index]
        );
    }
    let lines = ran.borrow().clone();
    eprintln!("row menu: fired {lines:?}");
    assert_eq!(lines.len(), 4, "not every menu item ran its action");
    assert!(
        lines[2].starts_with("copied "),
        "Copy path ran {:?}",
        lines[2]
    );

    // The clipboard is the observable half of a copy: ask GTK for it with the
    // read that is not the write.
    let read_back = read_clipboard_directly(&loader)
        .expect("the clipboard holds no text after Copy path");
    eprintln!("clipboard holds {read_back:?}");
    assert_eq!(
        read_back, path,
        "Copy path did not leave the row's path on the clipboard"
    );
    assert!(
        std::fs::metadata(&read_back).map(|m| m.is_file()).unwrap_or(false),
        "the copied path {read_back:?} is not a file on disk"
    );

    // The popup the app's right-click handler calls. GTK3 menus are not modal,
    // so this goes up and stays up; a window gives it a real parent to be shown
    // against, and the call's own answer says it was shown rather than refused.
    let window = app.create_window().expect("no GTK window");
    window.set_title("row menu");
    let widget = window.raw_handle();
    match menu.popup_at_pointer(widget, std::ptr::null_mut()) {
        Ok(()) => eprintln!("row menu: popped up at the pointer ({} items)", labels.len()),
        Err(e) => panic!("the row menu did not pop up: {e}"),
    }
}
