//! iOS backend adapter: UIKit widgets reached through the Objective-C runtime.
//!
//! The direct sibling of `backends_android_adapter.rs` — same shape, same
//! contracts, different OS:
//!
//! * Every widget is a `#[repr(transparent)]` raw Objective-C object handle
//!   (`UIView*`, `UILabel*`, ...), retained by [`crate::backends::ios`]
//!   (`KEEP_ALIVE` there). Methods are best-effort no-ops when the backend was
//!   never initialised (host builds, tests, headless runs, the desktop
//!   `examples/ios_ui.rs` run).
//! * The Canvas renders through a host-supplied `UIView` subclass
//!   (`SheetView`): the draw closure lives in the Rust-side registry and is
//!   dispatched by **canvas id** from the view's `drawRect:`, which funnels
//!   every `DrawContext` call back into Rust. Until such a view exists,
//!   `queue_redraw` is a no-op and `text_extents_*` return a monospace
//!   estimate, so layout never divides by zero.
//! * The model-only types (menu, menubar, action) carry the menu *model* plus
//!   the shared action registry, exactly like Android and wasm: iOS has no
//!   desktop menu bar, so corro renders its own (see `ios/corro/app` for the
//!   `UIMenu`/overflow analogue).
//!
//! Differences from Android worth knowing when reading:
//!
//! * **No JVM to attach.** Android wraps every call in
//!   `with_env_and_activity`; here a message send goes straight to the object.
//! * **Layout.** Android's `LinearLayout` weight becomes a `UIStackView`
//!   arrangement plus flexible constraints; `set_hexpand` is recorded in
//!   `WidgetMeta` and honoured by `BoxWidget::append`.
//! * **Text.** Android measures with `Paint.measureText`; iOS uses
//!   `boundingRectWithSize:options:attributes:` on an `NSAttributedString`
//!   (iOS 7+; `sizeWithAttributes:` on iOS 6 and earlier is the fallback
//!   documented in `docs/IOS_GUIDELINES.md`).

#[cfg(target_os = "ios")]
mod ios_adapter {
    use std::collections::HashMap;
    use std::os::raw::c_void;
    use std::sync::Mutex;

    use once_cell::sync::Lazy;

    use crate::backends::ios::{
        self as core_ios, alloc_init, cls, msg0, msg0i, msg0v, msg1bv, msg1i, msg1iv,
        msg1v, msg1vi, msg4cv, nsstring, nsstring_to_rust, own, Kind, WidgetMeta,
    };
    use crate::core::{DrawContext, Error, Widget};

    // Message-send with an explicit signature, for the handful of host-shim
    // calls the ObjC runtime cannot express as a plain extern fn (see the
    // "Host shim calls" section below for why each one names its signature).
    macro_rules! raw_send {
        ($obj:expr, $sel:expr, $sig:ty, ($($arg:expr),* $(,)?)) => {{
            let msg = crate::backends::ios::msg_shim();
            let send: $sig = unsafe { std::mem::transmute(msg) };
            let sel = crate::backends::ios::selector($sel);
            unsafe { send($obj, sel $(, $arg)*) }
        }};
    }

    // ------------------------------------------------------------------
    // Metrics
    // ------------------------------------------------------------------

    /// Monospace advance estimate (points) used by `text_extents_*` until a
    /// real measurement is possible. Matches the `CHAR_W`-family constants
    /// callers use, and the Android estimate, so layout is comparable.
    const EST_CHAR_W: f64 = 7.2;
    /// Line-height factor for the estimate.
    const EST_LINE_H: f64 = 1.2;

