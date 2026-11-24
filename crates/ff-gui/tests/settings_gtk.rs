//! Ticking a request-shaping setting re-asks the query that is on screen.
//!
//! The settings live in the Advanced foldout — Case sensitive, Fuzzy matching,
//! Count only — and each one changes what the daemon *returns*, so the results
//! grid can only be refreshed by asking the same query again. `main.rs` wires
//! them through `ff_gui::settings_ui::wire`, which is what this test drives:
//! real GTK checkboxes and entries, the app's own wiring function, and the app's
//! own request builder (`gather_options` → `query_request` → `encode_request`)
//! reading the widgets at the moment the search is armed.
//!
//! What it proves:
//!
//! * a tick with both query boxes empty arms nothing — there is no query, and an
//!   empty one would match everything;
//! * a tick with a query in either box arms the same debounce a keystroke arms;
//! * the request built from the widgets then carries the option, and unticking
//!   restores the plain request — so the rows that come back are for the options
//!   on screen, not the ones the last keystroke used.
//!
//! One GTK initialisation per process, so this is its own test binary: a second
//! `App::init` on another test thread corrupts the heap.

#![cfg(unix)]

use std::time::Duration;

use ff_gui::{
    distance_ui, encode_request, gather_options, settings_ui, DaemonClient, Debounce,
    DEFAULT_MAX_DISTANCE,
};

/// Pump GTK's main loop, the way `typing_gtk` does, so the signals a `set_active`
/// queues are delivered.
fn pump(loader: &gtk_dynamic_loader::Loader, iterations: usize) {
    let Some(gtk) = loader.libs.get("libgtk") else {
        return;
    };
    unsafe {
        let it = gtk.get::<unsafe extern "C" fn()>(b"gtk_main_iteration_do");
        if let Ok(f) = it {
            for _ in 0..iterations {
                f();
            }
        }
    }
}

#[test]
fn ticking_a_setting_re_asks_the_query_that_is_on_screen() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("settings: no display, skipping");
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => {
            eprintln!("settings: no GTK loader, skipping");
            return;
        }
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        eprintln!("settings: not GTK3, skipping");
        return;
    }
    let app = match rswidgets::core::App::init() {
        Ok(app) => app,
        Err(_) => {
            eprintln!("settings: GTK did not initialise, skipping");
            return;
        }
    };

    // The foldout's controls, built and wired the way `main.rs` builds them.
    let quick = app.create_entry().expect("basic entry");
    let advanced = app.create_entry().expect("advanced entry");
    let case = app.create_checkbutton("Case sensitive").expect("case box");
    let fuzzy = app.create_checkbutton("Fuzzy matching").expect("fuzzy box");
    let count = app
        .create_checkbutton("Count only (skip the list)")
        .expect("count box");
    let distance = distance_ui::build(&app, DEFAULT_MAX_DISTANCE).expect("distance row");
    let debounce = std::rc::Rc::new(Debounce::new(Duration::from_millis(20)));
    {
        let debounce = debounce.clone();
        settings_ui::wire(
            &[&case, &fuzzy, &count],
            &quick,
            &advanced,
            move || debounce.arm(),
        );
    }

    // The request the app's pump would build now, from the live widgets — the
    // same path it runs when the debounce comes due.
    let request_line = || {
        let opts = gather_options(
            case.is_active(),
            fuzzy.is_active(),
            count.is_active(),
            &distance.field.get_text().unwrap_or_default(),
        );
        let q = quick.get_text().unwrap_or_default();
        encode_request(&DaemonClient::query_request(&q, 10, &opts)).expect("encode the request")
    };

    // Nothing typed: a tick changes no query, so it must not arm a search.
    fuzzy.set_active(true);
    pump(&loader, 10);
    assert!(
        !debounce.pending(),
        "a tick with both boxes empty armed a search for an empty query"
    );
    fuzzy.set_active(false);
    pump(&loader, 10);
    debounce.clear();

    // With a query on screen each setting re-asks it, and the request carries
    // the setting. Unticking is a setting change too, so it re-asks as well.
    quick.set_text("report");
    pump(&loader, 10);
    assert!(
        !debounce.pending(),
        "this test wires the settings only; typing must not arm here"
    );
    let plain = request_line();
    for (toggle, name) in [
        (&case, "Case sensitive"),
        (&fuzzy, "Fuzzy matching"),
        (&count, "Count only"),
    ] {
        toggle.set_active(true);
        pump(&loader, 10);
        assert!(debounce.pending(), "ticking {name} did not arm a search");
        assert_ne!(
            request_line(),
            plain,
            "the search re-run for {name} asked the old question"
        );
        debounce.clear();
        toggle.set_active(false);
        pump(&loader, 10);
        assert!(debounce.pending(), "unticking {name} did not arm a search");
        assert_eq!(
            request_line(),
            plain,
            "unticking {name} left the request changed"
        );
        debounce.clear();
    }

    // The Advanced box is a query on its own: a tick refreshes with the Basic
    // box empty.
    quick.set_text("");
    advanced.set_text("ext:rs");
    pump(&loader, 10);
    case.set_active(true);
    pump(&loader, 10);
    assert!(
        debounce.pending(),
        "a tick with only the Advanced box filled did not re-ask its query"
    );
    println!(
        "settings: toggling Case/Fuzzy/Count re-arms the search and the request carries them"
    );
}
