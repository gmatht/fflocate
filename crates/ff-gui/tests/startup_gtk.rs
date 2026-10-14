//! Regression test: the deferred startup probe must settle the status line
//! through the real GTK pump.
//!
//! `run()` presents the window first and resolves the daemon check from the
//! 30 ms timer pump (`StartupProbe`), so a missing or wedged daemon can never
//! hold first paint hostage. The probe's state machine is pinned headless in
//! `tests/startup_probe.rs`; this test pins the *wiring* that machine depends
//! on: a pump tick calls `poll()`, and the resulting line lands on the label
//! via `set_text`.
//!
//! That wiring is exactly what silently fails on the `zork` backend (the REPL
//! adopts the widget tree out of the thread-local singleton, so timer-driven
//! `set_text` is dropped) — which is why this test drives real GTK instead.
//! Needs a display with GTK3, so on headless CI — or anywhere the toolkit is
//! missing — it passes trivially instead of failing.

#![cfg(unix)]

use std::cell::RefCell;
use std::os::raw::c_void;
use std::rc::Rc;

use ff_gui::{StartupPoll, StartupProbe};

/// Run pending main-context iterations, with real sleeps between them so a
/// loopback round-trip has wall-clock time to arrive.
fn pump(loader: &gtk_dynamic_loader::Loader, ms: u64) {
    let Some(gtk) = loader.libs.get("libgtk") else {
        return;
    };
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

fn label_text(loader: &gtk_dynamic_loader::Loader, widget: *mut c_void) -> Option<String> {
    let gtk = loader.libs.get("libgtk")?;
    let get =
        unsafe { gtk.get::<unsafe extern "C" fn(*mut c_void) -> *const u8>(b"gtk_label_get_text") }
            .ok()?;
    let ptr = unsafe { get(widget) };
    if ptr.is_null() {
        return None;
    }
    unsafe {
        std::ffi::CStr::from_ptr(ptr as *const i8)
            .to_str()
            .ok()
            .map(str::to_string)
    }
}

#[test]
fn startup_probe_settles_the_status_line_through_the_pump() {
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
    let Ok(app) = rswidgets::App::init() else {
        return;
    };

    // A canned daemon: one Stats record per connection, socket held open.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sock");
    let listener = fflocate::transport::Listener::bind(&path).unwrap();
    std::thread::spawn(move || {
        for _ in 0..10 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            use std::io::{BufRead, BufReader, Write};
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut req = String::new();
            if reader.read_line(&mut req).is_err() {
                return;
            }
            let resp = serde_json::to_string(&fflocate::proto::Response::Stats {
                stats: serde_json::json!({"entries": 7u64, "backend": "fake"}),
            })
            .unwrap();
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(b"\n");
            let _ = stream.flush();
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    });

    // The run() wiring in miniature: a status label plus the pump stepping
    // the probe to completion, exactly as main.rs registers it.
    let status = app.create_label("connecting").unwrap();
    let probe = Rc::new(RefCell::new(StartupProbe::new(
        path.clone(),
        std::time::Duration::from_secs(5),
        true,
    )));
    let step_status = status.clone();
    let step_probe = probe.clone();
    let pump_handle = rswidgets::App::timeout_add(
        30,
        Box::new(move || {
            if !step_probe.borrow().is_done() {
                match step_probe.borrow_mut().poll() {
                    StartupPoll::Pending => {}
                    StartupPoll::Note(line) | StartupPoll::Done(line) => {
                        step_status.set_text(&line)
                    }
                }
            }
            true
        }),
    );
    if pump_handle.is_err() {
        return;
    }

    // Present first (as run() does), then let the pump settle the label.
    let win = app.create_window().unwrap();
    win.present();
    pump(&loader, 2000);

    let ptr: *mut c_void = *status.as_ref();
    let text = label_text(&loader, ptr).unwrap_or_default();
    assert!(
        text.starts_with("connected to fflocated at "),
        "the pump must settle the status line, got {text:?}"
    );
    assert!(text.contains("7 entries"), "stats must land, got {text:?}");
    assert!(probe.borrow().is_done());
    // Keep the window alive until the read: dropping it first would free the
    // label out from under the pointer.
    let _ = win;
}