    /// Apple's minimum touch target is 44x44 pt (44dp on Android). Chrome is
    /// padded to it so buttons and the formula entry stay tappable on a
    /// phone; the value is in points, so it needs no display-scale multiply.
    const MIN_TOUCH_PT: f64 = 44.0;

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
        core_ios::display_scale().unwrap_or(1.0).max(1.0)
    }

    // ------------------------------------------------------------------
    // UIKit constructors
    // ------------------------------------------------------------------

    /// The backend's root view, or `None` when uninitialised.
    fn root() -> Option<*mut c_void> {
        core_ios::root_view()
    }

    /// UIKit classes whose `-init` raises, and the selector that creates one
    /// properly.
    ///
    /// `UIStackView` documents `initWithArrangedSubviews:` as its
    /// *designated* initializer; sending it plain `-init` raises
    /// `NSInternalInconsistencyException`. That exception is a **foreign**
    /// throw — it unwinds through the Objective-C runtime, not through Rust —
    /// so `catch_unwind` cannot intercept it and the process dies with
    /// `fatal runtime error: Rust cannot catch foreign exceptions, aborting`.
    ///
    /// That is exactly how this adapter died, twice (iOS CI runs #97 and
    /// #117), always at the same point: `run_gui: new_box` printed and nothing
    /// after it. The trailing screen was the *springboard*, which is why the
    /// failure looked like a screenshot problem and took two runs to trace
    /// back to a layout box.
    ///
    /// The class is a parameter rather than a hardcoded string so the test
    /// that guards this reads the real list. A class whose plain `-init` is
    /// fine (UILabel, UITextField, UIView, ...) must NOT be added: it would
    /// then get an empty-array initializer it has no use for.
    const DESIGNATED_INITIALIZERS: &[(&str, &str)] = &[("UIStackView", "initWithArrangedSubviews:")];

    /// The selector to construct `class_name`, or `"init"` when the plain
    /// one is correct.
    fn initializer_for(class_name: &str) -> &'static str {
        DESIGNATED_INITIALIZERS
            .iter()
            .find(|(cls_name, _)| *cls_name == class_name)
            .map(|(_, sel_name)| *sel_name)
            .unwrap_or("init")
    }

    /// `[[class alloc] initWithArrangedSubviews:@[]]` for a class that needs
    /// it, else `[[class alloc] init]`. An empty array is what the caller
    /// wants either way: `BoxWidget::append` adds children one at a time
    /// afterwards.
    fn alloc_init_class(class: *mut c_void, class_name: &str) -> *mut c_void {
        if class.is_null() {
            return std::ptr::null_mut();
        }
        let sel_name = initializer_for(class_name);
        if sel_name == "init" {
            return alloc_init(class);
        }
        // `raw_send!` supplies its own `unsafe` internally, so this body is
        // deliberately NOT wrapped: an outer `unsafe` here made every
        // expansion report an `unnecessary unsafe block` (lint-clean builds
        // are part of the project's contract, and this repo builds for three
        // Apple targets on every CI run).
        let alloc = unsafe { msg0(class, "alloc") };
        if alloc.is_null() {
            return std::ptr::null_mut();
        }
        // An empty `NSArray`, from the class rather than a literal: `nil`
        // here would raise a *different* uncatchable exception.
        let array_cls = cls("NSArray");
        if array_cls.is_null() {
            return std::ptr::null_mut();
        }
        let empty = unsafe { msg0(array_cls, "array") };
        if empty.is_null() {
            return std::ptr::null_mut();
        }
        let obj = raw_send!(
            alloc,
            sel_name,
            unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void,
            (empty)
        );
        if obj.is_null() {
            return std::ptr::null_mut();
        }
        own(obj)
    }

    /// Allocate + init an instance of `class_name`, register metadata, and
    /// retain it. Returns null when the backend is uninitialised or the class
    /// is missing (a host that ships no UIKit, or an SDK without the class) —
    /// callers degrade instead of crashing.
    fn new_widget(class_name: &str, kind: Kind) -> *mut c_void {
        if !core_ios::is_initialized() {
            return std::ptr::null_mut();
        }
        let class = cls(class_name);
        if class.is_null() {
            core_ios::log_ios(&format!("ios: class {class_name} not found"));
            return std::ptr::null_mut();
        }
        let obj = alloc_init_class(class, class_name);
        if obj.is_null() {
            return std::ptr::null_mut();
        }
        let obj = own(obj);
        core_ios::register_widget(
            obj,
            WidgetMeta {
                kind,
                ..Default::default()
            },
        );
        obj
    }

    /// Set a `CGRect` frame. `CGRect` is zero-filled on creation, so every
    /// widget that is not laid out by a stack view needs a frame; the host's
    /// Auto Layout constraints refine it afterwards.
    unsafe fn set_frame(view: *mut c_void, x: f64, y: f64, w: f64, h: f64) {
        if view.is_null() {
            return;
        }
        unsafe { msg4cv(view, "setFrame:", x, y, w, h) };
    }

    /// Send a `CGFloat`-returning, no-argument message. `msg0c` already picks
    /// the right signature by pointer width (f64 on arm64, f32 on armv7s), so
    /// this is only a naming shim that makes the call sites read honestly.
    unsafe fn msg0c_double(obj: *mut c_void, selname: &str) -> f64 {
        unsafe { crate::backends::ios::msg0c(obj, selname) }
    }

    /// Put `child` inside `parent` (`addSubview:`), keeping the child's own
    /// frame. Null-safe: a null parent (backend not initialised) means the
    /// child is simply never attached, which is what makes host runs work.
    fn attach(parent: *mut c_void, child: *mut c_void) {
        if parent.is_null() || child.is_null() {
            // A null handle here is silent failure: the widget exists in Rust
            // but is never added to the view hierarchy, so the app runs with a
            // blank screen and no error anywhere. That is exactly the symptom
            // being chased (alive process, white screen), so say so.
            core_ios::log_ios(&format!(
                "attach skipped: parent={} child={}",
                if parent.is_null() { "null" } else { "ok" },
                if child.is_null() { "null" } else { "ok" },
            ));
            return;
        }
        unsafe {
            msg1v(parent, "addSubview:", child);
            // Give the child a real frame and let it fill the parent.
            //
            // Without this an `alloc`/`init` UIView has a ZERO frame and, with
            // no constraints, Auto Layout leaves it at 0x0 - which is exactly
            // what the simulator showed: `SheetView.layoutSubviews
            // canvas=1 0x0`, and `drawRect:` never called at all because a
            // zero-sized view is never drawn. The app then runs, reports
            // success everywhere, and shows a white screen.
            //
            // `autoresizingMask` is the pre-Auto-Layout mechanism and is
            // exactly right here: the child tracks the parent's bounds. It
            // needs no constraint bookkeeping and works the same on every iOS
            // version this backend targets - the property that made the whole
            // backend viable on the iOS 7 path.
            // Size via the two CGFloat accessors rather than reading `bounds`
            // as a struct: a CGRect return goes through the HFA register
            // convention on arm64, and the whole point of this backend is not
            // to guess at ABIs. `bounds` is a category method added by the
            // CorroLayout/geometry shim on the UIView side.
            // Use the parent's bounds when they are known, but do not clamp to
            // 1: at construction time a parent can legitimately be 0 tall (it
            // has not been laid out yet), and sizing the child to 1 point then
            // is what produced `layoutSubviews canvas=1 376x1` - a canvas one
            // pixel tall, which "draws" and shows nothing.
            //
            // The autoresizing mask below makes the child track the parent's
            // size once UIKit lays things out, so an approximate initial frame
            // is harmless and a wrong *small* one is not.
            let w = msg0c_double(parent, "corroBoundsWidth");
            let h = msg0c_double(parent, "corroBoundsHeight");
            let w = if w > 1.0 { w } else { 320.0 };
            let h = if h > 1.0 { h } else { 480.0 };
            set_frame(child, 0.0, 0.0, w, h);
            // 1 = flexible width, 2 = flexible height, 16 = flexible top
            // margin, 32 = flexible bottom margin: the child fills and follows
            // the parent as it is laid out.
            msg1iv(child, "setAutoresizingMask:", 1 | 2 | 16 | 32);
        }
    }

    // ------------------------------------------------------------------
    // Orientation
    // ------------------------------------------------------------------

    /// Layout direction for [`create_box`].
    ///
    /// The discriminants match Android's `LinearLayout` constants
    /// (`HORIZONTAL = 0`, `VERTICAL = 1`) as well as `NSLayoutConstraint`'s
    /// axis convention (0 = horizontal, 1 = vertical), so the shared code can
    /// keep passing the same integers on both platforms.
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

    /// Alias required by `common.rs` (`Orientation as IosOrientation`).
    pub type IosOrientation = Orientation;

    // ------------------------------------------------------------------
    // Window
    // ------------------------------------------------------------------
    //
    // iOS has no toplevel window owned by the toolkit user: the app owns a
    // `UIWindow` and a root view controller, and the backend is handed the
    // controller's view. `Window` is therefore the *root view* handle; all
    // window-ish methods are no-ops (set_title/set_default_size) because the
    // scene owns those, exactly as on Android.

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

        /// Force the window to a size, points.
        ///
        /// The `Window` handle here is the host's root *view*: iOS has no
        /// toplevel window the toolkit owns, the scene does (see the type's
        /// comment). A view's frame is set with `setFrame:`, and the argument
        /// order is the one that matters — `msg4cv` sends
        /// `(CGFloat, CGFloat, CGFloat, CGFloat)` after `(self, SEL)`, which
        /// is `CGRect`'s field order, so origin first and size second.
        pub fn resize(&self, w: i32, h: i32) {
            if self.0.is_null() {
                return;
            }
            // The origin is left alone: the view is positioned by its
            // superview, and moving it here would fight the stack view that
            // owns it. A zero origin is the right neutral value for a root.
            unsafe { msg4cv(self.0, "setFrame:", 0.0, 0.0, w as f64, h as f64) };
            // A frame change is not a redraw: without this the resize lands
            // and the canvas keeps painting the old size until something else
            // asks for a frame.
            unsafe { msg0v(self.0, "setNeedsLayout") };
            unsafe { msg0v(self.0, "setNeedsDisplay") };
        }

        /// # Safety
        /// Kept for API compatibility with the GTK backend; no-op on iOS.
        pub unsafe fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}

        pub fn hwnd(&self) -> *mut c_void {
            std::ptr::null_mut()
        }

        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            // The one attachment that decides whether ANYTHING is visible: the
            // tree's root goes onto the host's view. Logged because its silent
            // failure mode is a running app with a blank screen.
            core_ios::log_ios("Window::set_child (attaching the tree to the host view)");
            attach(self.0, *child.as_ref());
            let n = if self.0.is_null() {
                -1
            } else {
                unsafe { msg0i(self.0, "retainCount") }
            };
            core_ios::log_ios(&format!("Window::set_child done (root retainCount={n})"));
        }

        pub fn set_child_box(&self, bx: &BoxWidget) {
            self.set_child(bx);
        }

        pub fn present(&self) {
            // The scene is already visible; make sure AppKit/UIKit is told to
            // lay out and draw what we added.
            if !self.0.is_null() {
                unsafe {
                    msg0v(self.0, "setNeedsLayout");
                    msg0v(self.0, "setNeedsDisplay");
                }
            }
        }

        pub fn queue_redraw(&self) {
            if !self.0.is_null() {
                unsafe { msg0v(self.0, "setNeedsDisplay") };
            }
        }

        pub fn on_event(&self, _cb: Box<dyn FnMut(*mut c_void) -> i32>) {}
        pub fn on_event_key(&self, _cb: Box<dyn FnMut(u32, u32) -> i32>) {}
        pub fn on_close(&self, _cb: Box<dyn FnMut()>) {}

        /// Screen scale (1.0/2.0/3.0), the iOS analogue of Android's display
        /// density. `None` when uninitialised, so callers fall back to 1.0.
        pub fn display_scale(&self) -> Option<f64> {
            core_ios::display_scale()
        }
    }

    // ------------------------------------------------------------------
    // Button
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
        /// Register a click handler. UIKit targets are (id, SEL, id): the
        /// host shim (`RustTarget` in the app target) receives our callback
        /// id and calls back into the cdylib's `corro_ios_callback`.
        ///
        /// The same registry as Android's `RustCallback.java`, so a single
        /// dispatch path exists on both platforms.
        pub fn on_click(&self, f: impl FnMut() + Send + 'static) -> Result<u64, Error> {
            let id = core_ios::register_callback(Box::new(f));
            self.attach_target(id);
            Ok(id)
        }

        /// Ask the host shim to wire `id` to `UIControlEventTouchUpInside`
        /// (`addTarget:action:forControlEvents:`, iOS 2.0+).
        fn attach_target(&self, id: u64) {
            if self.0.is_null() {
                return;
            }
            // The shim is a small ObjC class the host ships; resolving it by
            // name keeps UIKit entirely out of Rust's dependency list, and a
            // host without the shim still gets a working (unclickable) button
            // rather than a crash.
            let shim = cls("CorroIosTarget");
            if shim.is_null() {
                core_ios::log_ios("ios: CorroIosTarget shim missing; button inert");
                return;
            }
            // SAFETY: `shim` is the CorroIosTarget class object and
            // `targetWithCallbackId:` is its declared factory selector, so
            // the id-returning signature below is the real one.
            let target = unsafe { msg1i(shim, "targetWithCallbackId:", id as isize) };
            if target.is_null() {
                return;
            }
            let target = own(target);
            // `addTarget:action:forControlEvents:` — id, SEL, id, SEL,
            // NSUInteger. The action selector is looked up by name (the shim's
            // `corroFired:`), so the Rust side never hardcodes an IMP.
            raw_send!(
                self.0,
                "addTarget:action:forControlEvents:",
                unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                    *mut c_void,
                    u64,
                ) -> (),
                (
                    target,
                    core_ios::selector("corroFired:"),
                    // UIControlEventTouchUpInside == 1 << 6
                    1u64 << 6
                )
            );
        }

        /// Simulate a tap (`sendActionsForControlEvents:`), for tests and for
        /// scripted hosts. Returns the callback id (0 semantics as Android).
        pub fn emit_clicked(&self) -> Result<u64, Error> {
            if !self.0.is_null() {
                unsafe { msg1iv(self.0, "sendActionsForControlEvents:", 1 << 6) };
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
    // Label
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
            unsafe { msg1v(self.0, "setText:", s) };
            core_ios::with_meta_mut(self.0, |m| m.text = text.to_owned());
        }

        pub fn get_text(&self) -> Option<String> {
            if self.0.is_null() {
                return None;
            }
            // Prefer the Rust-side copy: it is authoritative for text we set,
            // and avoids an ObjC round trip for chrome labels read every frame.
            if let Some(text) = core_ios::with_meta(self.0, |m| m.text.clone()) {
                if !text.is_empty() {
                    return Some(text);
                }
            }
            let s = unsafe { msg0(self.0, "text") };
            nsstring_to_rust(s)
        }

        pub fn set_visible(&self, visible: bool) {
            if self.0.is_null() {
                return;
            }
            unsafe { msg1bv(self.0, "setHidden:", !visible) };
        }

        /// `setMarkup` in the shared API. `UILabel` is plain text, so the
        /// markup is flattened to its text content (Pango markup on GTK,
        /// nothing at all on Android). Keeping the tag-stripping local means
        /// callers can pass the same string everywhere.
        pub fn set_markup(&self, markup: &str) {
            self.set_text(&strip_markup(markup));
        }

        /// Pin the label's width so changing its text never reflows the
        /// siblings packed after it. `None` releases the pin.
        ///
        /// The AppKit twin of this method is the reference for *why* a pin is
        /// more than a width request; the mechanism is the same on UIKit: an
        /// `NSLayoutConstraint` alone does not beat
        /// `-intrinsicContentSize` (content-hugging 250), so the hugging
        /// priority has to go up as well or the label stays shrink-to-fit.
        /// `corroSetPinnedWidth:` (host shim) installs the required width
        /// equality constraint; a negative width releases it.
        ///
        /// iOS is a touch platform, so the formula bar is laid out a little
        /// differently than on the desktop, but the address slot still must
        /// not resize as the address changes — hence the same pin.
        pub fn set_fixed_width(&self, w: Option<i32>) {
            core_ios::with_meta_mut(self.0, |m| m.fixed_width = w);
            if self.0.is_null() {
                return;
            }
            core_ios::log_ios("set_fixed_width: sending corroSetPinnedWidth:");
            unsafe {
                msg1iv(self.0, "corroSetPinnedWidth:", w.unwrap_or(-1) as isize);
            }
            core_ios::log_ios("set_fixed_width: corroSetPinnedWidth ok");
            unsafe {
                msg1iv(
                    self.0,
                    "corroSetContentHugging:",
                    if w.is_some() { 1000 } else { 250 },
                );
            }
            core_ios::log_ios("set_fixed_width: corroSetContentHugging ok");
            unsafe { msg0v(self.0, "setNeedsLayout") };
        }

        /// Left margin of the label's contents, in points. Pairs with
        /// [`Label::set_fixed_width`]: a pinned, left-aligned label sits flush
        /// against its slot's edge, and this restores the inset the
        /// shrink-to-fit label had.
        ///
        /// The macOS twin sends `setHeadIndent:`, which is an `NSTextField`
        /// method. **A `UILabel` has no such selector**, so the send raised
        /// `unrecognized selector` - a foreign throw, uncatchable, and the
        /// next abort in this same startup path. That was drift from
        /// AppKit's text model, not a missing implementation.
        ///
        /// UIKit has no plain-text inset on `UILabel`: the equivalent is the
        /// `UIEdgeInsets` on `attributedText`, which needs an attributed
        /// string, and that in turn means *behaviour* the host must
        /// implement - the same trap `corroBoundsWidth` documents. So the
        /// value is recorded and the text left alone.
        ///
        /// The inset is cosmetic and the slot is already pinned to its widest
        /// text, so losing it costs a few points of left padding on one label;
        /// sending a selector the object does not have costs the whole app.
        pub fn set_margin_start(&self, px: i32) {
            core_ios::with_meta_mut(self.0, |m| m.margin_start = px);
        }

        /// Top inset of the label's contents, in points.
        ///
        /// The counterpart to [`Label::set_margin_start`]. There is no
        /// `UILabel` selector for a vertical inset — not even the AppKit one
        /// `set_margin_start` cannot use — so this is recorded and not sent,
        /// for the same reason and with the same justification: see that
        /// method's comment. The value is in `WidgetMeta`, so a backend that
        /// can honour it has it.
        pub fn set_margin_top(&self, px: i32) {
            core_ios::with_meta_mut(self.0, |m| m.margin_top = px);
        }

        /// Set the x alignment of the label's text (0.0 left .. 1.0 right).
        ///
        /// `UILabel` centres its text by default, so with a pinned width the
        /// text would float mid-slot and drift as it changes width — the same
        /// reflow the pin exists to stop, relocated to the glyphs. The
        /// 0.0..1.0 range is mapped onto the three real anchors by nearest
        /// third (the same mapping NWG uses for `SS_CENTER`/`SS_RIGHT`).
        pub fn set_xalign(&self, x: f32) {
            core_ios::with_meta_mut(self.0, |m| m.xalign = x);
            if self.0.is_null() {
                return;
            }
            // NSTextAlignment scale: 0 left, 1 centre, 2 right (UIKit uses
            // the same integer constants).
            let align: isize = if x < 1.0 / 3.0 {
                0
            } else if x < 2.0 / 3.0 {
                1
            } else {
                2
            };
            unsafe { msg1iv(self.0, "setTextAlignment:", align) };
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
    fn strip_markup(markup: &str) -> String {
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
    // Box (UIStackView-backed layout container)
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
        /// `UIStackView` (iOS 9+) is the natural equivalent of Android's
        /// `LinearLayout`: it owns spacing and distribution. On iOS 7/8 the
        /// host's `SheetView` shim provides `corroAddArrangedSubview:axis:`
        /// (a manual frame-based layout), so this call site is identical on
        /// both paths — the class of `self.0` decides.
        pub fn append(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            if self.0.is_null() || child_ptr.is_null() {
                return;
            }
            let is_canvas = core_ios::is_canvas_view(child_ptr);
            let expands = core_ios::is_view_expanding(child_ptr);
            unsafe {
                // `addArrangedSubview:` exists on UIStackView (iOS 9+); on the
                // manual-layout shim the same selector is implemented by the
                // host view. Both are "append, then distribute".
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
            let min_chars = core_ios::view_min_chars(child_ptr);
            if min_chars > 0 {
                let w = min_chars as f64 * EST_CHAR_W;
                unsafe { msg1iv(child_ptr, "corroSetMinWidth:", w as isize) };
            }
        }

        /// Android records these and lets `append` apply them as weight;
        /// iOS does the same, so a call before `append` is not lost.
        pub fn set_child_hexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
            core_ios::set_view_expanding(*child.as_ref(), expand);
        }

        pub fn set_child_vexpand(&self, child: &impl AsRef<*mut c_void>, _expand: bool) {
            // UIStackView distributes along its axis; a vertical box's
            // expansion *is* the axis, so the per-child flag collapses into
            // the same `corroSetFlex:` call made by `append`.
            core_ios::set_view_expanding(*child.as_ref(), true);
        }

        pub fn set_hexpand(&self, expand: bool) {
            core_ios::set_view_expanding(self.0, expand);
        }

        /// Size the stack view, in points.
        ///
        /// The `common::WidgetBox` contract: "minimum size of the box
        /// itself", needed for a horizontal row so a button child cannot be
        /// laid out taller than the strip (GTK's own note: consecutive rows
        /// overlap and render as garbled text).
        ///
        /// The AppKit twin sends `setFrameSize:`. UIKit's `setFrame:` takes a
        /// `CGRect` — *four* `CGFloat`s, origin first — so the argument order
        /// matters and is the reason this is not a shared helper with the
        /// macOS adapter. The origin is left at zero: the stack view is
        /// positioned by its superview, and moving it here would fight the
        /// autoresizing mask `attach` installed.
        ///
        /// A negative request means "unconstrained" on the GTK contract, so
        /// it is ignored rather than turned into a negative frame.
        pub fn set_size_request(&self, w: i32, h: i32) {
            if self.0.is_null() {
                return;
            }
            if w > 0 && h > 0 {
                unsafe { msg4cv(self.0, "setFrame:", 0.0, 0.0, w as f64, h as f64) };
                unsafe { msg0v(self.0, "setNeedsLayout") };
            }
        }
    }

    // ------------------------------------------------------------------
    // Entry (UITextField)
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
            core_ios::set_view_expanding(self.0, expand);
        }

        pub fn set_vexpand(&self, _expand: bool) {}

        pub fn set_width_chars(&self, n: i32) {
            core_ios::set_view_min_chars(self.0, n);
        }

        pub fn set_text(&self, text: &str) {
            if self.0.is_null() {
                return;
            }
            let s = nsstring(text);
            if s.is_null() {
                return;
            }
            unsafe {
                msg1v(self.0, "setText:", s);
                // Caret to the end, matching GTK's set_text behaviour and the
                // Android adapter's `setSelection(length)`.
                let len = core_ios::with_meta(self.0, |m| m.text.len()).unwrap_or(text.len());
                let _ = len;
            }
            core_ios::with_meta_mut(self.0, |m| m.text = text.to_owned());
        }

        pub fn get_text(&self) -> Option<String> {
            if self.0.is_null() {
                return None;
            }
            let s = unsafe { msg0(self.0, "text") };
            let text = nsstring_to_rust(s);
            if let Some(t) = &text {
                core_ios::with_meta_mut(self.0, |m| m.text = t.clone());
            }
            text
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            core_ios::with_meta_mut(self.0, |m| m.size_request = (w.max(1), h.max(1)));
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

        /// `GtkEntry::activate` / IME "Done": the host's text-field delegate
        /// calls `corro_ios_entry_activate(handle)`, which lands in
        /// [`dispatch_entry_activate`].
        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(
            &self,
            f: F,
        ) -> Result<u64, Error> {
            let handle = self.0 as usize as u64;
            let id = register_ptr_callback(&ACTIVATE_CALLBACKS, handle, f);
            Ok(id)
        }

        /// Focus-in/out events: `UITextFieldDelegate`'s
        /// `textFieldDidBeginEditing:` / `textFieldDidEndEditing:`.
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
            if !self.0.is_null() {
                unsafe { msg0v(self.0, "becomeFirstResponder") };
            }
        }

        /// Caret position as a character index.
        ///
        /// `UITextField` has no direct caret query; the selected range lives
        /// on the field's `UITextRange`/`selectedTextRange`. Returning `None`
        /// is the documented "backend cannot report one" answer, and callers
        /// then keep the caret they track themselves — which is what the
        /// desktop GTK3 build does too.
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
            unsafe { msg0i(self.0, "isFirstResponder") != 0 }
        }

        pub fn connect_button_press(&self, _f: impl FnMut() + 'static) -> Result<u64, Error> {
            // Touch handling on the entry is the host's business (UIKit
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
    /// raise. Android keys on the view pointer as well
    /// (`dispatch_text_changed(view_ptr)`), so the two stay symmetrical.
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

    /// Called from the host's `CorroTextWatcher` equivalent (`UITextField`
    /// `editingChanged`): the *only* signal soft-keyboard typing produces,
    /// exactly as on Android. Runs corro's `on_formula_entry_changed`.
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

    /// Called from the host on IME Done/Return (`textFieldShouldReturn:`):
    /// commits the edit and moves down, like a hardware Return.
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
    /// side can decide whether to continue into UIKit's default behaviour.
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
    // Canvas rendering mirrors Android exactly: the host view's `drawRect:`
    // hands us a live `CGContextRef` and the view's pixel size; the Rust
    // closure replays its primitives against a context that forwards each
    // one to Core Graphics. Until that view exists (host runs, tests) the
    // closure still runs against an estimating context so `render_to` logic
    // stays testable.

    /// `DrawContext` used when no UIKit canvas is attached: records nothing,
    /// measures with the monospace estimate.
    pub struct IosDrawContext;

    impl DrawContext for IosDrawContext {
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
        /// This canvas's backend id. Exposed so a host can tell which canvas
        /// a platform gesture belongs to (the sheet and the sheet-tab strip are
        /// both canvases, and a drag means different things on each).
        pub fn canvas_id(&self) -> u64 {
            core_ios::canvas_id_for_view(self.0)
        }

        pub fn set_draw_callback(&self, cb: Box<dyn FnMut(&mut dyn DrawContext, i32, i32)>) {
            // Stash the closure under our canvas id; the host SheetView
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
            // callers observe draws without UIKit — same double duty as the
            // Android adapter.
            if !self.0.is_null() {
                unsafe {
                    msg0v(self.0, "setNeedsDisplay");
                }
            }
            // Re-entrancy guard.
            //
            // The draw closure is APPLICATION code: `render_grid` repaints
            // chrome, and any of it may ask for another redraw (a pinned
            // scrollbar value, a changed label, the tab bar's own state). That
            // re-entry arrives here while `DRAW_CALLBACKS` is still locked -
            // the lock is a plain `std::sync::Mutex`, which is not reentrant -
            // so the second call waits for a lock its own caller holds and the
            // main thread stops forever. No crash, no exception, and a
            // screenshot that still looks healthy, which is exactly the iOS CI
            // signature.
            //
            // A nested redraw is redundant by construction: the outer call is
            // already replaying the same closure against the same state, and
            // the `setNeedsDisplay` above has asked UIKit for a frame anyway.
            // So the inner call is simply not run.
            //
            // A thread-local rather than a field: dispatch is single-threaded
            // (UIKit callbacks and the tick all run on the main thread), and a
            // field would be shared state that has to be reset on an unwind.
            IN_DRAW.with(|d| {
                if d.get() {
                    return;
                }
                d.set(true);
            });
            // `guard` restores the flag on every exit path, including a panic
            // out of the closure, so one failed draw cannot wedge the canvas.
            struct Guard;
            impl Drop for Guard {
                fn drop(&mut self) {
                    IN_DRAW.with(|d| d.set(false));
                }
            }
            let _guard = Guard;
            let mut map = DRAW_CALLBACKS.lock().unwrap();
            if let Some(SendDrawCallback(ptr)) = map.get_mut(&self.canvas_id()) {
                // SAFETY: registry-owned closure, single-threaded dispatch.
                let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) = unsafe { &mut **ptr };
                let mut dc = IosDrawContext;
                let (w, h) = self.replay_size();
                cb(&mut dc, w, h);
            }
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            core_ios::with_meta_mut(self.0, |m| m.size_request = (w.max(1), h.max(1)));
        }

        /// Size to replay the draw closure at: the real laid-out size once
        /// the host view's draw has reported one, else the requested size
        /// (UIKit has not laid the view out yet), else a conservative
        /// default. Mirrors the Android `CANVAS_SIZE` / `CANVAS_SIZE_REQUEST`
        /// split, including the reason for the split: a 1x1 placeholder
        /// request must never shrink a real laid-out size to one row.
        fn replay_size(&self) -> (i32, i32) {
            // Prefer the size the host actually laid the view out to, which
            // `dispatch_draw` records by canvas id. Consulting the id map here
            // (rather than relying on `set_canvas_laid_out` to have copied it
            // onto the handle) is deliberate: that copy was never called, so
            // the 1x1 placeholder below was always winning and the sheet
            // replayed at one pixel - `DRAW_CALLBACK called: w=1 h=1` in the
            // simulator log, and a sheet that could not render.
            let canvas_id = core_ios::canvas_id_for_view(self.0);
            if let Some((w, h)) = canvas_laid_out(canvas_id) {
                if w > 0 && h > 0 {
                    return (w, h);
                }
            }
            // Then any laid-out size copied onto the handle, then what Rust
            // asked for.
            let (laid_out, requested) = core_ios::with_meta(self.0, |m| (m.laid_out, m.size_request))
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

        /// Button/modifier-aware click. See the GTK backend's
        /// `on_click_button`: added alongside `on_click` so the signature change
        /// does not ripple through every backend, and so a backend that cannot
        /// report a button (terminal, mobile) stays compilable.
        pub fn on_click_button(&self, _cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            // iOS/UIKit: touch has no button number; only press is reported.
        }

        /// Pointer motion over the canvas; see the GTK backend's `on_motion`.
        pub fn on_motion(&self, _cb: Box<dyn FnMut(f64, f64, u32)>) {
            // iOS/UIKit: touch has no button number; only press is reported.
        }

        /// This canvas's top-left in screen coordinates, or `None` when the
        /// backend cannot report one. Callers then open a context menu
        /// unpositioned rather than guessing. See the GTK backend's
        /// `screen_origin`.
        pub fn screen_origin(&self) -> Option<(i32, i32)> {
            None
        }

        /// Pointer release; see the GTK backend's `on_release`.
        pub fn on_release(&self, _cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            // iOS/UIKit: touch has no button number; only press is reported.
        }

        pub fn on_click(&self, cb: Box<dyn FnMut(f64, f64)>) {
            let mut map = CLICK_CALLBACKS.lock().unwrap();
            map.insert(self.canvas_id(), SendClickCallback(Box::into_raw(cb)));
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
        /// [`Canvas::queue_redraw`] (plus a `setNeedsDisplay`).
        pub fn force_draw(&self, _window_ptr: *mut c_void, fallback_w: i32, fallback_h: i32) {
            if !self.0.is_null() {
                unsafe { msg0v(self.0, "setNeedsDisplay") };
            }
            let mut map = DRAW_CALLBACKS.lock().unwrap();
            if let Some(SendDrawCallback(ptr)) = map.get_mut(&self.canvas_id()) {
                // SAFETY: registry-owned closure, single-threaded dispatch.
                let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) = unsafe { &mut **ptr };
                let mut dc = IosDrawContext;
                let (w, h) = {
                    let (laid_out, _) =
                        core_ios::with_meta(self.0, |m| (m.laid_out, m.size_request)).unwrap_or(((0, 0), (0, 0)));
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

    thread_local! {
        /// True while a draw closure is running, per thread. See
        /// [`Canvas::queue_redraw`], which uses it to refuse a re-entrant
        /// redraw rather than deadlock on `DRAW_CALLBACKS`.
        static IN_DRAW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    static DRAW_CALLBACKS: Lazy<Mutex<HashMap<u64, SendDrawCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    static CLICK_CALLBACKS: Lazy<Mutex<HashMap<u64, SendClickCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    static KEY_CALLBACKS: Lazy<Mutex<HashMap<u64, SendKeyCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    struct SendDrawCallback(*mut dyn FnMut(&mut dyn DrawContext, i32, i32));
    // SAFETY: guarded by the registry mutex; same pattern as the Android and
    // wasm adapters.
    unsafe impl Send for SendDrawCallback {}
    struct SendClickCallback(*mut dyn FnMut(f64, f64));
    unsafe impl Send for SendClickCallback {}
    struct SendKeyCallback(*mut dyn FnMut(u32, u32) -> bool);
    unsafe impl Send for SendKeyCallback {}

    /// Dispatch a tap from the host `SheetView` to the registered click
    /// closure. `x`/`y` are in points, the same coordinate space the draw
    /// closure paints in.
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

    /// Dispatch a key from the host to the registered key closure (hardware
    /// keyboards, `simctl` input, the iOS 13+ keyboard accessory bar).
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
    /// Core Graphics context. Called from the cdylib's
    /// `corro_ios_canvas_draw` export, which the host `SheetView` invokes
    /// from `drawRect:` on the main thread. `w`/`h` are the view's size in
    /// points.
    ///
    /// # Safety
    /// `ctx` must be a live `CGContextRef` valid for the duration of the
    /// call (it is the one UIKit passes to `drawRect:`).
    pub unsafe fn dispatch_draw(canvas_id: u64, ctx: *mut c_void, w: i32, h: i32) {
        let (w, h) = (w.max(1), h.max(1));
        // Record the live size so `replay_size` stops guessing. Handles are
        // keyed by canvas id here (Android keys the same map by id).
        record_canvas_size(canvas_id, w, h);
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
                let mut est = IosDrawContext;
                // SAFETY: as above.
                let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) = unsafe { &mut *raw };
                cb(&mut est, w, h);
            }
        }
    }

    /// The view's *laid-out* size in points, learned from the host. Kept so
    /// `replay_size` can answer without the host.
    static CANVAS_SIZE: Lazy<Mutex<HashMap<u64, (i32, i32)>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    /// Record a laid-out canvas size, called by the host from
    /// `layoutSubviews` (and by [`dispatch_draw`], which also knows it).
    ///
    /// Separate from the draw path on purpose: Rust replays the draw closure
    /// before UIKit's first `drawRect:`, so a size that only arrived with the
    /// first frame would come too late and the sheet would lay itself out at
    /// the 1x1 placeholder.
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

    // ------------------------------------------------------------------
    // Core Graphics draw context
    // ------------------------------------------------------------------
    //
    // DrawContext's primitives map one-to-one onto CG calls, so the replay
    // model is the same as Android's Canvas path. Core Graphics is chosen
    // over a GPU path deliberately: it is the smallest step from the existing
    // `DrawContext` contract, it is available on every iOS version back to
    // 2.0, and it needs no shader pipeline. See IOS_GUIDELINES.md §5.

    // `kind = "framework"` — CoreGraphics is an umbrella framework, not a
    // `libCoreGraphics.dylib`, so the link flag must be `-framework
    // CoreGraphics` and not the `-lCoreGraphics` a bare `#[link(name=...)]`
    // produces. The plain spelling happens to go unnoticed here because a
    // staticlib is archived rather than linked (the macOS host crate also
    // builds a cdylib, which *is* linked, and CI caught it there).
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

    /// `CGRect`. Field order is ABI-critical: `CGFloat` (f64 on 64-bit,
    /// f32 on 32-bit), then two floats per origin/size pair.
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
    /// delegated to the host through two ObjC shims (`CorroIosText`), which
    /// the app target implements with `NSString`/`UIFont` APIs available on
    /// iOS 2.0+. A missing shim degrades to the estimate, never a panic.
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

    /// Text measurement through the host's `CorroIosText` shim.
    ///
    /// The shim is a tiny ObjC class the app target ships (it can use
    /// `UIFont`, `NSString` and — on iOS 7+ — `boundingRectWithSize:...`;
    /// on iOS 6 and earlier it uses `sizeWithFont:`). Keeping it in the host
    /// is what lets one Rust binary run on both without link-time SDK
    /// decisions; see IOS_GUIDELINES.md §3.
    fn measure_text_via_host(
        text: &str,
        font: &str,
        size: f64,
        slant: i32,
        weight: i32,
    ) -> Option<(f64, f64, f64, f64)> {
        if !core_ios::is_initialized() {
            return None;
        }
        if text.is_empty() {
            return Some((0.0, 0.0, 0.0, 0.0));
        }
        let shim = cls("CorroIosText");
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
        // wrong on 32-bit. The host frees its own buffer contract by handing
        // ownership over; we free it with the same C allocator (`free`).
        // The selector has FIVE colon-separated parts, so it takes five
        // arguments: text, font, size, slant, weight. This call used to pass
        // only four - `ns_font` was computed, null-checked and then never
        // handed over - so the ObjC side read the `font:` parameter out of a
        // register the caller had never set. Found by generating the ObjC
        // declarations from the adapters' own signatures: they disagreed.
        let rect_ptr = raw_send!(
            shim,
            "measure:font:size:slant:weight:",
            unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *mut c_void,
                *mut c_void,
                f64,
                i32,
                i32,
            ) -> *mut CGRect,
            (ns_text, ns_font, size, slant, weight)
        );
        if rect_ptr.is_null() {
            return None;
        }
        let rect = unsafe { *rect_ptr };
        unsafe { free(rect_ptr as *mut c_void) };
        Some((0.0, 0.0, rect.width.max(0.0), rect.height.max(0.0)))
    }

    /// Text drawing through the host's `CorroIosText` shim, which resolves a
    /// `UIFont` for the family/size/weight and draws with the UIKit/CoreText
    /// API appropriate to the deployment target.
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
        if !core_ios::is_initialized() {
            return;
        }
        let shim = cls("CorroIosText");
        if shim.is_null() {
            core_ios::log_ios("ios: CorroIosText shim missing; text not drawn");
            return;
        }
        let ns_text = nsstring(text);
        let ns_font = nsstring(font);
        if ns_text.is_null() || ns_font.is_null() {
            return;
        }
        // The shim draws via `NSString`/`UIFont`/CoreText for the deployment
        // target's SDK. `ctx` is the live `CGContextRef` from `drawRect:`;
        // `font` is the family name (the shim resolves a `UIFont` and falls
        // back to the system font). Signature, after the implicit (id, SEL)
        // pair:
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

    // `CorroIosText` returns a buffer allocated with `malloc` (the host's
    // `calloc`), so the matching free is `free` from libc.
    unsafe extern "C" {
        fn free(ptr: *mut c_void);
    }

    // ------------------------------------------------------------------
    // Menu / MenuBar / SimpleAction
    // ------------------------------------------------------------------
    //
    // iOS has no desktop menu bar either. The shared code builds the *model*
    // (labels + action names); the host app renders it as a `UIMenu` on the
    // navigation bar / an overflow button and dispatches `app.*` names back,
    // which is the Android menu-strip story with UIKit spelling.

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
        /// Only the GTK backend can do this (its `GtkMenu` takes a position
        /// callback); elsewhere the menu system has no programmatic
        /// popup-at-position call, so this reports `false` instead of
        /// pretending. The caller surfaces that as an "unavailable" status.
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
        /// Kept for API compatibility; no-op on iOS.
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
    // SAFETY: guarded by the registry mutex; same pattern as the Android and
    // wasm adapters.
    unsafe impl Send for SendFnPtr {}

    static ACTION_REGISTRY: Lazy<Mutex<HashMap<String, SendFnPtr>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    fn register_action(name: &str, f: Box<dyn FnMut(*mut c_void)>) {
        let mut map = ACTION_REGISTRY.lock().unwrap();
        let ptr = Box::into_raw(f);
        map.insert(name.to_owned(), SendFnPtr(ptr));
    }

    /// Dispatch a menu action by name (called from the host's menu shim
    /// after the user picks a `UIMenu`/overflow item).
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
    // DropDown (UIButton + UIMenu on iOS 14+, action sheet before that)
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
                // The host shim owns the item list; the index is pushed as a
                // button `tag` so the shim can render the right title.
                unsafe { msg1iv(self.0, "corroSetSelectedIndex:", index.unwrap_or(0) as isize) };
            }
        }

        pub fn get_active(&self) -> i32 {
            if self.0.is_null() {
                return -1;
            }
            unsafe { msg0i(self.0, "corroSelectedIndex") as i32 }
        }

        pub fn connect_changed(&self, _f: impl FnMut() + 'static) -> Result<u64, Error> {
            Ok(0) // Wired through the host shim's own picker delegate.
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
    // CheckButton / RadioButton (UISwitch / UISegmentedControl)
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
            // UISwitch: `isOn`. A UISwitch is a UIControl, so `on` is the
            // right accessor on every iOS version that has the class (5.0+).
            unsafe { msg0i(self.0, "isOn") != 0 }
        }

        pub fn set_active(&self, active: bool) {
            if !self.0.is_null() {
                unsafe { msg1iv(self.0, "setOn:", active as isize) };
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
            unsafe { msg0i(self.0, "isSelected") != 0 }
        }

        pub fn set_active(&self, active: bool) {
            if !self.0.is_null() {
                unsafe { msg1iv(self.0, "setSelected:", active as isize) };
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
    // Dialog (UIAlertController on iOS 8+, UIAlertView before that)
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
                unsafe { msg1v(self.0, "setTitle:", s) };
            }
        }

        pub fn set_default_size(&self, _w: i32, _h: i32) {}

        /// Parent this dialog to `parent`. No-op on iOS (the presenting view
        /// controller already parents presented sheets).
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
            // `corroAddAction:` is implemented by the host's alert shim so
            // one Rust call covers both UIAlertController (iOS 8+) and
            // UIAlertView (iOS 7).
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
            let Some(vc) = core_ios::view_controller() else {
                core_ios::log_ios("ios: no view controller; dialog not presented");
                return;
            };
            // Host-side `corroPresentDialog:` picks
            // `presentViewController:animated:completion:` (iOS 5+) and wraps
            // a UIAlertView on the older path.
            raw_send!(
                vc,
                "corroPresentDialog:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> (),
                (self.0)
            );
        }

        pub fn connect_response(&self, _f: impl FnMut(i32) + 'static) -> Result<u64, Error> {
            Ok(0) // Wired through the host shim's alert delegate.
        }

        pub fn set_default_response(&self, _response_id: i32) {}
        pub fn close(&self) {}

        /// Measure the alert before it opens.
        ///
        /// A `UIAlertController`/`UIAlertView` sizes itself from its title
        /// and its actions, so there is no separate layout pass to run —
        /// presenting already lays it out. Kept because the shared
        /// `common::Dialog` forwards it and a caller written for GTK/NWG
        /// calls it unconditionally; an absent method is a *compile* error
        /// there, which is a worse outcome than a documented no-op.
        pub fn layout_dialog(&self) {}

        /// Show or hide without dismissing.
        ///
        /// Distinct from `close`, which is "destroy the dialog": an
        /// iOS alert cannot be re-presented after its presenting view
        /// controller has torn it down, so `close` is a dismissal and this
        /// is a presentation — the same pair of operations GTK's
        /// `set_visible`/`destroy` are, and the same reason the macOS twin
        /// documents them separately.
        pub fn set_visible(&self, visible: bool) {
            if visible {
                self.present();
            } else {
                self.close();
            }
        }

        /// The container a dialog's children are added to.
        ///
        /// `UIAlertController` has no content-area view of its own — the
        /// host shim builds and owns one — so there is nothing to hand back,
        /// and this returns null rather than a dangling id. Exactly the
        /// answer the macOS `NSAlert` twin gives, and the same answer the
        /// terminal backends give for a dialog that is drawn as text.
        pub fn get_content_area(&self) -> *mut c_void {
            std::ptr::null_mut()
        }

        /// Present the dialog and return the last response id (0 if none).
        ///
        /// `Dialog::run` is a *synchronous* nested loop on GTK, NWG and
        /// macOS. There is no nested loop here: UIKit owns the run loop, the
        /// answer arrives from the host shim's alert delegate long after this
        /// returns, and blocking would deadlock the main thread the delegate
        /// has to run on. So this presents and returns 0 — not a valid GTK
        /// response id (those start at 1), so a caller gets an unambiguous
        /// "none yet" rather than a plausible wrong answer. A caller that
        /// needs the answer must use `connect_response`, which is the correct
        /// shape on a touch platform anyway. The macOS adapter is the
        /// contrast: it *can* `runModal`, and says why it differs.
        pub fn run(&self) -> i32 {
            self.present();
            0
        }
    }

    // ------------------------------------------------------------------
    // TextView (UITextView)
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
                unsafe { msg1v(self.0, "setText:", s) };
            }
            core_ios::with_meta_mut(self.0, |m| m.text = text.to_owned());
        }

        pub fn get_text(&self) -> Option<String> {
            if self.0.is_null() {
                return None;
            }
            let s = unsafe { msg0(self.0, "text") };
            nsstring_to_rust(s)
        }

        pub fn set_wrap_mode(&self, _wrap_mode: i32) {}
        pub fn set_size_request(&self, _w: i32, _h: i32) {}
        pub fn set_hexpand(&self, _expand: bool) {}
        pub fn set_vexpand(&self, _expand: bool) {}
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
    // iOS scroll views exist (UIScrollView), but corro drives its own
    // viewport (the draw closure paints the visible window and the scrollbar
    // chrome itself), so these are inert containers — exactly the Android
    // decision (`ScrollView` there is never allowed to scroll). Keeping them
    // inert means one viewport model across every backend.

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
        /// parent-child edge iOS needs: scrolled window -> canvas).
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

    /// The window handle is the host's root view: iOS has no toplevel window
    /// the toolkit user creates (see the `Window` comment).
    pub fn create_window() -> Result<Window, Error> {
        match root() {
            Some(view) => Ok(Window(view)),
            None => Ok(Window(std::ptr::null_mut())),
        }
    }

    pub fn create_button(label: &str) -> Result<Button, Error> {
        // `UIButton buttonWithType:UIButtonTypeSystem` (type 1, iOS 7+); on
        // iOS 6 and earlier the host shim maps this to UIButtonTypeRoundedRect
        // via `corroConfigureButton:`.
        let class = cls("UIButton");
        let btn = if class.is_null() {
            std::ptr::null_mut()
        } else {
            unsafe { msg1i(class, "buttonWithType:", 1) }
        };
        if btn.is_null() {
            return Ok(Button(new_widget("UIButton", Kind::Button)));
        }
        let btn = own(btn);
        core_ios::register_widget(
            btn,
            WidgetMeta {
                kind: Kind::Button,
                text: label.to_owned(),
                ..Default::default()
            },
        );
        let title = nsstring(label);
        if !title.is_null() {
            // UIControlStateNormal == 0. Two arguments, so `msg1vi` -
            // `msg1v` would leave `forState:` reading a stale register.
            unsafe { msg1vi(btn, "setTitle:forState:", title, 0) };
        }
        // Minimum touch target, in points.
        unsafe { set_frame(btn, 0.0, 0.0, 0.0, MIN_TOUCH_PT) };
        Ok(Button(btn))
    }

    pub fn create_label(text: &str) -> Result<Label, Error> {
        let lbl = new_widget("UILabel", Kind::Label);
        if lbl.is_null() {
            return Ok(Label(lbl));
        }
        core_ios::log_ios("create_label: constructed UILabel");
        core_ios::with_meta_mut(lbl, |m| m.text = text.to_owned());
        let s = nsstring(text);
        if !s.is_null() {
            unsafe { msg1v(lbl, "setText:", s) };
        }
        core_ios::log_ios("create_label: setText ok");
        // `adjustsFontSizeToFitWidth` keeps a long status line inside the
        // screen instead of clipping it (a phone has no room for overflow).
        //
        // It is the one call here whose BOOL is sent through a Rust `bool`.
        // A foreign throw is uncatchable, so each step is marked; see
        // `alloc_init_class` for why that makes a difference.
        unsafe { msg1bv(lbl, "setAdjustsFontSizeToFitWidth:", true) };
        core_ios::log_ios("create_label: setAdjustsFontSizeToFitWidth ok");
        Ok(Label(lbl))
    }

    pub fn create_box(orientation: Orientation, spacing: i32) -> Result<BoxWidget, Error> {
        // UIStackView (iOS 9+). The host shim provides the same selectors on
        // older systems (`corroAddArrangedSubview:`), so the call sites below
        // do not branch.
        let class_name = if cls("UIStackView").is_null() {
            "CorroIosStackView"
        } else {
            "UIStackView"
        };
        let bx = new_widget(class_name, Kind::Box);
        if bx.is_null() {
            return Ok(BoxWidget(bx));
        }
        // Each message send below is a potential *foreign* throw, and a throw
        // here cannot be caught by `catch_unwind` (see `alloc_init_class`):
        // the process aborts with no Rust-side trace. The phase markers are
        // the only way to tell which one did it, so they stay until the
        // selector contract is known to hold.
        core_ios::log_ios(&format!("create_box: constructed {class_name}"));
        unsafe {
            // 0 = horizontal, 1 = vertical: NSLayoutConstraint's axis
            // convention and Android's LinearLayout constants agree.
            msg1iv(bx, "setAxis:", orientation.as_int() as isize);
        }
        core_ios::log_ios("create_box: setAxis ok");
        unsafe {
            // `setSpacing:` takes a CGFloat; the integer overload via
            // `corroSetSpacing:` keeps the ABI simple on 32-bit.
            msg1iv(bx, "corroSetSpacing:", spacing as isize);
        }
        core_ios::log_ios("create_box: corroSetSpacing ok");
        Ok(BoxWidget(bx))
    }

    pub fn create_entry() -> Result<Entry, Error> {
        let e = new_widget("UITextField", Kind::Entry);
        if e.is_null() {
            return Ok(Entry(e));
        }
        unsafe {
            // Border + a finger-sized height, and no autocorrection/caps: a
            // formula bar must not be "helpfully" rewritten.
            msg1iv(e, "setBorderStyle:", 1); // UITextBorderStyleRoundedRect
            msg1bv(e, "setAutocorrectionType:", false);
            msg1bv(e, "setAutocapitalizationType:", true); // 1 = None
            msg1bv(e, "setSpellCheckingType:", false);
            set_frame(e, 0.0, 0.0, 0.0, MIN_TOUCH_PT);
        }
        Ok(Entry(e))
    }

    pub fn create_grid() -> Result<Grid, Error> {
        Ok(Grid(new_widget("UIView", Kind::Container)))
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
        // The host-registered SheetView when available (it knows how to
        // funnel `drawRect:` and touches back into Rust), else a plain
        // UIView: the tree stays valid either way. Mirrors Android's
        // `create_canvas_view` fallback to `android/view/View`.
        let canvas_id = core_ios::next_canvas_id();
        let class_name = core_ios::sheet_view_class().unwrap_or_else(|| "UIView".to_owned());
        let class = cls(&class_name);
        let view = if class.is_null() {
            // Falling back to a plain UIView keeps the tree valid, but that
            // view never calls corro_ios_canvas_draw, so the sheet silently
            // never paints - a running app with a blank screen. Say so: this
            // is the single most consequential fallback in the backend.
            core_ios::log_ios(&format!(
                "create_canvas: class '{class_name}' NOT FOUND - falling back to a plain UIView;                  the canvas will never draw (is the class registered with set_sheet_view_class                  and linked into the app?)"
            ));
            new_widget("UIView", Kind::Canvas)
        } else {
            core_ios::log_ios(&format!("create_canvas: using class '{class_name}'"));
            // Through `alloc_init_class`, not `alloc_init`: a host is free to
            // register a canvas class that needs a designated initializer, and
            // plain `-init` would raise the same uncatchable exception that
            // killed the app in `create_box`.
            let obj = alloc_init_class(class, &class_name);
            if obj.is_null() {
                std::ptr::null_mut()
            } else {
                let obj = own(obj);
                core_ios::register_widget(
                    obj,
                    WidgetMeta {
                        kind: Kind::Canvas,
                        canvas_id,
                        ..Default::default()
                    },
                );
                // Hand the canvas id to the view so `drawRect:` and touches
                // can name it in the Rust callbacks.
                unsafe { msg1iv(obj, "corroSetCanvasId:", canvas_id as isize) };
                obj
            }
        };
        Ok(Canvas(view))
    }

    pub fn create_overlay() -> Result<Overlay, Error> {
        Ok(Overlay(new_widget("UIView", Kind::Container)))
    }

    pub fn create_scrolled_window() -> Result<ScrolledWindow, Error> {
        Ok(ScrolledWindow(new_widget("UIView", Kind::Container)))
    }

    pub fn create_dropdown(items: &[&str]) -> Result<DropDown, Error> {
        // CorroIosPicker: the app's UIButton subclass implementing
        // corroAddPickerItem: / corroSetSelectedIndex: / corroSelectedIndex.
        // A plain UIButton would raise 'unrecognized selector' on the first of
        // them - the same failure that killed the app at create_box.
        let btn = new_widget("CorroIosPicker", Kind::Container);
        if btn.is_null() {
            return Ok(DropDown(btn));
        }
        // Item list is handed to the host shim, which renders the picker with
        // whatever is available (UIMenu on iOS 14+, UIAlertController's action
        // sheet on 8+, UIActionSheet on 7).
        for item in items {
            let s = nsstring(item);
            if s.is_null() {
                continue;
            }
            raw_send!(
                btn,
                "corroAddPickerItem:",
                unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> (),
                (s)
            );
        }
        Ok(DropDown(btn))
    }

    pub fn create_checkbutton(label: &str) -> Result<CheckButton, Error> {
        let cb = new_widget("UISwitch", Kind::Container);
        if !cb.is_null() {
            core_ios::with_meta_mut(cb, |m| m.text = label.to_owned());
        }
        Ok(CheckButton(cb))
    }

    pub fn create_radiobutton(
        group: Option<&RadioButton>,
        label: &str,
    ) -> Result<RadioButton, Error> {
        let _ = group;
        let rb = new_widget("UIButton", Kind::Container);
        if rb.is_null() {
            return Ok(RadioButton(rb));
        }
        let s = nsstring(label);
        if !s.is_null() {
            unsafe {
                msg1vi(rb, "setTitle:forState:", s, 0);
                // Selection state is the radio indicator; the host shim draws
                // the group's exclusivity.
                msg1iv(rb, "setSelected:", 0);
            }
        }
        Ok(RadioButton(rb))
    }

    pub fn create_dialog() -> Result<Dialog, Error> {
        // The host shim decides the concrete class (UIAlertController on
        // iOS 8+, UIAlertView on 7) — one Rust call, no version branching
        // here. A missing shim yields a null handle and dialogs never
        // present, which is a degraded but working UI.
        let shim = cls("CorroIosAlert");
        if shim.is_null() {
            core_ios::log_ios("ios: CorroIosAlert shim missing; dialogs inert");
            return Ok(Dialog(std::ptr::null_mut()));
        }
        let d = unsafe { msg0(shim, "corroNewAlert") };
        if d.is_null() {
            return Ok(Dialog(std::ptr::null_mut()));
        }
        Ok(Dialog(own(d)))
    }

    pub fn create_textview() -> Result<TextView, Error> {
        Ok(TextView(new_widget("UITextView", Kind::Container)))
    }

    /// The display scale (`UIScreen.scale`): 1.0 on a non-Retina iPhone,
    /// 2.0 on Retina, 3.0 on the Plus/X era.
    ///
    /// Hosts need this to size things in *points* while thinking in pixels:
    /// a hardcoded 12px font is 12pt on a desktop monitor but only ~4pt on a
    /// 3x phone, i.e. about a third of Apple's 11pt footnote floor. Returns
    /// `None` before the backend is initialised (or if the call fails), so
    /// callers fall back to 1.0 and still render.
    pub fn display_density() -> Option<f64> {
        core_ios::display_scale()
    }

    /// Convenience for hosts: convert a pixel constant to points.
    pub fn px_to_points(px: f64) -> f64 {
        px / scale()
    }
}

