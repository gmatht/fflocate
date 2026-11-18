//! Windows-only reproduction for the fx-bar cram: the address label, fx
//! label and formula entry pile up at the left instead of the entry
//! filling the bar.
//!
//! Runs only on Windows (needs the native NWG backend + a message pump;
//! Linux CI skips it via cfg). Build the same widget tree the app builds
//! (window -> vbox -> hbox formula bar [addr, fx, expanding entry]),
//! present it, then poll the native entry rect until it fills the bar or
//! a deadline passes. A persistently narrow entry reproduces the report
//! (fixed-small widgets crammed left, empty bar right); resizing or late
//! layouts heal transient states, so only a stable narrow rect fails.
//!
//! Run on Windows with: cargo test --test fx_bar_nwg_cram
#![cfg(all(windows, not(feature = "pancurses"), not(feature = "zork")))]


use std::time::{Duration, Instant};

/// Non-blocking message pump: dispatch everything pending, then return
/// (unlike nwg::dispatch_thread_events, which runs until quit and would
/// hang the test). Lets posted cascades (e.g. the label set_text nudge)
/// land between settle polls.
fn pump_once() {
    unsafe {
        let mut msg: winapi::um::winuser::MSG = std::mem::zeroed();
        while winapi::um::winuser::PeekMessageW(
            &mut msg,
            std::ptr::null_mut(),
            0,
            0,
            winapi::um::winuser::PM_REMOVE,
        ) != 0
        {
            winapi::um::winuser::TranslateMessage(&msg);
            winapi::um::winuser::DispatchMessageW(&msg);
        }
    }
}

fn rect_of(hwnd: *mut std::os::raw::c_void) -> Option<(i32, i32, i32, i32)> {
    if hwnd.is_null() {
        return None;
    }
    unsafe {
        let mut rect: winapi::shared::windef::RECT = std::mem::zeroed();
        if winapi::um::winuser::GetWindowRect(hwnd as _, &mut rect) == 0 {
            return None;
        }
        Some((rect.left, rect.top, rect.right, rect.bottom))
    }
}

#[test]
fn fx_bar_entry_fills_remainder_no_overlap() {
    use rswidgets::prelude::*;
    use std::os::raw::c_void;

    let app = App::init().expect("App::init");
    let win = app.create_window().expect("window");
    win.set_title("fx cram repro");
    // App order: default size immediately after creation (before children).
    win.set_default_size(1200, 800);
    let vbox = app
        .create_box(Orientation::Vertical, 2)
        .expect("root vbox");
    let bar = app
        .create_box(Orientation::Horizontal, 2)
        .expect("formula bar");
    let addr = app.create_label("A1").expect("addr label");
    let fx = app.create_label("  fx  ").expect("fx label");
    let entry = app.create_entry().expect("formula entry");
    entry.set_hexpand(true);
    bar.append(&addr);
    bar.append(&fx);
    bar.append(&entry);
    bar.set_child_hexpand(&entry, true);
    vbox.append(&bar);
    win.set_child_box(&vbox);
    // Siblings like the real app (status label + hidden tab strip): they
    // share the root box and must not disturb the formula bar's layout.
    let status = app.create_label("Ready").expect("status label");
    vbox.append(&status);
    win.present();
    eprintln!("[repro] presented");

    let entry_hwnd = *AsRef::<*mut c_void>::as_ref(&entry);
    let bar_hwnd = *AsRef::<*mut c_void>::as_ref(&bar);
    let addr_hwnd = *AsRef::<*mut c_void>::as_ref(&addr);
    assert!(!entry_hwnd.is_null(), "entry must have a native HWND");
    assert!(!bar_hwnd.is_null(), "formula bar must have a native HWND");

    // Settle-poll: a late layout pass heals transient states; only a
    // stably narrow entry (the reported cram) fails the deadline.
    // Stage 1: initial layout.
    eprintln!("[repro] stage 1: initial layout");
    let (ex0, ex1, bw) = poll_fill(bar_hwnd, entry_hwnd, addr_hwnd, "initial");
    eprintln!("[repro] stage 1 done: entry=({ex0},{ex1}) bar={bw}");
    // Entry starts right of the labels and reaches near the right edge.
    assert!(
        ex0 >= 0 && ex0 < 200,
        "entry must start right after the labels (x0: {ex0})"
    );
    assert!(
        ex1 >= bw - 20,
        "entry must reach near the bar edge, not cram left (x1: {ex1}, bar: {bw})"
    );

    // Stage 2: label text change (the app rewrites the address on every
    // cursor move; Label::set_text nudges a parent re-layout). The entry
    // must still fill afterwards.
    addr.set_text("[A1]");
    eprintln!("[repro] stage 2: after set_text");
    let (ex0b, ex1b, bw2) = poll_fill(bar_hwnd, entry_hwnd, addr_hwnd, "after set_text");
    eprintln!("[repro] stage 2 done");
    assert!(
        ex1b >= bw2 - 20,
        "entry must still fill after label text change (x1: {ex1b}, bar: {bw2})"
    );
    let _ = (ex0b, bw);

    // Stage 3: top-level resize (exercises the WM_SIZE -> root layout
    // cascade with different client widths, both directions).
    let win_hwnd = *AsRef::<*mut c_void>::as_ref(&win);
    for (w, h) in [(1000, 700), (500, 300)] {
        eprintln!("[repro] stage 3: resize to {w}x{h}");
        unsafe {
            winapi::um::winuser::SetWindowPos(
                win_hwnd as _,
                std::ptr::null_mut(),
                0,
                0,
                w,
                h,
                winapi::um::winuser::SWP_NOMOVE | winapi::um::winuser::SWP_NOZORDER,
            );
        }
        let (rx0, rx1, rbw) = poll_fill(bar_hwnd, entry_hwnd, addr_hwnd, "after resize");
        assert!(
            rx1 >= rbw - 20,
            "entry must fill after resize to {w}x{h} (x1: {rx1}, bar: {rbw})"
        );
        assert!(
            rx0 >= 0 && rx0 < 200,
            "entry must stay right of labels after resize (x0: {rx0})"
        );
    }
}

/// Poll until the entry fills its bar (see main test); panics with geometry
/// on deadline. Shared by all stages so each reports where fill broke.
fn poll_fill(
    bar_hwnd: *mut std::os::raw::c_void,
    entry_hwnd: *mut std::os::raw::c_void,
    addr_hwnd: *mut std::os::raw::c_void,
    stage: &str,
) -> (i32, i32, i32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        pump_once();
        let Some((bl, _bt, br, _bb)) = rect_of(bar_hwnd) else {
            panic!("formula bar has no rect ({stage})");
        };
        let bar_w = br - bl;
        if let Some((x0, x1)) = rect_of(entry_hwnd).map(|(l, _t, r, _b)| (l - bl, r - bl)) {
            if x1 - x0 > bar_w / 2 && x0 >= 0 {
                return (x0, x1, bar_w);
            }
        }
        if Instant::now() > deadline {
            let entry_rel =
                rect_of(entry_hwnd).map(|(l, _t, r, _b)| (l - bl, r - bl));
            let addr_rel =
                rect_of(addr_hwnd).map(|(l, _t, r, _b)| (l - bl, r - bl));
            panic!(
                "fx entry never filled the bar {stage} (cram reproduced): bar_w={bar_w} entry={entry_rel:?} addr={addr_rel:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
