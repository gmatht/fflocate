// High-level ergonomic wrappers over gtk_compat for the rswidgets API
#[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
mod gtk_adapter {
    use std::os::raw::c_void;
    use std::cell::RefCell;

    use std::rc::Rc;
    use crate::core::{Error, Widget};
    use gtk_dynamic_loader::{Window as GWindow, Button as GButton, Label as GLabel, BoxWidget as GBox, Grid as GGrid, Entry as GEntry, Dialog as GDialog, DropDown as GDropDown, CheckButton as GCheckButton, RadioButton as GRadioButton, TextView as GTextView, ScrolledWindow as GScrolledWindow};

    /// Window wrapper around gtk_compat::Window.
    /// Stores event controllers in _controllers so that on_event_key
    /// (which adds a GtkEventControllerKey to the window) keeps the
    /// Rust-side wrapper alive.  Without this the controller is dropped
    /// while still owned by the window, causing a segfault later.
    /// Swallow GTK4 `key-released` on a key controller, if this GTK is GTK4.
    ///
    /// A physical key produces BOTH `key-pressed` and `key-released` from the
    /// same `GtkEventControllerKey`. On the GTK4/WSLg versions where a release
    /// is routed through the *pressed* handler, a host hooking only
    /// `key-pressed` sees every key twice — which forced `gui_backend.rs` to
    /// carry consecutive-keyval dedup heuristics gated on `feature = "gtk4"`.
    ///
    /// Registering the release signal here and returning `GDK_EVENT_STOP`
    /// without invoking the host gives every host a pure press stream, so no
    /// application needs to know which toolkit it is running on.
    ///
    /// No-op on GTK3, which exposes only `key-press-event`.
    fn swallow_key_releases_gtk4(ctrl: &gtk_dynamic_loader::EventControllerKey) {
        let _ = ctrl.connect_key_released(Box::new(|_keyval: u32, _state: u32| -> i32 {
            // GDK_EVENT_STOP: consume the release, never call the host.
            1
        }));
    }

    pub struct Window(pub GWindow, pub Rc<RefCell<Vec<Box<dyn std::any::Any>>>>);

