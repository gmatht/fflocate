//! The parts of the macOS GUI that `cargo test` can reach.
//!
//! AppKit needs the process's main thread and a window-server session, so the
//! widget-level wiring lives in `examples/macos_probe.rs` (run by CI as a
//! binary). What is left for a normal test is the app-owned data the AppKit
//! widgets are built from: the menu model and its action names. A duplicated
//! or empty action name there would leave a menu item that either does nothing
//! or fires another item's handler, and neither shows up as a compile error.
#![cfg(target_os = "macos")]

use std::collections::HashSet;

/// Every menu item names an action, every action name is unique, and the
/// conventional menus are present.
#[test]
fn the_menu_model_has_unique_actions_and_the_usual_menus() {
    let model = ff_gui::menubar_model();
    let menus: Vec<&str> = model.iter().map(|(label, _)| *label).collect();
    for expected in ["File", "Search", "View", "Help"] {
        assert!(
            menus.contains(&expected),
            "the menu bar has no {expected} menu: {menus:?}"
        );
    }
    let mut seen = HashSet::new();
    let mut items = 0usize;
    for (menu, entries) in &model {
        assert!(!entries.is_empty(), "the {menu} menu has no items");
        for (label, action) in entries {
            assert!(!label.trim().is_empty(), "the {menu} menu has an unlabelled item");
            let name = action.action_name();
            assert!(!name.trim().is_empty(), "{label} names no action");
            assert!(
                seen.insert(name),
                "two menu items share the action name {name:?}"
            );
            items += 1;
        }
    }
    assert!(items >= 10, "the menu model shrank to {items} items");
}

/// The row actions a right-click offers are the ones the menu lists: a label
/// without an action (or the reverse) is a menu entry that cannot work.
#[test]
fn every_row_action_has_a_label() {
    for action in ff_gui::RowAction::all() {
        assert!(
            !action.label().trim().is_empty(),
            "{action:?} has no menu label"
        );
    }
    assert_eq!(
        ff_gui::RowAction::all().len(),
        6,
        "the right-click menu changed size; check the probe's menu checks too"
    );
}
