#[cfg(feature = "pancurses")]
use std::process::Command;

/// Run the dialog example and verify the TUI is not blank:
/// the captured terminal output must contain expected widget labels.
#[test]
#[cfg(feature = "pancurses")]
fn pancurses_dialog_renders_content() {
    let output = Command::new("timeout")
        .args(["3", "cargo", "run", "--features", "pancurses", "--example", "dialog"])
        .output()
        .expect("failed to run dialog example");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // The dialog example sets these widget labels:
    //   DropDown: "Choice 1", "Choice 2", "Choice 3"
    //   CheckButton: "Enable feature"
    //   RadioButton: "Option A", "Option B", "Option C"
    //   Entry: "Hello" / "World"
    //   TextView: "Multi-line"
    // At least some of them must appear in raw terminal output.
    let expected = ["Choice", "Enable", "Option", "World", "Multi"];
    let mut found = false;
    for text in &expected {
        if stdout.contains(text) {
            found = true;
            break;
        }
    }
    assert!(found, "TUI output appears blank — none of {:?} found in terminal output", expected);

    // Must not panic
    assert!(!stdout.contains("panicked"), "example panicked");
}

/// `common::Entry`'s change-suppression contract, now that the pancurses
/// backend actually fires `connect_changed`.
///
/// These two features interact: `set_text_suppressing_changed` sets a flag that
/// `connect_changed` checks, so if firing is ever removed the suppression test
/// would pass vacuously (nothing fires either way) — hence both directions are
/// asserted here. The GUI backend relies on this for 13 call sites (preset edit
/// buffers, movie replay, Insert Date/Time) where re-entering the edit logic
/// would corrupt the buffer.
#[cfg(all(feature = "pancurses", not(feature = "gtk")))]
mod entry_change_suppression {
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn changes_fire_and_suppression_blocks_them() {
        // The generic wrapper over this backend's entry adapter.
        let entry = rswidgets::common::Entry::new(
            rswidgets::backends_pancurses_adapter::create_entry().expect("entry"),
        );

        // Direction 1: an ordinary set_text fires.
        let fired = Rc::new(Cell::new(0));
        let f = fired.clone();
        let _ = entry.connect_changed(move || f.set(f.get() + 1));
        entry.set_text("typed");
        assert_eq!(1, fired.get(), "a normal text change must fire");

        // Direction 2: a suppressing set_text does not.
        entry.set_text_suppressing_changed("preset");
        assert_eq!(1, fired.get(), "a suppressed change must not fire");
        assert_eq!(Some("preset".to_string()), entry.get_text());

        // …and firing resumes afterwards (the flag is cleared, not sticky).
        entry.set_text("again");
        assert_eq!(2, fired.get(), "firing must resume after a suppressed set");
    }
}
