//! Shared Apple backend core: Objective-C runtime + Foundation marshalling,
//! handle lifetime, callback registry and widget registry.
//!
//! This module is the **Apple half** of what used to live entirely in
//! `backends/ios.rs`. It is compiled for every Apple platform
//! (`target_vendor = "apple"`: iOS, macOS, and the rest) and is deliberately
//! UIKit/AppKit-free: nothing here names a `UI*` or `NS*` widget class. The
//! platform adapters build on top of it:
//!
//! * `backends/ios.rs` — the UIKit layer (iOS): the `UIScreen` scale, the
//!   `UIView`/`UIViewController` root, and the `IosApp` entry point.
//! * `backends_macos_adapter.rs` — the hand-written AppKit adapter (macOS)
//!   over this same runtime layer.
//!
//! What is shared, and why it maps 1:1 to the Android module
//! (`backends/android.rs`):
//!
//! * **ObjC runtime** — `objc_msgSend` cast per ABI shape, `objc_getClass`,
//!   `sel_registerName`, `objc_retain`. The Objective-C runtime is the same
//!   library on iOS and macOS (and Apple's `CGFloat` split — `f64` on 64-bit,
//!   `f32` on 32-bit — is Apple-wide, not iOS-wide), so this is genuinely
//!   shared code, not two copies.
//! * **Foundation marshalling** — `NSString` <-> Rust `String` via
//!   `stringWithUTF8String:` / `UTF8String`. Foundation exists on both.
//! * **Handle lifetime** — Objective-C objects are retained by us and never
//!   released (`retain()` on the way in, `KEEP_ALIVE` is belt and braces).
//!   The framework keeps its hierarchy alive, but callbacks can hold handles
//!   after a view is removed, so owning a retain is what makes a dangling
//!   pointer impossible. Releasing on drop is deliberately *not* done:
//!   widgets are `Copy`-like handles shared across closures (same decision as
//!   Android's leaked `GlobalRef` list).
//! * **Callback registry** — Rust closures are keyed by a `u64` id; the ObjC
//!   side calls back into `#[no_mangle] extern "C"` entry points in the host
//!   binary/cdylib, which dispatch by id here. This is the
//!   `RustCallback.java` trampoline from Android, minus the JVM.
//! * **Widget registry** — a pointer-keyed map from handle to a small
//!   `WidgetMeta` (kind, text, expanding, min chars, canvas id). Android
//!   keeps the equivalent state in the View objects themselves; ObjC objects
//!   are harder to stash Rust state on, so we keep it beside them.
//!
//! Every function here is a **no-op before [`init`]** (host unit tests, the
//! desktop `examples/ios_ui.rs` run, headless builds), exactly like Android's
//! `with_env_and_activity` returning `Err` and callers ignoring it — so the
//! shared GUI code compiles and runs unchanged on a host without the
//! framework.

#![allow(dead_code)] // several helpers are used only by some widget kinds

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::sync::Mutex;

// ------------------------------------------------------------------
// ObjC runtime bindings
// ------------------------------------------------------------------
//
// Declared by hand rather than through the `objc2` crates on purpose:
// `objc_msgSend` is the only entry point needed, the selector names are
// strings in Apple's ABI (stable since the first iPhone OS), and this keeps
// the dependency list at zero — which matters for the armv7s / iOS 7 build
// path in `docs/IOS_GUIDELINES.md` §9 (an old SDK plus old Rust toolchain
// plus a heavy ObjC binding stack is exactly where the 32-bit build gets
// fragile). The same hand-rolled FFI is the house style on the other
// platforms here (the `gtk_dynamic_loader` crate on Linux, the vendored
// `native-windows-gui` on Windows).
//
// `objc_msgSend` is variadic and *must* be cast to the exact function
// signature for each call (the ABI differs per return type: `CGFloat` floats
// come back in different registers than integers on armv7).

#[link(name = "objc")]
unsafe extern "C" {
    /// The single message-send entry point. Never call it directly:
    /// use the typed wrappers below, which pick the right signature.
    fn objc_msgSend();
    /// `id objc_getClass(const char *name)`.
    fn objc_getClass(name: *const std::os::raw::c_char) -> *mut std::os::raw::c_void;
    /// `SEL sel_registerName(const char *)`.
    fn sel_registerName(name: *const std::os::raw::c_char) -> *mut std::os::raw::c_void;
    /// `id objc_retain(id)`.
    fn objc_retain(obj: *mut std::os::raw::c_void) -> *mut std::os::raw::c_void;
}

