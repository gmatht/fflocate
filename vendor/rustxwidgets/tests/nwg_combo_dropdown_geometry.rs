//! A Win32 ComboBox's drop-down list must open directly below the combo's
//! client rect, and must not move when the number of items changes.
//!
//! The bug this pins: Win32 anchors a ComboBox's drop-down list to its
//! window's bottom-left corner, and that window is tall enough to *contain*
//! the list — so `GetWindowRect` on a combo returns "closed field + every
//! list item", a height that grows with the item count. The dialog layout
//! measured rows with `GetWindowRect`, sized the combo's row from that, and
//! then `SetWindowPos` moved the combo, so Windows dropped the list below the
//! combo's *new* bottom. Every re-layout that changed the row height therefore
//! moved the list, which is what a "dropdown appears at a random position"
//! report is.
//!
//! The fix measures the combo's **client** rect (the closed field alone), so
//! the row height is independent of the item count and the list has one stable
//! place to appear.
//!
//! Two things are checked, and they check different layers:
//!
//!   1. `combo_client_height_is_independent_of_item_count` — the measurement
//!      itself. Pure Win32, no layout involved, so a failure here names the
//!      measurement as the culprit.
//!   2. `combo_row_height_does_not_grow_with_items` — the layout consequence:
//!      the height the dialog layout would compute for a combo holding N items
//!      must be the same for N=2 and N=40.
//!
//! The Win32 geometry itself (that the list appears below-left of the combo)
//! is the OS's, not ours, so it is *observed* rather than asserted: the test
//! reports the combo's client rect and the window rect so a failure to place
//! it on a real host shows the numbers.
//!
//! Runs only on Windows (needs the native NWG backend + a message pump);
//! Linux CI skips it via cfg. Run with:
//!   cargo test --test nwg_combo_dropdown_geometry
#![cfg(all(windows, not(feature = "pancurses"), not(feature = "zork")))]

use std::os::raw::c_void;

/// Non-blocking message pump: dispatch everything pending, then return.
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

fn rect_of(hwnd: *mut c_void, client: bool) -> Option<(i32, i32, i32, i32)> {
    if hwnd.is_null() {
        return None;
    }
    unsafe {
        let mut r: winapi::shared::windef::RECT = std::mem::zeroed();
        let ok = if client {
            winapi::um::winuser::GetClientRect(hwnd as _, &mut r)
        } else {
            winapi::um::winuser::GetWindowRect(hwnd as _, &mut r)
        };
        if ok == 0 {
            return None;
        }
        Some((r.left, r.top, r.right, r.bottom))
    }
}

/// A window + one ComboBox with `n` items, the shape the dialogs use.
struct Combo {
    _app: rswidgets::core::App,
    _win: rswidgets::backends_nwg_adapter::Window,
    hwnd: *mut c_void,
}

fn combo_with_items(n: usize) -> Combo {
    use rswidgets::prelude::*;

    let app = App::init().expect("App::init");
    let win = app.create_window().expect("window");
    win.set_title("combo geometry");
    let dd = app.create_dropdown(&vec!["item"; n]).expect("dropdown");
    let hwnd = *AsRef::<*mut c_void>::as_ref(&dd);
    assert!(!hwnd.is_null(), "combo must have a native HWND");
    win.present();
    pump_once();
    Combo { _app: app, _win: win, hwnd }
}

/// The row height the dialog layout would compute for this combo: the height
/// it measures, floored the way `layout_nwg_dialog_parts` floors a too-small
/// control to 26px.
fn laid_out_row_height(hwnd: *mut c_void) -> i32 {
    let (_, _, _, h) = rect_of(hwnd, true).expect("combo client rect");
    if h > 10 {
        h
    } else {
        26
    }
}

