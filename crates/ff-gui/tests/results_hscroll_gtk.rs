//! Regression test: the default results view must not need a horizontal
//! scrollbar.
//!
//! Root causes it guards:
//!
//! (a) One long path used to ratchet the view wider than the window and it
//! never shrank back. Now the Name column has a fixed 250 px default and the
//! Path column expands into the spare width (in
//! `gtk_dynamic_loader::TreeView::new`), with end-ellipsized renderers, so
//! over-long paths truncate with "…" instead of forcing the bar. (FIXED
//! sizing cannot be the mechanism: it and `resizable` are mutually exclusive
//! in GTK3 — last call wins — so FIXED columns would kill the draggable
//! dividers. Verified live: widths hold 250/719 steady through long rows,
//! short-after-long, clear and 200 long rows.)
//!
//! (b) Even empty, the pristine view sat ~38 px wider than the window: the
//! sortable headers grow a little in the first frames (sort-arrow space
//! arriving with the theme), widening their columns in place with no
//! compensating redistribution — and neither queued resizes, visibility
//! toggles, sort signals nor re-appends redistribute (only a real allocation
//! change or row arrival does). So `run()` maps the window 1 px narrow and
//! steps it out to the documented width once settled; that step *is* a real
//! allocation change and the columns snap to the viewport exactly. The step
//! is skipped when the user has resized meanwhile (their own resize already
//! redistributed).
//!
//! Needs a display with GTK3 (it reads live window size and the scroller's
//! adjustment straight from the toolkit), so on headless CI — or anywhere
//! the toolkit is missing — it passes trivially instead of failing.
//!
//! The view under test is built exactly the way `ff-gui`'s `run()` builds it
//! (same columns, same scrolled container, same map-then-settle sequence),
//! then filled with long-path rows as a first search would. What is pinned:
//! Name defaults to 250 px and the scroller's horizontal adjustment never
//! exceeds its page — empty, filled, and cleared.

// gtk_dynamic_loader only exists on unix; elsewhere there is nothing to test.
#![cfg(unix)]

use std::os::raw::c_void;

use rswidgets::prelude::*;

/// 1 px narrower than the default, exactly as `run()` maps it.
const INITIAL_W: i32 = 999;
/// The documented default, exactly as `run()` settles it.
const DEFAULT_W: i32 = 1000;

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

/// The scroller's horizontal `(upper, page)`: a horizontal scrollbar is needed
/// iff `upper > page`.
unsafe fn hadj(loader: &gtk_dynamic_loader::Loader, sw: *mut c_void) -> (f64, f64) {
    let gtk = loader.libs.get("libgtk").unwrap();
    let get_hadj = *gtk
        .get::<unsafe extern "C" fn(*mut c_void) -> *mut c_void>(
            b"gtk_scrolled_window_get_hadjustment",
        )
        .unwrap();
    let get_upper = *gtk
        .get::<unsafe extern "C" fn(*mut c_void) -> f64>(b"gtk_adjustment_get_upper")
        .unwrap();
    let get_page = *gtk
        .get::<unsafe extern "C" fn(*mut c_void) -> f64>(b"gtk_adjustment_get_page_size")
        .unwrap();
    let adj = get_hadj(sw);
    (get_upper(adj), get_page(adj))
}

fn long_paths(n: usize) -> Vec<String> {
    // Realistic worst case: deep tree + long names, like a node_modules hit.
    (0..n)
        .map(|i| {
            format!(
                "/home/me/projects/some-long-project-name/node_modules/@scope/package-{i:03}/dist/cjs/deeply/nested/output/bundle-{i:03}.min.js"
            )
        })
        .collect()
}

fn rows_for(paths: &[String]) -> Vec<Vec<CellValue>> {
    ff_gui::rows_from_paths(paths)
        .into_iter()
        .map(|r| {
            let fallback =
                if r.icon == "folder" { "folder" } else { "text-x-generic" }.to_string();
            vec![
                CellValue::Icon { name: r.icon, fallback },
                CellValue::Text(r.name),
                CellValue::Text(r.path),
            ]
        })
        .collect()
}

