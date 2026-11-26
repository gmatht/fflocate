//! `ff` — thin wrapper around the library entry point.
//!
//! The CLI lives in [`ff_cli`] so the multi-call binary (`ffbusy`) can link
//! it; this binary exists for scripts and packaging that expect `ff` alone.

fn main() -> std::process::ExitCode {
    ff_cli::main_entry()
}
