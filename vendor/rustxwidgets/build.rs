//! Generate the Android resources an rswidgets app needs, when the app has
//! not provided its own.
//!
//! This script lives in the rswidgets crate, so it cannot `use rswidgets`
//! (a crate's build script runs before, and independently of, the crate).
//! The generator therefore lives in `src/android_generator.rs` for downstream
//! apps to call from *their* build script, and this script includes the same
//! source directly so an app that vendors rswidgets still gets resources
//! without adding a build-dependency.
//!
//! Opt-in: set `RSWIDGETS_ANDROID_PROJECT` to the Android project root (the
//! directory containing `app/`). Without the variable nothing is written, so
//! a plain desktop build is unaffected. Generation is additive: existing
//! files are never overwritten.
//!
//! See `docs/ANDROID_GUIDELINES.md`.

#[path = "src/android_generator.rs"]
mod android_generator;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/android_generator.rs");

    // `cfg(feature = "gtk4-rs")` appears ~80 times as a gate on the local-path
    // gtk4-rs stack. That stack was removed in a1bd343a (its path deps broke
    // every CI build), which deliberately left the gates in place: they are
    // inert, and the adapter source is kept for local use. Without this
    // declaration rustc warns on every one of them ("unexpected cfg condition
    // value"), and 122 such warnings bury the handful of real ones.
    //
    // Declaring the name is the precise fix: it tells rustc the feature is
    // expected, so the gates stay inert and silent, while any *other* typo'd
    // feature name still warns. Do not "fix" these by deleting the gates —
    // that would drop the scaffolding for the dlopen adapter.
    println!("cargo::rustc-check-cfg=cfg(feature, values(\"gtk4-rs\"))");

    android_generator::run();
}
