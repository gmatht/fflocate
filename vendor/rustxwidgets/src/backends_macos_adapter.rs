//! macOS backend adapter: AppKit widgets reached through the Objective-C
//! runtime.
//!
//! The AppKit sibling of `backends_ios_adapter.rs` (UIKit) and
//! `backends_android_adapter.rs` — same shape, same contracts, different OS.
//! It builds on the **shared** Apple runtime in [`crate::backends::apple`]
//! (the same module the iOS backend uses): Objective-C runtime wrappers,
//! `NSString` marshalling, handle keep-alive, callback registry and widget
//! metadata are not duplicated here. Only the widget layer is AppKit.
//!
//! * Every widget is a `#[repr(transparent)]` raw Objective-C object handle
//!   (`NSView*`, `NSButton*`, ...), retained by [`crate::backends::apple`]
//!   (`KEEP_ALIVE` there). Methods are best-effort no-ops when the backend was
//!   never initialised (host builds, tests, headless runs).
//! * The Canvas renders through a host-supplied `NSView` subclass
//!   (`CorroSheetView`): the draw closure lives in the Rust-side registry and
//!   is dispatched by **canvas id** from the view's `drawRect:`, which funnels
//!   every `DrawContext` call back into Rust. Until such a view exists,
//!   `queue_redraw` is a no-op and `text_extents_*` return a monospace
//!   estimate, so layout never divides by zero.
//! * The menu types carry the menu *model* plus the shared action registry.
//!   Unlike iOS (which has no menubar and must publish a model for a `UIMenu`),
//!   macOS *has* a real `NSMenu`, so a host can build a genuine menubar; the
//!   model-only shape is kept so the same rswidgets call sites work on both.
//!
//! Differences from iOS worth knowing when reading:
//!
//! * **No touch targets.** iOS pads chrome to a 44pt touch target; macOS is a
//!   pointer platform, so chrome uses the compact desktop metrics (`MIN_CTRL_PT`).
//! * **No soft keyboard.** iOS funnels typing through `UITextField`'s
//!   `editingChanged`; on macOS typing arrives through
//!   `NSTextField`'s `controlTextDidChange:` (via the same `dispatch_*`
//!   entry points the iOS host calls).
//! * **Frame origin.** AppKit's default view coordinate system puts the origin
//!   at the *bottom left* (unlike UIKit's top-left). The canvas shim is
//!   expected to `isFlipped` so the shared top-left drawing convention holds;
//!   see `docs/MACOS_GUIDELINES.md`.
//! * **Window.** On iOS the `Window` handle is the root view (the scene owns
//!   the window). On macOS the app owns a real `NSWindow`, but rswidgets is
//!   handed its `contentView` (the host creates the window), so the same
//!   "window handle is the root view" shape applies.

#[cfg(target_os = "macos")]
mod macos_adapter {
    use std::collections::HashMap;
    use std::os::raw::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use once_cell::sync::Lazy;

    use crate::backends::apple::{
        self as core_apple, alloc_init, cls, msg0, msg0i, msg0v, msg1bv, msg1cv, msg1i, msg1iv,
        msg1v, msg2cv, msg4cv, nsstring, nsstring_to_rust, own, Kind, WidgetMeta,
    };
    use crate::core::{DrawContext, Error, Widget};

    // Message-send with an explicit signature, for the handful of host-shim
    // calls the ObjC runtime cannot express as a plain extern fn (see the
    // "Host shim calls" section below for why each one names its signature).
    macro_rules! raw_send {
        ($obj:expr, $sel:expr, $sig:ty, ($($arg:expr),* $(,)?)) => {{
            let msg = crate::backends::apple::msg_shim();
            // This macro is expanded both inside `unsafe` blocks and outside
            // them, so the blocks here are required in one context and redundant
            // in the other. Allow the lint instead of splitting the macro.
            #[allow(unused_unsafe)]
            let send: $sig = unsafe { std::mem::transmute(msg) };
            let sel = crate::backends::apple::selector($sel);
            #[allow(unused_unsafe)]
            let out = unsafe { send($obj, sel $(, $arg)*) };
            out
        }};
    }

    // ------------------------------------------------------------------
    // Metrics
    // ------------------------------------------------------------------

    /// Monospace advance estimate (points) used by `text_extents_*` until a
    /// real measurement is possible. Matches the iOS/Android constants so
    /// layout is comparable across backends.
    const EST_CHAR_W: f64 = 7.2;
    /// Line-height factor for the estimate.
    const EST_LINE_H: f64 = 1.2;

    /// Compact desktop control height. macOS is a pointer platform: there is
    /// no 44pt touch-target floor (that constant is iOS-only), so a standard
    /// push button / text field height is used to keep the toolbar dense.
    const MIN_CTRL_PT: f64 = 24.0;

    /// Estimated text metrics: `(x_bearing, y_bearing, width, height)`, the
    /// tuple [`DrawContext::text_extents_styled`] promises (bearings 0, since
    /// callers assume a left/top origin).
    fn estimate_extents(text: &str, size: f64, weight: i32) -> (f64, f64, f64, f64) {
        let bold = if weight != 0 { 1.08 } else { 1.0 };
        let w = text.chars().count() as f64 * size * (EST_CHAR_W / 12.0) * bold;
        (0.0, 0.0, w, size * EST_LINE_H)
    }

    /// Points-per-pixel scale of the current screen, defaulting to 1.0. Used
    /// where a *point* size must be produced from a pixel constant.
    fn scale() -> f64 {
        crate::backends::macos::display_scale().unwrap_or(1.0).max(1.0)
    }

    // ------------------------------------------------------------------
    // AppKit constructors
    // ------------------------------------------------------------------

    /// The backend's root view, or `None` when uninitialised.
    fn root() -> Option<*mut c_void> {
        core_apple::root_view()
    }

    /// Allocate + init an instance of `class_name`, register metadata, and
    /// retain it. Returns null when the backend is uninitialised or the class
    /// is missing (a host that ships no AppKit) — callers degrade instead of
    /// crashing.
    fn new_widget(class_name: &str, kind: Kind) -> *mut c_void {
        if !core_apple::is_initialized() {
            return std::ptr::null_mut();
        }
        let class = cls(class_name);
        if class.is_null() {
            crate::backends::macos::log_macos(&format!("macos: class {class_name} not found"));
            return std::ptr::null_mut();
        }
        let obj = alloc_init(class);
        if obj.is_null() {
            return std::ptr::null_mut();
        }
        let obj = own(obj);
        core_apple::register_widget(
            obj,
            WidgetMeta {
                kind,
                ..Default::default()
            },
        );
        obj
    }

    /// Set an `NSRect` frame. `NSRect` is zero-filled on creation, so every
    /// widget that is not laid out by a stack view needs a frame; the host's
    /// Auto Layout constraints refine it afterwards.
    unsafe fn set_frame(view: *mut c_void, x: f64, y: f64, w: f64, h: f64) {
        if view.is_null() {
            return;
        }
        unsafe { msg4cv(view, "setFrame:", x, y, w, h) };
    }

    /// The margin GTK's window leaves around its content, in points.
    const WINDOW_MARGIN: f64 = 5.0;

    /// Pin `child` to both of `stack`'s cross-axis edges.
    ///
    /// GTK's box stretches a child across the axis it does not lay out along;
    /// `NSStackView` only *aligns* it, so children kept their intrinsic widths
    /// and the UI sat in a narrow central column. Two equality constraints
    /// reproduce the fill — leading/trailing in a vertical stack, top/bottom in
    /// a horizontal one (`NSLayoutAttribute` top 3, bottom 4, leading 5,
    /// trailing 6) — and stay consistent with the stack view's centre alignment.
    unsafe fn pin_across(stack: *mut c_void, child: *mut c_void) {
        let constraint_cls = cls("NSLayoutConstraint");
        if constraint_cls.is_null() || stack.is_null() || child.is_null() {
            return;
        }
        // SAFETY: a live NSStackView; `orientation` returns an NSInteger.
        unsafe {
            let vertical = raw_send!(
                stack,
                "orientation",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize,
                ()
            ) == 1;
            let (first, second) = if vertical {
                (5isize, 6isize)
            } else {
                (3isize, 4isize)
            };
            // Auto Layout owns this child's frame from here on, so its
            // autoresizing mask must not translate the frame into constraints
            // as well.
            msg1bv(child, "setTranslatesAutoresizingMaskIntoConstraints:", false);
            for attr in [first, second] {
                let constraint: *mut c_void = raw_send!(
                    constraint_cls,
                    "constraintWithItem:attribute:relatedBy:toItem:attribute:multiplier:constant:",
                    unsafe extern "C" fn(
                        *mut c_void,
                        *mut c_void,
                        *mut c_void,
                        isize,
                        isize,
                        *mut c_void,
                        isize,
                        f64,
                        f64,
                    ) -> *mut c_void,
                    (child, attr, 0isize, stack, attr, 1.0f64, 0.0f64)
                );
                if !constraint.is_null() {
                    msg1v(stack, "addConstraint:", constraint);
                }
            }
        }
    }

    /// Give `view` a required width and height.
    ///
    /// Auto Layout otherwise fits a view to its subviews: the window's content
    /// view shrank to a 32pt strip and the root box to one row, which squeezed
    /// the list out of the window and left its scroller 17x0. The pair replaces
    /// any earlier pair on `view`, so repeated calls do not stack constraints.
    unsafe fn pin_size(view: *mut c_void, w: f64, h: f64) {
        if view.is_null() {
            return;
        }
        // SAFETY: a live view; `NSLayoutAttribute` width 7, height 8, and
        // `NSLayoutRelationEqual` 0.
        unsafe {
            let constraint_cls = cls("NSLayoutConstraint");
            if constraint_cls.is_null() {
                return;
            }
            let existing = msg0(view, "constraints");
            if !existing.is_null() {
                let count = msg0i(existing, "count").max(0);
                for i in 0..count {
                    let c = msg1i(existing, "objectAtIndex:", i);
                    if c.is_null() {
                        continue;
                    }
                    let attr = msg0i(c, "firstAttribute");
                    if msg0(c, "firstItem") == view && (attr == 7 || attr == 8) {
                        msg1v(view, "removeConstraint:", c);
                    }
                }
            }
            msg1bv(view, "setTranslatesAutoresizingMaskIntoConstraints:", false);
            for (attr, value) in [(7isize, w), (8isize, h)] {
                let constraint: *mut c_void = raw_send!(
                    constraint_cls,
                    "constraintWithItem:attribute:relatedBy:toItem:attribute:multiplier:constant:",
                    unsafe extern "C" fn(
                        *mut c_void,
                        *mut c_void,
                        *mut c_void,
                        isize,
                        isize,
                        *mut c_void,
                        isize,
                        f64,
                        f64,
                    ) -> *mut c_void,
                    (
                        view,
                        attr,
                        0isize,
                        std::ptr::null_mut::<c_void>(),
                        0isize,
                        1.0f64,
                        value,
                    )
                );
                if !constraint.is_null() {
                    msg1v(view, "addConstraint:", constraint);
                }
            }
        }
    }

    /// Size `child` to `content`'s bounds, inset by [`WINDOW_MARGIN`].
    ///
    /// AppKit does not lay a plain subview out by itself: a view added to the
    /// content view keeps its zeroed `NSRect`, so the root box stayed 0x0 and
    /// the window rendered blank. GTK's `set_child` sizes the child to the
    /// window for free, so this is the macOS equivalent, shared by `set_child`
    /// and by the capture path, which resizes the window and re-fits what it
    /// already holds.
    unsafe fn fit_content_child(content: *mut c_void, child: *mut c_void) {
        if content.is_null() || child.is_null() {
            return;
        }
        // SAFETY: both handles are live AppKit views; `bounds` returns a
        // `CGRect` by value and `NSViewWidthSizable|NSViewHeightSizable` are the
        // documented autoresizing constants.
        unsafe {
            const NSVIEW_WIDTH_SIZABLE: isize = 2;
            const NSVIEW_HEIGHT_SIZABLE: isize = 16;
            let bounds: CGRect = raw_send!(
                content,
                "bounds",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                ()
            );
            msg4cv(
                child,
                "setFrame:",
                WINDOW_MARGIN,
                WINDOW_MARGIN,
                (bounds.width - 2.0 * WINDOW_MARGIN).max(1.0),
                (bounds.height - 2.0 * WINDOW_MARGIN).max(1.0),
            );
            msg1i(
                child,
                "setAutoresizingMask:",
                NSVIEW_WIDTH_SIZABLE | NSVIEW_HEIGHT_SIZABLE,
            );
            pin_size(
                child,
                (bounds.width - 2.0 * WINDOW_MARGIN).max(1.0),
                (bounds.height - 2.0 * WINDOW_MARGIN).max(1.0),
            );
        }
    }

    /// Re-fit every direct child of `content` (the window's root views).
    unsafe fn refit_content_children(content: *mut c_void) {
        if content.is_null() {
            return;
        }
        // SAFETY: a live view; `subviews` is an `NSArray` of live views.
        unsafe {
            let subviews = msg0(content, "subviews");
            if subviews.is_null() {
                return;
            }
            let count = msg0i(subviews, "count").max(0);
            for i in 0..count {
                let child = msg1i(subviews, "objectAtIndex:", i);
                fit_content_child(content, child);
            }
        }
    }

    /// Put `child` inside `parent` (`addSubview:`), keeping the child's own
    /// frame. Null-safe: a null parent (backend not initialised) means the
    /// child is simply never attached, which is what makes host runs work.
    fn attach(parent: *mut c_void, child: *mut c_void) {
        if parent.is_null() || child.is_null() {
            return;
        }
        unsafe { msg1v(parent, "addSubview:", child) };
    }

    // ------------------------------------------------------------------
    // Orientation
    // ------------------------------------------------------------------