/// Look up a class by name (`UIView`, `NSWindow`, ...). Returns null when
/// the class does not exist in this SDK.
pub fn cls(name: &str) -> *mut std::os::raw::c_void {
    let mut buf = Vec::with_capacity(name.len() + 1);
    buf.extend_from_slice(name.as_bytes());
    buf.push(0);
    unsafe { objc_getClass(buf.as_ptr() as *const std::os::raw::c_char) }
}

/// Register (or look up) a selector. Safe for constant selector names;
/// for the handful of hot ones we go through this each call, which is
/// what the frameworks themselves do on first use.
fn sel(name: &str) -> *mut std::os::raw::c_void {
    let mut buf = Vec::with_capacity(name.len() + 1);
    buf.extend_from_slice(name.as_bytes());
    buf.push(0);
    unsafe { sel_registerName(buf.as_ptr() as *const std::os::raw::c_char) }
}

/// The raw `objc_msgSend` entry point, for callers that must transmute
/// it to their own exact signature (the adapters' host-shim calls do
/// this; a Rust `extern "C"` declaration cannot express a variadic
/// function pointer, so the address is exposed instead of the symbol).
pub fn msg_shim() -> *const () {
    objc_msgSend as *const ()
}

/// `sel_registerName` for a selector name. Public so the adapters can pass
/// selectors into host-shim calls without re-declaring the runtime.
pub fn selector(name: &str) -> *mut std::os::raw::c_void {
    sel(name)
}

// Typed `objc_msgSend` wrappers. Each names its exact signature; do NOT
// add a generic one (that is the classic arm64 crash: the register file
// is not preserved across a mismatched call).

/// `id (*)(id, SEL)`
pub unsafe fn msg0(obj: *mut std::os::raw::c_void, selname: &str) -> *mut std::os::raw::c_void {
    let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void) -> *mut std::os::raw::c_void =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname)) }
}

/// `void (*)(id, SEL)`
pub unsafe fn msg0v(obj: *mut std::os::raw::c_void, selname: &str) {
    let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname)) }
}

/// `id (*)(id, SEL, id)`
pub unsafe fn msg1(
    obj: *mut std::os::raw::c_void,
    selname: &str,
    a: *mut std::os::raw::c_void,
) -> *mut std::os::raw::c_void {
    let f: unsafe extern "C" fn(
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
    ) -> *mut std::os::raw::c_void = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), a) }
}

/// `void (*)(id, SEL, id)`
pub unsafe fn msg1v(obj: *mut std::os::raw::c_void, selname: &str, a: *mut std::os::raw::c_void) {
    let f: unsafe extern "C" fn(
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
    ) = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), a) }
}

/// `void (*)(id, SEL, id, NSUInteger)`
pub unsafe fn msg2v(
    obj: *mut std::os::raw::c_void,
    selname: &str,
    a: *mut std::os::raw::c_void,
    b: u64,
) {
    let f: unsafe extern "C" fn(
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
        u64,
    ) = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), a, b) }
}

/// `void (*)(id, SEL, BOOL)`
pub unsafe fn msg1bv(obj: *mut std::os::raw::c_void, selname: &str, v: bool) {
    let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void, bool) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), v) }
}

/// `void (*)(id, SEL, NSInteger)` (`NSInteger` is a pointer-sized int on
/// every Apple target: 32-bit armv7 and 64-bit arm64 alike).
pub unsafe fn msg1iv(obj: *mut std::os::raw::c_void, selname: &str, v: isize) {
    let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void, isize) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), v) }
}

/// `void (*)(id, SEL, id, NSInteger)` — a setter taking an object and a
/// state/flag integer, e.g. `-setTitle:forState:`.
///
/// Needed as its own wrapper because [`msg1v`] passes exactly one argument.
/// Sending a two-part selector through it leaves the second register
/// whatever the call happened to hold, so `forState:` arrives as garbage —
/// and if the value is out of range for the state enum, UIKit raises, which
/// is a *foreign* exception no `catch_unwind` can intercept.
pub unsafe fn msg1vi(
    obj: *mut std::os::raw::c_void,
    selname: &str,
    a: *mut std::os::raw::c_void,
    b: isize,
) {
    let f: unsafe extern "C" fn(
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
        isize,
    ) = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), a, b) }
}

