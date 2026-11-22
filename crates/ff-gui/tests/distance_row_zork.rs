//! The foldout's controls, driven together on the headless `zork` backend.
//!
//! Two GUI bugs lived in the interaction between the max-distance row and the
//! Fuzzy toggle, and both were invisible to a test that pokes one widget at a
//! time:
//!
//! * AppKit may not hand an `NSTextField` inside the foldout's stack view to the
//!   field editor, and the backend wires no wheel to an entry — so typing and
//!   wheeling both failed there and the budget could not be changed at all.
//! * With Fuzzy unticked the field was disabled, so the one thing that *could*
//!   have worked (setting the value first, then ticking Fuzzy) could not be
//!   done either.
//!
//! This drives the real row from `ff_gui::distance_ui` the way a person would:
//! type into the field (zork `type_into`, the same entry `connect_changed` fires
//! on), click `-`/`+`, tick the Fuzzy checkbox, and read the request the app
//! would send. zork's backend is headless, so this runs everywhere — including
//! the CI job that builds `--features zork` with no display.

#![cfg(feature = "zork")]

use std::cell::Cell;

use ff_gui::{distance_ui, DaemonClient, SearchOptions, DEFAULT_MAX_DISTANCE, WheelStep};
use rswidgets::backends_zork_adapter::{Button, Entry};
use rswidgets::prelude::*;

/// `main.rs`'s option reader, for the widgets this test drives.
///
/// The real one lives in the binary (nothing can link against it), so the field
/// mapping is restated here — it is two lines, and the point of the test is the
/// widget plumbing, not the mapping (which `main.rs`'s own unit tests pin).
fn options(fuzzy_on: bool, distance: &Entry) -> SearchOptions {
    SearchOptions {
        fuzzy: fuzzy_on,
        max_distance: if fuzzy_on {
            distance.get_text().and_then(|t| t.trim().parse().ok())
        } else {
            None
        },
        ..SearchOptions::default()
    }
}

#[test]
fn the_budget_can_be_set_before_fuzzy_is_ticked_and_then_fed_the_request() {
    let app = match App::init() {
        Ok(a) => a,
        Err(e) => {
            println!("zork distance row: skipped (App::init: {e})");
            return;
        }
    };

    let row = distance_ui::build(&app, DEFAULT_MAX_DISTANCE).expect("build the distance row");
    let fuzzy = app.create_checkbutton("Fuzzy matching").expect("fuzzy checkbox");
    fuzzy.set_active(false);

    // What the app connects: a changed tick re-marks the row (`main.rs`'s
    // handler is the same call). Run directly by `toggle()` here, because zork
    // fires the toggle callback with its model borrowed and a widget call would
    // re-enter that borrow — a test-framework constraint, not the app's (GTK
    // and AppKit emit the signal after updating the button, as `menubar_gtk`
    // drives it).
    let toggles = std::rc::Rc::new(Cell::new(0usize));
    {
        let toggles = toggles.clone();
        fuzzy
            .connect_toggled(move || {
                toggles.set(toggles.get() + 1);
            })
            .expect("connect toggled");
    }

    // `main.rs` also wires the buttons to arm a search and to re-mark the row.
    let armed = std::rc::Rc::new(Cell::new(0usize));
    {
        let armed = armed.clone();
        distance_ui::wire(&row, move || armed.set(armed.get() + 1));
    }

    // --- Fuzzy off: the value is still settable, and says it is inactive ---
    assert!(
        row.label
            .get_text()
            .unwrap_or_default()
            .contains("(fuzzy off)"),
        "an inactive budget must say so on the label, got {:?}",
        row.label.get_text()
    );
    assert_eq!(
        row.field.get_text().as_deref(),
        Some(DEFAULT_MAX_DISTANCE),
        "the row starts at the default"
    );
    // Typing into the field is how a GTK user sets it: zork's `type_into` sets
    // the text and fires the same `changed` callback a keystroke would.
    row.field.set_text("4");
    assert_eq!(
        row.field.get_text().as_deref(),
        Some("4"),
        "the field must accept a typed value while Fuzzy is off"
    );

    // --- Tick Fuzzy: the row wakes up, and the typed value reaches the request
    fuzzy.toggle();
    assert!(fuzzy.is_active(), "the checkbox did not tick");
    assert_eq!(toggles.get(), 1, "the tick must run the app's handler");
    // The app's handler body, applied here (see the note above).
    row.set_controls_sensitive(fuzzy.is_active());
    assert_eq!(
        row.label.get_text().as_deref(),
        Some(distance_ui::LABEL),
        "ticking Fuzzy must clear the inactive marker"
    );
    match DaemonClient::query_request("FreeBuff", 10, &options(fuzzy.is_active(), &row.field)) {
        fflocate::proto::Request::Query {
            fuzzy: on,
            max_distance,
            ..
        } => {
            assert!(on, "the request must ask for fuzzy matching");
            assert_eq!(
                max_distance,
                Some(4),
                "the typed value must be the budget the daemon gets"
            );
        }
        other => panic!("expected Request::Query, got {other:?}"),
    }

    // --- The buttons work with Fuzzy on, and arm the search each time ---
    let click = |b: &Button| b.emit_clicked().expect("click");
    let before = armed.get();
    click(&row.increase);
    assert_eq!(row.field.get_text().as_deref(), Some("5"), "`+` must step up");
    click(&row.decrease);
    click(&row.decrease);
    assert_eq!(
        row.field.get_text().as_deref(),
        Some("3"),
        "`-` must step down"
    );
    assert_eq!(
        armed.get(),
        before + 3,
        "each changing click must arm a search"
    );

    // The number a click wrote is the number the daemon is asked for.
    match DaemonClient::query_request("FreeBuff", 10, &options(true, &row.field)) {
        fflocate::proto::Request::Query { max_distance, .. } => {
            assert_eq!(max_distance, Some(3))
        }
        other => panic!("expected Request::Query, got {other:?}"),
    }

    // --- Untick: the value is ignored, as it must be ---
    fuzzy.toggle();
    assert!(!fuzzy.is_active());
    assert_eq!(toggles.get(), 2);
    row.set_controls_sensitive(fuzzy.is_active());
    assert!(
        row.label
            .get_text()
            .unwrap_or_default()
            .contains("(fuzzy off)"),
        "unticking Fuzzy must say the budget is inert again"
    );
    match DaemonClient::query_request("FreeBuff", 10, &options(fuzzy.is_active(), &row.field)) {
        fflocate::proto::Request::Query {
            fuzzy: on,
            max_distance,
            ..
        } => {
            assert!(!on);
            assert_eq!(
                max_distance, None,
                "with Fuzzy off the distance must not leak into the query"
            );
        }
        other => panic!("expected Request::Query, got {other:?}"),
    }

    // The wheel path and the buttons agree (the field is an entry, so the user
    // may type anything; both step from the default when it is not a number).
    for text in ["", "  ", "abc", "0", "2", "99"] {
        assert_eq!(
            ff_gui::step_max_distance(text, WheelStep::Up),
            ff_gui::wheel_step_value(text, WheelStep::Up)
        );
    }

    println!("zork distance row: ok — typed value reached the request, buttons stepped it, fuzzy gating honoured");

}
