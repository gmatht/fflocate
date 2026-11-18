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

    println!("probe: ok");
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    println!("macos_probe: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