/// `void (*)(id, SEL, CGFloat)` — setters taking a point value
/// (`setHeadIndent:`, `constraintEqualToConstant:`). Split by width for the
/// same reason as [`msg4cv`]: `CGFloat` is `double` on 64-bit and `float` on
/// 32-bit, and sending the wrong one corrupts the register file.
#[cfg(target_pointer_width = "64")]
pub unsafe fn msg1cv(obj: *mut std::os::raw::c_void, selname: &str, v: f64) {
    let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void, f64) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), v) }
}

/// 32-bit (armv7s) `CGFloat` is `float`.
#[cfg(target_pointer_width = "32")]
pub unsafe fn msg1cv(obj: *mut std::os::raw::c_void, selname: &str, v: f64) {
    let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void, f32) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), v as f32) }
}

/// `void (*)(id, SEL, CGFloat, CGFloat)` — two doubles, for a `CGSize` or
/// `CGPoint` argument. `CGFloat` is `float` on 32-bit and `double` on 64-bit,
/// so the width is selected at compile time exactly as in `msg4cv` below.
#[cfg(target_pointer_width = "64")]
pub unsafe fn msg2cv(obj: *mut std::os::raw::c_void, selname: &str, a: f64, b: f64) {
    let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void, f64, f64) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), a, b) }
}

/// 32-bit (armv7s) `CGFloat` is `float`.
#[cfg(target_pointer_width = "32")]
pub unsafe fn msg2cv(obj: *mut std::os::raw::c_void, selname: &str, a: f64, b: f64) {
    let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void, f32, f32) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), a as f32, b as f32) }
}

/// `void (*)(id, SEL, CGFloat, CGFloat, CGFloat, CGFloat)`. `CGFloat` is
/// `float` on 32-bit and `double` on 64-bit — the single most important
/// ABI difference between the arm64 and armv7s builds, so it is selected
/// at compile time rather than assumed.
#[cfg(target_pointer_width = "64")]
pub unsafe fn msg4cv(
    obj: *mut std::os::raw::c_void,
    selname: &str,
    a: f64,
    b: f64,
    c: f64,
    d: f64,
) {
    let f: unsafe extern "C" fn(
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
        f64,
        f64,
        f64,
        f64,
    ) = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), a, b, c, d) }
}

/// 32-bit (armv7s) `CGFloat` is `float`.
#[cfg(target_pointer_width = "32")]
pub unsafe fn msg4cv(
    obj: *mut std::os::raw::c_void,
    selname: &str,
    a: f64,
    b: f64,
    c: f64,
    d: f64,
) {
    let f: unsafe extern "C" fn(
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
        f32,
        f32,
        f32,
        f32,
    ) = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), a as f32, b as f32, c as f32, d as f32) }
}

/// `id (*)(id, SEL, NSInteger)` — factory initializers taking an integer
/// (e.g. `buttonWithType:`).
pub unsafe fn msg1i(
    obj: *mut std::os::raw::c_void,
    selname: &str,
    v: isize,
) -> *mut std::os::raw::c_void {
    let f: unsafe extern "C" fn(
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
        isize,
    ) -> *mut std::os::raw::c_void = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), v) }
}

/// `CGFloat (*)(id, SEL)` — the `double` case (arm64).
#[cfg(target_pointer_width = "64")]
pub unsafe fn msg0c(obj: *mut std::os::raw::c_void, selname: &str) -> f64 {
    let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void) -> f64 =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname)) }
}

/// `CGFloat (*)(id, SEL)` — the `float` case (armv7s).
#[cfg(target_pointer_width = "32")]
pub unsafe fn msg0c(obj: *mut std::os::raw::c_void, selname: &str) -> f64 {
    let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void) -> f32 =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname)) as f64 }
}

/// `NSInteger (*)(id, SEL)`
pub unsafe fn msg0i(obj: *mut std::os::raw::c_void, selname: &str) -> isize {
    let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void) -> isize =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname)) }
}