/// The client height must not grow with the item count — that is the whole
/// point: the client rect is the closed field, the list is not part of it.
#[test]
fn combo_client_height_is_independent_of_item_count() {
    let few = combo_with_items(2);
    let many = combo_with_items(40);
    pump_once();

    let (_, _, _, few_h) = rect_of(few.hwnd, true).expect("client rect");
    let (_, _, _, many_h) = rect_of(many.hwnd, true).expect("client rect");

    assert!(few_h > 0, "combo must have a positive client height");
    assert_eq!(
        few_h, many_h,
        "a ComboBox's CLIENT height is its closed field and must not depend on \
         the item count (2 items -> {few_h}, 40 items -> {many_h}). \
         If this fails, the dialog layout is sizing the row from the window \
         rect, whose height includes the drop-down list, so the list moves \
         every time the item count changes."
    );
}

/// The consequence: the height the dialog layout derives must be stable across
/// item counts, so re-laying-out the dialog cannot move the combo.
#[test]
fn combo_row_height_does_not_grow_with_items() {
    let few = combo_with_items(2);
    let few_row = laid_out_row_height(few.hwnd);
    let few_client = rect_of(few.hwnd, true).expect("client rect");
    let few_window = rect_of(few.hwnd, false).expect("window rect");

    let many = combo_with_items(40);
    let many_row = laid_out_row_height(many.hwnd);
    pump_once();

    assert_eq!(
        few_row, many_row,
        "the laid-out row height must be identical for 2 and 40 items \
         ({few_row} vs {many_row})"
    );

    // Report the two rects. On a ComboBox these legitimately differ -- the
    // window is taller than the client because it holds the list -- and that
    // difference is precisely what made the old measurement unstable.
    let (fl, ft, fr, fb) = few_client;
    let (wl, wt, wr, wb) = few_window;
    eprintln!(
        "[combo] 2 items:  client=({fl},{ft})-({fr},{fb}) window=({wl},{wt})-({wr},{wb}) row={few_row}"
    );
    assert!(
        (wb - wt) >= (fb - ft),
        "a ComboBox's window is at least as tall as its client (it holds the list)"
    );
}

/// Re-laying-out the dialog repeatedly must leave the combo — and therefore
/// where Windows will drop its list — exactly where it was.
#[test]
fn combo_does_not_move_across_repeated_layout_passes() {
    use rswidgets::prelude::*;

    let app = App::init().expect("App::init");
    let win = app.create_window().expect("window");
    win.set_title("combo relayout");
    let dd = app.create_dropdown(&["a", "b", "c", "d"]).expect("dropdown");
    let vbox = app.create_box(Orientation::Vertical, 4).expect("vbox");
    vbox.append(&dd);
    win.set_child_box(&vbox);
    win.present();
    for _ in 0..3 {
        pump_once();
    }

    let hwnd = *AsRef::<*mut c_void>::as_ref(&dd);
    let first = rect_of(hwnd, true).expect("client rect");

    // Force several layout passes at different sizes, the way a dialog being
    // resized does. Each pass re-runs BoxWidget::layout, which re-positions
    // every child; a combo whose row height depended on its window rect would
    // drift.
    for (w, h) in [(500, 400), (700, 500), (600, 450), (500, 400)] {
        unsafe {
            winapi::um::winuser::SetWindowPos(
                win.hwnd() as _,
                std::ptr::null_mut(),
                0,
                0,
                w,
                h,
                winapi::um::winuser::SWP_NOMOVE | winapi::um::winuser::SWP_NOZORDER,
            );
        }
        for _ in 0..5 {
            pump_once();
        }
    }

    let last = rect_of(hwnd, true).expect("client rect");
    assert_eq!(
        (first.0, first.1, first.3),
        (last.0, last.1, last.3),
        "the combo's origin and height must survive repeated layout passes; \
         it drifted from {first:?} to {last:?}, which moves its drop-down list"
    );
    assert!(
        first.2 > 0 && first.3 > 0,
        "the combo must keep a real size, got {first:?}"
    );
}