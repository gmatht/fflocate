//! Daemon results -> rendered rows -> captured pixels, on GTK.
//!
//! The Linux counterpart of `daemon_pixels_mac.rs`. GTK has no
//! `write_view_png`, so the capture here goes through GTK's own drawing: the
//! widget tree is rendered by `gtk_widget_draw` into a Cairo image surface, the
//! exact call GTK makes into a widget's `draw` handler (the canvas wraps that
//! `cairo_t` in `GtkDrawContext`). The pixels are then read back out of that
//! surface. Nothing is stubbed: the rows come from a real `fflocated` child
//! process, and the assertion is about what Cairo rasterized.
//!
//! Needs a display, like `tests/menubar_gtk.rs`, so it prints a skip line and
//! exits 0 when there is none; CI runs it under Xvfb.
//!
//! `harness = false`, like its AppKit sibling: the test binary has its own
//! `main`, so the capture runs once on a single GTK thread.

use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rswidgets::compat::CellValue;
// `Window::raw_handle` comes from the widget trait, as it does in the app.
use rswidgets::core::Widget as _;
use rswidgets::prelude::App;

/// The daemon binary this build produced, if there is one.
fn daemon_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FFLOCATED_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let name = format!("fflocated{}", std::env::consts::EXE_SUFFIX);
    let mut best: Option<PathBuf> = None;
    for dir in exe.ancestors().skip(1).take(8) {
        let candidate = dir.join(&name);
        let depth = |p: &PathBuf| p.components().count();
        if candidate.is_file() && best.as_ref().map_or(true, |b| depth(&candidate) < depth(b)) {
            best = Some(candidate);
        }
    }
    best
}

/// Cairo and GTK's synchronous widget draw, loaded directly: the app's GTK
/// build has no capture call, and these are the primitives such a call needs.
struct Gtk {
    _gtk: libloading::Library,
    _cairo: libloading::Library,
    widget_draw: unsafe extern "C" fn(*mut c_void, *mut c_void),
    main_iteration: unsafe extern "C" fn(i32) -> i32,
    surface_create: unsafe extern "C" fn(i32, i32, i32) -> *mut c_void,
    create: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    set_source_rgb: unsafe extern "C" fn(*mut c_void, f64, f64, f64),
    paint: unsafe extern "C" fn(*mut c_void),
    flush: unsafe extern "C" fn(*mut c_void),
    get_data: unsafe extern "C" fn(*mut c_void) -> *mut u8,
    get_stride: unsafe extern "C" fn(*mut c_void) -> i32,
    destroy: unsafe extern "C" fn(*mut c_void),
    surface_destroy: unsafe extern "C" fn(*mut c_void),
}

impl Gtk {
    fn load() -> Option<Gtk> {
        unsafe {
            let gtk = libloading::Library::new("libgtk-3.so.0").ok()?;
            let cairo = libloading::Library::new("libcairo.so.2").ok()?;
            Some(Gtk {
                widget_draw: *gtk.get(b"gtk_widget_draw\0").ok()?,
                main_iteration: *gtk.get(b"gtk_main_iteration_do\0").ok()?,
                surface_create: *cairo.get(b"cairo_image_surface_create\0").ok()?,
                create: *cairo.get(b"cairo_create\0").ok()?,
                set_source_rgb: *cairo.get(b"cairo_set_source_rgb\0").ok()?,
                paint: *cairo.get(b"cairo_paint\0").ok()?,
                flush: *cairo.get(b"cairo_surface_flush\0").ok()?,
                get_data: *cairo.get(b"cairo_image_surface_get_data\0").ok()?,
                get_stride: *cairo.get(b"cairo_image_surface_get_stride\0").ok()?,
                destroy: *cairo.get(b"cairo_destroy\0").ok()?,
                surface_destroy: *cairo.get(b"cairo_surface_destroy\0").ok()?,
                _gtk: gtk,
                _cairo: cairo,
            })
        }
    }

    /// Render `widget` into a `width`x`height` Cairo image surface and return
    /// the pixels as ones-complement ink per row band: ARGB32, top row first.
    fn capture(&self, widget: *mut c_void, width: i32, height: i32) -> Option<Vec<u8>> {
        unsafe {
            let surface = (self.surface_create)(0, width, height); // CAIRO_FORMAT_ARGB32
            if surface.is_null() {
                return None;
            }
            let cr = (self.create)(surface);
            if cr.is_null() {
                (self.surface_destroy)(surface);
                return None;
            }
            // A white page first, like the window's own background, so "no
            // ink" is distinguishable from "nothing was drawn".
            (self.set_source_rgb)(cr, 1.0, 1.0, 1.0);
            (self.paint)(cr);
            (self.widget_draw)(widget, cr);
            (self.flush)(surface);
            let data = (self.get_data)(surface);
            let stride = (self.get_stride)(surface) as usize;
            let mut out = Vec::with_capacity(stride * height as usize);
            if !data.is_null() && stride > 0 {
                out.extend_from_slice(std::slice::from_raw_parts(data, stride * height as usize));
            }
            (self.destroy)(cr);
            (self.surface_destroy)(surface);
            Some(out)
        }
    }
}