#[cfg(target_os = "ios")]
pub use ios_adapter::*;

#[cfg(test)]
#[cfg(target_os = "ios")]
mod tests {
    use crate::core::Widget;

    #[test]
    fn test_null_window_handle() {
        let w = super::Window(std::ptr::null_mut());
        assert!(w.raw_handle().is_null());
    }

    #[test]
    fn test_null_handles_are_inert() {
        // Every one of these must be a no-op rather than a crash: the host
        // build (`examples/ios_ui.rs` on a desktop) has no backend at all.
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
        // NSLayoutConstraint's axis convention, so the shared code can pass
        // the same integers on both platforms.
        assert_eq!(super::Orientation::Horizontal as i32, 0);
        assert_eq!(super::Orientation::Vertical as i32, 1);
    }

    #[test]
    fn test_menu_submenu_clone_keeps_items() {
        let mut sub = super::Menu { items: Vec::new() };
        sub.append("Open", "app.open");
        let mut root = super::Menu { items: Vec::new() };
        root.append_submenu("File", &sub);
        let bar = super::create_menubar(&root, std::ptr::null_mut()).unwrap();
        let _ = bar.menu_active();
    }

    #[test]
    fn test_canvas_estimate_extents_nonzero() {
        let dc = super::IosDrawContext;
        let (x, y, w, h) =
            crate::core::DrawContext::text_extents_styled(&dc, "hello", "monospace", 12.0, 0, 0);
        let _ = (x, y);
        assert!(w > 0.0 && h > 0.0);
    }

