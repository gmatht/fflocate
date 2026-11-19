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
    fn send0i(obj: *mut std::ffi::c_void, selname: &str) -> isize {
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> isize =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(selname)) }
    }
    fn send0v(obj: *mut std::ffi::c_void, selname: &str) {
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(selname)) }
    }
    fn send1v(obj: *mut std::ffi::c_void, selname: &str, arg: *mut std::ffi::c_void) {
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *mut std::ffi::c_void) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(selname), arg) }
    }
    fn send_priority(
        obj: *mut std::ffi::c_void,
        selname: &str,
        priority: f32,
        orientation: isize,
    ) {
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, f32, isize) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe {
            f(
                obj,
                rswidgets::backends::apple::selector(selname),
                priority,
                orientation,
            )
        }
    }
    fn send1i(obj: *mut std::ffi::c_void, selname: &str, arg: isize) -> *mut std::ffi::c_void {
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, isize) -> *mut std::ffi::c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(selname), arg) }
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
    println!("probe: {} rows with icons in the list", rows.len());
    assert!(
        scrolled_colours.len() >= 20,
        "the populated results list rendered too flat: {} distinct colours",
        scrolled_colours.len()
    );
    // Text and icon glyphs are the only dark pixels; an empty white viewport
    // would have none.
    assert!(
        scrolled_px
            .chunks_exact(4)
            .any(|p| p[0] < 120 && p[1] < 120 && p[2] < 120),
        "no dark text or icon pixels in the populated list render"
    );

    // --- A box's children span its width, the way GTK's box stretches them ---
    //
    // `NSStackView` only aligns arranged subviews, so they kept their intrinsic
    // widths and the window's UI sat in a narrow central column. These frames
    // are the difference between that and a GTK-like layout.
    {
        let probe_box = app.create_box(Orientation::Vertical, 4)?;
        let probe_label = app.create_label("box child")?;
        probe_box.append(&probe_label);
        win.set_child(&probe_box);
        let box_ptr = probe_box.raw_handle();
        send0v(box_ptr, "layoutSubtreeIfNeeded");
        let box_bounds = send_rect(box_ptr, "bounds");
        let child_frame = send_rect(probe_label.raw_handle(), "frame");
        println!(
            "probe: box {}pt wide, child {}pt ({}% of the box)",
            box_bounds.w,
            child_frame.w,
            if box_bounds.w > 0.0 {
                (child_frame.w * 100.0 / box_bounds.w).round() as i64
            } else {
                0
            }
        );
        assert!(
            box_bounds.w > 100.0,
            "the box itself has no width: {}pt",
            box_bounds.w
        );
        // The window keeps a 5pt margin around this box, and the box's child
        // spans the box: a child left at its intrinsic width would be far
        // narrower than this.
        assert!(
            (box_bounds.w - child_frame.w).abs() <= 8.0,
            "the box's child is {}pt wide in a {}pt box: it is not filling the width",
            child_frame.w,
            box_bounds.w
        );
    }

    // --- Scrolling reaches the end of the content ---
    scrolled.scroll_to(0.0, 0.0, 0.0, doc_h, doc_h, clip_h);
    let end = send_rect(clip, "bounds").y;
    let last = (doc_h - clip_h).max(0.0);
    println!("probe: scrolled to the end at {end}pt (last {last}pt)");
    assert!(
        (end - last).abs() < 0.5,
        "scrolling to the end left the clip view at {end}pt instead of {last}pt"
    );

    // --- A dialog presents its buttons, and a click reaches Rust ---
    {
        let dlg = app.new_dialog()?;
        dlg.set_title("ff-gui probe: daemon status");
        dlg.add_button("Close", 0);
        dlg.add_button("Open", 1);
        let fired: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(usize::MAX));
        {
            let fired = fired.clone();
            dlg.connect_response(move |response| {
                fired.store(response as usize, Ordering::SeqCst);
            })?;
        }
        let alert = dlg.inner.raw_handle();
        // An alert lays its buttons out on request; before that they exist but
        // have no frames, which is what "presented with no buttons" looked like.
        send0v(alert, "layout");
        let buttons = send0(alert, "buttons");
        let count = send0i(buttons, "count") as usize;
        let first = send1i(buttons, "objectAtIndex:", 0);
        let first_frame = if first.is_null() {
            Rect::default()
        } else {
            send_rect(first, "frame")
        };
        println!(
            "probe: alert has {count} buttons, first is {}x{} at ({}, {})",
            first_frame.w, first_frame.h, first_frame.x, first_frame.y
        );
        assert_eq!(count, 2, "the alert presents {count} buttons, expected 2");
        assert!(
            first_frame.w > 0.0 && first_frame.h > 0.0,
            "the alert's first button has no frame, so nothing would be clickable"
        );
        // Show the alert, then press its first button: `-performClick:` sends
        // the action the adapter installed, exactly as a user's click does.
        dlg.set_visible(true);
        send1v(first, "performClick:", std::ptr::null_mut());
        let response = fired.load(Ordering::SeqCst);
        println!("probe: alert response {response} (0 = first button)");
        assert_eq!(
            response, 0,
            "the alert's clicked button did not reach the Rust response handler"
        );
    }

    // --- The capture is the whole window: title bar, scroller, GTK's columns ---
    //
    // A screenshot of the content view alone cannot be compared with a Linux
    // capture of a whole window, and one taken with the platform's overlay
    // scrollers shows no scroller at all, because they fade out.
    {
        let outer = app.create_box(Orientation::Vertical, 6)?;
        let margin_label = app.create_label("max distance:")?;
        outer.append(&margin_label);
        outer.append(&scrolled);
        win.set_child(&outer);
        // The list takes the spare height, as it does in the app (where
        // `set_vexpand` asks for exactly this): a stack view gives leftover
        // space to the child that resists growing least.
        send_priority(
            scrolled.inner.raw_handle(),
            "setContentHuggingPriority:forOrientation:",
            1.0,
            1,
        );

        let view = win.raw_handle();
        // Who has no height? The stack, the label or the list?
        {
            let b = send_rect(outer.raw_handle(), "frame");
            let l = send_rect(margin_label.raw_handle(), "frame");
            let s = send_rect(scrolled.inner.raw_handle(), "frame");
            println!(
                "probe: before capture box {}x{} at ({}, {}), label {}x{}, list {}x{}, distribution {}",
                b.w,
                b.h,
                b.x,
                b.y,
                l.w,
                l.h,
                s.w,
                s.h,
                send0i(outer.raw_handle(), "distribution")
            );
        }
        let shot = rswidgets::backends_macos_adapter::capture(view, Some((1350.0, 858.0)))
            .ok_or("the window produced no capture")?;
        let scroller_view = send0(sv, "verticalScroller");
        let scroller_frame = if scroller_view.is_null() {
            Rect::default()
        } else {
            send_rect(scroller_view, "frame")
        };
        let sv_frame = send_rect(sv, "frame");
        {
            let b = send_rect(outer.raw_handle(), "frame");
            let l = send_rect(margin_label.raw_handle(), "frame");
            let c = send_rect(sv, "superview");
            println!(
                "probe: after capture box {}x{} at ({}, {}), label {}x{}, its superview {}x{}",
                b.w, b.h, b.x, b.y, l.w, l.h, c.w, c.h
            );
        }
        println!(
            "probe: scroller style {} (0 overlay, 1 legacy), vertical scroller {} at x={} {}x{}, scroll view {}x{}",
            send0i(sv, "scrollerStyle"),
            !scroller_view.is_null(),
            scroller_frame.x,
            scroller_frame.w,
            scroller_frame.h,
            sv_frame.w,
            sv_frame.h
        );
        println!(
            "probe: capture {}x{} (linux 1350x858), title bar {}, scroller ink {} px on {}% of the gutter",
            shot.width, shot.height, shot.title_bar, shot.scroller_ink, shot.scroller_coverage
        );
        assert_eq!(
            (shot.width, shot.height),
            (1350, 858),
            "the capture is not the size of the Linux one"
        );
        assert!(shot.title_bar, "the capture has no window title bar");
        assert!(
            shot.scroller_coverage >= 60,
            "no visible vertical scrollbar: ink on {}% of the gutter",
            shot.scroller_coverage
        );

        // Columns: the list must be exactly as wide as its viewport (a canvas
        // wider than the window laid its columns out for the wider one, which
        // put the path column at 80% and clipped it), and the path cells must
        // land where Linux puts them. Measured from the pixels the list drew,
        // the same way the Linux number was measured.
        let clip_w = send_rect(clip, "bounds").w;
        let doc_w = send_rect(doc, "frame").w;
        let (pw, ph) = (doc_w.max(1.0) as u32, send_rect(doc, "frame").h.max(1.0) as u32);
        let (_, _, list_px) =
            rswidgets::backends_macos_adapter::render_view_rgba(doc, pw, ph)
                .ok_or("the list produced no bitmap")?;
        // The first data row sits under the 22px header; its third run of dark
        // pixels is the path cell.
        let mut path_start = 0.0f64;
        for y in 24..36usize.min(ph as usize) {
            let mut run: Option<usize> = None;
            for x in 0..pw as usize {
                let i = (y * pw as usize + x) * 4;
                let dark = list_px[i] < 140 && list_px[i + 1] < 140 && list_px[i + 2] < 140;
                if dark && run.is_none() {
                    run = Some(x);
                } else if !dark {
                    if let Some(start) = run {
                        if start as f64 > pw as f64 * 0.3 {
                            path_start = start as f64;
                            break;
                        }
                        run = None;
                    }
                }
            }
            if path_start > 0.0 {
                break;
            }
        }
        let path_share = path_start / pw as f64;
        println!(
            "probe: list viewport {clip_w}pt, canvas {doc_w}pt, path column at {:.1}% (linux 53.6%)",
            path_share * 100.0
        );
        assert!(
            (doc_w - clip_w).abs() <= 1.0,
            "the list canvas is {doc_w}pt wide in a {clip_w}pt viewport"
        );
        assert!(
            (path_share - 0.536).abs() < 0.02,
            "the path column starts at {:.1}% of the list, Linux puts it at 53.6%",
            path_share * 100.0
        );

        // Left margin: the top-level box keeps the few points GTK's window
        // leaves around its content, so a label does not touch the edge.
        let mut x = send_rect(margin_label.raw_handle(), "frame").x;
        let mut v = send0(margin_label.raw_handle(), "superview");
        while !v.is_null() && v != view {
            x += send_rect(v, "frame").x;
            v = send0(v, "superview");
        }
        println!("probe: left label margin {x}pt (linux 5)");
        assert!(
            (x - 5.0).abs() <= 2.5,
            "the label sits {x}pt from the window edge, Linux puts it at 5pt"
        );
    }

    println!("probe: ok");
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    println!("macos_probe: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