/// `id (*)(id, SEL, id, SEL)` — `performSelector:withObject:` on the
/// canvas view, used to let the host view drive its own redraw.
pub unsafe fn msg2(
    obj: *mut std::os::raw::c_void,
    selname: &str,
    a: *mut std::os::raw::c_void,
    b: *mut std::os::raw::c_void,
) -> *mut std::os::raw::c_void {
    let f: unsafe extern "C" fn(
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
        *mut std::os::raw::c_void,
    ) -> *mut std::os::raw::c_void = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel(selname), a, b) }
}

/// `id alloc` + `id init` on a class: `[[Cls alloc] init]`.
pub fn alloc_init(class: *mut std::os::raw::c_void) -> *mut std::os::raw::c_void {
    if class.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let obj = msg0(class, "alloc");
        if obj.is_null() {
            return std::ptr::null_mut();
        }
        let obj = msg0(obj, "init");
        if obj.is_null() {
            return std::ptr::null_mut();
        }
        retain(obj)
    }
}

/// Retain an object and hand back the same pointer (handle ownership).
pub fn retain(obj: *mut std::os::raw::c_void) -> *mut std::os::raw::c_void {
    if obj.is_null() {
        return obj;
    }
    unsafe { objc_retain(obj) }
}

/// An `NSString` from a Rust `&str`. Returns a retained object (owned by
/// the autorelease pool otherwise; we keep it simple and retain).
///
/// `stringWithUTF8String:` is available from iOS 2.0 / OS X 10.0, so this is
/// safe on the oldest supported path.
pub fn nsstring(s: &str) -> *mut std::os::raw::c_void {
    let class = cls("NSString");
    if class.is_null() {
        return std::ptr::null_mut();
    }
    let mut buf: Vec<u8> = Vec::with_capacity(s.len() + 1);
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
    unsafe {
        let f: unsafe extern "C" fn(
            *mut std::os::raw::c_void,
            *mut std::os::raw::c_void,
            *const std::os::raw::c_char,
        ) -> *mut std::os::raw::c_void =
            std::mem::transmute(objc_msgSend as *const ());
        let obj = f(
            class,
            sel("stringWithUTF8String:"),
            buf.as_ptr() as *const std::os::raw::c_char,
        );
        retain(obj)
    }
}

