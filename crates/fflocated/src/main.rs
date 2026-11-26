//! `fflocated` — thin wrapper around the library entry point.
//!
//! The daemon lives in [`fflocated::entry`] so the multi-call binary
//! (`ffbusy`) can link it; this binary exists for packaging and for scripts
//! that exec `fflocated` directly.

fn main() -> std::process::ExitCode {
    fflocated::entry::main_entry()
}
