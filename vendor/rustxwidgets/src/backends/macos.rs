//! macOS backend core: AppKit layer over the shared Apple runtime.
//!
//! The macOS sibling of [`crate::backends::ios`]. Both delegate the
//! Apple-generic half — Objective-C runtime, `NSString` marshalling, handle
//! lifetime, callback registry, widget registry — to
//! [`crate::backends::apple`]; what lives here is what is genuinely AppKit:
//!
//! * [`display_scale`] — `NSScreen.backingScaleFactor` (1.0 / 2.0). iOS uses
//!   `UIScreen.scale`, so the selector is different even though the idea is
//!   the same.
//! * [`init_with_root`] — the typed `NSView*`/`NSWindowController*` entry
//!   point (a thin wrapper over [`apple::init_with_root_impl`]).
//! * [`MacosApp`] / [`init_backend`] — the `BackendApp`.
//!
//! ## Who owns the run loop
//!
//! On iOS `UIApplicationMain` never returns and the host app is the loop.
//! On macOS a plain `NSApplication` app has *two* valid shapes, and this
//! backend supports both:
//!
//! * **Host-driven** (the iOS/Android shape): a host `NSApplicationDelegate`
//!   calls [`init_with_root`] from `applicationDidFinishLaunching:` and then
//!   `-[NSApp run]`. [`MacosApp::run`] returns immediately, exactly like
//!   `IosApp::run`, because `NSApp` owns the loop; the app is leaked.
//! * **Self-driven** (a plain binary, which is what `ff-gui` is): the first
//!   `create_window` finds no host root, so it creates the shared
//!   `NSApplication` and an `NSWindow`, initialises the layer with that
//!   window's content view, and orders it front. [`MacosApp::run`] then runs
//!   `-[NSApplication run]` on the caller's thread and blocks there, the same
//!   contract the GTK backend gives `g_main_loop_run`.
//!
//! See `rustxWidgets/docs/MACOS_GUIDELINES.md` and `rustxWidgets/docs/IOS_GUIDELINES.md`.

#![allow(dead_code)] // several helpers are used only by some widget kinds

// Re-export the shared Apple runtime surface, so a macOS host can reach the
// same names the iOS host does (`backends::macos::msg0`, `WidgetMeta`, ...).
#[cfg(target_os = "macos")]
pub use crate::backends::apple::*;

// `log_macos` mirrors the iOS `log_ios` alias; the shared module calls it
// `log_apple` (NSLog works on both platforms).
#[cfg(target_os = "macos")]
pub use crate::backends::apple::log_apple as log_macos;

use std::error::Error as StdError;

// ------------------------------------------------------------------
// macOS-specific: root view + display scale + backend entry point
// ------------------------------------------------------------------

/// Hand the (AppKit) backend the host's root view and window controller.
///
/// Mirrors Android's `init_with_layout(env, activity, layout)` and iOS's
/// `init_with_root(root, vc)`: after this, `create_*` builds real AppKit
/// objects as subviews of `root` (typically the window's `contentView`).
///
/// # Safety
/// `root` and `vc` must be live Objective-C objects (`NSView*` and
/// `NSWindowController*`) owned by the caller for the process lifetime.
/// Called from the host's root-ready entry point.
#[cfg(target_os = "macos")]
pub unsafe fn init_with_root(
    root: *mut std::os::raw::c_void,
    vc: *mut std::os::raw::c_void,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    // SAFETY: forwarded unchanged; the shared layer only calls `-retain`.
    unsafe { crate::backends::apple::init_with_root_impl(root, vc, "macos") }
}

/// Display scale (`NSScreen.backingScaleFactor`: 1.0 on a non-Retina display,
/// 2.0 on Retina). The macOS analogue of iOS's `UIScreen.scale` and Android's
/// `display_density`: corro multiplies its pixel metrics by this so chrome is
/// legible on a Retina panel without changing a desktop call site.
///
/// Returns `None` before init or when `NSScreen` is unavailable, so callers
/// fall back to 1.0 and still render.
#[cfg(target_os = "macos")]
pub fn display_scale() -> Option<f64> {
    use crate::backends::apple::{cls, is_initialized, msg0, msg0c};
    if !is_initialized() {
        return None;
    }
    let screen_cls = cls("NSScreen");
    if screen_cls.is_null() {
        return None;
    }
    unsafe {
        // `[NSScreen mainScreen]` can be nil when no display is attached (a
        // headless CI box); fall back to `[NSScreen screens][0]` and finally
        // give up, so the caller uses 1.0.
        let mut screen = msg0(screen_cls, "mainScreen");
        if screen.is_null() {
            let screens = msg0(screen_cls, "screens");
            if screens.is_null() {
                return None;
            }
            screen = msg0(screens, "firstObject");
            if screen.is_null() {
                return None;
            }
        }
        let scale = msg0c(screen, "backingScaleFactor");
        if scale.is_finite() && scale > 0.0 {
            Some(scale)
        } else {
            None
        }
    }
}

/// The macOS `BackendApp`. Like iOS's `IosApp`, `run()` returns immediately:
/// the host app owns `NSApplication` and calls `-[NSApp run]`, which never
/// returns; AppKit then drives every callback on the main thread. There is no
/// loop for rswidgets to own — the app *is* the loop.
#[cfg(target_os = "macos")]
pub struct MacosApp;

#[cfg(target_os = "macos")]
impl MacosApp {
    pub fn new() -> Result<Box<dyn crate::backends::BackendApp>, Box<dyn StdError + Send + Sync>> {
        Ok(Box::new(MacosApp))
    }
}