/// Read an `NSString*` back as a Rust `String` (`UTF8String` + `strlen`;
/// no `CStr` dependency needed).
pub fn nsstring_to_rust(obj: *mut std::os::raw::c_void) -> Option<String> {
    if obj.is_null() {
        return None;
    }
    unsafe {
        let ptr = msg0(obj, "UTF8String") as *const u8;
        if ptr.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let bytes = std::slice::from_raw_parts(ptr, len);
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
}

// ------------------------------------------------------------------
// Backend state
// ------------------------------------------------------------------

/// The host's root view. Set by this module's `init_with_root` (called from
/// the platform layer: iOS's `UIScreen`/`UIView` root, AppKit's
/// `NSWindow.contentView`); all widget creation hangs off it.
static ROOT_VIEW: Mutex<Option<usize>> = Mutex::new(None);
/// The host's view controller / window controller, for presenting dialogs.
static VIEW_CONTROLLER: Mutex<Option<usize>> = Mutex::new(None);

/// Retains applied by this module, kept for the process lifetime. The
/// framework already owns its hierarchy; this list exists so a handle
/// captured in a Rust closure stays valid even after the view leaves the
/// hierarchy (removeFromSuperview does not deallocate while we hold a
/// retain).
static KEEP_ALIVE: Lazy<Mutex<Vec<usize>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// Backend initialisation state. `true` after the host handed us its
/// root view; every factory returns a null handle before that, which the
/// adapters turn into inert no-ops (Android's behaviour when JNI is not
/// attached).
static INITIALISED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// True once `init_with_root` has run. Host builds and tests see false,
/// so the whole widget tree stays inert instead of panicking.
pub fn is_initialized() -> bool {
    INITIALISED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Hand the backend the host's root view (and its controller).
///
/// Mirrors Android's `init_with_layout(env, activity, layout)`: after
/// this, `create_*` builds real framework objects as subviews of `root`.
/// Idempotent-safe in the sense that a second call replaces the stored
/// root (matching the single-window model, where a second call would
/// mean a second window — not something this backend supports).
///
/// The `label` is used in the error message and the init log line so the
/// platform layer (iOS vs macOS) is identifiable in a crash report; it is
/// purely cosmetic.
///
/// # Safety
/// `root` and `vc` must be live Objective-C objects owned by the caller for
/// the process lifetime (iOS: `UIView*` + `UIViewController*`; macOS:
/// `NSView*` + `NSWindowController*`). Called from the host's root-ready
/// entry point.
pub unsafe fn init_with_root_impl(
    root: *mut std::os::raw::c_void,
    vc: *mut std::os::raw::c_void,
    label: &str,
) -> Result<(), Box<dyn StdError + Send + Sync>> {
    if root.is_null() {
        return Err(format!("{label} backend init: null root view").into());
    }
    // SAFETY: the caller guarantees both are live ObjC objects; the
    // message sends below only call `-retain` through objc_retain.
    let root = retain(root);
    let vc = if vc.is_null() {
        std::ptr::null_mut()
    } else {
        retain(vc)
    };
    {
        let mut keep = KEEP_ALIVE.lock().unwrap();
        keep.push(root as usize);
        if !vc.is_null() {
            keep.push(vc as usize);
        }
    }
    *ROOT_VIEW.lock().unwrap() = Some(root as usize);
    *VIEW_CONTROLLER.lock().unwrap() = if vc.is_null() { None } else { Some(vc as usize) };
    INITIALISED.store(true, std::sync::atomic::Ordering::Relaxed);
    log_apple(&format!("{label} backend initialised"));
    Ok(())
}

/// The host's root view, or `None` when the backend was never initialised.
pub fn root_view() -> Option<*mut std::os::raw::c_void> {
    ROOT_VIEW.lock().unwrap().map(|p| p as *mut std::os::raw::c_void)
}

/// The host's controller (for presentation, e.g. iOS
/// `presentViewController:`), if given.
pub fn view_controller() -> Option<*mut std::os::raw::c_void> {
    VIEW_CONTROLLER.lock().unwrap().map(|p| p as *mut std::os::raw::c_void)
}

/// Retain a handle created by this module and record it in `KEEP_ALIVE`.
pub fn own(obj: *mut std::os::raw::c_void) -> *mut std::os::raw::c_void {
    let obj = retain(obj);
    if !obj.is_null() {
        KEEP_ALIVE.lock().unwrap().push(obj as usize);
    }
    obj
}

/// Write a debug line to the system log under the `rswidgets` tag.
/// Best-effort: silently dropped when the backend is not initialised.
///
/// `NSLog` (Foundation, a plain C function — not a message send) exists on
/// every Apple platform and writes to the device console (iOS: `xcrun simctl
/// spawn booted log stream`; macOS: Console.app / `log stream`) — the
/// equivalent of Android's `logcat -s rswidgets`.
pub fn log_apple(msg: &str) {
    // NOTE: deliberately NOT gated on `is_initialized()`.
    //
    // It used to be, on the theory that logging before the backend is up has
    // nowhere to go. In practice that discarded exactly the messages worth
    // having: the host logs "ios_main: init_with_root" BEFORE initialising, so
    // the guard swallowed it, and a startup failure then produced a silent
    // abort with no indication of how far it got. stderr exists from process
    // start, so there is always somewhere to go.
    //
    // Three earlier attempts at this call also died here, each differently, so
    // the mechanism is spelled out rather than left obvious-looking:
    //   1. a runtime `char*` as the FORMAT (`NSLog(fmt, msg)`) segfaults on
    //      arm64 macOS/Simulator;
    //   2. a format built from `concat!(...).as_bytes()` was a temporary
    //      dropped before the variadic call - a dangling pointer;
    //   3. even `NSLog(@"%@", nsstring)` aborted inside
    //      objc_msgSend/_CFLogvEx3: Rust's variadic FFI does not marshall
    //      object arguments the way Clang does, so Foundation's printf-style
    //      functions are not safely callable from Rust at all.
    //
    // Hence a direct write(2), tagged like the Android backend's logcat so the
    // documented greps still work.
    let mut line = String::with_capacity(msg.len() + 12);
    line.push_str("[rswidgets] ");
    line.push_str(msg);
    line.push('\n');
    unsafe {
        unsafe extern "C" {
            fn write(fd: i32, buf: *const std::os::raw::c_void, count: usize) -> isize;
        }
        // Also append to a file, in the app's own container.
        //
        // The iOS CI app is launched with `simctl launch --console-pty`, so its
        // stderr is a PTY with a small fixed buffer that simctl drains only as
        // fast as it can. Diagnostics come out in bursts - a commit repaints
        // the whole grid - and a burst that outruns the reader is *lost*,
        // silently. That is not hypothetical: with only stderr, the arming
        // line itself was missing from the log while the per-edit lines that
        // came after it were present, which is exactly backwards and made the
        // failure look like a hang when it was output loss.
        //
        // A file cannot fill and is not drained by anything, so this path is
        // where the log has to come from. Best-effort: if the container has no
        // writable directory the stderr line still stands.
        // Log to BOTH `/tmp` and the app container's own `tmp`, because they
        // are not the same directory on iOS: an app's `/tmp` is already its
        // container's, but a host-side reader that wants the file has to know
        // the container path, and `get_app_container` needs a bundle id that
        // is only resolved at build time. Writing both means the reader does
        // not have to be right about any of that - it can find whichever one
        // it can actually see.
        //
        // `std::fs::OpenOptions` rather than a hand-declared `open(2)`: the
        // C name `open` is reserved in Rust, and both escapes from that fail
        // differently - renaming it asks the linker for a symbol that does not
        // exist (the iOS build failed with an undefined `_libc_open`), and
        // `#[link_name = "open"]` is rejected outright. The standard library
        // already has the right name for this, and it is a one-line call.
        use std::io::Write;
        for path in ["/tmp/corro-app.log", "./corro-app.log"] {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = f.write_all(line.as_bytes());
            }
        }
        write(
            2,
            line.as_ptr() as *const std::os::raw::c_void,
            line.len(),
        );
    }
}

// ------------------------------------------------------------------
// Callback registry
// ------------------------------------------------------------------

/// A main-thread-only closure, storable in a `static`.
///
/// The registry holds these in a `Mutex` for interior mutability (the map is
/// shared, and `dispatch_callback` needs `&mut`), and Rust therefore demands
/// `Send`. Dispatch is in fact single-threaded on the UI main thread - every
/// callback in both Apple backends comes from UIKit/AppKit or from an NSTimer
/// on the main run loop - so `Send` is not a property these closures have; it
/// is a property the *storage* wants.
///
/// Rather than require every registration site to prove a thread-safety they do
/// not have (which pushed the iOS periodic tick into a chain of `Send` wrapper
/// types), the assertion is made once, here, where the reasoning belongs.
struct StoredCallback(Box<dyn FnMut()>);
// SAFETY: `dispatch_callback` is only ever called from the main thread (UIKit
// delegates, the canvas draw callback, the NSTimer tick); the mutex is not
// contended across threads in practice, and the pointer it hands out is
// dereferenced inside the same call.
unsafe impl Send for StoredCallback {}

static CALLBACKS: Lazy<Mutex<HashMap<u64, StoredCallback>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static NEXT_CALLBACK_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Register a Rust closure and return its id. The id is passed to ObjC
/// (as an `NSNumber` target/`tag`, or a raw `NSInteger` in the shim), and
/// the shim calls the host's callback entry point which lands in
/// [`dispatch_callback`].
pub fn register_callback(f: Box<dyn FnMut()>) -> u64 {
    let id = NEXT_CALLBACK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    CALLBACKS.lock().unwrap().insert(id, StoredCallback(f));
    id
}

pub fn unregister_callback(id: u64) {
    CALLBACKS.lock().unwrap().remove(&id);
}

/// Run a registered closure. Resolves the pointer under the lock, drops
/// the lock, then invokes — callbacks re-enter (`queue_redraw` from a
/// draw closure), and holding the lock across the call deadlocks. Same
/// rule as ANDROID_GUIDELINES.md §5.
pub fn dispatch_callback(id: u64) {
    let raw: Option<*mut dyn FnMut()> = {
        let mut map = CALLBACKS.lock().unwrap();
        match map.get_mut(&id) {
            Some(StoredCallback(f)) => Some(&mut **f as *mut dyn FnMut()),
            None => None,
        }
    };
    if let Some(raw) = raw {
        // SAFETY: the registry owns this closure for the process
        // lifetime (entries are never dropped, only replaced), and
        // dispatch is single-threaded on the UI main thread.
        unsafe { (*raw)() };
    }
}

// ------------------------------------------------------------------
// Widget metadata
// ------------------------------------------------------------------

/// What a raw handle is, for the few places the shared code asks a
/// question the ObjC object cannot answer directly (`is_canvas_view`,
/// expand weighting).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Window,
    Box,
    Label,
    Button,
    Entry,
    Canvas,
    Container,
}

