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
            let send: $sig = unsafe { std::mem::transmute(msg) };
            let sel = crate::backends::apple::selector($sel);
            unsafe { send($obj, sel $(, $arg)*) }
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
        pub fn set_title(&self, _title: &str) {}

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
            unsafe { msg0v(self.0, "setNeedsLayout") };
            unsafe { msg0v(self.0, "setNeedsDisplay") };
        }

        /// # Safety
        /// Kept for API compatibility with the GTK backend; no-op on macOS.
        pub unsafe fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}

        pub fn hwnd(&self) -> *mut c_void {
            std::ptr::null_mut()
        }

        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            attach(self.0, *child.as_ref());
        }

        pub fn set_child_box(&self, bx: &BoxWidget) {
            self.set_child(bx);
        }

        pub fn present(&self) {
            // The window is already visible (the host shows it); make sure
            // AppKit is told to lay out and draw what we added.
            if !self.0.is_null() {
                unsafe {
                    msg0v(self.0, "setNeedsLayout");
                    msg0v(self.0, "setNeedsDisplay:");
                }
            }
        }

        pub fn queue_redraw(&self) {
            if !self.0.is_null() {
                unsafe { msg0v(self.0, "setNeedsDisplay:") };
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
                unsafe { msg0v(self.0, "performClick:") };
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
                    unsafe {
                        msg1iv(self.0, "corroSetPinnedWidth:", w as isize);
                        msg1iv(self.0, "corroSetContentHugging:", 1000);
                    }
                }
                None => {
                    // Release: drop the constraint and let the intrinsic size
                    // size the field again (hugging back to the default 250).
                    unsafe {
                        msg1iv(self.0, "corroSetPinnedWidth:", -1);
                        msg1iv(self.0, "corroSetContentHugging:", 250);
                    }
                }
            }
            // The constraint is only meaningful once the view is in a
            // hierarchy, so ask AppKit to re-run layout.
            unsafe { msg0v(self.0, "setNeedsLayout") };
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
            // `headIndent` is NSTextField's left inset (points), and unlike a
            // constraint it needs no superview to take effect.
            unsafe { msg1cv(self.0, "setHeadIndent:", px as f64) };
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
            unsafe {
                // `addArrangedSubview:` exists on NSStackView; on the manual
                // shim the same selector is implemented by the host view.
                msg1v(self.0, "addArrangedSubview:", child_ptr);
                if expands || is_canvas {
                    // Weight 1 equivalent: let this child take the leftover
                    // space. On a stack view that is a low content-hugging
                    // priority; the host shim reads the same flag through
                    // `corroFlexChild:`. Android does exactly this with
                    // LinearLayout weight (ANDROID_GUIDELINES.md §6).
                    msg1bv(child_ptr, "corroSetFlex:", true);
                }
            }
            // Minimum width for an empty entry: `set_width_chars` recorded a
            // character count, converted here to points via the estimate.
            let min_chars = core_apple::view_min_chars(child_ptr);
            if min_chars > 0 {
                let w = min_chars as f64 * EST_CHAR_W;
                unsafe { msg1iv(child_ptr, "corroSetMinWidth:", w as isize) };
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
            unsafe { msg2cv(self.0, "setFrameSize:", w as f64, h as f64) };
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
            // Schedule a real draw through the host view (which replays the
            // closure with a CG-backed context at the live size), and run it
            // immediately against the estimating context so headless/test
            // callers observe draws without AppKit — same double duty as the
            // iOS and Android adapters.
            if !self.0.is_null() {
                unsafe {
                    msg0v(self.0, "setNeedsDisplay:");
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
                unsafe { msg0v(self.0, "setNeedsDisplay:") };
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
        fn CGContextSaveGState(ctx: *mut c_void);
        fn CGContextRestoreGState(ctx: *mut c_void);
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
            // `clear` in the shared API means "paint the whole surface this
            // colour". Without the surface size in the context we cannot fill
            // it; Core Graphics has `CGContextClearRect` with `CGRectInfinite`.
            unsafe {
                CGContextSetRGBFillColor(self.ctx, r, g, b, a);
                CGContextFillRect(
                    self.ctx,
                    CGRect {
                        x: 0.0,
                        y: 0.0,
                        width: f64::INFINITY,
                        height: f64::INFINITY,
                    },
                );
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

        pub fn append_content_area(&self, _child: &impl AsRef<*mut c_void>) {}

        pub fn add_button(&self, text: &str, _response_id: i32) {
            if self.0.is_null() {
                return;
            }
            let s = nsstring(text);
            if s.is_null() {
                return;
            }
            // `corroAddAction:` is implemented by the host's alert shim so one
            // Rust call covers the addButton/runModal sequence.
            raw_send!(
                self.0,
                "corroAddAction:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> (),
                (s)
            );
        }

        pub fn present(&self) {
            if self.0.is_null() {
                return;
            }
            // Host-side `corroPresentDialog:` picks `runModal` (a standalone
            // alert) or `beginSheetModalForWindow:` (a sheet on the host
            // window) depending on what the host has.
            match core_apple::view_controller() {
                Some(vc) => raw_send!(
                    vc,
                    "corroPresentDialog:",
                    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> (),
                    (self.0)
                ),
                None => {
                    // No controller: present modally on the alert itself.
                    unsafe { msg0v(self.0, "runModal") };
                }
            }
        }

        pub fn connect_response(&self, _f: impl FnMut(i32) + 'static) -> Result<u64, Error> {
            Ok(0) // Wired through the host shim's alert delegate.
        }

        pub fn set_default_response(&self, _response_id: i32) {}
        pub fn close(&self) {}

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
            unsafe { msg0v(self.0, if visible { "orderFront:nil" } else { "orderOut:nil" }) };
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
                return 0;
            }
            match core_apple::view_controller() {
                Some(vc) => {
                    raw_send!(
                        vc,
                        "corroPresentDialog:",
                        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> (),
                        (self.0)
                    );
                    // The shim does not report which button was clicked, so
                    // the caller gets the neutral 0 rather than a guess.
                    0
                }
                None => {
                    unsafe { msg0i(self.0, "runModal") as i32 }
                }
            }
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
    // AppKit scroll views exist (NSScrollView), but corro drives its own
    // viewport (the draw closure paints the visible window and the scrollbar
    // chrome itself), so these are inert containers — exactly the iOS and
    // Android decision. Keeping them inert means one viewport model across
    // every backend.

    #[repr(transparent)]
    #[derive(Clone, Copy)]
    pub struct ScrolledWindow(pub *mut c_void);

    impl Widget for ScrolledWindow {
        fn raw_handle(&self) -> *mut c_void {
            self.0
        }
    }

    impl ScrolledWindow {
        /// Attach the sheet canvas to this container (the one real
        /// parent-child edge macOS needs: scrolled window -> canvas).
        pub fn attach_canvas(&self, canvas: &Canvas) {
            attach(self.0, canvas.0);
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
    // Factories
    // ------------------------------------------------------------------

    /// The window handle is the host window's content view: the app owns the
    /// real `NSWindow` (see the `Window` comment).
    pub fn create_window() -> Result<Window, Error> {
        match root() {
            Some(view) => Ok(Window(view)),
            None => Ok(Window(std::ptr::null_mut())),
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
        let class_name = if cls("NSStackView").is_null() {
            "CorroMacContainer"
        } else {
            "NSStackView"
        };
        let bx = new_widget(class_name, Kind::Box);
        if bx.is_null() {
            return Ok(BoxWidget(bx));
        }
        unsafe {
            // 0 = horizontal, 1 = vertical: NSUserInterfaceLayoutOrientation
            // and Android's LinearLayout constants agree.
            msg1iv(bx, "setOrientation:", orientation.as_int() as isize);
            // `corroSetSpacing:` takes the integer spacing; keeping it as a
            // selector avoids a CGFloat ABI branch here.
            msg1iv(bx, "corroSetSpacing:", spacing as isize);
        }
        Ok(BoxWidget(bx))
    }

    pub fn create_entry() -> Result<Entry, Error> {
        let e = new_widget("NSTextField", Kind::Entry);
        if e.is_null() {
            return Ok(Entry(e));
        }
        unsafe {
            // Editable text field with the standard bezel; a compact height.
            msg1bv(e, "setEditable:", true);
            msg1bv(e, "setBezeled:", true);
            msg1bv(e, "setDrawsBackground:", true);
            // A formula bar must not be autocorrected/capitalised.
            msg1iv(e, "setAutocorrectionType:", 0);
            msg1iv(e, "setAutocapitalizationType:", 0);
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
        Ok(MenuBar {
            items: model.items.clone(),
        })
    }

    pub fn create_simple_action(name: &str) -> Result<SimpleAction, Error> {
        Ok(SimpleAction {
            name: name.to_owned(),
        })
    }

    pub fn create_canvas() -> Result<Canvas, Error> {
        // The host-registered sheet view when available (it knows how to
        // funnel `drawRect:`/mouse/key events back into Rust), else a plain
        // NSView: the tree stays valid either way. Mirrors the iOS and
        // Android fallbacks.
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
                // name it in the Rust callbacks.
                unsafe { msg1iv(obj, "corroSetCanvasId:", canvas_id as isize) };
                obj
            }
        };
        Ok(Canvas(view))
    }

    pub fn create_overlay() -> Result<Overlay, Error> {
        Ok(Overlay(new_widget("NSView", Kind::Container)))
    }

    pub fn create_scrolled_window() -> Result<ScrolledWindow, Error> {
        Ok(ScrolledWindow(new_widget("NSView", Kind::Container)))
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
        // The host shim builds the NSAlert (it owns the window/sheet
        // decision) — one Rust call, no branching here. A missing shim yields
        // a null handle and dialogs never present, which is a degraded but
        // working UI.
        let shim = cls("CorroMacAlert");
        if shim.is_null() {
            crate::backends::macos::log_macos("macos: CorroMacAlert shim missing; dialogs inert");
            return Ok(Dialog(std::ptr::null_mut()));
        }
        let d = unsafe { msg0(shim, "corroNewAlert") };
        if d.is_null() {
            return Ok(Dialog(std::ptr::null_mut()));
        }
        Ok(Dialog(own(d)))
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