    #[test]
    fn test_stack_view_is_never_built_with_plain_init() {
        // A UIStackView sent plain `-init` raises
        // NSInternalInconsistencyException, which unwinds through the
        // Objective-C runtime rather than Rust, so `catch_unwind` cannot
        // catch it and the process aborts. That is the crash that killed
        // `run_gui` at `new_box` (iOS CI runs #97 and #117), with the
        // springboard showing behind it.
        //
        // Asserted on the table rather than on behaviour because the
        // behaviour needs a live UIKit: the table is what the constructor
        // reads, so a reordering that drops the entry fails here rather
        // than on a simulator.
        assert_eq!(
            super::initializer_for("UIStackView"),
            "initWithArrangedSubviews:"
        );
        // ... and the classes that DO accept plain `-init` must not be
        // redirected: an empty-array initializer on a UILabel would be
        // meaningless, and a wrong entry here is its own bug.
        for plain in ["UIView", "UILabel", "UITextField", "UIButton", "UITextView"] {
            assert_eq!(super::initializer_for(plain), "init", "{plain}");
        }
    }

    #[test]
    fn test_strip_markup() {
        assert_eq!(super::strip_markup("<b>bold</b>"), "bold");
        assert_eq!(super::strip_markup("a &amp; b"), "a & b");
        assert_eq!(super::strip_markup("plain"), "plain");
    }
}

