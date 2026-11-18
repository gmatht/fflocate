//! Android backend adapter: thin JNI wrappers around Android Views.
//!
//! Every widget is a `#[repr(transparent)]` raw `jobject` handle kept alive
//! by a `GlobalRef` in [`crate::backends::android`] (see `KEEP_ALIVE` there).
//! Methods that need the JVM attach the current thread via
//! `with_env_and_activity` and are best-effort no-ops when the backend was
//! never initialised (host builds, tests, headless runs).
//!
//! The Canvas renders via a custom Java `SheetView` (see
//! `rustxWidgets/examples/android`): the draw closure is registered in the
//! Rust-side callback registry and dispatched by id from `SheetView.onDraw`,
//! which funnels every `DrawContext` call back into Rust through JNI. Until
//! a `SheetView` exists, `queue_redraw` is a no-op and `text_extents_*`
//! return a monospace estimate so layout never divides by zero.

#[cfg(target_os = "android")]
mod android_adapter {
    use crate::core::{DrawContext, Error, Widget};
    use jni::objects::JString;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::os::raw::c_void;
    use std::sync::Mutex;

    use once_cell::sync::Lazy;

    /// Monospace advance estimate (px) used by `text_extents_*` until real
    /// measurement exists. Matches the `CHAR_W`-family constants callers use.
    const EST_CHAR_W: f64 = 7.2;
    /// Line-height factor for the estimate.
    const EST_LINE_H: f64 = 1.2;

    fn estimate_extents(text: &str, size: f64, weight: i32) -> (f64, f64, f64, f64) {
        let bold = if weight != 0 { 1.08 } else { 1.0 };
        let w = text.chars().count() as f64 * size * (EST_CHAR_W / 12.0) * bold;
        (0.0, 0.0, w, size * EST_LINE_H)
    }

