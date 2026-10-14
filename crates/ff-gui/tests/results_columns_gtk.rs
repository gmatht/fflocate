//! Regression test: the results columns must be user-resizable — dragging a
//! header separator widens Name with real pointer events — and Name must
//! default to a roomy width.
//!
//! Root causes it guards:
//!
//! (a) `TreeView::new` set every column resizable *before* the sizing mode,
//! and the sizing call cleared the flag (columns read back resizable=0, so
//! the separators never dragged). The flag is now applied last and reads 1.
//!
//! (b) The Name column defaulted to 74 px of a 1000 px window (Path took
//! 895), so filenames ellipsized to stubs. Name now defaults to 250 px with
//! Path absorbing the rest.
//!
//! Needs a display with GTK3 plus `xdotool` (real pointer events — the drag
//! path is a server-side pointer grab that synthesized GDK events cannot
//! drive), so on headless CI — or anywhere the toolkit is missing — it
//! passes trivially instead of failing.
//!
//! The view under test is built exactly the way `ff-gui`'s `run()` builds it
//! (same columns, same scrolled container, same map-then-settle sequence).
//! What is pinned: every column reads resizable=1, Name defaults to 250 px,
//! and an `xdotool` drag of the Name|Path separator widens Name by the
//! dragged distance. Path keeps its width (its `expand` backfills, so the
//! total grows and a scrollbar appears) — that is GTK-native behavior, and
//! the honest contract: the boundary moves, Name shows more, nothing is
//! squeezed out of reach.

// gtk_dynamic_loader only exists on unix; elsewhere there is nothing to test.
#![cfg(unix)]

use std::os::raw::c_void;

use rswidgets::prelude::*;

/// 1 px narrower than the default, exactly as `run()` maps it.
const INITIAL_W: i32 = 999;
/// The documented default, exactly as `run()` settles it.
const DEFAULT_W: i32 = 1000;
/// The roomy Name default `TreeView::new` applies.
const NAME_DEFAULT_W: i32 = 250;

/// Run pending main-context iterations, with real sleeps between them so
/// theme settling and layout have wall-clock time to land.
fn pump(loader: &gtk_dynamic_loader::Loader, ms: u64) {
    let Some(gtk) = loader.libs.get("libgtk") else { return };
    unsafe {
        let it = gtk.get::<unsafe extern "C" fn()>(b"gtk_main_iteration_do");
        if let Ok(f) = it {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
            while std::time::Instant::now() < end {
                f();
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}

fn has_xdotool() -> bool {
    std::process::Command::new("xdotool")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Screen position of the `fflocate` window via the same server `xdotool`
/// drives (robust against WM offsets, unlike widget-origin reads which come
/// back (0,0) on an unrealized path).
fn window_position() -> Option<(i32, i32)> {
    let out = std::process::Command::new("xdotool")
        .args(["search", "--name", "fflocate", "getwindowgeometry", "--shell"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut x = None;
    let mut y = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("X=") {
            x = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("Y=") {
            y = v.trim().parse().ok();
        }
    }
    Some((x?, y?))
}

#[test]
fn results_columns_are_drag_resizable_with_a_roomy_name_default() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return;
    }
    if !has_xdotool() {
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => return,
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        return;
    }
    let Ok(app) = rswidgets::App::init() else { return };

    // Exactly what `ff-gui`'s `run()` builds: the same three columns in the
    // same scrolled container, mapped 1 px narrow, then settled out.
    let win = app.create_window().unwrap();
    win.set_title("fflocate");
    win.set_default_size(INITIAL_W, 700);
    let results =
        app.create_treeview(&[("", true, true), ("Name", false, true), ("Path", false, true)])
            .unwrap();
    let sw = results.into_scrolled().unwrap();
    sw.set_vexpand(true);
    sw.set_hexpand(true);
    let layout = app.create_box(Orientation::Vertical, 1).unwrap();
    layout.set_vexpand(true);
    layout.set_hexpand(true);
    layout.append(&sw);
    win.set_child(&layout);
    win.present();
    pump(&loader, 500);
    win.resize(DEFAULT_W, 700);
    pump(&loader, 500);

    unsafe {
        let gtk = loader.libs.get("libgtk").unwrap();
        let get_col = *gtk
            .get::<unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void>(
                b"gtk_tree_view_get_column",
            )
            .unwrap();
        let col_width = *gtk
            .get::<unsafe extern "C" fn(*mut c_void) -> i32>(b"gtk_tree_view_column_get_width")
            .unwrap();
        let col_resizable = *gtk
            .get::<unsafe extern "C" fn(*mut c_void) -> i32>(b"gtk_tree_view_column_get_resizable")
            .unwrap();
        let tv_ptr: *mut c_void = *results.as_ref();

        // (a) Every column is user-resizable — the separators drag.
        for i in 0..3 {
            assert_eq!(
                col_resizable(get_col(tv_ptr, i)),
                1,
                "column {i} must be user-resizable"
            );
        }
        // (b) Name defaults to the roomy width, not the 74 px stub.
        let name_w = col_width(get_col(tv_ptr, 1));
        assert_eq!(
            name_w, NAME_DEFAULT_W,
            "Name must default to {NAME_DEFAULT_W} px, got {name_w}"
        );

        // The drag: grab the Name|Path header separator and pull it right
        // 120 px in small steps (one jump can outrun the grab on a loaded
        // Xvfb; steps track like a real pointer). The separator sits at the
        // right edge of the Name column in the header row; the window's
        // screen position plus the settled column widths locate it.
        let Some((ox, oy)) = window_position() else {
            return;
        };
        let icon_w = col_width(get_col(tv_ptr, 0));
        let sep_x = ox + icon_w + name_w;
        // y just inside the header (header height is ~25 px on this theme;
        // 10 px down is on the separator's grab zone, not a sort click).
        let sep_y = oy + 10;
        std::process::Command::new("xdotool")
            .args(["mousemove", &sep_x.to_string(), &sep_y.to_string(), "mousedown", "1"])
            .status()
            .ok();
        pump(&loader, 200);
        for x in (1..=6).map(|k| sep_x + k * 20) {
            std::process::Command::new("xdotool")
                .args(["mousemove", &x.to_string(), &sep_y.to_string()])
                .status()
                .ok();
            pump(&loader, 120);
        }
        std::process::Command::new("xdotool")
            .args(["mouseup", "1"])
            .status()
            .ok();
        pump(&loader, 500);

        // The boundary moved: Name widened by (about) the dragged distance.
        // A few px of slack for grip snap and server rounding — the point is
        // real pixels moved under a real pointer grab, not a flag that reads
        // 1 while nothing drags.
        let name_after = col_width(get_col(tv_ptr, 1));
        assert!(
            name_after >= name_w + 100,
            "dragging the separator right 120 px must widen Name ({name_w} -> {name_after})"
        );
    }
    // Keep the window alive until the reads are done: dropping it first would
    // free the widgets out from under the pointers.
    let _ = win;
}