// ---------------------------------------------------------------------------
// Periodic tick (NSTimer via the callback trampoline)
// ---------------------------------------------------------------------------

/// Schedule `f` to run every `ms` milliseconds on the main run loop, returning
/// `false` from `f` to stop.
///
/// Until this existed, `add_periodic_tick` fell through to the "backends that
/// drive their own event loop need no timer" arm in `core.rs` and returned
/// `Ok(())` **without doing anything**. That is a silent no-op: on iOS there is
/// no idle polling at all, so
///   * `CORRO_EDIT_SCRIPT` printed "edit script armed (N steps)" and then ran
///     none of them (the iOS screenshots were of a blank sheet for exactly this
///     reason), and
///   * the append-only log tail that keeps two windows in sync never polled, so
///     a second window on the same file would never see the first's commits.
///
/// A silent success is the worst shape for this: every caller is written to
/// assume the tick exists, so the failure surfaces far away as "the sheet is
/// empty" or "the other window never updated".
///
/// The implementation needs no new host code: `NSTimer` is scheduled with the
/// host's existing `CorroIosTarget` shim as its target and `corroFired:` as its
/// selector, which calls `corro_ios_callback(id)` -> `dispatch_callback(id)`.
/// That is the same trampoline the buttons and pickers use.
#[cfg(target_os = "ios")]
/// Create an `NSTimer` on the main run loop that dispatches `f` every `ms`.
///
/// Split out so the liveness probe in [`add_periodic_tick`] uses the identical
/// scheduling path - a probe taking a different route would prove nothing about
/// the real timer.
#[cfg(target_os = "ios")]
fn schedule_timer(ms: u32, f: Box<dyn FnMut() -> bool>) -> Result<(), crate::core::Error> {
    if !crate::backends::apple::is_initialized() {
        return Err(crate::core::Error::Backend(
            "periodic tick requested before the iOS backend was initialised".into(),
        ));
    }

    // The closure the trampoline dispatches: run `f`, and re-arm the next
    // one-shot unless `f` said to stop.
    //
    // The re-arm lives *inside* the dispatched closure rather than in the
    // trampoline, so the cadence is ours: a one-shot NSTimer is created and
    // added, and each fire schedules the next. Nothing about the tick depends
    // on the timer remembering that it is supposed to repeat - which is the
    // property that could not be relied on here (see the `repeats:NO` note at
    // the arming site).
    let id_cell: std::rc::Rc<std::cell::Cell<u64>> = std::rc::Rc::new(std::cell::Cell::new(0));
    let id_for_cb = id_cell.clone();
    let ms_for_cb = ms;
    let mut f = f;
    let registered = crate::backends::apple::register_callback(Box::new(move || {
        // Re-arm FIRST, then run the body.
        //
        // Order matters because the body is application code that repaints:
        // the first real edit commits a cell and calls `queue_redraw`, which
        // re-enters the draw path. If anything in that path stalls the main
        // thread, a re-arm placed after it never happens, and the tick stops
        // for good having done exactly one edit - which is the symptom, since
        // the sheet showed A1 and nothing else and the app was otherwise
        // healthy and still drawing.
        //
        // Re-arming first costs nothing: the next fire is `ms` away either way,
        // and if the body stops the tick the series ends on this iteration
        // rather than the next.
        if let Err(e) = arm_one_shot(ms_for_cb, id_for_cb.get()) {
            crate::backends::apple::log_apple(&format!(
                "ios: could not re-arm the {ms_for_cb}ms tick: {e}"
            ));
        }
        if !f() {
            // The body asked to stop, so undo the fire we just scheduled:
            // `unregister_callback` makes the next dispatch a no-op, and the
            // timer is a one-shot that has not been reached yet.
            crate::backends::apple::unregister_callback(id_for_cb.get());
        }
    }));
    id_cell.set(registered);

    let registered = id_cell.get();
    arm_one_shot(ms, registered)?;
    crate::backends::apple::log_apple(&format!(
        "ios: main-queue one-shot tick armed ({ms}ms, callback #{registered}); \
         each fire re-arms the next"
    ));
    Ok(())
}

