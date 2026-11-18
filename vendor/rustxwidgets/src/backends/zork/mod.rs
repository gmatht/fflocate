//! Headless `zork` backend for rswidgets.
//!
//! The backend is split into three pieces:
//!
//! * [`model`] — a pure, I/O-free in-memory model of the widget
//!   (`ZorkState` / `ZorkNode` / `ZorkKind`) plus all create/set/get/fire
//!   operations. This is the single source of truth shared by every driver.
//! * [`repl`] — the interactive text REPL (`ZorkApp`), kept for manual
//!   exploration and demos. It is *one* driver over the model.
//! * [`harness`] — a typed, synchronous in-process test API. This is the
//!   recommended replacement for a JSON backend: it exercises the very same
//!   model without stringly-typed commands, parsing, or serialization.
//!
//! The [`facade`] submodule re-exports the old free-function API
//! (`create_window`, `set_label_text`, …) over a thread-local model so the
//! existing `backends_zork_adapter` shim keeps compiling unchanged. New code
//! should use [`harness`] or [`model`] directly.
//!
//! JSON only earns its keep at a process boundary (snapshot diffs, external
//! non-Rust drivers). See [`model::ZorkState::snapshot`].

pub mod facade;
pub mod harness;
pub mod model;
pub mod repl;

// Re-export the free-function facade's items at the `zork` module root so the
// adapter (`crate::backends_zork_adapter`) resolves `crate::backends::zork::*`.
pub use facade::*;

pub use model::{
    Callback, MenuItemData, MenuItemKind, SheetChrome, SheetInput, Timer, ZorkKind, ZorkNode, ZorkProps,
    ZorkState,
};

use crate::backends::BackendApp;

/// Construct the interactive REPL driver over the *existing* model.
///
/// Adopting rather than starting fresh matters when a host has already built a
/// widget tree through the adapter — corro builds ~100 widgets (window, canvas,
/// formula row, tab bar, 67 menu actions) and then hands off here. With a fresh
/// model the REPL would describe an empty room and none of that would be
/// reachable.
pub fn init() -> Result<Box<dyn BackendApp>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(Box::new(repl::ZorkApp::adopt_singleton()))
}