#[cfg(target_os = "macos")]
impl crate::backends::BackendApp for MacosApp {
    fn run(self: Box<Self>) -> Result<(), Box<dyn StdError + Send + Sync>> {
        // Self-driven shape (a plain binary such as `ff-gui`): the backend made
        // its own `NSWindow`, so it owns `-[NSApplication run]` too, which blocks
        // until the app terminates.
        //
        // Host-driven shape (an `.app` whose delegate calls `init_with_root`):
        // the host runs `-[NSApp run]` itself, so this returns immediately.
        if crate::backends_macos_adapter::owns_run_loop() {
            unsafe { crate::backends_macos_adapter::run_app_loop() };
        }
        Ok(())
    }
}

/// The macOS backend entry point, matching the shape `backends::init()`
/// expects (`Result<Box<dyn BackendApp>, Box<dyn Error + Send + Sync>>`).
#[cfg(target_os = "macos")]
pub fn init_backend(
) -> Result<Box<dyn crate::backends::BackendApp>, Box<dyn StdError + Send + Sync>> {
    Ok(Box::new(MacosApp))
}

// ------------------------------------------------------------------
// Timers
// ------------------------------------------------------------------

/// Run `cb` every `ms` milliseconds on the main run loop until it returns
/// `false`.
///
/// `NSTimer scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:`
/// needs an ObjC target with a method to call, so this registers a one-method
/// class the first time it is used and stores the Rust closure on the instance
/// (associated object, `ASSIGN` — the box is owned here, not by ObjC). The
/// timer is scheduled on the *current* run loop, which for a GUI app is the
/// main thread's; call this from there.
#[cfg(target_os = "macos")]
pub fn timeout_add_repeating(
    ms: u32,
    cb: Box<dyn FnMut() -> bool>,
) -> Result<(), crate::core::Error> {
    use crate::backends::apple::{cls, msg0, msg0v, msg_shim, selector};
    use std::ffi::CString;
    use std::os::raw::{c_char, c_void};

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_allocateClassPair(
            superclass: *mut c_void,
            name: *const c_char,
            extra_bytes: usize,
        ) -> *mut c_void;
        fn objc_registerClassPair(cls: *mut c_void);
        fn class_addMethod(
            cls: *mut c_void,
            name: *mut c_void,
            imp: *const c_void,
            types: *const c_char,
        ) -> bool;
        fn objc_getAssociatedObject(obj: *mut c_void, key: *const c_void) -> *mut c_void;
        fn objc_setAssociatedObject(obj: *mut c_void, key: *const c_void, value: *mut c_void, policy: usize);
    }

    // OBJC_ASSOCIATION_ASSIGN: the value is a plain Rust box, not an ObjC
    // object, so the runtime must not try to `retain` it.
    const ASSIGN: usize = 0;
    static ASSOC_KEY: u8 = 0;

    struct Tick(Box<dyn FnMut() -> bool>);

    /// `-[RustxTickTarget tick:]`. Resolves the closure, runs it, and
    /// invalidates the timer (freeing the closure) when it returns `false`.
    extern "C" fn tick_imp(obj: *mut c_void, _cmd: *mut c_void, timer: *mut c_void) {
        unsafe {
            let key = &ASSOC_KEY as *const u8 as *const c_void;
            let state = objc_getAssociatedObject(obj, key) as *mut Tick;
            if state.is_null() {
                return;
            }
            let keep = ((*state).0)();
            if !keep {
                msg0v(timer, "invalidate");
                objc_setAssociatedObject(obj, key, std::ptr::null_mut(), ASSIGN);
                drop(Box::from_raw(state));
            }
        }
    }

    // One class for every timer in the process; a second call reuses it.
    let class_name = CString::new("RustxTickTarget").expect("literal");
    let mut class = cls("RustxTickTarget");
    if class.is_null() {
        class = unsafe { objc_allocateClassPair(cls("NSObject"), class_name.as_ptr(), 0) };
        if class.is_null() {
            return Err(crate::core::Error::Backend(
                "cannot create the NSTimer target class".to_string(),
            ));
        }
        let types = CString::new("v@:@").expect("literal");
        unsafe {
            class_addMethod(
                class,
                selector("tick:"),
                tick_imp as *const () as *const c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(class);
        }
    }

    let target = unsafe { msg0(msg0(class, "alloc"), "init") };
    if target.is_null() {
        return Err(crate::core::Error::Backend(
            "cannot allocate the NSTimer target".to_string(),
        ));
    }
    let state = Box::into_raw(Box::new(Tick(cb)));
    unsafe {
        objc_setAssociatedObject(
            target,
            &ASSOC_KEY as *const u8 as *const c_void,
            state as *mut c_void,
            ASSIGN,
        );
    }

    // id objc_msgSend(id, SEL, double, id, SEL, id, BOOL)
    let send: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        f64,
        *mut c_void,
        *mut c_void,
        *mut c_void,
        bool,
    ) -> *mut c_void = unsafe { std::mem::transmute(msg_shim()) };
    let timer = unsafe {
        send(
            cls("NSTimer"),
            selector("scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:"),
            f64::from(ms.max(1)) / 1000.0,
            target,
            selector("tick:"),
            std::ptr::null_mut(),
            true,
        )
    };
    if timer.is_null() {
        return Err(crate::core::Error::Backend(
            "NSTimer scheduling failed (no run loop on this thread?)".to_string(),
        ));
    }
    // The run loop keeps a scheduled timer alive, but the handle is ours to
    // hold: retaining it keeps the invalidation working if the loop is torn
    // down before the first fire.
    unsafe { msg0v(timer, "retain") };
    Ok(())
}

// ------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    #[test]
    fn test_not_initialized_before_init() {
        assert!(!is_initialized());
        assert!(root_view().is_none());
        assert!(display_scale().is_none());
    }
}
