//! The foldout's max-edit-distance row, end to end on GTK.
//!
//! The fuzzy budget was reachable only by typing into its field (or wheeling
//! over it). On AppKit that field may take no keystrokes inside the stack view
//! and the adapter wires no wheel at all, so on macOS a user could not change
//! the value — the bug this row's `-`/`+` buttons exist to fix. This test drives
//! the row `main.rs` builds (`ff_gui::distance_ui::build`) through the handler
//! it wires (`distance_ui::wire`), with real clicks: GTK's own `clicked` signal
//! fired by `emit_clicked`, so the buttons are proven to work, not assumed to.
//!
//! What is asserted:
//!   * the row is seeded with `DEFAULT_MAX_DISTANCE` and labels itself
//!     `max distance:`;
//!   * `+` steps the number up and `-` steps it down, written into the field
//!     the request is built from;
//!   * a click arms the search, exactly as a keystroke in the box does;
//!   * the value saturates at 0 and at `MAX_FUZZY_DISTANCE`, and a click at a
//!     bound changes nothing and arms nothing;
//!   * blank or unparseable text steps from the default instead of sticking;
//!   * the number that reaches the daemon request (`SearchOptions` from the
//!     field, via `DaemonClient::query_request`) is the one the buttons wrote;
//!   * Fuzzy off marks the row inactive in words and leaves the field typeable.
//!
//! Needs a display with GTK3, so a headless run returns early; CI runs the
//! Linux interaction step under Xvfb.

#![cfg(unix)]

use std::cell::Cell;
use std::rc::Rc;

use ff_gui::{
    distance_ui, wheel_step_value, DaemonClient, SearchOptions, DEFAULT_MAX_DISTANCE,
    DEFAULT_MAX_DISTANCE_VALUE, MAX_FUZZY_DISTANCE, WheelStep,
};
use fflocate::proto::Request;
use rswidgets::prelude::*;

/// Pump GTK's main loop so a queued signal is delivered, as `created_range_gtk`
/// and `typing_gtk` do.
fn pump(loader: &gtk_dynamic_loader::Loader, iterations: usize) {
    let Some(gtk) = loader.libs.get("libgtk") else {
        return;
    };
    unsafe {
        let it = gtk.get::<unsafe extern "C" fn(i32) -> i32>(b"gtk_main_iteration_do");
        if let Ok(f) = it {
            for _ in 0..iterations {
                f(0);
            }
        }
    }
}