#[cfg(all(target_os = "linux", feature = "gtk", not(feature = "zork")))]
fn main() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("pixels: no display, skipping");
        return;
    }
    let Some(gdk) = Gtk::load() else {
        eprintln!("pixels: no GTK3/Cairo libraries, skipping");
        return;
    };
    let Some(daemon_bin) = daemon_binary() else {
        eprintln!("pixels: no fflocated binary in this build, skipping");
        return;
    };
    if gtk_dynamic_loader::Loader::new().is_err() {
        eprintln!("pixels: GTK did not load, skipping");
        return;
    }

    // --- A daemon, and two result sets ---
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("sub")).expect("root");
    for name in [
        "alpha-pixels-1.txt",
        "alpha-pixels-2.md",
        "sub/alpha-pixels-3.rs",
        "beta-pixels-1.txt",
        "beta-pixels-2.md",
        "sub/beta-pixels-3.rs",
        "unrelated.rs",
    ] {
        std::fs::write(root.join(name), "pixel fixture\n").expect("fixture file");
    }
    let socket = dir.path().join("ff.sock");
    struct Guard(std::process::Child);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let child = std::process::Command::new(&daemon_bin)
        .arg("--foreground")
        .arg("--backend")
        .arg("fake")
        .arg("--root")
        .arg(&root)
        .arg("--state-dir")
        .arg(dir.path().join("state"))
        .arg("--socket")
        .arg(&socket)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn fflocated");
    let _guard = Guard(child);
    let client = ff_gui::DaemonClient::with_socket(socket.clone());
    let search = |query: &str| -> Vec<String> {
        let started = Instant::now();
        let mut last = String::new();
        while started.elapsed() < Duration::from_secs(30) {
            match client.search(query, 20) {
                Ok(outcome) if outcome.paths.len() == 3 => return outcome.paths,
                Ok(outcome) => last = format!("{} hit(s)", outcome.paths.len()),
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        panic!("the daemon never returned the 3 {query:?} rows: {last}");
    };
    let alpha = search("alpha-pixels");
    let beta = search("beta-pixels");
    println!(
        "pixels: daemon {} answered 3 hits for \"alpha-pixels\" and 3 for \"beta-pixels\"",
        daemon_bin.display()
    );
    println!("pixels: alpha rows {alpha:?}");

    // --- The app's results list, in a real GTK window ---
    let app = App::init().expect("GTK init");
    let win = app.create_window().expect("window");
    win.set_title("ff-gui daemon pixels test");
    let list = app
        .create_treeview(&[("", true, true), ("Name", false, true), ("Path", false, true)])
        .expect("treeview");
    let painted = list.clone().into_scrolled().expect("scrolled");
    let column = app.create_box(rswidgets::prelude::Orientation::Vertical, 1).expect("box");
    column.append(&painted);
    win.set_child(&column);
    win.present();
    let widget = win.raw_handle();
    // Let GTK map and allocate the tree; `gtk_widget_draw` draws the allocation
    // it already has, so this has to settle before the capture.
    for _ in 0..50 {
        unsafe {
            (gdk.main_iteration)(0);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let cells = |paths: &[String]| -> Vec<Vec<CellValue>> {
        paths
            .iter()
            .map(|p| {
                let name = p.rsplit('/').next().unwrap_or(p).to_string();
                vec![
                    CellValue::Icon {
                        name: "text-x-generic".to_string(),
                        fallback: "R".to_string(),
                    },
                    CellValue::Text(name),
                    CellValue::Text(p.clone()),
                ]
            })
            .collect()
    };
    let width = 900i32;
    let height = 420i32;
    let shot = |paths: &[String]| -> Vec<u8> {
        list.set_rows(&cells(paths));
        for _ in 0..5 {
            unsafe {
                (gdk.main_iteration)(0);
            }
        }
        gdk.capture(widget, width, height).expect("cairo surface")
    };

    let alpha_px = shot(&alpha);
    let stride = alpha_px.len() / height as usize;
    let ink = |px: &[u8]| -> usize {
        px.chunks(4)
            .filter(|p| p[0] < 200 || p[1] < 200 || p[2] < 200)
            .count()
    };
    let alpha_ink = ink(&alpha_px);
    println!(
        "pixels: captured {width}x{height} from the alpha rows (stride {stride}): ink {alpha_ink} px"
    );
    assert_eq!(alpha_px.len(), stride * height as usize, "the capture is not the surface that was asked for");
    assert!(alpha_ink > 100, "the daemon's rows painted {alpha_ink} px: nothing reached the surface");

    let beta_px = shot(&beta);
    let changed = alpha_px.iter().zip(beta_px.iter()).filter(|(a, b)| a != b).count();
    println!(
        "pixels: the beta rows changed {changed} px of the {width}x{height} capture ({} ink)",
        ink(&beta_px)
    );
    assert!(ink(&beta_px) > 100, "the second result set painted nothing");
    assert!(changed > 500, "swapping three rows for three different ones changed only {changed} px");

    let empty_px = shot(&[]);
    let blank_changed = alpha_px.iter().zip(empty_px.iter()).filter(|(a, b)| a != b).count();
    println!(
        "pixels: the empty list differs from the alpha rows by {blank_changed} px ({} ink)",
        ink(&empty_px)
    );
    assert!(blank_changed > 500, "an empty list changed only {blank_changed} px from the daemon's rows");

    println!("pixels: ok — daemon rows rendered through GTK and captured, two result sets and an empty list all differ");
}

#[cfg(not(all(target_os = "linux", feature = "gtk", not(feature = "zork"))))]
fn main() {
    // The capture renders through GTK; AppKit has its own in
    // `tests/daemon_pixels_mac.rs`.
    println!("daemon_pixels_gtk: needs the GTK backend (Linux, `gtk` on, `zork` off)");
}
