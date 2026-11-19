//! Headless proof that the AppKit backend is interactive, not just launching.
//!
//! Run it on a macOS host with a window-server session:
//!
//! ```sh
//! cargo run -p ff-gui --example macos_probe
//! ```
//!
//! It does what a user would do, through AppKit rather than around it:
//!
//! * presses a button with `-performClick:`, which sends the button's action to
//!   its target — the `CorroMacTarget` this crate now builds at runtime — and
//!   checks the Rust `on_click` closure actually ran, and
//! * renders the results canvas (a `CorroSheetView`, also built at runtime) into
//!   a bitmap and checks it is not uniformly blank.
//!
//! Either failure means the port is not interactive, so the process panics and
//! the CI step turns that into a readable annotation.

// The AppKit backend only exists when rswidgets was built with it: `zork` wins
// over every other backend, exactly as it does for `ff-gui` itself. Without this
// gate the example would fail to compile for the documented
// `--no-default-features --features zork` configuration — and `cargo test`
// compiles examples, so that would break the build rather than the probe.
#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use rswidgets::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let app = App::init()?;
    let win = app.create_window()?;
    win.set_title("ff-gui probe");

    // --- A button press that goes through AppKit's target/action ---
    let hits = Arc::new(AtomicUsize::new(0));
    let button = app.create_button("Search")?;
    {
        let hits = hits.clone();
        button.on_click(move || {
            hits.fetch_add(1, Ordering::SeqCst);
        })?;
    }
    win.set_child(&button);
    // `-performClick:` is what a click ends in: AppKit sends the control's
    // action to its target. If the target class were missing, the button would
    // be inert and the counter below would stay at zero.
    unsafe {
        let send: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *mut std::ffi::c_void) =
            std::mem::transmute(rswidgets::backends::apple::msg_shim());
        send(
            button.raw_handle(),
            rswidgets::backends::apple::selector("performClick:"),
            std::ptr::null_mut(),
        );
    }
    let fired = hits.load(Ordering::SeqCst);
    println!("probe: button action fired {fired} time(s)");
    assert_eq!(
        fired, 1,
        "AppKit did not deliver the button action to the Rust callback"
    );

    // --- The canvas must paint, not stay blank ---
    let canvas = app.create_canvas()?;
    canvas.set_content_size(320, 120);
    canvas.set_draw_callback(Box::new(|ctx, w, _h| {
        // Deliberately three distinct regions: a clear, a header strip and a
        // red block, plus text. A render that stays blank collapses to one
        // colour and fails the assertions below.
        ctx.clear(1.0, 1.0, 1.0, 1.0);
        ctx.fill_rect(0.0, 0.0, f64::from(w), 24.0, 0.93, 0.93, 0.95, 1.0);
        ctx.fill_rect(8.0, 40.0, 120.0, 32.0, 0.90, 0.10, 0.10, 1.0);
        ctx.draw_text(12.0, 100.0, "ff-gui", "sans", 14.0, 0.0, 0.0, 0.0, 1.0);
    }));
    let (w, h, px) = rswidgets::backends_macos_adapter::render_view_rgba(
        canvas.raw_handle(),
        320,
        120,
    )
    .ok_or("the sheet view produced no bitmap")?;
    let mut colours = std::collections::BTreeSet::new();
    for p in px.chunks_exact(4) {
        colours.insert((p[0], p[1], p[2], p[3]));
    }
    println!(
        "probe: canvas render {w}x{h}, {} distinct colours",
        colours.len()
    );
    assert!(
        colours.len() >= 3,
        "canvas render is blank or nearly so: {} distinct colours",
        colours.len()
    );
    let reddish = px.chunks_exact(4).any(|p| p[0] > 180 && p[1] < 90 && p[2] < 90);
    assert!(
        reddish,
        "the red block drawn by the canvas closure is missing from the render"
    );

    // --- The results list scrolls, the way it does on Linux ---
    //
    // `compat::TreeView` is the app's results list: one canvas whose content is
    // every row, inside a scrolled window. On GTK that container is a
    // `GtkScrolledWindow`, so the list is clipped, has a scroller and scrolls
    // with the wheel. On AppKit it used to be a bare `NSView`, which clipped
    // nothing, had no scroller and could not scroll: the rows simply drew past
    // the bottom of the window. This builds the same widget and asks the
    // resulting view the questions a user would answer by looking at it.
    fn send0(obj: *mut std::ffi::c_void, selname: &str) -> *mut std::ffi::c_void {
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> *mut std::ffi::c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(selname)) }
    }
    fn send_bool(obj: *mut std::ffi::c_void, selname: &str) -> bool {
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> bool =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(selname)) }
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    fn send_rect(obj: *mut std::ffi::c_void, selname: &str) -> Rect {
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> Rect =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(selname)) }
    }

    use rswidgets::compat::{CellValue, TreeView};
    let tree = TreeView::new(&[("", true, true), ("Name", false, true), ("Path", false, true)])?;
    let rows: Vec<Vec<CellValue>> = (0..40)
        .map(|i| {
            vec![
                CellValue::Icon {
                    name: "text-x-generic".to_owned(),
                    fallback: "=".to_owned(),
                },
                CellValue::Text(format!("file-{i:02}.rs")),
                CellValue::Text(format!("/tmp/fx/dir{}/file-{i:02}.rs", i % 7)),
            ]
        })
        .collect();
    tree.set_rows(&rows);
    let scrolled = tree.clone().into_scrolled()?;
    win.set_child(&scrolled);
    let sv = scrolled.inner.raw_handle();
    assert!(!sv.is_null(), "the scrolled window produced no view");
    // `set_child` has to size the root view: AppKit does not lay a plain
    // subview out, and an `NSRect` starts zero-filled — that blank window was
    // exactly what the first CI screenshot showed. The frame is *checked* here
    // rather than set, so a regression fails the probe instead of quietly
    // producing a white screenshot.
    {
        let frame = send_rect(sv, "frame");
        println!("probe: root view frame {}x{}", frame.w, frame.h);
        assert!(
            frame.w > 100.0 && frame.h > 100.0,
            "set_child left the root view at {}x{}: the window would render blank",
            frame.w,
            frame.h
        );
    }

    let doc = send0(sv, "documentView");
    let clip = send0(sv, "contentView");
    assert!(!doc.is_null(), "the list canvas is not the scroll view's document view");
    let doc_h = send_rect(doc, "bounds").h;
    let clip_h = send_rect(clip, "bounds").h;
    let scroller = send_bool(sv, "hasVerticalScroller");
    println!(
        "probe: results list {doc_h}pt of content in a {clip_h}pt viewport, vertical scroller {scroller}"
    );
    assert!(scroller, "the results scroll view has no vertical scroller");
    assert!(
        doc_h > clip_h,
        "40 rows are not taller than the viewport ({doc_h} <= {clip_h}): nothing to scroll"
    );

    // Scroll by the same adjustment terms GTK callers use, then check the clip
    // view actually moved: a scroller that is drawn but does not scroll is the
    // failure mode this is here to catch.
    scrolled.scroll_to(0.0, 0.0, 0.0, 100.0, doc_h, clip_h);
    let top = send_rect(clip, "bounds").y;
    println!("probe: scrolled to {top}pt (asked for 100) of {doc_h}pt");
    assert!(
        (top - 100.0).abs() < 0.5,
        "scroll_to left the clip view at {top}pt instead of 100pt"
    );

    // And the scrolled view still paints: a scrolled list that renders blank is
    // no more useful than an inert one.
    let (sw_px_w, sw_px_h, scrolled_px) =
        rswidgets::backends_macos_adapter::render_view_rgba(sv, 640, 200)
            .ok_or("the scrolled results view produced no bitmap")?;
    let mut scrolled_colours = std::collections::BTreeSet::new();
    for p in scrolled_px.chunks_exact(4) {
        scrolled_colours.insert((p[0], p[1], p[2], p[3]));
    }
    println!(
        "probe: scrolled render {sw_px_w}x{sw_px_h}, {} distinct colours",
        scrolled_colours.len()
    );
    assert!(
        scrolled_colours.len() >= 3,
        "the scrolled results list rendered blank: {} distinct colours",
        scrolled_colours.len()
    );

    println!("probe: ok");
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    println!("macos_probe: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
