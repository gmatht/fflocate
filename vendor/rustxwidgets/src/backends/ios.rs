//! iOS backend core: UIKit layer over the shared Apple runtime.
//!
//! This module used to hold the whole iOS backend (ObjC runtime, Foundation
//! marshalling, registries and the UIKit-flavoured entry points) in one file.
//! It now delegates the **Apple-generic** half — the Objective-C runtime,
//! `NSString` marshalling, handle lifetime, callback registry and widget
//! registry — to [`crate::backends::apple`], which is compiled for every
//! Apple platform (`target_vendor = "apple"`) and names no widget class. The
//! macOS AppKit backend builds on the same module.
//!
//! What is genuinely iOS-specific and lives here:
//!
//! * [`display_scale`] — `UIScreen.scale` (1.0 / 2.0 / 3.0). macOS uses
//!   `NSScreen.backingScaleFactor`, so this selector is not shared.
//! * [`init_with_root`] — the typed `UIView*`/`UIViewController*` entry point
//!   (a thin wrapper over [`apple::init_with_root_impl`] so the macOS layer
//!   can hand over an `NSView*`/`NSWindowController*` instead).
//! * [`IosApp`] / [`init_backend`] — the `BackendApp` whose `run()` returns
//!   immediately, because `UIApplicationMain` owns the loop (see
//!   `docs/IOS_GUIDELINES.md` §7).
//!
//! Everything else is re-exported verbatim from [`crate::backends::apple`],
//! so any existing `backends::ios::msg0`, `backends::ios::WidgetMeta`, ... 
//! call site keeps working unchanged and iOS behaviour is identical.
//!
//! See `rustxWidgets/docs/IOS_GUIDELINES.md` and `ios/corro`.

#![allow(dead_code)] // several helpers are used only by some widget kinds

// Re-export the whole shared Apple runtime surface. `pub use ... ::*` keeps
// every previous `backends::ios::<item>` path valid: the adapters, corro's
// host crate and the unit tests below all name these through `ios`.
#[cfg(target_vendor = "apple")]
pub use crate::backends::apple::*;

// `log_ios` was the original name (and is the name the hosts, the docs and
// corro call); the shared module calls it `log_apple`. Keep the iOS name as
// the stable alias so no iOS call site changes.
#[cfg(target_vendor = "apple")]
pub use crate::backends::apple::log_apple as log_ios;

use std::error::Error as StdError;

// ------------------------------------------------------------------
// iOS-specific: root view + display scale + backend entry point
// ------------------------------------------------------------------

/// Hand the (UIKit) backend the host's root view and view controller.
///
/// Mirrors Android's `init_with_layout(env, activity, layout)`: after this,
/// `create_*` builds real UIKit objects as subviews of `root`.
///
/// # Safety
/// `root` and `vc` must be live Objective-C objects (`UIView*` and
/// `UIViewController*`) owned by the caller for the process lifetime.
/// Called from the host cdylib's `corro_ios_root_ready` export.
#[cfg(target_os = "ios")]
pub unsafe fn init_with_root(
    root: *mut std::os::raw::c_void,
    vc: *mut std::os::raw::c_void,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    // SAFETY: forwarded unchanged; the shared layer only calls `-retain`.
    unsafe { crate::backends::apple::init_with_root_impl(root, vc, "ios") }
}

/// Display scale (`UIScreen.scale`: 1.0 on a non-Retina iPhone, 2.0 on a
/// Retina one, 3.0 on the Plus/X era). The iOS analogue of Android's
/// `display_density`: corro multiplies its pixel metrics by this so the
/// grid is finger-sized without changing a desktop call site.
///
/// Returns `None` before init or when `UIScreen` is unavailable, so
/// callers fall back to 1.0 and still render.
#[cfg(target_os = "ios")]
pub fn display_scale() -> Option<f64> {
    use crate::backends::apple::{cls, is_initialized, msg0, msg0c};
    if !is_initialized() {
        return None;
    }
    let screen_cls = cls("UIScreen");
    if screen_cls.is_null() {
        return None;
    }
    unsafe {
        let screen = msg0(screen_cls, "mainScreen");
        if screen.is_null() {
            return None;
        }
        let scale = msg0c(screen, "scale");
        if scale.is_finite() && scale > 0.0 {
            Some(scale)
        } else {
            None
        }
    }
}

/// The iOS `BackendApp`. Like Android's, `run()` returns immediately:
/// the host app calls `UIApplicationMain`, which never returns, and
/// UIKit then drives every callback on the main thread. There is no
/// loop for rswidgets to own — the app *is* the loop.
#[cfg(target_os = "ios")]
pub struct IosApp;

#[cfg(target_os = "ios")]
impl IosApp {
    pub fn new() -> Result<Box<dyn crate::backends::BackendApp>, Box<dyn StdError + Send + Sync>> {
        Ok(Box::new(IosApp))
    }
}

#[cfg(target_os = "ios")]
impl crate::backends::BackendApp for IosApp {
    fn run(self: Box<Self>) -> Result<(), Box<dyn StdError + Send + Sync>> {
        Ok(())
    }
}

/// The iOS backend entry point, matching the shape `backends::init()`
/// expects (`Result<Box<dyn BackendApp>, Box<dyn Error + Send + Sync>>`)
/// — *not* `IosResult<Box<...>>`, which would nest the Result and break
/// every caller that formats the error.
#[cfg(target_os = "ios")]
pub fn init_backend(
) -> Result<Box<dyn crate::backends::BackendApp>, Box<dyn StdError + Send + Sync>> {
    Ok(Box::new(IosApp))
}

// ------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------

#[cfg(test)]
#[cfg(target_os = "ios")]
mod tests {
    use super::*;

    #[test]
    fn test_not_initialized_before_init() {
        assert!(!is_initialized());
        assert!(root_view().is_none());
        assert!(display_scale().is_none());
    }

    // The registry/metadata/canvas-id tests now live with the shared code in
    // `backends/apple.rs`, where both the iOS and macOS layers exercise them.
}