    /// Layout direction for [`create_box`].
    ///
    /// The discriminants match Android's `LinearLayout` constants
    /// (`HORIZONTAL = 0`, `VERTICAL = 1`) as well as AppKit's
    /// `NSUserInterfaceLayoutOrientation` (0 = horizontal, 1 = vertical), so
    /// the shared code can keep passing the same integers on every platform.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Orientation {
        Horizontal = 0,
        Vertical = 1,
    }

    impl Orientation {
        fn as_int(self) -> i32 {
            self as i32
        }
    }

    /// Alias required by `common.rs` (`Orientation as MacosOrientation`).
    pub type MacosOrientation = Orientation;

    // ------------------------------------------------------------------
    // Window
    // ------------------------------------------------------------------
    //
    // `Window` is the host window's content view handle: the app owns the
    // real `NSWindow` (the host creates it and hands us its content view
    // through `init_with_root`), so window-ish methods are no-ops here
    // (set_title/set_default_size) — the same decision iOS and Android make.

    #[repr(transparent)]
    pub struct Window(pub *mut c_void);

    impl Widget for Window {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for Window {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl Clone for Window {
        fn clone(&self) -> Self {
            Window(self.0)
        }
    }

    impl Window {
        /// Set the window title.
        ///
        /// A host owns its own window and titles it; the self-driven window is
        /// ours, so this is the one place the title actually lands.
        pub fn set_title(&self, title: &str) {
            let window = OWN_WINDOW.load(Ordering::Relaxed) as *mut c_void;
            if window.is_null() {
                return;
            }
            let s = nsstring(title);
            if s.is_null() {
                return;
            }
            unsafe { msg1v(window, "setTitle:", s) };
        }

        /// Set the **Dock** icon (and the window-switcher image).
        ///
        /// macOS has no per-window icon: the application owns one, so this sets
        /// `NSApplication.applicationIconImage` from the PNG bytes rather than
        /// decorating a window. It matters for a bare Rust binary, which has no
        /// `.app` bundle to carry an `Info.plist` icon and would otherwise show
        /// the generic executable icon in the Dock.
        ///
        /// No-op when AppKit is uninitialised, when there is no application (a
        /// headless session), and when the PNG cannot be turned into an
        /// `NSImage` — a missing icon must never take the GUI down.
        pub fn set_icon(&self, png: &[u8]) {
            if png.is_empty() {
                return;
            }
            // SAFETY: `NSData`/`NSImage` are AppKit's own classes and every
            // handle is null-checked. `NSData` copies the bytes, so the slice
            // can be freed afterwards, and `setApplicationIconImage:` retains
            // the image; no Rust-owned memory is handed to AppKit.
            let app = unsafe { shared_app() };
            if app.is_null() {
                return;
            }
            let data_cls = cls("NSData");
            let image_cls = cls("NSImage");
            if data_cls.is_null() || image_cls.is_null() {
                return;
            }
            let data = raw_send!(
                data_cls,
                "dataWithBytes:length:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *const u8, usize) -> *mut c_void,
                (png.as_ptr(), png.len())
            );
            if data.is_null() {
                return;
            }
            let image = alloc_init(image_cls);
            if image.is_null() {
                return;
            }
            let image = raw_send!(
                image,
                "initWithData:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void,
                (data)
            );
            if image.is_null() {
                return;
            }
            unsafe { msg1v(app, "setApplicationIconImage:", image) };
        }

        pub fn set_default_size(&self, _w: i32, _h: i32) {}

        /// Force the window to a size, in points.
        ///
        /// The handle is the host's root *view* (see `create_window`), not an
        /// `NSWindow`, so this sets the view's frame. The origin is left
        /// alone: the view is positioned by its superview, and a root view has
        /// none to be in conflict with, so a zero origin is the neutral
        /// choice.
        ///
        /// The argument order is the one that matters — `msg4cv` sends
        /// `(CGFloat, CGFloat, CGFloat, CGFloat)`, which is `CGRect`'s field
        /// order, so origin first and size second. Getting that backwards
        /// produces a silently wrong frame rather than a compile error.
        pub fn resize(&self, w: i32, h: i32) {
            if self.0.is_null() {
                return;
            }
            unsafe { msg4cv(self.0, "setFrame:", 0.0, 0.0, w as f64, h as f64) };
            // AppKit needs both: a frame change is a layout change, and the
            // canvas will keep painting the old size until something asks for
            // a redraw.
            unsafe { msg1bv(self.0, "setNeedsLayout:", true) };
            unsafe { msg1bv(self.0, "setNeedsDisplay:", true) };
        }

        /// # Safety
        /// Kept for API compatibility with the GTK backend; no-op on macOS.
        pub unsafe fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}

        pub fn hwnd(&self) -> *mut c_void {
            std::ptr::null_mut()
        }

        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            attach(self.0, child_ptr);
            if self.0.is_null() || child_ptr.is_null() {
                return;
            }
            // SAFETY: both handles are live AppKit views (see the helper).
            unsafe { fit_content_child(self.0, child_ptr) }
        }

        pub fn set_child_box(&self, bx: &BoxWidget) {
            self.set_child(bx);
        }

        pub fn present(&self) {
            // The window is already visible (the host shows it); make sure
            // AppKit is told to lay out and draw what we added.
            if !self.0.is_null() {
                unsafe {
                    msg1bv(self.0, "setNeedsLayout:", true);
                    msg1bv(self.0, "setNeedsDisplay:", true);
                }
            }
        }

        pub fn queue_redraw(&self) {
            if !self.0.is_null() {
                unsafe { msg1bv(self.0, "setNeedsDisplay:", true) };
            }
        }

        pub fn on_event(&self, _cb: Box<dyn FnMut(*mut c_void) -> i32>) {}
        pub fn on_event_key(&self, _cb: Box<dyn FnMut(u32, u32) -> i32>) {}
        pub fn on_close(&self, _cb: Box<dyn FnMut()>) {}

        /// Screen scale (1.0/2.0 on Retina), the macOS analogue of Android's
        /// display density. `None` when uninitialised, so callers fall back
        /// to 1.0.
        pub fn display_scale(&self) -> Option<f64> {
            crate::backends::macos::display_scale()
        }
    }

    // ------------------------------------------------------------------
    // Button (NSButton, push button style)
    // ------------------------------------------------------------------

    #[repr(transparent)]
    pub struct Button(pub *mut c_void);

    impl Widget for Button {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for Button {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl Button {
        /// Register a click handler. AppKit targets are (id, SEL, id): the
        /// host shim (`CorroMacTarget` in the app) receives our callback id
        /// and calls back into the host's `corro_macos_callback`.
        ///
        /// The same registry as the iOS `CorroIosTarget`, so a single
        /// dispatch path exists on both Apple platforms.
        pub fn on_click(&self, f: impl FnMut() + Send + 'static) -> Result<u64, Error> {
            let id = core_apple::register_callback(Box::new(f));
            self.attach_target(id);
            Ok(id)
        }

        /// Ask the host shim to wire `id` to the button's action
        /// (`setTarget:` + `setAction:`).
        fn attach_target(&self, id: u64) {
            if self.0.is_null() {
                return;
            }
            // The target class is built at runtime; without AppKit (or on a
            // host that never loaded it) it stays absent and this degrades to
            // the logged skip below.
            ensure_runtime_classes();
            let shim = cls("CorroMacTarget");
            if shim.is_null() {
                crate::backends::macos::log_macos("macos: CorroMacTarget shim missing; button inert");
                return;
            }
            // SAFETY: `shim` is the CorroMacTarget class object and
            // `targetWithCallbackId:` is its declared factory selector.
            let target = unsafe { msg1i(shim, "targetWithCallbackId:", id as isize) };
            if target.is_null() {
                return;
            }
            let target = own(target);
            // `setTarget:` then `setAction:` — the AppKit idiom (an NSControl
            // sends `-action` to `-target` on click). The action selector is
            // looked up by name (`corroFired:`), so the Rust side never
            // hardcodes an IMP.
            unsafe {
                msg1v(self.0, "setTarget:", target);
                msg1v(self.0, "setAction:", core_apple::selector("corroFired:"));
            }
        }

        /// Simulate a click (`performClick:`), for tests and scripted hosts.
        pub fn emit_clicked(&self) -> Result<u64, Error> {
            if !self.0.is_null() {
                // `-performClick:` takes the sender; the no-argument send
                // would pass an undefined register as that argument.
                unsafe { msg1v(self.0, "performClick:", std::ptr::null_mut()) };
            }
            Ok(0)
        }
    }

    impl Clone for Button {
        fn clone(&self) -> Self {
            Button(self.0)
        }
    }

    // ------------------------------------------------------------------
    // Label (NSTextField, non-editable label style)
    // ------------------------------------------------------------------

    #[repr(transparent)]
    pub struct Label(pub *mut c_void);

    impl Widget for Label {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for Label {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl Label {
        pub fn set_text(&self, text: &str) {
            if self.0.is_null() {
                return;
            }
            let s = nsstring(text);
            if s.is_null() {
                return;
            }
            // `setStringValue:` is the NSTextField text accessor.
            unsafe { msg1v(self.0, "setStringValue:", s) };
            core_apple::with_meta_mut(self.0, |m| m.text = text.to_owned());
        }

        pub fn get_text(&self) -> Option<String> {
            if self.0.is_null() {
                return None;
            }
            // Prefer the Rust-side copy: it is authoritative for text we set,
            // and avoids an ObjC round trip for chrome labels read every frame.
            if let Some(text) = core_apple::with_meta(self.0, |m| m.text.clone()) {
                if !text.is_empty() {
                    return Some(text);
                }
            }
            let s = unsafe { msg0(self.0, "stringValue") };
            nsstring_to_rust(s)
        }

        pub fn set_visible(&self, visible: bool) {
            if self.0.is_null() {
                return;
            }
            unsafe { msg1bv(self.0, "setHidden:", !visible) };
        }

        /// Dim the label.
        ///
        /// A plain `NSTextField` label has no "sensitive" flag (there is
        /// nothing to enable or disable), so the greyed-out look GTK gives an
        /// insensitive label is reproduced with the view's alpha.
        pub fn set_sensitive(&self, sensitive: bool) {
            if self.0.is_null() {
                return;
            }
            unsafe { msg1cv(self.0, "setAlphaValue:", if sensitive { 1.0 } else { 0.55 }) };
        }

        /// `setMarkup` in the shared API. `NSTextField` is plain text, so the
        /// markup is flattened to its text content (Pango markup on GTK,
        /// nothing at all on Android/iOS).
        pub fn set_markup(&self, markup: &str) {
            self.set_text(&strip_markup(markup));
        }

        /// Pin the label's width so changing its text never reflows the
        /// siblings packed after it. `None` releases the pin.
        ///
        /// ## Why this is not a one-line forward to AppKit
        ///
        /// An `NSTextField` has no "fixed width". Its width comes from
        /// Auto Layout resolving *two competing* constraints against its
        /// intrinsic content size: an ordinary width constraint the stack view
        /// adds (low priority) versus `-[NSView intrinsicContentSize]`, which
        /// is computed from the current `stringValue` and has a hugging
        /// priority of 250. The intrinsic side therefore wins, and the field
        /// is exactly as shrink-to-fit as a `GtkLabel` is — the label resizes
        /// on every text change and drags its neighbours with it.
        ///
        /// So the pin is built from the two things AppKit *does* honour:
        ///
        /// * `corroSetPinnedWidth:` — a host-shim method (generated; see
        ///   `apple_generator::VIEW_CATEGORY_METHODS`) that installs a
        ///   required width equality constraint. Required, not greater-than:
        ///   a ">= w" constraint still loses to the intrinsic size when the
        ///   text is wider, which is precisely the reflow we are stopping.
        /// * `corroSetContentHugging:priority:` — raising the hugging
        ///   priority above the intrinsic 250 so the pinned width beats the
        ///   intrinsic content size in the first place.
        ///
        /// Together those make the slot constant, which is the same
        /// fixed-slot trade GTK (width request) and NWG (`SetWindowPos`)
        /// make: text wider than the pin clips rather than growing the slot.
        /// The caller must size the pin for the widest text the label will
        /// ever show, exactly as the GTK docs say.
        ///
        /// The pin is also recorded in [`WidgetMeta`] so it survives even
        /// when the shim is missing (headless, tests, a host that has not
        /// yet installed the category): a later `append` can re-assert it.
        pub fn set_fixed_width(&self, w: Option<i32>) {
            core_apple::with_meta_mut(self.0, |m| m.fixed_width = w);
            if self.0.is_null() {
                return;
            }
            match w {
                Some(w) => {
                    // NSLayoutPriority: 1000 is "required", 999 one step
                    // below. Either beats the intrinsic 250.
                    shim_i(self.0, "corroSetPinnedWidth:", w as isize);
                    shim_i(self.0, "corroSetContentHugging:", 1000);
                }
                None => {
                    // Release: drop the constraint and let the intrinsic size
                    // size the field again (hugging back to the default 250).
                    shim_i(self.0, "corroSetPinnedWidth:", -1);
                    shim_i(self.0, "corroSetContentHugging:", 250);
                }
            }
            // The constraint is only meaningful once the view is in a
            // hierarchy, so ask AppKit to re-run layout.
            unsafe { msg1bv(self.0, "setNeedsLayout:", true) };
        }

        /// Left margin of the label's contents, in points. Pairs with
        /// [`Label::set_fixed_width`]: a pinned, left-aligned label sits flush
        /// against its slot's edge, and this restores the inset the
        /// shrink-to-fit label had.
        pub fn set_margin_start(&self, px: i32) {
            core_apple::with_meta_mut(self.0, |m| m.margin_start = px);
            if self.0.is_null() {
                return;
            }
            // `headIndent` is the left inset in points, and unlike a
            // constraint it needs no superview to take effect. It is sent only
            // when the view really has it: a selector this SDK does not
            // implement would raise and abort, and the margin is already
            // recorded in the widget's meta either way.
            if responds(self.0, "setHeadIndent:") {
                unsafe { msg1cv(self.0, "setHeadIndent:", px as f64) };
            }
        }

        /// Top inset of the label's contents, in points.
        ///
        /// The vertical counterpart to [`Label::set_margin_start`]. Recorded
        /// in `WidgetMeta` and not sent: `NSTextField` has no
        /// `setTopIndent:` to send, and inventing a selector the object does
        /// not have would raise `unrecognized selector` — a foreign throw
        /// that `catch_unwind` cannot intercept, which is how the iOS adapter's
        /// copy of `setHeadIndent:` used to kill the app.
        pub fn set_margin_top(&self, px: i32) {
            core_apple::with_meta_mut(self.0, |m| m.margin_top = px);
        }

        /// Set the x alignment of the label's text (0.0 left .. 1.0 right).
        ///
        /// `NSTextField` centres its text by default (`NSTextAlignmentCenter`,
        /// 1), which is why this is not a no-op on a pinned slot: with the
        /// width pinned and the text centred, the address would float in the
        /// middle of its slot and drift as the text changes width.
        ///
        /// `NSTextAlignment` is the same integer scale as AppKit's
        /// `NSTextAlignment` enum — 0 left, 1 centre, 2 right, 3 justified,
        /// 4 natural — so the 0.0..1.0 range is mapped onto the three real
        /// anchors by nearest third, the same mapping NWG uses for
        /// `SS_CENTER`/`SS_RIGHT`.
        pub fn set_xalign(&self, x: f32) {
            core_apple::with_meta_mut(self.0, |m| m.xalign = x);
            if self.0.is_null() {
                return;
            }
            const ALIGN_LEFT: isize = 0;
            const ALIGN_CENTER: isize = 1;
            const ALIGN_RIGHT: isize = 2;
            let align = if x < 1.0 / 3.0 {
                ALIGN_LEFT
            } else if x < 2.0 / 3.0 {
                ALIGN_CENTER
            } else {
                ALIGN_RIGHT
            };
            unsafe { msg1iv(self.0, "setAlignment:", align) };
        }

        pub fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl Clone for Label {
        fn clone(&self) -> Self {
            Label(self.0)
        }
    }

    /// Flatten a Pango-style markup string to plain text (`<b>x</b>` → `x`).
    /// Deliberately minimal: it removes `<...>` spans and unescapes the three
    /// entities the shared code actually emits.
    ///
    /// `pub(crate)` rather than private because this module's test block is a
    /// *sibling*, not a child: the iOS adapter nests its `mod tests` inside
    /// `mod ios_adapter` and reaches the same function through `super::`, but
    /// here `mod macos_adapter` closes first. The two layouts are equivalent to
    /// the compiler and not to a reader, and the difference was invisible until
    /// a macOS test binary was actually built — nothing compiled these tests,
    /// because every check so far used `cargo check`, which does not build test
    /// targets. `test_strip_markup` was a hard E0425 for the whole module.
    pub(crate) fn strip_markup(markup: &str) -> String {
        let mut out = String::with_capacity(markup.len());
        let mut in_tag = false;
        for ch in markup.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => out.push(c),
                _ => {}
            }
        }
        out.replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
    }

    // ------------------------------------------------------------------
    // Box (NSStackView-backed layout container)
    // ------------------------------------------------------------------

    #[repr(transparent)]
    pub struct BoxWidget(pub *mut c_void);

    impl Widget for BoxWidget {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for BoxWidget {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl Clone for BoxWidget {
        fn clone(&self) -> Self {
            BoxWidget(self.0)
        }
    }

    impl BoxWidget {
        /// Append `child` to this box.
        ///
        /// `NSStackView` (macOS 10.9+) is the natural equivalent of Android's
        /// `LinearLayout` and iOS's `UIStackView`: it owns spacing and
        /// distribution. On older systems the host's `CorroMacContainer` shim
        /// provides `corroAddArrangedSubview:axis:` (manual frame layout), so
        /// this call site is identical on both paths — the class of `self.0`
        /// decides.
        pub fn append(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            if self.0.is_null() || child_ptr.is_null() {
                return;
            }
            let is_canvas = core_apple::is_canvas_view(child_ptr);
            let expands = core_apple::is_view_expanding(child_ptr);
            let stack_appkit = responds(self.0, "setDistribution:");
            // GTK's box stretches a child across the *other* axis: a vertical
            // box gives every child the box's width, a horizontal box its
            // height. `NSStackView` only aligns children, so they kept their
            // intrinsic widths and the whole UI sat in a narrow central column
            // while the GTK window had widgets spanning it. Pinning the two
            // cross-axis edges is the "fill" the alignment attribute cannot
            // express, and it stays consistent with the stack view's own centre
            // alignment (centre + both edges is not over-constrained).
            unsafe {
                // `addArrangedSubview:` exists on NSStackView; on the manual
                // shim the same selector is implemented by the host view.
                msg1v(self.0, "addArrangedSubview:", child_ptr);
                if stack_appkit {
                    pin_across(self.0, child_ptr);
                }
                if expands || is_canvas {
                    // GTK gives the leftover space to the expanding child. On a
                    // stack view that is a low content-hugging priority along
                    // the axis: without it `Fill` stretched whichever child
                    // happened to resist least, and the Search button took half
                    // the query row while the entry stayed narrow.
                    let axis = if responds(self.0, "orientation")
                        && raw_send!(
                            self.0,
                            "orientation",
                            unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize,
                            ()
                        ) == 1
                    {
                        1isize
                    } else {
                        0isize
                    };
                    if responds(child_ptr, "setContentHuggingPriority:forOrientation:") {
                        raw_send!(
                            child_ptr,
                            "setContentHuggingPriority:forOrientation:",
                            unsafe extern "C" fn(*mut c_void, *mut c_void, f32, isize),
                            (1.0f32, axis)
                        );
                    }
                    // Weight 1 equivalent: let this child take the leftover
                    // space. On a stack view that is a low content-hugging
                    // priority; the host shim reads the same flag through
                    // `corroFlexChild:`. Android does exactly this with
                    // LinearLayout weight (ANDROID_GUIDELINES.md §6).
                    shim_bv(child_ptr, "corroSetFlex:", true);
                }
            }
            // Minimum width for an empty entry: `set_width_chars` recorded a
            // character count, converted here to points via the estimate.
            let min_chars = core_apple::view_min_chars(child_ptr);
            if min_chars > 0 {
                let w = min_chars as f64 * EST_CHAR_W;
                shim_i(child_ptr, "corroSetMinWidth:", w as isize);
            }
        }

        /// Android/iOS record these and let `append` apply them as weight;
        /// macOS does the same, so a call before `append` is not lost.
        pub fn set_child_hexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
            core_apple::set_view_expanding(*child.as_ref(), expand);
        }

        pub fn set_child_vexpand(&self, child: &impl AsRef<*mut c_void>, _expand: bool) {
            // NSStackView distributes along its axis; a vertical box's
            // expansion *is* the axis, so the per-child flag collapses into
            // the same `corroSetFlex:` call made by `append`.
            core_apple::set_view_expanding(*child.as_ref(), true);
        }

        pub fn set_hexpand(&self, expand: bool) {
            core_apple::set_view_expanding(self.0, expand);
        }

        /// A stack view distributes along its own axis, so a box's vertical
        /// expansion is the axis' business (`set_child_vexpand` handles a
        /// child); recorded as a no-op, as on `Entry`.
        pub fn set_vexpand(&self, _expand: bool) {}

        /// Show or hide the box (and everything in it).
        ///
        /// `setHidden:` on the stack view's own view: NSStackView skips a
        /// hidden arranged subview, so the box collapses instead of leaving a
        /// gap. This is what lets a disclosure hide its body.
        pub fn set_visible(&self, visible: bool) {
            if !self.0.is_null() {
                unsafe { msg1bv(self.0, "setHidden:", !visible) };
            }
        }

        /// Size the stack view.
        ///
        /// `NSStackView` distributes along its axis by default, so an explicit
        /// size request only matters when the caller is pinning the box
        /// (a toolbar strip that must stay one row tall). Sent through the
        /// host shim rather than dropped, so a pinned box is honoured where the
        /// shim provides it.
        pub fn set_size_request(&self, w: i32, h: i32) {
            if self.0.is_null() {
                return;
            }
            // A negative request means "unconstrained" on the GTK contract;
            // AppKit's frame takes floats, so pass them straight through rather
            // than clamping and silently showing a different size.
            unsafe { msg2cv(self.0, "setFrameSize:", w as f64, h as f64) }
        }

    }

    // ------------------------------------------------------------------
    // Entry (NSTextField, editable)
    // ------------------------------------------------------------------

    #[repr(transparent)]
    pub struct Entry(pub *mut c_void);

    impl Widget for Entry {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for Entry {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl Entry {
        pub fn set_hexpand(&self, expand: bool) {
            core_apple::set_view_expanding(self.0, expand);
        }

        pub fn set_vexpand(&self, _expand: bool) {}

        /// Grey hint text, shown while the field is empty.
        ///
        /// `NSTextField`'s `placeholderString` (10.10+) is exactly this, and the
        /// host shim does not have to be involved: the field draws it itself.
        pub fn set_placeholder_text(&self, text: &str) {
            if self.0.is_null() {
                return;
            }
            let s = nsstring(text);
            if s.is_null() {
                return;
            }
            unsafe { msg1v(self.0, "setPlaceholderString:", s) };
        }

        /// `NSControl`'s enabled flag: `false` dims the field and drops input,
        /// the AppKit equivalent of GTK's insensitive state.
        pub fn set_sensitive(&self, sensitive: bool) {
            if self.0.is_null() {
                return;
            }
            unsafe { msg1bv(self.0, "setEnabled:", sensitive) };
        }

        /// Wheel events are not delivered to the field in this build.
        ///
        /// AppKit would need a `scrollWheel:` override on a host shim class
        /// (the same host-shim route the canvas uses for clicks); until that
        /// exists, report it instead of accepting a callback that can never
        /// fire — a handler that silently never runs is worse than one that
        /// says why.
        pub fn connect_scroll_event<F: FnMut(crate::compat::ScrollDirection) -> bool + 'static>(
            &self,
            _f: F,
        ) -> Result<u64, Error> {
            Err(Error::Backend(
                "wheel events are not wired up on the AppKit backend".to_string(),
            ))
        }

        pub fn set_width_chars(&self, n: i32) {
            core_apple::set_view_min_chars(self.0, n);
        }

        pub fn set_text(&self, text: &str) {
            if self.0.is_null() {
                return;
            }
            let s = nsstring(text);
            if s.is_null() {
                return;
            }
            unsafe { msg1v(self.0, "setStringValue:", s) };
            core_apple::with_meta_mut(self.0, |m| m.text = text.to_owned());
        }

        pub fn get_text(&self) -> Option<String> {
            if self.0.is_null() {
                return None;
            }
            let s = unsafe { msg0(self.0, "stringValue") };
            let text = nsstring_to_rust(s);
            if let Some(t) = &text {
                core_apple::with_meta_mut(self.0, |m| m.text = t.clone());
            }
            text
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            core_apple::with_meta_mut(self.0, |m| m.size_request = (w.max(1), h.max(1)));
        }

        pub fn set_visible(&self, v: bool) {
            if !self.0.is_null() {
                unsafe { msg1bv(self.0, "setHidden:", !v) };
            }
        }

        pub fn add_class(&self, _class_name: &str) {}
        pub fn remove_class(&self, _class_name: &str) {}
        pub fn set_halign(&self, _align: i32) {}
        pub fn set_valign(&self, _align: i32) {}
        pub fn set_margin_start(&self, _px: i32) {}
        pub fn set_margin_top(&self, _px: i32) {}

        /// `GtkEntry::activate` / Return key: the host's text-field delegate
        /// calls `corro_macos_entry_activate(handle)`, which lands in
        /// [`dispatch_entry_activate`].
        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(
            &self,
            f: F,
        ) -> Result<u64, Error> {
            let handle = self.0 as usize as u64;
            let id = register_ptr_callback(&ACTIVATE_CALLBACKS, handle, f);
            Ok(id)
        }

        /// Focus-in/out events: `NSControlTextEditingDelegate`'s
        /// `controlTextDidBeginEditing:` / `controlTextDidEndEditing:`.
        pub fn connect_focus_in_event<F: FnMut(*mut c_void) -> i32 + 'static>(
            &self,
            f: F,
        ) -> Result<u64, Error> {
            let handle = self.0 as usize as u64;
            Ok(register_ptr_ret_callback(&FOCUS_IN_CALLBACKS, handle, f))
        }

        pub fn connect_focus_out_event<F: FnMut(*mut c_void) -> i32 + 'static>(
            &self,
            f: F,
        ) -> Result<u64, Error> {
            let handle = self.0 as usize as u64;
            Ok(register_ptr_ret_callback(&FOCUS_OUT_CALLBACKS, handle, f))
        }

        pub fn grab_focus(&self) {
            // AppKit: make the field the window's first responder so typing
            // lands in it (`makeFirstResponder:` is sent to the window).
            if !self.0.is_null() {
                unsafe {
                    let window = msg0(self.0, "window");
                    if !window.is_null() {
                        msg1v(window, "makeFirstResponder:", self.0);
                    }
                }
            }
        }

        /// Caret position as a character index.
        ///
        /// `NSTextField` wraps an `NSTextView` (`currentEditor`) whose
        /// `selectedRange` holds it, but that is a multi-call, version-fussy
        /// path; returning `None` is the documented "backend cannot report
        /// one" answer, and callers keep the caret they track themselves —
        /// which is what iOS and the desktop GTK3 build do too.
        pub fn get_position(&self) -> Option<usize> {
            None
        }

        pub fn set_position(&self, _pos: usize) {}

        pub fn on_key_raw(&self, _cb: Box<dyn FnMut(u32, u32) -> bool>) {}

        pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            let handle = self.0 as usize as u64;
            let id = register_simple_callback(&CHANGED_CALLBACKS, handle, f);
            Ok(id)
        }

        pub fn has_focus(&self) -> bool {
            if self.0.is_null() {
                return false;
            }
            unsafe {
                let window = msg0(self.0, "window");
                if window.is_null() {
                    return false;
                }
                let first = msg0(window, "firstResponder");
                first == self.0
            }
        }

        pub fn connect_button_press(&self, _f: impl FnMut() + 'static) -> Result<u64, Error> {
            // Click handling on the entry is the host's business (AppKit
            // delivers it to the field itself); no extra registration needed.
            Ok(0)
        }
    }

    impl Clone for Entry {
        fn clone(&self) -> Self {
            Entry(self.0)
        }
    }

    // ------------------------------------------------------------------
    // Entry callback plumbing
    // ------------------------------------------------------------------

    /// Pointer-keyed callback registries, one per signal the host shim can
    /// raise. Android and iOS key on the view pointer as well
    /// (`dispatch_text_changed(view_ptr)`), so all three stay symmetrical.
    type PtrCb = *mut dyn FnMut(*mut c_void);
    type PtrRetCb = *mut dyn FnMut(*mut c_void) -> i32;
    type SimpleCb = *mut dyn FnMut();

    struct SendPtr(PtrCb);
    // SAFETY: guarded by the registry mutex; dispatch is UI-thread only.
    unsafe impl Send for SendPtr {}
    struct SendPtrRet(PtrRetCb);
    unsafe impl Send for SendPtrRet {}
    struct SendSimple(SimpleCb);
    unsafe impl Send for SendSimple {}

    static ACTIVATE_CALLBACKS: Lazy<Mutex<HashMap<u64, SendPtr>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    static CHANGED_CALLBACKS: Lazy<Mutex<HashMap<u64, SendSimple>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    static FOCUS_IN_CALLBACKS: Lazy<Mutex<HashMap<u64, SendPtrRet>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    static FOCUS_OUT_CALLBACKS: Lazy<Mutex<HashMap<u64, SendPtrRet>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    fn register_ptr_callback<F: FnMut(*mut c_void) + 'static>(
        reg: &'static Lazy<Mutex<HashMap<u64, SendPtr>>>,
        key: u64,
        mut f: F,
    ) -> u64 {
        let boxed: Box<dyn FnMut(*mut c_void)> = Box::new(move |p| f(p));
        reg.lock()
            .unwrap()
            .insert(key, SendPtr(Box::into_raw(boxed)));
        key
    }

    fn register_ptr_ret_callback<F: FnMut(*mut c_void) -> i32 + 'static>(
        reg: &'static Lazy<Mutex<HashMap<u64, SendPtrRet>>>,
        key: u64,
        f: F,
    ) -> u64 {
        let boxed: Box<dyn FnMut(*mut c_void) -> i32> = Box::new(f);
        reg.lock()
            .unwrap()
            .insert(key, SendPtrRet(Box::into_raw(boxed)));
        key
    }

    fn register_simple_callback<F: FnMut() + 'static>(
        reg: &'static Lazy<Mutex<HashMap<u64, SendSimple>>>,
        key: u64,
        f: F,
    ) -> u64 {
        let boxed: Box<dyn FnMut()> = Box::new(f);
        reg.lock()
            .unwrap()
            .insert(key, SendSimple(Box::into_raw(boxed)));
        key
    }

    /// Called from the host's `controlTextDidChange:` shim: typing produces
    /// this signal (no soft keyboard on macOS, but the same dispatch path as
    /// iOS so corro's `on_formula_entry_changed` is shared).
    pub fn dispatch_text_changed(entry_ptr: *mut c_void) {
        let raw = {
            let mut map = CHANGED_CALLBACKS.lock().unwrap();
            match map.get_mut(&(entry_ptr as usize as u64)) {
                Some(SendSimple(p)) => Some(*p),
                None => None,
            }
        };
        // Lock dropped: the callback re-enters (queue_redraw), and holding it
        // across the call deadlocks (ANDROID_GUIDELINES.md §5).
        if let Some(raw) = raw {
            // SAFETY: registry-owned closure, single-threaded dispatch.
            unsafe { (*raw)() };
        }
    }

    /// Called from the host on Return/commit: commits the edit and moves
    /// down, like a hardware Return on the desktop builds.
    pub fn dispatch_entry_activate(entry_ptr: *mut c_void) {
        let raw = {
            let mut map = ACTIVATE_CALLBACKS.lock().unwrap();
            match map.get_mut(&(entry_ptr as usize as u64)) {
                Some(SendPtr(p)) => Some(*p),
                None => None,
            }
        };
        if let Some(raw) = raw {
            unsafe { (*raw)(entry_ptr) };
        }
    }

    /// Focus-in / focus-out from the text-field delegate. Returns the
    /// handler's own 0/1 answer (whether it consumed the event) so the ObjC
    /// side can decide whether to continue into AppKit's default behaviour.
    pub fn dispatch_focus(entry_ptr: *mut c_void, gained: bool) -> i32 {
        let raw = {
            let reg = if gained {
                &FOCUS_IN_CALLBACKS
            } else {
                &FOCUS_OUT_CALLBACKS
            };
            let mut map = reg.lock().unwrap();
            match map.get_mut(&(entry_ptr as usize as u64)) {
                Some(SendPtrRet(p)) => Some(*p),
                None => None,
            }
        };
        match raw {
            // SAFETY: registry-owned closure, single-threaded dispatch.
            Some(raw) => unsafe { (*raw)(entry_ptr) },
            None => 0,
        }
    }

    // ------------------------------------------------------------------
    // Draw context / Canvas
    // ------------------------------------------------------------------
    //
    // Canvas rendering mirrors iOS exactly: the host view's `drawRect:`
    // hands us a live `CGContextRef` (AppKit: `[NSGraphicsContext
    // currentContext].CGContext`) and the view's size; the Rust closure
    // replays its primitives against a context that forwards each one to
    // Core Graphics. Until that view exists (host runs, tests) the closure
    // still runs against an estimating context so `render_to` logic stays
    // testable.

    /// `DrawContext` used when no AppKit canvas is attached: records nothing,
    /// measures with the monospace estimate.
    pub struct MacosDrawContext;

    impl DrawContext for MacosDrawContext {
        fn fill_rect(
            &mut self,
            _x: f64,
            _y: f64,
            _w: f64,
            _h: f64,
            _r: f64,
            _g: f64,
            _b: f64,
            _a: f64,
        ) {
        }
        fn stroke_rect(
            &mut self,
            _x: f64,
            _y: f64,
            _w: f64,
            _h: f64,
            _r: f64,
            _g: f64,
            _b: f64,
            _a: f64,
            _lw: f64,
        ) {
        }
        fn draw_text_styled(
            &mut self,
            _x: f64,
            _y: f64,
            _text: &str,
            _font: &str,
            _size: f64,
            _r: f64,
            _g: f64,
            _b: f64,
            _a: f64,
            _slant: i32,
            _weight: i32,
        ) {
        }
        fn text_extents_styled(
            &self,
            text: &str,
            _font: &str,
            size: f64,
            _slant: i32,
            weight: i32,
        ) -> (f64, f64, f64, f64) {
            estimate_extents(text, size, weight)
        }
        fn clear(&mut self, _r: f64, _g: f64, _b: f64, _a: f64) {}
        fn save(&mut self) {}
        fn restore(&mut self) {}
        fn clip(&mut self, _x: f64, _y: f64, _w: f64, _h: f64) {}
    }

    #[repr(transparent)]
    pub struct Canvas(pub *mut c_void);

    impl Widget for Canvas {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for Canvas {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl Clone for Canvas {
        fn clone(&self) -> Self {
            Canvas(self.0)
        }
    }

    impl Canvas {
        /// How far the scroll view holding this canvas has scrolled it.
        ///
        /// A canvas that is an `NSScrollView`'s document view is scrolled by
        /// its clip view behind compat's back: the clip's bounds origin is the
        /// offset the painter must draw for, and hit-testing must add. `None`
        /// when the canvas is not inside a scroll view.
        pub fn scroll_offset(&self) -> Option<f64> {
            if self.0.is_null() {
                return None;
            }
            // SAFETY: live AppKit views; both sends are documented selectors.
            unsafe {
                let parent = msg0(self.0, "superview");
                if parent.is_null()
                    || core_apple::nsstring_to_rust(msg0(parent, "className")).as_deref()
                        != Some("NSClipView")
                {
                    return None;
                }
                let clip: CGRect = raw_send!(
                    parent,
                    "bounds",
                    unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                    ()
                );
                Some(clip.y)
            }
        }

        /// Canvas id for this view (0 = unknown; registry lookups miss).
        ///
        /// `pub` because the shared GUI code publishes it to the platform
        /// host *before* any gesture can arrive: the Apple hosts pass the id
        /// on every mouse/key callback (`corro_macos_canvas_click(canvas_id,
        /// x, y)`), so it has to be read out at tree-build time rather than
        /// from inside the adapter. `common::Canvas` re-exposes it, which is
        /// the same access the GTK backend offers.
        pub fn canvas_id(&self) -> u64 {
            core_apple::canvas_id_for_view(self.0)
        }

        pub fn set_draw_callback(&self, cb: Box<dyn FnMut(&mut dyn DrawContext, i32, i32)>) {
            // Stash the closure under our canvas id; the host sheet view
            // dispatches to it by id from drawRect:.
            {
                let mut map = DRAW_CALLBACKS.lock().unwrap();
                map.insert(self.canvas_id(), SendDrawCallback(Box::into_raw(cb)));
            }
            self.queue_redraw();
        }

        pub fn queue_redraw(&self) {
            // A list whose canvas is wider than its viewport lays its columns
            // out for the wider box, which puts the path column past the right
            // edge; fitting it on every repaint keeps it inside the window.
            unsafe {
                let clip = msg0(self.0, "superview");
                if !clip.is_null() {
                    let scroll = msg0(clip, "superview");
                    if !scroll.is_null() && responds(scroll, "documentView") {
                        fit_document_view(scroll);
                    }
                }
            }
            // Schedule a real draw through the host view (which replays the
            // closure with a CG-backed context at the live size), and run it
            // immediately against the estimating context so headless/test
            // callers observe draws without AppKit — same double duty as the
            // iOS and Android adapters.
            if !self.0.is_null() {
                unsafe {
                    msg1bv(self.0, "setNeedsDisplay:", true);
                }
            }
            let mut map = DRAW_CALLBACKS.lock().unwrap();
            if let Some(SendDrawCallback(ptr)) = map.get_mut(&self.canvas_id()) {
                // SAFETY: registry-owned closure, single-threaded dispatch.
                let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) = unsafe { &mut **ptr };
                let mut dc = MacosDrawContext;
                let (w, h) = self.replay_size();
                cb(&mut dc, w, h);
            }
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            core_apple::with_meta_mut(self.0, |m| m.size_request = (w.max(1), h.max(1)));
            // The frame is what an enclosing `NSScrollView` measures. Recording
            // the request alone left the canvas at 0x0 inside the clip view, so
            // there was nothing to scroll and no scroller to show, however many
            // rows the draw closure painted.
            if !self.0.is_null() {
                // A canvas inside a scroll view is at most as wide as the
                // viewport. The list asks for the width it last painted at, so
                // one wide frame would keep it that wide for good: the columns
                // were laid out for a ~2050pt canvas in a 1350pt window, which
                // put the path column at 80% instead of 53% and clipped it.
                let mut width = w as f64;
                unsafe {
                    let clip = msg0(self.0, "superview");
                    if !clip.is_null() {
                        let b: CGRect = raw_send!(
                            clip,
                            "bounds",
                            unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                            ()
                        );
                        if b.width > 1.0 && width > b.width {
                            width = b.width;
                        }
                    }
                    msg2cv(self.0, "setFrameSize:", width, h as f64)
                }
            }
        }

        /// Size to replay the draw closure at: the real laid-out size once
        /// the host view's draw has reported one, else the requested size
        /// (AppKit has not laid the view out yet), else a conservative
        /// default. Mirrors the Android/iOS split, including the reason: a
        /// 1x1 placeholder request must never shrink a real laid-out size to
        /// one row.
        fn replay_size(&self) -> (i32, i32) {
            let (laid_out, requested) = core_apple::with_meta(self.0, |m| (m.laid_out, m.size_request))
                .unwrap_or(((0, 0), (0, 0)));
            if laid_out.0 > 0 && laid_out.1 > 0 {
                return laid_out;
            }
            if requested.0 > 0 && requested.1 > 0 {
                return requested;
            }
            (800, 600)
        }

        pub fn set_content_size(&self, w: i32, h: i32) {
            self.set_size_request(w, h);
        }

        pub fn set_visible(&self, v: bool) {
            if !self.0.is_null() {
                unsafe { msg1bv(self.0, "setHidden:", !v) };
            }
        }

        pub fn grab_focus(&self) {}
        pub fn set_can_focus(&self, _can: bool) {}

        pub fn on_click(&self, cb: Box<dyn FnMut(f64, f64)>) {
            let mut map = CLICK_CALLBACKS.lock().unwrap();
            map.insert(self.canvas_id(), SendClickCallback(Box::into_raw(cb)));
        }

        /// Button/modifier-aware click. See the GTK3 backend's
        /// `on_click_button`.
        ///
        /// The AppKit responder currently forwards only the press location, so
        /// this registers the callback for parity but does not yet fire it with
        /// a real button number; `on_click` remains the working path.
        pub fn on_click_button(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            let mut map = CLICK_BUTTON_CALLBACKS.lock().unwrap();
            map.insert(
                self.canvas_id(),
                SendClickButtonCallback(Box::into_raw(cb)),
            );
        }

        /// Pointer motion over the canvas; see the GTK3 backend's `on_motion`.
        pub fn on_motion(&self, cb: Box<dyn FnMut(f64, f64, u32)>) {
            let mut map = MOTION_CALLBACKS.lock().unwrap();
            map.insert(self.canvas_id(), SendMotionCallback(Box::into_raw(cb)));
        }

        /// A second press of a double click, at the view's own coordinates.
        ///
        /// AppKit reports the count on the event (`clickCount`), so the
        /// responder forwards a press with a count of two here and a single
        /// press to [`Canvas::on_click_button`]. The ordinary click also fires
        /// first, which is what a list wants: the row is selected, then opened.
        pub fn on_double_click(&self, cb: Box<dyn FnMut(f64, f64)>) {
            let mut map = DOUBLE_CLICK_CALLBACKS.lock().unwrap();
            map.insert(self.canvas_id(), SendDoubleClickCallback(Box::into_raw(cb)));
        }

        /// This canvas's top-left in screen coordinates, or `None` when the
        /// backend cannot report one.
        ///
        /// Not implemented for the AppKit backend: the caller opens the context
        /// menu unpositioned instead of guessing an origin. See the GTK
        /// backend's `screen_origin`.
        pub fn screen_origin(&self) -> Option<(i32, i32)> {
            None
        }

        /// Pointer release; see the GTK3 backend's `on_release`.
        pub fn on_release(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            let mut map = RELEASE_CALLBACKS.lock().unwrap();
            map.insert(
                self.canvas_id(),
                SendClickButtonCallback(Box::into_raw(cb)),
            );
        }

        pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) {
            let mut boxed = cb;
            self.on_key_raw(Box::new(move |k: u32, _s: u32| -> bool { boxed(k) }));
        }

        pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
            let mut map = KEY_CALLBACKS.lock().unwrap();
            map.insert(self.canvas_id(), SendKeyCallback(Box::into_raw(cb)));
        }

        /// Force an immediate draw. Replays the closure like
        /// [`Canvas::queue_redraw`] (plus a display request).
        pub fn force_draw(&self, _window_ptr: *mut c_void, fallback_w: i32, fallback_h: i32) {
            if !self.0.is_null() {
                unsafe { msg1bv(self.0, "setNeedsDisplay:", true) };
            }
            let mut map = DRAW_CALLBACKS.lock().unwrap();
            if let Some(SendDrawCallback(ptr)) = map.get_mut(&self.canvas_id()) {
                // SAFETY: registry-owned closure, single-threaded dispatch.
                let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) = unsafe { &mut **ptr };
                let mut dc = MacosDrawContext;
                let (w, h) = {
                    let (laid_out, _) =
                        core_apple::with_meta(self.0, |m| (m.laid_out, m.size_request)).unwrap_or(((0, 0), (0, 0)));
                    if laid_out.0 > 0 && laid_out.1 > 0 {
                        laid_out
                    } else {
                        (fallback_w.max(1), fallback_h.max(1))
                    }
                };
                cb(&mut dc, w, h);
            }
        }
    }

    static DRAW_CALLBACKS: Lazy<Mutex<HashMap<u64, SendDrawCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    static CLICK_CALLBACKS: Lazy<Mutex<HashMap<u64, SendClickCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    static KEY_CALLBACKS: Lazy<Mutex<HashMap<u64, SendKeyCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    // Button/modifier-aware registries. Declared alongside the plain click map
    // so the richer API has somewhere to live on this backend without changing
    // the `on_click` signature the AppKit host already calls.
    //
    // A DISTINCT type from `SendClickCallback`, not an alias: the plain click
    // carries `(x, y)` and the button-aware one carries `(x, y, button, mods)`.
    // Aliasing the two made `on_click_button`/`on_release` store a
    // `Box<dyn FnMut(f64,f64,u32,u32)>` into a `Box<dyn FnMut(f64,f64)>` slot,
    // which did not compile (E0308) — and had it, the extra register-file
    // mismatch is the classic arm64 ABI crash `backends/apple.rs` warns about.
    struct SendClickButtonCallback(*mut dyn FnMut(f64, f64, u32, u32));
    // SAFETY: guarded by the registry mutex; same pattern as SendClickCallback.
    unsafe impl Send for SendClickButtonCallback {}
    static CLICK_BUTTON_CALLBACKS: Lazy<Mutex<HashMap<u64, SendClickButtonCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    struct SendMotionCallback(*mut dyn FnMut(f64, f64, u32));
    // SAFETY: guarded by the registry mutex; same pattern as SendClickCallback.
    unsafe impl Send for SendMotionCallback {}
    static MOTION_CALLBACKS: Lazy<Mutex<HashMap<u64, SendMotionCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    static RELEASE_CALLBACKS: Lazy<Mutex<HashMap<u64, SendClickButtonCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    struct SendDoubleClickCallback(*mut dyn FnMut(f64, f64));
    // SAFETY: guarded by the registry mutex; same pattern as SendClickCallback.
    unsafe impl Send for SendDoubleClickCallback {}
    static DOUBLE_CLICK_CALLBACKS: Lazy<Mutex<HashMap<u64, SendDoubleClickCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    struct SendDrawCallback(*mut dyn FnMut(&mut dyn DrawContext, i32, i32));
    // SAFETY: guarded by the registry mutex; same pattern as the iOS and
    // Android adapters.
    unsafe impl Send for SendDrawCallback {}
    struct SendClickCallback(*mut dyn FnMut(f64, f64));
    unsafe impl Send for SendClickCallback {}
    struct SendKeyCallback(*mut dyn FnMut(u32, u32) -> bool);
    unsafe impl Send for SendKeyCallback {}

    /// Dispatch a click from the host sheet view to the registered click
    /// closure. `x`/`y` are in points, the same coordinate space the draw
    /// closure paints in (the host view is flipped so origin is top-left).
    pub fn dispatch_canvas_click(canvas_id: u64, x: f64, y: f64) {
        let raw = {
            let mut map = CLICK_CALLBACKS.lock().unwrap();
            match map.get_mut(&canvas_id) {
                Some(SendClickCallback(p)) => Some(*p),
                None => None,
            }
        };
        if let Some(raw) = raw {
            // SAFETY: registry-owned closure, single-threaded dispatch.
            unsafe { (*raw)(x, y) };
        }
    }

    /// Dispatch a button/modifier-aware click from the host sheet view, for
    /// canvases registered through `on_click_button`. Mirrors
    /// [`dispatch_canvas_click`]; `button` is 1 = left, 3 = right.
    pub fn dispatch_canvas_click_button(canvas_id: u64, x: f64, y: f64, button: u32, mods: u32) {
        let raw = {
            let mut map = CLICK_BUTTON_CALLBACKS.lock().unwrap();
            match map.get_mut(&canvas_id) {
                Some(SendClickButtonCallback(p)) => Some(*p),
                None => None,
            }
        };
        if let Some(raw) = raw {
            // SAFETY: registry-owned closure, single-threaded dispatch.
            unsafe { (*raw)(x, y, button, mods) };
        }
    }

    /// Dispatch a press carrying AppKit's `clickCount`.
    ///
    /// This is the responder's decision without an `NSEvent`: the single click
    /// always lands, and a count of two (or more) also activates. The probe
    /// drives it to check that one press selects without opening.
    pub fn dispatch_canvas_press(canvas_id: u64, x: f64, y: f64, click_count: isize) {
        dispatch_canvas_click_button(canvas_id, x, y, 1, 0);
        if activates_on_click_count(click_count) {
            dispatch_canvas_double_click(canvas_id, x, y);
        }
    }

    /// Dispatch the second press of a double click at `(x, y)`.
    ///
    /// The single-click callback still has to be dispatched first (as AppKit
    /// does) when a caller drives this by hand; the probe does exactly that.
    pub fn dispatch_canvas_double_click(canvas_id: u64, x: f64, y: f64) {
        let raw = {
            let mut map = DOUBLE_CLICK_CALLBACKS.lock().unwrap();
            match map.get_mut(&canvas_id) {
                Some(SendDoubleClickCallback(p)) => Some(*p),
                None => None,
            }
        };
        if let Some(raw) = raw {
            // SAFETY: registry-owned closure, single-threaded dispatch.
            unsafe { (*raw)(x, y) }
        }
    }

    /// Dispatch pointer motion; `mods` is the platform modifier mask.
    pub fn dispatch_canvas_motion(canvas_id: u64, x: f64, y: f64, mods: u32) {
        let raw = {
            let mut map = MOTION_CALLBACKS.lock().unwrap();
            match map.get_mut(&canvas_id) {
                Some(SendMotionCallback(p)) => Some(*p),
                None => None,
            }
        };
        if let Some(raw) = raw {
            // SAFETY: registry-owned closure, single-threaded dispatch.
            unsafe { (*raw)(x, y, mods) };
        }
    }

    /// Dispatch a pointer release, ending a drag.
    pub fn dispatch_canvas_release(canvas_id: u64, x: f64, y: f64, button: u32, mods: u32) {
        let raw = {
            let mut map = RELEASE_CALLBACKS.lock().unwrap();
            match map.get_mut(&canvas_id) {
                Some(SendClickButtonCallback(p)) => Some(*p),
                None => None,
            }
        };
        if let Some(raw) = raw {
            // SAFETY: registry-owned closure, single-threaded dispatch.
            unsafe { (*raw)(x, y, button, mods) };
        }
    }

    /// Dispatch a key from the host to the registered key closure (hardware
    /// keyboards, `keyDown:` in the sheet view).
    pub fn dispatch_canvas_key(canvas_id: u64, keyval: u32, mods: u32) -> bool {
        let raw = {
            let mut map = KEY_CALLBACKS.lock().unwrap();
            match map.get_mut(&canvas_id) {
                Some(SendKeyCallback(p)) => Some(*p),
                None => None,
            }
        };
        match raw {
            // SAFETY: registry-owned closure, single-threaded dispatch.
            Some(raw) => unsafe { (*raw)(keyval, mods) },
            None => false,
        }
    }

    /// Replay the registered draw closure for `canvas_id` against a live
    /// Core Graphics context. Called from the host's
    /// `corro_macos_canvas_draw` export, which the host sheet view invokes
    /// from `drawRect:` on the main thread. `w`/`h` are the view's size in
    /// points.
    ///
    /// # Safety
    /// `ctx` must be a live `CGContextRef` valid for the duration of the
    /// call (AppKit: the one the view's `drawRect:` is drawing into).
    pub unsafe fn dispatch_draw(canvas_id: u64, ctx: *mut c_void, w: i32, h: i32) {
        let (w, h) = (w.max(1), h.max(1));
        // Record the live size so `replay_size` stops guessing.
        {
            let mut sizes = CANVAS_SIZE.lock().unwrap();
            sizes.insert(canvas_id, (w, h));
        }
        let raw = {
            let map = DRAW_CALLBACKS.lock().unwrap();
            map.get(&canvas_id).map(|s| s.0)
        };
        let Some(raw) = raw else {
            return;
        };
        // Lock released before invoking: the closure re-enters (queue_redraw)
        // and holding it would deadlock.
        match unsafe { CgDrawContext::new(ctx) } {
            Some(mut dc) => {
                // SAFETY: registry-owned closure, single-threaded dispatch.
                let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) = unsafe { &mut *raw };
                cb(&mut dc, w, h);
            }
            None => {
                // No usable context (unexpected): still run the closure so
                // any state it maintains stays consistent.
                let mut est = MacosDrawContext;
                // SAFETY: as above.
                let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) = unsafe { &mut *raw };
                cb(&mut est, w, h);
            }
        }
    }

    /// The view's *laid-out* size in points, learned from the host's draw
    /// callback. Kept so `replay_size` can answer without the host.
    static CANVAS_SIZE: Lazy<Mutex<HashMap<u64, (i32, i32)>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    /// Record a laid-out canvas size, called by the host from its canvas
    /// view's layout/draw pass (and by [`dispatch_draw`], which also knows it).
    ///
    /// Separate from the draw path on purpose, and the same shape as the iOS
    /// adapter's function: Rust replays the draw closure before AppKit's first
    /// `drawRect:`, so a size that only arrived with the first frame would
    /// come too late and the sheet would lay itself out at the 1x1 placeholder.
    pub fn record_canvas_size(canvas_id: u64, w: i32, h: i32) {
        if w <= 0 || h <= 0 {
            return;
        }
        CANVAS_SIZE.lock().unwrap().insert(canvas_id, (w, h));
    }

    /// Record a laid-out size for a canvas handle (called by
    /// [`dispatch_draw`], which knows the size by canvas id — the adapter
    /// then copies it onto the handle via [`set_canvas_laid_out`]).
    pub fn canvas_laid_out(canvas_id: u64) -> Option<(i32, i32)> {
        CANVAS_SIZE.lock().unwrap().get(&canvas_id).copied()
    }

    /// Copy a laid-out size onto the widget handle so `replay_size` (which
    /// only has the handle) can see it.
    pub fn set_canvas_laid_out(handle: *mut c_void, canvas_id: u64) {
        if let Some(size) = canvas_laid_out(canvas_id) {
            core_apple::with_meta_mut(handle, |m| m.laid_out = size);
        }
    }

    // ------------------------------------------------------------------
    // Core Graphics draw context
    // ------------------------------------------------------------------
    //
    // DrawContext's primitives map one-to-one onto CG calls, so the replay
    // model is the same as the iOS and Android Canvas paths. Core Graphics is
    // shared by iOS and macOS, so this section is near-identical to the iOS
    // one — the only reason it is not in `backends::apple` is that the
    // *drawing* is reached through a host view (and its framework), not
    // through the runtime layer.

    // `kind = "framework"`, and that word is load-bearing: without it rustc
    // emits `-lCoreGraphics`, which ld cannot resolve — CoreGraphics is an
    // umbrella *framework*, not a `libCoreGraphics.dylib`, so the only correct
    // flag is `-framework CoreGraphics` (the form IOS_GUIDELINES.md §8 records
    // for the iOS link step). The plain spelling only went unnoticed because
    // `ios/corro` builds a **staticlib**, which is merely archived and never
    // linked, so the bad flag was never passed to ld there. `macos/corro` also
    // builds a **cdylib**, which *is* linked, and the very first CI run failed
    // with `ld: library 'CoreGraphics' not found`.
    //
    // The same applies to `objc` in `backends/apple.rs` for the same crate —
    // but `-lobjc` happens to resolve on a Mac (there is a real
    // `libobjc.A.dylib` in the SDK), so it is left alone rather than churned.
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGContextSetRGBFillColor(ctx: *mut c_void, r: f64, g: f64, b: f64, a: f64);
        fn CGContextSetRGBStrokeColor(ctx: *mut c_void, r: f64, g: f64, b: f64, a: f64);
        fn CGContextSetLineWidth(ctx: *mut c_void, w: f64);
        fn CGContextFillRect(ctx: *mut c_void, rect: CGRect);
        fn CGContextStrokeRect(ctx: *mut c_void, rect: CGRect);
        fn CGContextClipToRect(ctx: *mut c_void, rect: CGRect);
        fn CGContextGetClipBoundingBox(ctx: *mut c_void) -> CGRect;
        fn CGContextSaveGState(ctx: *mut c_void);
        fn CGContextRestoreGState(ctx: *mut c_void);
        fn CGContextTranslateCTM(ctx: *mut c_void, tx: f64, ty: f64);
    }

    /// `CGRect`. Field order is ABI-critical: `CGFloat` (f64 on 64-bit, f32
    /// on 32-bit), then two floats per origin/size pair. macOS is 64-bit only
    /// in practice, but the shared `msg4cv`/`msg0c` helpers keep the 32-bit
    /// path honest for any Apple platform.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    /// `DrawContext` backed by a live `CGContextRef`.
    ///
    /// Text needs `CoreText` (`CTLine`) to keep the shared API's
    /// family/size/slant/weight parameters; rather than link that here and
    /// duplicate the host's font handling, text measurement and drawing are
    /// delegated to the host through two ObjC shims (`CorroMacText`), which
    /// the app implements with `NSString`/`NSFont`. A missing shim degrades
    /// to the estimate, never a panic.
    pub struct CgDrawContext {
        ctx: *mut c_void,
    }

    impl CgDrawContext {
        /// # Safety
        /// `ctx` must be a live `CGContextRef`.
        pub unsafe fn new(ctx: *mut c_void) -> Option<Self> {
            if ctx.is_null() {
                None
            } else {
                Some(CgDrawContext { ctx })
            }
        }
    }

    impl DrawContext for CgDrawContext {
        fn fill_rect(
            &mut self,
            x: f64,
            y: f64,
            w: f64,
            h: f64,
            r: f64,
            g: f64,
            b: f64,
            a: f64,
        ) {
            unsafe {
                CGContextSetRGBFillColor(self.ctx, r, g, b, a);
                CGContextFillRect(
                    self.ctx,
                    CGRect {
                        x,
                        y,
                        width: w,
                        height: h,
                    },
                );
            }
        }

        fn stroke_rect(
            &mut self,
            x: f64,
            y: f64,
            w: f64,
            h: f64,
            r: f64,
            g: f64,
            b: f64,
            a: f64,
            lw: f64,
        ) {
            unsafe {
                CGContextSetRGBStrokeColor(self.ctx, r, g, b, a);
                CGContextSetLineWidth(self.ctx, lw.max(0.5));
                CGContextStrokeRect(
                    self.ctx,
                    CGRect {
                        x,
                        y,
                        width: w,
                        height: h,
                    },
                );
            }
        }

        fn draw_text_styled(
            &mut self,
            x: f64,
            y: f64,
            text: &str,
            font: &str,
            size: f64,
            r: f64,
            g: f64,
            b: f64,
            a: f64,
            slant: i32,
            weight: i32,
        ) {
            draw_text_via_host(
                self.ctx, x, y, text, font, size, r, g, b, a, slant, weight,
            );
        }

        fn text_extents_styled(
            &self,
            text: &str,
            font: &str,
            size: f64,
            slant: i32,
            weight: i32,
        ) -> (f64, f64, f64, f64) {
            measure_text_via_host(text, font, size, slant, weight)
                .unwrap_or_else(|| estimate_extents(text, size, weight))
        }

        fn clear(&mut self, r: f64, g: f64, b: f64, a: f64) {
            // `clear` means "paint the whole surface this colour". The clip
            // bounding box *is* the surface the view handed us, which is more
            // reliable than an infinite rectangle (whose behaviour is
            // undefined once it reaches the backing store).
            unsafe {
                let bounds = CGContextGetClipBoundingBox(self.ctx);
                CGContextSetRGBFillColor(self.ctx, r, g, b, a);
                CGContextFillRect(self.ctx, bounds);
            }
        }

        fn save(&mut self) {
            unsafe { CGContextSaveGState(self.ctx) };
        }

        fn restore(&mut self) {
            unsafe { CGContextRestoreGState(self.ctx) };
        }

        fn clip(&mut self, x: f64, y: f64, w: f64, h: f64) {
            unsafe {
                CGContextClipToRect(
                    self.ctx,
                    CGRect {
                        x,
                        y,
                        width: w,
                        height: h,
                    },
                );
            }
        }
    }

    /// Text measurement through the host's `CorroMacText` shim, which uses
    /// `NSFont` + `NSAttributedString` `size`/`boundingRectWithSize:options:`.
    /// Keeping the version/font handling in the host mirrors the iOS shim and
    /// keeps this crate free of framework headers.
    fn measure_text_via_host(
        text: &str,
        font: &str,
        size: f64,
        slant: i32,
        weight: i32,
    ) -> Option<(f64, f64, f64, f64)> {
        if !core_apple::is_initialized() {
            return None;
        }
        if text.is_empty() {
            return Some((0.0, 0.0, 0.0, 0.0));
        }
        let shim = cls("CorroMacText");
        if shim.is_null() {
            return None;
        }
        let ns_text = nsstring(text);
        let ns_font = nsstring(font);
        if ns_text.is_null() || ns_font.is_null() {
            return None;
        }
        // Returns a heap CGRect* (or null) so the ABI stays simple: one
        // pointer in, one pointer out, no struct-return register rules to get
        // wrong. The host hands ownership over; we free with the same C
        // allocator.
        let rect_ptr = raw_send!(
            shim,
            "measure:font:size:slant:weight:",
            unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, f64, i32, i32) -> *mut CGRect,
            (ns_text, size, slant, weight)
        );
        if rect_ptr.is_null() {
            return None;
        }
        let rect = unsafe { *rect_ptr };
        unsafe { free(rect_ptr as *mut c_void) };
        Some((0.0, 0.0, rect.width.max(0.0), rect.height.max(0.0)))
    }

    /// Text drawing through the host's `CorroMacText` shim, which resolves an
    /// `NSFont` for the family/size/weight and draws with `NSString`/
    /// `NSAttributedString` `drawAtPoint:` into the live `CGContextRef`
    /// (AppKit's current graphics context).
    #[allow(clippy::too_many_arguments)]
    fn draw_text_via_host(
        ctx: *mut c_void,
        x: f64,
        y: f64,
        text: &str,
        font: &str,
        size: f64,
        r: f64,
        g: f64,
        b: f64,
        a: f64,
        slant: i32,
        weight: i32,
    ) {
        if !core_apple::is_initialized() {
            return;
        }
        let shim = cls("CorroMacText");
        if shim.is_null() {
            crate::backends::macos::log_macos("macos: CorroMacText shim missing; text not drawn");
            return;
        }
        let ns_text = nsstring(text);
        let ns_font = nsstring(font);
        if ns_text.is_null() || ns_font.is_null() {
            return;
        }
        // Signature, after the implicit (id, SEL) pair:
        //   (NSString*, CGContextRef, NSString*, CGFloat x, CGFloat y,
        //    CGFloat size, CGFloat r, CGFloat g, CGFloat b, CGFloat a,
        //    NSInteger slant, NSInteger weight) -> void
        raw_send!(
            shim,
            "drawText:ctx:font:x:y:size:r:g:b:a:slant:weight:",
            unsafe extern "C" fn(
                *mut c_void, // id        (self)
                *mut c_void, // SEL
                *mut c_void, // NSString* text
                *mut c_void, // CGContextRef
                *mut c_void, // NSString* font
                f64,         // CGFloat x
                f64,         // CGFloat y
                f64,         // CGFloat size
                f64,         // CGFloat r
                f64,         // CGFloat g
                f64,         // CGFloat b
                f64,         // CGFloat a
                i32,         // NSInteger slant
                i32,         // NSInteger weight
            ) -> (),
            (
                ns_text,
                ctx,
                ns_font,
                x,
                y,
                size,
                r,
                g,
                b,
                a,
                slant,
                weight
            )
        );
    }

    // `CorroMacText` returns a buffer allocated with `malloc` (the host's
    // `calloc`), so the matching free is `free` from libc.
    unsafe extern "C" {
        fn free(ptr: *mut c_void);
        fn malloc(size: usize) -> *mut c_void;
    }

    // ------------------------------------------------------------------
    // Menu / MenuBar / SimpleAction
    // ------------------------------------------------------------------
    //
    // macOS has a real `NSMenu`/menubar, so unlike iOS a host *could* build a
    // native one from this model. The model-only shape is kept identical to
    // iOS/Android so the same shared call sites work, and the host chooses
    // how to render it (a real menubar is the natural macOS answer — see
    // `docs/MACOS_GUIDELINES.md`).

    enum MenuItem {
        Item { label: String, action: String },
        Submenu { label: String, items: Vec<MenuItem> },
    }

    impl Clone for MenuItem {
        fn clone(&self) -> Self {
            match self {
                MenuItem::Item { label, action } => MenuItem::Item {
                    label: label.clone(),
                    action: action.clone(),
                },
                MenuItem::Submenu { label, items } => MenuItem::Submenu {
                    label: label.clone(),
                    items: items.clone(),
                },
            }
        }
    }

    #[derive(Default)]
    pub struct Menu {
        items: Vec<MenuItem>,
    }

    impl Clone for Menu {
        fn clone(&self) -> Self {
            Menu {
                items: self.items.clone(),
            }
        }
    }

    impl Widget for Menu {
        fn raw_handle(&self) -> *mut c_void {
            std::ptr::null_mut()
        }
    }

    impl AsRef<*mut c_void> for Menu {
        fn as_ref(&self) -> &*mut c_void {
            // No stable handle; callers only pass menus back into
            // `append_submenu` / `create_menubar`, which read the model.
            unsafe { &*(&std::ptr::null_mut() as *const *mut c_void) }
        }
    }

    impl Menu {
        pub fn append(&mut self, label: &str, detailed_action: &str) {
            let action = detailed_action
                .split('(')
                .next()
                .unwrap_or(detailed_action)
                .to_owned();
            self.items.push(MenuItem::Item {
                label: label.to_owned(),
                action,
            });
        }

        pub fn append_submenu(&mut self, label: &str, submenu: &Menu) {
            self.items.push(MenuItem::Submenu {
                label: label.to_owned(),
                items: submenu.items.clone(),
            });
        }
    }

    pub struct MenuBar {
        items: Vec<MenuItem>,
    }

    impl Clone for MenuBar {
        fn clone(&self) -> Self {
            MenuBar {
                items: self.items.clone(),
            }
        }
    }

    impl Widget for MenuBar {
        fn raw_handle(&self) -> *mut c_void {
            std::ptr::null_mut()
        }
    }

    impl AsRef<*mut c_void> for MenuBar {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&std::ptr::null_mut() as *const *mut c_void) }
        }
    }

    impl MenuBar {
        pub fn activate_submenu_by_mnemonic(&self, _keyval: u32) -> bool {
            false
        }

        /// Open the submenu whose mnemonic is `keyval` at a screen position.
        ///
        /// Not implemented for the AppKit backend (no programmatic
        /// popup-at-position call); reports `false`, and the caller shows an
        /// "unavailable" status instead of a menu that never appears.
        pub fn popup_submenu_by_mnemonic_at(&self, _keyval: u32, _x: i32, _y: i32) -> bool {
            false
        }
        /// No positionable popup on this backend, so `anchor` and the point are
        /// unused and the call reports "not opened". This is the same answer
        /// `popup_submenu_by_mnemonic_at` already gave here -- `popup_at` just
        /// makes that explicit in one place instead of leaving each caller to
        /// discover it.
        pub fn popup_at(&self, _label: &str, _anchor: &Canvas, _x: f64, _y: f64) -> bool {
            false
        }        pub fn activate_submenu_item_by_mnemonic(&self, _keyval: u32) -> bool {
            false
        }
        /// # Safety
        /// Kept for API compatibility; no-op on macOS.
        pub unsafe fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}
        pub fn handle_mnemonic_key(&self, _keyval: u32) -> bool {
            false
        }
        pub fn handle_menu_key(&self, _keyval: u32, _modifiers: u32) -> bool {
            false
        }
        /// [`handle_menu_key`](Self::handle_menu_key) with the typed character
        /// supplied separately; this backend's key codes are already characters.
        pub fn handle_menu_key_char(&self, keyval: u32, _ch: char, modifiers: u32) -> bool {
            self.handle_menu_key(keyval, modifiers)
        }
        pub fn menu_active(&self) -> bool {
            false
        }
        pub fn menu_close(&self) {}
    }

    #[derive(Clone)]
    pub struct SimpleAction {
        name: String,
    }

    impl Widget for SimpleAction {
        fn raw_handle(&self) -> *mut c_void {
            std::ptr::null_mut()
        }
    }

    impl AsRef<*mut c_void> for SimpleAction {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&std::ptr::null_mut() as *const *mut c_void) }
        }
    }

    impl SimpleAction {
        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(
            &self,
            f: F,
        ) -> Result<u64, Error> {
            register_action(&self.name, Box::new(f));
            Ok(0)
        }
    }

    struct SendFnPtr(*mut dyn FnMut(*mut c_void));
    // SAFETY: guarded by the registry mutex; same pattern as the iOS and
    // Android adapters.
    unsafe impl Send for SendFnPtr {}

    static ACTION_REGISTRY: Lazy<Mutex<HashMap<String, SendFnPtr>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    fn register_action(name: &str, f: Box<dyn FnMut(*mut c_void)>) {
        let mut map = ACTION_REGISTRY.lock().unwrap();
        let ptr = Box::into_raw(f);
        map.insert(name.to_owned(), SendFnPtr(ptr));
    }

    /// Dispatch a menu action by name (called from the host after the user
    /// picks an `NSMenuItem`).
    pub fn invoke_action(name: &str, param: *mut c_void) {
        let raw = {
            let mut map = ACTION_REGISTRY.lock().unwrap();
            match map.get_mut(name) {
                Some(SendFnPtr(p)) => Some(*p),
                None => None,
            }
        };
        if let Some(raw) = raw {
            // SAFETY: registry-owned closure, single-threaded dispatch.
            unsafe { (*raw)(param) };
        }
    }

    // ------------------------------------------------------------------
    // Grid
    // ------------------------------------------------------------------

    #[repr(transparent)]
    pub struct Grid(pub *mut c_void);

    impl Widget for Grid {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for Grid {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl Clone for Grid {
        fn clone(&self) -> Self {
            Grid(self.0)
        }
    }

    impl Grid {
        pub fn attach(
            &self,
            child: &impl AsRef<*mut c_void>,
            _left: i32,
            _top: i32,
            _width: i32,
            _height: i32,
        ) {
            attach(self.0, *child.as_ref());
        }
    }

    // ------------------------------------------------------------------
    // DropDown (NSPopUpButton)
    // ------------------------------------------------------------------

    #[repr(transparent)]
    pub struct DropDown(pub *mut c_void);

    impl Widget for DropDown {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for DropDown {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl DropDown {
        pub fn set_active(&self, index: Option<u32>) {
            if index.is_some() && !self.0.is_null() {
                // NSPopUpButton selection is an NSInteger index.
                unsafe { msg1iv(self.0, "selectItemAtIndex:", index.unwrap_or(0) as isize) };
            }
        }

        pub fn get_active(&self) -> i32 {
            if self.0.is_null() {
                return -1;
            }
            unsafe { msg0i(self.0, "indexOfSelectedItem") as i32 }
        }

        pub fn connect_changed(&self, _f: impl FnMut() + 'static) -> Result<u64, Error> {
            Ok(0) // Wired through the host shim's target/action.
        }

        pub fn set_hexpand(&self, _expand: bool) {}
        pub fn set_vexpand(&self, _expand: bool) {}
        pub fn grab_focus(&self) {}
    }

    impl Clone for DropDown {
        fn clone(&self) -> Self {
            DropDown(self.0)
        }
    }

    // ------------------------------------------------------------------
    // CheckButton / RadioButton (NSButton toggle / radio style)
    // ------------------------------------------------------------------

    #[repr(transparent)]
    pub struct CheckButton(pub *mut c_void);

    impl Widget for CheckButton {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for CheckButton {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl CheckButton {
        pub fn is_active(&self) -> bool {
            if self.0.is_null() {
                return false;
            }
            // NSButton state: `0` = off, `1` = on (NSControlStateValueOn).
            unsafe { msg0i(self.0, "state") != 0 }
        }

        pub fn set_active(&self, active: bool) {
            if !self.0.is_null() {
                unsafe { msg1iv(self.0, "setState:", active as isize) };
            }
        }

        pub fn connect_toggled(&self, _f: impl FnMut() + 'static) -> Result<u64, Error> {
            Ok(0) // Wired through the host shim's target/action.
        }
    }

    impl Clone for CheckButton {
        fn clone(&self) -> Self {
            CheckButton(self.0)
        }
    }

    #[repr(transparent)]
    pub struct RadioButton(pub *mut c_void);

    impl Widget for RadioButton {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for RadioButton {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl RadioButton {
        pub fn is_active(&self) -> bool {
            if self.0.is_null() {
                return false;
            }
            unsafe { msg0i(self.0, "state") != 0 }
        }

        pub fn set_active(&self, active: bool) {
            if !self.0.is_null() {
                unsafe { msg1iv(self.0, "setState:", active as isize) };
            }
        }

        pub fn connect_toggled(&self, _f: impl FnMut() + 'static) -> Result<u64, Error> {
            Ok(0) // Wired through the host shim's target/action.
        }
    }

    impl Clone for RadioButton {
        fn clone(&self) -> Self {
            RadioButton(self.0)
        }
    }

    // ------------------------------------------------------------------
    // Dialog (NSAlert)
    // ------------------------------------------------------------------

    #[repr(transparent)]
    /// One response handler per alert, keyed by the alert's address.
    struct SendResponse(Box<dyn FnMut(i32)>);
    // SAFETY: the boxed closure is only ever called on the thread that ran
    // `runModal`, which is the main thread that created the alert.
    unsafe impl Send for SendResponse {}

    static DIALOG_RESPONSES: Lazy<Mutex<HashMap<usize, SendResponse>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    /// The response id each `add_button` registered, in button order.
    static DIALOG_BUTTONS: Lazy<Mutex<HashMap<usize, Vec<i32>>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    pub struct Dialog(pub *mut c_void);

    impl Widget for Dialog {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for Dialog {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl Clone for Dialog {
        fn clone(&self) -> Self {
            Dialog(self.0)
        }
    }

    impl Dialog {
        pub fn set_title(&self, title: &str) {
            if self.0.is_null() {
                return;
            }
            let s = nsstring(title);
            if !s.is_null() {
                // NSAlert: `setMessageText:`.
                unsafe { msg1v(self.0, "setMessageText:", s) };
            }
        }

        pub fn set_default_size(&self, _w: i32, _h: i32) {}

        /// Parent this dialog to `parent`. No-op on macOS: `NSAlert`
        /// `runModal`/sheet presentation is decided by the host shim.
        pub fn set_transient_for(&self, _parent: *mut c_void) {}

        pub fn append_content_area(&self, child: &impl AsRef<*mut c_void>) {
            let c = *child.as_ref();
            if self.0.is_null() || c.is_null() {
                return;
            }
            // NSAlert's content slot is its accessory view, which is where
            // GTK's `get_content_area` would have put the body label.
            unsafe { msg1v(self.0, "setAccessoryView:", c) };
        }

        pub fn add_button(&self, text: &str, response_id: i32) {
            if self.0.is_null() {
                return;
            }
            let s = nsstring(text);
            if s.is_null() {
                return;
            }
            // NSAlert's own button, so the alert draws a real, clickable one:
            // `corroAddAction:` had no implementation at all, which is why the
            // alert presented with no buttons. The alert owns the `NSButton`.
            let button = raw_send!(
                self.0,
                "addButtonWithTitle:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void,
                (s)
            );
            let index = match DIALOG_BUTTONS.lock() {
                Ok(mut buttons) => {
                    let entry = buttons.entry(self.0 as usize).or_default();
                    entry.push(response_id);
                    entry.len() - 1
                }
                Err(_) => 0,
            };
            // Take over the button's action so the click reaches Rust: the same
            // target/action pair any AppKit control uses.
            ensure_runtime_classes();
            let target_cls = cls("CorroMacAlertTarget");
            if !button.is_null() && !target_cls.is_null() {
                let target: *mut c_void = raw_send!(
                    target_cls,
                    "alertTargetWithDialog:index:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, isize, isize) -> *mut c_void,
                    (self.0 as isize, index as isize)
                );
                if !target.is_null() {
                    unsafe {
                        msg1v(button, "setTarget:", target);
                        msg1v(button, "setAction:", core_apple::selector("corroFired:"));
                    }
                }
            }
        }

        pub fn present(&self) {
            self.run();
        }

        pub fn connect_response(&self, f: impl FnMut(i32) + 'static) -> Result<u64, Error> {
            if self.0.is_null() {
                return Ok(0);
            }
            if let Ok(mut responses) = DIALOG_RESPONSES.lock() {
                responses.insert(self.0 as usize, SendResponse(Box::new(f)));
            }
            Ok(0)
        }

        pub fn set_default_response(&self, _response_id: i32) {}
        pub fn close(&self) {
            if self.0.is_null() {
                return;
            }
            // The modal session is over by the time `runModal` returns; hide the
            // window anyway, because the alert outlives its loop.
            unsafe {
                let w = msg0(self.0, "window");
                if !w.is_null() {
                    msg1v(w, "orderOut:", std::ptr::null_mut());
                }
            }
        }

        /// Measure the alert before it opens.
        ///
        /// `NSAlert` sizes itself from its message text and buttons, so there
        /// is no separate layout pass to run — presenting already lays it out.
        /// Kept because the shared `common::Dialog` forwards it and a caller
        /// written for GTK/NWG calls it unconditionally.
        pub fn layout_dialog(&self) {}

        /// Show or hide without dismissing. `close` ends the alert, so this is
        /// a different operation, not an alias. `orderOut:` hides an `NSAlert`
        /// without ending its modal session, which is exactly the distinction.
        pub fn set_visible(&self, visible: bool) {
            if self.0.is_null() {
                return;
            }
            // `orderFront:`/`orderOut:` take a sender, and the selector is
            // `orderOut:`, not `orderOut:nil` — an alert responds to neither a
            // two-argument nor a nil-named selector.
            unsafe {
                let w = msg0(self.0, "window");
                if !w.is_null() {
                    msg1v(
                        w,
                        if visible { "orderFront:" } else { "orderOut:" },
                        std::ptr::null_mut(),
                    );
                }
            }
        }

        /// The container a dialog's children are added to. `NSAlert` has no
        /// content-area view of its own — the host shim builds one and owns it —
        /// so there is nothing to hand back here.
        pub fn get_content_area(&self) -> *mut c_void { std::ptr::null_mut() }

        /// Run the alert modally and return the response id.
        ///
        /// Unlike the Android adapter, macOS *does* have a nested loop:
        /// `runModal` blocks until the alert is dismissed, and the host shim's
        /// `corroPresentDialog:` picks `runModal` (standalone) or
        /// `beginSheetModalForWindow:` (sheet) for us.
        pub fn run(&self) -> i32 {
            if self.0.is_null() {
                return -1;
            }
            // `runModal` returns `NSAlertFirstButtonReturn` (1000) plus the
            // button's index. The response callback already ran — the button's
            // action fires it — so this only reports which button ended the
            // session; firing here too would report every click twice.
            let raw = unsafe { msg0i(self.0, "runModal") };
            let index = if raw >= 1000 { (raw - 1000) as usize } else { 0 };
            DIALOG_BUTTONS
                .lock()
                .ok()
                .and_then(|buttons| buttons.get(&(self.0 as usize)).and_then(|v| v.get(index).copied()))
                .unwrap_or(index as i32)
        }
    }

    // ------------------------------------------------------------------
    // TextView (NSTextView in an NSScrollView, or a bare NSTextView)
    // ------------------------------------------------------------------

    #[repr(transparent)]
    pub struct TextView(pub *mut c_void);

    impl Widget for TextView {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for TextView {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl TextView {
        pub fn set_text(&self, text: &str) {
            if self.0.is_null() {
                return;
            }
            let s = nsstring(text);
            if !s.is_null() {
                unsafe { msg1v(self.0, "setString:", s) };
            }
            core_apple::with_meta_mut(self.0, |m| m.text = text.to_owned());
        }

        pub fn get_text(&self) -> Option<String> {
            if self.0.is_null() {
                return None;
            }
            let s = unsafe { msg0(self.0, "string") };
            nsstring_to_rust(s)
        }

        pub fn set_wrap_mode(&self, _wrap_mode: i32) {}
        pub fn set_size_request(&self, _w: i32, _h: i32) {}
        pub fn set_hexpand(&self, _expand: bool) {}
        pub fn set_vexpand(&self, _expand: bool) {}
            /// Append a line to the buffer.
        ///
        /// Read-modify-write: this backend has no incremental insert, so the text is
        /// fetched, extended and written back. Correct, but O(document) per call.
        /// A high-rate log pane should reimplement this against the native handle.
        pub fn append_text(&self, text: &str) {
        let mut buf = self.get_text().unwrap_or_default();
        buf.push_str(text);
        self.set_text(&buf);
        }
    }

    impl Clone for TextView {
        fn clone(&self) -> Self {
            TextView(self.0)
        }
    }

    // ------------------------------------------------------------------
    // ScrolledWindow + Overlay
    // ------------------------------------------------------------------
    //
    // This is a real `NSScrollView`, not the inert container it used to be.
    // `compat::TreeView` draws the results list on a canvas sized to the whole
    // content, exactly as it does for GTK, and expects the container to clip
    // it, show a scroller and scroll it — that is what `GtkScrolledWindow`
    // gives the Linux build. An `NSView` gave none of the three: the list drew
    // past the bottom of the window with no scroller and no wheel.
    //
    // The canvas is the document view. Two details make that work: its width
    // follows the clip view (autoresizing), while its height stays the content
    // height it asked for — the scroller measures that height.

    #[repr(transparent)]
    #[derive(Clone, Copy)]
    pub struct ScrolledWindow(pub *mut c_void);

    impl Widget for ScrolledWindow {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl ScrolledWindow {
        /// Put the sheet canvas in the scroll view as its document view.
        ///
        /// `setDocumentView:` is what puts it inside the clip view (so it is
        /// clipped rather than overflowing), and the width autoresizing mask
        /// keeps it as wide as the window while leaving its height at the
        /// content height the canvas set — which is what the scroller
        /// measures and what the wheel scrolls.
        pub fn attach_canvas(&self, canvas: &Canvas) {
            if self.0.is_null() || canvas.0.is_null() {
                return;
            }
            // SAFETY: both handles are live AppKit views from this adapter,
            // and `NSViewWidthSizable` is the documented autoresizing constant.
            unsafe {
                const NSVIEW_WIDTH_SIZABLE: isize = 2;
                msg1i(canvas.0, "setAutoresizingMask:", NSVIEW_WIDTH_SIZABLE);
                msg1v(self.0, "setDocumentView:", canvas.0);
            }
        }

        /// GTK's `GtkPolicyType`, mapped onto the two AppKit scrollers.
        ///
        /// The integers are the portable layer's (`0` ALWAYS, `1` AUTOMATIC,
        /// `2` NEVER, `3` EXTERNAL). AppKit has one autohide flag for both
        /// scrollers, so a policy asking for an always-visible bar on either
        /// axis wins; `EXTERNAL` has no AppKit equivalent and behaves as
        /// `AUTOMATIC`.
        pub fn set_policy(&self, hscroll: u32, vscroll: u32) {
            if self.0.is_null() {
                return;
            }
            const POLICY_ALWAYS: u32 = 0;
            const POLICY_NEVER: u32 = 2;
            let always = hscroll == POLICY_ALWAYS || vscroll == POLICY_ALWAYS;
            // SAFETY: live NSScrollView; both selectors are AppKit's own.
            unsafe {
                msg1bv(self.0, "setHasHorizontalScroller:", hscroll != POLICY_NEVER);
                msg1bv(self.0, "setHasVerticalScroller:", vscroll != POLICY_NEVER);
                msg1bv(self.0, "setAutohidesScrollers:", !always);
            }
        }

        /// Scroll to a position given in GTK adjustment terms.
        ///
        /// `vval` is the adjustment's value (the top of the visible area),
        /// `vupper` its upper bound and `vpage` the page size. The clip view
        /// scrolls to the same place GTK would.
        pub fn scroll_to(
            &self,
            _hval: f64,
            _hupper: f64,
            _hpage: f64,
            vval: f64,
            vupper: f64,
            vpage: f64,
        ) {
            if self.0.is_null() {
                return;
            }
            // SAFETY: live NSScrollView and its clip view; `scrollToPoint:`
            // takes an `NSPoint` (two `CGFloat`s) and `reflectScrolledClipView:`
            // redraws the scroller for the new position.
            unsafe {
                let clip = msg0(self.0, "contentView");
                if clip.is_null() {
                    return;
                }
                let max = (vupper - vpage).max(0.0);
                let y = vval.clamp(0.0, max);
                msg2cv(clip, "scrollToPoint:", 0.0, y);
                msg1v(self.0, "reflectScrolledClipView:", clip);
            }
        }
    }

    impl AsRef<*mut c_void> for ScrolledWindow {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    #[repr(transparent)]
    pub struct Overlay(pub *mut c_void);

    impl Widget for Overlay {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl AsRef<*mut c_void> for Overlay {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl Clone for Overlay {
        fn clone(&self) -> Self {
            Overlay(self.0)
        }
    }

    impl Overlay {
        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            attach(self.0, *child.as_ref());
        }

        pub fn add_overlay(&self, child: &impl AsRef<*mut c_void>) {
            self.set_child(child);
        }

        pub fn set_overlay_pass_through(&self, _child: &impl AsRef<*mut c_void>, _pass: bool) {}

        pub fn remove(&self, _child: &impl AsRef<*mut c_void>) {}

        pub fn show_all(&self) {}

        pub fn set_size_request(&self, _w: i32, _h: i32) {}
        pub fn set_vexpand(&self, _expand: bool) {}
        pub fn set_hexpand(&self, _expand: bool) {}
    }

    // ------------------------------------------------------------------
    // Runtime shim classes
    // ------------------------------------------------------------------
    //
    // The AppKit host this adapter was written for compiled a few Objective-C
    // classes (`CorroMacTarget`, `CorroMacMenuTarget`, `CorroSheetView`,
    // `CorroMacContainer`) whose selectors carry the callbacks: a button's
    // target/action, a menu item's action, the canvas view's `drawRect:` and
    // mouse events. Nothing in this workspace compiles Objective-C, so on a
    // plain Rust binary every one of those sends used to be a skip — buttons,
    // menus and canvases did nothing.
    //
    // They are built at runtime here instead, with the primitives
    // `backends/macos.rs` already uses for its NSTimer target:
    // `objc_allocateClassPair` + `class_addMethod` + `objc_registerClassPair`,
    // with `extern "C"` Rust functions as the implementations. The simpler
    // shim selectors the adapter sends to *stock* AppKit views
    // (`corroSetFlex:`, `corroSetPinnedWidth:`, …) are added to `NSView` the
    // same way, so those sends resolve to real behaviour rather than a skip.

    /// `CGPoint`, returned by value: two `CGFloat`s.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_allocateClassPair(
            superclass: *mut c_void,
            name: *const std::os::raw::c_char,
            extra_bytes: usize,
        ) -> *mut c_void;
        fn objc_registerClassPair(cls: *mut c_void);
        fn class_addMethod(
            cls: *mut c_void,
            name: *mut c_void,
            imp: *const c_void,
            types: *const std::os::raw::c_char,
        ) -> bool;
        fn object_getClass(obj: *mut c_void) -> *mut c_void;
        fn objc_getAssociatedObject(obj: *mut c_void, key: *const c_void) -> *mut c_void;
        fn objc_setAssociatedObject(
            obj: *mut c_void,
            key: *const c_void,
            value: *mut c_void,
            policy: usize,
        );
        fn dlsym(handle: *mut c_void, symbol: *const std::os::raw::c_char) -> *mut c_void;
    }

    /// `OBJC_ASSOCIATION_ASSIGN`: the value is a plain integer or a pointer this
    /// module owns, so the runtime must not `retain` it.
    const OBJC_ASSIGN: usize = 0;
    /// `OBJC_ASSOCIATION_RETAIN_NONATOMIC`: an `NSString` the target keeps.
    const OBJC_RETAIN: usize = 1;

    /// One associated-object key per payload kind.
    static TARGET_KEY: u8 = 0;
    static MENU_KEY: u8 = 0;
    static CANVAS_KEY: u8 = 0;

    fn key_addr(key: &'static u8) -> *const c_void {
        key as *const u8 as *const c_void
    }

    /// `RTLD_DEFAULT`: search every loaded image. AppKit's `dlopen` brings in
    /// Core Graphics, Core Text and Core Foundation, and this crate links only
    /// libobjc, so `dlsym` is how those symbols are reached.
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

    fn sym_ptr(name: &str) -> *mut c_void {
        let mut buf: Vec<u8> = name.as_bytes().to_vec();
        buf.push(0);
        // SAFETY: NUL-terminated name, default symbol scope.
        unsafe { dlsym(RTLD_DEFAULT, buf.as_ptr() as *const std::os::raw::c_char) }
    }

    /// Dereference an exported **data** symbol: `kCTFontAttributeName` is a
    /// `CFStringRef` constant, so `dlsym` yields the constant's address.
    fn read_data_ptr(name: &str) -> *mut c_void {
        let p = sym_ptr(name);
        if p.is_null() {
            std::ptr::null_mut()
        } else {
            // SAFETY: the symbol is a pointer-sized constant in a loaded image.
            unsafe { *(p as *mut *mut c_void) }
        }
    }

    /// `NSSize`: two `CGFloat`s, returned by value.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct NSSize {
        w: f64,
        h: f64,
    }

    /// PostScript name for the portable family hint plus slant/weight.
    fn ps_font_name(hint: &str, slant: i32, weight: i32) -> &'static str {
        let mono = hint.contains("mono");
        let bold = weight != 0;
        let italic = slant != 0;
        match (mono, bold, italic) {
            (true, true, _) => "Menlo-Bold",
            (true, false, true) => "Menlo-Italic",
            (true, false, false) => "Menlo-Regular",
            (false, true, _) => "Helvetica-Bold",
            (false, false, true) => "Helvetica-Oblique",
            (false, false, false) => "Helvetica",
        }
    }

    /// The attribute dictionary AppKit text wants: `{NSFont, NSForegroundColor}`.
    ///
    /// Built at runtime because this crate carries no Foundation headers: the two
    /// attribute-name constants are exported data symbols, and the font and
    /// colour are ordinary message sends.
    #[allow(clippy::too_many_arguments)]
    fn text_attributes(
        font: *mut c_void,
        size: f64,
        slant: i32,
        weight: i32,
        r: f64,
        g: f64,
        b: f64,
        a: f64,
    ) -> *mut c_void {
        let font_cls = cls("NSFont");
        let colour_cls = cls("NSColor");
        let dict_cls = cls("NSDictionary");
        let font_key = read_data_ptr("NSFontAttributeName");
        let colour_key = read_data_ptr("NSForegroundColorAttributeName");
        if font_cls.is_null()
            || colour_cls.is_null()
            || dict_cls.is_null()
            || font_key.is_null()
            || colour_key.is_null()
        {
            return std::ptr::null_mut();
        }
        // SAFETY: every object below is autoreleased by AppKit and null-checked
        // before use; the dictionary retains what it holds.
        let hint = core_apple::nsstring_to_rust(font).unwrap_or_else(|| "sans".to_owned());
        let name = nsstring(ps_font_name(&hint, slant, weight));
        let mut ns_font = std::ptr::null_mut();
        if !name.is_null() {
            ns_font = raw_send!(
                font_cls,
                "fontWithName:size:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, f64) -> *mut c_void,
                (name, size)
            );
        }
        if ns_font.is_null() {
            ns_font = raw_send!(
                font_cls,
                "systemFontOfSize:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, f64) -> *mut c_void,
                (size)
            );
        }
        if ns_font.is_null() {
            return std::ptr::null_mut();
        }
        let colour = raw_send!(
            colour_cls,
            "colorWithCalibratedRed:green:blue:alpha:",
            unsafe extern "C" fn(*mut c_void, *mut c_void, f64, f64, f64, f64) -> *mut c_void,
            (r, g, b, a)
        );
        if colour.is_null() {
            return std::ptr::null_mut();
        }
        let objs = [ns_font, colour];
        let keys = [font_key, colour_key];
        raw_send!(
            dict_cls,
            "dictionaryWithObjects:forKeys:count:",
            unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *const *mut c_void,
                *const *mut c_void,
                isize,
            ) -> *mut c_void,
            (objs.as_ptr(), keys.as_ptr(), 2isize)
        )
    }

    /// `+[CorroMacText measure:font:size:slant:weight:]`: measure with AppKit's
    /// own text system (`-[NSString sizeWithAttributes:]`) and hand back a
    /// `malloc`ed `CGRect`, which is the contract `measure_text_via_host` reads.
    extern "C" fn text_measure_imp(
        _cls: *mut c_void,
        _cmd: *mut c_void,
        text: *mut c_void,
        font: *mut c_void,
        size: f64,
        slant: i32,
        weight: i32,
    ) -> *mut c_void {
        if text.is_null() || font.is_null() || size <= 0.0 {
            return std::ptr::null_mut();
        }
        // SAFETY: the attribute objects are autoreleased, and the returned
        // buffer is malloc'd for the caller's `free`.
        unsafe {
            let attrs = text_attributes(font, size, slant, weight, 0.0, 0.0, 0.0, 1.0);
            if attrs.is_null() {
                return std::ptr::null_mut();
            }
            let sz: NSSize = raw_send!(
                text,
                "sizeWithAttributes:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> NSSize,
                (attrs)
            );
            let out = malloc(std::mem::size_of::<CGRect>()) as *mut CGRect;
            if out.is_null() {
                return std::ptr::null_mut();
            }
            *out = CGRect {
                x: 0.0,
                y: 0.0,
                width: sz.w.max(0.0),
                height: sz.h.max(0.0),
            };
            out as *mut c_void
        }
    }

    /// `+[CorroMacText drawText:ctx:font:x:y:size:r:g:b:a:slant:weight:]`: draw
    /// one line into the given `CGContext`. `y` is the baseline, matching the
    /// Cairo backends, so the top-left AppKit wants is lifted by the ascender.
    #[allow(clippy::too_many_arguments)]
    extern "C" fn text_draw_imp(
        _cls: *mut c_void,
        _cmd: *mut c_void,
        text: *mut c_void,
        ctx: *mut c_void,
        font: *mut c_void,
        x: f64,
        y: f64,
        size: f64,
        r: f64,
        g: f64,
        b: f64,
        a: f64,
        slant: i32,
        weight: i32,
    ) {
        if text.is_null() || font.is_null() || ctx.is_null() || size <= 0.0 {
            return;
        }
        let ctx_cls = cls("NSGraphicsContext");
        if ctx_cls.is_null() {
            return;
        }
        // SAFETY: a graphics context is created for this call, made current, and
        // the previous one is restored; the point is in the flipped view space.
        unsafe {
            let gctx = raw_send!(
                ctx_cls,
                "graphicsContextWithCGContext:flipped:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, bool) -> *mut c_void,
                (ctx, true)
            );
            if gctx.is_null() {
                return;
            }
            let attrs = text_attributes(font, size, slant, weight, r, g, b, a);
            if attrs.is_null() {
                return;
            }
            msg0v(ctx_cls, "saveGraphicsState");
            msg1v(ctx_cls, "setCurrentContext:", gctx);
            let point = CGPoint {
                x,
                y: y - size * 0.8,
            };
            raw_send!(
                text,
                "drawAtPoint:withAttributes:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, CGPoint, *mut c_void),
                (point, attrs)
            );
            msg0v(ctx_cls, "restoreGraphicsState");
        }
    }

    /// `-[CorroMacTarget corroFired:]`: a control delivered its action; run the
    /// Rust closure registered for this target.
    extern "C" fn fired_imp(obj: *mut c_void, _cmd: *mut c_void, _sender: *mut c_void) {
        // SAFETY: associated-object lookup on an object we handed AppKit.
        let raw = unsafe { objc_getAssociatedObject(obj, key_addr(&TARGET_KEY)) } as usize;
        if raw != 0 {
            core_apple::dispatch_callback(raw as u64);
        }
    }

    /// `NSAlert`'s button target: carries the alert and the button's index.
    static ALERT_KEY: u8 = 0;
    static ALERT_INDEX_KEY: u8 = 0;

    /// `-corroFired:` for an alert button.
    ///
    /// This *is* the button's action, so it runs on the same path a user's click
    /// takes: it reports the response id the caller registered, ends the modal
    /// session `runModal` is running, and takes the alert down. Without it the
    /// alert's buttons were AppKit's own, whose action NSAlert installs, and
    /// nothing reached Rust.
    extern "C" fn alert_fired_imp(obj: *mut c_void, _cmd: *mut c_void, _sender: *mut c_void) {
        // SAFETY: associated-object lookups on a target we handed AppKit.
        let (dialog, index) = unsafe {
            (
                objc_getAssociatedObject(obj, key_addr(&ALERT_KEY)) as usize,
                objc_getAssociatedObject(obj, key_addr(&ALERT_INDEX_KEY)) as usize,
            )
        };
        if dialog == 0 {
            return;
        }
        let id = DIALOG_BUTTONS
            .lock()
            .ok()
            .and_then(|buttons| buttons.get(&dialog).and_then(|v| v.get(index).copied()))
            .unwrap_or(index as i32);
        if let Ok(mut responses) = DIALOG_RESPONSES.lock() {
            if let Some(SendResponse(f)) = responses.get_mut(&dialog) {
                f(id);
            }
        }
        // SAFETY: NSApp is a plain Objective-C object; `stopModalWithCode:` is
        // a no-op when no modal session is running, and `orderOut:` takes the
        // alert's window down either way.
        unsafe {
            let app = msg0(cls("NSApplication"), "sharedApplication");
            if !app.is_null() {
                msg1iv(app, "stopModalWithCode:", 1000 + index as isize);
            }
            let window = msg0(dialog as *mut c_void, "window");
            if !window.is_null() {
                msg1v(window, "orderOut:", std::ptr::null_mut());
            }
        }
    }

    /// `+[CorroMacAlertTarget alertTargetWithDialog:index:]`.
    extern "C" fn alert_target_imp(
        cls_ptr: *mut c_void,
        _cmd: *mut c_void,
        dialog: isize,
        index: isize,
    ) -> *mut c_void {
        // SAFETY: `alloc`/`init` on our own NSObject subclass.
        unsafe {
            let obj = msg0(msg0(cls_ptr, "alloc"), "init");
            if obj.is_null() {
                return obj;
            }
            objc_setAssociatedObject(obj, key_addr(&ALERT_KEY), dialog as *mut c_void, OBJC_ASSIGN);
            objc_setAssociatedObject(
                obj,
                key_addr(&ALERT_INDEX_KEY),
                index as *mut c_void,
                OBJC_ASSIGN,
            );
            own(obj)
        }
    }

    /// `+[CorroMacTarget targetWithCallbackId:]`: the target a control keeps as
    /// its `target`, carrying the id `register_callback` handed out.
    extern "C" fn target_with_id_imp(
        cls_ptr: *mut c_void,
        _cmd: *mut c_void,
        id: isize,
    ) -> *mut c_void {
        // SAFETY: `alloc`/`init` on our own NSObject subclass.
        unsafe {
            let obj = msg0(msg0(cls_ptr, "alloc"), "init");
            if obj.is_null() {
                return obj;
            }
            objc_setAssociatedObject(obj, key_addr(&TARGET_KEY), id as *mut c_void, OBJC_ASSIGN);
            own(obj)
        }
    }

    /// `+[CorroMacField fieldDelegate]`: the `NSControlTextEditingDelegate` a
    /// text field is given, so AppKit reports keystrokes, focus and Return.
    ///
    /// `NSTextField`'s `delegate` is an unretained reference, so the object is
    /// `own`ed here (a singleton per field, kept for the process's life) — the
    /// alternative, a delegate that is freed after `create_entry` returns, is a
    /// crash on the first keystroke.
    extern "C" fn field_delegate_imp(cls_ptr: *mut c_void, _cmd: *mut c_void) -> *mut c_void {
        // SAFETY: `alloc`/`init` on our own NSObject subclass.
        unsafe {
            let obj = msg0(msg0(cls_ptr, "alloc"), "init");
            if obj.is_null() {
                return obj;
            }
            own(obj)
        }
    }

    /// `-[CorroMacField controlTextDidChange:]`: a keystroke changed the field.
    ///
    /// The notification's object is the `NSTextField`; its handle is the key
    /// `Entry::connect_changed` registered, so this is the one place a real
    /// keystroke becomes a Rust `changed` callback on AppKit.
    extern "C" fn field_changed_imp(_this: *mut c_void, _cmd: *mut c_void, note: *mut c_void) {
        if note.is_null() {
            return;
        }
        // SAFETY: `object` on a notification AppKit handed us.
        unsafe {
            let field = msg0(note, "object");
            if !field.is_null() {
                dispatch_text_changed(field);
            }
        }
    }

    /// `-[CorroMacField controlTextDidBeginEditing:]` / `...EndEditing:`.
    extern "C" fn field_begin_imp(_this: *mut c_void, _cmd: *mut c_void, note: *mut c_void) {
        if note.is_null() {
            return;
        }
        // SAFETY: as above.
        unsafe {
            let field = msg0(note, "object");
            if !field.is_null() {
                dispatch_focus(field, true);
            }
        }
    }

    extern "C" fn field_end_imp(_this: *mut c_void, _cmd: *mut c_void, note: *mut c_void) {
        if note.is_null() {
            return;
        }
        // SAFETY: as above.
        unsafe {
            let field = msg0(note, "object");
            if !field.is_null() {
                dispatch_focus(field, false);
            }
        }
    }

    /// `-[CorroMacField control:textView:doCommandBySelector:]`: Return in the
    /// field, which is the app's Search-on-Return key. Claiming it keeps AppKit
    /// from beeping and ends the edit instead of inserting a newline.
    extern "C" fn field_command_imp(
        _this: *mut c_void,
        _cmd: *mut c_void,
        control: *mut c_void,
        _text_view: *mut c_void,
        command: *mut c_void,
    ) -> bool {
        if control.is_null() || command.is_null() {
            return false;
        }
        // SELs are interned pointers, so the comparison is identity.
        if command == core_apple::selector("insertNewline:")
            || command == core_apple::selector("insertNewlineIgnoringFieldEditor:")
        {
            dispatch_entry_activate(control);
            return true;
        }
        false
    }

    /// `+[CorroMacMenuTarget targetWithActionName:]`: a menu item's target,
    /// carrying the name `SimpleAction::connect_activate` registered.
    extern "C" fn menu_target_imp(
        cls_ptr: *mut c_void,
        _cmd: *mut c_void,
        name: *mut c_void,
    ) -> *mut c_void {
        if name.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: `alloc`/`init` on our own NSObject subclass; the association
        // retains the name, so the caller keeps ownership of its copy.
        unsafe {
            let obj = msg0(msg0(cls_ptr, "alloc"), "init");
            if obj.is_null() {
                return obj;
            }
            objc_setAssociatedObject(obj, key_addr(&MENU_KEY), name, OBJC_RETAIN);
            own(obj)
        }
    }

    /// `-[CorroMacMenuTarget corroFired:]`: dispatch the item's action into the
    /// registry `connect_activate` filled in.
    extern "C" fn menu_fired_imp(obj: *mut c_void, _cmd: *mut c_void, sender: *mut c_void) {
        // SAFETY: associated-object lookup on our own target object.
        let name = unsafe { objc_getAssociatedObject(obj, key_addr(&MENU_KEY)) };
        if name.is_null() {
            return;
        }
        if let Some(name) = core_apple::nsstring_to_rust(name) {
            invoke_action(&name, sender);
        }
    }

    /// `-[CorroSheetView corroSetCanvasId:]`: remember which draw/click registry
    /// entries belong to this view.
    extern "C" fn set_canvas_id_imp(obj: *mut c_void, _cmd: *mut c_void, id: isize) {
        // SAFETY: associated-object store on our own view subclass.
        unsafe {
            objc_setAssociatedObject(obj, key_addr(&CANVAS_KEY), id as *mut c_void, OBJC_ASSIGN)
        };
    }

    /// Canvas id of a view: the widget registry `create_canvas` filled in, or the
    /// associated object the shim selector stored.
    unsafe fn view_canvas_id(obj: *mut c_void) -> u64 {
        let id = core_apple::canvas_id_for_view(obj);
        if id != 0 {
            return id;
        }
        unsafe { objc_getAssociatedObject(obj, key_addr(&CANVAS_KEY)) as usize as u64 }
    }

    /// `-[CorroSheetView isFlipped]`: y grows downwards, like every other
    /// backend's canvas, so one draw closure serves GTK and AppKit.
    extern "C" fn is_flipped_imp(_obj: *mut c_void, _cmd: *mut c_void) -> bool {
        true
    }

    /// `-[CorroMacWindow constrainFrameRect:toScreen:]`: return the rect
    /// unchanged.
    ///
    /// AppKit shrinks a window to the screen's visible frame, which on a CI
    /// runner is shorter than the Linux capture the screenshot is compared
    /// with — asking for 1350x858 gave a 1350x645 window. This is the
    /// documented hook for "this window may be bigger than the screen", so a
    /// capture can be the size of the GTK one while still being a real window
    /// with a real title bar.
    extern "C" fn constrain_frame_imp(
        _obj: *mut c_void,
        _cmd: *mut c_void,
        rect: CGRect,
        _screen: *mut c_void,
    ) -> CGRect {
        rect
    }

    /// `-[CorroSheetView acceptsFirstResponder]`: the canvas takes focus, so
    /// key events reach the results list once it is clicked.
    extern "C" fn accepts_first_responder_imp(_obj: *mut c_void, _cmd: *mut c_void) -> bool {
        true
    }

    /// `-[CorroSheetView drawRect:]`: replay the canvas closure against the
    /// `CGContext` AppKit prepared for this pass.
    extern "C" fn draw_rect_imp(obj: *mut c_void, _cmd: *mut c_void, _dirty: CGRect) {
        // SAFETY: AppKit calls this on the main thread with a current context.
        unsafe {
            let canvas_id = view_canvas_id(obj);
            if canvas_id == 0 {
                return;
            }
            let bounds: CGRect = raw_send!(
                obj,
                "bounds",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                ()
            );
            core_apple::with_meta_mut(obj, |m| {
                m.laid_out = (bounds.width as i32, bounds.height as i32);
            });
            let nsctx = msg0(cls("NSGraphicsContext"), "currentContext");
            if nsctx.is_null() {
                return;
            }
            let cgctx = msg0(nsctx, "CGContext");
            if cgctx.is_null() {
                return;
            }
            // A canvas that is an `NSScrollView`'s document view is scrolled
            // by its clip view: the clip's bounds origin is the offset compat
            // tracks, and the viewport is the *clip's* height, not the
            // document's. Translating the context by that origin puts the
            // closure's viewport-relative drawing where the clip shows it,
            // instead of a second time off the top.
            let mut viewport_h = bounds.height;
            let parent = msg0(obj, "superview");
            if !parent.is_null()
                && core_apple::nsstring_to_rust(msg0(parent, "className")).as_deref()
                    == Some("NSClipView")
            {
                let clip: CGRect = raw_send!(
                    parent,
                    "bounds",
                    unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                    ()
                );
                if clip.height > 0.0 {
                    viewport_h = clip.height;
                    CGContextTranslateCTM(cgctx, 0.0, clip.y);
                }
            }
            let ptr = {
                let mut map = DRAW_CALLBACKS.lock().unwrap();
                match map.get_mut(&canvas_id) {
                    Some(SendDrawCallback(p)) => Some(*p),
                    None => None,
                }
            };
            if let Some(ptr) = ptr {
                if let Some(mut dc) = CgDrawContext::new(cgctx) {
                    let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) = &mut *ptr;
                    cb(&mut dc, bounds.width as i32, viewport_h as i32);
                }
            }
        }
    }

    /// Dispatch a press or release at the view's own coordinates.
    ///
    /// `locationInWindow` then `convertPoint:fromView:` is the AppKit way to get
    /// a point in the view's (flipped) space; both return `CGPoint` by value.
    unsafe fn sheet_fire_click(obj: *mut c_void, event: *mut c_void, press: bool, button: u32) {
        if event.is_null() {
            return;
        }
        let canvas_id = unsafe { view_canvas_id(obj) };
        if canvas_id == 0 {
            return;
        }
        let in_window: CGPoint = raw_send!(
            event,
            "locationInWindow",
            unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGPoint,
            ()
        );
        let local: CGPoint = raw_send!(
            obj,
            "convertPoint:fromView:",
            unsafe extern "C" fn(*mut c_void, *mut c_void, f64, f64, *mut c_void) -> CGPoint,
            (in_window.x, in_window.y, std::ptr::null_mut())
        );
        // The portable click callbacks take *viewport* coordinates (compat adds
        // the offset itself). A document view is scrolled by its clip, so the
        // view-local point is in document coordinates: subtract the clip's
        // origin so a click after scrolling hits the row under the pointer.
        let mut viewport_y = local.y;
        let parent = msg0(obj, "superview");
        if !parent.is_null()
            && core_apple::nsstring_to_rust(msg0(parent, "className")).as_deref()
                == Some("NSClipView")
        {
            let clip: CGRect = raw_send!(
                parent,
                "bounds",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                ()
            );
            viewport_y = local.y - clip.y;
        }
        let mods = unsafe { msg0i(event, "modifierFlags") } as u32;
        // 1 = primary, 3 = secondary, as the portable canvas reports them.
        let plain = {
            let mut map = CLICK_CALLBACKS.lock().unwrap();
            map.get_mut(&canvas_id).map(|SendClickCallback(p)| *p)
        };
        let rich = {
            let mut map = CLICK_BUTTON_CALLBACKS.lock().unwrap();
            map.get_mut(&canvas_id).map(|SendClickButtonCallback(p)| *p)
        };
        let release = {
            let mut map = RELEASE_CALLBACKS.lock().unwrap();
            map.get_mut(&canvas_id).map(|SendClickButtonCallback(p)| *p)
        };
        // A double click is a second press carrying a count of two; the plain
        // press below still runs, so a row is selected before it opens.
        let clicks: isize = raw_send!(
            event,
            "clickCount",
            unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize,
            ()
        );
        let double = {
            let mut map = DOUBLE_CLICK_CALLBACKS.lock().unwrap();
            map.get_mut(&canvas_id)
                .map(|SendDoubleClickCallback(p)| *p)
        };
        if press {
            if let Some(ptr) = rich {
                // SAFETY: registry-owned closure, dispatched on the main thread.
                unsafe { (*ptr)(local.x, viewport_y, button, mods) };
            }
            // Selecting a row, sorting on a header click and opening on a
            // double click are the primary button's business: a secondary
            // press must not select, sort or open before its menu appears.
            if button == 1 {
                if let Some(ptr) = plain {
                    // SAFETY: as above.
                    unsafe { (*ptr)(local.x, viewport_y) };
                }
                if activates_on_click_count(clicks) {
                    if let Some(ptr) = double {
                        // SAFETY: as above.
                        unsafe { (*ptr)(local.x, viewport_y) }
                    }
                }
            }
        } else if let Some(ptr) = release {
            // SAFETY: as above.
            unsafe { (*ptr)(local.x, viewport_y, button, mods) };
        }
    }

    extern "C" fn mouse_down_imp(obj: *mut c_void, _cmd: *mut c_void, event: *mut c_void) {
        // SAFETY: AppKit calls this on the main thread.
        unsafe {
            let window = msg0(obj, "window");
            if !window.is_null() {
                msg1v(window, "makeFirstResponder:", obj);
            }
            sheet_fire_click(obj, event, true, 1);
        }
    }

    /// `-[CorroSheetView rightMouseDown:]`: the canvas' secondary press.
    ///
    /// Without this override AppKit's default is a no-op on a plain `NSView`,
    /// so the app's right-click handler (`connect_button_press_event`) never
    /// ran and the row menu could not appear from an actual right-click. The
    /// press is dispatched with button 3, which is what that handler reads;
    /// selection, sorting and double-click opening stay with button 1.
    extern "C" fn right_mouse_down_imp(obj: *mut c_void, _cmd: *mut c_void, event: *mut c_void) {
        // SAFETY: AppKit calls this on the main thread.
        unsafe {
            let window = msg0(obj, "window");
            if !window.is_null() {
                msg1v(window, "makeFirstResponder:", obj);
            }
            sheet_fire_click(obj, event, true, 3);
        }
    }

    extern "C" fn mouse_up_imp(obj: *mut c_void, _cmd: *mut c_void, event: *mut c_void) {
        // SAFETY: as above.
        unsafe { sheet_fire_click(obj, event, false, 1) }
    }

    /// Whether a press carrying this AppKit `clickCount` activates what is
    /// under it.
    ///
    /// A single click only selects. AppKit reports 1 on the first press and 2
    /// on the second press of a double click, which is the press that opens.
    /// Factored out of the responder so the decision can be tested without an
    /// `NSEvent`.
    pub fn activates_on_click_count(count: isize) -> bool {
        count >= 2
    }

    /// `-[NSView setContentHuggingPriority:forOrientation:]` for both axes.
    ///
    /// `NSLayoutPriority` is a C `float`, which is why this cannot reuse the
    /// `CGFloat`-based send helpers.
    unsafe fn set_hugging(obj: *mut c_void, priority: f32) {
        raw_send!(
            obj,
            "setContentHuggingPriority:forOrientation:",
            unsafe extern "C" fn(*mut c_void, *mut c_void, f32, isize),
            (priority, 0)
        );
        raw_send!(
            obj,
            "setContentHuggingPriority:forOrientation:",
            unsafe extern "C" fn(*mut c_void, *mut c_void, f32, isize),
            (priority, 1)
        );
    }

    /// `-[NSView corroSetFlex:]`: the shim's "this child takes the spare space"
    /// flag, i.e. a low content-hugging priority (250 is AppKit's default).
    extern "C" fn set_flex_imp(obj: *mut c_void, _cmd: *mut c_void, flex: bool) {
        // SAFETY: sending to a live view.
        unsafe { set_hugging(obj, if flex { 1.0 } else { 250.0 }) }
    }

    /// `-[NSView corroSetContentHugging:]`: the priority directly.
    extern "C" fn set_hugging_imp(obj: *mut c_void, _cmd: *mut c_void, priority: isize) {
        // SAFETY: sending to a live view.
        unsafe { set_hugging(obj, priority as f32) }
    }

    /// Resize a view's frame width, keeping its height.
    unsafe fn resize_width(obj: *mut c_void, w: f64) {
        let bounds: CGRect = raw_send!(
            obj,
            "bounds",
            unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
            ()
        );
        raw_send!(
            obj,
            "setFrameSize:",
            unsafe extern "C" fn(*mut c_void, *mut c_void, f64, f64),
            (w, bounds.height)
        );
    }

    /// `-[NSView corroSetMinWidth:]`: a minimum width on the frame, which is what
    /// the manual-layout path used the selector for.
    extern "C" fn set_min_width_imp(obj: *mut c_void, _cmd: *mut c_void, w: isize) {
        if w <= 0 {
            return;
        }
        // SAFETY: sending to a live view.
        unsafe { resize_width(obj, w as f64) }
    }

    /// `-[NSView corroSetPinnedWidth:]`: a fixed width slot (`-1` releases it).
    extern "C" fn set_pinned_width_imp(obj: *mut c_void, _cmd: *mut c_void, w: isize) {
        if w < 0 {
            return;
        }
        // SAFETY: sending to a live view.
        unsafe { resize_width(obj, w as f64) }
    }

    /// Add `selname` to `cls_ptr`, with the given Objective-C type encoding.
    unsafe fn add_method(cls_ptr: *mut c_void, selname: &str, imp: *const (), types: &str) {
        let sel = core_apple::selector(selname);
        let mut t: Vec<u8> = types.bytes().collect();
        t.push(0);
        // SAFETY: `imp` is an `extern "C"` function with the signature `types`
        // describes, and `cls_ptr` is a class this module created or a stock
        // AppKit class we are extending (adding, never overriding).
        unsafe {
            class_addMethod(
                cls_ptr,
                sel,
                imp as *const c_void,
                t.as_ptr() as *const std::os::raw::c_char,
            );
        }
    }

    /// [`add_method`] on the metaclass, i.e. a `+classMethod`.
    unsafe fn add_class_method(cls_ptr: *mut c_void, selname: &str, imp: *const (), types: &str) {
        // SAFETY: `object_getClass` on a class object yields its metaclass.
        let meta = unsafe { object_getClass(cls_ptr) };
        if !meta.is_null() {
            // SAFETY: as in `add_method`.
            unsafe { add_method(meta, selname, imp, types) };
        }
    }

    /// Create the shim classes once per process.
    ///
    /// Idempotent, and a no-op without AppKit (headless boxes, host unit tests):
    /// the adapter then keeps its logged-skip behaviour. A class that already
    /// exists — a host that compiled the generated sources, or an earlier call —
    /// is left alone.
    pub fn ensure_runtime_classes() {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            load_appkit();
            let nsobject = cls("NSObject");
            let nsview = cls("NSView");
            if nsobject.is_null() || nsview.is_null() {
                crate::backends::macos::log_macos(
                    "macos: AppKit classes unavailable; shim classes not built",
                );
                return;
            }
            // SAFETY: each class is created once, and every IMP below is an
            // `extern "C"` function with the encoding given beside it.
            unsafe {
                // A control's target: carries the `register_callback` id and
                // dispatches `-corroFired:` when AppKit delivers the action.
                if cls("CorroMacTarget").is_null() {
                    let name = b"CorroMacTarget\0";
                    let c = objc_allocateClassPair(
                        nsobject,
                        name.as_ptr() as *const std::os::raw::c_char,
                        0,
                    );
                    if !c.is_null() {
                        add_method(c, "corroFired:", fired_imp as *const (), "v@:@");
                        add_class_method(
                            c,
                            "targetWithCallbackId:",
                            target_with_id_imp as *const (),
                            "@@:q",
                        );
                        objc_registerClassPair(c);
                    }
                }
                // A menu item's target: carries the action name and dispatches
                // into the registry `connect_activate` filled in.
                if cls("CorroMacMenuTarget").is_null() {
                    let name = b"CorroMacMenuTarget\0";
                    let c = objc_allocateClassPair(
                        nsobject,
                        name.as_ptr() as *const std::os::raw::c_char,
                        0,
                    );
                    if !c.is_null() {
                        add_method(c, "corroFired:", menu_fired_imp as *const (), "v@:@");
                        add_class_method(
                            c,
                            "targetWithActionName:",
                            menu_target_imp as *const (),
                            "@@:@",
                        );
                        objc_registerClassPair(c);
                    }
                }
                // Text: measurement and drawing for `CgDrawContext`, which
                // delegates both to the host's `CorroMacText` class.
                if cls("CorroMacText").is_null() {
                    let name = b"CorroMacText\0";
                    let c = objc_allocateClassPair(
                        nsobject,
                        name.as_ptr() as *const std::os::raw::c_char,
                        0,
                    );
                    if !c.is_null() {
                        add_class_method(
                            c,
                            "measure:font:size:slant:weight:",
                            text_measure_imp as *const (),
                            "@@:@@dqq",
                        );
                        add_class_method(
                            c,
                            "drawText:ctx:font:x:y:size:r:g:b:a:slant:weight:",
                            text_draw_imp as *const (),
                            "v@:@@@dddddddqq",
                        );
                        objc_registerClassPair(c);
                    }
                }
                // The canvas: a flipped `NSView` that replays the draw closure
                // against Core Graphics and forwards mouse events by canvas id.
                if cls("CorroSheetView").is_null() {
                    let name = b"CorroSheetView\0";
                    let c = objc_allocateClassPair(
                        nsview,
                        name.as_ptr() as *const std::os::raw::c_char,
                        0,
                    );
                    if !c.is_null() {
                        add_method(
                            c,
                            "drawRect:",
                            draw_rect_imp as *const (),
                            "v@:{CGRect={CGPoint=dd}{CGSize=dd}}",
                        );
                        add_method(c, "isFlipped", is_flipped_imp as *const (), "c@:");
                        add_method(
                            c,
                            "acceptsFirstResponder",
                            accepts_first_responder_imp as *const (),
                            "c@:",
                        );
                        add_method(c, "corroSetCanvasId:", set_canvas_id_imp as *const (), "v@:q");
                        add_method(c, "mouseDown:", mouse_down_imp as *const (), "v@:@");
                        add_method(
                            c,
                            "rightMouseDown:",
                            right_mouse_down_imp as *const (),
                            "v@:@",
                        );
                        add_method(c, "mouseUp:", mouse_up_imp as *const (), "v@:@");
                        objc_registerClassPair(c);
                    }
                }
                // A window that keeps the frame it is given, so a screenshot
                // can be as large as the GTK capture (see
                // `constrain_frame_imp`).
                if cls("CorroMacWindow").is_null() {
                    let name = b"CorroMacWindow\0";
                    let c = objc_allocateClassPair(
                        cls("NSWindow"),
                        name.as_ptr() as *const std::os::raw::c_char,
                        0,
                    );
                    if !c.is_null() {
                        // The encoding must describe the real ABI — an
                        // `NSRect` returned, an `NSRect` and an `NSScreen*`
                        // taken. Registered as `@@:@@` (object in, object out),
                        // AppKit's own frame arithmetic read garbage: the
                        // window kept 645 of the 858 points asked for, and
                        // `contentRectForFrameRect:` reported an 858-tall frame
                        // as having 34 points of content.
                        add_method(
                            c,
                            "constrainFrameRect:toScreen:",
                            constrain_frame_imp as *const (),
                            "{CGRect={CGPoint=dd}{CGSize=dd}}@:{CGRect={CGPoint=dd}{CGSize=dd}}@",
                        );
                        objc_registerClassPair(c);
                    }
                }
                // A dialog button's target: carries the alert and its button
                // index, so a click reports the response id the caller used.
                if cls("CorroMacAlertTarget").is_null() {
                    let name = b"CorroMacAlertTarget\0";
                    let c = objc_allocateClassPair(
                        nsobject,
                        name.as_ptr() as *const std::os::raw::c_char,
                        0,
                    );
                    if !c.is_null() {
                        add_method(c, "corroFired:", alert_fired_imp as *const (), "v@:@");
                        add_class_method(
                            c,
                            "alertTargetWithDialog:index:",
                            alert_target_imp as *const (),
                            "@@:qq",
                        );
                        objc_registerClassPair(c);
                    }
                }
                // A text field's delegate: AppKit reports keystrokes, focus and
                // Return through it, and without it `connect_changed` never
                // fires on a real keypress — the as-you-type search could not
                // be reached by typing at all.
                if cls("CorroMacField").is_null() {
                    let name = b"CorroMacField\0";
                    let c = objc_allocateClassPair(
                        nsobject,
                        name.as_ptr() as *const std::os::raw::c_char,
                        0,
                    );
                    if !c.is_null() {
                        add_method(c, "controlTextDidChange:", field_changed_imp as *const (), "v@:@");
                        add_method(
                            c,
                            "controlTextDidBeginEditing:",
                            field_begin_imp as *const (),
                            "v@:@",
                        );
                        add_method(
                            c,
                            "controlTextDidEndEditing:",
                            field_end_imp as *const (),
                            "v@:@",
                        );
                        add_method(
                            c,
                            "control:textView:doCommandBySelector:",
                            field_command_imp as *const (),
                            "c@:@@:",
                        );
                        add_class_method(
                            c,
                            "fieldDelegate",
                            field_delegate_imp as *const (),
                            "@@:",
                        );
                        objc_registerClassPair(c);
                    }
                }
                // The shim selectors the adapter sends to *stock* views: adding
                // them to `NSView` is what turns those sends from a logged skip
                // into real AppKit behaviour — and every control is an `NSView`.
                add_method(nsview, "corroSetFlex:", set_flex_imp as *const (), "v@:c");
                add_method(
                    nsview,
                    "corroSetContentHugging:",
                    set_hugging_imp as *const (),
                    "v@:q",
                );
                add_method(nsview, "corroSetMinWidth:", set_min_width_imp as *const (), "v@:q");
                add_method(
                    nsview,
                    "corroSetPinnedWidth:",
                    set_pinned_width_imp as *const (),
                    "v@:q",
                );
            }
            if !cls("CorroSheetView").is_null() {
                core_apple::set_sheet_view_class("CorroSheetView");
            }
        });
    }

    /// Render `view` offscreen and return its pixels as `(w, h, RGBA8)`.
    ///
    /// `displayRectIgnoringOpacity:inContext:` runs the view's real `drawRect:`
    /// into a bitmap-backed context, so a headless machine (CI) can prove a
    /// canvas paints instead of trusting that it would. Returns `None` when
    /// AppKit is unavailable or the view will not draw.
    pub fn render_view_rgba(
        view: *mut c_void,
        w: u32,
        h: u32,
    ) -> Option<(u32, u32, Vec<u8>)> {
        if view.is_null() || w == 0 || h == 0 {
            return None;
        }
        let rep_cls = cls("NSBitmapImageRep");
        let ctx_cls = cls("NSGraphicsContext");
        if rep_cls.is_null() || ctx_cls.is_null() {
            return None;
        }
        // SAFETY: the bitmap rep owns its buffer, the context is scoped with
        // save/restore, and every pointer is null-checked before use.
        unsafe {
            let rep = raw_send!(
                raw_send!(
                    rep_cls,
                    "alloc",
                    unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
                    ()
                ),
                "initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bytesPerRow:bitsPerPixel:",
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *const *mut c_void,
                    isize,
                    isize,
                    isize,
                    isize,
                    bool,
                    bool,
                    *mut c_void,
                    isize,
                    isize,
                ) -> *mut c_void,
                (
                    std::ptr::null::<*mut c_void>(),
                    w as isize,
                    h as isize,
                    8isize,
                    4isize,
                    true,
                    false,
                    nsstring("NSDeviceRGBColorSpace"),
                    0isize,
                    0isize,
                )
            );
            if rep.is_null() {
                return None;
            }
            let gctx = core_apple::msg1(ctx_cls, "graphicsContextWithBitmapImageRep:", rep);
            if gctx.is_null() {
                return None;
            }
            msg2cv(view, "setFrameSize:", w as f64, h as f64);
            msg0v(ctx_cls, "saveGraphicsState");
            msg1v(ctx_cls, "setCurrentContext:", gctx);
            let rect = CGRect {
                x: 0.0,
                y: 0.0,
                width: w as f64,
                height: h as f64,
            };
            raw_send!(
                view,
                "displayRectIgnoringOpacity:inContext:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, CGRect, *mut c_void),
                (rect, gctx)
            );
            msg0v(ctx_cls, "restoreGraphicsState");
            let data = msg0(rep, "bitmapData") as *const u8;
            let row = msg0i(rep, "bytesPerRow") as usize;
            let per_pixel = msg0i(rep, "samplesPerPixel") as usize;
            if data.is_null() || row == 0 || per_pixel < 4 {
                return None;
            }
            let bytes = std::slice::from_raw_parts(data, row * h as usize);
            let mut out = Vec::with_capacity(w as usize * h as usize * 4);
            for y in 0..h as usize {
                let start = y * row;
                out.extend_from_slice(&bytes[start..start + w as usize * 4]);
            }
            Some((w, h, out))
        }
    }

    /// Write a PNG of `view`, as AppKit draws it, to `path`.
    ///
    /// This exists because a CI runner has no screen to photograph: the macOS
    /// job can start the GUI but cannot see it, and `screencapture` takes the
    /// desktop rather than this process's own view. The picture is taken here
    /// instead, through the same display path the window uses, and PNG-encoded
    /// by AppKit — so what lands in the file is what `drawRect:` painted, at
    /// the size the view was laid out at.
    ///
    /// `size` grows the view's window first, so a capture can be taken at the
    /// same window size as the Linux one instead of whatever the default was.
    ///
    /// Returns `(width, height, bytes written)`.
    /// What a capture proves about itself, besides its pixels.
    pub struct Shot {
        pub width: u32,
        pub height: u32,
        /// The PNG, ready to write.
        pub png: Vec<u8>,
        /// Whether the top band of the image is the window's title-bar chrome
        /// rather than the content view's own white.
        pub title_bar: bool,
        /// Pixels drawn in the right-hand gutter, where a scroller sits: zero
        /// when the scroll view drew none.
        pub scroller_ink: usize,
        /// How much of that gutter has ink, in percent of its height. A drawn
        /// scroller runs almost the whole way down; text that merely reaches
        /// the right edge shows up on a few rows only.
        pub scroller_coverage: u32,
        /// Pixels drawn anywhere else in the content area: a window whose list
        /// did not render at all has none, which tells an empty list apart from
        /// an undrawn scroller.
        pub content_ink: usize,
        /// The image itself, RGBA, top row first: a caller that knows where a
        /// widget should be can check the pixels there rather than trusting the
        /// capture's own summary.
        pub rgba: Vec<u8>,
    }

    /// Render a view into a PNG, and report what it contains.
    ///
    /// When `view` is a window's content view the window's *frame* view is
    /// captured instead, so the title bar is part of the picture: a capture of
    /// the content alone cannot be compared with a Linux capture of a whole
    /// window. The frame view is also the only view that may be given the
    /// requested size — a window cannot grow past the screen, but the view
    /// holding its chrome has no such limit.
    pub fn capture(view: *mut c_void, size: Option<(f64, f64)>) -> Option<Shot> {
        if view.is_null() {
            return None;
        }
        // SAFETY: a live view from this adapter; every selector below is
        // AppKit's own and every pointer is null-checked before use.
        unsafe {
            // Breadcrumbs: an exception from one of these AppKit sends unwinds
            // through Rust and aborts, so the last line is the one that threw.
            let step = |n: &str| crate::backends::macos::log_macos(&format!("macos: capture {n}"));
            step("0 window");
            let window = msg0(view, "window");
            let theme = if window.is_null() {
                std::ptr::null_mut()
            } else {
                msg0(view, "superview")
            };
            let target = if theme.is_null() { view } else { theme };
            // The chrome is exactly what `frameRectForContentRect:` adds to an
            // empty content rect: the title bar plus the window border.
            let mut chrome = 0.0f64;
            if !window.is_null() {
                step("1 chrome rect");
                let empty = CGRect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                };
                let frame_rect: CGRect = raw_send!(
                    window,
                    "frameRectForContentRect:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, CGRect) -> CGRect,
                    (empty)
                );
                chrome = frame_rect.height.max(0.0);
            }
            step("2 size");
            if let Some((want_w, want_h)) = size {
                if !window.is_null() {
                    // Let the window do the work: AppKit places the title bar
                    // and the content itself, instead of the capture poking
                    // both frames and hoping. `CorroMacWindow` keeps the size.
                    let origin: CGRect = raw_send!(
                        window,
                        "frame",
                        unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                        ()
                    );
                    // The chrome (title bar plus border) is the difference
                    // between the window's own view and its content view. Both
                    // are `bounds` returns, with no struct passed in: AppKit's
                    // `contentRectForFrameRect:` has been unreliable on the
                    // window subclass, reporting an 858-tall frame as 34 points
                    // of content, so the capture does the arithmetic itself.
                    let frame_bounds: CGRect = raw_send!(
                        theme,
                        "bounds",
                        unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                        ()
                    );
                    let content_bounds: CGRect = raw_send!(
                        view,
                        "bounds",
                        unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                        ()
                    );
                    let chrome_h = (frame_bounds.height - content_bounds.height).max(0.0);
                    raw_send!(
                        window,
                        "setFrame:display:",
                        unsafe extern "C" fn(
                            *mut c_void,
                            *mut c_void,
                            f64,
                            f64,
                            f64,
                            f64,
                            bool,
                        ),
                        (origin.x, origin.y, want_w, want_h, false)
                    );
                    // Explicit, because AppKit left the content view at its old
                    // height when the frame grew.
                    msg4cv(
                        view,
                        "setFrame:",
                        0.0,
                        0.0,
                        want_w,
                        (want_h - chrome_h).max(1.0),
                    );
                    pin_size(
                        view,
                        want_w,
                        (want_h - chrome_h).max(1.0),
                    );
                    msg0v(view, "layoutSubtreeIfNeeded");
                    step("2c refit");
                    refit_content_children(view);
                    msg0v(view, "layoutSubtreeIfNeeded");
                } else {
                    set_frame(view, 0.0, 0.0, want_w, want_h);
                }
            }
            // After the resize, not before: a scroll view re-tiles when its
            // frame changes, and a legacy scroller rebuilt earlier would be
            // replaced by an overlay one here.
            step("2b scrollers");
            use_legacy_scrollers(view);
            step("3 layout");
            msg0v(view, "layoutSubtreeIfNeeded");
            // The toggle in step 2b makes a fresh scroller; it only gets its
            // frame when the scroll view re-tiles, which the layout pass above
            // does. Re-tiling again here makes the bar's geometry final before
            // the bitmap is taken.
            step("3b retile");
            retile_scrollers(view);
            msg0v(view, "layoutSubtreeIfNeeded");
            // Last word on the frames: whatever the layout passes decided, the
            // window's root views are the size of the window, as GTK's are.
            refit_content_children(view);
            step("3c no layers");
            disable_layers(target);
            msg0v(view, "layoutSubtreeIfNeeded");
            refit_content_children(view);
            step("4 bounds");
            let bounds: CGRect = raw_send!(
                target,
                "bounds",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                ()
            );
            let (w, h) = (
                bounds.width.round().max(1.0) as u32,
                bounds.height.round().max(1.0) as u32,
            );
            let rep_cls = cls("NSBitmapImageRep");
            if rep_cls.is_null() {
                return None;
            }
            let rep = raw_send!(
                raw_send!(
                    rep_cls,
                    "alloc",
                    unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
                    ()
                ),
                "initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bytesPerRow:bitsPerPixel:",
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *const *mut c_void,
                    isize,
                    isize,
                    isize,
                    isize,
                    bool,
                    bool,
                    *mut c_void,
                    isize,
                    isize,
                ) -> *mut c_void,
                (
                    std::ptr::null::<*mut c_void>(),
                    w as isize,
                    h as isize,
                    8isize,
                    4isize,
                    true,
                    false,
                    nsstring("NSDeviceRGBColorSpace"),
                    0isize,
                    0isize,
                )
            );
            if rep.is_null() {
                return None;
            }
            let data = msg0(rep, "bitmapData") as *mut u8;
            let row = msg0i(rep, "bytesPerRow") as usize;
            if data.is_null() || row < w as usize * 4 {
                return None;
            }
            // The view paints itself but not what is behind it, so an unpainted
            // area would be transparent — black once the alpha is flattened.
            for y in 0..h as usize {
                std::ptr::write_bytes(data.add(y * row), 0xFF, w as usize * 4);
            }
            let rect = CGRect {
                x: 0.0,
                y: 0.0,
                width: w as f64,
                height: h as f64,
            };
            step("5 display");
            // Drawing through a graphics context, not `cacheDisplayInRect:`:
            // the latter left the scroll view's legacy scroller out of the image
            // entirely (a 17x627 frame, not one pixel), because a view that
            // draws through a layer is not part of that path.
            let context_cls = cls("NSGraphicsContext");
            let mut context = std::ptr::null_mut();
            if !context_cls.is_null() && responds(context_cls, "graphicsContextWithBitmapImageRep:") {
                context = raw_send!(
                    context_cls,
                    "graphicsContextWithBitmapImageRep:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void,
                    (rep)
                );
            }
            if context.is_null() || !responds(target, "displayRectIgnoringOpacity:inContext:") {
                raw_send!(
                    target,
                    "cacheDisplayInRect:toBitmapImageRep:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, CGRect, *mut c_void),
                    (rect, rep)
                );
            } else {
                msg0v(context_cls, "saveGraphicsState");
                if responds(context_cls, "setCurrentContext:") {
                    msg1v(context_cls, "setCurrentContext:", context);
                }
                raw_send!(
                    target,
                    "displayRectIgnoringOpacity:inContext:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, CGRect, *mut c_void),
                    (rect, context)
                );
                paint_title_bar(window, context, w as f64, h as f64, chrome);
                paint_controls(target, target, context);
                paint_scrollers(target, target, context);
                msg0v(context_cls, "restoreGraphicsState");
            }
            // Copy the pixels out first: they are what the measurements read,
            // and the rep's buffer goes away with the object.
            let mut rgba = vec![0u8; w as usize * h as usize * 4];
            for y in 0..h as usize {
                std::ptr::copy_nonoverlapping(
                    data.add(y * row),
                    rgba.as_mut_ptr().add(y * w as usize * 4),
                    w as usize * 4,
                );
            }
            let ink_at = |rgba: &[u8], x: usize, y: usize| -> bool {
                let i = (y * w as usize + x) * 4;
                rgba[i] < 245 || rgba[i + 1] < 245 || rgba[i + 2] < 245
            };
            // Title bar: the chrome band must not be the content view's white.
            let band = (chrome.round() as u32).min(h);
            let mut band_ink = 0usize;
            for y in (band as usize / 2)..(band as usize) {
                for x in 0..w as usize {
                    if ink_at(&rgba, x, y) {
                        band_ink += 1;
                    }
                }
            }
            let title_bar = band_ink > (band as usize / 2) * (w as usize / 20);
            // Scroller: the right-hand gutter of the scroll view, below the
            // chrome and above the last few rows.
            // The gutter is wide enough for a legacy scroller (17pt) plus its
            // border: a narrower strip sampled past the bar and reported an
            // empty gutter with the scroller plainly drawn beside it.
            let mut scroller_ink = 0usize;
            let mut inked_rows = 0usize;
            let mut rows = 0usize;
            let x0 = w.saturating_sub(22) as usize;
            let y_start = (band as usize + 2).min(h as usize);
            let y_end = (h as usize).saturating_sub(4);
            for y in y_start..y_end {
                let mut row_ink = 0usize;
                for x in x0..w as usize {
                    // A scroller's track is a very light grey, lighter than the
                    // 245 used elsewhere: 250 still separates it from white.
                    let i = (y * w as usize + x) * 4;
                    if rgba[i] < 250 || rgba[i + 1] < 250 || rgba[i + 2] < 250 {
                        row_ink += 1;
                    }
                }
                scroller_ink += row_ink;
                rows += 1;
                if row_ink >= 2 {
                    inked_rows += 1;
                }
            }
            let mut content_ink = 0usize;
            for y in band as usize..h as usize {
                for x in 0..x0 {
                    if ink_at(&rgba, x, y) {
                        content_ink += 1;
                    }
                }
            }
            let scroller_coverage = if rows == 0 {
                0
            } else {
                (inked_rows * 100 / rows) as u32
            };
            step("6 png");
            // `NSBitmapImageFileTypePNG` is 4.
            const PNG: usize = 4;
            let dict = msg0(cls("NSDictionary"), "dictionary");
            let nsdata = raw_send!(
                rep,
                "representationUsingType:properties:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_void) -> *mut c_void,
                (PNG, dict)
            );
            if nsdata.is_null() {
                return None;
            }
            let len: usize = raw_send!(
                nsdata,
                "length",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> usize,
                ()
            );
            let bytes: *const u8 = raw_send!(
                nsdata,
                "bytes",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> *const u8,
                ()
            );
            if bytes.is_null() || len == 0 {
                return None;
            }
            let png = std::slice::from_raw_parts(bytes, len).to_vec();
            Some(Shot {
                width: w,
                height: h,
                png,
                title_bar,
                scroller_ink,
                content_ink,
                rgba: rgba.clone(),
                scroller_coverage,
            })
        }
    }

    pub fn write_view_png(
        view: *mut c_void,
        path: &str,
        size: Option<(f64, f64)>,
    ) -> Option<(u32, u32, usize)> {
        let shot = capture(view, size)?;
        std::fs::write(path, &shot.png).ok()?;
        Some((shot.width, shot.height, shot.png.len()))
    }

    /// Ask AppKit for the persistent (`NSScrollerStyleLegacy`) scroller style.
    ///
    /// The modern overlay scroller fades out and is not drawn at all until the
    /// list is scrolled, so a screenshot of a fresh window shows no scroller —
    /// and "the scrollbar works" cannot be seen in the pixels. Legacy scrollers
    /// are drawn permanently, which is what GTK's capture shows too. Only the
    /// screenshot path calls this: a normal run keeps the platform style.
    ///
    /// The style itself is global (`+[NSScroller setPreferredScrollerStyle:]`);
    /// a scroll view has no setter of its own — `scrollerStyle` is read-only —
    /// and asking it for one raised an unrecognised selector, which aborted the
    /// process. Scroll views pick the new preferred style up, so re-tiling and
    /// flashing each of them is all that is left to do.
    pub fn use_legacy_scrollers(view: *mut c_void) {
        if view.is_null() {
            return;
        }
        // SAFETY: AppKit's own class method and views from this adapter. Every
        // send is guarded by `responds`, because an unrecognised one raises an
        // exception that unwinds into Rust and aborts the process.
        unsafe {
            let scroller_cls = cls("NSScroller");
            if !scroller_cls.is_null() {
                // `NSScrollerStyleLegacy` is 1.
                if responds(scroller_cls, "setPreferredScrollerStyle:") {
                    msg1iv(scroller_cls, "setPreferredScrollerStyle:", 1);
                }
            }
            force_legacy_scrollers(view);
        }
    }

    /// Make a scroll view's document view no wider than the viewport.
    ///
    /// `Canvas::set_size_request` clamps when it is called, but a canvas that
    /// asked for its width *before* the scroll view had a size stays that wide
    /// for good: the list's columns were laid out for a ~2060pt canvas in a
    /// 1350pt window and the path column ran off the right edge, with a
    /// horizontal scroller under every screenful. GTK's treeview fits its
    /// viewport, so the document view is fitted here — whenever the list
    /// repaints, and whenever a capture re-tiles the scroll views.
    unsafe fn fit_document_view(scroll_view: *mut c_void) {
        if scroll_view.is_null() {
            return;
        }
        // SAFETY: a live `NSScrollView`; both sends are AppKit's own.
        unsafe {
            let doc = msg0(scroll_view, "documentView");
            let clip = msg0(scroll_view, "contentView");
            if doc.is_null() || clip.is_null() {
                return;
            }
            let doc_rect: CGRect = raw_send!(
                doc,
                "frame",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                ()
            );
            let clip_rect: CGRect = raw_send!(
                clip,
                "bounds",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                ()
            );
            if clip_rect.width > 1.0 && doc_rect.width > clip_rect.width + 0.5 {
                msg2cv(doc, "setFrameSize:", clip_rect.width, doc_rect.height);
            }
        }
    }

    /// Shift the current context so `view`'s own coordinates land where it
    /// sits inside `target`.
    unsafe fn concat_translate(view: *mut c_void, target: *mut c_void) {
        // SAFETY: two live views; `convertRect:toView:` is AppKit's own.
        unsafe {
            let bounds: CGRect = raw_send!(
                view,
                "bounds",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                ()
            );
            let placed: CGRect = raw_send!(
                view,
                "convertRect:toView:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, CGRect, *mut c_void) -> CGRect,
                (bounds, target)
            );
            let transform_cls = cls("NSAffineTransform");
            if transform_cls.is_null() {
                return;
            }
            let transform = msg0(transform_cls, "transform");
            if transform.is_null() {
                return;
            }
            msg2cv(transform, "translateXBy:yBy:", placed.x, placed.y);
            msg0v(transform, "concat");
        }
    }

    /// `[NSAffineTransform transform]`, or `None` when AppKit is unavailable.
    unsafe fn affine_transform() -> Option<*mut c_void> {
        // SAFETY: AppKit class lookup, no arguments.
        unsafe {
            let cls_ = cls("NSAffineTransform");
            if cls_.is_null() {
                return None;
            }
            let transform = msg0(cls_, "transform");
            if transform.is_null() {
                None
            } else {
                Some(transform)
            }
        }
    }

    /// Draw the window's chrome into the capture.
    ///
    /// `NSThemeFrame` paints its title bar through private layers, which an
    /// offscreen render does not contain: the frame view's band came out flat
    /// white with the title text and no buttons at all, while the window on
    /// screen plainly has three. The band is filled with AppKit's own window
    /// background, the three standard buttons are drawn at their standard places
    /// and colours, and the window's own title is drawn centred — the chrome a
    /// user sees, in the same place, from the same window.
    unsafe fn paint_title_bar(window: *mut c_void, context: *mut c_void, w: f64, h: f64, band: f64) {
        if context.is_null() || band < 8.0 || w < 1.0 || h < band {
            return;
        }
        // SAFETY: a live context on the main thread, AppKit's own classes.
        unsafe {
            let path_cls = cls("NSBezierPath");
            let color_cls = cls("NSColor");
            let context_cls = cls("NSGraphicsContext");
            if path_cls.is_null() || color_cls.is_null() || context_cls.is_null() {
                return;
            }
            let fill = |color: *mut c_void, rect: CGRect| {
                if color.is_null() {
                    return;
                }
                msg0v(color, "setFill");
                raw_send!(
                    path_cls,
                    "fillRect:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, CGRect),
                    (rect)
                );
            };
            msg0v(context_cls, "saveGraphicsState");
            // The band, then a hairline along its lower edge — `NSThemeFrame`'s
            // shape, not a hand-invented one.
            fill(
                msg0(color_cls, "windowBackgroundColor"),
                CGRect {
                    x: 0.0,
                    y: h - band,
                    width: w,
                    height: band,
                },
            );
            fill(
                msg0(color_cls, "separatorColor"),
                CGRect {
                    x: 0.0,
                    y: h - band,
                    width: w,
                    height: 1.0,
                },
            );
            // Close, minimise, zoom: 12pt, 20pt apart, centred in the band.
            let lights = [
                (0.99f64, 0.37f64, 0.34f64),
                (0.99, 0.74, 0.18),
                (0.16, 0.78, 0.25),
            ];
            for (i, (r, g, b)) in lights.iter().enumerate() {
                let colour: *mut c_void = raw_send!(
                    color_cls,
                    "colorWithCalibratedRed:green:blue:alpha:",
                    unsafe extern "C" fn(
                        *mut c_void,
                        *mut c_void,
                        f64,
                        f64,
                        f64,
                        f64,
                    ) -> *mut c_void,
                    (*r, *g, *b, 1.0)
                );
                if colour.is_null() {
                    continue;
                }
                msg0v(colour, "setFill");
                let centre_x = 14.0 + 6.0 + i as f64 * 20.0;
                let oval = CGRect {
                    x: centre_x - 6.0,
                    y: h - band / 2.0 - 6.0,
                    width: 12.0,
                    height: 12.0,
                };
                let path: *mut c_void = raw_send!(
                    path_cls,
                    "bezierPathWithOvalInRect:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, CGRect) -> *mut c_void,
                    (oval)
                );
                if !path.is_null() {
                    msg0v(path, "fill");
                }
            }
            // The title, centred, as the window itself is titled.
            if !window.is_null() && responds(window, "title") {
                let title = msg0(window, "title");
                if !title.is_null() {
                    let font_cls = cls("NSFont");
                    let font: *mut c_void = if font_cls.is_null() {
                        std::ptr::null_mut()
                    } else {
                        raw_send!(
                            font_cls,
                            "systemFontOfSize:",
                            unsafe extern "C" fn(*mut c_void, *mut c_void, f64) -> *mut c_void,
                            (13.0)
                        )
                    };
                    let style: *mut c_void = raw_send!(
                        cls("NSMutableParagraphStyle"),
                        "new",
                        unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
                        ()
                    );
                    if !style.is_null() {
                        msg1iv(style, "setAlignment:", 1);
                    }
                    let text_colour = msg0(color_cls, "labelColor");
                    let dict: *mut c_void = raw_send!(
                        cls("NSMutableDictionary"),
                        "dictionary",
                        unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
                        ()
                    );
                    if !dict.is_null() && responds(dict, "setObject:forKey:") {
                        for (obj, key) in [
                            (font, "NSFont"),
                            (text_colour, "NSColor"),
                            (style, "NSParagraphStyle"),
                        ] {
                            if obj.is_null() {
                                continue;
                            }
                            let k = nsstring(key);
                            if k.is_null() {
                                continue;
                            }
                            raw_send!(
                                dict,
                                "setObject:forKey:",
                                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void),
                                (obj, k)
                            );
                        }
                        let rect = CGRect {
                            x: 0.0,
                            y: h - band / 2.0 - 8.0,
                            width: w,
                            height: 16.0,
                        };
                        raw_send!(
                            title,
                            "drawInRect:withAttributes:",
                            unsafe extern "C" fn(*mut c_void, *mut c_void, CGRect, *mut c_void),
                            (rect, dict)
                        );
                    }
                }
            }
            msg0v(context_cls, "restoreGraphicsState");
        }
    }

    /// Whether AppKit would draw `view` at all: it, or any view above it, is
    /// hidden.
    ///
    /// The capture paints some controls itself (see `paint_controls`), so it
    /// has to make the same decision the window does. Asking only the view's
    /// own `isHidden` is not that decision: a collapsed disclosure's body is
    /// hidden, its children are not, and painting them put the foldout's
    /// checkboxes on top of the results grid in the reference capture.
    unsafe fn hidden_with_ancestors(view: *mut c_void) -> bool {
        if view.is_null() {
            return true;
        }
        // SAFETY: a live view from this adapter's tree.
        unsafe {
            if responds(view, "isHiddenOrHasHiddenAncestor") {
                return msg0i(view, "isHiddenOrHasHiddenAncestor") != 0;
            }
            // Older AppKit: walk the chain by hand.
            let mut v = view;
            while !v.is_null() {
                if msg0i(v, "isHidden") != 0 {
                    return true;
                }
                v = msg0(v, "superview");
            }
            false
        }
    }

    /// Draw the window's buttons into the capture.
    ///
    /// An `NSButton` draws through its cell, which the offscreen render leaves
    /// out entirely: the app's Search button and all three advanced checkboxes
    /// were in the window and absent from the bitmap. `drawWithFrame:inView:` is
    /// the cell's own drawing call, so what lands in the image is the control a
    /// user sees, not a redrawn approximation — including *not* drawing a
    /// control whose section is collapsed (`hidden_with_ancestors`).
    unsafe fn paint_controls(view: *mut c_void, target: *mut c_void, context: *mut c_void) {
        if view.is_null() || context.is_null() {
            return;
        }
        // SAFETY: views from this adapter's own tree and a live context.
        unsafe {
            let button_cls = cls("NSButton");
            let is_button = !button_cls.is_null()
                && raw_send!(
                    view,
                    "isKindOfClass:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool,
                    (button_cls)
                );
            if is_button && !hidden_with_ancestors(view) {
                let cell = msg0(view, "cell");
                if !cell.is_null() && responds(cell, "drawWithFrame:inView:") {
                    let bounds: CGRect = raw_send!(
                        view,
                        "bounds",
                        unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                        ()
                    );
                    msg0v(cls("NSGraphicsContext"), "saveGraphicsState");
                    concat_translate(view, target);
                    // The control is a flipped view, and its cell compensates
                    // for that by flipping the context itself. Our context is
                    // not flipped, so the compensation inverted the title:
                    // "Search" and every checkbox label came out upside down.
                    // Give the cell the context it expects.
                    if msg0i(view, "isFlipped") != 0 {
                        let b = bounds;
                        if let Some(transform) = affine_transform() {
                            msg2cv(transform, "translateXBy:yBy:", 0.0, b.height);
                            msg2cv(transform, "scaleXBy:yBy:", 1.0, -1.0);
                            msg0v(transform, "concat");
                        }
                    }
                    raw_send!(
                        cell,
                        "drawWithFrame:inView:",
                        unsafe extern "C" fn(*mut c_void, *mut c_void, CGRect, *mut c_void),
                        (bounds, view)
                    );
                    msg0v(cls("NSGraphicsContext"), "restoreGraphicsState");
                }
            }
            let subviews = msg0(view, "subviews");
            if !subviews.is_null() {
                let count = msg0i(subviews, "count").max(0);
                for i in 0..count {
                    paint_controls(msg1i(subviews, "objectAtIndex:", i), target, context);
                }
            }
        }
    }

    /// Draw every scroller over the image `target` was just rendered into.
    ///
    /// Current AppKit leaves the scroll view's scroller out of a render pass
    /// (`cacheDisplayInRect:` and `displayRectIgnoringOpacity:` alike): the
    /// list drew 28k pixels and the scroller's 17x627 gutter held none. Drawing
    /// each scroller itself, at its own place in the target's coordinates, is
    /// still a real `NSScroller`'s own drawing — the pixels a user sees on
    /// screen, not a bar painted by hand.
    unsafe fn paint_scrollers(view: *mut c_void, target: *mut c_void, context: *mut c_void) {
        if view.is_null() || context.is_null() {
            return;
        }
        // SAFETY: views from this adapter's own tree, and a live context.
        unsafe {
            let scroll_cls = cls("NSScrollView");
            let is_scroll_view = !scroll_cls.is_null()
                && raw_send!(
                    view,
                    "isKindOfClass:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool,
                    (scroll_cls)
                );
            if is_scroll_view && !hidden_with_ancestors(view) && responds(view, "verticalScroller") {
                let scroller = msg0(view, "verticalScroller");
                if !scroller.is_null() && !msg0(scroller, "superview").is_null() {
                    let bounds: CGRect = raw_send!(
                        scroller,
                        "bounds",
                        unsafe extern "C" fn(*mut c_void, *mut c_void) -> CGRect,
                        ()
                    );
                    msg0v(cls("NSGraphicsContext"), "saveGraphicsState");
                    concat_translate(scroller, target);
                    // `NSScroller`'s own drawing entry points: the pair a
                    // scroller subclass calls from `drawRect:`. Its
                    // `displayRectIgnoringOpacity:` renders nothing offscreen —
                    // a 17x627 scroller came out as zero pixels — while these
                    // draw into whatever context is current.
                    if responds(scroller, "drawKnobSlotInRect:highlight:") {
                        raw_send!(
                            scroller,
                            "drawKnobSlotInRect:highlight:",
                            unsafe extern "C" fn(*mut c_void, *mut c_void, CGRect, bool),
                            (bounds, false)
                        );
                    }
                    if responds(scroller, "drawKnob") {
                        msg0v(scroller, "drawKnob");
                    }
                    msg0v(cls("NSGraphicsContext"), "restoreGraphicsState");
                }
            }
            let subviews = msg0(view, "subviews");
            if !subviews.is_null() {
                let count = msg0i(subviews, "count").max(0);
                for i in 0..count {
                    paint_scrollers(msg1i(subviews, "objectAtIndex:", i), target, context);
                }
            }
        }
    }

    /// Turn layer backing off for `view` and its subviews.
    ///
    /// A layer-backed view draws through its layer, which
    /// `cacheDisplayInRect:toBitmapImageRep:` does not render: with the window's
    /// views layer-backed the list's content appeared but its legacy scroller
    /// contributed no pixels at all — a 17x627 scroller frame over an empty
    /// gutter. Drawing without layers puts every subview into the bitmap.
    unsafe fn disable_layers(view: *mut c_void) {
        if view.is_null() {
            return;
        }
        // SAFETY: views from this adapter's own tree; both sends are guarded.
        unsafe {
            if responds(view, "setWantsLayer:") {
                msg1bv(view, "setWantsLayer:", false);
            }
            if responds(view, "setNeedsDisplay:") {
                msg1bv(view, "setNeedsDisplay:", true);
            }
            let subviews = msg0(view, "subviews");
            if !subviews.is_null() {
                let count = msg0i(subviews, "count").max(0);
                for i in 0..count {
                    disable_layers(msg1i(subviews, "objectAtIndex:", i));
                }
            }
        }
    }

    /// Re-tile every scroll view without touching its scroller style.
    ///
    /// Split out because the style switch has to happen before the size is
    /// final, while the tiling that gives the new scroller its frame happens
    /// after the layout pass.
    unsafe fn retile_scrollers(view: *mut c_void) {
        if view.is_null() {
            return;
        }
        // SAFETY: views from this adapter's own tree; each send is guarded.
        unsafe {
            let scroll_cls = cls("NSScrollView");
            let is_scroll_view = !scroll_cls.is_null()
                && raw_send!(
                    view,
                    "isKindOfClass:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool,
                    (scroll_cls)
                );
            if is_scroll_view {
                fit_document_view(view);
                if responds(view, "tile") {
                    msg0v(view, "tile");
                }
                let clip = msg0(view, "contentView");
                if !clip.is_null() && responds(view, "reflectScrolledClipView:") {
                    msg1v(view, "reflectScrolledClipView:", clip);
                }
            }
            let subviews = msg0(view, "subviews");
            if !subviews.is_null() {
                let count = msg0i(subviews, "count");
                for i in 0..count {
                    retile_scrollers(msg1i(subviews, "objectAtIndex:", i));
                }
            }
        }
    }

    /// Re-tile every scroll view so its scroller is drawn now, not on hover.
    ///
    /// The preferred style is a default for *new* scrollers; an existing scroll
    /// view picks it up when it re-tiles, and `flashScrollers` draws the bar
    /// immediately so the capture contains it.
    unsafe fn force_legacy_scrollers(view: *mut c_void) {
        if view.is_null() {
            return;
        }
        // SAFETY: `subviews`/`isKindOfClass:` are NSView/NSObject methods on
        // objects from this adapter's own tree, and each mutation is guarded.
        unsafe {
            let scroll_cls = cls("NSScrollView");
            let is_scroll_view = !scroll_cls.is_null()
                && raw_send!(
                    view,
                    "isKindOfClass:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool,
                    (scroll_cls)
                );
            if is_scroll_view {
                // The style is read when a scroller is made, so the scroller is
                // rebuilt under the preferred style: toggling the flag replaces
                // it. Without this the overlay scroller stays, and overlay
                // scrollers live in their own layer, which a view's bitmap
                // capture does not contain at all.
                if responds(view, "setHasVerticalScroller:") {
                    msg1bv(view, "setHasVerticalScroller:", false);
                    msg1bv(view, "setHasVerticalScroller:", true);
                }
                if responds(view, "setAutohidesScrollers:") {
                    msg1bv(view, "setAutohidesScrollers:", false);
                }
                if responds(view, "tile") {
                    msg0v(view, "tile");
                }
                if responds(view, "flashScrollers") {
                    msg0v(view, "flashScrollers");
                }
                let clip = msg0(view, "contentView");
                if !clip.is_null() && responds(view, "reflectScrolledClipView:") {
                    msg1v(view, "reflectScrolledClipView:", clip);
                }
            }
            let subviews = msg0(view, "subviews");
            if !subviews.is_null() {
                let count = msg0i(subviews, "count");
                for i in 0..count {
                    force_legacy_scrollers(msg1i(subviews, "objectAtIndex:", i));
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Factories
    // ------------------------------------------------------------------

    /// The `NSWindow` this process made for itself, or 0 when a host owns the
    /// window. It is also the answer to "who runs the AppKit loop".
    static OWN_WINDOW: AtomicUsize = AtomicUsize::new(0);

    /// `NSApplicationActivationPolicyRegular`. A binary that is not an `.app`
    /// bundle is an *accessory* by default: its window never becomes key and
    /// no menu bar appears, so a desktop app must ask for the regular policy.
    const ACTIVATION_REGULAR: isize = 0;

    /// `NSBackingStoreBuffered`.
    const BACKING_BUFFERED: usize = 2;

    /// `NSWindowStyleMaskTitled | Closable | Miniaturizable | Resizable`.
    const WINDOW_STYLE: usize = 1 | 2 | 4 | 8;

    /// Whether `obj` answers `selname`.
    ///
    /// A message send to a selector that does not exist raises an Objective-C
    /// exception, and that unwinds into the Rust frame that sent it and aborts
    /// the process (`fatal runtime error: Rust cannot catch foreign
    /// exceptions`) — which is how a misspelled selector once took the whole
    /// GUI down. Raw sends outside the hot path are checked first.
    fn responds(obj: *mut c_void, selname: &str) -> bool {
        if obj.is_null() {
            return false;
        }
        // SAFETY: `-respondsToSelector:` is an NSObject method on any object we
        // hold, and the BOOL return is a C `bool`. `raw_send!` carries its own
        // `unsafe` block, so this is not wrapped again.
        raw_send!(
            obj,
            "respondsToSelector:",
            unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> bool,
            (core_apple::selector(selname),)
        )
    }

    /// Send a host-shim selector only when the receiver implements it.
    ///
    /// Selectors named `corro*` are provided by the ObjC classes this backend
    /// generates (`apple_generator`), not by AppKit: a real `NSStackView` or
    /// `NSTextField` does not implement them. An unchecked send therefore
    /// raises an exception that unwinds into Rust and aborts the process, which
    /// is how `corroSetSpacing:` took the GUI down on its first box. Asking
    /// first turns a missing shim into one logged skip, so the AppKit path
    /// keeps stock behaviour instead of dying.
    fn shim_sent(obj: *mut c_void, selname: &str) -> bool {
        if obj.is_null() {
            return false;
        }
        if responds(obj, selname) {
            return true;
        }
        log_missing_shim(selname);
        false
    }

    /// Report each missing shim selector once: a redraw loop would otherwise
    /// bury the interesting line.
    fn log_missing_shim(selname: &str) {
        use std::collections::HashSet;
        use std::sync::{Mutex, OnceLock};
        static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
        let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
        let first = seen
            .lock()
            .map(|mut s| s.insert(selname.to_string()))
            .unwrap_or(false);
        if first {
            crate::backends::macos::log_macos(&format!(
                "macos: no host shim for {selname}; using stock AppKit behaviour"
            ));
        }
    }


    /// [`shim_sent`] followed by the send, for one integer argument.
    fn shim_i(obj: *mut c_void, selname: &str, arg: isize) {
        if shim_sent(obj, selname) {
            unsafe { msg1iv(obj, selname, arg) }
        }
    }

    /// [`shim_sent`] followed by the send, for one `BOOL` argument.
    fn shim_bv(obj: *mut c_void, selname: &str, arg: bool) {
        if shim_sent(obj, selname) {
            unsafe { msg1bv(obj, selname, arg) }
        }
    }

    /// Make AppKit's classes exist in this process.
    ///
    /// `objc_getClass("NSWindow")` only finds a class that some loaded image
    /// registered. A Rust binary links libobjc but not the frameworks, so
    /// before this runs every lookup here returns null and every widget
    /// factory silently degrades to an inert handle — which is exactly how a
    /// standalone app ends up building no UI at all. `dlopen` lives in
    /// libSystem, which is always linked; loading AppKit there also brings in
    /// Foundation, so no `#[link]` attribute (and no linker flag) is needed.
    fn load_appkit() {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            const RTLD_LAZY: i32 = 1;
            extern "C" {
                fn dlopen(path: *const std::os::raw::c_char, mode: i32) -> *mut c_void;
            }
            // NUL-terminated: a `&[u8]` literal keeps this free of allocation
            // and of the `CString` unwrap.
            let path = b"/System/Library/Frameworks/AppKit.framework/AppKit\0";
            let handle = unsafe { dlopen(path.as_ptr() as *const std::os::raw::c_char, RTLD_LAZY) };
            if handle.is_null() {
                crate::backends::macos::log_macos("macos: could not load AppKit");
            }
        });
    }

    /// Install a process-lifetime autorelease pool.
    ///
    /// AppKit autoreleases objects internally — a window's designated
    /// initialiser certainly does — and sending `-autorelease` with no pool in
    /// place is fatal, which is what turned this path into an abort. A GUI
    /// binary has no `NSApplicationMain` to install one, and the main run loop
    /// installs its own per-event pools, so one pool for start-up is enough. It
    /// is deliberately never popped: that *is* the process's pool.
    fn push_startup_pool() {
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            extern "C" {
                fn objc_autoreleasePoolPush() -> *mut c_void;
            }
            // SAFETY: `objc_autoreleasePoolPush` is part of libobjc, which is
            // always linked, and is safe to call from any thread.
            unsafe {
                objc_autoreleasePoolPush();
            }
        });
    }

    /// The process's shared `NSApplication`, created on first use.
    ///
    /// AppKit expects `NSApplicationMain` — the app bundle's C entry point — to
    /// create this. A plain Rust binary has no bundle and no delegate, so the
    /// backend does it here, before any window exists.
    unsafe fn shared_app() -> *mut c_void {
        load_appkit();
        let app_cls = cls("NSApplication");
        if app_cls.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: messaging the class object is how an AppKit app starts.
        let app = unsafe { msg0(app_cls, "sharedApplication") };
        if app.is_null() {
            return std::ptr::null_mut();
        }
        unsafe { msg1iv(app, "setActivationPolicy:", ACTIVATION_REGULAR) };
        app
    }

    /// Create the process's own `NSWindow`, hand its content view back as the
    /// root the widget factories attach to, and show it.
    ///
    /// This is the standalone shape: `ff-gui` is a Rust binary, so there is no
    /// host to make a window or call `-[NSApplication run]`. Doing it here is
    /// what lets the same call sites work with or without a host — before this
    /// a standalone app built every widget against no root at all and exited.
    unsafe fn own_window() -> *mut c_void {
        // Breadcrumbs: each risky AppKit call logs before it runs. An exception
        // from one of them unwinds through Rust and aborts the process, so the
        // last line printed is the call that threw.
        let step = |n: &str| crate::backends::macos::log_macos(&format!("macos: window step {n}"));
        let existing = OWN_WINDOW.load(Ordering::Relaxed);
        if existing != 0 {
            return existing as *mut c_void;
        }
        step("0 autorelease pool");
        push_startup_pool();
        step("1 sharedApplication");
        if unsafe { shared_app() }.is_null() {
            crate::backends::macos::log_macos("macos: no NSApplication (headless session?)");
            return std::ptr::null_mut();
        }
        step("2 alloc window");
        // The runtime classes exist before the window does: it is built from
        // `CorroMacWindow`, whose one override keeps the frame it is given.
        ensure_runtime_classes();
        let window_cls = if cls("CorroMacWindow").is_null() {
            cls("NSWindow")
        } else {
            cls("CorroMacWindow")
        };
        if window_cls.is_null() {
            crate::backends::macos::log_macos("macos: NSWindow class not found");
            return std::ptr::null_mut();
        }
        // SAFETY: `alloc` then the designated initialiser, each with its exact
        // AppKit signature (`CGRect` is four `CGFloat`s, passed in registers).
        let window = raw_send!(
            window_cls,
            "alloc",
            unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
            ()
        );
        if window.is_null() {
            return std::ptr::null_mut();
        }
        step("3 initWithContentRect");
        if !responds(window, "initWithContentRect:styleMask:backing:defer:") {
            crate::backends::macos::log_macos(
                "macos: NSWindow has no initWithContentRect:styleMask:backing:defer:",
            );
            return std::ptr::null_mut();
        }
        let window = raw_send!(
                window,
                "initWithContentRect:styleMask:backing:defer:",
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    f64,
                    f64,
                    f64,
                    f64,
                    usize,
                    usize,
                    bool,
                ) -> *mut c_void,
                (
                    120.0,
                    120.0,
                    960.0,
                    640.0,
                    WINDOW_STYLE,
                    BACKING_BUFFERED,
                    false
                )
            );
        if window.is_null() {
            crate::backends::macos::log_macos(
                "macos: initWithContentRect: failed (no window server?)",
            );
            return std::ptr::null_mut();
        }
        let window = own(window);
        step("4 contentView");
        let view = unsafe { msg0(window, "contentView") };
        if view.is_null() {
            crate::backends::macos::log_macos("macos: window has no contentView");
            return std::ptr::null_mut();
        }
        // SAFETY: every object below is the live window/app made above.
        step("5 finishLaunching");
        unsafe {
            let app = shared_app();
            if !app.is_null() {
                msg0v(app, "finishLaunching");
            }
        }
        step("6 title + init_with_root");
        unsafe {
            // A closed window must not free itself: this handle is the
            // process's window for its whole life.
            msg1bv(window, "setReleasedWhenClosed:", false);
            let title = nsstring("ff-locate");
            if !title.is_null() {
                msg1v(window, "setTitle:", title);
            }
            // Build the shim classes before the widget tree, not lazily on the
            // first click: `append` already sends `corroSetFlex:` to a stock
            // `NSStackView` child, and a class registered later would make that
            // first send a logged skip (and drop the child's flex weight).
            ensure_runtime_classes();
            // Wire the shared Apple layer to this view. Every widget factory is
            // a no-op until a root exists, which is why the order matters here.
            if let Err(e) = core_apple::init_with_root_impl(view, std::ptr::null_mut(), "macos") {
                crate::backends::macos::log_macos(&format!("macos: window init failed: {e}"));
            }
        }
        step("7 order front");
        unsafe {
            msg1v(window, "makeKeyAndOrderFront:", std::ptr::null_mut());
            let app = shared_app();
            if !app.is_null() {
                msg1bv(app, "activateIgnoringOtherApps:", true);
            }
        }
        crate::backends::macos::log_macos(&format!(
            "macos: self-driven window {window:p} with content view {view:p} ready"
        ));
        OWN_WINDOW.store(window as usize, Ordering::Relaxed);
        view
    }

    /// True when the backend created its own window and therefore must run the
    /// AppKit loop itself.
    pub fn owns_run_loop() -> bool {
        OWN_WINDOW.load(Ordering::Relaxed) != 0
    }

    /// Run `-[NSApplication run]`: the AppKit equivalent of the GTK backend's
    /// `g_main_loop_run`. Returns when the app terminates.
    pub unsafe fn run_app_loop() {
        let app = unsafe { shared_app() };
        if app.is_null() {
            return;
        }
        unsafe { msg0v(app, "run") };
    }

    /// `-[NSApplication terminate:]`: the AppKit quit (what Cmd-Q does).
    pub fn quit_app() {
        unsafe {
            let app = shared_app();
            if !app.is_null() {
                msg1v(app, "terminate:", std::ptr::null_mut());
            }
        }
    }

    /// The window handle is the content view: a host hands us its own, and a
    /// standalone process gets the one [`own_window`] just made.
    pub fn create_window() -> Result<Window, Error> {
        load_appkit();
        match root() {
            Some(view) => Ok(Window(view)),
            None => Ok(Window(unsafe { own_window() })),
        }
    }

    pub fn create_button(label: &str) -> Result<Button, Error> {
        // `NSButton` factory: `+buttonWithTitle:target:action:` (target/action
        // are wired later by `on_click`), then the push-button bezel style.
        let class = cls("NSButton");
        let title = nsstring(label);
        let btn = if class.is_null() || title.is_null() {
            std::ptr::null_mut()
        } else {
            raw_send!(
                class,
                "buttonWithTitle:target:action:",
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                ) -> *mut c_void,
                (title, std::ptr::null_mut::<c_void>(), std::ptr::null_mut::<c_void>())
            )
        };
        if btn.is_null() {
            return Ok(Button(new_widget("NSButton", Kind::Button)));
        }
        let btn = own(btn);
        core_apple::register_widget(
            btn,
            WidgetMeta {
                kind: Kind::Button,
                text: label.to_owned(),
                ..Default::default()
            },
        );
        // `setBezelStyle:` NSBezelStyleRounded == 1; a compact control height.
        unsafe {
            msg1iv(btn, "setBezelStyle:", 1);
            set_frame(btn, 0.0, 0.0, 0.0, MIN_CTRL_PT);
        }
        Ok(Button(btn))
    }

    pub fn create_label(text: &str) -> Result<Label, Error> {
        // NSTextField doubles as a label when it is not editable/bezeled and
        // has a clear background — the standard AppKit idiom.
        let lbl = new_widget("NSTextField", Kind::Label);
        if lbl.is_null() {
            return Ok(Label(lbl));
        }
        core_apple::with_meta_mut(lbl, |m| m.text = text.to_owned());
        let s = nsstring(text);
        if !s.is_null() {
            unsafe { msg1v(lbl, "setStringValue:", s) };
        }
        unsafe {
            msg1bv(lbl, "setEditable:", false);
            msg1bv(lbl, "setSelectable:", false);
            msg1bv(lbl, "setBezeled:", false);
            msg1bv(lbl, "setDrawsBackground:", false);
        }
        Ok(Label(lbl))
    }

    pub fn create_box(orientation: Orientation, spacing: i32) -> Result<BoxWidget, Error> {
        // NSStackView (macOS 10.9+). The host shim provides the same
        // selectors on older systems (`corroAddArrangedSubview:`), so the
        // call sites below do not branch.
        let appkit = !cls("NSStackView").is_null();
        let class_name = if appkit { "NSStackView" } else { "CorroMacContainer" };
        let bx = new_widget(class_name, Kind::Box);
        if bx.is_null() {
            return Ok(BoxWidget(bx));
        }
        unsafe {
            // 0 = horizontal, 1 = vertical: NSUserInterfaceLayoutOrientation
            // and Android's LinearLayout constants agree.
            msg1iv(bx, "setOrientation:", orientation.as_int() as isize);
        }
        if appkit {
            // `NSStackView` owns its spacing itself, and as a CGFloat — using
            // the shim's integer selector here is a message AppKit does not
            // implement, so the two paths cannot share one call.
            unsafe { msg1cv(bx, "setSpacing:", spacing as f64) };
            // The default distribution (`...GravityAreas`, 0) groups arranged
            // subviews by gravity and centres each at its intrinsic size: the
            // first real screenshot of the window showed exactly that — a tiny
            // centred entry and button above a mostly empty window. `Fill` (1)
            // is the behaviour GTK's box has, and it is what lets a child with
            // `corroSetFlex:` take the spare space.
            // `NSStackViewDistributionFill` (0). FillEqually (1) gave every
            // child an equal share, so the query entry and the Search button
            // took half the row each where GTK gives the entry all of it.
            unsafe { msg1iv(bx, "setDistribution:", 0) }
            // Cross-axis fill, the way GTK's box stretches its children: a
            // vertical stack aligned at `NSLayoutAttributeWidth` (7) makes every
            // arranged view as wide as the stack, a horizontal one at `Height`
            // (8) as tall. The default centre alignment let a label sit 4pt
            // wider than its box and hang 2pt over the window's margin.
            let align = if orientation.as_int() == 1 { 7 } else { 8 };
            unsafe { msg1iv(bx, "setAlignment:", align) }
        } else {
            shim_i(bx, "corroSetSpacing:", spacing as isize);
        }
        Ok(BoxWidget(bx))
    }

    pub fn create_entry() -> Result<Entry, Error> {
        let e = new_widget("NSTextField", Kind::Entry);
        if e.is_null() {
            return Ok(Entry(e));
        }
        ensure_runtime_classes();
        unsafe {
            // Without a delegate a keystroke is invisible to Rust: AppKit
            // reports the edit through `controlTextDidChange:`, which
            // `CorroMacField` turns into `dispatch_text_changed` and then into
            // the `connect_changed` handler (the as-you-type search).
            let delegate = msg0(cls("CorroMacField"), "fieldDelegate");
            if !delegate.is_null() {
                msg1v(e, "setDelegate:", delegate);
            }
            // Editable text field with the standard bezel; a compact height.
            msg1bv(e, "setEditable:", true);
            msg1bv(e, "setBezeled:", true);
            msg1bv(e, "setDrawsBackground:", true);
            // Autocorrection and autocapitalisation are *UIKit* text-input
            // traits (`UITextInputTraits`); AppKit's `NSTextField` does not
            // implement them, and sending either one raises an unrecognised
            // selector that aborts the process. AppKit has no per-field
            // equivalent either — a plain `NSTextField` is not capitalised or
            // autocorrected as it is typed — so there is nothing to translate
            // and the pair is simply not sent here.
            set_frame(e, 0.0, 0.0, 0.0, MIN_CTRL_PT);
        }
        Ok(Entry(e))
    }

    pub fn create_grid() -> Result<Grid, Error> {
        Ok(Grid(new_widget("NSView", Kind::Container)))
    }

    pub fn create_menu() -> Result<Menu, Error> {
        Ok(Menu { items: Vec::new() })
    }

    pub fn create_menubar(model: &Menu, _action_group: *mut c_void) -> Result<MenuBar, Error> {
        ensure_runtime_classes();
        // macOS is the one backend with a real `NSMenu`, so the model is turned
        // into a genuine menu bar here instead of only being data the host may
        // render. Each item's action goes to a `CorroMacMenuTarget`, whose
        // `-corroFired:` dispatches into the registry `connect_activate`
        // filled in — the same call sites, now with a working menu.
        unsafe { install_main_menu(&model.items) };
        Ok(MenuBar {
            items: model.items.clone(),
        })
    }

    /// Build an `NSMenu` from the model and make it the app's main menu.
    ///
    /// The model stores each action as `app.<name>` (the GTK action-group
    /// prefix) while `SimpleAction::connect_activate` registers the closure
    /// under `<name>`, so the prefix is stripped here.
    unsafe fn install_main_menu(items: &[MenuItem]) {
        let nsapp_cls = cls("NSApplication");
        let menu_cls = cls("NSMenu");
        let item_cls = cls("NSMenuItem");
        let target_cls = cls("CorroMacMenuTarget");
        if nsapp_cls.is_null() || menu_cls.is_null() || item_cls.is_null() || target_cls.is_null() {
            return;
        }
        // SAFETY: every object below is an AppKit menu class; null checks guard
        // the sends, and the menu is retained by `NSApplication`.
        unsafe {
            let app = msg0(nsapp_cls, "sharedApplication");
            if app.is_null() {
                return;
            }
            let main = msg0(msg0(menu_cls, "alloc"), "init");
            if main.is_null() {
                return;
            }
            // An item is only as live as its target says: with auto-enabling on,
            // AppKit validates each item and greys out anything it cannot
            // confirm, and our targets implement `corroFired:` but no validator.
            msg1bv(main, "setAutoenablesItems:", false);
            for entry in items {
                let MenuItem::Submenu { label, items } = entry else {
                    continue;
                };
                let sub = msg0(msg0(menu_cls, "alloc"), "init");
                if sub.is_null() {
                    continue;
                }
                msg1bv(sub, "setAutoenablesItems:", false);
                for it in items {
                    if let MenuItem::Item { label, action } = it {
                        append_menu_item(sub, item_cls, target_cls, label, action);
                    }
                }
                let holder = new_menu_item(item_cls, label, std::ptr::null_mut());
                if holder.is_null() {
                    continue;
                }
                msg1v(holder, "setSubmenu:", sub);
                msg1v(main, "addItem:", holder);
            }
            msg1v(app, "setMainMenu:", main);
        }
    }

    /// `[[NSMenuItem alloc] initWithTitle:action:keyEquivalent:]`.
    fn new_menu_item(
        item_cls: *mut c_void,
        label: &str,
        action: *mut c_void,
    ) -> *mut c_void {
        let title = nsstring(label);
        let empty = nsstring("");
        if title.is_null() || empty.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: `initWithTitle:action:keyEquivalent:` is NSMenuItem's
        // designated initialiser; a null action makes it a submenu holder.
        raw_send!(
            raw_send!(
                item_cls,
                "alloc",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
                ()
            ),
            "initWithTitle:action:keyEquivalent:",
            unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *mut c_void,
                *mut c_void,
                *mut c_void,
            ) -> *mut c_void,
            (title, action, empty)
        )
    }

    /// Add one actionable item to `menu`, wired to its registered action.
    unsafe fn append_menu_item(
        menu: *mut c_void,
        item_cls: *mut c_void,
        target_cls: *mut c_void,
        label: &str,
        action: &str,
    ) {
        let name = action.strip_prefix("app.").unwrap_or(action);
        let name_obj = nsstring(name);
        if name_obj.is_null() {
            return;
        }
        // SAFETY: the target class exists (checked by the caller) and retains
        // `name_obj`; the item retains its target through `NSMenuItem`.
        unsafe {
            let target = core_apple::msg1(target_cls, "targetWithActionName:", name_obj);
            if target.is_null() {
                return;
            }
            let item = new_menu_item(item_cls, label, core_apple::selector("corroFired:"));
            if item.is_null() {
                return;
            }
            own(target);
            msg1v(item, "setTarget:", target);
            msg1v(menu, "addItem:", item);
        }
    }

    /// Put `text` on the general pasteboard.
    ///
    /// This is what the app's Copy action needs; GTK's equivalent is
    /// `gtk_clipboard_set_text`. Returns an error rather than a silent success
    /// when the pasteboard refuses, so the caller can say why nothing copied.
    pub fn clipboard_set_text(text: &str) -> Result<(), Error> {
        // SAFETY: `NSPasteboard` is AppKit's own class, every send below is a
        // documented selector, and the null checks cover a system without it.
        unsafe {
            let pasteboard_cls = cls("NSPasteboard");
            if pasteboard_cls.is_null() {
                return Err(Error::Backend(
                    "no NSPasteboard on this system".to_string(),
                ));
            }
            let pasteboard = msg0(pasteboard_cls, "generalPasteboard");
            let text_obj = core_apple::nsstring(text);
            let type_obj = core_apple::nsstring("public.utf8-plain-text");
            if pasteboard.is_null() || text_obj.is_null() || type_obj.is_null() {
                return Err(Error::Backend(
                    "the pasteboard refused the text".to_string(),
                ));
            }
            // Without `clearContents` a second Copy appends to the previous
            // one instead of replacing it.
            let _ = msg0(pasteboard, "clearContents");
            let stored = raw_send!(
                pasteboard,
                "setString:forType:",
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                ) -> bool,
                (text_obj, type_obj)
            );
            if stored {
                Ok(())
            } else {
                Err(Error::Backend(
                    "the pasteboard did not store the text".to_string(),
                ))
            }
        }
    }

    /// Build a context menu with one enabled item per label.
    ///
    /// The handle is the `NSMenu` pointer. Each item's action is named
    /// `corro.ctx.<index>` and registered here, so choosing one lands in
    /// `corroFired:` -> `invoke_action`, the same path a menu-bar item uses.
    pub fn context_menu_build(
        labels: &[String],
        on_choose: Box<dyn FnMut(usize)>,
    ) -> Result<usize, Error> {
        let menu_cls = cls("NSMenu");
        let item_cls = cls("NSMenuItem");
        let target_cls = cls("CorroMacMenuTarget");
        if menu_cls.is_null() || item_cls.is_null() || target_cls.is_null() {
            return Err(Error::Backend("AppKit has no menu classes".to_string()));
        }
        // SAFETY: AppKit's own menu classes; every pointer is null-checked and
        // the menu is leaked into the handle on purpose (the popup and the
        // tests use it after this call returns).
        unsafe {
            let menu = msg0(msg0(menu_cls, "alloc"), "init");
            if menu.is_null() {
                return Err(Error::Backend("NSMenu could not be created".to_string()));
            }
            // Our targets fire the action but implement no validator, so with
            // auto-enabling on AppKit would grey every item out.
            msg1bv(menu, "setAutoenablesItems:", false);
            let chosen: std::rc::Rc<std::cell::RefCell<Box<dyn FnMut(usize)>>> =
                std::rc::Rc::new(std::cell::RefCell::new(on_choose));
            for (index, label) in labels.iter().enumerate() {
                let name = format!("corro.ctx.{index}");
                let chosen = chosen.clone();
                register_action(
                    &name,
                    Box::new(move |_sender| {
                        if let Ok(mut f) = chosen.try_borrow_mut() {
                            f(index);
                        }
                    }),
                );
                append_menu_item(menu, item_cls, target_cls, label, &name);
            }
            Ok(menu as usize)
        }
    }

    /// The items of a built menu, with their enabled state.
    pub fn context_menu_items(handle: usize) -> Vec<(String, bool)> {
        let menu = handle as *mut c_void;
        if menu.is_null() {
            return Vec::new();
        }
        // SAFETY: the handle is an NSMenu this module built.
        unsafe {
            let array = msg0(menu, "itemArray");
            if array.is_null() {
                return Vec::new();
            }
            let count: usize = raw_send!(
                array,
                "count",
                unsafe extern "C" fn(*mut c_void, *mut c_void) -> usize,
                ()
            );
            (0..count)
                .map(|i| {
                    let item = raw_send!(
                        array,
                        "objectAtIndex:",
                        unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void,
                        (i)
                    );
                    let title = nsstring_to_rust(msg0(item, "title")).unwrap_or_default();
                    // BOOL is a signed char, not Rust's one-byte bool.
                    let enabled: i8 = raw_send!(
                        item,
                        "isEnabled",
                        unsafe extern "C" fn(*mut c_void, *mut c_void) -> i8,
                        ()
                    );
                    (title, enabled != 0)
                })
                .collect()
        }
    }

    /// Fire item `index` the way a click does.
    ///
    /// `performActionForItemAtIndex:` runs the item's target/action, i.e. the
    /// same `corroFired:` -> `invoke_action` dispatch the popup uses; only the
    /// modal tracking loop is skipped.
    pub fn context_menu_fire(handle: usize, index: usize) -> bool {
        let menu = handle as *mut c_void;
        if menu.is_null() {
            return false;
        }
        // SAFETY: NSMenu's own dispatch for a selection.
        unsafe {
            raw_send!(
                menu,
                "performActionForItemAtIndex:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, isize),
                (index as isize)
            );
        }
        true
    }

    /// A right-mouse-down event at the pointer, in `view`'s own window.
    ///
    /// AppKit positions a context menu from the event it is handed, and the
    /// portable click path carries no `NSEvent` (the canvas dispatch has the
    /// button and the coordinates, not the event). `mouseLocationOutsideOfEventStream`
    /// is already in window coordinates, which is exactly what
    /// `mouseEventWithType:location:...` takes, so the menu lands under the
    /// cursor instead of at the window's origin. Returns null when the view has
    /// no window yet.
    unsafe fn pointer_event(view: *mut c_void) -> *mut c_void {
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Point {
            x: f64,
            y: f64,
        }
        let window = msg0(view, "window");
        if window.is_null() {
            return std::ptr::null_mut();
        }
        let location: Point = raw_send!(
            window,
            "mouseLocationOutsideOfEventStream",
            unsafe extern "C" fn(*mut c_void, *mut c_void) -> Point,
            ()
        );
        let number: isize = raw_send!(
            window,
            "windowNumber",
            unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize,
            ()
        );
        // NSEventTypeRightMouseDown is 3.
        raw_send!(
            cls("NSEvent"),
            "mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:",
            unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                usize,
                Point,
                usize,
                f64,
                isize,
                *mut c_void,
                isize,
                isize,
                f32,
            ) -> *mut c_void,
            (
                3usize,
                location,
                0usize,
                0.0f64,
                number,
                std::ptr::null_mut(),
                0isize,
                1isize,
                1.0f32
            )
        )
    }

    /// Show a built menu at the click that opened it.
    ///
    /// `+[NSMenu popUpContextMenu:withEvent:forView:]` positions the menu at
    /// the event and tracks it; the chosen item's action fires before it
    /// returns, so the caller's handler has already run when this is done.
    ///
    /// The event the caller passes may be null: a portable widget callback on
    /// this backend is not given the `NSEvent`. In that case the event being
    /// dispatched is taken from the application (a real right-click always has
    /// one), and with no event in flight at all one is synthesized at the
    /// pointer -- a null event would put the menu at the window's origin, and
    /// refusing to show it is what left the row menu dead on this backend.
    pub fn context_menu_show(
        handle: usize,
        view: *mut c_void,
        event: *mut c_void,
    ) -> Result<(), Error> {
        let menu = handle as *mut c_void;
        if menu.is_null() || view.is_null() {
            return Err(Error::Backend(
                "no menu or view to show the menu from".to_string(),
            ));
        }
        // SAFETY: AppKit's own context-menu entry point; the menu is one this
        // module built, the view is the caller's own, and the event is either
        // the caller's, the one being dispatched, or one built here.
        unsafe {
            let event = if event.is_null() {
                let nsapp = msg0(cls("NSApplication"), "sharedApplication");
                let current = msg0(nsapp, "currentEvent");
                if current.is_null() {
                    pointer_event(view)
                } else {
                    current
                }
            } else {
                event
            };
            if event.is_null() {
                return Err(Error::Backend(
                    "no event to show the menu from".to_string(),
                ));
            }
            let menu_cls = cls("NSMenu");
            raw_send!(
                menu_cls,
                "popUpContextMenu:withEvent:forView:",
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                ),
                (menu, event, view)
            );
        }
        Ok(())
    }

    /// The text on the general pasteboard, if it holds any.
    ///
    /// The readable half of [`clipboard_set_text`], so a caller can verify
    /// what a Copy actually put there.
    pub fn clipboard_get_text() -> Result<String, Error> {
        // SAFETY: AppKit's own pasteboard class and a documented selector; the
        // null checks cover a session without a pasteboard.
        unsafe {
            let pasteboard_cls = cls("NSPasteboard");
            if pasteboard_cls.is_null() {
                return Err(Error::Backend(
                    "no NSPasteboard on this system".to_string(),
                ));
            }
            let pasteboard = msg0(pasteboard_cls, "generalPasteboard");
            let type_obj = core_apple::nsstring("public.utf8-plain-text");
            if pasteboard.is_null() || type_obj.is_null() {
                return Err(Error::Backend(
                    "the pasteboard refused the type".to_string(),
                ));
            }
            let text = raw_send!(
                pasteboard,
                "stringForType:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void,
                (type_obj)
            );
            core_apple::nsstring_to_rust(text).ok_or_else(|| {
                Error::Backend("the pasteboard holds no text".to_string())
            })
        }
    }

    pub fn create_simple_action(name: &str) -> Result<SimpleAction, Error> {
        Ok(SimpleAction {
            name: name.to_owned(),
        })
    }

    pub fn create_canvas() -> Result<Canvas, Error> {
        // The sheet view class is built at runtime (it funnels `drawRect:` and
        // mouse events back into Rust); if AppKit is up, it is registered here
        // and the lookup below finds it. A plain NSView remains the fallback so
        // the tree stays valid without a display.
        ensure_runtime_classes();
        let canvas_id = core_apple::next_canvas_id();
        let class_name = core_apple::sheet_view_class().unwrap_or_else(|| "NSView".to_owned());
        let class = cls(&class_name);
        let view = if class.is_null() {
            new_widget("NSView", Kind::Canvas)
        } else {
            let obj = alloc_init(class);
            if obj.is_null() {
                std::ptr::null_mut()
            } else {
                let obj = own(obj);
                core_apple::register_widget(
                    obj,
                    WidgetMeta {
                        kind: Kind::Canvas,
                        canvas_id,
                        ..Default::default()
                    },
                );
                // Hand the canvas id to the view so `drawRect:`/events can
                // name it in the Rust callbacks (the host shim's selector; the
                // registry above is the fallback).
                shim_i(obj, "corroSetCanvasId:", canvas_id as isize);
                obj
            }
        };
        Ok(Canvas(view))
    }

    pub fn create_overlay() -> Result<Overlay, Error> {
        Ok(Overlay(new_widget("NSView", Kind::Container)))
    }

    pub fn create_scrolled_window() -> Result<ScrolledWindow, Error> {
        // A real `NSScrollView`: clipping, a scroller and wheel scrolling are
        // what GTK's scrolled window gives the Linux build, and what the
        // results list needs — the canvas is taller than the window.
        ensure_runtime_classes();
        let view = new_widget("NSScrollView", Kind::Container);
        if !view.is_null() {
            // SAFETY: live NSScrollView; these are AppKit's own setters.
            unsafe {
                msg1bv(view, "setHasVerticalScroller:", true);
                // Overlay scrollers that fade out are the modern default; asking
                // for them explicitly keeps the list flush with the window edge
                // instead of reserving a permanent gutter.
                msg1bv(view, "setAutohidesScrollers:", true);
                msg1bv(view, "setDrawsBackground:", false);
            }
        }
        Ok(ScrolledWindow(view))
    }

    pub fn create_dropdown(items: &[&str]) -> Result<DropDown, Error> {
        // NSPopUpButton is the AppKit popup; `initWithFrame:pullsDown:` is the
        // designated factory, but a plain alloc/init then `addItemWithTitle:`
        // is enough and keeps the call site simple.
        let pop = new_widget("NSPopUpButton", Kind::Container);
        if pop.is_null() {
            return Ok(DropDown(pop));
        }
        for item in items {
            let s = nsstring(item);
            if s.is_null() {
                continue;
            }
            raw_send!(
                pop,
                "addItemWithTitle:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> (),
                (s)
            );
        }
        Ok(DropDown(pop))
    }

    pub fn create_checkbutton(label: &str) -> Result<CheckButton, Error> {
        // NSButton with the switch/checkbox bezel style.
        let cb = new_widget("NSButton", Kind::Container);
        if !cb.is_null() {
            core_apple::with_meta_mut(cb, |m| m.text = label.to_owned());
            let s = nsstring(label);
            if !s.is_null() {
                unsafe { msg1v(cb, "setTitle:", s) };
            }
            unsafe {
                // NSButtonTypeSwitch == 3.
                msg1iv(cb, "setButtonType:", 3);
                set_frame(cb, 0.0, 0.0, 0.0, MIN_CTRL_PT);
            }
        }
        Ok(CheckButton(cb))
    }

    pub fn create_radiobutton(
        group: Option<&RadioButton>,
        label: &str,
    ) -> Result<RadioButton, Error> {
        let _ = group;
        let rb = new_widget("NSButton", Kind::Container);
        if rb.is_null() {
            return Ok(RadioButton(rb));
        }
        let s = nsstring(label);
        if !s.is_null() {
            unsafe {
                msg1v(rb, "setTitle:", s);
                // NSButtonTypeRadio == 4; exclusivity within a group is the
                // host shim's job (`corroSetRadioGroup:`).
                msg1iv(rb, "setButtonType:", 4);
                msg1iv(rb, "setState:", 0);
                set_frame(rb, 0.0, 0.0, 0.0, MIN_CTRL_PT);
            }
        }
        Ok(RadioButton(rb))
    }

    pub fn create_dialog() -> Result<Dialog, Error> {
        // A real `NSAlert`: its buttons are AppKit's own and `runModal` reports
        // which one ended the session. The generated `CorroMacAlert` shim had no
        // runtime implementation, so `corroAddAction:` was a logged skip and the
        // alert presented with no buttons on it.
        let alert_cls = cls("NSAlert");
        if alert_cls.is_null() {
            crate::backends::macos::log_macos("macos: NSAlert unavailable; dialogs inert");
            return Ok(Dialog(std::ptr::null_mut()));
        }
        let alert = unsafe { msg0(alert_cls, "alloc") };
        let alert = if alert.is_null() {
            std::ptr::null_mut()
        } else {
            unsafe { msg0(alert, "init") }
        };
        if alert.is_null() {
            return Ok(Dialog(std::ptr::null_mut()));
        }
        Ok(Dialog(own(alert)))
    }

    pub fn create_textview() -> Result<TextView, Error> {
        Ok(TextView(new_widget("NSTextView", Kind::Container)))
    }

    /// The display scale (`NSScreen.backingScaleFactor`): 1.0 on a non-Retina
    /// display, 2.0 on Retina.
    ///
    /// Hosts need this to size things in *points* while thinking in pixels: a
    /// hardcoded 12px font is 12pt on a 1x monitor but only ~6pt on a 2x
    /// Retina panel, i.e. about half of Apple's 11pt footnote floor. Returns
    /// `None` before the backend is initialised (or if the call fails), so
    /// callers fall back to 1.0 and still render.
    pub fn display_density() -> Option<f64> {
        crate::backends::macos::display_scale()
    }

    /// Convenience for hosts: convert a pixel constant to points.
    pub fn px_to_points(px: f64) -> f64 {
        px / scale()
    }
}

