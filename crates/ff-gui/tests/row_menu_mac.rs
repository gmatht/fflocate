//! The row context menu and Copy path on macOS, in the half `cargo test` can
//! reach without a window server.
//!
//! A `cargo test` binary has no AppKit: nothing in it initialises the backend,
//! so `objc_getClass("NSMenu")` and `objc_getClass("NSPasteboard")` both return
//! nil and the NSMenu cannot even be built. The AppKit half — the menu's items
//! and their enabled state read back out of the `NSMenu`, each item fired
//! through `performActionForItemAtIndex:`, the menu popped up at the pointer, and
//! the pasteboard read back by a second AppKit call — therefore runs in
//! `examples/macos_probe.rs`, which CI executes as a binary on the main thread
//! with a real window. This file covers what is testable here: the menu model
//! the popup is built from, its dispatch, and the exact text Copy path produces
//! for a real file.

#![cfg(target_os = "macos")]

use ff_gui::RowAction;
use rswidgets::compat::ContextMenu;

/// The four actions a right-click offers, in menu order.
fn row_actions() -> [RowAction; 4] {
    [
        RowAction::Open,
        RowAction::OpenContainingFolder,
        RowAction::CopyPath,
        RowAction::OpenInTerminal,
    ]
}

#[test]
fn the_row_menu_model_dispatches_and_copy_path_copies_the_real_path() {
    let dir = tempfile::tempdir().expect("no temp dir");
    let file = dir.path().join("ff-gui-row-menu.txt");
    std::fs::write(&file, "row menu\n").expect("could not write the row's file");
    let path = file.to_string_lossy().into_owned();

    let ran: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let copied: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let mut menu = ContextMenu::new();
    for action in row_actions() {
        let ran = ran.clone();
        let copied = copied.clone();
        let path = path.clone();
        menu.append_item(
            action.label(),
            Box::new(move || {
                // The copy is captured instead of reaching the pasteboard: this
                // process has no AppKit, and the probe checks the pasteboard
                // itself.
                let line = ff_gui::run_row_action(action, &path, |text| {
                    copied.borrow_mut().push(text.to_string());
                    Ok(())
                });
                ran.borrow_mut().push(line);
            }),
        );
    }

    let labels = menu.labels();
    eprintln!("row menu (appkit model): {labels:?}");
    assert_eq!(
        labels,
        row_actions()
            .iter()
            .map(|a| a.label().to_string())
            .collect::<Vec<_>>(),
        "the row menu labels are not the four row actions"
    );

    for action in row_actions() {
        assert!(
            menu.activate(action.label()),
            "{} could not be dispatched",
            action.label()
        );
    }
    let lines = ran.borrow().clone();
    eprintln!("row menu (appkit model): fired {lines:?}");
    assert_eq!(lines.len(), 4, "not every menu item ran its action");
    for (line, action) in lines.iter().zip(row_actions().iter()) {
        assert!(
            line.starts_with(action.label()),
            "choosing {:?} ran {line:?}",
            action.label()
        );
    }

    let copied = copied.borrow().clone();
    assert_eq!(
        copied.len(),
        1,
        "only Copy path should put text on the clipboard: {copied:?}"
    );
    assert_eq!(
        copied[0], path,
        "Copy path did not copy the row's absolute path"
    );
    assert!(
        std::fs::metadata(&copied[0])
            .map(|m| m.is_file())
            .unwrap_or(false),
        "the copied path {:?} is not a file on disk",
        copied[0]
    );
    assert!(
        lines[2].starts_with("copied "),
        "Copy path reported {:?}",
        lines[2]
    );
}
