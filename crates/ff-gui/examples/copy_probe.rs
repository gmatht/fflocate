//! "Copy path" from the row menu, end to end, with the real system clipboard.
//!
//! This mirrors `main.rs`: a window, the results widget's clipboard hook
//! (`compat::clipboard_set_text` → the GTK clipboard), a menu whose items run
//! `ff_gui::run_row_action`, and the item fired through GTK's own `activate`
//! signal — the signal a click in the menu emits.
//!
//! It then runs GTK's main loop, as the app does, because the clipboard
//! selection is served by the process that owns it: another program (Klipper,
//! `wl-paste`, an editor) can only read the text while this process is alive
//! *and* pumping its loop.
//!
//! ```text
//! cargo run -p ff-gui --example copy_probe -- /tmp/row/alpha.txt [seconds]
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use ff_gui::RowAction;
// `raw_handle` comes from the widget trait, as it does in the app.
use rswidgets::core::Widget as _;

/// The GTK clipboard route this probe demonstrates. On a build without that
/// backend (macOS's AppKit pasteboard, the `zork` fallback, an Android target)
/// there is nothing to drive, so the example compiles to a note instead of
/// failing a cross-check or the `--no-default-features --features zork` build.
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
fn main() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("copy probe: no display");
        std::process::exit(2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = match args.first() {
        Some(p) => p.clone(),
        None => {
            eprintln!("usage: copy_probe <path> [seconds]");
            std::process::exit(2);
        }
    };
    let hold: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(15);

    let app = match rswidgets::App::init() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("copy probe: GTK did not initialise: {e}");
            std::process::exit(2);
        }
    };

    // The app always has a window; the clipboard needs a realized display
    // connection, and on a Wayland session the data source is tied to it.
    let window = match app.create_window() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("copy probe: no window: {e}");
            std::process::exit(2);
        }
    };
    window.set_title("ff-gui copy probe");

    // The row menu the app builds: the shared plan for this path, each item
    // running its action through the shared action runner. The probe offers
    // every action, so its filesystem probe is `|_| true`.
    let (target, plan) = ff_gui::row_menu_plan(3, Some(0), std::slice::from_ref(&path), |_| true)
        .expect("a right-click on a row produced no menu");
    let status: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let mut menu = rswidgets::compat::ContextMenu::new();
    for item in &plan {
        let status = status.clone();
        let target = target.clone();
        let action = item.action;
        menu.append_item_enabled(
            item.label,
            item.enabled,
            Box::new(move || {
                let line = ff_gui::run_row_action(action, &target, |text| {
                    rswidgets::compat::clipboard_set_text(text).map_err(|e| e.to_string())
                });
                *status.borrow_mut() = Some(line);
            }),
        );
    }
    let handle = menu.build().expect("the row menu could not be built");
    let labels: Vec<String> = rswidgets::compat::ContextMenu::items_of(handle)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    let copy_index = labels
        .iter()
        .position(|l| l == RowAction::CopyPath.label())
        .expect("the menu has no Copy item");
    println!("copy probe: menu items {labels:?}, firing {:?}", labels[copy_index]);

    // Show the window and give the toolkit a turn before the menu item runs, so
    // the surface and the display's data device exist by the time Copy writes.
    let loader = rswidgets::backends::gtk::loader().expect("no GTK loader");
    if let Some(show) = loader.symbols.gtk_widget_show_all {
        // SAFETY: the pointer is the window the compat layer just created.
        unsafe { show(window.raw_handle()) };
    }
    if let Some(present) = loader.symbols.gtk_window_present {
        // SAFETY: as above; the argument is that window.
        unsafe { present(window.raw_handle()) };
    }

    if !rswidgets::compat::ContextMenu::fire(handle, copy_index) {
        eprintln!("copy probe: the Copy item could not be fired");
        std::process::exit(1);
    }
    let line = status.borrow().clone().unwrap_or_default();
    println!("copy probe: status line {line:?}");

    // Serve the selection: run the loop the app runs, and leave it after the
    // hold period. `exit` is deliberate — this probe has nothing to shut down.
    let hold_ms = hold * 1000;
    if let Some(timeout) = loader.symbols.g_timeout_add {
        // SAFETY: the callback is `int (*)(void*)` as GLib defines it, and it
        // never touches anything but the process exit.
        unsafe extern "C" fn quit(_data: *mut std::ffi::c_void) -> i32 {
            println!("copy probe: done");
            std::process::exit(0);
        }
        unsafe {
            timeout(hold_ms, Some(quit), std::ptr::null_mut());
        }
    }
    println!("copy probe: running the main loop for {hold}s");
    // A failed run loop must not look like a clean probe run.
    app.run().expect("copy probe: the main loop failed");
}

#[cfg(not(all(feature = "gtk", target_os = "linux", not(feature = "zork"))))]
fn main() {
    eprintln!("copy probe: needs the GTK clipboard backend");
    std::process::exit(2);
}
