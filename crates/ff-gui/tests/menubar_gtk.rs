//! Regression test: the menubar's items must be *enabled*, not greyed out.
//!
//! Root causes it guards (both had to be fixed before every leaf read
//! sensitive on every display):
//!
//! (a) `main.rs` registered every `GSimpleAction` *before* the action group
//! existed — `register_action` is a silent no-op until `ensure_action_group`
//! has run, so no action was ever registered — and never inserted the group
//! under the name "app" anywhere in the widget hierarchy, so no item's
//! action could resolve. GTK renders an unresolvable item insensitive.
//!
//! (b) The group itself was a `GtkApplication`, which resolves `app.<name>`
//! items only on some displays (greyed out on WSLg's `:0`, fine under Xvfb).
//! `ensure_action_group` now hands out a plain `GSimpleActionGroup`, which
//! resolves identically everywhere.
//!
//! Needs a display with GTK3 (real `GtkMenuItem` widgets only exist there),
//! so on headless CI — or anywhere the toolkit is missing — it passes
//! trivially instead of failing.
//!
//! The bar under test is built exactly the way `ff-gui`'s `run()` builds it
//! (same API calls, same order): group first, then one registered action per
//! [`ff_gui::MenuAction`] referenced as `app.<name>`, then the bar, then the
//! `"app"` insertion on the window and the bar itself. Two things are pinned:
//! every leaf is sensitive (not greyed out), and activating one fires the
//! action it names (the wiring is live, not just painted enabled).

// gtk_dynamic_loader only exists on unix; elsewhere there is nothing to test.
#![cfg(unix)]

use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ff_gui::{menubar_model, MenuAction};

#[repr(C)]
struct GList {
    data: *mut c_void,
    next: *mut GList,
    _prev: *mut c_void,
}

/// Pump the main context so `present()`ed windows map before we read them.
fn pump(loader: &gtk_dynamic_loader::Loader) {
    let Some(gtk) = loader.libs.get("libgtk") else {
        return;
    };
    unsafe {
        // `Library::get` infers the symbol type; no direct libloading import
        // needed (same shape as `tests/context_menu_gtk.rs`).
        let it = gtk.get::<unsafe extern "C" fn()>(b"gtk_main_iteration_do");
        if let Ok(f) = it {
            for _ in 0..100 {
                f();
            }
        }
    }
}

