//! The scrollbar knob and a resize, driven through AppKit itself.
//!
//! The sibling of `tests/list_interaction_mac.rs` for the two parts of the list
//! that are AppKit's rather than compat's: the scroller inside the scroll view,
//! and the reflow a window resize forces.
//!
//! On this backend the results list is a `CorroSheetView` inside a real
//! `NSScrollView` with a real `NSScroller`, and a knob drag is AppKit's own
//! tracking loop. So nothing here calls a Rust scroll handler: the test posts
//! the same `NSEvents` a mouse produces (`leftMouseDragged` steps and a
//! `leftMouseUp`, queued *before* the press so the tracking loop finds them)
//! and delivers the `leftMouseDown` with `sendEvent:` to the window, which is
//! the authoritative route into AppKit. Where the knob is comes from
//! `-rectForPart:` (the same rect the user sees), the viewport and offset come
//! from the clip view's `bounds`, and the rest is read back through
//! `compat::reflow`'s geometry, which is derived from the painter the canvas
//! runs.
//!
//! AppKit needs the main thread, so this target has `harness = false` and its
//! own `fn main` — see `[[test]]` in `Cargo.toml`.

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::c_void;

    use rswidgets::compat::CellValue;
    use rswidgets::prelude::*;

    // --- The objc sends this test needs ---
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Point {
        x: f64,
        y: f64,
    }
    fn send0(obj: *mut c_void, sel: &str) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send0v(obj: *mut c_void, sel: &str) {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send1v(obj: *mut c_void, sel: &str, arg: *mut c_void) {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel), arg) }
    }
    fn send0i(obj: *mut c_void, sel: &str) -> isize {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send_rect(obj: *mut c_void, sel: &str) -> Rect {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> Rect =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send_rect_i(obj: *mut c_void, sel: &str, arg: isize) -> Rect {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, isize) -> Rect =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel), arg) }
    }
    fn send_rect_to_view(obj: *mut c_void, sel: &str, r: Rect, view: *mut c_void) -> Rect {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, Rect, *mut c_void) -> Rect =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel), r, view) }
    }
    fn send_event_flag(app: *mut c_void, sel: &str, ev: *mut c_void, flag: bool) {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, bool) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(app, rswidgets::backends::apple::selector(sel), ev, flag) }
    }
    /// `+[NSEvent mouseEventWithType:...]`: `kind` is the NSEventType
    /// (1 down, 6 dragged, 2 up), `p` is in window coordinates.
    fn make_event(cls: *mut c_void, kind: usize, p: Point, window_number: isize) -> *mut c_void {
        type F = unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            usize,
            Point,
            usize,
            f64,
            isize,
            *mut c_void,
            isize,
            isize,
            f32,
        ) -> *mut c_void;
        let f: F = unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe {
            f(
                cls,
                rswidgets::backends::apple::selector(
                    "mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:",
                ),
                kind,
                p,
                0,
                0.0,
                window_number,
                std::ptr::null_mut(),
                0,
                1,
                1.0,
            )
        }
    }

    // --- A list with more real files than fit ---
    let app = App::init()?;
    let win = app.create_window()?;
    win.set_title("ff-gui drag and reflow test");
    let list = app.create_treeview(&[("", true, true), ("Name", false, true), ("Path", false, true)])?;
    let dir = tempfile::tempdir()?;
    let count = 40;
    let mut rows = Vec::new();
    for i in 0..count {
        let name = format!("file-{i:02}.rs");
        let path = dir.path().join(&name);
        std::fs::write(&path, "drag fixture\n")?;
        rows.push(vec![
            CellValue::Icon {
                name: "text-x-generic".to_string(),
                fallback: "-".to_string(),
            },
            CellValue::Text(name),
            CellValue::Text(path.to_string_lossy().into_owned()),
        ]);
    }
    list.set_rows(&rows);

    let painted = list.clone().into_scrolled()?;
    let column = app.create_box(Orientation::Vertical, 2)?;
    column.append(&painted);
    win.set_child(&column);
    win.present();
    let window = win.raw_handle();
    let _ = rswidgets::backends_macos_adapter::capture(window, Some((900.0, 240.0)));
    send0v(window, "layoutSubtreeIfNeeded");

    let scroll_view = painted.inner.raw_handle();
    let clip = send0(scroll_view, "contentView");
    assert!(!clip.is_null(), "the scroll view has no clip view");
    let viewport = || send_rect(clip, "bounds");
    let canvas_frame = || send_rect(list.raw_handle(), "frame");
    let scroller = send0(scroll_view, "verticalScroller");
    assert!(!scroller.is_null(), "the scroll view has no vertical scroller");
    let knob = || {
        // NSScrollerKnob is part 2; window coordinates are the space the
        // synthesized NSEvents are in.
        send_rect_to_view(
            scroller,
            "convertRect:toView:",
            send_rect_i(scroller, "rectForPart:", 2),
            std::ptr::null_mut(),
        )
    };

    let v0 = viewport();
    let f0 = canvas_frame();
    let geo0 = list.layout(f0.w, v0.h, v0.y);
    assert!(
        geo0.content_height > geo0.viewport_height + 1.0,
        "{count} rows are not taller than the {}pt viewport",
        geo0.viewport_height
    );
    assert!(
        geo0.thumb_height > 0.0 && geo0.thumb_height < geo0.track_height,
        "the layout says the knob is {}pt in a {}pt track",
        geo0.thumb_height,
        geo0.track_height
    );
    let k0 = knob();
    assert!(
        k0.w > 0.0 && k0.h > 0.0,
        "the scroller would not say where its knob is: {}x{}",
        k0.w,
        k0.h
    );
    // The knob the user can grab and the knob the painter derives have to be
    // the same one, or a drag is aimed at something that is not there.
    assert!(
        (k0.h - geo0.thumb_height).abs() <= 3.0,
        "AppKit's knob is {}pt tall but the layout derived {}pt",
        k0.h,
        geo0.thumb_height
    );
    println!(
        "drag: canvas {:.0}x{:.0}, viewport {:.0}pt, knob {:.0}x{:.0}pt at ({:.0}, {:.0}), {count} rows",
        f0.w, f0.h, v0.h, k0.w, k0.h, k0.x, k0.y
    );

    // --- The knob, dragged the way a mouse drags it ---
    let centre = Point {
        x: k0.x + k0.w / 2.0,
        y: k0.y + k0.h / 2.0,
    };
    let drag = 20.0f64;
    let window_number = send0i(window, "windowNumber");
    let event_cls = rswidgets::backends::apple::cls("NSEvent");
    let app_obj = send0(
        rswidgets::backends::apple::cls("NSApplication"),
        "sharedApplication",
    );
    const PRESSED: usize = 1;
    const RELEASED: usize = 2;
    const DRAGGED: usize = 6;
    const STEPS: usize = 6;
    // Queue the motion and the release first: the scroller tracks the mouse in
    // its own loop, and events posted after the press would never be seen.
    for step in 1..=STEPS {
        let p = Point {
            x: centre.x,
            y: centre.y - drag * step as f64 / STEPS as f64,
        };
        let ev = make_event(event_cls, DRAGGED, p, window_number);
        send_event_flag(app_obj, "postEvent:atStart:", ev, false);
    }
    let end = Point {
        x: centre.x,
        y: centre.y - drag,
    };
    let up = make_event(event_cls, RELEASED, end, window_number);
    send_event_flag(app_obj, "postEvent:atStart:", up, false);
    let down = make_event(event_cls, PRESSED, centre, window_number);
    send1v(window, "sendEvent:", down);

    let v1 = viewport();
    let f1 = canvas_frame();
    let geo1 = list.layout(f1.w, v1.h, v1.y);
    let k1 = knob();
    // How far the offset should have moved: the knob travels the track minus
    // its own length, and the offset covers the content minus the viewport.
    let travel = (geo0.track_height - geo0.thumb_height).max(1.0);
    let expected = drag * (geo0.content_height - geo0.viewport_height) / travel;
    println!(
        "drag: dragged the knob {drag:.0}pt down -> knob ({:.0}, {:.0}) -> ({:.0}, {:.0}); offset {:.0} -> {:.0}; first row {} -> {} ({:?})",
        k0.x,
        k0.y,
        k1.x,
        k1.y,
        v0.y,
        v1.y,
        geo0.first_row,
        geo1.first_row,
        list.cell_text(geo1.first_row, 1).unwrap_or_default()
    );
    assert!(
        v1.y > v0.y + 1.0,
        "dragging the knob left the clip at {:.0}pt (was {:.0}): the drag did not scroll",
        v1.y,
        v0.y
    );
    let moved = v1.y - v0.y;
    assert!(
        moved >= expected * 0.5 && moved <= expected * 2.0,
        "a {drag:.0}pt drag moved the offset {moved:.0}pt; the knob's travel implies {expected:.0}pt"
    );
    assert!(
        v1.y <= geo0.content_height - geo0.viewport_height + 1.0,
        "the offset {:.0}pt is past the end of {}pt of content",
        v1.y,
        geo0.content_height
    );
    assert!(
        geo1.first_row > geo0.first_row,
        "the first drawn row did not move (row {} before and after)",
        geo0.first_row
    );
    let shown = list.cell_text(geo1.first_row, 1).unwrap_or_default();
    assert_eq!(
        shown,
        format!("file-{:02}.rs", geo1.first_row),
        "the row drawn first after the drag is {shown:?}, not the one at offset {:.0}pt",
        v1.y
    );
    assert!(
        k1.y < k0.y - 1.0,
        "the knob did not follow the drag: y {:.0}pt then {:.0}pt",
        k0.y,
        k1.y
    );
    assert!(
        (k1.h - geo1.thumb_height).abs() <= 3.0,
        "after the drag AppKit's knob is {}pt but the layout derived {}pt",
        k1.h,
        geo1.thumb_height
    );

    // --- A drag that starts outside the knob does not scroll ---
    //
    // Same events, aimed at the rows: a press in the canvas is a selection, not
    // a scrollbar drag, and the offset must not move at all.
    let outside = Point {
        x: f1.x + 80.0,
        y: f1.y + 140.0,
    };
    for step in 1..=4 {
        let p = Point {
            x: outside.x,
            y: outside.y - 30.0 * step as f64 / 4.0,
        };
        let ev = make_event(event_cls, DRAGGED, p, window_number);
        send_event_flag(app_obj, "postEvent:atStart:", ev, false);
    }
    let up = make_event(
        event_cls,
        RELEASED,
        Point {
            x: outside.x,
            y: outside.y - 30.0,
        },
        window_number,
    );
    send_event_flag(app_obj, "postEvent:atStart:", up, false);
    let down = make_event(event_cls, PRESSED, outside, window_number);
    send1v(window, "sendEvent:", down);
    let v2 = viewport();
    println!(
        "drag: a 30pt drag started outside the knob at ({:.0}, {:.0}) -> offset {:.0} (unchanged)",
        outside.x, outside.y, v2.y
    );
    assert!(
        (v2.y - v1.y).abs() <= 0.5,
        "a drag that started outside the knob scrolled {:.0}pt -> {:.0}pt",
        v1.y,
        v2.y
    );

    // --- A resize reflows the list onto the new size ---
    let _ = rswidgets::backends_macos_adapter::capture(window, Some((640.0, 430.0)));
    send0v(window, "layoutSubtreeIfNeeded");
    let v3 = viewport();
    let f3 = canvas_frame();
    let geo3 = list.layout(f3.w, v3.h, v3.y);
    let k3 = knob();
    let render0 = list.render_offscreen(f0.w, v0.h, v0.y);
    let render3 = list.render_offscreen(f3.w, v3.h, v3.y);
    println!(
        "reflow: resized {:.0}x{:.0} -> {:.0}x{:.0}: canvas {:.0} -> {:.0}, viewport {:.0} -> {:.0}, visible rows {}/{} -> {}/{}, knob {:.0} -> {:.0}pt, header x {:?} -> {:?}",
        f0.w,
        f0.h,
        f3.w,
        f3.h,
        f0.w,
        f3.w,
        v0.h,
        v3.h,
        geo0.visible_rows,
        count,
        geo3.visible_rows,
        count,
        k0.h,
        k3.h,
        render0.header_xs.iter().map(|x| x.round()).collect::<Vec<_>>(),
        render3.header_xs.iter().map(|x| x.round()).collect::<Vec<_>>()
    );
    assert!(
        f3.w < f0.w - 1.0 && v3.h > v0.h + 1.0,
        "the resize did not reach the list: canvas {:.0} -> {:.0}, viewport {:.0} -> {:.0}",
        f0.w,
        f3.w,
        v0.h,
        v3.h
    );
    assert!(
        (geo3.width - f3.w).abs() <= 1.0,
        "the layout is {}pt wide but the canvas is {:.0}pt",
        geo3.width,
        f3.w
    );
    assert!(
        (geo3.viewport_height - v3.h).abs() <= 1.0,
        "the layout uses a {}pt viewport but the clip is {:.0}pt tall",
        geo3.viewport_height,
        v3.h
    );
    assert!(
        geo3.visible_rows > geo0.visible_rows,
        "the taller viewport shows {} rows, not more than the {} it showed",
        geo3.visible_rows,
        geo0.visible_rows
    );
    assert_eq!(
        geo3.row_rects.len(),
        geo3.visible_rows,
        "the row bands and the visible count disagree"
    );
    assert_ne!(
        render3.header_xs, render0.header_xs,
        "the header columns did not move with the width"
    );
    let ratio = f3.w / f0.w;
    for (old, new) in render0.header_xs.iter().zip(render3.header_xs.iter()) {
        assert!(
            (new - old * ratio).abs() <= 6.0,
            "a header title stayed at {new:.0}pt after the resize; the new width implies {:.0}pt",
            old * ratio
        );
    }
    // The knob re-derives itself from the new track: a taller viewport is a
    // bigger share of the content, so the thumb grows and still agrees with the
    // layout's own geometry.
    assert!(
        k3.h > k0.h + 1.0,
        "the knob did not grow with the taller viewport: {:.0}pt -> {:.0}pt",
        k0.h,
        k3.h
    );
    assert!(
        (k3.h - geo3.thumb_height).abs() <= 3.0,
        "after the resize AppKit's knob is {:.0}pt but the layout derived {:.0}pt",
        k3.h,
        geo3.thumb_height
    );

    println!("drag: ok — knob dragged, offset and rows followed, outside press held still");
    println!("reflow: ok — rows and columns reflowed, knob re-derived from the new track");
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    // AppKit's scroller and window resizing are the subject; the GTK counterparts
    // are `tests/reflow_gtk.rs` and `tests/scroll_gtk.rs`.
    println!("drag_reflow_mac: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
