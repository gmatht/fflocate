//! The row context menu on GTK: the four actions, each one running, and Copy
//! path leaving the row's path readable on the clipboard.
//!
//! Builds the menu the popup would show (`ContextMenu::build` is the popup's
//! first step) and fires each item through GTK's own `activate` signal, which
//! is exactly what a click in the menu emits. Needs a display with GTK3, so on
//! a headless run it returns early; CI runs it under Xvfb.

#![cfg(unix)]

use ff_gui::RowAction;

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
    let Ok(_app) = rswidgets::App::init() else {
        eprintln!("row menu: GTK did not initialise, skipping");
        return;
    };

    let path = "/tmp/ff-gui-row-menu.txt".to_string();
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

    // The clipboard is the observable half of a copy: ask it what it holds.
    let read_back = rswidgets::compat::clipboard_get_text()
        .expect("the clipboard holds no text after Copy path");
    eprintln!("clipboard holds {read_back:?}");
    assert_eq!(
        read_back, path,
        "Copy path did not leave the row's path on the clipboard"
    );

    // The popup itself is asynchronous on GTK (the choice arrives on the main
    // loop), but the path must no longer report the feature missing: with no
    // widget it fails on the widget.
    let err = menu
        .popup_at_pointer(std::ptr::null_mut(), std::ptr::null_mut())
        .err()
        .map(|e| format!("{e}"));
    eprintln!("row menu: popup with no widget -> {err:?}");
    if let Some(e) = &err {
        assert!(
            !e.contains("not available") && !e.contains("not wired up"),
            "the popup still reports the feature missing: {e}"
        );
    }
}