#[test]
fn menubar_items_are_enabled_and_fire() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return;
    }
    // rswidgets drives the dlopen loader internally; this test reaches the raw
    // toolkit through the re-export, so the bar is built on the same loader
    // the app uses.
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => return,
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        return;
    }
    let Ok(app) = rswidgets::App::init() else {
        return;
    };

    let fired = Arc::new(AtomicBool::new(false));

    // Exactly what `ff-gui`'s `run()` does, in the same order: the group FIRST
    // (registering before this is a silent no-op), one registered action per
    // menu action, then the bar, then the "app" insertion on window and bar.
    let group = match app.ensure_action_group() {
        Ok(g) => g,
        Err(_) => return,
    };
    let mut model = match app.new_menu() {
        Ok(m) => m,
        Err(_) => return,
    };
    let mut expected_leaves = 0u32;
    for (menu_label, items) in menubar_model() {
        let mut sub = match app.new_menu() {
            Ok(m) => m,
            Err(_) => return,
        };
        for (label, action) in items {
            let act = match app.new_simple_action(action.action_name()) {
                Ok(a) => a,
                Err(_) => return,
            };
            let f = fired.clone();
            let name: &'static str = action.action_name();
            if act
                .connect_activate(move |_| {
                    if name == MenuAction::Quit.action_name() {
                        f.store(true, Ordering::SeqCst);
                    }
                })
                .is_err()
            {
                return;
            }
            if app.register_action(&act).is_err() {
                return;
            }
            sub.append(label, &format!("app.{}", action.action_name()));
            expected_leaves += 1;
        }
        model.append_submenu(menu_label, &sub);
    }
    // SAFETY: `group` is the live GActionGroup pointer `ensure_action_group`
    // just returned, valid for the life of the app.
    // The window must stay alive for the whole read: the "app" insertion
    // lives on it, and dropping it takes the resolution (hence the
    // sensitivity) with it.
    let (_win, bar) = unsafe {
        let bar = match app.new_menubar(&model, group) {
            Ok(b) => b,
            Err(_) => return,
        };
        let win = match app.create_window() {
            Ok(w) => w,
            Err(_) => return,
        };
        win.insert_action_group("app", group);
        bar.insert_action_group("app", group);
        let layout = match app.create_box(rswidgets::prelude::Orientation::Vertical, 1) {
            Ok(l) => l,
            Err(_) => return,
        };
        layout.append(&bar);
        win.set_child(&layout);
        win.present();
        (win, bar)
    };
    pump(&loader);

    let gtk = match loader.libs.get("libgtk") {
        Some(l) => l,
        None => return,
    };
    let get_children = unsafe {
        gtk.get::<unsafe extern "C" fn(*mut c_void) -> *mut GList>(b"gtk_container_get_children")
    };
    let get_submenu = unsafe {
        gtk.get::<unsafe extern "C" fn(*mut c_void) -> *mut c_void>(b"gtk_menu_item_get_submenu")
    };
    let is_sensitive =
        unsafe { gtk.get::<unsafe extern "C" fn(*mut c_void) -> i32>(b"gtk_widget_is_sensitive") };
    let activate_item =
        unsafe { gtk.get::<unsafe extern "C" fn(*mut c_void)>(b"gtk_menu_item_activate") };
    let (Ok(get_children), Ok(get_submenu), Ok(is_sensitive), Ok(activate_item)) =
        (get_children, get_submenu, is_sensitive, activate_item)
    else {
        return;
    };

    // Collect the leaf items (the real GtkMenuItems under each submenu),
    // grouped per top-level menu so the Quit leaf can be found by position.
    let mut menus: Vec<Vec<*mut c_void>> = Vec::new();
    unsafe {
        let bar_ptr: *mut c_void = *bar.as_ref();
        let mut top = get_children(bar_ptr);
        while !top.is_null() {
            let sub = get_submenu((*top).data);
            if !sub.is_null() {
                let mut leaves = Vec::new();
                let mut m = get_children(sub);
                while !m.is_null() {
                    leaves.push((*m).data);
                    m = (*m).next;
                }
                menus.push(leaves);
            }
            top = (*top).next;
        }
    }
    let model = menubar_model();
    assert_eq!(menus.len(), model.len(), "one submenu per top-level menu");
    let leaves: Vec<*mut c_void> = menus.iter().flatten().copied().collect();
    assert_eq!(
        leaves.len() as u32,
        expected_leaves,
        "the bar must hold one leaf per menu action"
    );
    assert!(
        !leaves.is_empty(),
        "the model must produce at least one leaf"
    );

    // Every leaf must be sensitive: an unresolvable `app.<name>` renders
    // greyed out, which is the bug this guards.
    let mut grey = 0u32;
    for leaf in &leaves {
        if unsafe { is_sensitive(*leaf) } == 0 {
            grey += 1;
        }
    }
    assert_eq!(
        grey,
        0,
        "every menu item must be enabled, {grey} of {} are greyed out",
        leaves.len()
    );

    // And the wiring must be live: activating the Quit leaf fires its action.
    // Quit is not leaves[0] (File's first item is Open Containing Folder) —
    // locate it by position within the File submenu, straight from the model.
    let quit_idx = model[0]
        .1
        .iter()
        .position(|(_, a)| *a == MenuAction::Quit)
        .expect("Quit is on the File menu");
    assert_eq!(
        menus[0].len(),
        model[0].1.len(),
        "the File submenu must hold every File item"
    );
    unsafe { activate_item(menus[0][quit_idx]) };
    pump(&loader);
    assert!(
        fired.load(Ordering::SeqCst),
        "activating the Quit item must fire its registered action"
    );
}