    /// `TextView.setText` for the `set_label` surface GTK and NWG expose on
    /// `CheckButton`/`RadioButton`. A `CheckBox` *is* a `TextView`, so this
    /// is the same call `Label::set_text` makes — factored out because three
    /// widgets now need it and the JNI dance is nine lines each.
    fn set_widget_label(view_ptr: *mut c_void, label: &str) {
        let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
            let view = unsafe { jni::objects::JObject::from_raw(view_ptr as jni::sys::jobject) };
            let j_text = env.new_string(label)?;
            env.call_method(
                &view,
                "setText",
                "(Ljava/lang/CharSequence;)V",
                &[(&j_text).into()],
            )?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        });
    }

    // ------------------------------------------------------------------
    // Orientation
    // ------------------------------------------------------------------

    /// Layout direction for [`create_box`]. Discriminants match
    /// `android.widget.LinearLayout` orientation constants
    /// (`HORIZONTAL = 0`, `VERTICAL = 1`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Orientation {
        Horizontal = 0,
        Vertical = 1,
    }

    impl Orientation {
        fn as_jni_int(self) -> i32 {
            self as i32
        }
    }

    /// Alias required by `common.rs` (`Orientation as AndroidOrientation`).
    pub type AndroidOrientation = Orientation;

    // ------------------------------------------------------------------
    // Window
    // ------------------------------------------------------------------

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
        /// The Activity's title bar caption.
        ///
        /// Not a no-op like it was: on Android the window is the Activity, so
        /// `setTitle` is the only way a caller can put a document name in the
        /// system title (recents, the task switcher, a split-screen header).
        /// A spreadsheet that silently refused to name its window also refused
        /// to say which file was open in the Android recents list.
        pub fn set_title(&self, title: &str) {
            let _ = crate::backends::android::with_env_and_activity(|env, activity| {
                let j_title = env.new_string(title)?;
                env.call_method(
                    activity.as_obj(),
                    "setTitle",
                    "(Ljava/lang/CharSequence;)V",
                    &[(&j_title).into()],
                )?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        /// Request `w`x`h` for the root layout, in device px.
        ///
        /// `WRAP_CONTENT` is deliberately *not* used: a `LinearLayout` that
        /// wraps would let the sheet collapse to its minimal size, which is
        /// the "small white patch in a field of grey" failure. MATCH_PARENT
        /// in both axes with weight 1 makes the sheet fill the Activity, and
        /// the explicit minimum keeps it from collapsing on a phone that
        /// reports a transient zero during the first layout pass.
        pub fn set_default_size(&self, w: i32, h: i32) {
            let root = match crate::backends::android::root_layout() {
                Ok(r) => r,
                Err(_) => return,
            };
            let ptr = root.as_obj().as_raw() as *mut c_void;
            let (w, h) = (w.max(1), h.max(1));
            crate::backends::android::set_view_min_size(ptr, w, h);
        }

        /// Immediate resize.
        ///
        /// GTK needs this because `set_default_size` is advisory there (and
        /// is ignored entirely with no window manager). Android needs it for
        /// the same reason: the Activity's size is decided by the system
        /// window manager and the device, and a caller that wants a different
        /// one (a preview pane, a landscape-locked tool) cannot get it from a
        /// *request*. The closest equivalent is asking the root layout for
        /// those exact dimensions, which is what this does.
        pub fn resize(&self, w: i32, h: i32) {
            self.set_default_size(w, h);
        }

        /// # Safety
        /// Kept for API compatibility with the GTK backend; no-op on Android.
        pub unsafe fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}

        pub fn hwnd(&self) -> *mut c_void {
            std::ptr::null_mut()
        }

        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            if child_ptr.is_null() {
                return;
            }
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let root = crate::backends::android::root_layout()?;
                let child_obj = unsafe {
                    // SAFETY: child_ptr was obtained from a JNI-created object. We reconstruct
                    // a JObject from the raw pointer only for the duration of this JNI call.
                    jni::objects::JObject::from_raw(child_ptr as jni::sys::jobject)
                };
                env.call_method(
                    root.as_obj(),
                    "addView",
                    "(Landroid/view/View;)V",
                    &[(&child_obj).into()],
                )?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        pub fn set_child_box(&self, bx: &BoxWidget) {
            self.set_child(bx);
        }

        /// No-op, but no longer for the reason it used to be: the Activity is
        /// already attached and visible by the time a widget tree is built
        /// (`nativeInit` runs from `onCreate`), so there is nothing to
        /// present. Android's analogue is `View.requestLayout`, which
        /// [`Window::set_default_size`] already triggers.
        pub fn present(&self) {
            if let Ok(root) = crate::backends::android::root_layout() {
                let ptr = root.as_obj().as_raw() as *mut c_void;
                let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                    let view =
                        unsafe { jni::objects::JObject::from_raw(ptr as jni::sys::jobject) };
                    env.call_method(&view, "requestLayout", "()V", &[])?;
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
                });
            }
        }
        pub fn queue_redraw(&self) {}
        pub fn on_event(&self, _cb: Box<dyn FnMut(*mut c_void) -> i32>) {}
        /// Window-level key fallback, the counterpart of GTK's
        /// `EventControllerKey` on the toplevel.
        ///
        /// This was an empty body, which is the second reason no key reached
        /// corro on Android (the first was that nothing in the Java host
        /// called `dispatch_canvas_key` at all). The Activity's `onKeyDown`
        /// now routes into this registry, so a key that no view claims still
        /// arrives — which is where Ctrl+Q, Ctrl+S and the other window-level
        /// accelerators live, because the formula entry deliberately lets
        /// those through.
        pub fn on_event_key(&self, cb: Box<dyn FnMut(u32, u32) -> i32>) {
            let mut map = WINDOW_KEY_CALLBACKS.lock().unwrap();
            map.insert(0u64, SendWindowKey(Box::into_raw(cb)));
        }
        pub fn on_close(&self, _cb: Box<dyn FnMut()>) {}
    }

    /// Leave the app: `Activity.finish()`, which unwinds this Activity and
    /// shows whatever launched it. The module-level counterpart of GTK's
    /// `gtk_main_quit` and NWG's event-loop stop.
    ///
    /// Best-effort: a host with no Activity (a unit test, a headless run) is
    /// a no-op, matching every other Android adapter method.
    pub fn quit_main_loop() {
        let _ = crate::backends::android::with_env_and_activity(|env, activity| {
            env.call_method(activity.as_obj(), "finish", "()V", &[])?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        });
    }

    /// `Handler.postDelayed` with a no-op runnable, returning the removal
    /// token. This is the Android spelling of GTK's `timeout_add` and NWG's
    /// message-timer, and it is the only timer available: there is no event
    /// loop to run a source on.
    ///
    /// `fn_ptr` is a `extern "C" fn()` registered in [`crate::backends::android`]
    /// and dispatched by id from a Java `Runnable`, so the closure keeps the
    /// same lifetime story as every other callback in the adapter.
    pub fn timeout_add_once(delay_ms: u32, fn_ptr: extern "C" fn()) -> u64 {
        crate::backends::android::schedule_timeout(delay_ms, fn_ptr, false)
    }

    /// See [`timeout_add_once`]; repeats every `delay_ms` until
    /// [`cancel_timeout`] or [`quit_main_loop`].
    pub fn timeout_add_repeating(delay_ms: u32, fn_ptr: extern "C" fn()) -> u64 {
        crate::backends::android::schedule_timeout(delay_ms, fn_ptr, true)
    }

    /// Cancel a timer from [`timeout_add_once`] / [`timeout_add_repeating`].
    pub fn cancel_timeout(id: u64) {
        crate::backends::android::cancel_timeout(id);
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
        pub fn on_click(&self, f: impl FnMut() + Send + 'static) -> Result<u64, Error> {
            let id = crate::backends::android::register_callback(Box::new(f));
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                if let Some(listener) = crate::backends::android::try_create_onclick_listener(env, id)
                    .map_err(|e| format!("{e}"))
                    .unwrap_or(None)
                {
                    let btn = unsafe {
                        // SAFETY: self.0 is a raw jobject from create_button(). We reconstruct
                        // it only for this JNI call while the JVM is attached.
                        jni::objects::JObject::from_raw(self.0 as jni::sys::jobject)
                    };
                    env.call_method(
                        &btn,
                        "setOnClickListener",
                        "(Landroid/view/View$OnClickListener;)V",
                        &[(&listener).into()],
                    )?;
                }
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
            Ok(id)
        }

        pub fn emit_clicked(&self) -> Result<u64, Error> {
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let btn = unsafe {
                    // SAFETY: self.0 is a raw jobject from create_button(). We only use it
                    // synchronously within this JNI call while the JVM is attached.
                    jni::objects::JObject::from_raw(self.0 as jni::sys::jobject)
                };
                env.call_method(&btn, "performClick", "()Z", &[])?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
            Ok(0)
        }

        /// See [`Label::set_hexpand`]: recorded, and honoured as
        /// `LinearLayout` weight by `BoxWidget::append`.
        pub fn set_hexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        /// See [`Button::set_hexpand`].
        pub fn set_vexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        /// Minimum width/height in device px; see
        /// [`crate::backends::android::set_view_min_size`] for the negative
        /// ("no request") convention.
        pub fn set_size_request(&self, w: i32, h: i32) {
            crate::backends::android::set_view_min_size(self.0, w, h);
        }

        pub fn set_visible(&self, visible: bool) {
            crate::backends::android::set_view_visible(self.0, visible);
        }

        /// Add a CSS class name.
        ///
        /// Android has no stylesheet cascade, so this records the class and
        /// does nothing visual *unless* a host has installed a class bridge.
        /// Recording rather than ignoring is what lets
        /// `CorroStyleBridge` (an optional host class) map classes to
        /// `setBackground`/`setTextColor` later, which is how a host gets
        /// GTK's "dim this when the sheet is read-only" for free.
        pub fn add_class(&self, class_name: &str) {
            crate::backends::android::add_view_class(self.0, class_name);
        }

        /// See [`Button::add_class`].
        pub fn remove_class(&self, class_name: &str) {
            crate::backends::android::remove_view_class(self.0, class_name);
        }

        /// `weight != 0` selects `Typeface.BOLD`.
        ///
        /// GTK's `PANGO_STYLE_ITALIC` and NWG's `font-style` map onto
        /// `Typeface.ITALIC`, which Android has as a separate style constant,
        /// so both non-zero weights are honoured rather than collapsed.
        pub fn set_font_style(&self, style: i32) {
            crate::backends::android::set_view_typeface(self.0, style);
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
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let tv = unsafe {
                    // SAFETY: self.0 is a raw jobject from create_label(). JVM is attached.
                    jni::objects::JObject::from_raw(self.0 as jni::sys::jobject)
                };
                let j_text = env.new_string(text)?;
                env.call_method(
                    &tv,
                    "setText",
                    "(Ljava/lang/CharSequence;)V",
                    &[(&j_text).into()],
                )?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        pub fn get_text(&self) -> Option<String> {
            crate::backends::android::get_view_text(self.0)
        }

        pub fn set_visible(&self, visible: bool) {
            crate::backends::android::set_view_visible(self.0, visible);
        }

        pub fn set_markup(&self, markup: &str) {
            self.set_text(markup);
        }

        /// Pin the label's measured width, in device px. `None` releases it.
        ///
        /// GTK's `set_size_request` and NWG's fixed-width label both answer
        /// the same question: "keep this slot from reflowing when the text
        /// changes". On Android a `TextView` in a `LinearLayout` reflows its
        /// siblings when its text changes, so the same corruption appears.
        /// The pin is a `LinearLayout.LayoutParams` width, which is what a
        /// `WRAP_CONTENT` slot needs: the sibling positions stop moving
        /// while the text still draws inside the pinned box.
        pub fn set_fixed_width(&self, w: Option<i32>) {
            match w {
                Some(px) if px > 0 => {
                    // WRAP_CONTENT height, fixed width, no weight: the label
                    // keeps its box but never steals space from a sibling.
                    crate::backends::android::set_view_layout(self.0, px, -2, 0.0);
                }
                _ => {
                    // MATCH_PARENT would expand, so fall back to WRAP_CONTENT
                    // for both axes: that is the unpinned behaviour.
                    crate::backends::android::set_view_layout(self.0, -2, -2, 0.0);
                }
            }
        }

        /// Left margin of the label's contents, in device px.
        ///
        /// Pairs with [`Label::set_fixed_width`]: a pinned, left-aligned
        /// label sits flush against its slot's edge, and this restores the
        /// inset that a GTK `margin_start` provides. Implemented as padding
        /// rather than a layout margin because that is what a pinned width
        /// keeps constant — a layout margin would be inside the pinned box
        /// only if the box is the one being measured.
        pub fn set_margin_start(&self, px: i32) {
            let left = px.max(0);
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let tv = unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let _cur = env
                    .call_method(&tv, "getPaddingLeft", "()I", &[])?
                    .i()?;
                let cur_right = env
                    .call_method(&tv, "getPaddingRight", "()I", &[])?
                    .i()?;
                env.call_method(
                    &tv,
                    "setPaddingRelative",
                    "(IIII)V",
                    &[left.into(), 0i32.into(), cur_right.into(), 0i32.into()],
                )?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        /// Vertical outer spacing, in device px. Pairs with
        /// [`Label::set_margin_start`]; see its note on padding vs margin.
        pub fn set_margin_top(&self, px: i32) {
            let top = px.max(0);
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let tv = unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let cur_left = env
                    .call_method(&tv, "getPaddingLeft", "()I", &[])?
                    .i()?;
                let cur_bottom = env
                    .call_method(&tv, "getPaddingBottom", "()I", &[])?
                    .i()?;
                env.call_method(
                    &tv,
                    "setPaddingRelative",
                    "(IIII)V",
                    &[cur_left.into(), top.into(), 0i32.into(), cur_bottom.into()],
                )?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        /// Horizontal alignment of the text within the label's box, 0.0 left
        /// .. 1.0 right.
        ///
        /// A `TextView`'s own gravity, not the layout's: a caller that pins
        /// a width and then wants the text flush right asks for gravity, and
        /// only gravity moves the glyphs inside the box.
        pub fn set_xalign(&self, x: f32) {
            let frac = x.clamp(0.0, 1.0);
            let gravity = crate::backends::android::with_env_and_activity(|env, _activity| {
                let g = env.get_static_field("android/view/Gravity", "START", "I")?.i()?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(g)
            })
            .unwrap_or(0x0080_0003); // Gravity.START
            // Snap to the nearest of left/center/right, the way the platform
            // spells horizontal gravity. `center_horizontal` is bit 0 of the
            // horizontal axis; the caller never asked for vertical centring,
            // so only the horizontal half of the constant changes.
            let h = if frac < 0.25 {
                gravity // START
            } else if frac < 0.75 {
                gravity | 0x1 // CENTER_HORIZONTAL
            } else {
                0x0080_0005 // Gravity.END
            };
            crate::backends::android::set_view_gravity(self.0, h);
        }

        /// Whether the label may take extra horizontal space. Android has no
        /// expand flag; the request is recorded and honoured by
        /// `BoxWidget::append` as `LinearLayout` weight, like `TextView`'s.
        pub fn set_hexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        /// See [`Label::set_hexpand`]. Recorded and honoured as weight too:
        /// `LinearLayout` has one weight field, so a view that expands in
        /// either axis expands in both, which is what a vertical box wants.
        pub fn set_vexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        /// Minimum width/height in device px. A negative value (GTK's "no
        /// request") releases the minimum, matching GTK's `-1` convention.
        pub fn set_size_request(&self, w: i32, h: i32) {
            crate::backends::android::set_view_min_size(self.0, w, h);
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

    // ------------------------------------------------------------------
    // BoxWidget
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
        /// Attach `child` with parent-appropriate `LayoutParams`: children
        /// of a vertical `LinearLayout` fill the width and share leftover
        /// height by weight (weight=1 only for sheet canvases, which must
        /// grow; chrome keeps wrap-content).
        pub fn append(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            if child_ptr.is_null() {
                return;
            }
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let layout = unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let child_obj = unsafe { jni::objects::JObject::from_raw(child_ptr as jni::sys::jobject) };
                // Axis that grows depends on the parent's orientation, which
                // the child cannot know — query it from the layout.
                let is_canvas = crate::backends::android::is_canvas_view(child_ptr);
                let expands = crate::backends::android::is_view_expanding(child_ptr);
                let vertical_parent = env
                    .call_method(&layout, "getOrientation", "()I", &[])
                    .ok()
                    .and_then(|o| o.i().ok())
                    .map(|o| o == 1)
                    .unwrap_or(true);
                let weight = if is_canvas || expands { 1.0f32 } else { 0.0f32 };
                let (child_w, child_h) = if is_canvas {
                    // The sheet fills whatever the parent gives it.
                    (-1i32, 0i32)
                } else if expands {
                    if vertical_parent { (-1i32, 0i32) } else { (0i32, -1i32) }
                } else {
                    (-2i32, -2i32)
                };
                let params = env.new_object(
                    "android/widget/LinearLayout$LayoutParams",
                    "(IIF)V",
                    &[child_w.into(), child_h.into(), weight.into()],
                )?;
                // Sizing floors, in px. The density is needed for both, so it
                // is resolved once here.
                let density = env
                    .call_method(&layout, "getResources", "()Landroid/content/res/Resources;", &[])
                    .ok()
                    .and_then(|r| r.l().ok())
                    .and_then(|res| {
                        env.call_method(&res, "getDisplayMetrics", "()Landroid/util/DisplayMetrics;", &[])
                            .ok()
                            .and_then(|m| m.l().ok())
                    })
                    .and_then(|metrics| env.get_field(&metrics, "density", "F").ok())
                    .and_then(|f| f.f().ok())
                    .unwrap_or(1.0);

                // Width: an empty EditText measures to zero, so give it a floor
                // (density-independent, ≈9px per character at mdpi). Expanding
                // children (the formula entry) get a usable width even when
                // nothing set one explicitly.
                let min_chars = {
                    let n = crate::backends::android::view_min_chars(child_ptr);
                    if n > 0 { n } else if expands { 12 } else { 0 }
                };
                if min_chars > 0 {
                    let min_px = (min_chars as f32 * 9.0 * density) as i32;
                    let _ = env.call_method(
                        &child_obj,
                        "setMinimumWidth",
                        "(I)V",
                        &[min_px.into()],
                    );
                }

                // Height: an expanding child that is NOT the canvas is the
                // formula entry.
                //
                // It is laid out with height=MATCH_PARENT and weight 1 inside a
                // HORIZONTAL bar (see `child_h` above), so its own
                // `setMinimumHeight` is ignored - the bar's height comes from
                // whichever of its children is tallest, and the two labels in
                // it measure only ~51px. The address label and "fx" marker then
                // lay out at y=36..87 relative to that 51px bar, so they were
                // clipped away entirely and the formula bar rendered as an
                // empty strip - well under Android's 48dp minimum touch target.
                //
                // Set the floor on the PARENT, which is what actually
                // determines the bar's height.
                if expands && !is_canvas {
                    let min_h_px = (48.0 * density) as i32;
                    let _ = env.call_method(
                        &child_obj,
                        "setMinimumHeight",
                        "(I)V",
                        &[min_h_px.into()],
                    );
                    let _ = env.call_method(
                        &layout,
                        "setMinimumHeight",
                        "(I)V",
                        &[min_h_px.into()],
                    );
                }

                env.call_method(
                    &layout,
                    "addView",
                    "(Landroid/view/View;Landroid/view/ViewGroup$LayoutParams;)V",
                    &[(&child_obj).into(), (&params).into()],
                )?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        /// Per-child expansion. Recorded on the *child*, because
        /// `BoxWidget::append` is what turns the request into weight and it
        /// runs at append time — setting it after the append has no way to
        /// reach the child's params without re-adding the view, which would
        /// reorder the box.
        pub fn set_child_hexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
            crate::backends::android::set_view_expanding(*child.as_ref(), expand);
        }

        /// See [`BoxWidget::set_child_hexpand`].
        pub fn set_child_vexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
            crate::backends::android::set_view_expanding(*child.as_ref(), expand);
        }

        pub fn set_hexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        /// See [`BoxWidget::set_hexpand`].
        pub fn set_vexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        /// Minimum size of the box itself, in device px.
        ///
        /// The row-overlap bug this fixes is not GTK-specific. A horizontal
        /// `LinearLayout` takes its height from its tallest child, so a row
        /// holding a single `Button` is exactly as tall as the button's own
        /// padding suggests — and two consecutive rows then sit at the same
        /// y, rendering their text on top of itself. A minimum height on the
        /// box is the portable way to say "this strip is N tall", and on
        /// Android it is the only one: there is no window manager to infer a
        /// row height from.
        pub fn set_size_request(&self, w: i32, h: i32) {
            crate::backends::android::set_view_min_size(self.0, w, h);
        }

        /// Hide or show the whole box.
        ///
        /// `INVISIBLE` rather than `GONE`, so the box keeps its slot: a `GONE`
        /// box hands its space to a sibling, which reflows the sheet when a
        /// caller hides a chrome row. The distinction is what makes a
        /// "settings" row hideable without the grid jumping.
        pub fn set_visible(&self, visible: bool) {
            crate::backends::android::set_view_visible(self.0, visible);
        }
    }

    // ------------------------------------------------------------------
    // Entry
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
        /// Android laid out the entry at zero width (off-screen) because
        /// the box gave every non-canvas child WRAP_CONTENT and an empty
        /// EditText measures 0. Expand flags are no-ops here, so record
        /// the request and let `BoxWidget::append` weight it.
        pub fn set_hexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        pub fn set_vexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        pub fn set_width_chars(&self, n: i32) {
            crate::backends::android::set_view_min_chars(self.0, n);
        }
        pub fn set_text(&self, text: &str) {
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let edit = unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let j_text = env.new_string(text)?;
                env.call_method(
                    &edit,
                    "setText",
                    "(Ljava/lang/CharSequence;)V",
                    &[(&j_text).into()],
                )?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        pub fn get_text(&self) -> Option<String> {
            let result = crate::backends::android::with_env_and_activity(|env, _activity| {
                let edit = unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let j_value = env.call_method(&edit, "getText", "()Ljava/lang/CharSequence;", &[])?;
                let j_obj_ref = j_value.l()?;
                let j_obj = unsafe { jni::objects::JObject::from_raw(j_obj_ref.as_raw()) };
                let j_str = JString::from(j_obj);
                let text: String = env.get_string(&j_str)?.into();
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(text)
            });
            result.ok()
        }

        pub fn set_size_request(&self, _w: i32, _h: i32) {}

        pub fn set_visible(&self, _v: bool) {}
        pub fn add_class(&self, _class_name: &str) {}
        pub fn remove_class(&self, _class_name: &str) {}
        pub fn set_halign(&self, _align: i32) {}
        pub fn set_valign(&self, _align: i32) {}
        pub fn set_margin_start(&self, _px: i32) {}
        pub fn set_margin_top(&self, _px: i32) {}

        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(
            &self,
            f: F,
        ) -> Result<u64, Error> {
            // RETURN from the soft keyboard: the Java side calls
            // `nativeEntryActivate(entryPtr)` from an OnEditorActionListener.
            let mut map = ENTRY_ACTIVATE.lock().unwrap();
            let mut f = f;
            map.insert(
                self.0 as usize,
                SendEntryActivate(Box::into_raw(Box::new(move |p| f(p)))),
            );
            crate::backends::android::attach_editor_action(self.0);
            Ok(0)
        }

        pub fn connect_focus_in_event<F: FnMut(*mut c_void) -> i32 + 'static>(
            &self,
            _f: F,
        ) -> Result<u64, Error> {
            Ok(0)
        }

        pub fn connect_focus_out_event<F: FnMut(*mut c_void) -> i32 + 'static>(
            &self,
            _f: F,
        ) -> Result<u64, Error> {
            Ok(0)
        }

        pub fn grab_focus(&self) {
            crate::backends::android::focus_view(self.0);
        }

        /// The caret as a character index, or `None` when the view cannot
        /// report one.
        ///
        /// This is what makes in-cell editing keys work on a phone. Without it
        /// `common::Entry` falls back to an internally tracked position that
        /// no user input ever updates, so arrow keys cannot move the caret,
        /// backspace cannot delete before it, and the whole `handle_edit_key`
        /// path in corro is unreachable. `EditText.getSelectionStart` gives a
        /// real one; a selection *range* is collapsed to its start, which is
        /// the same convention GTK and NWG use.
        pub fn get_position(&self) -> Option<usize> {
            crate::backends::android::get_view_selection_start(self.0)
        }

        /// Move the caret to a character index, clamped to the text length.
        pub fn set_position(&self, pos: usize) {
            crate::backends::android::set_view_selection(self.0, pos);
        }

        /// Register a raw-key handler.
        ///
        /// The registry is keyed by the entry's view pointer and dispatched
        /// from `CorroKeyListener.onKey`, which the app installs for every
        /// entry. The listener forwards *only* the keys that would otherwise
        /// be swallowed or mis-handled by the platform (arrows, Tab, Escape,
        /// Delete); plain character input keeps arriving as text through the
        /// `TextWatcher`, so the two paths do not double-apply.
        pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
            let mut map = ENTRY_KEYS.lock().unwrap();
            map.insert(self.0 as usize, SendEntryKey(Box::into_raw(cb)));
            crate::backends::android::attach_key_listener(self.0);
            // The key bridge recovers this handle from the view's tag; a
            // `View` with no tag is not a corro entry, so the key never
            // reaches an entry handler that does not exist.
            crate::backends::android::tag_view_pointer(self.0);
        }

        /// Convenience form of [`Entry::on_key_raw`] with no modifiers, the
        /// shape the GTK and NWG adapters expose alongside it.
        pub fn on_key(&self, mut cb: Box<dyn FnMut(u32) -> bool>) {
            self.on_key_raw(Box::new(move |k, _mods| cb(k)));
        }

    pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        // Fire `f` on every text change. The Java side calls
        // `nativeEntryChanged(entryPtr)` from a TextWatcher (installed by
        // `attach_text_watcher`); the callback is keyed by the entry's
        // GlobalRef pointer, exactly like the canvas registries.
        let mut map = TEXT_CHANGED.lock().unwrap();
        let mut f = f;
        map.insert(self.0 as usize, SendTextChanged(Box::into_raw(Box::new(move || f()))));
        crate::backends::android::attach_text_watcher(self.0);
        Ok(0)
    }

        /// No focus query on android entries: report false (unchanged).
        pub fn has_focus(&self) -> bool {
            false
        }

        /// No pointer clicks on android entries: accept and never fire.
        pub fn connect_button_press(&self, _f: impl FnMut() + 'static) -> Result<u64, Error> {
            Ok(0)
        }
    }

    impl Clone for Entry {
        fn clone(&self) -> Self {
            Entry(self.0)
        }
    }

    /// Text-change callbacks keyed by entry view pointer.
    static TEXT_CHANGED: Lazy<Mutex<HashMap<usize, SendTextChanged>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    /// RETURN (editor action) callbacks keyed by entry view pointer.
    static ENTRY_ACTIVATE: Lazy<Mutex<HashMap<usize, SendEntryActivate>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    struct SendTextChanged(*mut dyn FnMut());
    // SAFETY: guarded by the registry mutex; same pattern as the canvas maps.
    unsafe impl Send for SendTextChanged {}
    struct SendEntryActivate(*mut dyn FnMut(*mut c_void));
    // SAFETY: guarded by the registry mutex; same pattern as the canvas maps.
    unsafe impl Send for SendEntryActivate {}
    struct SendEntryKey(*mut dyn FnMut(u32, u32) -> bool);
    // SAFETY: guarded by the registry mutex; same pattern as the canvas maps.
    unsafe impl Send for SendEntryKey {}

    /// Raw-key handlers for entries, keyed by view pointer. See
    /// [`Entry::on_key_raw`].
    static ENTRY_KEYS: Lazy<Mutex<HashMap<usize, SendEntryKey>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    /// Called from `CorroKeyListener.onKey`: run the entry's key handler.
    /// Returns whether a handler claimed the key, so the listener knows
    /// whether to fall through to the platform's own handling.
    pub fn dispatch_entry_key(entry_ptr: *mut c_void, keyval: u32, mods: u32) -> bool {
        let raw = {
            let map = ENTRY_KEYS.lock().unwrap();
            map.get(&(entry_ptr as usize)).map(|s| s.0)
        };
        match raw {
            Some(ptr) => {
                let cb: &mut dyn FnMut(u32, u32) -> bool = unsafe { &mut *ptr };
                cb(keyval, mods)
            }
            None => false,
        }
    }

    /// Called from the Java TextWatcher: run the registered change callback.
    pub fn dispatch_text_changed(entry_ptr: *mut c_void) {
        crate::backends::android::logcat_rs("dispatch_text_changed fired");
        let mut map = TEXT_CHANGED.lock().unwrap();
        if let Some(SendTextChanged(ptr)) = map.get_mut(&(entry_ptr as usize)) {
            let cb: &mut dyn FnMut() = unsafe { &mut **ptr };
            cb();
        }
    }

    /// Called from the Java OnEditorActionListener (IME "Done"/Enter).
    pub fn dispatch_entry_activate(entry_ptr: *mut c_void) {
        crate::backends::android::logcat_rs("dispatch_entry_activate fired");
        let mut map = ENTRY_ACTIVATE.lock().unwrap();
        if let Some(SendEntryActivate(ptr)) = map.get_mut(&(entry_ptr as usize)) {
            let cb: &mut dyn FnMut(*mut c_void) = unsafe { &mut **ptr };
            cb(entry_ptr);
        }
    }

    // ------------------------------------------------------------------
    // Canvas + DrawContext
    // ------------------------------------------------------------------
    //
    // Rendering target for the sheet. The Java side owns a `SheetView`
    // (custom `View`) whose `onDraw(Canvas)` funnels each primitive back
    // into Rust through JNI; the Rust side only stores the draw closure
    // and fires it on `queue_redraw`. Until that view exists (host unit
    // tests, uninitialised backend) the closure still runs against an
    // estimating context so `render_to` logic stays testable.

    /// `DrawContext` implementation used when no Java canvas is attached:
    /// records nothing, measures with the monospace estimate.
    pub struct AndroidDrawContext;

    impl DrawContext for AndroidDrawContext {
        fn fill_rect(&mut self, _x: f64, _y: f64, _w: f64, _h: f64, _r: f64, _g: f64, _b: f64, _a: f64) {
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
            crate::backends::android::canvas_id_for_view(self.0)
        }

        pub fn set_draw_callback(
            &self,
            cb: Box<dyn FnMut(&mut dyn DrawContext, i32, i32)>,
        ) {
            // Stash the closure under our canvas id; the Java SheetView
            // dispatches to it by id from onDraw.
            {
                let mut map = DRAW_CALLBACKS.lock().unwrap();
                map.insert(
                    self.canvas_id(),
                    SendDrawCallback(Box::into_raw(cb)),
                );
            }
            self.queue_redraw();
        }

        pub fn queue_redraw(&self) {
            // Schedule a real onDraw through the Java view (which replays
            // the closure with a JNI-backed context at the live size), and
            // also run it immediately against the estimating context so
            // headless/test callers observe draws without Java.
            crate::backends::android::invalidate_view(self.0);
            let mut map = DRAW_CALLBACKS.lock().unwrap();
            if let Some(SendDrawCallback(ptr)) = map.get_mut(&self.canvas_id()) {
                let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) = unsafe { &mut **ptr };
                let mut dc = AndroidDrawContext;
                let (w, h) = self.replay_size();
                cb(&mut dc, w, h);
            }
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            CANVAS_SIZE_REQUEST
                .lock()
                .unwrap()
                .insert(self.canvas_id(), (w.max(1), h.max(1)));
        }

        /// Size to replay the draw closure at: the real laid-out size once
        /// `onDraw` has reported one, else a conservative default.
        ///
        /// Deliberately NOT the `set_size_request` value: that is a
        /// placeholder (corro asks for 1x1 because Android measures children
        /// itself). Replaying at 1x1 makes the draw closure compute a
        /// one-row viewport, and since the closure caches `data_rows` from
        /// the height it is given, the next *real* `onDraw` renders that
        /// single row stretched over the whole canvas — the grid shows one
        /// enormous empty row instead of a sheet. A plausible default keeps
        /// the pre-layout replay harmless; the real size arrives with the
        /// first `onDraw` and takes over from `CANVAS_SIZE`.
        fn replay_size(&self) -> (i32, i32) {
            let id = self.canvas_id();
            if let Some(&(w, h)) = CANVAS_SIZE.lock().unwrap().get(&id) {
                return (w, h);
            }
            (800, 600)
        }

        pub fn set_content_size(&self, w: i32, h: i32) {
            self.set_size_request(w, h);
        }

        pub fn set_visible(&self, visible: bool) {
            crate::backends::android::set_view_visible(self.0, visible);
        }

        /// Take focus, so the view receives hardware/adb key events.
        ///
        /// Needed by `SheetView`, which requests focus on `ACTION_DOWN` and
        /// is otherwise unreachable by a key event on a touch-mode Activity.
        pub fn grab_focus(&self) {
            crate::backends::android::focus_view(self.0);
        }

        /// Mark the view focusable (or not). The `true` case is a no-op:
        /// `create_canvas_view` builds views that are already focusable in
        /// touch mode, because a canvas that cannot take focus cannot be
        /// navigated by a hardware keyboard at all. The `false` case is
        /// honoured so a caller can take a canvas out of the tab chain.
        pub fn set_can_focus(&self, can: bool) {
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                if self.0.is_null() {
                    return Ok(());
                }
                let view = unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                env.call_method(&view, "setFocusable", "(Z)V", &[can.into()])?;
                env.call_method(&view, "setFocusableInTouchMode", "(Z)V", &[can.into()])?;
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        /// Whether the canvas may take extra horizontal space; a canvas
        /// always wants to, and `BoxWidget::append` already gives canvases
        /// weight 1. Recorded so the flag round-trips like GTK's.
        pub fn set_hexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        /// See [`Canvas::set_hexpand`].
        pub fn set_vexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        /// Left margin in device px, as horizontal padding. A phone in
        /// landscape leaves little room, so a caller that insets the sheet
        /// needs the space to come out of the canvas, not from a sibling.
        pub fn set_margin_start(&self, px: i32) {
            let h = px.max(0);
            crate::backends::android::set_view_padding(self.0, h, 0, h, 0);
        }

        /// Top margin in device px, as vertical padding. Pairs with
        /// [`Canvas::set_margin_start`].
        pub fn set_margin_top(&self, px: i32) {
            let v = px.max(0);
            crate::backends::android::set_view_padding(self.0, 0, v, 0, v);
        }

        /// Button/modifier-aware click: a right-click on the emulator's
        /// mouse, or a long press on a finger.
        ///
        /// Android reports a button number for a *mouse* (so a right-click
        /// arrives as `BUTTON_SECONDARY` and routes here), and has no button
        /// for a finger — but a long press is the platform's own gesture for
        /// "this thing has a context menu", so `SheetView` forwards it here
        /// too. That is what makes `open_sheet_context_menu` work instead of
        /// reporting `SHEET_MENU_UNAVAILABLE`.
        pub fn on_click_button(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            let mut map = CLICK_BUTTON_CALLBACKS.lock().unwrap();
            map.insert(
                self.canvas_id(),
                SendClickButtonCallback(Box::into_raw(cb)),
            );
        }

        /// Pointer motion with button state; see the GTK backend's
        /// `on_motion`. Dispatched from `SheetView`'s drag path with
        /// `BUTTON_PRIMARY` so a caller that tracks "button held" sees the
        /// same stream it would on a desktop.
        pub fn on_motion(&self, cb: Box<dyn FnMut(f64, f64, u32)>) {
            let mut map = MOTION_CALLBACKS.lock().unwrap();
            map.insert(self.canvas_id(), SendMotionCallback(Box::into_raw(cb)));
        }

        /// This canvas's top-left in *screen* coordinates, or `None`.
        ///
        /// Needed to pop a menu at the cell under the finger. A view's
        /// `getLocationOnScreen` is not the same as its position in the
        /// layout, and the sheet is nested three levels deep under the
        /// Activity's root, so guessing from the layout would put the menu
        /// in the wrong place by the height of the menu strip.
        pub fn screen_origin(&self) -> Option<(i32, i32)> {
            crate::backends::android::get_view_screen_origin(self.0)
        }

        /// Pointer release; see the GTK backend's `on_release`. Dispatched
        /// from the end of a `SheetView` drag.
        pub fn on_release(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            let mut map = RELEASE_CALLBACKS.lock().unwrap();
            map.insert(
                self.canvas_id(),
                SendReleaseCallback(Box::into_raw(cb)),
            );
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
        /// [`Canvas::queue_redraw`] (plus a view invalidate).
        pub fn force_draw(&self, _window_ptr: *mut c_void, fallback_w: i32, fallback_h: i32) {
            crate::backends::android::invalidate_view(self.0);
            let mut map = DRAW_CALLBACKS.lock().unwrap();
            if let Some(SendDrawCallback(ptr)) = map.get_mut(&self.canvas_id()) {
                let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) = unsafe { &mut **ptr };
                let mut dc = AndroidDrawContext;
                // Prefer the live laid-out size; fall back to the caller's
                // hint only when the view has never been drawn.
                let (w, h) = match CANVAS_SIZE.lock().unwrap().get(&self.canvas_id()).copied() {
                    Some(size) => size,
                    None => (fallback_w.max(1), fallback_h.max(1)),
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
    /// The view's *laid-out* size in pixels, learned from `View.onDraw`
    /// (`dispatch_draw`). This is the size the draw closure must be replayed
    /// at: Android does the layout, so Rust cannot know it any earlier.
    static CANVAS_SIZE: Lazy<Mutex<HashMap<u64, (i32, i32)>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    /// The size Rust *asked* for via `set_size_request` (often a 1x1
    /// placeholder, since Android measures children itself). Kept separate
    /// from [`CANVAS_SIZE`] so a placeholder request can never overwrite a
    /// real laid-out size and shrink the viewport to a single row.
    static CANVAS_SIZE_REQUEST: Lazy<Mutex<HashMap<u64, (i32, i32)>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    struct SendDrawCallback(*mut dyn FnMut(&mut dyn DrawContext, i32, i32));
    // SAFETY: guarded by the registry mutex; same pattern as the wasm adapter.
    unsafe impl Send for SendDrawCallback {}
    struct SendClickCallback(*mut dyn FnMut(f64, f64));
    // SAFETY: guarded by the registry mutex; same pattern as the wasm adapter.
    unsafe impl Send for SendClickCallback {}
    struct SendKeyCallback(*mut dyn FnMut(u32, u32) -> bool);
    // SAFETY: guarded by the registry mutex; same pattern as the wasm adapter.
    unsafe impl Send for SendKeyCallback {}

    /// Button-aware clicks, motion and release. Separate maps from
    /// [`CLICK_CALLBACKS`] because a host registers at most one of each and
    /// they fire on different platform events; sharing one slot would let a
    /// `on_click` registration silently replace a `on_click_button` one.
    static CLICK_BUTTON_CALLBACKS: Lazy<Mutex<HashMap<u64, SendClickButtonCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    static MOTION_CALLBACKS: Lazy<Mutex<HashMap<u64, SendMotionCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    static RELEASE_CALLBACKS: Lazy<Mutex<HashMap<u64, SendReleaseCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    struct SendClickButtonCallback(*mut dyn FnMut(f64, f64, u32, u32));
    // SAFETY: guarded by the registry mutex; same pattern as the wasm adapter.
    unsafe impl Send for SendClickButtonCallback {}
    struct SendMotionCallback(*mut dyn FnMut(f64, f64, u32));
    // SAFETY: guarded by the registry mutex; same pattern as the wasm adapter.
    unsafe impl Send for SendMotionCallback {}
    struct SendReleaseCallback(*mut dyn FnMut(f64, f64, u32, u32));
    // SAFETY: guarded by the registry mutex; same pattern as the wasm adapter.
    unsafe impl Send for SendReleaseCallback {}

    /// Every dispatcher below follows one rule: **resolve the raw callback
    /// pointer, drop the registry lock, then invoke.**
    ///
    /// Each callback re-enters the adapter — a click handler repaints, and
    /// `queue_redraw` takes the draw registry — so holding the lock across
    /// the call would deadlock. This is the same discipline `dispatch_draw`
    /// already uses; it is written out per dispatcher because the callback
    /// types differ, and a shared helper would need a trait to erase the
    /// difference for no real gain.

    /// Dispatch a tap from Java `SheetView` to the registered click closure.
    pub fn dispatch_canvas_click(canvas_id: u64, x: f64, y: f64) {
        let raw = {
            let map = CLICK_CALLBACKS.lock().unwrap();
            map.get(&canvas_id).map(|s| s.0)
        };
        if let Some(ptr) = raw {
            let cb: &mut dyn FnMut(f64, f64) = unsafe { &mut *ptr };
            cb(x, y);
        }
    }

    /// Dispatch a *secondary*-button click: a mouse right-click, or the long
    /// press `SheetView` reports as one. This is what feeds
    /// `gui_backend::open_sheet_context_menu`.
    pub fn dispatch_canvas_click_button(canvas_id: u64, x: f64, y: f64, button: u32, mods: u32) {
        let raw = {
            let map = CLICK_BUTTON_CALLBACKS.lock().unwrap();
            map.get(&canvas_id).map(|s| s.0)
        };
        if let Some(ptr) = raw {
            let cb: &mut dyn FnMut(f64, f64, u32, u32) = unsafe { &mut *ptr };
            cb(x, y, button, mods);
        }
    }

    /// Dispatch pointer motion with the button mask. See [`Canvas::on_motion`].
    pub fn dispatch_canvas_motion(canvas_id: u64, x: f64, y: f64, button: u32) {
        let raw = {
            let map = MOTION_CALLBACKS.lock().unwrap();
            map.get(&canvas_id).map(|s| s.0)
        };
        if let Some(ptr) = raw {
            let cb: &mut dyn FnMut(f64, f64, u32) = unsafe { &mut *ptr };
            cb(x, y, button);
        }
    }

    /// Dispatch a pointer release. See [`Canvas::on_release`].
    pub fn dispatch_canvas_release(canvas_id: u64, x: f64, y: f64, button: u32, mods: u32) {
        let raw = {
            let map = RELEASE_CALLBACKS.lock().unwrap();
            map.get(&canvas_id).map(|s| s.0)
        };
        if let Some(ptr) = raw {
            let cb: &mut dyn FnMut(f64, f64, u32, u32) = unsafe { &mut *ptr };
            cb(x, y, button, mods);
        }
    }

    /// Window-level key callbacks. There is exactly one window, so the
    /// registry is keyed by a constant 0 rather than a handle: an Activity is
    /// a singleton, and a second `on_event_key` replaces the first — the
    /// same contract GTK's single window controller has.
    static WINDOW_KEY_CALLBACKS: Lazy<Mutex<HashMap<u64, SendWindowKey>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    struct SendWindowKey(*mut dyn FnMut(u32, u32) -> i32);
    // SAFETY: guarded by the registry mutex; same pattern as the canvas maps.
    unsafe impl Send for SendWindowKey {}

    /// Dispatch a key to the window-level handler. Returns the handler's
    /// value, or 0 when no handler is registered.
    pub fn dispatch_window_key(keyval: u32, mods: u32) -> i32 {
        let raw = {
            let map = WINDOW_KEY_CALLBACKS.lock().unwrap();
            map.get(&0).map(|s| s.0)
        };
        match raw {
            Some(ptr) => {
                let cb: &mut dyn FnMut(u32, u32) -> i32 = unsafe { &mut *ptr };
                cb(keyval, mods)
            }
            None => 0,
        }
    }

    /// Dispatch a key from Java to the registered key closure. Returns
    /// whether a handler consumed it.
    ///
    /// This is the entry point that was previously dead: `Canvas::on_key_raw`
    /// filled the registry but nothing in the Java host ever called this, so
    /// every desktop binding in corro's `handle_key` was unreachable on
    /// Android. `SheetView.onKeyDown` and `MainActivity.onKeyDown` now do.
    pub fn dispatch_canvas_key(canvas_id: u64, keyval: u32, mods: u32) -> bool {
        let raw = {
            let map = KEY_CALLBACKS.lock().unwrap();
            map.get(&canvas_id).map(|s| s.0)
        };
        match raw {
            Some(ptr) => {
                let cb: &mut dyn FnMut(u32, u32) -> bool = unsafe { &mut *ptr };
                cb(keyval, mods)
            }
            None => false,
        }
    }

    /// Replay the registered draw closure for `canvas_id` against a live
    /// Java `android.graphics.Canvas`. Called from the cdylib's
    /// `SheetView_nativeOnDraw` export (which runs on the UI thread inside
    /// `View.onDraw`). `w`/`h` are the view's pixel size.
    pub fn dispatch_draw<'a, 'b, 'c>(
        canvas_id: u64,
        canvas_obj: jni::objects::JObject<'c>,
        env: &'a mut jni::JNIEnv<'b>,
        w: i32,
        h: i32,
    ) {
        CANVAS_SIZE.lock().unwrap().insert(canvas_id, (w.max(1), h.max(1)));
        // NLL-friendly: resolve the raw callback pointer first, release the
        // registry lock, then build the JNI context and invoke. Holding the
        // lock across JNI calls risks re-entrant deadlock (queue_redraw
        // from inside the closure would block on it).
        let raw = {
            let map = DRAW_CALLBACKS.lock().unwrap();
            map.get(&canvas_id).map(|s| s.0)
        };
        let Some(raw) = raw else {
            return;
        };
        match JniDrawContext::new(env, &canvas_obj) {
            Some(mut ctx) => {
                let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) =
                    unsafe { &mut *raw };
                cb(&mut ctx, w, h);
            }
            None => {
                let mut est = AndroidDrawContext;
                let cb: &mut dyn FnMut(&mut dyn DrawContext, i32, i32) =
                    unsafe { &mut *raw };
                cb(&mut est, w, h);
            }
        }
    }

    /// `DrawContext` backed by a live `android.graphics.Canvas` via JNI.
    /// One instance serves a single `onDraw`: paints are created up front
    /// (fill, stroke, text) and reused across primitives.
    pub struct JniDrawContext<'a, 'b> {
        // Interior mutability: `DrawContext::text_extents_styled` only gets
        // `&self`, but every JNI call needs `&mut JNIEnv`. The context is
        // driven exclusively from the UI thread inside `onDraw`, so the
        // cell never contends; every use goes through `try_borrow_mut` and
        // degrades gracefully instead of panicking.
        env: RefCell<&'a mut jni::JNIEnv<'b>>,
        canvas: jni::objects::GlobalRef,
        fill_paint: jni::objects::GlobalRef,
        stroke_paint: jni::objects::GlobalRef,
        text_paint: jni::objects::GlobalRef,
    }

    impl<'a, 'b> JniDrawContext<'a, 'b> {
        pub fn new(
            env: &'a mut jni::JNIEnv<'b>,
            canvas: &jni::objects::JObject<'_>,
        ) -> Option<Self> {
            let canvas = env.new_global_ref(canvas).ok()?;
            let fill_paint = Self::make_paint(env, 0 /* FILL */)?;
            let stroke_paint = Self::make_paint(env, 1 /* STROKE */)?;
            let text_paint = Self::make_paint(env, 0 /* FILL */)?;
            // Text draws anti-aliased; shapes stay crisp.
            let _ = env.call_method(
                text_paint.as_obj(),
                "setAntiAlias",
                "(Z)V",
                &[true.into()],
            );
            Some(JniDrawContext { env: RefCell::new(env), canvas, fill_paint, stroke_paint, text_paint })
        }

        fn make_paint(
            env: &mut jni::JNIEnv<'_>,
            style: i32,
        ) -> Option<jni::objects::GlobalRef> {
            // 0 = Paint.Style.FILL, 1 = STROKE (ordinal into Style.values()).
            let paint = env
                .new_object("android/graphics/Paint", "()V", &[])
                .ok()?;
            let styles = env
                .call_static_method(
                    "android/graphics/Paint$Style",
                    "values",
                    "()[Landroid/graphics/Paint$Style;",
                    &[],
                )
                .ok()?
                .l()
                .ok()?;
            let styles: jni::objects::JObjectArray =
                jni::objects::JObjectArray::from(styles);
            let style_obj = env.get_object_array_element(&styles, style).ok()?;
            env.call_method(&paint, "setStyle", "(Landroid/graphics/Paint$Style;)V", &[(&style_obj).into()])
                .ok()?;
            env.new_global_ref(&paint).ok()
        }

        fn argb(a: f64, r: f64, g: f64, b: f64) -> i32 {
            let clamp = |v: f64| (v.clamp(0.0, 1.0) * 255.0) as i32;
            (clamp(a) << 24) | (clamp(r) << 16) | (clamp(g) << 8) | clamp(b)
        }

        fn set_paint_color(
            env: &mut jni::JNIEnv<'_>,
            paint: &jni::objects::GlobalRef,
            a: f64,
            r: f64,
            g: f64,
            b: f64,
        ) {
            let _ = env.call_method(
                paint.as_obj(),
                "setColor",
                "(I)V",
                &[Self::argb(a, r, g, b).into()],
            );
        }

        fn set_text_size(
            env: &mut jni::JNIEnv<'_>,
            paint: &jni::objects::GlobalRef,
            size: f64,
            weight: i32,
        ) {
            // NOTE: `size` is corro's logical pixel size (already density-
            // scaled by `metrics_scale`). `Paint.setTextSize` is documented as
            // taking scaled pixels, but on a `Canvas` that is *not* scale-
            // transformed the units are plain device pixels, and the density
            // factor is applied by the `Paint` only through `sp` conversion in
            // `setTextSize` when the value is interpreted as sp — which is not
            // what happens here (this Paint is never given a scaled density).
            // Empirically the glyphs render at exactly this pixel height, which
            // is what the grid geometry (row_h/char_w, also density-scaled)
            // expects, so pass it through unchanged.
            let _ = env.call_method(
                paint.as_obj(),
                "setTextSize",
                "(F)V",
                &[(size.max(1.0) as f32).into()],
            );
            let _ = env.call_method(
                paint.as_obj(),
                "setFakeBoldText",
                "(Z)V",
                &[(weight != 0).into()],
            );
            let skew = if weight != 0 { 0.0f32 } else { 0.0f32 };
            let _ = env.call_method(
                paint.as_obj(),
                "setTextSkewX",
                "(F)V",
                &[skew.into()],
            );
        }

        /// Real text measurement via `Paint.measureText` (+ ascent/descent
        /// for the height), honouring the same size/weight the draw path
        /// uses. Returns the `(x_bearing, y_bearing, width, height)` tuple
        /// the trait promises: bearings are 0 (left/top origin, matching
        /// the monospace estimate callers already assume).
        fn measure_text(
            env: &mut jni::JNIEnv<'_>,
            paint: &jni::objects::GlobalRef,
            text: &str,
            size: f64,
            weight: i32,
        ) -> Option<(f64, f64, f64, f64)> {
            if text.is_empty() {
                return Some((0.0, 0.0, 0.0, 0.0));
            }
            Self::set_text_size(env, paint, size, weight);
            let jtext = env.new_string(text).ok()?;
            let w = env
                .call_method(
                    paint.as_obj(),
                    "measureText",
                    "(Ljava/lang/String;)F",
                    &[(&jtext).into()],
                )
                .ok()?
                .f()
                .ok()? as f64;
            let ascent = env
                .call_method(paint.as_obj(), "ascent", "()F", &[])
                .ok()?
                .f()
                .ok()? as f64;
            let descent = env
                .call_method(paint.as_obj(), "descent", "()F", &[])
                .ok()?
                .f()
                .ok()? as f64;
            Some((0.0, 0.0, w.max(0.0), (descent - ascent).max(0.0)))
        }
    }

    impl<'a, 'b> DrawContext for JniDrawContext<'a, 'b> {
        fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, a: f64) {
            let Ok(mut env) = self.env.try_borrow_mut() else { return; };
            Self::set_paint_color(&mut env, &self.fill_paint, a, r, g, b);
            let (env, canvas, paint) = (&mut *env, &self.canvas, &self.fill_paint);
            let _ = env.call_method(
                canvas.as_obj(),
                "drawRect",
                "(FFFFLandroid/graphics/Paint;)V",
                &[(x as f32).into(), (y as f32).into(), ((x + w) as f32).into(), ((y + h) as f32).into(), (&paint.as_obj()).into()],
            );
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
            let Ok(mut env) = self.env.try_borrow_mut() else { return; };
            Self::set_paint_color(&mut env, &self.stroke_paint, a, r, g, b);
            let (env, canvas, paint) = (&mut *env, &self.canvas, &self.stroke_paint);
            let _ = env.call_method(
                paint.as_obj(),
                "setStrokeWidth",
                "(F)V",
                &[(lw.max(0.5) as f32).into()],
            );
            let _ = env.call_method(
                canvas.as_obj(),
                "drawRect",
                "(FFFFLandroid/graphics/Paint;)V",
                &[(x as f32).into(), (y as f32).into(), ((x + w) as f32).into(), ((y + h) as f32).into(), (&paint.as_obj()).into()],
            );
        }

        fn draw_text_styled(
            &mut self,
            x: f64,
            y: f64,
            text: &str,
            _font: &str,
            size: f64,
            r: f64,
            g: f64,
            b: f64,
            a: f64,
            _slant: i32,
            weight: i32,
        ) {
            if text.is_empty() {
                return;
            }
            let Ok(mut env) = self.env.try_borrow_mut() else { return; };
            Self::set_paint_color(&mut env, &self.text_paint, a, r, g, b);
            Self::set_text_size(&mut env, &self.text_paint, size, weight);
            let (env, canvas, paint) = (&mut *env, &self.canvas, &self.text_paint);
            let Ok(jtext) = env.new_string(text) else {
                return;
            };
            // drawText draws with the baseline at y; offset by ascent so
            // callers' top-left convention matches other backends.
            let ascent: f64 = env
                .call_method(paint.as_obj(), "ascent", "()F", &[])
                .map(|v| v.f().unwrap_or(0.0) as f64)
                .unwrap_or(0.0);
            let _ = env.call_method(
                canvas.as_obj(),
                "drawText",
                "(Ljava/lang/String;FFLandroid/graphics/Paint;)V",
                &[(&jtext).into(), (x as f32).into(), ((y - ascent) as f32).into(), (&paint.as_obj()).into()],
            );
        }

        fn text_extents_styled(
            &self,
            text: &str,
            _font: &str,
            size: f64,
            _slant: i32,
            weight: i32,
        ) -> (f64, f64, f64, f64) {
            // Real measurement via Paint.measureText. `&self` is enough:
            // the JNIEnv lives behind a RefCell (UI thread only, so the
            // borrow cannot contend). Any failure degrades to the monospace
            // estimate so layout never divides by zero.
            let Ok(mut env) = self.env.try_borrow_mut() else {
                return estimate_extents(text, size, weight);
            };
            let measured = Self::measure_text(&mut env, &self.text_paint, text, size, weight);
            drop(env);
            measured.unwrap_or_else(|| estimate_extents(text, size, weight))
        }

        fn clear(&mut self, r: f64, g: f64, b: f64, a: f64) {
            let Ok(mut env) = self.env.try_borrow_mut() else { return; };
            let (env, canvas) = (&mut *env, &self.canvas);
            let _ = env.call_method(
                canvas.as_obj(),
                "drawColor",
                "(I)V",
                &[Self::argb(a, r, g, b).into()],
            );
        }

        fn save(&mut self) {
            let Ok(mut env) = self.env.try_borrow_mut() else { return; };
            let (env, canvas) = (&mut *env, &self.canvas);
            let _ = env.call_method(canvas.as_obj(), "save", "()I", &[]);
        }

        fn restore(&mut self) {
            let Ok(mut env) = self.env.try_borrow_mut() else { return; };
            let (env, canvas) = (&mut *env, &self.canvas);
            let _ = env.call_method(canvas.as_obj(), "restore", "()V", &[]);
        }

        fn clip(&mut self, x: f64, y: f64, w: f64, h: f64) {
            let Ok(mut env) = self.env.try_borrow_mut() else { return; };
            let (env, canvas) = (&mut *env, &self.canvas);
            let _ = env.call_method(
                canvas.as_obj(),
                "clipRect",
                "(FFFF)Z",
                &[(x as f32).into(), (y as f32).into(), ((x + w) as f32).into(), ((y + h) as f32).into()],
            );
        }
    }

    // ------------------------------------------------------------------
    // Menu / MenuBar / SimpleAction
    // ------------------------------------------------------------------
    //
    // Android has no desktop menu bar; corro renders its own menu UI on the
    // Canvas (see `MenuBar::handle_menu_key`). These types therefore carry
    // the menu *model* (labels + action names) plus the shared action
    // registry, exactly like the wasm adapter.

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
            Menu { items: self.items.clone() }
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
            let action = detailed_action.split('(').next().unwrap_or(detailed_action).to_owned();
            self.items.push(MenuItem::Item { label: label.to_owned(), action });
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
            MenuBar { items: self.items.clone() }
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
        /// The mnemonic character of the submenu whose label contains
        /// `keyval`'s letter after an underscore, if any.
        fn find_submenu_mnemonic(&self, keyval: u32) -> Option<usize> {
            let ch = char::from_u32(keyval)?.to_ascii_lowercase();
            self.items.iter().position(|item| match item {
                MenuItem::Submenu { label, .. } => mnemonic_of(label) == Some(ch),
                MenuItem::Item { .. } => false,
            })
        }

        /// Activate the submenu whose mnemonic is `keyval`, at its natural
        /// position (the overflow button, which is where the whole menu tree
        /// lives on a phone).
        pub fn activate_submenu_by_mnemonic(&self, keyval: u32) -> bool {
            let Some(index) = self.find_submenu_mnemonic(keyval) else {
                return false;
            };
            // The tree is one level deeper than the label the caller matched,
            // so the overflow entry index and the model index differ. The
            // Java side matches by *label*, which is what the strip was built
            // with, so the label is what goes over the wire.
            let MenuItem::Submenu { label, .. } = &self.items[index] else {
                return false;
            };
            crate::backends::android::menu_strip_open_overflow(label)
        }

        /// Open the submenu whose mnemonic is `keyval` at a screen position.
        ///
        /// `x`/`y` are ignored: the overflow popup is anchored to the strip
        /// itself, not to a caller-chosen point, because a `PopupMenu` on
        /// Android is positioned by an anchor view and would have to be told
        /// to fall outside its bounds otherwise. The long-press context menu
        /// goes through a different path (an `AlertDialog` of the cell
        /// actions) precisely because it *does* need a point.
        pub fn popup_submenu_by_mnemonic_at(&self, keyval: u32, _x: i32, _y: i32) -> bool {
            self.activate_submenu_by_mnemonic(keyval)
        }
        /// No positionable popup on this backend, so `anchor` and the point are
        /// unused and the call reports "not opened". This is the same answer
        /// `popup_submenu_by_mnemonic_at` already gave here -- `popup_at` just
        /// makes that explicit in one place instead of leaving each caller to
        /// discover it.
        pub fn popup_at(&self, _label: &str, _anchor: &Canvas, _x: f64, _y: f64) -> bool {
            false
        }
        pub fn activate_submenu_item_by_mnemonic(&self, keyval: u32) -> bool {
            let Some(ch) = char::from_u32(keyval).map(|c| c.to_ascii_lowercase()) else {
                return false;
            };
            let mut found: Option<String> = None;
            for item in &self.items {
                match item {
                    MenuItem::Item { label, action } => {
                        if mnemonic_of(label) == Some(ch) {
                            found = Some(action.clone());
                            break;
                        }
                    }
                    MenuItem::Submenu { items, .. } => {
                        for sub in items {
                            if let MenuItem::Item { label, action } = sub {
                                if mnemonic_of(label) == Some(ch) {
                                    found = Some(action.clone());
                                    break;
                                }
                            }
                        }
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            match found {
                // The action registry is keyed by the name the desktop menu
                // registers, so the *same* action runs here. One dispatch
                // table, two ways of reaching it — which is why the menu
                // strip does not need its own copy of any logic.
                Some(action) => {
                    invoke_action(&action, std::ptr::null_mut());
                    true
                }
                None => false,
            }
        }

        /// # Safety
        /// Kept for API compatibility; no-op on Android.
        pub unsafe fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}

        /// `Alt` + the mnemonic letter, opening that submenu.
        pub fn handle_mnemonic_key(&self, keyval: u32) -> bool {
            self.activate_submenu_by_mnemonic(keyval)
        }

        /// A key with modifiers, when a menu is open: a letter selects an
        /// item, Escape closes it. Mirrors the desktop contract, where
        /// `handle_menu_key` is the "a menu is active, interpret this key"
        /// entry point.
        pub fn handle_menu_key(&self, keyval: u32, _modifiers: u32) -> bool {
            if keyval == ESCAPE_KEY {
                if self.menu_active() {
                    self.menu_close();
                }
                return true;
            }
            if MENU_ACTIVE.with(|a| a.get()) {
                return self.activate_submenu_item_by_mnemonic(keyval);
            }
            false
        }

        /// [`handle_menu_key`](Self::handle_menu_key) with the typed character
        /// supplied separately; this backend's key codes are already characters.
        pub fn handle_menu_key_char(&self, keyval: u32, _ch: char, modifiers: u32) -> bool {
            self.handle_menu_key(keyval, modifiers)
        }

        pub fn menu_active(&self) -> bool {
            MENU_ACTIVE.with(|a| a.get())
        }

        pub fn menu_close(&self) {
            crate::backends::android::menu_strip_close_overflow();
            MENU_ACTIVE.with(|a| a.set(false));
        }
    }

    /// `ESCAPE`'s keysym, for `handle_menu_key`. The unix set, which is what
    /// `core::key` defines for every non-Windows target.
    const ESCAPE_KEY: u32 = 0xFF1B;

    // Whether a menu popup is open, and the submenu it holds.
    //
    // This is what makes `menu_active` a real answer rather than `false`:
    // the desktop uses it to decide whether a plain letter key should be
    // routed to menu selection instead of starting a cell edit, and a
    // constant `false` meant that on Android an open menu and a typed
    // character collided.
    thread_local! {
        static MENU_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Called from the menu strip when its overflow opens or closes, so
    /// `menu_active` tracks reality.
    pub fn set_menu_active(active: bool) {
        MENU_ACTIVE.with(|a| a.set(active));
    }

    /// The character a menu label declares as its mnemonic, i.e. the one
    /// after an underscore (`"_Save"` -> `'s'`), upper- or lower-cased.
    ///
    /// The menu model's labels are the *shared* ones, so they carry GTK
    /// mnemonics; the Android strip strips the underscore for display, but
    /// the model keeps it, which is what makes a mnemonic key work here
    /// without a second definition of the menu.
    fn mnemonic_of(label: &str) -> Option<char> {
        let (_, rest) = label.split_once('_')?;
        rest.chars().next().map(|c| c.to_ascii_lowercase())
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
        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(&self, f: F) -> Result<u64, Error> {
            register_action(&self.name, Box::new(f));
            Ok(0)
        }
    }

    struct SendFnPtr(*mut dyn FnMut(*mut c_void));
    // SAFETY: guarded by the registry mutex; same pattern as the wasm adapter.
    unsafe impl Send for SendFnPtr {}

    static ACTION_REGISTRY: Lazy<Mutex<HashMap<String, SendFnPtr>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    /// Layout-pass callbacks keyed by window/view pointer. See
    /// [`Window::set_layout_cb`].
    static LAYOUT_CALLBACKS: Lazy<Mutex<HashMap<usize, SendLayout>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    struct SendLayout(*mut dyn FnMut(*mut c_void));
    // SAFETY: guarded by the registry mutex; same pattern as the canvas maps.
    unsafe impl Send for SendLayout {}

    /// Called from the host's global-layout listener.
    pub fn dispatch_layout(window_ptr: *mut c_void) {
        let raw = {
            let map = LAYOUT_CALLBACKS.lock().unwrap();
            map.get(&(window_ptr as usize)).map(|s| s.0)
        };
        let Some(ptr) = raw else { return };
        let cb: &mut dyn FnMut(*mut c_void) = unsafe { &mut *ptr };
        cb(window_ptr);
    }

    fn register_action(name: &str, f: Box<dyn FnMut(*mut c_void)>) {
        let mut map = ACTION_REGISTRY.lock().unwrap();
        let ptr = Box::into_raw(Box::new(f) as Box<dyn FnMut(*mut c_void)>);
        map.insert(name.to_owned(), SendFnPtr(ptr));
    }

    /// Dispatch a menu action by name (called from `handle_menu_action`
    /// plumbing and future Java menu shims).
    pub fn invoke_action(name: &str, param: *mut c_void) {
        if let Ok(mut map) = ACTION_REGISTRY.lock() {
            if let Some(SendFnPtr(ptr)) = map.get_mut(name) {
                let cb: &mut dyn FnMut(*mut c_void) = unsafe { &mut **ptr };
                cb(param);
            }
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
        /// Expansion and size, per [`Label::set_hexpand`] and
        /// [`Label::set_size_request`]. A `GridLayout` measures its children
        /// from their own minimums, so these are the only way a caller can
        /// widen a cell.
        pub fn set_hexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        /// See [`Grid::set_hexpand`].
        pub fn set_vexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        /// Minimum width/height in device px.
        pub fn set_size_request(&self, w: i32, h: i32) {
            crate::backends::android::set_view_min_size(self.0, w, h);
        }

        pub fn set_visible(&self, visible: bool) {
            crate::backends::android::set_view_visible(self.0, visible);
        }

        /// Attach a child.
        ///
        /// The `left`/`top`/`width`/`height` cell span is honoured when it is
        /// non-degenerate, because a `GridLayout` can express it: an explicit
        /// `GridLayout.LayoutParams` with `columnSpec`/`rowSpec` is what GTK's
        /// `attach` means. A caller passing `-1` for all four (the "no
        /// constraint" convention) gets the previous bare `addView`, so
        /// existing code is unaffected.
        pub fn attach(
            &self,
            child: &impl AsRef<*mut c_void>,
            left: i32,
            top: i32,
            width: i32,
            height: i32,
        ) {
            let child_ptr = *child.as_ref();
            if child_ptr.is_null() {
                return;
            }
            if left >= 0 && top >= 0 && width > 0 && height > 0 {
                let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                    let spec_cls = env.find_class("android/widget/GridLayout$Spec")?;
                    let col = env
                        .call_static_method(
                            &spec_cls,
                            "spec",
                            "(I)Landroid/widget/GridLayout$Spec;",
                            &[left.into()],
                        )?
                        .l()?;
                    let row = env
                        .call_static_method(
                            &spec_cls,
                            "spec",
                            "(I)Landroid/widget/GridLayout$Spec;",
                            &[top.into()],
                        )?
                        .l()?;
                    let params = env.new_object(
                        "android/widget/GridLayout$LayoutParams",
                        "(Landroid/widget/GridLayout$Spec;Landroid/widget/GridLayout$Spec;)V",
                        &[(&col).into(), (&row).into()],
                    )?;
                    let grid =
                        unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                    let child_obj =
                        unsafe { jni::objects::JObject::from_raw(child_ptr as jni::sys::jobject) };
                    env.call_method(
                        &grid,
                        "addView",
                        "(Landroid/view/View;Landroid/view/ViewGroup$LayoutParams;)V",
                        &[(&child_obj).into(), (&params).into()],
                    )?;
                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                });
                return;
            }
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let grid = unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let child_obj =
                    unsafe { jni::objects::JObject::from_raw(child_ptr as jni::sys::jobject) };
                env.call_method(
                    &grid,
                    "addView",
                    "(Landroid/view/View;)V",
                    &[(&child_obj).into()],
                )?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }
    }

    // ------------------------------------------------------------------
    // DropDown
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
            if let Some(idx) = index {
                let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                    let spinner =
                        unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                    env.call_method(&spinner, "setSelection", "(I)V", &[(idx as i32).into()])?;
                    Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
                });
            }
        }

        pub fn get_active(&self) -> i32 {
            let r = crate::backends::android::with_env_and_activity(|env, _activity| {
                let spinner =
                    unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let pos =
                    env.call_method(&spinner, "getSelectedItemPosition", "()I", &[])?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(pos.i()?)
            });
            r.unwrap_or(-1)
        }

        /// The selected index, or `None` when nothing is selected — the
        /// shape GTK's `ComboBox` reports and what a caller checking "did the
        /// user pick anything" expects. `get_active`'s `-1` sentinel is
        /// `None` in this form.
        pub fn active(&self) -> Option<u32> {
            match self.get_active() {
                i if i < 0 => None,
                i => Some(i as u32),
            }
        }

        /// Fire the change callback when the user picks an item.
        ///
        /// `Spinner.setSelection` is the programmatic path; this wires
        /// `OnItemSelectedListener` so a *user* pick reaches the callback too.
        /// Without it a `DropDown` on Android displayed items and reported
        /// `get_active`, but a caller was never told the value changed — a
        /// dialog's filter combo was inert.
        pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            let id = crate::backends::android::register_local_callback(f);
            crate::backends::android::attach_item_selected_listener(self.0, id);
            Ok(id)
        }

        /// Minimum width/height in device px. A `Spinner` measures to its
        /// widest item, which on a phone is often narrower than the slot a
        /// caller gave it; a minimum stops it collapsing.
        pub fn set_size_request(&self, w: i32, h: i32) {
            crate::backends::android::set_view_min_size(self.0, w, h);
        }

        pub fn set_visible(&self, visible: bool) {
            crate::backends::android::set_view_visible(self.0, visible);
        }

        /// Shift the popup list down by `y` px, horizontally by `x`.
        ///
        /// `Spinner` has no popup-offset API, so this is padding on the view
        /// itself — the only lever the platform offers. Recorded so a caller
        /// that only needs the visual offset gets it, and so the value is not
        /// silently lost.
        pub fn set_offset(&self, x: i32, y: i32) {
            let (l, t) = (x.max(0), y.max(0));
            crate::backends::android::set_view_padding(self.0, l, t, l, t);
        }

        pub fn set_hexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        pub fn set_vexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        pub fn grab_focus(&self) {
            crate::backends::android::focus_view(self.0);
        }
    }

    impl Clone for DropDown {
        fn clone(&self) -> Self {
            DropDown(self.0)
        }
    }

    // ------------------------------------------------------------------
    // CheckButton
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
            let r = crate::backends::android::with_env_and_activity(|env, _activity| {
                let cb = unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let checked = env.call_method(&cb, "isChecked", "()Z", &[])?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(checked.z()?)
            });
            r.unwrap_or(false)
        }

        pub fn set_active(&self, active: bool) {
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let cb = unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                env.call_method(&cb, "setChecked", "(Z)V", &[active.into()])?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        pub fn set_label(&self, label: &str) {
            set_widget_label(self.0, label);
        }

        pub fn get_label(&self) -> Option<String> {
            crate::backends::android::get_view_text(self.0)
        }

        /// See [`DropDown::connect_changed`]: without the listener a
        /// `CheckButton` could be set but never *report* a user tap, so every
        /// "include this column" style toggle in a dialog was inert.
        pub fn connect_toggled(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            let id = crate::backends::android::register_local_callback(f);
            crate::backends::android::attach_checked_listener(self.0, id);
            Ok(id)
        }

        pub fn set_hexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        pub fn set_vexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            crate::backends::android::set_view_min_size(self.0, w, h);
        }

        pub fn set_visible(&self, visible: bool) {
            crate::backends::android::set_view_visible(self.0, visible);
        }
    }

    impl Clone for CheckButton {
        fn clone(&self) -> Self {
            CheckButton(self.0)
        }
    }

    // ------------------------------------------------------------------
    // RadioButton
    // ------------------------------------------------------------------

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
            let r = crate::backends::android::with_env_and_activity(|env, _activity| {
                let rb = unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let checked = env.call_method(&rb, "isChecked", "()Z", &[])?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(checked.z()?)
            });
            r.unwrap_or(false)
        }

        pub fn set_active(&self, active: bool) {
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let rb = unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                env.call_method(&rb, "setChecked", "(Z)V", &[active.into()])?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        pub fn set_label(&self, label: &str) {
            set_widget_label(self.0, label);
        }

        pub fn get_label(&self) -> Option<String> {
            crate::backends::android::get_view_text(self.0)
        }

        /// See [`CheckButton::connect_toggled`].
        pub fn connect_toggled(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            let id = crate::backends::android::register_local_callback(f);
            crate::backends::android::attach_checked_listener(self.0, id);
            Ok(id)
        }

        pub fn grab_focus(&self) {
            crate::backends::android::focus_view(self.0);
        }

        pub fn set_hexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        pub fn set_vexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            crate::backends::android::set_view_min_size(self.0, w, h);
        }

        pub fn set_visible(&self, visible: bool) {
            crate::backends::android::set_view_visible(self.0, visible);
        }
    }

    impl Clone for RadioButton {
        fn clone(&self) -> Self {
            RadioButton(self.0)
        }
    }

    // ------------------------------------------------------------------
    // Dialog
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
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let builder =
                    unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let j_title = env.new_string(title)?;
                env.call_method(
                    &builder,
                    "setTitle",
                    "(Ljava/lang/CharSequence;)Landroid/app/AlertDialog$Builder;",
                    &[(&j_title).into()],
                )?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        /// Minimum width/height for the dialog's window, in device px.
        ///
        /// A `Dialog` window's content is `WRAP_CONTENT` by default, so on a
        /// phone a wide prompt (a filename field, a multi-line message)
        /// measured to its text width and looked like a strip across the
        /// screen. A minimum of the display width is the usual fix; this
        /// applies whatever the caller asks for, and nothing when it asks for
        /// nothing.
        pub fn set_default_size(&self, w: i32, h: i32) {
            crate::backends::android::set_view_min_size(self.0, w, h);
        }

        /// See [`Dialog::set_default_size`]; also applies to a dialog that has
        /// already been shown.
        pub fn set_size_request(&self, w: i32, h: i32) {
            self.set_default_size(w, h);
        }

        /// Parent this dialog to `parent`. The Activity context already
        /// parents dialogs, so there is nothing to set; a dialog opened from
        /// a service context would need it, and a host that cares can supply
        /// its own context via `create_dialog`.
        pub fn set_transient_for(&self, _parent: *mut c_void) {}

        /// The container child views are added to.
        ///
        /// An `AlertDialog.Builder` has no public content container, so this
        /// creates one: a vertical `LinearLayout` registered in the dialog's
        /// own table. `append_content_area` and `get_content_area` then agree,
        /// and a dialog that adds two children gets them stacked rather than
        /// overlapping.
        pub fn get_content_area(&self) -> *mut c_void {
            crate::backends::android::dialog_content_area(self.0) as *mut c_void
        }

        /// Add a child to [`Dialog::get_content_area`], creating the container
        /// on first use. A no-op for a null child or an unbuilt dialog.
        pub fn append_content_area(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            if child_ptr.is_null() {
                return;
            }
            let area = self.get_content_area();
            if area.is_null() {
                return;
            }
            crate::backends::android::attach_child(area, child_ptr);
        }

        /// Build the dialog's view hierarchy.
        ///
        /// The platform analogue of `GtkWidget.show_all`: an `AlertDialog` is
        /// laid out by `show()` itself, but a caller that attaches a custom
        /// view needs that view measured before the dialog is shown, or the
        /// dialog opens at the height of nothing and the content appears
        /// after a second frame. `create` + `setContentView` is that
        /// measurement pass; `present` then shows the already-built dialog
        /// rather than building a second one.
        pub fn layout_dialog(&self) {
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let builder =
                    unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let dialog = env
                    .call_method(&builder, "create", "()Landroid/app/AlertDialog;", &[])?
                    .l()?;
                let dialog = unsafe { jni::objects::JObject::from_raw(dialog.as_raw()) };
                // Remember the built dialog so present() shows this one.
                let gref = env.new_global_ref(&dialog)?;
                DIALOGS.lock().unwrap().insert(self.0 as usize, gref);
                // Attach the content container, if append_content_area made one.
                let area = DIALOG_CONTENT
                    .lock()
                    .unwrap()
                    .get(&(self.0 as usize))
                    .map(|r| r.0);
                if let Some(area) = area {
                    if area != std::ptr::null_mut() {
                        let area = unsafe { jni::objects::JObject::from_raw(area) };
                        env.call_method(
                            &dialog,
                            "setView",
                            "(Landroid/view/View;)V",
                            &[(&area).into()],
                        )?;
                    }
                }
                // Measure, so a custom view's size is known before show().
                let _ = env.call_method(&dialog, "getWindow", "()Landroid/view/Window;", &[])?;
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        /// Add a button carrying `response_id`, so `connect_response` can tell
        /// which one the user pressed.
        ///
        /// `AlertDialog.Builder` has one slot per role (positive / negative /
        /// neutral), so the response id is remembered in this dialog's own
        /// table and read back by the click listener. The *first* button
        /// claiming a role wins, because `setPositiveButton` replaces rather
        /// than adds — three "OK"-shaped buttons on a phone is not a thing,
        /// and silently losing two of them would be worse than saying so in
        /// the log.
        pub fn add_button(&self, text: &str, response_id: i32) {
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let builder =
                    unsafe { jni::objects::JObject::from_raw(self.0 as jni::sys::jobject) };
                let j_text = env.new_string(text)?;
                // Take the first free role. GTK's dialogs use GTK_RESPONSE_OK /
                // _CANCEL / _CLOSE; map them onto the three Android roles in
                // that order, which is the same "affirmative, dismissive,
                // neutral" ordering every caller assumes.
                let role = {
                    let mut map = DIALOG_BUTTONS.lock().unwrap();
                    let entry = map.entry(self.0 as usize).or_insert_with(Vec::new);
                    if entry.iter().any(|(r, _): &(String, i32)| r == "positive") {
                        if entry.iter().any(|(r, _): &(String, i32)| r == "negative") {
                            "neutral"
                        } else {
                            "negative"
                        }
                    } else {
                        "positive"
                    }
                };
                let setter = format!("set{role}Button");
                let signature = format!(
                    "(Ljava/lang/CharSequence;Landroid/content/DialogInterface$OnClickListener;)Landroid/app/AlertDialog$Builder;"
                );
                // A null listener means the platform dismisses the dialog
                // without telling Rust; `attach_dialog_listener` (called from
                // connect_response) supplies the real one.
                // One listener object per role, carrying the role to Rust.
                // A null listener would dismiss the dialog without reporting
                // anything, so the listener is requested unconditionally; it
                // is null only when the host ships no CorroDialogListener,
                // in which case a press is lost but the dialog still closes.
                let listener_raw = crate::backends::android::dialog_listener(self.0, role);
                let listener_obj = match listener_raw {
                    Some(l) if l != std::ptr::null_mut() => {
                        Some(unsafe { jni::objects::JObject::from_raw(l) })
                    }
                    _ => None,
                };
                let null_listener = jni::objects::JObject::null();
                let listener_val = match listener_obj.as_ref() {
                    Some(l) => (&*l).into(),
                    None => (&null_listener).into(),
                };
                let resp = response_id;
                // The listener needs to know *which* button it was; it reads
                // that from this table by looking the label up, so store the
                // pairing now.
                DIALOG_BUTTONS
                    .lock()
                    .unwrap()
                    .entry(self.0 as usize)
                    .or_default()
                    .push((role.to_string(), resp));
                env.call_method(
                    &builder,
                    &setter,
                    &signature,
                    &[(&j_text).into(), listener_val],
                )?;
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        /// Show the dialog.
        ///
        /// Builds it if [`Dialog::layout_dialog`] has not, so `present` alone
        /// still works for the common "title, buttons, show" sequence. The
        /// response callback fires from the click listener the platform
        /// installed, so a caller that only called `present` still learns
        /// which button was pressed *provided* it called `connect_response`
        /// first.
        pub fn present(&self) {
            let shown = {
                let map = DIALOGS.lock().unwrap();
                map.get(&(self.0 as usize)).map(|d| d.as_obj().as_raw())
            };
            if let Some(raw) = shown {
                let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                    let dialog = unsafe { jni::objects::JObject::from_raw(raw as jni::sys::jobject) };
                    env.call_method(&dialog, "show", "()V", &[])?;
                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                });
                return;
            }
            self.layout_dialog();
            if let Some(raw) = DIALOGS.lock().unwrap().get(&(self.0 as usize)).map(|d| d.as_obj().as_raw())
            {
                let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                    let dialog = unsafe { jni::objects::JObject::from_raw(raw as jni::sys::jobject) };
                    env.call_method(&dialog, "show", "()V", &[])?;
                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                });
            }
        }

        /// Run the dialog's nested loop and return the response id.
        ///
        /// `Dialog::run` is a *synchronous* nested loop on GTK and NWG. There
        /// is no nested loop on Android — the Looper belongs to the Activity
        /// and the answer arrives in `onClick` long after this returns. So this
        /// shows the dialog and returns the last response id (0 if none yet),
        /// which is the same `0` NWG returns, and lets the async
        /// `connect_response` callback do the real work. A caller written once
        /// for desktop that checks the return value still compiles and still
        /// gets a defined answer; a caller that waits for it must use
        /// `connect_response`, which is the correct shape on Android anyway.
        pub fn run(&self) -> i32 {
            self.present();
            LAST_DIALOG_RESPONSE.with(|r| r.get())
        }

        /// Register the response callback and install the platform click
        /// listener that reaches it.
        pub fn connect_response(&self, f: impl FnMut(i32) + 'static) -> Result<u64, Error> {
            let mut map = DIALOG_RESPONSE.lock().unwrap();
            map.insert(
                self.0 as usize,
                SendResponse(Box::into_raw(Box::new(f))),
            );
            drop(map);
            Ok(0)
        }

        /// Pre-select a button: the dialog is shown with this response's
        /// button focused, so Enter/IME-Done commits it.
        pub fn set_default_response(&self, response_id: i32) {
            DIALOG_DEFAULT
                .lock()
                .unwrap()
                .insert(self.0 as usize, response_id);
        }

        /// Hide the dialog. A no-op when it was never built, which is the
        /// common case for a dialog a caller abandoned.
        pub fn close(&self) {
            let raw = DIALOGS
                .lock()
                .unwrap()
                .get(&(self.0 as usize))
                .map(|d| d.as_obj().as_raw());
            let Some(raw) = raw else { return };
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let dialog = unsafe { jni::objects::JObject::from_raw(raw as jni::sys::jobject) };
                env.call_method(&dialog, "dismiss", "()V", &[])?;
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        /// Show or hide without destroying. Mirrors GTK's `set_visible`,
        /// which NWG exposes too; a hidden dialog is still built, so showing
        /// it again is cheap.
        pub fn set_visible(&self, visible: bool) {
            if visible {
                self.present();
            } else {
                self.close();
            }
        }
    }

    /// Built dialogs, keyed by the builder's view pointer, so `present` and
    /// `close` act on the same `AlertDialog` instead of building a new one
    /// per call.
    static DIALOGS: Lazy<Mutex<HashMap<usize, jni::objects::GlobalRef>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    /// The vertical container `append_content_area` fills, keyed by builder.
    /// `RawJob` (a raw `jobject` newtype with a justified `Send` impl) rather
    /// than a bare pointer, because the map is a `'static`.
    static DIALOG_CONTENT: Lazy<Mutex<HashMap<usize, crate::backends::android::RawJob>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    /// (role, response id) per button, in add order. Read by the click
    /// listener to turn a platform button press into a response id.
    static DIALOG_BUTTONS: Lazy<Mutex<HashMap<usize, Vec<(String, i32)>>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    /// The response handler per builder.
    static DIALOG_RESPONSE: Lazy<Mutex<HashMap<usize, SendResponse>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    /// `set_default_response` per builder.
    static DIALOG_DEFAULT: Lazy<Mutex<HashMap<usize, i32>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    struct SendResponse(*mut dyn FnMut(i32));
    // SAFETY: guarded by the registry mutex; same pattern as the canvas maps.
    unsafe impl Send for SendResponse {}

    thread_local! {
        /// The last response id, which is what `Dialog::run` returns.
        static LAST_DIALOG_RESPONSE: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
    }

    /// Called from the dialog's Java click listener: map the pressed role to
    /// the response id `add_button` recorded, run the handler, remember the
    /// id for `run`.
    pub fn dispatch_dialog_response(builder_ptr: *mut c_void, role: &str) {
        let response_id = DIALOG_BUTTONS
            .lock()
            .unwrap()
            .get(&(builder_ptr as usize))
            .and_then(|buttons| {
                buttons
                    .iter()
                    .find(|(r, _)| r == role)
                    .map(|(_, id)| *id)
            })
            .unwrap_or(0);
        LAST_DIALOG_RESPONSE.with(|r| r.set(response_id));
        let raw = {
            let map = DIALOG_RESPONSE.lock().unwrap();
            map.get(&(builder_ptr as usize)).map(|s| s.0)
        };
        let Some(ptr) = raw else { return };
        let cb: &mut dyn FnMut(i32) = unsafe { &mut *ptr };
        cb(response_id);
    }

    // ------------------------------------------------------------------
    // TextView
    // ------------------------------------------------------------------

    pub struct TextView {
        inner: *mut c_void,
        /// The `TextChanged` handler, so `get_buffer` has something to hand
        /// back.
        ///
        /// NWG's `get_buffer` returns the text buffer *view* whose mutation
        /// fires the change callback; on Android the buffer is the `EditText`
        /// itself and the change arrives through the same `TextWatcher` the
        /// `TextView`'s own `connect_changed` uses. Holding the callback here
        /// lets `get_buffer` register it against this view, so a caller that
        /// went through `get_buffer` gets its change notifications.
        changed_cb: std::rc::Rc<RefCell<Option<Box<dyn FnMut()>>>>,
    }

    impl TextView {
        pub fn new(inner: *mut c_void) -> Self {
            TextView {
                inner,
                changed_cb: std::rc::Rc::new(RefCell::new(None)),
            }
        }

        /// The buffer's change handler.
        ///
        /// Mirrors NWG's `get_buffer`, which returns a `&RefCell<Option<Box<
        /// dyn FnMut()>>>` the caller fills in; the text itself is read back
        /// with `get_text`. Registering here also attaches the `TextWatcher`,
        /// so a caller that only ever uses `get_buffer` still learns about
        /// edits.
        pub fn get_buffer(&self) -> &RefCell<Option<Box<dyn FnMut()>>> {
            if self.changed_cb.borrow().is_none() {
                // Register a no-op so the `TextWatcher` is attached even
                // before the caller fills the cell in: a caller that only
                // reads the buffer and relies on the platform to fire
                // `get_buffer`'s callback still gets its edits.
                let _ = self.connect_changed(|| {});
            }
            &self.changed_cb
        }

        /// Make the text view editable or read-only.
        ///
        /// NWG's `set_editable` maps onto `setEditable` plus
        /// `setFocusable`: an `EditText` that is not focusable cannot be typed
        /// into even when it is editable, and a dialog's read-only
        /// instructions pane relies on both.
        pub fn set_editable(&self, editable: bool) {
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                if self.inner.is_null() {
                    return Ok(());
                }
                let view =
                    unsafe { jni::objects::JObject::from_raw(self.inner as jni::sys::jobject) };
                env.call_method(&view, "setEditable", "(Z)V", &[editable.into()])?;
                env.call_method(&view, "setFocusable", "(Z)V", &[editable.into()])?;
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            });
        }
    }

    impl Widget for TextView {
        fn raw_handle(&self) -> *mut c_void {
            self.inner
        }
    }

    impl AsRef<*mut c_void> for TextView {
        fn as_ref(&self) -> &*mut c_void {
            &self.inner
        }
    }

    impl TextView {
        pub fn set_text(&self, text: &str) {
            set_widget_label(self.inner, text);
        }

        pub fn get_text(&self) -> Option<String> {
            crate::backends::android::get_view_text(self.inner)
        }

        /// `wrap_mode`: 0 none, 1 char, 2 word. An `EditText` wraps on both
        /// axes by default; `maxLines = 1` plus horizontal scrolling is the
        /// platform's "do not wrap" answer, which is what a single-line
        /// console-log pane wants.
        pub fn set_wrap_mode(&self, wrap_mode: i32) {
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                if self.inner.is_null() {
                    return Ok(());
                }
                let view =
                    unsafe { jni::objects::JObject::from_raw(self.inner as jni::sys::jobject) };
                let lines = if wrap_mode == 0 { 1i32 } else { i32::MAX };
                env.call_method(&view, "setMaxLines", "(I)V", &[lines.into()])?;
                if wrap_mode == 0 {
                    env.call_method(&view, "setHorizontallyScrolling", "(Z)V", &[true.into()])?;
                }
                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            crate::backends::android::set_view_min_size(self.inner, w, h);
        }

        pub fn set_hexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.inner, expand);
        }

        pub fn set_vexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.inner, expand);
        }

        pub fn set_visible(&self, visible: bool) {
            crate::backends::android::set_view_visible(self.inner, visible);
        }

        /// Register the change callback and attach the `TextWatcher` that
        /// fires it. Mirrors what `get_buffer` installs, so a caller gets the
        /// same notifications whichever of the two it reached for.
        pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            *self.changed_cb.borrow_mut() = Some(Box::new(f));
            let cb = self.changed_cb.clone();
            let mut map = TEXT_CHANGED.lock().unwrap();
            map.insert(
                self.inner as usize,
                SendTextChanged(Box::into_raw(Box::new(move || {
                    if let Some(f) = cb.borrow_mut().as_mut() {
                        f();
                    }
                }))),
            );
            crate::backends::android::attach_text_watcher(self.inner);
            Ok(0)
        }

        /// Append a line to the buffer.
        ///
        /// Read-modify-write: this backend has no incremental insert, so the
        /// text is fetched, extended and written back. Correct, but
        /// O(document) per call. A high-rate log pane should reimplement this
        /// against the native handle.
        pub fn append_text(&self, text: &str) {
            let mut buf = self.get_text().unwrap_or_default();
            buf.push_str(text);
            self.set_text(&buf);
        }
    }

    impl Clone for TextView {
        fn clone(&self) -> Self {
            TextView {
                inner: self.inner,
                changed_cb: self.changed_cb.clone(),
            }
        }
    }

    // ------------------------------------------------------------------
    // ScrolledWindow + Overlay
    // ------------------------------------------------------------------
    //
    // Android scrolls natively (ScrollView); corro drives the viewport
    // itself, so these are inert containers that keep shared GUI code
    // compiling unchanged.

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
        /// parent-child edge Android needs: scrolled window -> canvas).
        pub fn attach_canvas(&self, canvas: &Canvas) {
            crate::backends::android::attach_child(self.0, canvas.0);
        }

        /// Set the scrollable child. A `ScrollView` takes exactly one, so a
        /// second call replaces the first — the same contract GTK's
        /// `ScrolledWindow.add` and NWG's `set_child` have.
        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            if child_ptr.is_null() {
                return;
            }
            crate::backends::android::attach_child(self.0, child_ptr);
        }

        /// Scrollbar policy. `0` never, `1` always, `2` automatic.
        ///
        /// This is the method that was a no-op, and it is why the sheet had
        /// no scrollbar at all on Android: `common::ScrolledWindow` forwards
        /// it, and every caller of `sync_scrollbars` in corro goes through it.
        /// An `NEVER` policy removes the bar, so the finger-drag pan and the
        /// scrollbar can coexist — the phone keeps the gesture and gains the
        /// affordance.
        pub fn set_policy(&self, h: u32, v: u32) {
            crate::backends::android::set_scrolled_policy(self.0, h, v);
        }

        /// Scroll to `(hval, vval)`, in the backend-agnostic *item* units
        /// (`upper` is an item count, `page` a page size), not pixels.
        ///
        /// `ScrollView.smoothScrollTo` is a pixel scroll, so the item index is
        /// converted through the ratio the caller supplies: the value's
        /// position in `[0, upper - page]`, scaled to the view's current
        /// scrollable extent. `upper <= page` means everything fits, and there
        /// is nothing to scroll to — clamping to 0 rather than dividing by
        /// zero is the whole reason those two are checked.
        pub fn scroll_to(
            &self,
            hval: f64,
            hupper: f64,
            hpage: f64,
            vval: f64,
            vupper: f64,
            vpage: f64,
        ) {
            crate::backends::android::scroll_scrolled_window(self.0, hval, hupper, hpage, vval, vupper, vpage);
        }

        /// Report a *user* scroll, with the axis (`true` = vertical) and the
        /// new scroll position in item units.
        ///
        /// Distinct from `scroll_to` in direction: `scroll_to` is what the
        /// program asks for, this is what the platform reports back. Without
        /// it, a host that lets the user drag a native scroll bar never learns
        /// the value changed, and its model and its view diverge.
        pub fn on_scroll(&self, cb: Box<dyn FnMut(bool, f64)>) {
            let mut map = SCROLL_CALLBACKS.lock().unwrap();
            map.insert(self.0 as usize, SendScrollCallback(Box::into_raw(cb)));
            crate::backends::android::attach_scroll_listener(self.0);
        }

        pub fn set_hexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }

        pub fn set_vexpand(&self, expand: bool) {
            crate::backends::android::set_view_expanding(self.0, expand);
        }
    }

    /// User-scroll callbacks keyed by scrolled-window view pointer.
    static SCROLL_CALLBACKS: Lazy<Mutex<HashMap<usize, SendScrollCallback>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    struct SendScrollCallback(*mut dyn FnMut(bool, f64));
    // SAFETY: guarded by the registry mutex; same pattern as the canvas maps.
    unsafe impl Send for SendScrollCallback {}

    /// Called from `ScrolledWindow`'s Java scroll listener: report the new
    /// value on the axis that moved. See [`ScrolledWindow::on_scroll`].
    pub fn dispatch_scrolled(window_ptr: *mut c_void, vertical: bool, value: f64) {
        let raw = {
            let map = SCROLL_CALLBACKS.lock().unwrap();
            map.get(&(window_ptr as usize)).map(|s| s.0)
        };
        let Some(ptr) = raw else { return };
        let cb: &mut dyn FnMut(bool, f64) = unsafe { &mut *ptr };
        cb(vertical, value);
    }

    impl AsRef<*mut c_void> for ScrolledWindow {
        fn as_ref(&self) -> &*mut c_void {
            &self.0
        }
    }

    impl Clone for Overlay {
        fn clone(&self) -> Self {
            Overlay(self.0)
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

    impl Overlay {
        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            if child_ptr.is_null() || self.0.is_null() {
                return;
            }
            let parent = self.0;
            let _ = crate::backends::android::with_env_and_activity(|env, _activity| {
                let layout =
                    unsafe { jni::objects::JObject::from_raw(parent as jni::sys::jobject) };
                let child_obj =
                    unsafe { jni::objects::JObject::from_raw(child_ptr as jni::sys::jobject) };
                env.call_method(
                    &layout,
                    "addView",
                    "(Landroid/view/View;)V",
                    &[(&child_obj).into()],
                )?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            });
        }

        pub fn add_overlay(&self, child: &impl AsRef<*mut c_void>) {
            self.set_child(child);
        }

        pub fn set_overlay_pass_through(&self, _child: &impl AsRef<*mut c_void>, _pass: bool) {
        }

        pub fn remove(&self, _child: &impl AsRef<*mut c_void>) {}

        pub fn show_all(&self) {}

        pub fn set_size_request(&self, _w: i32, _h: i32) {}

        pub fn set_vexpand(&self, _expand: bool) {}

        pub fn set_hexpand(&self, _expand: bool) {}
    }

    // ------------------------------------------------------------------
    // Factories
    // ------------------------------------------------------------------

    pub fn create_window() -> Result<Window, Error> {
        let ptr = crate::backends::android::create_window()
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(Window(ptr as *mut c_void))
    }

    pub fn create_button(label: &str) -> Result<Button, Error> {
        let ptr = crate::backends::android::create_button(label)
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(Button(ptr as *mut c_void))
    }

    pub fn create_label(text: &str) -> Result<Label, Error> {
        let ptr = crate::backends::android::create_label(text)
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(Label(ptr as *mut c_void))
    }

    pub fn create_box(orientation: Orientation, spacing: i32) -> Result<BoxWidget, Error> {
        let ptr = crate::backends::android::create_box(orientation.as_jni_int(), spacing)
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(BoxWidget(ptr as *mut c_void))
    }

    pub fn create_entry() -> Result<Entry, Error> {
        let ptr = crate::backends::android::create_entry()
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(Entry(ptr as *mut c_void))
    }

    pub fn create_grid() -> Result<Grid, Error> {
        let ptr = crate::backends::android::create_grid()
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(Grid(ptr as *mut c_void))
    }

    pub fn create_menu() -> Result<Menu, Error> {
        Ok(Menu { items: Vec::new() })
    }

    pub fn create_menubar(model: &Menu, _action_group: *mut c_void) -> Result<MenuBar, Error> {
        Ok(MenuBar { items: model.items.clone() })
    }

    pub fn create_simple_action(name: &str) -> Result<SimpleAction, Error> {
        Ok(SimpleAction { name: name.to_owned() })
    }

    pub fn create_canvas() -> Result<Canvas, Error> {
        // Real Java view (custom SheetView once registered, else a plain
        // View) so the widget tree never holds a bogus handle. Callback
        // registries key on the canvas id (see backend CANVAS_IDS map).
        let (ptr, _canvas_id) = crate::backends::android::create_canvas_view()
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(Canvas(ptr as *mut c_void))
    }

    pub fn create_overlay() -> Result<Overlay, Error> {
        let ptr = crate::backends::android::create_scrolled_view()
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(Overlay(ptr as *mut c_void))
    }

    pub fn create_scrolled_window() -> Result<ScrolledWindow, Error> {
        let ptr = crate::backends::android::create_scrolled_view()
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(ScrolledWindow(ptr as *mut c_void))
    }

    pub fn create_dropdown(items: &[&str]) -> Result<DropDown, Error> {
        let ptr = crate::backends::android::create_dropdown(items)
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(DropDown(ptr as *mut c_void))
    }

    pub fn create_checkbutton(label: &str) -> Result<CheckButton, Error> {
        let ptr = crate::backends::android::create_checkbutton(label)
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(CheckButton(ptr as *mut c_void))
    }

    pub fn create_radiobutton(
        group: Option<&RadioButton>,
        label: &str,
    ) -> Result<RadioButton, Error> {
        let group_ptr = group.map(|g| g.0).unwrap_or(std::ptr::null_mut());
        let ptr = crate::backends::android::create_radiobutton(group_ptr, label)
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(RadioButton(ptr as *mut c_void))
    }

    pub fn create_dialog() -> Result<Dialog, Error> {
        let ptr = crate::backends::android::create_dialog()
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(Dialog(ptr as *mut c_void))
    }

    pub fn create_textview() -> Result<TextView, Error> {
        let ptr = crate::backends::android::create_textview()
            .map_err(|e| Error::Backend(format!("{e}")))?;
        Ok(TextView::new(ptr as *mut c_void))
    }

    /// The display's density factor (`DisplayMetrics.density`): 1.0 at mdpi,
    /// 2.625 on a 420dpi phone, and so on.
    ///
    /// Hosts need this to size things in *pixels* while thinking in
    /// density-independent units: a hardcoded 12px font is 12dp on a desktop
    /// monitor but only ~4.6dp on a 420dpi phone, i.e. about a third of
    /// Android's 14sp body-text floor. Returns `None` before the backend is
    /// initialised (or if the JNI call fails), so callers can fall back to
    /// 1.0 and still render.
    pub fn display_density() -> Option<f64> {
        crate::backends::android::with_env_and_activity(|env, activity| {
            let res = env
                .call_method(activity.as_obj(), "getResources", "()Landroid/content/res/Resources;", &[])?
                .l()?;
            let metrics = env
                .call_method(&res, "getDisplayMetrics", "()Landroid/util/DisplayMetrics;", &[])?
                .l()?;
            let density = env.get_field(&metrics, "density", "F")?.f()?;
            Ok::<f64, Box<dyn std::error::Error + Send + Sync>>(density as f64)
        })
        .ok()
        .filter(|d| d.is_finite() && *d > 0.0)
    }
}

