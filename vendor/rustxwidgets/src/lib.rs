//! rustxwidgets: cross-platform thin GUI abstraction (GTK-dlopen on Linux, NWG on Windows)
//! Vendored snapshot: `warn(missing_docs)` upstream would drown workspace-wide
//! lints, so it is off here (see also the `[lints]` ceiling in Cargo.toml).
#![allow(missing_docs)]

pub mod prelude;
pub mod core;
pub mod common;
pub mod overflow;
pub mod lifecycle_stress;
pub mod backends;
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
mod backends_gtk_adapter_impl;
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
pub mod backends_gtk_adapter;
#[cfg(all(windows, not(feature = "zork")))]
pub mod backends_nwg_adapter;
#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
pub mod backends_wasm_adapter;
#[cfg(all(target_os = "android", not(feature = "zork")))]
pub mod backends_android_adapter;
#[cfg(feature = "pancurses")]
pub mod backends_pancurses_adapter;
#[cfg(feature = "zork")]
pub mod backends_zork_adapter;

pub use prelude::*;

/// Normalize a delivered mouse button to the toolkit's canonical press.
///
/// GTK reports a middle-press as button 2; the widget API treats a middle
/// click as a primary press (it picks the row under the pointer). Backends
/// deliver this normalized value, and exporting the mapping keeps client
/// tests honest about what actually arrives.
pub fn normalize_press_button(button: u32) -> u32 {
    match button {
        2 => 1,
        other => other,
    }
}
