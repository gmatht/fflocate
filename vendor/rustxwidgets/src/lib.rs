//! rswidgets: cross-platform thin GUI abstraction (GTK-dlopen on Linux, NWG on Windows)
#![warn(missing_docs)]
#![allow(missing_docs)] // many backend items are undocumented; silence the noisy per-item warnings

pub mod prelude;
pub mod core;
pub mod common;
pub mod compat;
// Shared backend-agnostic model types at the crate root (menu model,
// events, actions, sizers, message boxes). Backends and apps refer to
// these as `crate::MenuItem` etc.; never put app-specific content here.
pub use core::{Action, Align, CallbackResult, Event, LayoutDir, Label, MessageBoxKind, MessageBoxResult, MenuItem, Sizer, SizerChild, SizerFlags};
// i18n mechanism (deferred labels + translator hook). No translated text lives
// in this crate; the app owns the catalog.
pub use core::{locale_is_rtl, set_layout_direction, set_layout_direction_hook, set_translator, tr};
// Portable periodic tick (GTK timeout / Win32 WM_TIMER) — see core::add_periodic_tick.
pub use core::add_periodic_tick;
pub mod spreadsheet;
pub mod gridview;
pub mod overflow;
pub mod tabbar;

/// Re-export the dynamic GTK loader so host apps using the `gtk` backend can reach
/// raw symbols (signal wiring, event state, ...) without depending on it directly.
#[cfg(feature = "gtk")]
pub use gtk_dynamic_loader;
pub mod lifecycle_stress;
/// Portable Win32-backend helpers (pure logic + tests, host-independent).
/// Public so host apps can share the Windows-input predicates.
pub mod win32_portable;
pub mod backends;
#[cfg(all(feature = "gtk4-rs", target_os = "linux", not(feature = "zork")))]
pub mod backends_gtk_adapter {
    // The gtk4 adapter uses dlopen-loaded sys crates via gtk4-rs.
    // Source is in backends_gtk4_adapter.rs
    include!("backends_gtk4_adapter.rs");
}
// NOTE: the GTK adapter module stays available alongside `pancurses`
// (combined-gui builds need both: native GUI + terminal fallback). Only the
// `backends::init` re-export is priority-gated (see backends/mod.rs).
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs")))]
mod backends_gtk_adapter_impl;
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs")))]
pub mod backends_gtk_adapter;
// Available on all Windows builds (not just non-pancurses ones) so
// combined gui+pancurses builds can drive the native GUI side by side
// with the terminal, mirroring the gtk adapter on Linux.
#[cfg(all(windows, not(feature = "zork")))]
pub mod backends_nwg_adapter;
#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
pub mod backends_wasm_adapter;
#[cfg(all(target_os = "android", not(feature = "zork")))]
pub mod backends_android_adapter;
/// iOS backend adapter (UIKit over the Objective-C runtime) — see
/// `rustxWidgets/docs/IOS_GUIDELINES.md` and `ios/corro`.
#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub mod backends_ios_adapter;
/// macOS backend adapter (AppKit over the *same* Objective-C runtime layer the
/// iOS adapter uses — see `backends/apple.rs`) — see
/// `rustxWidgets/docs/MACOS_GUIDELINES.md`.
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub mod backends_macos_adapter;
/// Android resource generation for downstream app crates: a `build.rs` can
/// call [`android_generator::run`] to materialise a Material 3 theme, the
/// palette it references, an adaptive launcher icon and a manifest when the
/// app has not provided its own. Additive by design (never overwrites).
/// See `docs/ANDROID_GUIDELINES.md`.
///
/// There is deliberately no iOS counterpart: an iOS app is defined by its
/// Xcode project, `Info.plist` and asset catalog, which the Rust side cannot
/// sensibly synthesise. See `docs/IOS_GUIDELINES.md` §1.
pub mod android_generator;
/// Generate the Objective-C forwarding shims the Apple backends send messages
/// to (the Rust<->ObjC ABI contract, derived from one table), so the two sides
/// cannot drift. The canvas view and text measurer stay hand-written
/// deliberately — their bodies are behaviour, not forwarding. See
/// `rustxWidgets/docs/MACOS_GUIDELINES.md` §3 and `IOS_GUIDELINES.md` §3.
pub mod apple_generator;
#[cfg(feature = "pancurses")]
pub mod backends_pancurses_adapter;
#[cfg(feature = "pancurses")]
pub use crate::backends::pancurses::set_frame_hook;
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