    impl Widget for Window {
        fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() }
    }

    impl AsRef<*mut c_void> for Window { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }

    impl Clone for Window { fn clone(&self) -> Self { Window(self.0.clone(), self.1.clone()) } }

    impl Window {
        pub fn set_title(&self, title: &str) {
            self.0.set_title(title);
        }

        pub fn set_default_size(&self, w: i32, h: i32) {
            self.0.set_default_size(w, h);
        }
        /// Immediate resize; see the loader's `Window::resize`. Needed where
        /// `set_default_size` is advisory (no window manager).
        pub fn resize(&self, w: i32, h: i32) { self.0.resize(w, h); }

        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            self.0.set_child(child);
        }
        pub fn set_child_box(&self, bx: &BoxWidget) { self.set_child(bx); }

        pub fn present(&self) {
            self.0.present();
        }

        /// Queue a redraw of the entire window.  On GTK4 the DrawingArea
        /// may have its own draw function, but forcing a window-level
        /// queue_draw cascades to all children (including the canvas),
        /// ensuring the draw callback fires even when the canvas-level
        /// queue_draw alone doesn't trigger the frame clock.
        pub fn queue_redraw(&self) {
            if let Some(loader) = crate::backends::gtk::loader() {
                let win_ptr = *self.0.as_ref();
                if !win_ptr.is_null() {
                    if let Some(qd) = loader.symbols.gtk_widget_queue_draw {
                        unsafe { qd(win_ptr); }
                    }
                }
            }
        }

        /// # Safety
        /// `group_ptr` must be a valid GActionGroup pointer or null.
        pub unsafe fn insert_action_group(&self, name: &str, group_ptr: *mut std::os::raw::c_void) {
            self.0.insert_action_group(name, group_ptr);
        }

        pub fn hwnd(&self) -> *mut c_void {
            *self.0.as_ref()
        }
        pub fn on_event(&self, cb: Box<dyn FnMut(*mut c_void) -> i32>) {
            if let Some(loader) = crate::backends::gtk::loader() {
                let win_ptr = *self.0.as_ref();
                if !win_ptr.is_null() {
                    let l = loader.clone();
                    unsafe {
                        let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                            &l, win_ptr, "event", cb,
                        );
                    }
                }
            }
        }

        pub fn on_close(&self, cb: Box<dyn FnMut()>) {
            if let Some(loader) = crate::backends::gtk::loader() {
                let win_ptr = *self.0.as_ref();
                if !win_ptr.is_null() {
                    let l = loader.clone();
                    let is_gtk4 = l.symbols.gtk_drawing_area_set_draw_func.is_some();
                    let signal = if is_gtk4 { "close-request" } else { "delete-event" };
                    let mut cb = cb;
                    unsafe {
                        let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                            &l, win_ptr, signal,
                            Box::new(move |_ev: *mut c_void| -> i32 {
                                cb();
                                0
                            }),
                        );
                    }
                }
            }
        }
        pub fn on_event_key(&self, cb: Box<dyn FnMut(u32, u32) -> i32>) {
            if let Some(loader) = crate::backends::gtk::loader() {
                let win_ptr = *self.0.as_ref();
                if !win_ptr.is_null() {
                    let l = loader.clone();
                    let is_gtk4 = l.symbols.gtk_drawing_area_set_draw_func.is_some();
                    // Shared callback wrapper used by both GTK4 and GTK3 paths.
                    let shared_cb: Rc<RefCell<Option<Box<dyn FnMut(u32, u32) -> i32>>>> = Rc::new(RefCell::new(Some(cb)));
                    if is_gtk4 {
                        // CAPTURE-phase controller: handles ALL keys (not just navigation keys).
                        // On GTK4/WSLg, when the formula entry doesn't have keyboard focus,
                        // keyboard events are silently dropped because there's no focused
                        // widget to receive them.  By processing ALL keys in the window
                        // CAPTURE phase, we ensure every keystroke reaches our key handler
                        // regardless of focus state.
                        //
                        // We always call the application callback and return its result
                        // (GDK_EVENT_STOP for handled keys, GDK_EVENT_PROPAGATE otherwise).
                        // For printable characters, the application's callback now returns
                        // STOP (1) because start_edit_with/set_text updates the entry widget
                        // directly — there's no need for the event to reach the entry widget.
                        //
                        // Mutual-exclusion flag: the CAPTURE controller sets this before
                        // returning STOP; the BUBBLE controller checks it to avoid
                        // processing the same event twice.  On some GTK4/WSLg versions
                        // the BUBBLE controller fires even when CAPTURE returns STOP,
                        // so a simple GDK_EVENT_STOP return is not sufficient — we need
                        // this explicit handshake.
                        let capture_handled: std::cell::Cell<bool> = std::cell::Cell::new(false);
                        let capture_handled = std::rc::Rc::new(capture_handled);
                        if let Ok(ctrl) = gtk_dynamic_loader::EventControllerKey::new(l.clone()) {
                            ctrl.set_propagation_phase_capture();
                            let sc = shared_cb.clone();
                            let ch = capture_handled.clone();
                            let _ = ctrl.connect_key_pressed(Box::new(move |keyval: u32, state: u32| -> i32 {
                                ch.set(false);
                                let result = if let Some(ref mut f) = *sc.borrow_mut() {
                                    f(keyval, state)
                                } else {
                                    0
                                };
                                if result != 0 {
                                    ch.set(true);
                                }
                                result
                            }));
                            swallow_key_releases_gtk4(&ctrl);
                            ctrl.add_to_widget(&self.0);
                            self.1.borrow_mut().push(Box::new(ctrl));
                        }
                        // BUBBLE-phase controller: safety net.  The CAPTURE-phase controller
                        // above handles all keys and sets capture_handled=true.  If BUBBLE
                        // fires despite CAPTURE having returned STOP (a known issue on some
                        // GTK4/WSLg versions), we check the flag and skip.
                        if let Ok(ctrl) = gtk_dynamic_loader::EventControllerKey::new(l.clone()) {
                            let sc = shared_cb.clone();
                            let ch = capture_handled.clone();
                            let _ = ctrl.connect_key_pressed(Box::new(move |keyval: u32, state: u32| -> i32 {
                                if ch.get() {
                                    ch.set(false);
                                    return 0;
                                }
                                if let Some(ref mut f) = *sc.borrow_mut() {
                                    f(keyval, state)
                                } else {
                                    0
                                }
                            }));
                            swallow_key_releases_gtk4(&ctrl);
                            ctrl.add_to_widget(&self.0);
                            self.1.borrow_mut().push(Box::new(ctrl));
                        }
                    } else {
                        unsafe {
                            let sc = shared_cb.clone();
                            let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                                &l.clone(), win_ptr, "event",
                                Box::new(move |ev: *mut c_void| -> i32 {
                                    // The unfiltered "event" signal also delivers key
                                    // RELEASES, which carry the same keyval/state as
                                    // presses and are indistinguishable downstream —
                                    // heuristics there eat genuine repeats. Drop
                                    // everything but GDK_KEY_PRESS (8) at the source.
                                    // If the symbol is missing, fail open (old behavior).
                                    if let Some(get_ty) = l.symbols.gdk_event_get_event_type {
                                        if get_ty(ev) != 8 {
                                            return 0;
                                        }
                                    }
                                    let mut keyval: u32 = 0;
                                    if let Some(get_kv) = l.symbols.gdk_event_get_keyval {
                                        if get_kv(ev, &mut keyval) == 0 {
                                            return 0;
                                        }
                                    } else {
                                        return 0;
                                    }
                                    let mut state: u32 = 0;
                                    if let Some(get_st) = l.symbols.gdk_event_get_state {
                                        get_st(ev, &mut state);
                                    }
                                    if let Some(ref mut f) = *sc.borrow_mut() {
                                        f(keyval, state)
                                    } else {
                                        0
                                    }
                                }),
                            );
                        }
                    }
                }
            }
        }
    }

    #[repr(transparent)]
    pub struct Button(pub GButton);

    impl Widget for Button { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }
    impl AsRef<*mut c_void> for Button { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }

    impl Button {
        pub fn on_click(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            self.0.connect_clicked(f).map_err(|e| Error::Backend(format!("{}", e)))
        }

        pub fn emit_clicked(&self) -> Result<u64, Error> {
            self.0.emit_clicked().map_err(|e| Error::Backend(format!("{}", e)))
        }

        pub fn set_size_request(&self, w: i32, h: i32) { self.0.set_size_request(w, h); }
        pub fn set_visible(&self, visible: bool) { self.0.set_visible(visible); }
        pub fn set_hexpand(&self, expand: bool) { self.0.set_hexpand(expand); }
        pub fn set_vexpand(&self, expand: bool) { self.0.set_vexpand(expand); }
        pub fn set_font_style(&self, weight: i32, italic: bool) { self.0.set_font_style(weight, italic); }
        pub fn add_class(&self, class_name: &str) { self.0.add_class(class_name); }
        pub fn remove_class(&self, class_name: &str) { self.0.remove_class(class_name); }
    }

    impl Clone for Button { fn clone(&self) -> Self { Button(self.0.clone()) } }

    #[repr(transparent)]
    pub struct Label(pub GLabel);

    impl Widget for Label { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }
    impl AsRef<*mut c_void> for Label { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }

    impl Label {
        pub fn set_text(&self, text: &str) {
            self.0.set_text(text);
        }

        pub fn get_text(&self) -> Option<String> {
            self.0.get_text()
        }
        pub fn add_class(&self, class_name: &str) { self.0.add_class(class_name); }
        pub fn remove_class(&self, class_name: &str) { self.0.remove_class(class_name); }
        pub fn set_markup(&self, markup: &str) { self.0.set_markup(markup); }
        pub fn set_visible(&self, visible: bool) { self.0.set_visible(visible); }
        pub fn set_hexpand(&self, expand: bool) { self.0.set_hexpand(expand); }
        pub fn set_vexpand(&self, expand: bool) { self.0.set_vexpand(expand); }
        pub fn set_size_request(&self, w: i32, h: i32) { self.0.set_size_request(w, h); }
        /// Left margin of the label's contents, in device px.
        ///
        /// Useful next to [`Label::set_fixed_width`]: a pinned-and-left-aligned
        /// label sits flush against its slot's edge, so this is how the text
        /// keeps the inset it had when the label sized itself to its content.
        pub fn set_margin_start(&self, px: i32) { self.0.set_margin_start(px); }
        /// Pin the label's width so changing its text never reflows the
        /// siblings packed after it. `None` releases the pin.
        ///
        /// GTK honours a width request as the widget's *minimum* width, so the
        /// label keeps its slot and only clips text wider than the pin — the
        /// fixed-slot behaviour, and the same trade the NWG implementation
        /// makes. This used to be a no-op here, on the belief that "a GtkLabel
        /// already lays out at its natural text width and the box does not
        /// re-pack children on a text change". That belief was wrong: the
        /// formula row's address label grows with its text and drags the `fx`
        /// caption and the entry's caret sideways on every cursor move. A GTK
        /// width request is what actually stops it, so the pin is real now.
        ///
        /// The caller must size the pin for the widest text the label will
        /// ever show, since GTK clips rather than growing past the request.
        pub fn set_fixed_width(&self, w: Option<i32>) {
            match w {
                // Height -1 keeps the natural height: only the width is pinned.
                Some(w) => self.0.set_size_request(w, -1),
                None => self.0.set_size_request(-1, -1),
            }
        }
        /// Vertical outer spacing; pairs with `set_margin_start` so a label
        /// can be inset on all four sides from one code path.
        pub fn set_margin_top(&self, m: i32) { self.0.set_margin_top(m); }
        /// Set the x alignment of the label's text (0.0 left .. 1.0 right)
        pub fn set_xalign(&self, x: f32) { self.0.set_xalign(x); }
        pub fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() }
    }

    impl Clone for Label { fn clone(&self) -> Self { Label(self.0.clone()) } }

    #[repr(transparent)]
    pub struct BoxWidget(pub GBox);

    impl Widget for BoxWidget { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }
    impl AsRef<*mut c_void> for BoxWidget { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }

    impl Clone for BoxWidget { fn clone(&self) -> Self { BoxWidget(self.0.clone()) } }

    impl BoxWidget {
        pub fn append(&self, child: &impl AsRef<*mut c_void>) {
            self.0.append(child);
        }

        pub fn set_size_request(&self, w: i32, h: i32) { self.0.set_size_request(w, h); }
        pub fn set_vexpand(&self, expand: bool) { self.0.set_vexpand(expand); }
        pub fn set_hexpand(&self, expand: bool) { self.0.set_hexpand(expand); }
        pub fn set_visible(&self, visible: bool) { self.0.set_visible(visible); }
        pub fn set_child_hexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
            if let Some(loader) = crate::backends::gtk::loader() {
                let child_ptr = *child.as_ref();
                if !child_ptr.is_null() {
                    unsafe { gtk_dynamic_loader::widget_set_hexpand(&loader, child_ptr, expand); }
                }
            }
        }
        pub fn set_child_vexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
            if let Some(loader) = crate::backends::gtk::loader() {
                let child_ptr = *child.as_ref();
                if !child_ptr.is_null() {
                    unsafe { gtk_dynamic_loader::widget_set_vexpand(&loader, child_ptr, expand); }
                }
            }
        }
    }

    #[repr(transparent)]
    pub struct Grid(pub GGrid);
    impl Widget for Grid { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }
    impl AsRef<*mut c_void> for Grid { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }
    impl Clone for Grid { fn clone(&self) -> Self { Grid(self.0.clone()) } }

    impl Grid {
        pub fn attach(&self, child: &impl AsRef<*mut c_void>, left: i32, top: i32, width: i32, height: i32) {
            self.0.attach(child, left, top, width, height);
        }
        pub fn set_visible(&self, visible: bool) { self.0.set_visible(visible); }
        pub fn set_hexpand(&self, expand: bool) { self.0.set_hexpand(expand); }
        pub fn set_vexpand(&self, expand: bool) { self.0.set_vexpand(expand); }
        pub fn set_size_request(&self, w: i32, h: i32) { self.0.set_size_request(w, h); }
    }

    pub struct Entry {
        inner: GEntry,
        _controllers: Rc<RefCell<Vec<Box<dyn std::any::Any>>>>,
    }
    impl Widget for Entry { fn raw_handle(&self) -> *mut c_void { *self.inner.as_ref() } }
    impl AsRef<*mut c_void> for Entry { fn as_ref(&self) -> &*mut c_void { self.inner.as_ref() } }

    impl Entry {
        pub fn set_text(&self, text: &str) { self.inner.set_text(text); }
        pub fn get_text(&self) -> Option<String> { self.inner.get_text() }
        pub fn get_position(&self) -> Option<usize> {
            self.inner.get_position().and_then(|p| if p < 0 { None } else { Some(p as usize) })
        }
        pub fn set_position(&self, pos: usize) {
            self.inner.set_position(pos as i32);
        }
        pub fn set_width_chars(&self, n: i32) { self.inner.set_width_chars(n); }
        pub fn set_size_request(&self, w: i32, h: i32) { self.inner.set_size_request(w, h); }
        pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> { self.inner.connect_changed(f).map_err(|e| Error::Backend(format!("{}", e))) }
        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(&self, f: F) -> Result<u64, Error> {
            self.inner.connect_activate(f).map_err(|e| Error::Backend(format!("{}", e)))
        }
        pub fn connect_button_press(&self, f: impl FnMut() + 'static) -> Result<u64, Error> { self.inner.connect_button_press(f).map_err(|e| Error::Backend(format!("{}", e))) }
        pub fn add_class(&self, class_name: &str) { self.inner.add_class(class_name); }
        pub fn remove_class(&self, class_name: &str) { self.inner.remove_class(class_name); }
        pub fn grab_focus(&self) { self.inner.grab_focus(); }
        pub fn has_focus(&self) -> bool { self.inner.has_focus() }
        pub fn connect_focus_in_event<F: FnMut(*mut c_void) -> i32 + 'static>(&self, f: F) -> Result<u64, Error> {
            self.inner.connect_focus_in_event(f).map_err(|e| Error::Backend(format!("{}", e)))
        }
        pub fn connect_focus_out_event<F: FnMut(*mut c_void) -> i32 + 'static>(&self, f: F) -> Result<u64, Error> {
            self.inner.connect_focus_out_event(f).map_err(|e| Error::Backend(format!("{}", e)))
        }
        pub fn set_margin_start(&self, margin: i32) { self.inner.set_margin_start(margin); }
        pub fn set_margin_top(&self, margin: i32) { self.inner.set_margin_top(margin); }
        pub fn set_halign(&self, align: i32) { self.inner.set_halign(align); }
        pub fn set_valign(&self, align: i32) { self.inner.set_valign(align); }
        pub fn set_visible(&self, visible: bool) { self.inner.set_visible(visible); }
        pub fn set_hexpand(&self, expand: bool) { self.inner.set_hexpand(expand); }
        pub fn set_vexpand(&self, expand: bool) { self.inner.set_vexpand(expand); }
        pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
            if let Some(loader) = crate::backends::gtk::loader() {
                let entry_ptr = *self.inner.as_ref();
                if !entry_ptr.is_null() {
                    let symbols = &loader.symbols;
                    let is_gtk4 = symbols.gtk_drawing_area_set_draw_func.is_some();
                    if is_gtk4 {
                        // GTK4: GtkEntry's internal EventControllerKey (CAPTURE phase) consumes
                        // RETURN/TAB/ESCAPE/arrows before a BUBBLE-phase EventControllerKey can
                        // fire.  We use CAPTURE phase here so our controller fires BEFORE the
                        // internal handler, intercepting RETURN directly instead of relying on
                        // the "activate" signal (which doesn't fire reliably on WSLg/WSL).
                        //
                        // For printable characters our callback returns GDK_EVENT_PROPAGATE (false),
                        // letting the internal handler insert the character normally.
                        // The "changed" signal then fires on_formula_entry_changed as before.
                        let shared_cb = std::rc::Rc::new(std::cell::RefCell::new(Some(cb)));
                        self._controllers.borrow_mut().push(Box::new(shared_cb.clone()));
                        if let Ok(ctrl) = gtk_dynamic_loader::EventControllerKey::new(loader.clone()) {
                            ctrl.set_propagation_phase_capture();
                            let sc = shared_cb.clone();
                            let _ = ctrl.connect_key_pressed(Box::new(move |keyval: u32, state: u32| -> i32 {
                                if let Some(ref mut f) = *sc.borrow_mut() {
                                    if f(keyval, state) { 1 } else { 0 }
                                } else { 0 }
                            }));
                            swallow_key_releases_gtk4(&ctrl);
                            ctrl.add_to_widget(&self.inner);
                            self._controllers.borrow_mut().push(Box::new(ctrl));
                        }
                        // NOTE: The application-level connect_activate callback in gui_backend.rs
                        // is retained as a secondary fallback.  With CAPTURE phase we handle RETURN
                        // directly and stop propagation, so "activate" is never emitted — but if
                        // the CAPTURE controller somehow doesn't fire (e.g., older GTK), the
                        // connect_activate path still works.
                    } else {
                        // GTK3 path: connect to raw "key-press-event" signal
                        let l = loader.clone();
                        let mut cb = cb;
                        unsafe {
                            let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                                &l.clone(), entry_ptr, "key-press-event",
                                Box::new(move |ev: *mut c_void| -> i32 {
                                    let keyval = gtk_dynamic_loader::EventControllerKey::get_keyval_static(&l, ev);
                                    if keyval == 0 { return 0; }
                                    let state = gtk_dynamic_loader::EventControllerKey::get_state_static(&l, ev);
                                    if cb(keyval, state) { 1 } else { 0 }
                                }),
                            );
                        }
                    }
                }
            }
        }
    }

    impl Clone for Entry { fn clone(&self) -> Self { Entry { inner: self.inner.clone(), _controllers: self._controllers.clone() } } }
    impl Clone for DropDown { fn clone(&self) -> Self { DropDown(self.0.clone()) } }
    impl Clone for CheckButton { fn clone(&self) -> Self { CheckButton(self.0.clone()) } }
    impl Clone for RadioButton { fn clone(&self) -> Self { RadioButton(self.0.clone()) } }
    impl Clone for TextView { fn clone(&self) -> Self { TextView(self.0.clone()) } }

    // Factories delegate to backend so they share the App-owned loader
    pub fn create_window() -> Result<Window, Error> {
        let gw = crate::backends::gtk::create_window().map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(Window(gw, Rc::new(RefCell::new(Vec::new()))))
    }

    pub fn create_button(label: &str) -> Result<Button, Error> {
        let btn = crate::backends::gtk::create_button(label).map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(Button(btn))
    }

    pub fn create_label(text: &str) -> Result<Label, Error> {
        let l = crate::backends::gtk::create_label(text).map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(Label(l))
    }

    pub fn create_box(orientation: gtk_dynamic_loader::Orientation, spacing: i32) -> Result<BoxWidget, Error> {
        let b = crate::backends::gtk::create_box(orientation, spacing).map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(BoxWidget(b))
    }

    pub fn create_grid() -> Result<Grid, Error> {
        let g = crate::backends::gtk::create_grid().map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(Grid(g))
    }

    pub fn create_entry() -> Result<Entry, Error> {
        let e = crate::backends::gtk::create_entry().map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(Entry { inner: e, _controllers: Rc::new(RefCell::new(Vec::new())) })
    }

    // ---- Menu types ----

    #[repr(transparent)]
    pub struct Menu(pub gtk_dynamic_loader::Menu);

    impl Clone for Menu { fn clone(&self) -> Self { Menu(self.0.clone()) } }

    impl Menu {
        pub fn append(&mut self, label: &str, detailed_action: &str) {
            self.0.append(label, detailed_action);
        }

        pub fn append_submenu(&mut self, label: &str, submenu: &Menu) {
            self.0.append_submenu(label, &submenu.0);
        }
    }

    pub fn create_menu() -> Result<Menu, Error> {
        let m = crate::backends::gtk::create_menu().map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(Menu(m))
    }

    #[repr(transparent)]
    pub struct MenuBar(pub gtk_dynamic_loader::MenuBar);

    impl Clone for MenuBar { fn clone(&self) -> Self { MenuBar(self.0.clone()) } }
    impl Widget for MenuBar { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }
    impl AsRef<*mut c_void> for MenuBar { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }
    impl MenuBar {
        pub fn activate_submenu_by_mnemonic(&self, keyval: u32) -> bool {
            self.0.activate_submenu_by_mnemonic(keyval)
        }
        /// Open the submenu whose mnemonic is `keyval` at a screen position.
        ///
        /// Used by the sheet-tab context menu: a right-click on a tab should
        /// drop the Sheet menu under the pointer (LibreOffice Calc behaviour),
        /// not under the menu bar. Falls back to the unpositioned open on GTK4.
        pub fn popup_submenu_by_mnemonic_at(&self, keyval: u32, screen_x: i32, screen_y: i32) -> bool {
            self.0.popup_submenu_by_mnemonic_at(keyval, screen_x, screen_y)
        }
        /// `popup_submenu_by_mnemonic_at` with the caller's widget-relative
        /// point resolved to screen coordinates here.
        ///
        /// The GTK3 path keeps its existing positioning (it hands GTK the point
        /// and lets GTK flip off-screen edges itself); GTK4 has no equivalent
        /// hook and falls back to the unpositioned open, as before.
        pub fn popup_at(&self, label: &str, anchor: &Canvas, x: f64, y: f64) -> bool {
            let keyval = match label.chars().next() {
                Some(c) => c as u32,
                None => return false,
            };
            match anchor.screen_origin() {
                Some((ox, oy)) => {
                    self.popup_submenu_by_mnemonic_at(keyval, ox + x as i32, oy + y as i32)
                }
                None => self.activate_submenu_by_mnemonic(keyval),
            }
        }
        pub fn activate_submenu_item_by_mnemonic(&self, keyval: u32) -> bool {
            self.0.activate_submenu_item_by_mnemonic(keyval)
        }
        pub unsafe fn insert_action_group(&self, name: &str, group_ptr: *mut std::os::raw::c_void) {
            self.0.insert_action_group(name, group_ptr);
        }
        pub fn handle_mnemonic_key(&self, keyval: u32) -> bool {
            self.0.handle_mnemonic_key(keyval)
        }
        pub fn handle_menu_key(&self, keyval: u32, modifiers: u32) -> bool {
            self.0.handle_menu_key(keyval, modifiers)
        }

        /// [`handle_menu_key`](Self::handle_menu_key) with the typed character
        /// supplied separately (see `common::MenuBar::handle_menu_key_char`).
        /// GTK keyvals are X11 keysyms, not characters (GDK_KEY_KP_0 is
        /// 0xffb0), so `ch` is the more faithful input where a caller has it;
        /// GTK's own mnemonics are handled natively, so this stays a forward.
        pub fn handle_menu_key_char(&self, keyval: u32, ch: char, modifiers: u32) -> bool {
            let _ = ch;
            self.0.handle_menu_key(keyval, modifiers)
        }
        pub fn menu_active(&self) -> bool {
            self.0.menu_active()
        }
        pub fn menu_close(&self) {
            self.0.menu_close();
        }
    }

    /// # Safety
    /// `action_group` must be a valid GActionGroup pointer or null.
    pub unsafe fn create_menubar(model: &Menu, action_group: *mut std::os::raw::c_void) -> Result<MenuBar, Error> {
        let b = crate::backends::gtk::create_menubar(&model.0, action_group).map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(MenuBar(b))
    }

    #[repr(transparent)]
    pub struct SimpleAction(pub gtk_dynamic_loader::SimpleAction);

    impl Clone for SimpleAction { fn clone(&self) -> Self { SimpleAction(self.0.clone()) } }

    impl SimpleAction {
        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(&self, f: F) -> Result<u64, Error> {
            self.0.connect_activate(f).map_err(|e| Error::Backend(format!("{}", e)))
        }
    }

    pub fn create_simple_action(name: &str) -> Result<SimpleAction, Error> {
        let a = crate::backends::gtk::create_simple_action(name).map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(SimpleAction(a))
    }

    // ---- Application (action group host for menus) ----

    #[repr(transparent)]
    pub struct Application(pub gtk_dynamic_loader::Application);

    impl Application {
        pub fn register(&self) -> Result<(), Error> {
            self.0.register().map_err(|e| Error::Backend(format!("{}", e)))
        }
        pub fn as_ptr(&self) -> *mut c_void {
            self.0.as_ptr()
        }
        pub fn add_action(&self, action: &SimpleAction) -> Result<(), Error> {
            self.0.add_action(&action.0).map_err(|e| Error::Backend(format!("{}", e)))
        }
    }

    pub fn create_application() -> Result<Application, Error> {
        let loader = crate::backends::gtk::loader()
            .ok_or_else(|| Error::Backend("GTK loader not initialized".into()))?;
        let app = gtk_dynamic_loader::Application::new(loader, Some("org.corro.Corro"))
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(Application(app))
    }

    // ---- Dialog ----

    #[repr(transparent)]
    pub struct Dialog(pub GDialog);
    impl Clone for Dialog { fn clone(&self) -> Self { Dialog(self.0.clone()) } }
    impl Widget for Dialog { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }
    impl AsRef<*mut c_void> for Dialog { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }

    impl Dialog {
        pub fn set_title(&self, title: &str) { self.0.set_title(title); }
        pub fn set_transient_for(&self, parent: *mut c_void) { self.0.set_transient_for(parent); }
        pub fn set_default_size(&self, w: i32, h: i32) { self.0.set_default_size(w, h); }
        pub fn add_button(&self, text: &str, response_id: i32) { self.0.add_button(text, response_id); }
        pub fn set_default_response(&self, response_id: i32) { self.0.set_default_response(response_id); }
        pub fn get_content_area(&self) -> *mut c_void { self.0.get_content_area() }
        pub fn append_content_area(&self, child: &impl AsRef<*mut c_void>) { self.0.append_content_area(child); }
        pub fn present(&self) { self.0.present(); }
        pub fn connect_response<F: FnMut(i32) + 'static>(&self, f: F) -> Result<u64, Error> {
            self.0.connect_response(f).map_err(|e| Error::Backend(format!("{}", e)))
        }
        pub fn close(&self) { self.0.close(); }
        /// Measure the dialog without mapping it; see the loader's
        /// `layout_dialog`. A caller attaching a custom view needs the size
        /// settled before the window opens.
        pub fn layout_dialog(&self) { self.0.layout_dialog(); }
        /// Hide or show without destroying. `close` would destroy a
        /// `GtkDialog`, so this is a different operation, not an alias.
        pub fn set_visible(&self, visible: bool) { self.0.set_visible(visible); }
        /// Run the nested main loop and return the response id. Falls back to
        /// present-and-return-0 when `gtk_dialog_run` is unavailable.
        pub fn run(&self) -> i32 { self.0.run() }
        pub fn mark_destroyed(&self) { self.0.mark_destroyed(); }

    }

    pub fn create_dialog() -> Result<Dialog, Error> {
        let d = crate::backends::gtk::create_dialog().map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(Dialog(d))
    }

    // ---- DropDown ----

    #[repr(transparent)]
    pub struct DropDown(pub GDropDown);
    impl Widget for DropDown { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }
    impl AsRef<*mut c_void> for DropDown { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }

    impl DropDown {
        pub fn set_active(&self, index: Option<u32>) {
            if let Some(idx) = index { self.0.set_active(idx); }
        }
        pub fn grab_focus(&self) { self.0.grab_focus(); }
        /// (visible, mapped, alloc_w, alloc_h, has_parent) diagnostic.
        pub fn diagnostics(&self) -> (bool, bool, i32, i32, bool) { self.0.diagnostics() }
        /// True when the GTK4 DropDown backend is in use (vs GTK3 combo).
        ///
        /// Delegates to the loader, which decides the backend ONCE in
        /// `DropDown::new` and records it on the widget. It used to be
        /// recomputed per call from `symbols.gtk_drop_down_new.is_some()`, and
        /// on Linux that probe said "GTK4" for a `GtkComboBoxText` -- libgtk-3
        /// exports `gtk_drop_down_new` as a 3.24 forwarder. Every caller
        /// branched on GTK4 behaviour it never got.
        pub fn is_gtk4(&self) -> bool { self.0.is_gtk4() }
        pub fn has_size_request_symbol(&self) -> bool { self.0.has_size_request_symbol() }
        pub fn get_active(&self) -> i32 { self.0.get_active() }
        pub fn connect_changed<F: FnMut() + 'static>(&self, f: F) -> Result<u64, Error> {
            self.0.connect_changed(f).map_err(|e| Error::Backend(format!("{}", e)))
        }
        pub fn set_visible(&self, visible: bool) { self.0.set_visible(visible); }
        pub fn set_hexpand(&self, expand: bool) { self.0.set_hexpand(expand); }
        pub fn set_vexpand(&self, expand: bool) { self.0.set_vexpand(expand); }
        pub fn set_size_request(&self, w: i32, h: i32) { self.0.set_size_request(w, h); }
        /// Offset within an overlay parent, in widget pixels.
        pub fn set_offset(&self, x: i32, y: i32) {
            self.0.set_align_start();
            self.0.set_margin_start(x);
            self.0.set_margin_top(y);
        }
    }

    pub fn create_dropdown(items: &[&str]) -> Result<DropDown, Error> {
        let d = crate::backends::gtk::create_dropdown(items).map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(DropDown(d))
    }

    // ---- CheckButton ----

    #[repr(transparent)]
    pub struct CheckButton(pub GCheckButton);
    impl Widget for CheckButton { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }
    impl AsRef<*mut c_void> for CheckButton { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }

    impl CheckButton {
        pub fn is_active(&self) -> bool { self.0.is_active() }
        pub fn set_active(&self, active: bool) { self.0.set_active(active); }
        pub fn connect_toggled<F: FnMut() + 'static>(&self, f: F) -> Result<u64, Error> {
            self.0.connect_toggled(f).map_err(|e| Error::Backend(format!("{}", e)))
        }
        pub fn set_visible(&self, visible: bool) { self.0.set_visible(visible); }
        pub fn set_hexpand(&self, expand: bool) { self.0.set_hexpand(expand); }
        pub fn set_vexpand(&self, expand: bool) { self.0.set_vexpand(expand); }
        pub fn set_size_request(&self, w: i32, h: i32) { self.0.set_size_request(w, h); }
    }

    pub fn create_checkbutton(label: &str) -> Result<CheckButton, Error> {
        let c = crate::backends::gtk::create_checkbutton(label).map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(CheckButton(c))
    }

    // ---- RadioButton ----

    #[repr(transparent)]
    pub struct RadioButton(pub GRadioButton);
    impl Widget for RadioButton { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }
    impl AsRef<*mut c_void> for RadioButton { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }

    impl RadioButton {
        pub fn is_active(&self) -> bool { self.0.is_active() }
        pub fn set_active(&self, active: bool) { self.0.set_active(active); }
        pub fn grab_focus(&self) { self.0.grab_focus(); }
        pub fn connect_toggled<F: FnMut() + 'static>(&self, f: F) -> Result<u64, Error> {
            self.0.connect_toggled(f).map_err(|e| Error::Backend(format!("{}", e)))
        }
        pub fn set_visible(&self, visible: bool) { self.0.set_visible(visible); }
        pub fn set_hexpand(&self, expand: bool) { self.0.set_hexpand(expand); }
        pub fn set_vexpand(&self, expand: bool) { self.0.set_vexpand(expand); }
        pub fn set_size_request(&self, w: i32, h: i32) { self.0.set_size_request(w, h); }
    }

    pub fn create_radiobutton(group: Option<&RadioButton>, label: &str) -> Result<RadioButton, Error> {
        let inner_group = group.map(|g| &g.0);
        let r = crate::backends::gtk::create_radiobutton(inner_group, label).map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(RadioButton(r))
    }

    // ---- TextView ----

    #[repr(transparent)]
    pub struct TextView(pub GTextView);
    impl Widget for TextView { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }
    impl AsRef<*mut c_void> for TextView { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }

    impl TextView {
        pub fn set_text(&self, text: &str) { self.0.set_text(text); }
        pub fn get_text(&self) -> Option<String> { self.0.get_text() }
        pub fn set_wrap_mode(&self, wrap_mode: i32) { self.0.set_wrap_mode(wrap_mode); }
        pub fn set_size_request(&self, w: i32, h: i32) { self.0.set_size_request(w, h); }
        pub fn set_hexpand(&self, expand: bool) { self.0.set_hexpand(expand); }
        pub fn set_vexpand(&self, expand: bool) { self.0.set_vexpand(expand); }
        pub fn set_visible(&self, visible: bool) { self.0.set_visible(visible); }
        /// Append a line without rebuilding the buffer (see the loader's
        /// `TextView::append_text`). Needed by any append-only log pane.
        pub fn append_text(&self, text: &str) { self.0.append_text(text); }
    }

    pub fn create_textview() -> Result<TextView, Error> {
        let t = crate::backends::gtk::create_textview().map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(TextView(t))
    }

    /// Run `f` once after `ms` milliseconds (see backends::gtk::timeout_add_once).
    pub fn timeout_add_once(ms: u32, f: Box<dyn FnOnce()>) -> Result<(), Error> {
        crate::backends::gtk::timeout_add_once(ms, f).map_err(|e| Error::Backend(format!("{}", e)))
    }

    /// Run `f` every `ms` milliseconds until it returns `false`
    /// (see backends::gtk::timeout_add_repeating). Used by apps that need a
    /// periodic tick (e.g. polling an append-only log for another window's
    /// revisions); the GTK main context keeps calling it with no input.
    pub fn timeout_add_repeating(ms: u32, f: Box<dyn FnMut() -> bool>) -> Result<(), Error> {
        crate::backends::gtk::timeout_add_repeating(ms, f)
            .map_err(|e| Error::Backend(format!("{}", e)))
    }


    // ---- Canvas (cross-platform drawing surface) ----

    pub struct GtkDrawContext<'a> {
        cc: gtk_dynamic_loader::CairoContext<'a>,
    }

    impl<'a> GtkDrawContext<'a> {
        pub fn new(cr: *mut c_void, loader: &'a std::sync::Arc<gtk_dynamic_loader::Loader>) -> Self {
            GtkDrawContext { cc: gtk_dynamic_loader::CairoContext::new(loader, cr) }
        }
    }

    impl crate::core::DrawContext for GtkDrawContext<'_> {
        fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, a: f64) {
            self.cc.set_source_rgba(r, g, b, a);
            self.cc.rectangle(x, y, w, h);
            self.cc.fill();
        }
        fn stroke_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, a: f64, lw: f64) {
            self.cc.set_line_width(lw);
            self.cc.set_source_rgba(r, g, b, a);
            self.cc.rectangle(x, y, w, h);
            self.cc.stroke();
        }
        fn draw_text(&mut self, x: f64, y: f64, text: &str, font: &str, size: f64, r: f64, g: f64, b: f64, a: f64) {
            self.draw_text_styled(x, y, text, font, size, r, g, b, a, 0, 0)
        }
        fn draw_text_styled(&mut self, x: f64, y: f64, text: &str, font: &str, size: f64, r: f64, g: f64, b: f64, a: f64, slant: i32, weight: i32) {
            self.cc.save();
            self.cc.set_source_rgba(r, g, b, a);
            self.cc.select_font_face(font, slant, weight);
            self.cc.set_font_size(size);
            // Cairo's move_to(x, y) treats y as the text BASELINE.
            // The callers pass y as the TEXT TOP (matching GDI semantics).
            // Convert: baseline = top - y_bearing (y_bearing is negative,
            // so this ADDS the ascent to top).
            let e = self.cc.text_extents(text);
            let baseline = y - e.y_bearing;
            self.cc.move_to(x, baseline);
            self.cc.show_text(text);
            self.cc.restore();
        }
        fn text_extents(&self, text: &str, font: &str, size: f64) -> (f64, f64, f64, f64) {
            self.text_extents_styled(text, font, size, 0, 0)
        }
        fn text_extents_styled(&self, text: &str, font: &str, size: f64, slant: i32, weight: i32) -> (f64, f64, f64, f64) {
            self.cc.save();
            self.cc.select_font_face(font, slant, weight);
            self.cc.set_font_size(size);
            let e = self.cc.text_extents(text);
            self.cc.restore();
            (e.x_bearing, e.y_bearing, e.width, e.height)
        }
        fn clear(&mut self, r: f64, g: f64, b: f64, a: f64) {
            self.cc.set_source_rgba(r, g, b, a);
            self.cc.paint();
        }
        fn save(&mut self) { self.cc.save(); }
        fn restore(&mut self) { self.cc.restore(); }
        fn clip(&mut self, x: f64, y: f64, w: f64, h: f64) {
            self.cc.rectangle(x, y, w, h);
            self.cc.clip();
        }
        /// Blit an RGBA8 image. Straight-alpha in, premultiplied internally:
        /// see `gtk_dynamic_loader::wrappers::CairoContext::draw_rgba_image`,
        /// which does the conversion, because Cairo's ARGB32 is premultiplied
        /// and the two are not interchangeable.
        fn draw_rgba_image(&mut self, x: f64, y: f64, pixels: &[u8], width: u32, height: u32, scale: f64) -> bool {
            self.cc.draw_rgba_image(x, y, pixels, width, height, scale)
        }
    }

    /// Canvas wraps a DrawingArea into the cross-platform Canvas API.
    /// Stores event controllers in a reference-counted slot so they outlive
    /// the constructor scope (GTK4 controllers are freed if dropped).
    /// Also stores a copy of the draw callback for the `force_draw` fallback
    /// that renders directly to the window surface (bypassing the frame clock).
    /// The click callbacks registered on one GTK3 canvas, and the single
    /// `button-press-event` connection that drives them.
    ///
    /// See [`Canvas::click_handlers`] for why these live together rather than
    /// each owning a signal connection. `installed` is what makes the first
    /// registration connect the signal and later ones just fill in a slot.
    #[derive(Default)]
    struct PressHandlers {
        plain: Option<Box<dyn FnMut(f64, f64)>>,
        with_button: Option<Box<dyn FnMut(f64, f64, u32, u32)>>,
        /// Canonical gesture stream, fed from the same three native signals as
        /// the other callbacks so a caller can use one handler instead of three.
        gesture: Option<Box<dyn FnMut(crate::core::Gesture)>>,
        installed: bool,
    }

    impl PressHandlers {
        /// Map a GDK button number to the portable `Button` enum.
        ///
        /// GDK numbers 1/2/3 are left/middle/right, as everywhere else; the
        /// rest are passed through so a caller can still see 4..n.
        fn map_button(b: u32) -> crate::core::Button {
            match b {
                1 => crate::core::Button::Primary,
                2 => crate::core::Button::Middle,
                3 => crate::core::Button::Secondary,
                other => crate::core::Button::Other(other.min(255) as u8),
            }
        }

        /// Decode a GDK modifier mask into the portable `Modifiers`.
        ///
        /// The bits are gdk-sys' GdkModifierType: shift 1 << 0, lock 1 << 1,
        /// control 1 << 2, mod1 (alt) 1 << 3, and the super/command key is
        /// mod4 (1 << 6) on X11/Wayland and on Windows.
        fn map_state(state: u32) -> crate::core::Modifiers {
            crate::core::Modifiers {
                shift: state & (1 << 0) != 0,
                ctrl: state & (1 << 2) != 0,
                alt: state & (1 << 3) != 0,
                meta: state & (1 << 6) != 0,
            }
        }

        /// Whether any mouse button is down, per the GDK state mask.
        ///
        /// The button mask bits are 1 << 8 (button 1) through 1 << 12
        /// (button 5), so bits 8..=12 set means a drag rather than a hover.
        /// `on_motion` is documented as firing for both, and this is how the two
        /// are told apart without a second native handler.
        fn button_held(state: u32) -> bool {
            (state >> 8) & 0b1_1111 != 0
        }

        /// Feed the gesture stream, if one is registered.
        fn dispatch_gesture(&mut self, g: crate::core::Gesture) {
            if let Some(cb) = self.gesture.as_mut() {
                cb(g);
            }
        }

        /// Invoke whichever callbacks are registered, and report whether the
        /// press should be consumed.
        ///
        /// The button-aware callback is called first when both are present, so
        /// it sees the raw button number before the plain callback can decide
        /// anything. The press is always consumed once at least one callback
        /// ran — the historical `on_click` behaviour, which callers rely on.
        fn dispatch(&mut self, x: f64, y: f64, button: u32, state: u32) -> bool {
            if let Some(with_button) = self.with_button.as_mut() {
                with_button(x, y, button, state);
            }
            // `on_click` is the *primary* click, so it only fires for button 1.
            //
            // It used to be connected to `button-press-event` and called for
            // every button, which was invisible until the tab strip wanted a
            // right-click *and* a left-click on the same widget: the right-click
            // still ran the left-click path (switching sheets and queueing
            // redraws), and that redraw raced the context menu the right-click
            // had just opened, leaving the menu shown but not clickable.
            if button == 1 {
                if let Some(plain) = self.plain.as_mut() {
                    plain(x, y);
                }
            }
            self.with_button.is_some() || self.plain.is_some()
        }
    }

    pub struct Canvas {
        pub drawing_area: gtk_dynamic_loader::DrawingArea,
        draw_cb: Rc<RefCell<Option<Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>>>>,
        _controllers: Rc<RefCell<Vec<Box<dyn std::any::Any>>>>,
        /// The GTK3 `button-press-event` callbacks for this widget.
        ///
        /// `on_click` and `on_click_button` both want the same signal, and GTK3
        /// stops signal emission at the first handler that returns `TRUE`. Two
        /// independent connections therefore cannot both run: whichever is
        /// connected first consumes the press and the other silently never
        /// fires (observed directly — `on_click` connected first, so
        /// `on_click_button`'s handler never ran).
        ///
        /// So both registrations fill this one record instead of connecting
        /// separately, and a single installed handler invokes whichever are
        /// present. That is the only arrangement in which the tab strip — which
        /// registers both — sees the button number *and* keeps its plain
        /// left-click switch.
        click_handlers: Rc<RefCell<PressHandlers>>,
    }

    impl Clone for Canvas {
        fn clone(&self) -> Self {
            Canvas {
                drawing_area: self.drawing_area.clone(),
                draw_cb: self.draw_cb.clone(),
                _controllers: self._controllers.clone(),
                click_handlers: self.click_handlers.clone(),
            }
        }
    }
    impl AsRef<*mut c_void> for Canvas {
        fn as_ref(&self) -> &*mut c_void { self.drawing_area.as_ref() }
    }
    impl Widget for Canvas {
        fn raw_handle(&self) -> *mut c_void { *self.drawing_area.as_ref() }
    }

    impl Canvas {
        pub fn set_draw_callback(&self, cb: Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>) {
            // Store the callback for force_draw fallback
            *self.draw_cb.borrow_mut() = Some(cb);

            let loader = crate::backends::gtk::loader()
                .expect("GTK loader not initialized after Canvas creation");
            let symbols = &loader.symbols;
            if symbols.gtk_drawing_area_set_draw_func.is_some() {
                // GTK4 path — use draw_cb so force_draw can also invoke it
                let cb_stored = self.draw_cb.clone();
                let loader_clone = loader.clone();
                let _ = self.drawing_area.set_draw_func(Box::new(move |cr: *mut c_void, w: i32, h: i32| {
                    let mut ctx = GtkDrawContext::new(cr, &loader_clone);
                    if let Some(ref mut cb) = *cb_stored.borrow_mut() {
                        cb(&mut ctx, w, h);
                    }
                }));
                // Request an immediate initial redraw.  On GTK4 the frame clock
                // may not tick immediately (especially with the Cairo renderer
                // or on virtual displays such as WSL), so we explicitly queue a
                // redraw after setting the draw func to kickstart the first frame.
                self.drawing_area.queue_draw();
            } else {
                // GTK3 path — use widget allocation to provide real w/h
                let cb_stored = self.draw_cb.clone();
                let loader_clone = loader.clone();
                let _ = self.drawing_area.connect_draw_gtk3(Box::new(move |widget: *mut c_void, cr: *mut c_void| -> i32 {
                    let w = if let Some(f) = loader_clone.symbols.gtk_widget_get_allocated_width {
                        unsafe { f(widget) }
                    } else { 0 };
                    let h = if let Some(f) = loader_clone.symbols.gtk_widget_get_allocated_height {
                        unsafe { f(widget) }
                    } else { 0 };
                    let mut ctx = GtkDrawContext::new(cr, &loader_clone);
                    if let Some(ref mut cb) = *cb_stored.borrow_mut() {
                        cb(&mut ctx, w, h);
                    }
                    0
                }));
            }
        }

        /// Force an immediate draw of the canvas content directly to the window
        /// surface, bypassing the GTK4 frame clock.  This is a fallback for
        /// virtual displays (WSL, Xvfb) where the frame clock may never tick.
        /// `window_ptr` must be a valid GtkWindow pointer.
        ///
        /// `fallback_w`/`fallback_h` are used when the surface reports zero
        /// dimensions (the X11/Wayland surface hasn't been configured yet
        /// even though GTK widget allocation already reflects the requested
        /// default size from `set_default_size`).
        ///
        /// Two rendering paths are tried:
        /// 1. `gdk_surface_create_cairo_context` (GTK 4.14+) — preferred, creates
        ///    a GdkCairoContext that draws directly to the surface buffer.
        /// 2. `gdk_surface_begin_draw_frame` + `gdk_draw_context_get_cairo_context`
        ///    + `gdk_surface_end_draw_frame` (GTK 4.0-4.14) — deprecated but
        ///    present on older GTK4 (e.g. Ubuntu 24.04 with GTK 4.12).
        ///
        /// After rendering, `gdk_display_sync` is called to flush the display.
        pub fn force_draw(&self, window_ptr: *mut c_void, fallback_w: i32, fallback_h: i32) {
            let loader = match crate::backends::gtk::loader() {
                Some(l) => l,
                None => return,
            };
            let symbols = &loader.symbols;
            let get_surface = match symbols.gtk_native_get_surface {
                Some(f) => f,
                None => return,
            };
            let get_w = match symbols.gdk_surface_get_width {
                Some(f) => f,
                None => return,
            };
            let get_h = match symbols.gdk_surface_get_height {
                Some(f) => f,
                None => return,
            };

            let surface = unsafe { get_surface(window_ptr) };
            if surface.is_null() { return; }
            let mut w = unsafe { get_w(surface) };
            let mut h = unsafe { get_h(surface) };
            if w <= 0 || h <= 0 {
                // Surface not yet configured by display server.  Use caller-provided
                // fallback dimensions so the draw callback still runs and the canvas
                // claims focus despite the absent server-side configuration.
                w = fallback_w;
                h = fallback_h;
                if w <= 0 || h <= 0 { return; }
            }

            // Approach A (GTK 4.14+): gdk_surface_create_cairo_context
            if let Some(create_cairo) = symbols.gdk_surface_create_cairo_context {
                let cairo_destroy = match symbols.cairo_destroy {
                    Some(f) => f,
                    None => return,
                };
                let cr = unsafe { create_cairo(surface) };
                if cr.is_null() { return; }
                let mut ctx = GtkDrawContext::new(cr, &loader);
                if let Some(ref mut cb) = *self.draw_cb.borrow_mut() {
                    cb(&mut ctx, w, h);
                }
                unsafe { cairo_destroy(cr); }
            } else {
                // Approach B (GTK 4.0-4.14): begin_draw_frame + end_draw_frame
                let begin_frame = match symbols.gdk_surface_begin_draw_frame {
                    Some(f) => f,
                    None => return,
                };
                let get_cr = match symbols.gdk_draw_context_get_cairo_context {
                    Some(f) => f,
                    None => return,
                };
                let end_frame = match symbols.gdk_surface_end_draw_frame {
                    Some(f) => f,
                    None => return,
                };
                let cairo_destroy = match symbols.cairo_destroy {
                    Some(f) => f,
                    None => return,
                };
                let context = unsafe { begin_frame(surface, std::ptr::null_mut()) };
                if context.is_null() { return; }
                let cr = unsafe { get_cr(context) };
                if cr.is_null() {
                    unsafe { end_frame(surface, context); }
                    return;
                }
                let mut ctx = GtkDrawContext::new(cr, &loader);
                if let Some(ref mut cb) = *self.draw_cb.borrow_mut() {
                    cb(&mut ctx, w, h);
                }
                unsafe { cairo_destroy(cr); }
                unsafe { end_frame(surface, context); }
            }

            // Sync the display to ensure the rendered content reaches the
            // display server (X11: XFlush; Wayland: wl_display_flush).
            if let (Some(get_disp), Some(disp_sync)) = (
                symbols.gtk_widget_get_display,
                symbols.gdk_display_sync,
            ) {
                let display = unsafe { get_disp(window_ptr) };
                if !display.is_null() {
                    unsafe { disp_sync(display); }
                }
            }
        }

        pub fn queue_redraw(&self) {
            self.drawing_area.queue_draw();
        }

        pub fn set_size_request(&self, w: i32, h: i32) {
            self.drawing_area.set_size_request(w, h);
        }

        pub fn set_content_size(&self, w: i32, h: i32) {
            self.drawing_area.set_content_width(w);
            self.drawing_area.set_content_height(h);
        }

        /// Click with the button number and modifier state.
        ///
        /// `button` is the GDK button (1 = left, 2 = middle, 3 = right); `state`
        /// is the `GdkModifierType` mask (bit 0 Shift, bit 2 Control, bit 3 Alt).
        ///
        /// Added alongside [`Self::on_click`] rather than replacing it: `on_click`
        /// is called by every backend's adapter and by corro, and a signature
        /// change there would touch all eight backends including the mobile ones
        /// that cannot report a button at all. Callers that need to distinguish a
        /// right-click use this; everything else keeps `on_click`.
        ///
        /// On GTK4 the `GestureClick` "pressed" signal already carries the button
        /// number, and its `current_button` property gives the state mask; on
        /// GTK3 both come off the event with `gdk_event_get_button` /
        /// `gdk_event_get_state`.
        pub fn on_click_button(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            let loader = crate::backends::gtk::loader()
                .expect("GTK loader not initialized after Canvas creation");
            let symbols = &loader.symbols;
            let inner = *self.drawing_area.as_ref();
            let is_gtk4 = symbols.gtk_drawing_area_set_draw_func.is_some();
            if is_gtk4 {
                if let Ok(gesture) = gtk_dynamic_loader::GestureClick::new(loader.clone()) {
                    let mut cb = cb;
                    let _ = gesture.connect_pressed(Box::new(
                        move |n: i32, x: f64, y: f64| {
                            // GTK4 reports the button as 0 for "no button" in
                            // some synthesised presses; fall back to left.
                            let button = if n > 0 { n as u32 } else { 1 };
                            cb(x, y, button, 0);
                        },
                    ));
                    gesture.add_to_widget(&self.drawing_area);
                    self._controllers.borrow_mut().push(Box::new(gesture));
                }
                return;
            }
            // GDK event masks. Values are from gdk-sys 0.18 (GdkEventMask):
            // press 256, release 512, plain motion 4, motion-with-button1 32.
            //
            // BUTTON1_MOTION is the one a drag needs: plain POINTER_MOTION is
            // only delivered with no button held, so requesting it alone gave a
            // strip that saw presses but no motion in between, i.e. no drag.
            // `gtk_widget_add_events` only ever *adds* bits, so calling it from
            // several of these methods is safe.
            const GDK_BUTTON_PRESS_MASK: i32 = 256;
            const GDK_BUTTON_RELEASE_MASK: i32 = 512;
            const GDK_POINTER_MOTION_MASK: i32 = 4;
            const GDK_BUTTON1_MOTION_MASK: i32 = 32;
            unsafe {
                gtk_dynamic_loader::widget_add_events(
                    &loader,
                    inner,
                    GDK_BUTTON_PRESS_MASK
                        | GDK_BUTTON_RELEASE_MASK
                        | GDK_POINTER_MOTION_MASK
                        | GDK_BUTTON1_MOTION_MASK,
                );
            }
            let install = {
                let mut h = self.click_handlers.borrow_mut();
                h.with_button = Some(cb);
                if h.installed {
                    false
                } else {
                    h.installed = true;
                    true
                }
            };
            if !install {
                return;
            }
            // One connection drives both this and any later `on_click`.
            let handlers = self.click_handlers.clone();
            let l2 = loader.clone();
            let l3 = l2.clone();
            unsafe {
                let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                    &l3,
                    inner,
                    "button-press-event",
                    Box::new(move |ev: *mut c_void| -> i32 {
                        let Some((x, y)) = gtk_dynamic_loader::gdk_event_get_coords(&l2, ev) else {
                            return 0;
                        };
                        let button = gtk_dynamic_loader::gdk_event_get_button(&l2, ev).unwrap_or(1);
                        let state = gtk_dynamic_loader::gdk_event_get_state(&l2, ev).unwrap_or(0);
                        let consumed = {
                            let mut h = handlers.borrow_mut();
                            h.dispatch_gesture(crate::core::Gesture::Button {
                                button: PressHandlers::map_button(button),
                                pressed: true,
                                x,
                                y,
                                mods: PressHandlers::map_state(state),
                            });
                            h.dispatch(x, y, button, state)
                        };
                        if consumed { 1 } else { 0 }
                    }),
                );
            }
        }

        /// The canonical gesture stream: one callback for hover, drag, buttons
        /// and (with `on_scroll` installed by the host) scroll.
        ///
        /// This is the recommended way to handle pointer input on a canvas.
        /// The older `on_click` / `on_motion` / `on_release` remain for callers
        /// that want the raw numbers, but a caller that needs a drag has to
        /// reassemble one from three separate signals by hand -- including
        /// working out the click-versus-drag threshold, which is the part
        /// that is easy to get subtly wrong. Feeding everything into one
        /// `Gesture` and letting [`crate::core::DragTracker`] do that is why
        /// this exists.
        ///
        /// Shares the single signal connection the other callbacks use, so
        /// registering it does not add a second connection per widget.
        pub fn on_gesture(&self, cb: Box<dyn FnMut(crate::core::Gesture)>) {
            let loader = crate::backends::gtk::loader()
                .expect("GTK loader not initialized after Canvas creation");
            let inner = *self.drawing_area.as_ref();
            if loader.symbols.gtk_drawing_area_set_draw_func.is_some() {
                // GTK4 uses event controllers rather than widget signals; the
                // existing click path handles that and the motion path does
                // not yet (see `on_motion`). Not a silent no-op: the caller
                // gets no events on GTK4 rather than wrong ones.
                return;
            }
            // press 256, release 512, motion 4, button1-motion 32. The drag
            // needs 32: plain POINTER_MOTION is only delivered with no button
            // held. `gtk_widget_add_events` only adds bits, so this is safe to
            // call alongside the other registrations.
            const GDK_BUTTON_PRESS_MASK: i32 = 256;
            const GDK_BUTTON_RELEASE_MASK: i32 = 512;
            const GDK_POINTER_MOTION_MASK: i32 = 4;
            const GDK_BUTTON1_MOTION_MASK: i32 = 32;
            unsafe {
                gtk_dynamic_loader::widget_add_events(
                    &loader,
                    inner,
                    GDK_BUTTON_PRESS_MASK
                        | GDK_BUTTON_RELEASE_MASK
                        | GDK_POINTER_MOTION_MASK
                        | GDK_BUTTON1_MOTION_MASK,
                );
            }
            {
                let mut h = self.click_handlers.borrow_mut();
                h.gesture = Some(cb);
            }

            // Install the connections that feed the stream. This is the part
            // that is easy to omit: registering the callback and adding the
            // event masks is not enough, because a widget only delivers
            // `motion-notify-event` to a handler that is actually connected to
            // it. `on_click_button`/`on_motion`/`on_release` each connect their
            // own signal; on_gesture has to connect too, or it silently never
            // fires -- which is exactly what the first version of this did.
            self.install_scroll_gesture();

            let handlers = self.click_handlers.clone();
            let l2 = loader.clone();
            let l3 = l2.clone();

            unsafe {
                let h = l2.clone();
                let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                    &l3,
                    inner,
                    "motion-notify-event",
                    Box::new(move |ev: *mut c_void| -> i32 {
                        let Some((x, y)) = gtk_dynamic_loader::gdk_event_get_coords(&h, ev) else {
                            return 0;
                        };
                        let state = gtk_dynamic_loader::gdk_event_get_state(&h, ev).unwrap_or(0);
                        let g = if PressHandlers::button_held(state) {
                            crate::core::Gesture::Drag { x, y, mods: PressHandlers::map_state(state) }
                        } else {
                            crate::core::Gesture::Hover { x, y, mods: PressHandlers::map_state(state) }
                        };
                        handlers.borrow_mut().dispatch_gesture(g);
                        0
                    }),
                );
            }

            let handlers = self.click_handlers.clone();
            let l2 = loader.clone();
            let l3 = l2.clone();
            unsafe {
                let h = l2.clone();
                let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                    &l3,
                    inner,
                    "button-press-event",
                    Box::new(move |ev: *mut c_void| -> i32 {
                        let Some((x, y)) = gtk_dynamic_loader::gdk_event_get_coords(&h, ev) else {
                            return 0;
                        };
                        let button = gtk_dynamic_loader::gdk_event_get_button(&h, ev).unwrap_or(1);
                        let state = gtk_dynamic_loader::gdk_event_get_state(&h, ev).unwrap_or(0);
                        handlers.borrow_mut().dispatch_gesture(crate::core::Gesture::Button {
                            button: PressHandlers::map_button(button),
                            pressed: true,
                            x,
                            y,
                            mods: PressHandlers::map_state(state),
                        });
                        0
                    }),
                );
            }

            let handlers = self.click_handlers.clone();
            let l2 = loader.clone();
            let l3 = l2.clone();
            unsafe {
                let h = l2.clone();
                let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                    &l3,
                    inner,
                    "button-release-event",
                    Box::new(move |ev: *mut c_void| -> i32 {
                        let Some((x, y)) = gtk_dynamic_loader::gdk_event_get_coords(&h, ev) else {
                            return 0;
                        };
                        let button = gtk_dynamic_loader::gdk_event_get_button(&h, ev).unwrap_or(1);
                        let state = gtk_dynamic_loader::gdk_event_get_state(&h, ev).unwrap_or(0);
                        handlers.borrow_mut().dispatch_gesture(crate::core::Gesture::Button {
                            button: PressHandlers::map_button(button),
                            pressed: false,
                            x,
                            y,
                            mods: PressHandlers::map_state(state),
                        });
                        0
                    }),
                );
            }
        }

        /// Connect the wheel / touchpad scroll signal to the gesture stream.
        ///
        /// Called by `on_gesture`; exposed separately so a host that wants a
        /// scroll handler without the rest can install just this one.
        fn install_scroll_gesture(&self) {
            let loader = crate::backends::gtk::loader()
                .expect("GTK loader not initialized after Canvas creation");
            let inner = *self.drawing_area.as_ref();
            if loader.symbols.gtk_drawing_area_set_draw_func.is_some() {
                return; // GTK4: as `on_gesture`.
            }
            // GDK_SCROLL_MASK = 1 << 21 (2097152).
            const GDK_SCROLL_MASK: i32 = 1 << 21;
            unsafe {
                gtk_dynamic_loader::widget_add_events(&loader, inner, GDK_SCROLL_MASK);
            }
            let handlers = self.click_handlers.clone();
            let l2 = loader.clone();
            let l3 = l2.clone();
            unsafe {
                let h = l2.clone();
                let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                    &l3,
                    inner,
                    "scroll-event",
                    Box::new(move |ev: *mut c_void| -> i32 {
                        let Some((dx, dy)) = gtk_dynamic_loader::gdk_event_get_scroll_deltas(&h, ev)
                        else {
                            return 0;
                        };
                        // Horizontal deltas are carried through: the `Gesture`
                        // type distinguishes them, and `ZoomState` is the thing
                        // that decides to ignore them. Deciding here would make
                        // a horizontal pan impossible.
                        let (x, y) =
                            gtk_dynamic_loader::gdk_event_get_coords(&h, ev).unwrap_or((0.0, 0.0));
                        let state = gtk_dynamic_loader::gdk_event_get_state(&h, ev).unwrap_or(0);
                        handlers.borrow_mut().dispatch_gesture(crate::core::Gesture::Scroll {
                            delta: crate::core::ScrollDelta { dx, dy },
                            x,
                            y,
                            mods: PressHandlers::map_state(state),
                        });
                        1 // consumed: a wheel over the canvas should not scroll the page
                    }),
                );
            }
        }

        /// Pointer motion over the canvas, whether or not a button is held.
        ///
        /// `state` is the modifier mask; the GDK button mask bits
        /// (`GDK_BUTTON1_MASK` = 1 << 8, `GDK_BUTTON3_MASK` = 1 << 10) say which
        /// button is down, which is how a caller distinguishes "hovering" from
        /// "dragging with the left button".
        pub fn on_motion(&self, cb: Box<dyn FnMut(f64, f64, u32)>) {
            let loader = crate::backends::gtk::loader()
                .expect("GTK loader not initialized after Canvas creation");
            let inner = *self.drawing_area.as_ref();
            if loader.symbols.gtk_drawing_area_set_draw_func.is_some() {
                // GTK4 uses event controllers; a motion controller is a separate
                // object type from GestureClick, so this path is left to the
                // GTK3 backend until a GTK4 host needs it (the desktop GUI runs
                // GTK3 by default - see `GTK_DLOPEN_PREFER_GTK3`).
                return;
            }
            // Plain motion (4, no button held) plus motion-with-button1 (32,
            // a drag). See `on_click_button` for the mask values.
            const GDK_POINTER_MOTION_MASK: i32 = 4;
            const GDK_BUTTON1_MOTION_MASK: i32 = 32;
            unsafe {
                gtk_dynamic_loader::widget_add_events(
                    &loader,
                    inner,
                    GDK_POINTER_MOTION_MASK | GDK_BUTTON1_MOTION_MASK,
                );
            }
            // The shared handler, so the gesture stream is fed from the same
            // single signal connection the other callbacks use rather than a
            // second one.
            let gesture_handlers = self.click_handlers.clone();
            let mut cb = cb;
            let l2 = loader.clone();
            let l3 = l2.clone();
            unsafe {
                let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                    &l3,
                    inner,
                    "motion-notify-event",
                    Box::new(move |ev: *mut c_void| -> i32 {
                        if let Some((x, y)) = gtk_dynamic_loader::gdk_event_get_coords(&l2, ev) {
                            let state =
                                gtk_dynamic_loader::gdk_event_get_state(&l2, ev).unwrap_or(0);
                            {
                                let mut h = gesture_handlers.borrow_mut();
                                let g = if PressHandlers::button_held(state) {
                                    crate::core::Gesture::Drag { x, y, mods: PressHandlers::map_state(state) }
                                } else {
                                    crate::core::Gesture::Hover { x, y, mods: PressHandlers::map_state(state) }
                                };
                                h.dispatch_gesture(g);
                            }
                            cb(x, y, state);
                            return 0; // do not consume: hover must not block others
                        }
                        0
                    }),
                );
            }
        }

        /// Pointer release: ends a drag. `button` as in [`Self::on_click_button`].
        pub fn on_release(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            let loader = crate::backends::gtk::loader()
                .expect("GTK loader not initialized after Canvas creation");
            let symbols = &loader.symbols;
            let inner = *self.drawing_area.as_ref();
            if symbols.gtk_drawing_area_set_draw_func.is_some() {
                return; // GTK4: see `on_motion`
            }
            const GDK_BUTTON_RELEASE_MASK: i32 = 512;
            unsafe { gtk_dynamic_loader::widget_add_events(&loader, inner, GDK_BUTTON_RELEASE_MASK); }
            // The shared handler, so the gesture stream is fed from the same
            // single signal connection the other callbacks use rather than a
            // second one.
            let gesture_handlers = self.click_handlers.clone();
            let mut cb = cb;
            let l2 = loader.clone();
            let l3 = l2.clone();
            unsafe {
                let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                    &l3,
                    inner,
                    "button-release-event",
                    Box::new(move |ev: *mut c_void| -> i32 {
                        if let Some((x, y)) = gtk_dynamic_loader::gdk_event_get_coords(&l2, ev) {
                            let button =
                                gtk_dynamic_loader::gdk_event_get_button(&l2, ev).unwrap_or(1);
                            let state =
                                gtk_dynamic_loader::gdk_event_get_state(&l2, ev).unwrap_or(0);
                            {
                                let mut h = gesture_handlers.borrow_mut();
                                h.dispatch_gesture(crate::core::Gesture::Button {
                                    button: PressHandlers::map_button(button),
                                    pressed: false,
                                    x,
                                    y,
                                    mods: PressHandlers::map_state(state),
                                });
                            }
                            cb(x, y, button, state);
                        }
                        // Do NOT consume the release: a popup menu opened over
                        // this widget completes its row activation on the
                        // button release, and returning 1 here swallowed that
                        // (the menu appeared at the right place, but no row
                        // ever activated).
                        0
                    }),
                );
            }
        }

        pub fn on_click(&self, cb: Box<dyn FnMut(f64, f64)>) {
            // Minimal click contract: every backend implements this. The
            // button/modifier-aware variant is `on_click_button`; when both are
            // registered they share one GTK3 signal connection (see
            // `Canvas::click_handlers`) rather than racing for the press.
            let loader = crate::backends::gtk::loader()
                .expect("GTK loader not initialized after Canvas creation");
            let symbols = &loader.symbols;
            let inner = *self.drawing_area.as_ref();
            // gtk_drawing_area_set_draw_func is GTK4-only; gtk_gesture_click_new exists in GTK3 >= 3.24
            let is_gtk4 = symbols.gtk_drawing_area_set_draw_func.is_some();
            if is_gtk4 {
                // GTK4: use GestureClick — store in _controllers to keep alive
                if let Ok(gesture) = gtk_dynamic_loader::GestureClick::new(loader.clone()) {
                    let mut cb = cb;
                    let _ = gesture.connect_pressed(Box::new(move |_n: i32, x: f64, y: f64| {
                        cb(x, y);
                    }));
                    gesture.add_to_widget(&self.drawing_area);
                    self._controllers.borrow_mut().push(Box::new(gesture));
                }
                return;
            }
            let install = {
                let mut h = self.click_handlers.borrow_mut();
                h.plain = Some(cb);
                if h.installed {
                    false
                } else {
                    h.installed = true;
                    true
                }
            };
            if !install {
                return;
            }
            // Only the plain callback is registered: ask for the press mask and
            // connect the shared handler. A later `on_click_button` will find
            // `installed == true` and just fill its slot.
            let mask = 256; // GDK_BUTTON_PRESS_MASK (gdk-sys: GdkEventMask)
            unsafe { gtk_dynamic_loader::widget_add_events(&loader, inner, mask); }
            let handlers = self.click_handlers.clone();
            let l2 = loader.clone();
            let l3 = l2.clone();
            unsafe {
                let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                    &l3, inner, "button-press-event",
                    Box::new(move |ev: *mut c_void| -> i32 {
                        let Some((x, y)) = gtk_dynamic_loader::gdk_event_get_coords(&l2, ev) else {
                            return 0;
                        };
                        let button = gtk_dynamic_loader::gdk_event_get_button(&l2, ev).unwrap_or(1);
                        let state = gtk_dynamic_loader::gdk_event_get_state(&l2, ev).unwrap_or(0);
                        if handlers.borrow_mut().dispatch(x, y, button, state) { 1 } else { 0 }
                    }),
                );
            }
        }


        /// This canvas's top-left in screen coordinates, for placing a context
        /// menu at a widget-relative click point.
        ///
        /// `None` before the widget is realized or when the GTK4 path lacks the
        /// origin symbols; callers then fall back to an unpositioned popup.
        pub fn screen_origin(&self) -> Option<(i32, i32)> {
            let loader = crate::backends::gtk::loader()?;
            let inner = *self.drawing_area.as_ref();
            unsafe { gtk_dynamic_loader::widget_screen_origin(&loader, inner) }
        }

        pub fn on_key(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
            let loader = crate::backends::gtk::loader()
                .expect("GTK loader not initialized after Canvas creation");
            let symbols = &loader.symbols;
            let inner = *self.drawing_area.as_ref();
            // gtk_drawing_area_set_draw_func is GTK4-only; gtk_gesture_click_new exists in GTK3 >= 3.24
            let is_gtk4 = symbols.gtk_drawing_area_set_draw_func.is_some();
            if is_gtk4 {
                if let Ok(ctrl) = gtk_dynamic_loader::EventControllerKey::new(loader.clone()) {
                    let mut cb = cb;
                    let _ = ctrl.connect_key_pressed(Box::new(move |keyval: u32, state: u32| -> i32 {
                        if cb(keyval, state) { 1 } else { 0 }
                    }));
                    swallow_key_releases_gtk4(&ctrl);
                    ctrl.add_to_widget(&self.drawing_area);
                    self._controllers.borrow_mut().push(Box::new(ctrl));
                }
            } else {
                let mut cb = cb;
                let l2 = loader.clone();
                let l3 = l2.clone();
                unsafe {
                    let _ = gtk_dynamic_loader::widget_connect_signal_bool(
                        &l3, inner, "key-press-event",
                        Box::new(move |ev: *mut c_void| -> i32 {
                            let keyval = gtk_dynamic_loader::EventControllerKey::get_keyval_static(&l2, ev);
                            if keyval == 0 { return 0; }
                            let state = gtk_dynamic_loader::EventControllerKey::get_state_static(&l2, ev);
                            if cb(keyval, state) { 1 } else { 0 }
                        }),
                    );
                }
            }
        }

        pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
            self.on_key(cb);
        }

        pub fn grab_focus(&self) {
            self.drawing_area.grab_focus();
        }

        pub fn set_can_focus(&self, can: bool) {
            self.drawing_area.set_can_focus(can);
        }

        pub fn set_visible(&self, visible: bool) {
            self.drawing_area.set_visible(visible);
        }
        /// Whether the canvas may take extra horizontal space.
        ///
        /// Missing until now, so a `Canvas` inside a `ScrolledWindow` could not
        /// be told to fill it: the viewer had to fall back to a fixed
        /// `set_size_request`, which does not follow the window being resized.
        /// The loader already binds `gtk_widget_set_hexpand`.
        pub fn set_hexpand(&self, expand: bool) { self.drawing_area.set_hexpand(expand); }
        pub fn set_vexpand(&self, expand: bool) { self.drawing_area.set_vexpand(expand); }
        /// Outer spacing in pixels; `start`/`end` are the logical-direction
        /// (left/right) names, so they follow the locale's text direction.
        pub fn set_margin_top(&self, px: i32) { self.drawing_area.set_margin_top(px); }
        pub fn set_margin_start(&self, px: i32) { self.drawing_area.set_margin_start(px); }
    }

    pub fn create_canvas() -> Result<Canvas, Error> {
        let da = crate::backends::gtk::create_drawing_area().map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(Canvas {
            drawing_area: da,
            draw_cb: Rc::new(RefCell::new(None)),
            _controllers: Rc::new(RefCell::new(Vec::new())),
            click_handlers: Rc::new(RefCell::new(PressHandlers::default())),
        })
    }

    // ---- ScrolledWindow ----

    #[repr(transparent)]
    pub struct ScrolledWindow(pub GScrolledWindow);

    impl Clone for ScrolledWindow { fn clone(&self) -> Self { ScrolledWindow(self.0.clone()) } }

    impl ScrolledWindow {
        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            self.0.set_child(child);
        }
        pub fn set_policy(&self, hscroll: u32, vscroll: u32) {
            self.0.set_policy(hscroll, vscroll);
        }
        /// Configure both scrollbar adjustments atomically:
        /// (value, upper, page) with lower 0 and step 1. Lower/upper define
        /// the scroll domain, page the thumb size, value the position.
        /// Backend-agnostic scrollbar model shared with the nwg backend:
        /// value tracks an item index (row/col), not pixels.
        pub fn scroll_to(&self, hval: f64, hupper: f64, hpage: f64, vval: f64, vupper: f64, vpage: f64) {
            if let Some(loader) = crate::backends::gtk::loader() {
                let symbols = &loader.symbols;
                let (Some(get_h), Some(get_v), Some(configure)) = (
                    symbols.gtk_scrolled_window_get_hadjustment,
                    symbols.gtk_scrolled_window_get_vadjustment,
                    symbols.gtk_adjustment_configure,
                ) else { return; };
                unsafe {
                    let sw = *self.0.as_ref();
                    if sw.is_null() {
                        return;
                    }
                    let hadj = get_h(sw);
                    let vadj = get_v(sw);
                    if !hadj.is_null() {
                        configure(hadj, hval, 0.0, hupper.max(1.0), 1.0, hpage.max(1.0), hpage.max(1.0));
                    }
                    if !vadj.is_null() {
                        configure(vadj, vval, 0.0, vupper.max(1.0), 1.0, vpage.max(1.0), vpage.max(1.0));
                    }
                }
            }
        }
        /// Notify on user scrollbar interaction: `cb(vertical, value)`.
        /// Fires for thumb drags and trough clicks (both route through the
        /// adjustment's value-changed signal).
        /// Notify on user scrollbar interaction: `cb(vertical, value)`.
        /// Fires for thumb drags and trough clicks (both route through the
        /// adjustment's value-changed signal). One shared callback serves
        /// both adjustments; connect_signal frees it via destroy notify.
        pub fn on_scroll(&self, cb: Box<dyn FnMut(bool, f64)>) {
            if let Some(loader) = crate::backends::gtk::loader() {
                let symbols = &loader.symbols;
                let (Some(get_h), Some(get_v), Some(get_val)) = (
                    symbols.gtk_scrolled_window_get_hadjustment,
                    symbols.gtk_scrolled_window_get_vadjustment,
                    symbols.gtk_adjustment_get_value,
                ) else { return; };
                let sw = *self.0.as_ref();
                if sw.is_null() { return; }
                let shared: std::rc::Rc<std::cell::RefCell<Box<dyn FnMut(bool, f64)>>> =
                    std::rc::Rc::new(std::cell::RefCell::new(cb));
                unsafe {
                    let hadj = get_h(sw);
                    if !hadj.is_null() {
                        let sc = shared.clone();
                        let _ = gtk_dynamic_loader::connect_signal(
                            symbols, hadj, "value-changed",
                            Box::new(move || {
                                if let Ok(mut f) = sc.try_borrow_mut() {
                                    f(false, get_val(hadj));
                                }
                            }),
                            0,
                        );
                    }
                    let vadj = get_v(sw);
                    if !vadj.is_null() {
                        let sc = shared.clone();
                        let _ = gtk_dynamic_loader::connect_signal(
                            symbols, vadj, "value-changed",
                            Box::new(move || {
                                if let Ok(mut f) = sc.try_borrow_mut() {
                                    f(true, get_val(vadj));
                                }
                            }),
                            0,
                        );
                    }
                }
            }
        }
        pub fn set_hexpand(&self, expand: bool) { self.0.set_hexpand(expand); }
        pub fn set_vexpand(&self, expand: bool) { self.0.set_vexpand(expand); }
        pub fn set_size_request(&self, w: i32, h: i32) { self.0.set_size_request(w, h); }
    }

    impl AsRef<*mut c_void> for ScrolledWindow { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }
    impl Widget for ScrolledWindow { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }

    pub fn create_scrolled_window() -> Result<ScrolledWindow, Error> {
        let loader = crate::backends::gtk::loader()
            .ok_or_else(|| Error::Backend("GTK loader not initialized".into()))?;
        let sw = gtk_dynamic_loader::ScrolledWindow::new(loader.clone())
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(ScrolledWindow(sw))
    }

    // ---- Overlay (cross-platform stacking container) ----

    #[repr(transparent)]
    pub struct Overlay(pub gtk_dynamic_loader::Overlay);

    impl Clone for Overlay { fn clone(&self) -> Self { Overlay(self.0.clone()) } }
    impl AsRef<*mut c_void> for Overlay { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }
    impl Widget for Overlay { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }

    impl Overlay {
        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            self.0.set_child(child);
        }
        /// Add an overlay child. The child is wrapped in a `GtkFixed` so it
        /// can be positioned absolutely **and** gets the size it requests —
        /// a bare overlay child is allocated its minimum (observed 1x1),
        /// which made the in-grid dropdown invisible.
        pub fn add_overlay(&self, child: &impl AsRef<*mut c_void>) {
            match gtk_dynamic_loader::Fixed::new(
                crate::backends::gtk::loader().expect("gtk loader"),
            ) {
                Ok(fixed) => {
                    fixed.set_size_request(1, 1);
                    fixed.put(*child.as_ref(), 0, 0);
                    // GTK3: a child added to an already-visible container
                    // must be shown explicitly, or it stays unmapped (1x1).
                    fixed.show_child(*child.as_ref());
                    self.0.add_overlay(&fixed);
                    // Keep the Fixed alive: GTK owns it once added, but the
                    // wrapper must not drop its Rust bookkeeping early.
                    std::mem::forget(fixed);
                }
                Err(_) => {
                    // No GtkFixed available: fall back to a plain overlay
                    // child (positioning then degrades to stacked layout).
                    self.0.add_overlay(child);
                }
            }
        }
        pub fn set_overlay_pass_through(&self, child: &impl AsRef<*mut c_void>, pass: bool) {
            self.0.set_overlay_pass_through(child, pass);
        }
        pub fn remove(&self, child: &impl AsRef<*mut c_void>) {
            self.0.remove(child);
        }
        pub fn show_all(&self) {
            self.0.show_all();
        }
        pub fn set_size_request(&self, w: i32, h: i32) { self.0.set_size_request(w, h); }
        pub fn set_hexpand(&self, expand: bool) { self.0.set_hexpand(expand); }
        pub fn set_vexpand(&self, expand: bool) { self.0.set_vexpand(expand); }
    }

    // ---- Fixed (absolute positioning) ----

    #[repr(transparent)]
    pub struct Fixed(pub gtk_dynamic_loader::Fixed);

    impl Clone for Fixed { fn clone(&self) -> Self { Fixed(self.0.clone()) } }
    impl AsRef<*mut c_void> for Fixed { fn as_ref(&self) -> &*mut c_void { self.0.as_ref() } }
    impl Widget for Fixed { fn raw_handle(&self) -> *mut c_void { *self.0.as_ref() } }

    impl Fixed {
        /// Place `child` at `(x, y)` with the given size. `GtkFixed` honours
        /// the child's size request, so an absolutely-positioned child (the
        /// in-grid dropdown) gets the rectangle it asked for.
        pub fn put(&self, child: &impl AsRef<*mut c_void>, x: i32, y: i32) {
            self.0.put(*child.as_ref(), x, y);
        }
        pub fn set_size_request(&self, w: i32, h: i32) { self.0.set_size_request(w, h); }
        pub fn set_hexpand(&self, e: bool) { self.0.set_hexpand(e); }
        pub fn set_vexpand(&self, e: bool) { self.0.set_vexpand(e); }
        pub fn show_all(&self) {}
    }

    /// Creates a new GTK Fixed container.
    pub fn create_fixed() -> Result<Fixed, Error> {
        let loader = crate::backends::gtk::loader()
            .ok_or_else(|| Error::Backend("GTK loader not initialized".into()))?;
        let f = gtk_dynamic_loader::Fixed::new(loader.clone())
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(Fixed(f))
    }

    /// Creates a new GTK Overlay widget.
    pub fn create_overlay() -> Result<Overlay, Error> {
        let loader = crate::backends::gtk::loader()
            .ok_or_else(|| Error::Backend("GTK loader not initialized".into()))?;
        let o = gtk_dynamic_loader::Overlay::new(loader.clone())
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(Overlay(o))
    }

    // ---- File dialogs ----

    /// Opens a file dialog and returns the selected file path.
    pub fn open_file(title: &str) -> Result<Option<String>, Error> {
        let loader = crate::backends::gtk::loader()
            .ok_or_else(|| Error::Backend("GTK loader not initialized".into()))?;
        if let Ok(chooser) = unsafe { gtk_dynamic_loader::FileChooserNative::open(loader.clone(), title, std::ptr::null_mut()) } {
            if chooser.run() == -3 {
                return Ok(chooser.get_filename());
            }
        }
        Ok(None)
    }

    /// Opens a file save dialog and returns the selected file path.
    pub fn save_file(title: &str) -> Result<Option<String>, Error> {
        save_file_filtered(title, &[], "")
    }

    /// Save dialog with file-type filters (`(name, patterns)` with patterns
    /// like `*.corro`) and an optional suggested filename. No filters (or
    /// missing loader symbols) degrades to the plain dialog; empty
    /// suggested name leaves the entry blank.
    pub fn save_file_filtered(title: &str, filters: &[(&str, &[&str])], current_name: &str) -> Result<Option<String>, Error> {
        let loader = crate::backends::gtk::loader()
            .ok_or_else(|| Error::Backend("GTK loader not initialized".into()))?;
        if let Ok(chooser) = unsafe { gtk_dynamic_loader::FileChooserNative::save(loader.clone(), title, std::ptr::null_mut()) } {
            for (name, pats) in filters {
                chooser.add_filter(name, pats);
            }
            if !current_name.is_empty() {
                chooser.set_current_name(current_name);
            }
            if chooser.run() == -3 {
                return Ok(chooser.get_filename());
            }
        }
        Ok(None)
    }

    /// Open dialog with file-type filters (same semantics as
    /// [`Self::save_file_filtered`], minus the suggested name).
    pub fn open_file_filtered(title: &str, filters: &[(&str, &[&str])]) -> Result<Option<String>, Error> {
        let loader = crate::backends::gtk::loader()
            .ok_or_else(|| Error::Backend("GTK loader not initialized".into()))?;
        if let Ok(chooser) = unsafe { gtk_dynamic_loader::FileChooserNative::open(loader.clone(), title, std::ptr::null_mut()) } {
            for (name, pats) in filters {
                chooser.add_filter(name, pats);
            }
            if chooser.run() == -3 {
                return Ok(chooser.get_filename());
            }
        }
        Ok(None)
    }

    // ---- Spreadsheet (cross-platform grid widget) ----

    /// A spreadsheet widget that combines a canvas with an overlay for cross-platform grid rendering.
    pub struct Spreadsheet(pub Canvas, pub Overlay);

    impl Clone for Spreadsheet { fn clone(&self) -> Self { Spreadsheet(self.0.clone(), self.1.clone()) } }
    // The overlay is the outer container; as_ref/Widget must return its handle
    // so that adding the spreadsheet to a parent container adds the overlay
    // (which wraps the canvas), not the canvas itself.
    impl AsRef<*mut c_void> for Spreadsheet { fn as_ref(&self) -> &*mut c_void { self.1.as_ref() } }
    impl Widget for Spreadsheet { fn raw_handle(&self) -> *mut c_void { *self.1.as_ref() } }

    impl Spreadsheet {
        /// Sets the text content of a cell. User manages data via callbacks.
        pub fn set_cell(&self, _row: usize, _col: usize, _text: &str) { /* user manages data via callbacks */ }
        /// Gets the text content of a cell. Returns None as user manages data via callbacks.
        pub fn get_cell(&self, _row: usize, _col: usize) -> Option<String> { None }
        /// Queues the canvas for a redraw.
        pub fn queue_redraw(&self) { self.0.queue_redraw(); }

        /// Sets a callback for drawing the spreadsheet content.
        pub fn set_draw_callback(&self, cb: Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>) {
            self.0.set_draw_callback(cb);
        }

        /// Sets a callback for handling keyboard input.
        pub fn on_key(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
            self.0.on_key(cb);
        }

        /// Sets a callback for handling mouse click events.
        pub fn on_click(&self, cb: Box<dyn FnMut(f64, f64)>) {
            self.0.on_click(cb);
        }

        /// Sets whether the spreadsheet should expand horizontally.
        pub fn set_hexpand(&self, expand: bool) { self.1.set_hexpand(expand); }
        /// Sets whether the spreadsheet should expand vertically.
        pub fn set_vexpand(&self, expand: bool) { self.1.set_vexpand(expand); }

        /// Returns a reference to the underlying canvas widget.
        pub fn canvas(&self) -> &Canvas {
            &self.0
        }

        /// Returns a reference to the overlay widget.
        pub fn overlay(&self) -> &Overlay {
            &self.1
        }
    }

    /// Creates a new spreadsheet widget with the specified number of rows and columns.
    pub fn create_spreadsheet(rows: usize, cols: usize) -> Result<Spreadsheet, Error> {
        let canvas = create_canvas()?;
        let overlay = create_overlay()?;
        let cw = 150i32; let ch = 28i32; let chw = 46i32;
        let total_w = chw + cols as i32 * cw;
        let total_h = ch + rows as i32 * ch;
        canvas.set_size_request(total_w, total_h);
        canvas.set_content_size(total_w, total_h);
        overlay.set_child(&canvas);
        Ok(Spreadsheet(canvas, overlay))
    }

    /// Quits the GTK main event loop.
    pub fn quit_main_loop() -> Result<(), Error> {
        crate::backends::gtk::quit_main_loop().map_err(|e| Error::Backend(format!("{}", e)))
    }

    /// Pump the GTK main context for `count` iterations.
    /// All iterations use blocking waits so poll() returns as soon as
    /// the X11 server or frame clock timer fires.  This ensures frame
    /// clock ticks are actually waited for rather than skipped.
    ///
    /// On a 60fps display each blocking iteration may take up to ~16ms
    /// (the frame clock interval).  500 blocking iterations = ~8s max.
    /// Callers should use a reasonable count (e.g. 500) to cover slow
    /// virtual displays (WSLg, Xvfb) without excessive delay.
    pub fn pump_main_context(count: usize) {
        if let Some(loader) = crate::backends::gtk::loader() {
            if let Some(glib_lib) = loader.libs.get("libglib") {
                type Iteration = unsafe extern "C" fn(*mut std::ffi::c_void, i32) -> i32;
                if let Ok(iter_fn) = unsafe { glib_lib.get::<Iteration>(b"g_main_context_iteration") } {
                    let iter = *iter_fn;
                    unsafe {
                        // All blocking iterations: on virtual displays (WSLg, Xvfb)
                        // the frame clock timer only fires during blocking waits.
                        // Non-blocking iterations return immediately and skip timer
                        // sources, so the draw callback never fires.  500 blocking
                        // iterations = ~8s max wait at 16ms/tick, which covers even
                        // the slowest virtual compositors.
                        for _ in 0..count {
                            iter(std::ptr::null_mut(), 1);
                        }
                    }
                }
            }
        }
    }

    #[cfg(test)]
    mod key_release_filter_tests {
        use super::*;

        /// The GTK4 release filter must exist as a callable helper.
        ///
        /// `gui_backend.rs` no longer carries any press/release dedup state: it
        /// relies entirely on this filter consuming GTK4's `key-released`
        /// signal. If the helper is removed, hosts silently regress to seeing
        /// every key twice — invisible on GTK3/nwg/wasm, visible only on
        /// GTK4/WSLg.
        #[test]
        fn release_filter_helper_is_present() {
            let f: fn(&gtk_dynamic_loader::EventControllerKey) = swallow_key_releases_gtk4;
            let _ = f;
        }

        /// Every GTK4 key registration must be paired with the release filter.
        ///
        /// Counting call sites is crude, but it is exactly the invariant that
        /// matters: N `connect_key_pressed` sites need N filters, or the
        /// unfiltered one double-fires. A source-level assertion keeps this
        /// honest without needing a GTK4 display.
        #[test]
        fn key_registrations_install_the_release_filter() {
            let src = include_str!("backends_gtk_adapter_impl.rs");
            let pressed = src.matches("connect_key_pressed(Box::new").count();
            let filtered = src.matches("swallow_key_releases_gtk4(&ctrl)").count();
            assert_eq!(
                pressed, filtered,
                "every connect_key_pressed needs a swallow_key_releases_gtk4 \
                 (pressed={pressed}, filtered={filtered})"
            );
            assert!(pressed >= 3, "expected window/entry/canvas sites, found {pressed}");
        }
    }

}


#[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
pub use gtk_adapter::*;

#[cfg(any(feature = "gtk4-rs", all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs"))))]
pub use gtk_dynamic_loader::Orientation;