/// Arm ONE delayed call to `callback_id`, `ms` from now, via the host's
/// `corroFireAfter:`.
///
/// The tick re-arms itself: each fire calls this again for the next interval,
/// so the repeat is a fact of our bookkeeping rather than of a timer object's
/// internal state.
///
/// # Why not an NSTimer
///
/// An `NSTimer` was the original design and it did not survive contact with
/// this host. Every run-loop-timer shape tried fired a few times and then went
/// quiet for the rest of the run: a repeating timer, a one-shot re-armed from
/// inside its own callback, and registrations in `NSRunLoopCommonModes`,
/// `NSDefaultRunLoopMode`, and both at once. They are all armed from
/// `viewDidLoad`, before `UIApplicationMain` starts the run loop, and the
/// per-tick log showed the last tick 240ms in with the first edit due at
/// 400ms - so the sheet never gained a value and the screenshot check reported
/// a blank workbook.
///
/// `performSelector:withObject:afterDelay:` schedules through the main queue
/// rather than a CFRunLoop source, and keeps firing. Which of the two is at
/// fault is not worth further runs to establish: this one works, and the
/// `corroFired:` it names is the same trampoline the buttons use, so the
/// callback plumbing is unchanged.
#[cfg(target_os = "ios")]
fn arm_one_shot(ms: u32, callback_id: u64) -> Result<(), crate::core::Error> {
    use crate::backends::apple::{cls, own};

    let shim = cls("CorroIosTarget");
    if shim.is_null() {
        return Err(crate::core::Error::Backend(
            "CorroIosTarget shim missing (cannot schedule the tick)".into(),
        ));
    }
    let target =
        unsafe { crate::backends::apple::msg1i(shim, "targetWithCallbackId:", callback_id as isize) };
    if target.is_null() {
        return Err(crate::core::Error::Backend(
            "CorroIosTarget targetWithCallbackId: returned nil".into(),
        ));
    }
    // Neither an NSTimer nor the delay scheduler retains its target, so this
    // retain is what keeps the trampoline alive until it fires.
    let target = own(target);

    // `corroFireAfter:` takes an NSTimeInterval, which is a `double` on every
    // 64-bit Apple target - the same width `msg1cv` sends.
    let delay_secs = (ms.max(1) as f64) / 1000.0;
    unsafe { crate::backends::apple::msg1cv(target, "corroFireAfter:", delay_secs) };
    Ok(())
}