/// Per-handle metadata. Android keeps this in the View objects (an
/// `EditText` knows it is expanding via its LayoutParams); here it is
/// beside them, keyed by the handle pointer.
#[derive(Clone)]
pub struct WidgetMeta {
    pub kind: Kind,
    /// Last text set/read. Kept in Rust because reading a label back
    /// through its `text` accessor is cheap but the text-field path needs
    /// the pre-edit value for the text-changed shim's dedup guard anyway.
    pub text: String,
    /// `set_hexpand` / `set_vexpand` request. Neither UIKit nor AppKit has
    /// an expand flag; the box appender turns this into a flexible
    /// constraint or a stack-view distribution, exactly like Android's
    /// LinearLayout weight (ANDROID_GUIDELINES.md §6).
    pub hexpand: bool,
    pub vexpand: bool,
    /// `set_width_chars(n)`: a minimum width in characters, so an empty
    /// text field (which measures ~0) stays clickable/tappable.
    pub min_chars: i32,
    /// Canvas id for `Kind::Canvas` handles (0 otherwise). The draw and
    /// click registries key on this, not on the pointer.
    pub canvas_id: u64,
    /// Size Rust *asked* for via `set_size_request`, in points.
    pub size_request: (i32, i32),
    /// The laid-out size in points, learned from the host view's draw
    /// callback. `(0, 0)` until the first draw.
    pub laid_out: (i32, i32),
    /// `Label::set_fixed_width`: a pinned width in points, so a label whose
    /// *text* changes (the formula bar's address slot, `A1` -> `AAA~1`) keeps
    /// its slot instead of resizing and shoving every sibling packed after it
    /// sideways. `None` releases the pin.
    ///
    /// Recorded on both Apple backends, and for the same reason GTK records
    /// it as a real width request: a stack view sizes a label from its
    /// intrinsic content width, so *only* an explicit width stops the
    /// reflow. `NSStackView` has no "fixed width" of its own — see
    /// `Label::set_fixed_width` in the adapters for how the pin becomes an
    /// `NSLayoutConstraint` plus a high hugging priority.
    pub fixed_width: Option<i32>,
    /// `Label::set_margin_start`: left inset of the label's contents, in
    /// points. Pairs with [`Self::fixed_width`] — a pinned, left-aligned
    /// label sits flush against its slot's edge, and this restores the inset
    /// the shrink-to-fit label used to have.
    pub margin_start: i32,
    /// `Label::set_margin_top`: the vertical counterpart of
    /// [`Self::margin_start`]. Recorded on both Apple backends; the iOS
    /// `UILabel` has no inset selector at all, so it is recorded and not sent.
    pub margin_top: i32,
    /// `Label::set_xalign`: horizontal text alignment, `0.0` left ..
    /// `1.0` right. `NSTextField`/`UILabel` centre their text by default, so
    /// a pinned slot would otherwise make the text *float* mid-slot and
    /// slide within it as the text changes width — the same reflow the pin
    /// exists to stop, relocated from the slot's edge to the glyphs.
    pub xalign: f32,
}

