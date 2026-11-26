//! `App::timeout_add` on GTK: the callback runs on the pump, and returning
//! `false` removes the source instead of leaving it armed.
//!
//! The 30 ms pump that drives the app's debounce, the startup probe and the
//! live feed is this timer. A timer that never fires stalls all of them; one
//! that ignores `false` keeps calling into widgets the caller expected to have
//! released. `startup_gtk` pins the fire-and-poll wiring but always returns
//! `true`, so the stop half had no test.
//!
//! Needs a display with GTK3 (a timer source lives on the default main
//! context), so a headless run returns early; CI runs it under Xvfb. A build
//! whose backend has no timer (`timeout_add` errors) also returns early rather
//! than failing a capability it never had.

#![cfg(unix)]

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Pump GTK's main loop without blocking, with real sleeps between iterations
/// so a 5 ms source has wall-clock time to become ready.
fn pump(loader: &gtk_dynamic_loader::Loader, ms: u64) {
    let Some(gtk) = loader.libs.get("libgtk") else {
        return;
    };
    let iterate = unsafe {
        gtk.get::<unsafe extern "C" fn(i32) -> i32>(b"gtk_main_iteration_do")
            .ok()
    };
    let Some(iterate) = iterate else { return };
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        // `0` = do not block: after the source is dropped there is no event
        // left to wake a blocking iteration.
        unsafe { iterate(0) };
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn app_timeout_add_fires_and_stops_on_false() {
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
    if rswidgets::App::init().is_err() {
        return;
    }

    // Three ticks, then `false` on the third: the count is what proves the
    // ticks happened, and staying at three afterwards is what proves the
    // source was removed.
    let ticks = Rc::new(Cell::new(0u32));
    let counter = ticks.clone();
    let armed = rswidgets::App::timeout_add(
        5,
        Box::new(move || {
            counter.set(counter.get() + 1);
            counter.get() < 3
        }),
    );
    if armed.is_err() {
        // Backend without a timer primitive; nothing to assert here.
        return;
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    while ticks.get() < 3 && Instant::now() < deadline {
        pump(&loader, 20);
    }
    assert_eq!(
        ticks.get(),
        3,
        "the timer must fire until its callback returns false"
    );

    // Well past several 5 ms periods: a source that ignored `false` would keep
    // counting here.
    pump(&loader, 300);
    assert_eq!(
        ticks.get(),
        3,
        "returning false must remove the source, not just be ignored"
    );
}
