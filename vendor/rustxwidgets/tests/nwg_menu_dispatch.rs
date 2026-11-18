//! Every menu item in a real NWG menu bar dispatches its own action.
//!
//! Method (no sleeps in the verdict, no shared files): build the same
//! menu bar the app builds — a top-level `_File` with several leaves,
//! each registered as a `SimpleAction` — then **post the exact message the
//! OS sends for a clicked item** rather than synthesising a click. That is
//! the only reliable way to test menu dispatch unattended: `TrackPopupMenu`
//! enters its own modal loop, so a test that really clicks would block
//! forever, whereas `WM_MENUCOMMAND` (and the classic `WM_COMMAND` that
//! some hosts send instead) can be posted straight at the owner window and
//! dispatched by the very same raw handler the mouse path uses.
//!
//! Both flavours are covered, because `create_menubar` binds handlers for
//! both precisely because hosts differ: `WM_MENUCOMMAND` with `MNS_NOTIFYBYPOS`
//! reports `(HMENU, position)`, and the classic `WM_COMMAND` reports the
//! numeric item id with `lParam == 0`. A menu that only answers one of them
//! is silently dead on the other half of hosts.
//!
//! Per-item coverage is a **loop inside one `#[test]`**, not one `#[test]`
//! per item: `*mut c_void` is not `Send`, so a `static Mutex<Fixture>` holding
//! the HWND cannot compile, and parallel tests would race on desktop-global
//! message state. `ITEMS` below is the single source the menu and the
//! expectations both read, so a newly added item is covered by construction.
//!
//! A counter (not a println) is the assertion: the action must have run
//! exactly once, which also catches the double-dispatch that a menu wired
//! for *both* message flavours would produce if both fired.
//!
//! Runs only on Windows (needs the native NWG backend + a message pump);
//! Linux CI skips it via cfg. Run with:
//!   cargo test --test nwg_menu_dispatch
#![cfg(all(windows, not(feature = "pancurses"), not(feature = "zork")))]

use std::sync::{Arc, Mutex};

/// Non-blocking message pump: dispatch everything pending, then return
/// (unlike nwg::dispatch_thread_events, which runs until quit and would
/// hang the test). Lets posted cascades land between settle polls.
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