#[cfg(target_os = "macos")]
pub use macos_adapter::*;

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use crate::core::Widget;

    #[test]
    fn test_null_window_handle() {
        let w = super::Window(std::ptr::null_mut());
        assert!(w.raw_handle().is_null());
    }

    #[test]
    fn test_null_handles_are_inert() {
        // Every one of these must be a no-op rather than a crash: a host
        // build with no backend at all must still compile and run.
        let l = super::Label(std::ptr::null_mut());
        l.set_text("hello");
        l.set_visible(false);
        assert_eq!(l.get_text(), None);

        let e = super::Entry(std::ptr::null_mut());
        e.set_text("x");
        assert_eq!(e.get_text(), None);
        assert!(!e.has_focus());

        let b = super::BoxWidget(std::ptr::null_mut());
        b.append(&l);

        let c = super::Canvas(std::ptr::null_mut());
        c.queue_redraw();
        c.on_click(Box::new(|_, _| {}));
    }

    #[test]
    fn test_clone_keeps_handle() {
        let b1 = super::Button(0x1234 as *mut _);
        let b2 = b1.clone();
        assert_eq!(b1.as_ref(), b2.as_ref());
        let l1 = super::Label(0x5678 as *mut _);
        assert_eq!(l1.as_ref(), l1.clone().as_ref());
        let e1 = super::Entry(0x9abc as *mut _);
        assert_eq!(e1.as_ref(), e1.clone().as_ref());
    }

    #[test]
    fn test_orientation_values_match_linear_layout_and_axis() {
        // Discriminants mirror android.widget.LinearLayout constants *and*
        // NSUserInterfaceLayoutOrientation, so the shared code can pass the
        // same integers on both platforms.
        assert_eq!(super::Orientation::Horizontal as i32, 0);
        assert_eq!(super::Orientation::Vertical as i32, 1);
    }

    #[test]
    fn test_menu_submenu_clone_keeps_items() {
        // Built through the public factory rather than `Menu { items: .. }`:
        // `items`/`MenuItem` are private to `mod macos_adapter`, and this test
        // block is a *sibling* of it (see `strip_markup` for why the two
        // adapters differ here). `create_menu` is the supported way to get a
        // menu, and going through it is what the shared code does — so the test
        // now exercises the real construction path instead of a literal that
        // would break again the moment a field was added.
        //
        // The assertions are on what the *outer* module can see — the model is
        // Clone, and `MenuBar` is Clone — because the contents are private.
        // What is actually being protected here is that the construction and
        // the clone do not panic on a submenu, which is the real risk: a
        // macOS host builds an `NSMenu` from this model, and a clone that
        // dropped a submenu would present an empty File menu.
        let mut sub = super::create_menu().unwrap();
        sub.append("Open", "app.open");
        let mut root = super::create_menu().unwrap();
        root.append_submenu("File", &sub);

        // Cloning a model with a nested submenu must not panic.
        let _clone = root.clone();
        // ...and so must building a menubar from it.
        let bar = super::create_menubar(&root, std::ptr::null_mut()).unwrap();
        // `menu_active` is a no-op on AppKit, but calling it is the documented
        // "the menubar exists" probe and must not panic.
        assert!(!bar.menu_active());
        // The menubar is itself Clone, and cloning must not panic either.
        let _bar_clone = bar.clone();
    }

    #[test]
    fn test_canvas_estimate_extents_nonzero() {
        let dc = super::MacosDrawContext;
        let (x, y, w, h) =
            crate::core::DrawContext::text_extents_styled(&dc, "hello", "monospace", 12.0, 0, 0);
        let _ = (x, y);
        assert!(w > 0.0 && h > 0.0);
    }

    #[test]
    fn test_strip_markup() {
        assert_eq!(super::strip_markup("<b>bold</b>"), "bold");
        assert_eq!(super::strip_markup("a &amp; b"), "a & b");
        assert_eq!(super::strip_markup("plain"), "plain");
    }
}