impl Default for WidgetMeta {
    fn default() -> Self {
        WidgetMeta {
            kind: Kind::Container,
            text: String::new(),
            hexpand: false,
            vexpand: false,
            min_chars: 0,
            canvas_id: 0,
            size_request: (0, 0),
            laid_out: (0, 0),
            fixed_width: None,
            margin_start: 0,
            margin_top: 0,
            xalign: 0.5,
        }
    }
}

/// Handle -> metadata. `usize` keys so the map is `Send` (raw pointers
/// are not); the pointer is only ever dereferenced on the UI thread.
static WIDGETS: Lazy<Mutex<HashMap<usize, WidgetMeta>>> = Lazy::new(|| Mutex::new(HashMap::new()));

pub fn register_widget(handle: *mut std::os::raw::c_void, meta: WidgetMeta) {
    if handle.is_null() {
        return;
    }
    WIDGETS.lock().unwrap().insert(handle as usize, meta);
}

pub fn with_meta<R>(handle: *mut std::os::raw::c_void, f: impl FnOnce(&WidgetMeta) -> R) -> Option<R> {
    if handle.is_null() {
        return None;
    }
    WIDGETS.lock().unwrap().get(&(handle as usize)).map(f)
}

pub fn with_meta_mut<R>(
    handle: *mut std::os::raw::c_void,
    f: impl FnOnce(&mut WidgetMeta) -> R,
) -> Option<R> {
    if handle.is_null() {
        return None;
    }
    WIDGETS.lock().unwrap().get_mut(&(handle as usize)).map(f)
}

