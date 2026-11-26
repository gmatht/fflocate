//! `ff-gui` — thin wrapper around the library entry point.
//!
//! The window lives in [`ff_gui::ui`] so the multi-call binary (`ffbusy`) can
//! link it; this binary exists for packaging and for `ff --gui`.

fn main() -> std::process::ExitCode {
    ff_gui::ui::main_entry()
}