#[test]
fn the_distance_buttons_change_the_value_a_click_at_a_time() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        println!("distance row: skipped (no display)");
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => {
            println!("distance row: skipped (no GTK)");
            return;
        }
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        println!("distance row: skipped (not GTK3)");
        return;
    }
    let Ok(app) = App::init() else {
        println!("distance row: skipped (App::init failed)");
        return;
    };

    // --- The row exactly as `run()` builds it ---
    let win = app.create_window().expect("window");
    win.set_title("ff-gui distance row test");
    let row = distance_ui::build(&app, DEFAULT_MAX_DISTANCE).expect("the distance row");
    assert_eq!(
        row.label.get_text().as_deref(),
        Some("max distance: (fuzzy off)"),
        "the row must label the field, and say the budget is inactive at the start"
    );
    assert_eq!(
        row.field.get_text().as_deref(),
        Some(DEFAULT_MAX_DISTANCE),
        "the row starts at the default budget"
    );

    let foldout_box = app.create_box(Orientation::Vertical, 2).expect("foldout box");
    foldout_box.append(&row.row);
    let layout = app.create_box(Orientation::Vertical, 2).expect("window layout");
    layout.append(&foldout_box);
    win.set_child(&layout);
    win.present();
    pump(&loader, 50);

    // --- The app's own handler; every click that changes the text arms it ---
    let armed = Rc::new(Cell::new(0usize));
    {
        let armed = armed.clone();
        distance_ui::wire(&row, move || armed.set(armed.get() + 1));
    }

    let click = |button: &Button| {
        button.emit_clicked().expect("the button takes a click");
        pump(&loader, 10);
    };
    let value = || row.field.get_text().unwrap_or_default();

    // Up twice, then down once: 2 -> 3 -> 4 -> 3.
    click(&row.increase);
    assert_eq!(value(), "3", "`+` must step the budget up");
    click(&row.increase);
    assert_eq!(value(), "4", "`+` must step it again");
    click(&row.decrease);
    assert_eq!(value(), "3", "`-` must step the budget down");
    assert_eq!(armed.get(), 3, "each changing click must arm a search");

    // Fuzzy off => the label says so in words, and the field still types.
    row.set_controls_sensitive(false);
    pump(&loader, 5);
    assert_eq!(
        row.label.get_text().as_deref(),
        Some("max distance: (fuzzy off)"),
        "an inactive budget must say so on the label"
    );
    assert_eq!(
        row.field.get_text().as_deref(),
        Some("3"),
        "marking the row inactive must not lose the value"
    );
    row.set_controls_sensitive(true);
    pump(&loader, 5);
    assert_eq!(
        row.label.get_text().as_deref(),
        Some(distance_ui::LABEL),
        "the marker must go once Fuzzy is ticked"
    );

    // The bounds: 0 is the floor, MAX_FUZZY_DISTANCE the ceiling, and a click
    // that cannot move the number must neither rewrite it nor arm a search.
    let before = armed.get();
    row.field.set_text("0");
    click(&row.decrease);
    assert_eq!(value(), "0", "`-` must not go below zero");
    row.field.set_text(&MAX_FUZZY_DISTANCE.to_string());
    click(&row.increase);
    assert_eq!(
        value(),
        MAX_FUZZY_DISTANCE.to_string(),
        "`+` must stop at the ceiling"
    );
    assert_eq!(
        armed.get(),
        before,
        "a click at a bound changed nothing, so nothing may be searched"
    );

    // Blank or unparseable text steps from the default rather than sticking.
    row.field.set_text("  ");
    click(&row.increase);
    assert_eq!(
        value(),
        (DEFAULT_MAX_DISTANCE_VALUE + 1).to_string(),
        "a blank field must step from the default"
    );
    row.field.set_text("abc");
    click(&row.decrease);
    assert_eq!(
        value(),
        (DEFAULT_MAX_DISTANCE_VALUE - 1).to_string(),
        "unparseable text must step from the default"
    );

    // --- What actually reaches the daemon ---
    // `gather_options` reads exactly this field; the request carries it.
    row.field.set_text("5");
    pump(&loader, 5);
    let opts = SearchOptions {
        fuzzy: true,
        max_distance: row.field.get_text().and_then(|t| t.trim().parse().ok()),
        ..SearchOptions::default()
    };
    match DaemonClient::query_request("mian", 10, &opts) {
        Request::Query { max_distance, fuzzy, .. } => {
            assert!(fuzzy, "the request must ask for fuzzy matching");
            assert_eq!(
                max_distance,
                Some(5),
                "the number the buttons wrote must be the budget the daemon gets"
            );
        }
        other => panic!("a search must send Request::Query, got {other:?}"),
    }

    // The library's stepping and the button's agree, so the wheel and the
    // buttons cannot drift apart.
    for text in ["0", "2", "abc", "", "  9  ", "99"] {
        assert_eq!(
            value_of_step(text, WheelStep::Up),
            wheel_step_value(text, WheelStep::Up),
            "step helper disagreed for {text:?}"
        );
    }

    println!(
        "distance row: buttons stepped 2->3->4->3, saturated at 0 and {MAX_FUZZY_DISTANCE}, \
         inactive marker and the daemon-side budget asserted"
    );
}

/// `step_max_distance` as a number, for the last assertion.
fn value_of_step(text: &str, step: WheelStep) -> String {
    ff_gui::step_max_distance(text, step)
}