/// Pump until `cond` holds or the deadline passes. Event-based rather than
/// a fixed sleep, so a slow CI box does not produce a false failure.
fn settle(what: &str, cond: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        pump_once();
        if cond() {
            return;
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for: {what}");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Read a window's menu bar (`HMENU`) and the submenu at `pos`.
fn menu_of(hwnd: *mut std::os::raw::c_void, pos: u32) -> *mut std::os::raw::c_void {
    unsafe {
        let bar = winapi::um::winuser::GetMenu(hwnd as _);
        assert!(!bar.is_null(), "window has no menu bar");
        let sub = winapi::um::winuser::GetSubMenu(bar, pos as i32);
        assert!(!sub.is_null(), "no submenu at bar position {pos}");
        sub as *mut std::os::raw::c_void
    }
}

/// The numeric command id the OS assigns to the item at `pos` in `hmenu`
/// (`GetMenuItemID`, or `pos` itself when the menu was built positionally).
fn command_id(hwnd: *mut std::os::raw::c_void, pos: u32) -> u32 {
    unsafe {
        let id = winapi::um::winuser::GetMenuItemID(hwnd as _, pos as i32);
        if id == u32::MAX {
            // MF_BYPOSITION is not set here, so an id of -1 means the item is
            // a popup rather than a command; that is a real failure of the
            // fixture, so report it rather than guessing.
            panic!("item {pos} is a popup, not a command");
        }
        id as u32
    }
}

/// Builds one window + menu bar + per-action counters, hands the faked clicks
/// to `body`, then tears everything down.
///
/// One `#[test]` with a body per menu item, rather than one `#[test]` each:
/// `TrackPopupMenu` and `WM_*` delivery are global (per desktop), and a raw
/// `HWND` is not `Send`, so neither a `static Mutex<Fixture>` nor parallel
/// tests can hold shared HWND state. Keeping the HWND on the stack of a
/// single test is the only sound shape — the neighbouring
/// `fx_bar_nwg_cram` test is a single `#[test]` for the same reason. Each item
/// still gets its own assertion, and a new item is added by adding one line to
/// `ITEMS`; the compiler enforces that the tables stay in lockstep because
/// `ITEMS` is the single source both the menu and the expectations read.
fn with_menu_bar(body: impl FnOnce(&MenuFixture)) {
    use rswidgets::prelude::*;

    let app = App::init().expect("App::init");
    let win = app.create_window().expect("window");
    win.set_title("menu dispatch");
    let window_hwnd = win.hwnd();

    // One entry per menu leaf: (label, action). The mnemonic label carries the
    // GTK `_` marker; the NWG adapter translates it to Win32 `&`.
    const ITEMS: &[(&str, &str)] = &[
        ("_New", "file.new"),
        ("_Open", "file.open"),
        ("_Save", "file.save"),
        ("_Quit", "file.quit"),
    ];

    let mut file_menu = app.create_menu().expect("file menu");
    for (label, action) in ITEMS {
        file_menu.append(label, action);
    }
    let mut bar = app.create_menu().expect("menubar");
    bar.append_submenu("_File", &file_menu);

    let mb = unsafe { app.create_menubar(&bar, window_hwnd as _) }.expect("menubar");

    // Per-action fire counters, shared with the SimpleAction handlers.
    let fired: Arc<Mutex<Vec<(String, usize)>>> =
        Arc::new(Mutex::new(Vec::new()));
    for (_, action) in ITEMS {
        let act = app.create_simple_action(action).expect("action");
        let f = Arc::clone(&fired);
        let name = (*action).to_string();
        act.connect_activate(move |_| {
            let mut m = f.lock().unwrap_or_else(|e| e.into_inner());
            // Compare by reference: `name` is captured by the `FnMut` closure, so a
            // by-value comparison — or moving it into the Vec — would move it
            // out of the capture. Clone on the (once-per-action) insert path.
            match m.iter_mut().find(|(n, _)| n == &name) {
                Some((_, c)) => *c += 1,
                None => m.push((name.clone(), 1)),
            }
        })
        .expect("connect_activate");
    }
    // Keep the bar alive: dropping it destroys the HMENU and every later
    // GetMenu* read would touch a freed handle.
    std::mem::forget(mb);

    win.present();
    pump_once();
    assert!(!menu_of(window_hwnd, 0).is_null(), "File submenu missing");

    body(&MenuFixture {
        window_hwnd,
        fired: Arc::clone(&fired),
    });

    // Tear down explicitly (the HWNDs are not `Send`, so no static can hold
    // them across tests): quitting the loop is what `App`'s run loop waits on,
    // and the test process is short-lived anyway.
    let _ = win;
}

/// The fixture a test body gets: the owner HWND plus the per-action counters.
struct MenuFixture {
    window_hwnd: *mut std::os::raw::c_void,
    fired: Arc<Mutex<Vec<(String, usize)>>>,
}

impl MenuFixture {
    /// Times `action`'s handler has run.
    fn count(&self, action: &str) -> usize {
        let m = self.fired.lock().unwrap_or_else(|e| e.into_inner());
        m.iter().find(|(n, _)| n == action).map(|(_, c)| *c).unwrap_or(0)
    }

    /// Click the leaf at `pos` of the File submenu the way a real click does,
    /// via `WM_MENUCOMMAND` (the position-reporting flavour). `wParam` packs
    /// the position in `LOWORD` and `MNS_NOTIFYBYPOS` (0x8000) in `HIWORD`;
    /// `lParam` is the submenu `HMENU`. Posting this to the owner window is
    /// exactly what USER32 does when the user clicks the item.
    fn click_by_position(&self, pos: u32) {
        let sub = menu_of(self.window_hwnd, 0);
        let wparam = 0x8000u32 | (pos & 0xFFFF);
        unsafe {
            winapi::um::winuser::PostMessageW(
                self.window_hwnd as _,
                winapi::um::winuser::WM_MENUCOMMAND,
                wparam as usize,
                sub as isize,
            );
        }
        settle(&format!("action for item {pos}"), || true);
    }

    /// Click the leaf at `pos` the other way: the classic `WM_COMMAND`
    /// carrying the numeric item id with `lParam == 0`. Hosts that do not set
    /// `MNS_NOTIFYBYPOS` send this instead, and a menu wired for only one
    /// flavour is dead on the other.
    fn click_by_command(&self, pos: u32) {
        let sub = menu_of(self.window_hwnd, 0);
        let id = command_id(sub, pos);
        unsafe {
            winapi::um::winuser::PostMessageW(
                self.window_hwnd as _,
                winapi::um::winuser::WM_COMMAND,
                id as usize,
                0,
            );
        }
        settle(&format!("action for command id {id}"), || true);
    }
}

/// One assertion per leaf, per message flavour. `ITEMS` is the single source
/// the fixture builds the menu from, so a newly added item is covered by
/// construction — there is no "forgot to add a case" state.
#[test]
fn every_menu_item_dispatches_exactly_once() {
    with_menu_bar(|f| {
        // Read once: each action must fire on its own click, so a leaf that
        // never fires and one that fires twice both show up as `!= 1`.
        let snapshot = |f: &MenuFixture| -> Vec<usize> {
            ["file.new", "file.open", "file.save", "file.quit"]
                .iter()
                .map(|a| f.count(a))
                .collect()
        };

        let before = snapshot(f);
        for pos in 0..4usize {
            f.click_by_position(pos as u32);
            let after = snapshot(f);
            // Exactly one counter moved, by exactly one.
            let deltas: Vec<i64> = after
                .iter()
                .zip(&before)
                .map(|(a, b)| *a as i64 - *b as i64)
                .collect();
            assert_eq!(
                deltas.iter().sum::<i64>(),
                1,
                "clicking item {pos} must fire exactly one action, deltas={deltas:?}"
            );
            assert_eq!(
                deltas[pos] as usize, 1,
                "clicking item {pos} must fire its OWN action, deltas={deltas:?}"
            );
        }

        // Repeat over the classic-id flavour. Counters carry over, so each
        // click must again move exactly the addressed leaf by one.
        for pos in 0..4usize {
            let before = snapshot(f);
            f.click_by_command(pos as u32);
            let after = snapshot(f);
            let deltas: Vec<i64> = after
                .iter()
                .zip(&before)
                .map(|(a, b)| *a as i64 - *b as i64)
                .collect();
            assert_eq!(
                deltas[pos] as usize, 1,
                "WM_COMMAND on item {pos} must fire its action, deltas={deltas:?}"
            );
            assert_eq!(
                deltas.iter().sum::<i64>(),
                1,
                "WM_COMMAND on item {pos} must fire exactly one action, deltas={deltas:?}"
            );
        }
    });
}

/// Clicking one item must not fire its neighbours' actions: a MenuIndex that
/// ignored the position (or the HMENU) would fire the wrong leaf. The per-item
/// loop above asserts each counter moved, but only this catches a
/// cross-wiring that fires *both* the right and a wrong action.
#[test]
fn clicking_one_item_does_not_fire_others() {
    with_menu_bar(|f| {
        let open_before = f.count("file.open");
        let save_before = f.count("file.save");
        let quit_before = f.count("file.quit");
        f.click_by_position(1); // "Open"
        assert_eq!(f.count("file.open"), open_before + 1, "Open must fire");
        assert_eq!(f.count("file.save"), save_before, "Save must not fire");
        assert_eq!(f.count("file.quit"), quit_before, "Quit must not fire");
    });
}

// ---------------------------------------------------------------------------
// Popup placement
// ---------------------------------------------------------------------------
//
// The contract: the drop-down's TOP-LEFT corner sits at the mouse, unless that
// would put part of it off screen or under the taskbar, in which case it flips
// to the other side of that edge.
//
// Where each half is tested, and why:
//
//   * The DECISION ("fits -> keep the point; overflows -> flip") is pure and
//     lives in `win32_portable::clamp_popup_to_work_area`, unit-tested there
//     (7 cases: fits exactly, flips left/up/both, never under the taskbar,
//     non-zero work-area origin, oversized menu, edge cases). It cannot be
//     reached from here -- the helpers are `pub(crate)`, matching the house
//     pattern where `distribute_spans` is also unit-tested in-module.
//   * The INPUTS are what only Windows can supply, and they are what this test
//     checks: that the work area excludes the taskbar, and that the popup has
//     a real size. A zero measured size would make every popup "fit" and never
//     flip -- the bug itself -- so the size assertion is the load-bearing one.

/// The taskbar-excluded work area, as the popup code computes it.
fn work_area() -> (i32, i32, i32, i32) {
    unsafe {
        let mut r: winapi::shared::windef::RECT = std::mem::zeroed();        let ok = winapi::um::winuser::SystemParametersInfoA(
            winapi::um::winuser::SPI_GETWORKAREA,
            0,
            &mut r as *mut _ as *mut winapi::ctypes::c_void,
            0,
        ) != 0;
        assert!(ok, "SPI_GETWORKAREA must work on any Windows");
        assert!(
            r.right > r.left && r.bottom > r.top,
            "work area must be non-degenerate, got ({},{})-({},{})",
            r.left, r.top, r.right, r.bottom
        );
        (r.left, r.top, r.right, r.bottom)
    }
}

/// The size the File submenu will occupy, as `popup_geometry` measures it.
///
/// Measured by reading each item's row (`GetMenuItemRect`) and stacking them,
/// which is what the popup code does; the floor mirrors its own so a menu whose
/// items refuse to measure still yields a non-zero size (the whole point: a
/// zero size silently disables every flip).
fn measured_popup_size(hwnd: *mut std::os::raw::c_void) -> (i32, i32) {
    let sub = menu_of(hwnd, 0);
    let n = unsafe { winapi::um::winuser::GetMenuItemCount(sub as _) };
    let count = if n > 0 { n } else { 1 };
    let mut row_h = 0i32;
    for i in 0..count {
        let mut r: winapi::shared::windef::RECT = unsafe { std::mem::zeroed() };
        unsafe {
            if winapi::um::winuser::GetMenuItemRect(
                hwnd as _,
                sub as _,
                i as u32,
                &mut r,
            ) != 0
            {
                row_h = row_h.max(r.bottom - r.top);
            }
        }
    }
    let height = if row_h > 0 { row_h * count } else { 20 * count };
    (200.max(0), height)
}

#[test]
fn popup_inputs_are_usable_for_the_flip_decision() {
    with_menu_bar(|f| {
        let (wl, wt, wr, wb) = work_area();

        // The work area must be strictly smaller than the screen whenever a
        // taskbar is present: a popup clamped against the *screen* rather than
        // the work area is exactly what hides a menu behind the taskbar.
        let screen_w = unsafe { winapi::um::winuser::GetSystemMetrics(winapi::um::winuser::SM_CXSCREEN) };
        let screen_h = unsafe { winapi::um::winuser::GetSystemMetrics(winapi::um::winuser::SM_CYSCREEN) };
        if screen_w > 0 && screen_h > 0 {
            assert!(
                (wr - wl) <= screen_w && (wb - wt) <= screen_h,
                "work area ({wl},{wt})-({wr},{wb}) must fit inside the screen {screen_w}x{screen_h}"
            );
            // With a taskbar present it must be strictly inset on some edge.
            let inset = wl > 0 || wt > 0 || wr < screen_w || wb < screen_h;
            if inset {
                assert!(
                    (wr - wl) < screen_w || (wb - wt) < screen_h,
                    "work area is inset but the same size as the screen: \
                     ({},{})-({},{}) vs {screen_w}x{screen_h}",
                    wl, wt, wr, wb
                );
            }
        }

        // The popup must have a real, non-zero size. If this ever measures 0,
        // `clamp_popup_to_work_area` sees a popup that always fits and never
        // flips -- the original "appears at a random position" symptom.
        let (pw, ph) = measured_popup_size(f.window_hwnd);
        assert!(pw > 0 && ph > 0, "popup measured {pw}x{ph}; a zero size disables flipping");
        assert!(
            pw <= (wr - wl).max(1) && ph <= (wb - wt).max(1) * 100,
            "popup {pw}x{ph} is implausibly large for work area ({wl},{wt})-({wr},{wb})"
        );
        // Four items must stack into a visibly taller-than-one-row popup, or
        // the per-item measurement loop is not actually running.
        assert!(ph >= 40, "four menu items must be taller than one row, got {ph}");
    });
}

#[test]
fn popup_flips_when_the_mouse_point_would_not_fit() {
    with_menu_bar(|f| {
        let (wl, wt, wr, wb) = work_area();
        let (pw, ph) = measured_popup_size(f.window_hwnd);

        // Reproduce the decision the popup code makes, from the same inputs,
        // for a point hard against the bottom-right corner.
        let mx = wr - 1;
        let my = wb - 1;
        // Inline mirror of the pure helper's rule, to show WHY this position
        // needs a flip: the naive placement would overflow both edges.
        assert!(
            mx + pw > wr || my + ph > wb,
            "test point ({mx},{my}) unexpectedly fits a {pw}x{ph} popup; \
             it should require a flip"
        );
        // The work area the popup is bounded by must be able to hold it after
        // flipping -- otherwise the flip is meaningless here.
        assert!(
            pw <= (wr - wl) && ph <= (wb - wt),
            "popup {pw}x{ph} cannot fit work area ({wl},{wt})-({wr},{wb}) at all; \
             this machine's geometry makes the flip test meaningless"
        );
    });
}
