//! Regression test: right-clicking a search result must pop up a context menu
//! whose items are actually shown.
//!
//! Root cause it guards: GTK3 widgets start hidden, and `ContextMenu` used to
//! never show its items — so the menu popped up empty and a right-click
//! appeared to do nothing at all, even though the handler ran and the row
//! lookup hit the right path.
//!
//! Needs a display with GTK3 (`GtkMenu` only exists there), so on headless CI
//! — or anywhere the toolkit is missing — it passes trivially instead of
//! failing.
//!
//! The menu under test is built from [`ff_gui::RowAction::all`], the same list
//! the GUI's right-click handler iterates, so the test also pins the menu
//! contents (Copy full path, Open, Open containing folder, Open with…, …).

// gtk_dynamic_loader only exists on unix; elsewhere there is nothing to test.
#![cfg(unix)]

use std::os::raw::c_void;

use ff_gui::RowAction;

#[repr(C)]
struct GList {
    data: *mut c_void,
    next: *mut GList,
    _prev: *mut c_void,
}

#[test]
fn right_click_menu_items_are_shown() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => return,
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        return;
    }
    let mut menu = match gtk_dynamic_loader::ContextMenu::new(loader.clone()) {
        Ok(m) => m,
        Err(_) => return,
    };
    // Exactly what `ff-gui`'s right-click handler appends per row.
    let expected = RowAction::all();
    for action in expected {
        menu.append_item(action.label(), Box::new(|| {}));
    }

    let gtk = match loader.libs.get("libgtk") {
        Some(l) => l,
        None => return,
    };
    let get_children = unsafe {
        gtk.get::<unsafe extern "C" fn(*mut c_void) -> *mut GList>(b"gtk_container_get_children")
    };
    let get_visible =
        unsafe { gtk.get::<unsafe extern "C" fn(*mut c_void) -> i32>(b"gtk_widget_get_visible") };
    let (Ok(get_children), Ok(get_visible)) = (get_children, get_visible) else {
        return;
    };
    let free_list = loader
        .libs
        .get("libglib")
        .and_then(|g| unsafe {
            g.get::<unsafe extern "C" fn(*mut c_void)>(b"g_list_free")
                .ok()
        })
        .map(|s| *s);

    let mut total = 0u32;
    let mut visible = 0u32;
    unsafe {
        let head = get_children(menu.ptr());
        let mut node = head;
        while !node.is_null() {
            total += 1;
            if get_visible((*node).data) != 0 {
                visible += 1;
            }
            node = (*node).next;
        }
        if let Some(free) = free_list {
            free(head as *mut c_void);
        }
    }
    assert_eq!(
        total,
        expected.len() as u32,
        "the menu must hold every row action"
    );
    assert_eq!(
        visible, total,
        "every menu item must be shown, or the popup looks empty"
    );
}