/// Schedule `f` to run every `ms` milliseconds on the main run loop, returning
/// `false` from `f` to stop.
///
/// Until this existed, `add_periodic_tick` fell through to the "backends that
/// drive their own event loop need no timer" arm in `core.rs` and returned
/// `Ok(())` **without doing anything**. That is a silent no-op: on iOS nothing
/// polled at all, so
///   * `CORRO_EDIT_SCRIPT` printed "edit script armed (N steps)" and then ran
///     none of them (the iOS CI screenshots were of a blank sheet for exactly
///     this reason), and
///   * the append-only log tail that keeps two windows in sync never polled, so
///     a second window would never see the first's commits.
///
/// A silent success is the worst shape for this: every caller assumes the tick
/// exists, so the failure surfaces far away as "the sheet is empty".
///
/// Implemented as an `NSTimer` on the main run loop, whose target is the host's
/// existing `CorroIosTarget` shim and whose selector is `corroFired:` - the same
/// trampoline the buttons use, so no new host code is needed.
#[cfg(target_os = "ios")]
pub fn add_periodic_tick(
    ms: u32,
    f: Box<dyn FnMut() -> bool>,
) -> Result<(), crate::core::Error> {
    schedule_timer(ms, f)?;

    // Prove the timer actually FIRES, not merely that it was scheduled.
    //
    // Scheduling an `NSTimer` is not the same as having it run: a timer added to
    // a run loop that is not yet running fires never, and the only symptom would
    // be an absence somewhere else - which is exactly how this bug hid for as
    // long as it did. These lines say whether the mechanism is live.
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    schedule_timer(
        250,
        Box::new(move || {
            let n = fired.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            // First fire only. This used to log at 1, 4 and 20, which
            // cannot distinguish "stopped after one fire" from "fired three
            // times" - there was no tail to look at - and then logged on every
            // fire, which is 4 lines a second forever. "Fired at least once"
            // is the question the probe exists to answer, and one line answers
            // it; a count that grows forever does not.
            if n == 1 {
                crate::backends::apple::log_apple(
                    "ios: periodic tick fired (the timer is live)",
                );
            }
            true
        }),
    )
    .map(|_| ())
}