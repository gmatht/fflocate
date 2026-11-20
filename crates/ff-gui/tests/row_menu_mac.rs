//! The row context menu and the clipboard on AppKit, under `cargo test`.
//!
//! What a `cargo test` thread cannot do is show the menu: `popUpContextMenu:`
//! enters AppKit's modal tracking loop on the main thread, and a menu that is
//! not dismissed would hang the job, so the popup itself is driven by
//! `examples/macos_probe.rs` (a binary, which CI runs on the main thread with a
//! real window). Everything the popup depends on is checked here: the menu is
//! built from the app's four row actions, its items and their enabled state are
//! read back out of the `NSMenu`, each item is fired through AppKit's
//! `performActionForItemAtIndex:` — the same target/action dispatch a click
//! inside the menu runs — and Copy path's effect is read back from the
//! pasteboard through a separate AppKit call.

#![cfg(target_os = "macos")]

use std::ffi::c_void;

use ff_gui::RowAction;
use rswidgets::backends::apple::{cls, msg_shim, nsstring, nsstring_to_rust, selector};

/// The four actions a right-click offers, in menu order.
fn row_actions() -> [RowAction; 4] {
    [
        RowAction::Open,
        RowAction::OpenContainingFolder,
        RowAction::CopyPath,
        RowAction::OpenInTerminal,
    ]
}

unsafe fn send0(obj: *mut c_void, sel: &str) -> *mut c_void {
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
        unsafe { std::mem::transmute(msg_shim()) };
    unsafe { f(obj, selector(sel)) }
}

unsafe fn send1v(obj: *mut c_void, sel: &str, arg: *mut c_void) -> *mut c_void {
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
        unsafe { std::mem::transmute(msg_shim()) };
    unsafe { f(obj, selector(sel), arg) }
}

/// Read the general pasteboard through AppKit directly.
///
/// Not `compat::clipboard_get_text`: Copy writes through the adapter, so asking
/// `NSPasteboard` for its string is a second, independent read of the same
/// system object.
fn pasteboard_text() -> Option<String> {
    unsafe {
        let pasteboard = send0(cls("NSPasteboard"), "generalPasteboard");
        if pasteboard.is_null() {
            return None;
        }
        let kind = nsstring("public.utf8-plaintext");
        nsstring_to_rust(send1v(pasteboard, "stringForType:", kind))
    }
}

#[test]
fn the_row_menu_and_copy_path_work_on_appkit() {
    let dir = tempfile::tempdir().expect("no temp dir");
    let file = dir.path().join("ff-gui-row-menu.txt");
    std::fs::write(&file, "row menu\n").expect("could not write the row's file");
    let path = file.to_string_lossy().into_owned();

    // Start from a pasteboard that does not already hold the path, so a read
    // that "succeeds" cannot be the previous contents.
    rswidgets::compat::clipboard_set_text("not the path yet")
        .expect("the pasteboard could not be written");

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
    eprintln!("row menu (appkit): items {items:?}");
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
    eprintln!("row menu (appkit): fired {lines:?}");
    assert_eq!(lines.len(), 4, "not every menu item ran its action");
    assert!(
        lines[2].starts_with("copied "),
        "Copy path ran {:?}",
        lines[2]
    );

    let read_back = pasteboard_text().expect("the pasteboard holds no text after Copy path");
    eprintln!("clipboard holds {read_back:?}");
    assert_eq!(
        read_back, path,
        "Copy path did not leave the row's path on the pasteboard"
    );
    assert!(
        std::fs::metadata(&read_back)
            .map(|m| m.is_file())
            .unwrap_or(false),
        "the copied path {read_back:?} is not a file on disk"
    );

    // The popup's own entry point must no longer refuse for lack of a feature;
    // with no view it fails on the view, which is the argument it is missing.
    let err = menu
        .popup_at_pointer(std::ptr::null_mut(), std::ptr::null_mut())
        .err()
        .map(|e| format!("{e}"));
    eprintln!("row menu (appkit): popup with no view -> {err:?}");
    if let Some(e) = &err {
        assert!(
            !e.contains("not available") && !e.contains("not wired up"),
            "the popup still reports the feature missing: {e}"
        );
    }
}