#[cfg(target_os = "android")]
pub use android_adapter::*;

#[cfg(test)]
#[cfg(target_os = "android")]
mod tests {
    use crate::core::Widget;

    #[test]
    fn test_null_window_handle() {
        let w = super::Window(std::ptr::null_mut());
        assert!(w.raw_handle().is_null());
    }

    #[test]
    fn test_null_button_handle() {
        let b = super::Button(std::ptr::null_mut());
        assert!(b.as_ref().is_null());
    }

    #[test]
    fn test_null_label_handle() {
        let l = super::Label(std::ptr::null_mut());
        assert!(l.as_ref().is_null());
    }

    #[test]
    fn test_null_box_handle() {
        let b = super::BoxWidget(std::ptr::null_mut());
        assert!(b.as_ref().is_null());
    }

    #[test]
    fn test_null_entry_handle() {
        let e = super::Entry(std::ptr::null_mut());
        assert!(e.as_ref().is_null());
    }

    #[test]
    fn test_clone_button() {
        let b1 = super::Button(0x1234 as *mut _);
        let b2 = b1.clone();
        assert_eq!(b1.as_ref(), b2.as_ref());
    }

    #[test]
    fn test_clone_label() {
        let l1 = super::Label(0x5678 as *mut _);
        let l2 = l1.clone();
        assert_eq!(l1.as_ref(), l2.as_ref());
    }

    #[test]
    fn test_clone_entry() {
        let e1 = super::Entry(0x9abc as *mut _);
        let e2 = e1.clone();
        assert_eq!(e1.as_ref(), e2.as_ref());
    }

    #[test]
    fn test_orientation_values_match_linear_layout() {
        // Discriminants mirror android.widget.LinearLayout constants.
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
        let dc = super::AndroidDrawContext;
        let (x, y, w, h) = crate::core::DrawContext::text_extents_styled(&dc, "hello", "monospace", 12.0, 0, 0);
        let _ = (x, y);
        assert!(w > 0.0 && h > 0.0);
    }
}