#[test]
fn default_results_view_needs_no_horizontal_scrollbar() {
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
    let loader: &gtk_dynamic_loader::Loader = &loader;
    // One test, sequential phases: GTK is not thread-safe, so two #[test]s in
    // one binary segfault each other (each of the other *_gtk.rs files holds
    // exactly one test for the same reason).
    main_sequence(loader);
    no_yank_sequence(loader);
}

/// The map-narrow-then-settle sequence `run()` performs, ending with the
/// scrollbar assertions on empty / filled / cleared.
fn main_sequence(loader: &gtk_dynamic_loader::Loader) {
    let Ok(app) = rswidgets::App::init() else { return };

    // Exactly what `ff-gui`'s `run()` builds: the same three columns in the
    // same scrolled container, mapped 1 px narrow.
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
    // Let the late header growth land, then run the same settle step `run()`
    // schedules: step out to the documented width unless resized meanwhile.
    pump(loader, 500);
    let (w, h) = win.get_size();
    assert_eq!((w, h), (INITIAL_W, 700), "nothing should have resized the window");
    win.resize(DEFAULT_W, h);
    pump(loader, 500);

    unsafe {
        let gtk = loader.libs.get("libgtk").unwrap();
        let get_n_cols = *gtk
            .get::<unsafe extern "C" fn(*mut c_void) -> u32>(b"gtk_tree_view_get_n_columns")
            .unwrap();
        let get_col = *gtk
            .get::<unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void>(
                b"gtk_tree_view_get_column",
            )
            .unwrap();
        let tv_ptr: *mut c_void = *results.as_ref();
        let sw_ptr: *mut c_void = *sw.as_ref();

        // Name carries the fixed 250 px default; Path expands into the
        // spare width. Widths are read below via col_width; sizing is
        // deliberately not pinned — resizable columns cannot be FIXED.
        let col_width = *gtk
            .get::<unsafe extern "C" fn(*mut c_void) -> i32>(b"gtk_tree_view_column_get_width")
            .unwrap();
        assert_eq!(get_n_cols(tv_ptr), 3, "Icon / Name / Path");
        assert_eq!(
            col_width(get_col(tv_ptr, 1)),
            250,
            "Name must default to the roomy 250 px"
        );

        let fits = |tag: &str| {
            let (upper, page) = hadj(loader, sw_ptr);
            assert!(
                upper <= page,
                "{tag}: no horizontal scrollbar allowed (upper={upper} page={page})"
            );
        };

        // The default screen: pristine and empty, at the documented size.
        assert_eq!(win.get_size().0, DEFAULT_W);
        fits("empty");
        // A first search landing long-path rows.
        results.set_rows(&rows_for(&long_paths(200)));
        pump(loader, 500);
        fits("filled with long paths");
        // Clearing must not bring the scrollbar back either.
        results.set_rows(&[]);
        pump(loader, 300);
        fits("cleared");
    }
    // Keep the window alive until the reads are done: dropping it first would
    // free the widgets out from under the pointers.
    let _ = win;
}

/// The user resizes before the settle step fires; the step must stand down.
fn no_yank_sequence(loader: &gtk_dynamic_loader::Loader) {
    let Ok(app) = rswidgets::App::init() else { return };

    let win = app.create_window().unwrap();
    win.set_default_size(INITIAL_W, 700);
    win.present();
    pump(loader, 300);
    // The user resizes before the settle step fires ...
    win.resize(1200, 700);
    pump(loader, 300);
    // ... so the step must stand down: the width stays theirs.
    let (w, _) = win.get_size();
    if w == INITIAL_W {
        win.resize(DEFAULT_W, 700);
    }
    pump(loader, 300);
    let (w, _) = win.get_size();
    assert_eq!(w, 1200, "the settle step must not yank a user resize");
    let _ = win;
}