pub fn kind_of(handle: *mut std::os::raw::c_void) -> Option<Kind> {
    with_meta(handle, |m| m.kind)
}

/// True when `handle` is a canvas: canvases grow, chrome wraps (the Apple
/// answer to Android's `is_canvas_view`).
pub fn is_canvas_view(handle: *mut std::os::raw::c_void) -> bool {
    matches!(kind_of(handle), Some(Kind::Canvas))
}

pub fn set_view_expanding(handle: *mut std::os::raw::c_void, expand: bool) {
    with_meta_mut(handle, |m| m.hexpand = expand);
}

pub fn is_view_expanding(handle: *mut std::os::raw::c_void) -> bool {
    with_meta(handle, |m| m.hexpand).unwrap_or(false)
}

pub fn set_view_min_chars(handle: *mut std::os::raw::c_void, n: i32) {
    with_meta_mut(handle, |m| m.min_chars = n);
}

pub fn view_min_chars(handle: *mut std::os::raw::c_void) -> i32 {
    with_meta(handle, |m| m.min_chars).unwrap_or(0)
}

/// Canvas id for a handle (0 = not a canvas / unknown).
pub fn canvas_id_for_view(handle: *mut std::os::raw::c_void) -> u64 {
    with_meta(handle, |m| m.canvas_id).unwrap_or(0)
}

/// Next canvas id. Ids start at 1 so 0 can mean "unknown".
static NEXT_CANVAS_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

pub fn next_canvas_id() -> u64 {
    NEXT_CANVAS_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Class name the host must register for canvases (`SheetView` on iOS, a
/// custom `NSView` subclass on macOS), set by the host before
/// `init_with_root`. Mirrors Android's `set_sheet_view_class`; unset falls
/// back to a plain `UIView`/`NSView`, which builds and lays out but never
/// draws (the tree stays valid).
static SHEET_VIEW_CLASS: Mutex<Option<String>> = Mutex::new(None);

pub fn set_sheet_view_class(name: &str) {
    *SHEET_VIEW_CLASS.lock().unwrap() = Some(name.to_owned());
}

pub fn sheet_view_class() -> Option<String> {
    SHEET_VIEW_CLASS.lock().unwrap().clone()
}

// ------------------------------------------------------------------
// Tests (host-independent parts)
// ------------------------------------------------------------------
//
// These exercise the runtime-free halves of this module (callbacks, widget
// metadata, canvas ids). They run on any Apple target but do not need the
// framework to be up; kept in the shared module so both the iOS and the
// AppKit layers get them.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_dispatch_callback() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let id = register_callback(Box::new(|| {
            COUNT.fetch_add(1, Ordering::SeqCst);
        }));
        dispatch_callback(id);
        assert_eq!(COUNT.load(Ordering::SeqCst), 1);
        unregister_callback(id);
        // Dispatch after unregister must be a no-op, not a panic.
        dispatch_callback(id);
        assert_eq!(COUNT.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_widget_meta_roundtrip() {
        let handle = 0x1000 as *mut std::os::raw::c_void;
        register_widget(
            handle,
            WidgetMeta {
                kind: Kind::Canvas,
                canvas_id: 7,
                ..Default::default()
            },
        );
        assert_eq!(kind_of(handle), Some(Kind::Canvas));
        assert!(is_canvas_view(handle));
        assert_eq!(canvas_id_for_view(handle), 7);
        set_view_expanding(handle, true);
        assert!(is_view_expanding(handle));
        set_view_min_chars(handle, 12);
        assert_eq!(view_min_chars(handle), 12);
        assert_eq!(kind_of(std::ptr::null_mut()), None);
        assert!(!is_canvas_view(0xdead as *mut std::os::raw::c_void));
    }

    #[test]
    fn test_canvas_ids_are_unique_and_nonzero() {
        let a = next_canvas_id();
        let b = next_canvas_id();
        assert_ne!(a, 0);
        assert_ne!(a, b);
    }
}
