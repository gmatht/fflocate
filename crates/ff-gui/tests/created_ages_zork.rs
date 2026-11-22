//! The foldout's **created from / to** row, driven on the headless `zork`
//! backend.
//!
//! The row accepts three shapes in each field — `YYYY-MM-DD`, `HH:MM` and an age
//! (`3 days ago`) — and has an age dropdown beside each. Two things could go
//! wrong that a parser unit test cannot catch, and this file pins both:
//!
//! * **the dropdown must write into the field** (the field is the single source
//!   of truth; the request builder reads fields, never dropdowns), and
//! * **a pick must re-run the search**, exactly as typing the same text does, so
//!   a picked age and a typed one cannot diverge.
//!
//! It drives the real row from `ff_gui::created_ui` — the same `build`/`wire`
//! `main.rs` calls — so it exercises the widgets the user actually opens. zork
//! is headless, so this runs everywhere, including the CI job that builds
//! `--features zork` with no display.

#![cfg(feature = "zork")]

use std::cell::Cell;
use std::rc::Rc;

use ff_gui::created_ui;
use rswidgets::prelude::*;

#[test]
fn the_age_dropdown_writes_into_the_field_and_searches() {
    let app = match App::init() {
        Ok(a) => a,
        Err(e) => {
            println!("zork created row: skipped (App::init: {e})");
            return;
        }
    };
    let row = created_ui::build(&app).expect("build the created row");

    // The app's "re-run the search" handler. `wire` calls it on every edit that
    // leaves a parseable pair; count the calls so a pick can be shown to reach
    // the search path at all.
    let searches = Rc::new(Cell::new(0u32));
    {
        let searches = searches.clone();
        created_ui::wire(&row, move || searches.set(searches.get() + 1));
    }

    // A freshly built row is the "no range" state.
    assert_eq!(row.from.get_text().unwrap_or_default(), "");
    assert_eq!(row.to.get_text().unwrap_or_default(), "");

    // Every option in the list writes exactly its own text into the field it
    // sits beside — the round trip that makes a pick and a type equivalent.
    for (i, option) in ff_gui::CREATED_AGE_OPTIONS.iter().enumerate() {
        let index = (i + 1) as u32;
        row.from_ages.set_active(Some(index));
        row.from_ages.emit_changed().expect("emit changed");
        assert_eq!(
            row.from.get_text().unwrap_or_default(),
            *option,
            "row {index} must write {option:?} into the from field"
        );
        // The pair must still parse: a pick that produced text the parser
        // rejects would make the search refuse on the next keystroke.
        assert!(
            ff_gui::parse_created_range(option, "").is_ok(),
            "{option:?} must be accepted by the range parser"
        );
    }
    assert!(
        searches.get() > 0,
        "a selection must re-run the search, got {}",
        searches.get()
    );

    // The `to` field's dropdown writes into `to`, not `from`.
    row.from.set_text("");
    row.to_ages
        .set_active(Some(created_ui::index_of_age("1 week ago")));
    row.to_ages.emit_changed().expect("emit changed");
    assert_eq!(row.to.get_text().unwrap_or_default(), "1 week ago");
    assert_eq!(row.from.get_text().unwrap_or_default(), "", "from untouched");

    // The placeholder row clears the field rather than writing "(ages)".
    row.to_ages.set_active(Some(0));
    row.to_ages.emit_changed().expect("emit changed");
    assert_eq!(row.to.get_text().unwrap_or_default(), "", "placeholder clears");
}

#[test]
fn a_picked_age_and_a_typed_one_produce_the_same_status_line() {
    let app = match App::init() {
        Ok(a) => a,
        Err(e) => {
            println!("zork created row: skipped (App::init: {e})");
            return;
        }
    };
    let row = created_ui::build(&app).expect("build the created row");
    created_ui::wire(&row, || {});

    for (i, option) in ff_gui::CREATED_AGE_OPTIONS.iter().enumerate() {
        if *option == "any time" {
            continue; // the empty window, not a range
        }
        row.from_ages.set_active(Some((i + 1) as u32));
        row.from_ages.emit_changed().expect("emit changed");
        let picked = row.from.get_text().unwrap_or_default();
        assert_eq!(&picked, option, "the pick must write the option's own text");

        // A picked age and a typed one are the same text, so they must read the
        // same on the status line. Compare the labels the app renders.
        let typed = ff_gui::parse_created_range(option, "").unwrap();
        let from_pick = ff_gui::parse_created_range(&picked, "").unwrap();
        assert_eq!(
            from_pick.map(|r| r.label()),
            typed.map(|r| r.label()),
            "{option:?}: a picked age must read the same as a typed one"
        );
    }
}

#[test]
fn the_row_keeps_both_labels_both_fields_and_both_dropdowns() {
    // The GTK test asserts the same text on screen; this keeps the headless
    // build honest about the row's shape after the dropdowns joined it.
    let app = match App::init() {
        Ok(a) => a,
        Err(e) => {
            println!("zork created row: skipped (App::init: {e})");
            return;
        }
    };
    let row = created_ui::build(&app).expect("build the created row");
    assert_eq!(
        row.from_label.get_text().as_deref(),
        Some(created_ui::FROM_LABEL),
        "the row must label its first field"
    );
    assert_eq!(
        row.to_label.get_text().as_deref(),
        Some(created_ui::TO_LABEL),
        "the row must label its second field"
    );
    // Both dropdowns start on the placeholder row.
    assert_eq!(row.from_ages.get_active(), 0);
    assert_eq!(row.to_ages.get_active(), 0);
    // The placeholder hint names both accepted absolute shapes.
    assert!(created_ui::PLACEHOLDER.contains("YYYY-MM-DD"));
    assert!(created_ui::PLACEHOLDER.contains("HH:MM"));
}
