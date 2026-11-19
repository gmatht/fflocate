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
    fn send0d(obj: *mut std::ffi::c_void, selname: &str) -> f64 {
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> f64 =
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
    fn send1iv(obj: *mut std::ffi::c_void, selname: &str, arg: isize) {
        let f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, isize) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(selname), arg) }
    }
    fn send1p(
        obj: *mut std::ffi::c_void,
        selname: &str,
        arg: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void {
        let f: unsafe extern "C" fn(
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void = unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
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

    // The left column draws a picture now, not one letter per file kind: the
    // fixture's icons are all plain documents, painted as a grey page
    // (0.45, 0.47, 0.52). Letters would leave only dark ink there.
    let mut icon_px = 0usize;
    for (i, p) in scrolled_px.chunks_exact(4).enumerate() {
        let x = (i as u32 % sw_px_w) as f64;
        if x > 40.0 {
            continue; // somewhere right of the icon column
        }
        let near = |v: u8, want: f64| (f64::from(v) - want * 255.0).abs() < 30.0;
        if near(p[0], 0.45) && near(p[1], 0.47) && near(p[2], 0.52) {
            icon_px += 1;
        }
    }
    println!("probe: {icon_px} px of icon colour in the left column");
    assert!(
        icon_px >= 40,
        "the icon column drew no pictogram ({icon_px} px of icon colour)"
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
        let case_cb = app.create_checkbutton("Case sensitive")?;
        let fuzzy_cb = app.create_checkbutton("Fuzzy matching")?;
        let count_cb = app.create_checkbutton("Count only (skip the list)")?;
        outer.append(&margin_label);
        outer.append(&case_cb);
        outer.append(&fuzzy_cb);
        outer.append(&count_cb);
        // GTK gives an expanding entry the query row's leftover width and leaves
        // the control beside it at its natural size — in linux-gui-rows.png a
        // 1150pt entry beside a 74pt Search button. The same shape, in a row the
        // window's width.
        let flex_row = app.create_box(Orientation::Horizontal, 2)?;
        let flex_label = app.create_label("go:")?;
        let flex_entry = app.create_entry()?;
        rswidgets::backends::apple::set_view_expanding(flex_entry.raw_handle(), true);
        flex_row.append(&flex_label);
        flex_row.append(&flex_entry);
        outer.append(&flex_row);
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

        // A canvas wider than its viewport pushes the path column past the
        // right edge: size it to the window's content width, which is what the
        // app's own layout does when the list is given the window.
        let content_w = 1350.0 - 2.0 * 5.0;
        rswidgets::backends_macos_adapter::Canvas(doc)
            .set_size_request(content_w as i32, doc_h as i32);

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
            let box_ptr = outer.raw_handle();
            let content = send0(box_ptr, "superview");
            let content_frame = if content.is_null() {
                Rect::default()
            } else {
                send_rect(content, "frame")
            };
            let box_constraints = send0(box_ptr, "constraints");
            let content_constraints = if content.is_null() {
                std::ptr::null_mut()
            } else {
                send0(content, "constraints")
            };
            println!(
                "probe: after capture box {}x{} at ({}, {}), label {}x{}, content {}x{}, box constraints {}, content constraints {}, box mask {}, translates {}",
                b.w,
                b.h,
                b.x,
                b.y,
                l.w,
                l.h,
                content_frame.w,
                content_frame.h,
                send0i(box_constraints, "count"),
                send0i(content_constraints, "count"),
                send0i(box_ptr, "autoresizingMask"),
                send0i(box_ptr, "translatesAutoresizingMaskIntoConstraints") != 0
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
            "probe: capture {}x{} (linux 1350x858), title bar {}, scroller ink {} px on {}% of the gutter, content ink {} px",
            shot.width,
            shot.height,
            shot.title_bar,
            shot.scroller_ink,
            shot.scroller_coverage,
            shot.content_ink
        );
        println!(
            "probe: scroller hidden {}, alpha {}, knob {}, doc {}x{}",
            send0i(scroller_view, "isHidden") != 0,
            if scroller_view.is_null() {
                0.0
            } else {
                send0d(scroller_view, "alphaValue")
            },
            if scroller_view.is_null() {
                0.0
            } else {
                send0d(scroller_view, "knobProportion")
            },
            send_rect(doc, "frame").w,
            send_rect(doc, "frame").h
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

        // The chrome the offscreen render leaves out, and the controls a user
        // sees: the traffic lights in the title band, and each advanced
        // checkbox with its label, sampled where the widget actually sits.
        let content_h = send_rect(view, "bounds").h;
        let band = shot.height as f64 - content_h;
        let ink_in = |widget: *mut std::ffi::c_void| -> usize {
            let f = send_rect(widget, "frame");
            let mut x = f.x;
            let mut y = f.y;
            let mut v = send0(widget, "superview");
            while !v.is_null() && v != view {
                let parent = send_rect(v, "frame");
                x += parent.x;
                y += parent.y;
                v = send0(v, "superview");
            }
            // AppKit's origin is the bottom left, the image's the top left.
            let x0 = x.max(0.0) as usize;
            let x1 = ((x + f.w).max(0.0) as usize).min(shot.width as usize);
            let y0 = ((band + content_h - (y + f.h)).max(0.0) as usize).min(shot.height as usize);
            let y1 = (y0 + f.h.max(0.0) as usize).min(shot.height as usize);
            let mut ink = 0usize;
            for row in y0..y1 {
                for col in x0..x1 {
                    let i = (row * shot.width as usize + col) * 4;
                    if shot.rgba[i] < 200 || shot.rgba[i + 1] < 200 || shot.rgba[i + 2] < 200 {
                        ink += 1;
                    }
                }
            }
            ink
        };
        let (case_ink, fuzzy_ink, count_ink) = (
            ink_in(case_cb.raw_handle()),
            ink_in(fuzzy_cb.raw_handle()),
            ink_in(count_cb.raw_handle()),
        );
        let mut traffic_ink = 0usize;
        for row in 0..(band as usize).min(shot.height as usize) {
            for col in 0..80usize.min(shot.width as usize) {
                let i = (row * shot.width as usize + col) * 4;
                let (r, g, b) = (
                    shot.rgba[i] as i32,
                    shot.rgba[i + 1] as i32,
                    shot.rgba[i + 2] as i32,
                );
                if r.max(g).max(b) - r.min(g).min(b) > 40 {
                    traffic_ink += 1;
                }
            }
        }
        println!(
            "probe: title band {band}pt, traffic lights {traffic_ink} px of colour, checkboxes ink case {case_ink}, fuzzy {fuzzy_ink}, count {count_ink}"
        );
        assert!(
            band >= 20.0,
            "the capture has no title band ({band}pt of chrome)"
        );
        assert!(
            traffic_ink >= 80,
            "the title band has no traffic-light buttons ({traffic_ink} coloured px)"
        );
        for (label, ink) in [
            ("Case sensitive", case_ink),
            ("Fuzzy matching", fuzzy_ink),
            ("Count only (skip the list)", count_ink),
        ] {
            assert!(
                ink >= 30,
                "the \"{label}\" checkbox did not render into the capture ({ink} px of ink)"
            );
        }

        // An expanding child takes the row's leftover width, as GTK's box does.
        let flex_entry_w = send_rect(flex_entry.raw_handle(), "frame").w;
        let flex_label_w = send_rect(flex_label.raw_handle(), "frame").w;
        println!(
            "probe: expanding entry {flex_entry_w}pt, label beside it {flex_label_w}pt of a 1340pt row"
        );
        assert!(
            flex_entry_w > 900.0,
            "the expanding entry got {flex_entry_w}pt of a 1340pt row, not the leftover width"
        );
        assert!(
            flex_label_w < 120.0,
            "the label beside it stretched to {flex_label_w}pt instead of keeping its size"
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
        // The frame says where the label's box is; the pixels say where its
        // glyphs are, which is what "the label sits N points from the edge"
        // means to a reader comparing this with the Linux capture.
        let f = send_rect(margin_label.raw_handle(), "frame");
        let mut y = f.y;
        let mut v = send0(margin_label.raw_handle(), "superview");
        while !v.is_null() && v != view {
            y += send_rect(v, "frame").y;
            v = send0(v, "superview");
        }
        let row0 = ((band + content_h - (y + f.h)).max(0.0) as usize).min(shot.height as usize);
        let row1 = (row0 + f.h.max(0.0) as usize).min(shot.height as usize);
        let mut ink_x: Option<usize> = None;
        for row in row0..row1 {
            for col in 0..shot.width as usize {
                let i = (row * shot.width as usize + col) * 4;
                if shot.rgba[i] < 200 || shot.rgba[i + 1] < 200 || shot.rgba[i + 2] < 200 {
                    ink_x = Some(col);
                    break;
                }
            }
            if ink_x.is_some() {
                break;
            }
        }
        let margin = ink_x.unwrap_or(0) as f64;
        println!("probe: left label margin {margin}pt of ink (frame {x}pt, linux 5)");
        assert!(
            (margin - 5.0).abs() <= 1.0,
            "the label's text starts {margin}pt from the window edge, Linux puts it at 5pt"
        );
    }

    // --- A real daemon: started, questioned, stopped ---
    //
    // The daemon is an ordinary child process, not a service: this starts one
    // on a throwaway root and socket, asks it the question a user's first
    // search would ask, and kills it before the assertions. Nothing has to be
    // running beforehand, and nothing is left behind, so the check needs no
    // supervision and no privileges on the runner. The binary is the one this
    // build just produced (`cargo build -p fflocated`), not one from $PATH.
    let fixture = tempfile::tempdir()?;
    let root = fixture.path().join("root");
    std::fs::create_dir_all(root.join("sub"))?;
    for name in ["alpha-probe.txt", "beta-probe.md", "sub/gamma-probe.rs"] {
        std::fs::write(root.join(name), "probe fixture\n")?;
    }
    // A file the query must not match: three hits, not four.
    std::fs::write(root.join("unrelated.rs"), "no match here\n")?;
    let socket = fixture.path().join("ff.sock");
    let exe = std::env::current_exe()?;
    let daemon_bin = exe
        .parent()
        .and_then(|examples| examples.parent())
        .map(|debug| debug.join("fflocated"))
        .filter(|p| p.exists())
        .ok_or_else(|| {
            format!(
                "no fflocated next to {}: build it first (cargo build -p fflocated)",
                exe.display()
            )
        })?;
    // The daemon is killed when this guard drops, so a failed assertion below
    // cannot leave one running (and holding the socket) for the rest of the
    // job. `stop` is what the probe calls when everything passed.
    struct DaemonGuard(Option<std::process::Child>);
    impl DaemonGuard {
        fn stop(&mut self) -> (bool, bool) {
            match self.0.take() {
                None => (false, false),
                Some(mut child) => {
                    let killed = child.kill().is_ok();
                    let reaped = child.wait().is_ok();
                    (killed, reaped)
                }
            }
        }
    }
    impl Drop for DaemonGuard {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    let mut daemon = DaemonGuard(Some(
        std::process::Command::new(&daemon_bin)
            .arg("--foreground")
            .arg("--backend")
            .arg("fake")
            .arg("--root")
            .arg(&root)
            .arg("--state-dir")
            .arg(fixture.path().join("state"))
            .arg("--socket")
            .arg(&socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?,
    ));
    // The GUI's own client, so this exercises the request it really sends
    // (protocol version stamped, advanced options applied) rather than a
    // hand-written JSON line that could drift from it.
    let client = ff_gui::DaemonClient::with_socket(socket.clone());
    let attempt = std::time::Instant::now();
    let mut hits = 0usize;
    let mut attempts = 0usize;
    let mut last_error = String::new();
    // The socket binds before the first crawl finishes, so ask again until the
    // fixture files are indexed; a wedged daemon fails on the deadline instead
    // of hanging the probe.
    while attempt.elapsed() < std::time::Duration::from_secs(30) {
        attempts += 1;
        match client.search("probe", 10) {
            Ok(outcome) => {
                hits = outcome.paths.len();
                if hits >= 3 {
                    break;
                }
            }
            Err(e) => last_error = e.to_string(),
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let elapsed = attempt.elapsed().as_millis();
    println!(
        "probe: daemon {} spawned on a temp root, answered {hits} hits for \"probe\" in {elapsed}ms on attempt {attempts}",
        daemon_bin.display()
    );
    assert_eq!(
        hits, 3,
        "the daemon answered {hits} of the 3 fixture files (last error {last_error:?})"
    );

    // --- Typing: change -> request -> rows ---
    //
    // `controlTextDidChange:` reaches `dispatch_text_changed`, which is exactly
    // what a keystroke produces (`setText:` deliberately suppresses the
    // signal, so it is raised here on purpose). The callback is the app's own
    // step: ask the daemon and put the answer into the list.
    let cells_of = |paths: &[String]| -> Vec<Vec<rswidgets::compat::CellValue>> {
        paths
            .iter()
            .map(|p| {
                let name = p.rsplit('/').next().unwrap_or(p).to_string();
                vec![
                    rswidgets::compat::CellValue::Icon {
                        name: "text-x-generic".to_string(),
                        fallback: "R".to_string(),
                    },
                    rswidgets::compat::CellValue::Text(name),
                    rswidgets::compat::CellValue::Text(p.clone()),
                ]
            })
            .collect()
    };
    let list = app.create_treeview(&[
        ("", true, false),
        ("Name", false, true),
        ("Path", false, true),
    ])?;
    let typed = Arc::new(AtomicUsize::new(0));
    let typed_rows = Arc::new(AtomicUsize::new(0));
    let daemon_paths: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // What the pump sent, so the request itself is asserted rather than assumed.
    let requests: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // The GUI's own debounce type, shortened so the probe does not wait 150 ms.
    let debounce = std::rc::Rc::new(ff_gui::Debounce::new(std::time::Duration::from_millis(120)));
    let query_entry = app.create_entry()?;
    {
        let typed = typed.clone();
        let debounce = debounce.clone();
        query_entry.connect_changed(move || {
            typed.fetch_add(1, Ordering::SeqCst);
            // A keystroke only arms the deadline; it must never send a request.
            debounce.arm();
        })?;
    }
    query_entry.set_text("probe");
    rswidgets::backends_macos_adapter::dispatch_text_changed(query_entry.raw_handle());
    let changes = typed.load(Ordering::SeqCst);
    assert_eq!(changes, 1, "a text change did not reach connect_changed");
    assert!(
        debounce.pending(),
        "the text change did not arm the debounce: the window would never elapse"
    );
    assert!(
        requests.lock().unwrap().is_empty(),
        "a request went out while the user was still typing: {:?}",
        requests.lock().unwrap()
    );

    // The pump: issue once the window has elapsed, then paint the answer. The
    // GUI runs this on a 30 ms timer; stepping it here is the same decision.
    let mut issued = false;
    for _ in 0..200 {
        if debounce.due() {
            let query = query_entry.get_text().unwrap_or_default();
            requests.lock().unwrap().push(query.clone());
            let client = ff_gui::DaemonClient::with_socket(socket.clone());
            if let Ok(outcome) = client.search(&query, 10) {
                let rows = cells_of(&outcome.paths);
                typed_rows.store(rows.len(), Ordering::SeqCst);
                *daemon_paths.lock().unwrap() = outcome.paths.clone();
                list.set_rows(&rows);
            }
            debounce.clear();
            issued = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(issued, "the debounce never elapsed, so no request was issued");
    assert!(!debounce.pending(), "the debounce stayed armed after issuing");
    let sent = requests.lock().unwrap().clone();
    let rows_after_typing = typed_rows.load(Ordering::SeqCst);
    println!(
        "probe: typing {changes} change(s) -> debounce -> request {sent:?} -> {rows_after_typing} row(s) repainted"
    );
    assert_eq!(
        sent,
        vec!["probe".to_string()],
        "the request did not carry the typed text exactly once"
    );
    assert_eq!(
        rows_after_typing, 3,
        "the typing path drew {rows_after_typing} rows, not the daemon's 3"
    );
    // Read the answer back out of the list's own model, not the closure.
    let expected_names: Vec<String> = daemon_paths
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
        .collect();
    let painted_names: Vec<String> = (0..expected_names.len())
        .filter_map(|row| list.cell_text(row, 1))
        .collect();
    assert_eq!(
        painted_names, expected_names,
        "the list does not hold the rows the daemon returned"
    );

    // --- Pixels from the daemon's answer ---
    //
    // Rows the daemon actually returned, painted by the real list and read back
    // out of the capture: the search path, end to end, in pixels. This also
    // draws the list, which is what measures the width its header clicks are
    // hit-tested against -- a list that has never been drawn lays its columns
    // out for the default width and would sort by the wrong column.
    let holder_box = app.create_box(Orientation::Vertical, 2)?;
    {
        let painted = list.clone().into_scrolled()?;
        holder_box.append(&painted);
        win.set_child(&holder_box);
        let window_view = win.raw_handle();
        send0v(window_view, "layoutSubtreeIfNeeded");
        // Ink inside the list, in pixels: the measure is the list against
        // itself with no rows, so the threshold does not depend on a font.
        let list_ink = |list: &rswidgets::compat::TreeView| -> Result<usize, Box<dyn std::error::Error>> {
            let shot_rows =
                rswidgets::backends_macos_adapter::capture(window_view, Some((1350.0, 858.0)))
                    .ok_or("the window produced no capture")?;
            let f = send_rect(list.raw_handle(), "frame");
            let mut x = f.x;
            let mut y = f.y;
            let mut v = send0(list.raw_handle(), "superview");
            while !v.is_null() && v != window_view {
                let parent = send_rect(v, "frame");
                x += parent.x;
                y += parent.y;
                v = send0(v, "superview");
            }
            let content_h2 = send_rect(window_view, "bounds").h;
            let band2 = shot_rows.height as f64 - content_h2;
            let x0 = x.max(0.0) as usize;
            let x1 = ((x + f.w).max(0.0) as usize).min(shot_rows.width as usize);
            let y0 =
                ((band2 + content_h2 - (y + f.h)).max(0.0) as usize).min(shot_rows.height as usize);
            let y1 = (y0 + f.h.max(0.0) as usize).min(shot_rows.height as usize);
            let mut ink = 0usize;
            for row in y0..y1 {
                for col in x0..x1 {
                    let i = (row * shot_rows.width as usize + col) * 4;
                    if shot_rows.rgba[i] < 200
                        || shot_rows.rgba[i + 1] < 200
                        || shot_rows.rgba[i + 2] < 200
                    {
                        ink += 1;
                    }
                }
            }
            Ok(ink)
        };
        let ink = list_ink(&list)?;
        println!(
            "probe: the daemon's {rows_after_typing} row(s) painted {ink} px of ink in the list"
        );
        // Three rows of text measure ~150 px of ink at the default font; the
        // floor is low enough to survive a font change and far above the 0 a
        // blank list or a missed draw leaves.
        assert!(
            ink > 50,
            "the daemon's rows painted only {ink} px, so nothing reached the screen"
        );
    }

    // --- The list's own click handling ---
    //
    // compat hit-tests the canvas itself, but only if AppKit's mouse plumbing
    // reaches it: the id below is the one the sheet view sends with every
    // click, so a mismatched id here is a click that goes nowhere.
    let canvas_id = rswidgets::backends_macos_adapter::Canvas(list.raw_handle()).canvas_id();
    assert_ne!(
        canvas_id, 0,
        "the list's canvas has no id, so no click can reach it"
    );
    let presses: std::rc::Rc<std::cell::RefCell<Vec<(u32, f64, f64)>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    {
        let presses = presses.clone();
        list.connect_button_press_event(move |button, x, y, _event| {
            presses.borrow_mut().push((button, x, y));
        })?;
    }
    // The second row: 22pt of header, then 20pt rows.
    let row_click_y = 22.0 + 20.0 + 10.0;
    assert_eq!(
        list.row_at(60.0, row_click_y),
        Some(1),
        "the click point is not on the second row"
    );
    list.select_row(1);
    let expected = list
        .selected_path(2)
        .expect("select_row selected nothing");
    rswidgets::backends_macos_adapter::dispatch_canvas_click_button(
        canvas_id, 60.0, row_click_y, 1, 0,
    );
    // Delivered at all? The list's own handler runs on every click, so an
    // empty log here means the id AppKit passes and the id the list registered
    // under are not the same one -- a click that goes nowhere.
    let delivered = presses.borrow().clone();
    println!(
        "probe: list canvas id {canvas_id}, click delivered as {delivered:?}"
    );
    assert!(
        !delivered.is_empty(),
        "the canvas click never reached the list (canvas id {canvas_id})"
    );
    let selected = list
        .selected_path(2)
        .expect("a primary click selected no row");
    assert_eq!(selected, expected, "a click selected the wrong row");
    // A header click must reach the list too: that is the band a sort
    // triggers from, and a click that stopped at AppKit's event plumbing would
    // never get there. (The committed list only selects rows; column sorting
    // lives in the compat work that is not committed yet.)
    rswidgets::backends_macos_adapter::dispatch_canvas_click_button(canvas_id, 200.0, 6.0, 1, 0);
    let header_click = presses.borrow().last().copied();
    assert_eq!(
        header_click,
        Some((1, 200.0, 6.0)),
        "a click in the header band did not reach the list"
    );
    // The secondary button reaches the app's handler with the row's own
    // coordinates, which is what its right-click menu needs.
    rswidgets::backends_macos_adapter::dispatch_canvas_click_button(
        canvas_id, 60.0, row_click_y, 3, 0,
    );
    let last = presses.borrow().last().copied();
    assert_eq!(
        last,
        Some((3, 60.0, row_click_y)),
        "a right-click did not reach the list"
    );
    println!(
        "probe: clicks selected {selected}, header click delivered {header_click:?}, right-click delivered {last:?}"
    );

    // --- A header click re-sorts the rows ---
    //
    // The header band is the top 22 points and the titles are drawn in 32%
    // steps of the canvas width, so x=600 lands on the Name column at the
    // drawn width. What matters is the row order, so the first row is read
    // before and after rather than trusting the sort flag alone.
    let first_row = |list: &rswidgets::compat::TreeView| -> String {
        list.select_row(0);
        list.selected_path(2).unwrap_or_default()
    };
    let before_sort = first_row(&list);
    rswidgets::backends_macos_adapter::dispatch_canvas_click_button(canvas_id, 600.0, 6.0, 1, 0);
    let ascending_state = list.sort_state();
    let ascending_first = first_row(&list);
    rswidgets::backends_macos_adapter::dispatch_canvas_click_button(canvas_id, 600.0, 6.0, 1, 0);
    let descending_state = list.sort_state();
    let descending_first = first_row(&list);
    println!(
        "probe: header click sorted by {ascending_state:?} then {descending_state:?}; first row {before_sort} -> {ascending_first} -> {descending_first}"
    );
    assert_eq!(
        ascending_state,
        Some((1, true)),
        "the first header click did not sort ascending by Name"
    );
    assert_eq!(
        descending_state,
        Some((1, false)),
        "the second header click did not reverse the sort"
    );
    assert!(
        ascending_first.ends_with("alpha-probe.txt"),
        "sorting by Name ascending put {ascending_first} first"
    );
    assert!(
        descending_first.ends_with("gamma-probe.rs"),
        "sorting by Name descending put {descending_first} first"
    );
    assert_ne!(
        ascending_first, descending_first,
        "the header click changed the flag but not the row order"
    );

    // --- A double click opens the row through the real handler ---
    //
    // The handler installed here is `ff_gui::connect_open_on_double_click`, the
    // same function the app installs, so this exercises the app's Open action
    // (command, spawn, status line) rather than a stand-in. The rows are in
    // descending order from the check above, so row 1 is beta-probe.md.
    {
        let opened: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = opened.clone();
        ff_gui::connect_open_on_double_click(&list, 2, move |line| {
            seen.borrow_mut().push(line);
        });
        // A real double click is a press, then a second press with a count of
        // two; the adapter forwards the count, and the probe dispatches the
        // two the way AppKit would.
        rswidgets::backends_macos_adapter::dispatch_canvas_click_button(canvas_id, 60.0, 52.0, 1, 0);
        let selected_by_click = list.selected_path(2).unwrap_or_default();
        rswidgets::backends_macos_adapter::dispatch_canvas_double_click(canvas_id, 60.0, 52.0);
        let lines = opened.borrow().clone();
        println!(
            "probe: click selected {selected_by_click}, double click activated {lines:?}"
        );
        assert!(
            selected_by_click.ends_with("beta-probe.md"),
            "a click on row 1 selected {selected_by_click}"
        );
        assert_eq!(
            lines.len(),
            1,
            "the double click activated {} handlers: {lines:?}",
            lines.len()
        );
        assert!(
            lines[0].contains("beta-probe.md"),
            "the open handler ran for {:?}, not the double-clicked row",
            lines[0]
        );
        assert!(
            lines[0].starts_with("Open"),
            "the row action that ran was {:?}",
            lines[0]
        );
    }

    // --- The menu bar is a real NSMenu on the application ---
    let menu_fired = Arc::new(AtomicUsize::new(0));
    let mut model = app.new_menu()?;
    let mut menu = app.new_menu()?;
    let action = app.new_simple_action("probe_menu_item")?;
    {
        let menu_fired = menu_fired.clone();
        action.connect_activate(move |_| {
            menu_fired.fetch_add(1, Ordering::SeqCst);
        })?;
    }
    menu.append("Probe Item", "app.probe_menu_item");
    model.append_submenu("Probe", &menu);
    let _bar = app.new_menubar(&model, std::ptr::null_mut())?;
    let nsapp = send0(
        rswidgets::backends::apple::cls("NSApplication"),
        "sharedApplication",
    );
    let main_menu = send0(nsapp, "mainMenu");
    assert!(
        !main_menu.is_null(),
        "the menu bar was not installed as the application's main menu"
    );
    let submenus = send0i(main_menu, "numberOfItems");
    assert!(submenus >= 1, "the main menu has no submenus");
    let holder = send1i(main_menu, "itemAtIndex:", 0);
    let installed = send0(holder, "submenu");
    assert!(!installed.is_null(), "the first submenu is empty");
    let items = send0i(installed, "numberOfItems");
    assert_eq!(items, 1, "the submenu holds {items} items, not 1");
    let item = send1i(installed, "itemAtIndex:", 0);
    assert!(send_bool(item, "isEnabled"), "the menu item is disabled");
    // The same path AppKit takes when the user picks the item.
    send1iv(installed, "performActionForItemAtIndex:", 0);
    let fired = menu_fired.load(Ordering::SeqCst);
    println!(
        "probe: menu bar with {submenus} submenu(s), item enabled, choosing it fired {fired} time(s)"
    );
    assert_eq!(fired, 1, "choosing the menu item did not run its action");

    // --- Copy: the pasteboard holds what the action stored ---
    {
        let text = "/tmp/probe/copied-path.rs";
        rswidgets::compat::clipboard_set_text(text)?;
        let pasteboard = send0(
            rswidgets::backends::apple::cls("NSPasteboard"),
            "generalPasteboard",
        );
        let plain = rswidgets::backends::apple::nsstring("public.utf8-plain-text");
        let stored = send1p(pasteboard, "stringForType:", plain);
        let read_back = rswidgets::backends::apple::nsstring_to_rust(stored).unwrap_or_default();
        println!("probe: clipboard stored {text:?} and read back {read_back:?}");
        assert_eq!(
            read_back, text,
            "the pasteboard did not keep what Copy stored"
        );
    }

    // --- A single press selects; only a double press activates ---
    // The responder reads AppKit's `clickCount`; this drives the same dispatch
    // with counts 1 and 2, so the decision is tested, not just compiled.
    {
        let canvas = app.create_canvas()?;
        let presses = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let activations = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let p = presses.clone();
        canvas.on_click_button(Box::new(move |_, _, _, _| p.set(p.get() + 1)));
        let act = activations.clone();
        canvas.on_double_click(Box::new(move |_, _| act.set(act.get() + 1)));
        let id = rswidgets::backends_macos_adapter::Canvas(canvas.raw_handle()).canvas_id();
        rswidgets::backends_macos_adapter::dispatch_canvas_press(id, 10.0, 10.0, 1);
        let after_one = (presses.get(), activations.get());
        rswidgets::backends_macos_adapter::dispatch_canvas_press(id, 10.0, 10.0, 2);
        let after_two = (presses.get(), activations.get());
        println!(
            "probe: clickCount 1 -> presses {:?}, clickCount 2 -> presses {:?} (presses, activations)",
            after_one, after_two
        );
        assert_eq!(after_one, (1, 0), "one press must select without activating");
        assert_eq!(after_two, (2, 1), "the second press must activate");
    }

    // --- The row menu: four actions, each runs, Copy reaches the clipboard ---
    // Built and fired through the same menu the popup shows: `build` is the
    // popup's first step, and `fire` is AppKit's `performActionForItemAtIndex:`,
    // i.e. the target/action dispatch a click inside the menu runs.
    {
        list.select_row(1);
        let row_path = list.selected_path(2).expect("the list has no second row");
        let status = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let mut menu = rswidgets::compat::ContextMenu::new();
        let actions = [
            ff_gui::RowAction::Open,
            ff_gui::RowAction::OpenContainingFolder,
            ff_gui::RowAction::CopyPath,
            ff_gui::RowAction::OpenInTerminal,
        ];
        for action in actions {
            let status = status.clone();
            let path = row_path.clone();
            menu.append_item(
                action.label(),
                Box::new(move || {
                    let line = ff_gui::run_row_action(action, &path, |text| {
                        rswidgets::compat::clipboard_set_text(text).map_err(|e| e.to_string())
                    });
                    *status.borrow_mut() = line;
                }),
            );
        }
        let handle = menu.build().expect("the row menu could not be built");
        let items = rswidgets::compat::ContextMenu::items_of(handle);
        let labels: Vec<String> = items.iter().map(|(l, _)| l.clone()).collect();
        println!("probe: row menu items {items:?}");
        assert_eq!(items.len(), 4, "the row menu does not offer four actions");
        assert_eq!(
            labels,
            actions.iter().map(|a| a.label().to_string()).collect::<Vec<_>>(),
            "the row menu labels are not the four row actions"
        );
        for (label, enabled) in &items {
            assert!(*enabled, "the row menu item {label:?} is disabled");
        }
        let name = row_path.rsplit('/').next().unwrap_or(&row_path);
        for (index, action) in actions.iter().enumerate() {
            assert!(
                rswidgets::compat::ContextMenu::fire(handle, index),
                "{} could not be fired",
                action.label()
            );
            let line = status.borrow().clone();
            println!("probe: choosing {:?} ran {line:?}", action.label());
            if *action == ff_gui::RowAction::CopyPath {
                assert_eq!(
                    line,
                    format!("copied {name}"),
                    "Copy path reported {line:?}"
                );
                let read_back = rswidgets::compat::clipboard_get_text()
                    .expect("the clipboard holds no text after Copy path");
                println!("probe: clipboard now holds {read_back:?}");
                assert_eq!(
                    read_back, row_path,
                    "Copy path did not leave the row's path on the clipboard"
                );
            } else {
                assert!(
                    line.starts_with(action.label()),
                    "choosing {:?} ran {line:?}",
                    action.label()
                );
                assert!(
                    line.contains(name),
                    "choosing {:?} did not name the row: {line:?}",
                    action.label()
                );
            }
        }
        // The popup is modal, so headless CI cannot track it; what can be shown
        // is that the path no longer reports the feature missing — without an
        // event it fails on the event.
        let err = menu
            .popup_at_pointer(list.raw_handle(), std::ptr::null_mut())
            .err()
            .map(|e| format!("{e}"));
        println!("probe: popup with no event -> {err:?}");
        if let Some(e) = &err {
            assert!(
                !e.contains("not available") && !e.contains("not wired up"),
                "the popup still reports the feature missing: {e}"
            );
        }
    }

    // --- The scrollbar: knob geometry, and the scroll a drag drives ---
    //
    // Dragging the knob is AppKit's own tracking loop on a real NSScroller, so
    // what is checked here is that the scroller is live (target/action wired to
    // the scroll view) and that the scroll it drives moves the clip view and
    // the knob together.
    {
        let scroller = send0(sv, "verticalScroller");
        assert!(!scroller.is_null(), "the scroll view has no vertical scroller");
        let target = send0(scroller, "target");
        let action_sel = send0(scroller, "action");
        assert!(
            !target.is_null() && !action_sel.is_null(),
            "the scroller has no target/action, so a drag could not scroll anything"
        );
        let knob_before = send0d(scroller, "doubleValue");
        // The list is already at its end from the scrolling check above, so
        // this scrolls back up; 196 pt is the whole range (826-630).
        rswidgets::backends_macos_adapter::ScrolledWindow(sv).scroll_to(
            0.0, 0.0, 0.0, 50.0, 826.0, 630.0,
        );
        let scrolled_to = send_rect(send0(sv, "contentView"), "bounds").y;
        let knob_after = send0d(scroller, "doubleValue");
        println!(
            "probe: scroller wired to its scroll view, scroll moved the clip to {scrolled_to}pt (knob {knob_before:.3} -> {knob_after:.3})"
        );
        assert!(
            (scrolled_to - 50.0).abs() < 2.0,
            "the clip view moved to {scrolled_to}pt, not 50"
        );
        assert!(
            knob_after < knob_before,
            "the knob did not follow: {knob_before:.3} -> {knob_after:.3}"
        );
    }

    // --- Resizing reaches the window ---
    //
    // The frame is what `resize` owns and it changes synchronously. Reflowing
    // the stack inside it happens on AppKit's next layout pass in the run
    // loop, which this probe never runs (it drives AppKit's calls directly),
    // so the assertion stops at the frame.
    {
        let before_w = send_rect(win.raw_handle(), "frame").w;
        win.resize(1200, 700);
        send0v(win.raw_handle(), "layoutSubtreeIfNeeded");
        let after_w = send_rect(win.raw_handle(), "frame").w;
        let content = send_rect(holder_box.raw_handle(), "frame");
        // The window is pinned to its content (the capture size), so a resize
        // request is refused and there is no half-applied layout to catch.
        // Recorded rather than asserted: making the window resizable is an
        // app decision, not something this probe can do for it.
        println!(
            "probe: resize asked for 1200x700; window stayed {before_w}pt -> {after_w}pt and the content box is {:.0}x{:.0}pt, so the window is fixed-size",
            content.w, content.h
        );
        assert!(
            content.w > 0.0 && content.w <= after_w + 1.0,
            "the content box ({:.0}pt) escaped its {after_w}pt window",
            content.w
        );
    }

    let (killed, reaped) = daemon.stop();
    println!("probe: daemon stopped, killed {killed} and reaped {reaped}");
    assert!(killed && reaped, "the daemon did not stop cleanly");

    println!("probe: ok");
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    println!("macos_probe: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
