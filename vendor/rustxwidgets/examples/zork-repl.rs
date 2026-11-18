//! Start the zork REPL over a model that has widgets but no `Spreadsheet`.
//!
//! corro's own build never creates a `Spreadsheet` node, and that is the model
//! the zork REPL starts with, so this is corro's shape exactly. It exists as
//! a fixture for `tests/zork_repl_says_why.rs`, which needs a program that
//! enters the REPL over a sheet-less model:
//!
//! * `zork-demo` builds the same kind of widget tree but its `app.run()` never
//!   takes over the event loop here, so it prints its banner and exits --
//!   there is no REPL to ask.
//! * `zork-sheet` does hand off to the REPL, but it builds a `Spreadsheet`,
//!   which is the other case.
//!
//! Neither can answer "what does the REPL say when there is no sheet", so this
//! one does.
//!
//! Run with:
//!     cargo run -p rswidgets --no-default-features --features zork \
//!         --example zork-repl

use rswidgets::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let app = App::init()?;
    let win = app.create_window()?;
    win.set_title("Zork Demo");

    // A few real widgets, so the model is not empty and a numbered target
    // names something -- `grid 1` has to resolve to a node that is not a
    // spreadsheet, which is a different answer from "there is none".
    let vbox = app.create_box(Orientation::Vertical, 1)?;
    let label = app.create_label("Count: 0")?;
    let entry = app.create_entry()?;
    entry.set_text("Hello, Zork!");
    vbox.append(&label);
    vbox.append(&entry);
    win.set_child(&vbox);
    win.present();

    // No `create_spreadsheet` anywhere above, on purpose: that absence is the
    // subject of the test that runs this.
    println!("No spreadsheet here -- exactly like corro's own tree.");
    println!("Type 'grid' or 'sheet' to be told so. 'quit' to exit.");

    use rswidgets::backends::BackendApp as _;
    Box::new(rswidgets::backends::zork::repl::ZorkApp::adopt_singleton())
        .run()
        .map_err(|e| e)
}
