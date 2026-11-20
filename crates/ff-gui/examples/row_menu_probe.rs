//! Pop the app's row menu at the pointer and leave it on screen.
//!
//! `main.rs` builds this menu from `RowAction::all()` and shows it with
//! `ContextMenu::popup_at_pointer`; this does the same from a one-shot process,
//! so the popup can be photographed from outside. It holds for a few seconds
//! and then exits, taking the menu with it.
//!
//! ```text
//! cargo run -p ff-gui --example row_menu_probe -- [seconds]
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use ff_gui::RowAction;
// `raw_handle` comes from the widget trait, as it does in the app.
use rswidgets::core::Widget as _;

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
fn main() {
    let hold: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("row menu probe: no display");
        std::process::exit(2);
    }
    let app = match rswidgets::App::init() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("row menu probe: GTK did not initialise: {e}");
            std::process::exit(2);
        }
    };
    let window = match app.create_window() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("row menu probe: no window: {e}");
            std::process::exit(2);
        }
    };
    window.set_title("ff-gui row menu probe");

    let loader = rswidgets::backends::gtk::loader().expect("no GTK loader");
    if let Some(show) = loader.symbols.gtk_widget_show_all {
        // SAFETY: the pointer is the window the compat layer just created.
        unsafe { show(window.raw_handle()) };
    }
    if let Some(present) = loader.symbols.gtk_window_present {
        // SAFETY: as above.
        unsafe { present(window.raw_handle()) };
    }

    // The app's menu: one item per row action, each running the action.
    let fired: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let mut menu = rswidgets::compat::ContextMenu::new();
    for action in RowAction::all() {
        let fired = fired.clone();
        menu.append_item(
            action.label(),
            Box::new(move || {
                let line = ff_gui::run_row_action(action, "/tmp/row-menu-probe.txt", |text| {
                    rswidgets::compat::clipboard_set_text(text).map_err(|e| e.to_string())
                });
                *fired.borrow_mut() = Some(line);
            }),
        );
    }
    let handle = menu.build().expect("the row menu could not be built");
    let items = rswidgets::compat::ContextMenu::items_of(handle);
    println!("row menu probe: items {items:?}");

    match menu.popup_at_pointer(window.raw_handle(), std::ptr::null_mut()) {
        Ok(()) => println!("row menu probe: popped up at the pointer, holding {hold}s"),
        Err(e) => {
            eprintln!("row menu probe: the menu did not pop up: {e}");
            std::process::exit(1);
        }
    }

    let hold_ms = hold * 1000;
    if let Some(timeout) = loader.symbols.g_timeout_add {
        // SAFETY: `int (*)(void*)`, GLib's timeout ABI; the body only exits.
        unsafe extern "C" fn quit(_data: *mut std::ffi::c_void) -> i32 {
            println!("row menu probe: done");
            std::process::exit(0);
        }
        unsafe {
            timeout(hold_ms, Some(quit), std::ptr::null_mut());
        }
    }
    app.run();
}

#[cfg(not(all(feature = "gtk", target_os = "linux", not(feature = "zork"))))]
fn main() {
    eprintln!("row menu probe: needs the GTK backend");
    std::process::exit(2);
}
