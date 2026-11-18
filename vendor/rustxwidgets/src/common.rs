//! Common type definitions shared across all backends.

// Platform-specific type re-exports using cfg
macro_rules! platform_module {
    ($backend:path, $Orientation:ident) => {
        pub use $backend::{Window, BoxWidget, Label, Entry, Canvas, Menu, SimpleAction, MenuBar, Dialog, DropDown, CheckButton, RadioButton, TextView, ScrolledWindow, Overlay, Orientation as $Orientation};
        pub type PlatformWindow = Window;
        pub type PlatformWidgetBox = BoxWidget;
        pub type PlatformLabel = Label;
        pub type PlatformEntry = Entry;
        pub type PlatformCanvas = Canvas;
        pub type PlatformMenu = Menu;
        pub type PlatformSimpleAction = SimpleAction;
        pub type PlatformMenuBar = MenuBar;
        pub type PlatformDialog = Dialog;
        pub type PlatformScrolledWindow = ScrolledWindow;
        #[allow(dead_code)] // not referenced by common_types_mod! on every backend
        pub type PlatformDropDown = DropDown;
        #[allow(dead_code)] // ditto: the mobile backends keep the overlay in the model
        pub type PlatformOverlay = Overlay;
        #[allow(dead_code)]
        pub type PlatformCheckButton = CheckButton;
        #[allow(dead_code)]
        pub type PlatformRadioButton = RadioButton;
        #[allow(dead_code)]
        pub type PlatformTextView = TextView;
    };
}

#[cfg(all(feature = "gtk4-rs", target_os = "linux", not(feature = "zork")))]
mod platform {
    platform_module!(crate::backends_gtk_adapter, GtkOrientation);
}

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs")))]
mod platform {
    platform_module!(crate::backends_gtk_adapter, GtkOrientation);
}

#[cfg(all(windows, not(feature = "zork")))]
mod platform {
    platform_module!(crate::backends_nwg_adapter, NwgOrientation);
}


#[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android", target_os = "ios"))))]
mod platform {
    platform_module!(crate::backends_pancurses_adapter, PancursesOrientation);
}

#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
mod platform {
    platform_module!(crate::backends_wasm_adapter, WasmOrientation);
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
mod platform {
    platform_module!(crate::backends_android_adapter, AndroidOrientation);
}

#[cfg(all(target_os = "ios", not(feature = "zork")))]
mod platform {
    platform_module!(crate::backends_ios_adapter, IosOrientation);
}

#[cfg(all(target_os = "macos", not(feature = "zork")))]
mod platform {
    platform_module!(crate::backends_macos_adapter, MacosOrientation);
}

#[cfg(feature = "zork")]
mod platform {
    platform_module!(crate::backends_zork_adapter, ZorkOrientation);
}

macro_rules! common_types_mod {
    () => {
        use super::platform::*;
        // `raw_handle` is a `core::Widget` trait method, and every platform
        // type implements it. Without the trait in scope it does not resolve
        // on the backends whose adapter has no inherent `raw_handle`, which is
        // what made the wasm32 build fail on this line alone. `allow` is
        // needed because the wasm adapter *does* have an inherent one, which
        // makes the import unused there.
        #[allow(unused_imports)]
        use crate::core::Widget;

        #[derive(Clone)]
        pub struct Window { pub inner: PlatformWindow }
        #[derive(Clone)]
        pub struct WidgetBox { pub inner: PlatformWidgetBox }
        #[derive(Clone)]
        pub struct Label { pub inner: PlatformLabel }
        #[derive(Clone)]
        pub struct Entry {
            pub inner: PlatformEntry,
            /// Tracked caret position for backends where `get_position`
            /// returns `None` (WASM, iOS, macOS, Android, zork). Updated by
            /// `set_position`; used as fallback by `get_position`. Backends
            /// with a real caret (GTK, NWG, pancurses) never consult it.
            caret_override: std::rc::Rc<std::cell::Cell<Option<usize>>>,
            /// When `true`, the `connect_changed` callback is suppressed.
            /// Set by `set_text_suppressing_changed` so programmatic
            /// `set_text` calls don't re-enter the edit logic.
            suppress_changed: std::rc::Rc<std::cell::Cell<bool>>,
        }
        #[derive(Clone)]
        pub struct Canvas { pub inner: PlatformCanvas }
        #[derive(Clone)]
        pub struct Menu { pub inner: PlatformMenu }
        #[derive(Clone)]
        pub struct SimpleAction { pub inner: PlatformSimpleAction }
        #[derive(Clone)]
        pub struct MenuBar { pub inner: PlatformMenuBar }
        #[derive(Clone)]
        pub struct Dialog { pub inner: PlatformDialog }
        #[derive(Clone)]
        pub struct ScrolledWindow { pub inner: PlatformScrolledWindow }

        impl Window {
            pub fn set_title(&self, title: &str) { self.inner.set_title(title); }
            pub fn set_default_size(&self, w: i32, h: i32) { self.inner.set_default_size(w, h); }
            /// Immediate resize. `set_default_size` is advisory under GTK and
            /// is not applied at all when no window manager is running (bare
            /// Xvfb, an offscreen CI runner), leaving the window 1x1 and the UI
            /// clipped to nothing. This forces the size.
            pub fn resize(&self, w: i32, h: i32) { self.inner.resize(w, h); }
            pub fn present(&self) { self.inner.present(); }
            pub fn insert_action_group(&self, name: &str, group_ptr: *mut std::os::raw::c_void) { #[allow(unused_unsafe)] unsafe { self.inner.insert_action_group(name, group_ptr); } }
            pub fn hwnd(&self) -> *mut std::os::raw::c_void { self.inner.hwnd() }
            pub fn set_child_box(&self, bx: &WidgetBox) { self.inner.set_child_box(&bx.inner); }
        }
        impl WidgetBox {
            pub fn append(&self, child: &impl AsRef<*mut std::os::raw::c_void>) { self.inner.append(child); }
            /// Minimum size of the box itself.
            ///
            /// Needed for a horizontal row: without an explicit height a Button
            /// child can be laid out taller than the strip, and consecutive rows
            /// overlap and render as garbled text.
            pub fn set_size_request(&self, w: i32, h: i32) { self.inner.set_size_request(w, h); }
            pub fn set_child_hexpand(&self, child: &impl AsRef<*mut std::os::raw::c_void>, expand: bool) { self.inner.set_child_hexpand(child, expand); }
            pub fn set_child_vexpand(&self, child: &impl AsRef<*mut std::os::raw::c_void>, expand: bool) { self.inner.set_child_vexpand(child, expand); }
            pub fn set_hexpand(&self, expand: bool) { self.inner.set_hexpand(expand); }
        }
        impl AsRef<*mut std::os::raw::c_void> for WidgetBox {
            fn as_ref(&self) -> &*mut std::os::raw::c_void { self.inner.as_ref() }
        }
        impl Label {
            pub fn set_text(&self, text: &str) { self.inner.set_text(text); }
            pub fn get_text(&self) -> Option<String> { self.inner.get_text() }
            pub fn set_visible(&self, visible: bool) { self.inner.set_visible(visible); }
            pub fn set_markup(&self, markup: &str) { self.inner.set_markup(markup); }
            /// Pin the label's width so changing its text never reflows the
            /// siblings packed after it. `None` releases the pin.
            pub fn set_fixed_width(&self, w: Option<i32>) { self.inner.set_fixed_width(w); }
            /// Left margin of the label's contents, in device px. Pairs with
            /// `set_fixed_width`: a pinned, left-aligned label sits flush
            /// against its slot's edge, and this restores the inset.
            pub fn set_margin_start(&self, px: i32) { self.inner.set_margin_start(px); }
            /// Vertical outer spacing. Pairs with `set_margin_start`: without
            /// it a caller could inset a label left and right but not top and
            /// bottom, which is not enough to space a toolbar from the content
            /// below it.
            pub fn set_margin_top(&self, px: i32) { self.inner.set_margin_top(px); }
            /// Set the x alignment of the label's text (0.0 left .. 1.0 right).
            pub fn set_xalign(&self, x: f32) { self.inner.set_xalign(x); }
            pub fn raw_handle(&self) -> *mut std::os::raw::c_void { self.inner.raw_handle() }
        }
        impl AsRef<*mut std::os::raw::c_void> for Label {
            fn as_ref(&self) -> &*mut std::os::raw::c_void { self.inner.as_ref() }
        }
        impl Entry {
            /// Wrap a platform-specific Entry with tracked caret and
            /// suppress-changed state.
            pub fn new(inner: PlatformEntry) -> Self {
                Entry {
                    inner,
                    caret_override: std::rc::Rc::new(std::cell::Cell::new(None)),
                    suppress_changed: std::rc::Rc::new(std::cell::Cell::new(false)),
                }
            }
            pub fn set_text(&self, text: &str) { self.inner.set_text(text); }
            pub fn get_text(&self) -> Option<String> { self.inner.get_text() }
            /// Caret position as a character index. Falls back to the
            /// internally tracked position when the backend cannot report one
            /// natively (WASM, iOS, macOS, Android, zork — see their adapters).
            /// GTK, NWG and pancurses report a real caret, so the fallback is
            /// not used on those.
            pub fn get_position(&self) -> Option<usize> {
                self.inner.get_position().or_else(|| self.caret_override.get())
            }
            /// Move the caret (character index). Also updates the internal
            /// tracked position so backends without native caret support
            /// stay consistent.
            pub fn set_position(&self, pos: usize) {
                self.caret_override.set(Some(pos));
                self.inner.set_position(pos);
            }
            /// Set the entry text while preserving the current caret
            /// position. The native `set_text` may reset the caret to
            /// the end on some backends; this method saves and restores it.
            pub fn set_text_preserving_caret(&self, text: &str) {
                let pos = self.get_position();
                self.inner.set_text(text);
                if let Some(p) = pos {
                    // Restore after set_text; clamp to new length.
                    let len = text.chars().count();
                    let clamped = p.min(len);
                    self.caret_override.set(Some(clamped));
                    self.inner.set_position(clamped);
                }
            }
            /// Set the entry text without firing the `connect_changed`
            /// callback. Use this for programmatic updates (preset edit
            /// buffers, movie replay, Insert Date/Time) where the app
            /// already knows the new value and the callback would cause
            /// re-entrant edits.
            pub fn set_text_suppressing_changed(&self, text: &str) {
                self.suppress_changed.set(true);
                self.inner.set_text(text);
                self.suppress_changed.set(false);
            }
            /// Whether the suppress_changed flag is currently set.
            pub fn is_suppressing_changed(&self) -> bool {
                self.suppress_changed.get()
            }
            pub fn grab_focus(&self) { self.inner.grab_focus(); }
            pub fn set_hexpand(&self, expand: bool) { self.inner.set_hexpand(expand); }
            pub fn set_vexpand(&self, expand: bool) { self.inner.set_vexpand(expand); }
            pub fn set_size_request(&self, w: i32, h: i32) { self.inner.set_size_request(w, h); }
            pub fn set_visible(&self, v: bool) { self.inner.set_visible(v); }
            /// Register a change callback. The callback is suppressed when
            /// `set_text_suppressing_changed` is active, so programmatic
            /// text updates don't re-enter edit logic.
            pub fn connect_changed(&self, mut f: impl FnMut() + 'static) -> Result<u64, crate::Error> {
                let suppress = self.suppress_changed.clone();
                self.inner.connect_changed(move || {
                    if !suppress.get() {
                        f();
                    }
                })
            }
            pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) { self.inner.on_key_raw(cb); }
            pub fn add_class(&self, class_name: &str) { self.inner.add_class(class_name); }
            pub fn remove_class(&self, class_name: &str) { self.inner.remove_class(class_name); }
            pub fn set_halign(&self, align: i32) { self.inner.set_halign(align); }
            pub fn set_valign(&self, align: i32) { self.inner.set_valign(align); }
            pub fn set_width_chars(&self, n: i32) { self.inner.set_width_chars(n); }
            pub fn connect_activate<F: FnMut(*mut std::os::raw::c_void) + 'static>(&self, f: F) -> Result<u64, crate::Error> { self.inner.connect_activate(f) }
            /// Whether the entry currently holds keyboard focus. Backends
            /// without a focus query report false, so callers there keep
            /// today's push behavior unchanged.
            pub fn has_focus(&self) -> bool { self.inner.has_focus() }
            /// Fires on pointer press (click) into the entry. Generic
            /// mechanism; apps use it for click-to-edit flows. Backends
            /// without pointer support on entries accept the callback and
            /// never fire it (behavior there is unchanged).
            pub fn connect_button_press(&self, f: impl FnMut() + 'static) -> Result<u64, crate::Error> { self.inner.connect_button_press(f) }
            pub fn connect_focus_in_event<F: FnMut(*mut std::os::raw::c_void) -> i32 + 'static>(&self, f: F) -> Result<u64, crate::Error> { self.inner.connect_focus_in_event(f) }
            pub fn connect_focus_out_event<F: FnMut(*mut std::os::raw::c_void) -> i32 + 'static>(&self, f: F) -> Result<u64, crate::Error> { self.inner.connect_focus_out_event(f) }
            pub fn set_margin_start(&self, px: i32) { self.inner.set_margin_start(px); }
            pub fn set_margin_top(&self, px: i32) { self.inner.set_margin_top(px); }
        }
        impl AsRef<*mut std::os::raw::c_void> for Entry {
            fn as_ref(&self) -> &*mut std::os::raw::c_void { self.inner.as_ref() }
        }
        impl Canvas {
            pub fn set_draw_callback(&self, cb: Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>) { self.inner.set_draw_callback(cb); }
            pub fn queue_redraw(&self) { self.inner.queue_redraw(); }
            pub fn set_size_request(&self, w: i32, h: i32) { self.inner.set_size_request(w, h); }
            pub fn on_click(&self, cb: Box<dyn FnMut(f64, f64)>) { self.inner.on_click(cb); }
            /// Forward the button/modifier-aware click through to the wrapped
            /// canvas, so a caller can use the richer API without knowing
            /// whether it holds a plain canvas or this wrapper.
            pub fn on_click_button(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) { self.inner.on_click_button(cb); }
            /// This canvas's top-left in screen coordinates, or `None` when
            /// unknown. Used to place the sheet-tab context menu at the
            /// pointer; see `MenuBar::popup_submenu_by_mnemonic_at`.
            pub fn screen_origin(&self) -> Option<(i32, i32)> { self.inner.screen_origin() }
            pub fn on_motion(&self, cb: Box<dyn FnMut(f64, f64, u32)>) { self.inner.on_motion(cb); }
            pub fn on_release(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) { self.inner.on_release(cb); }
            pub fn set_visible(&self, v: bool) { self.inner.set_visible(v); }
            pub fn set_content_size(&self, w: i32, h: i32) { self.inner.set_content_size(w, h); }
            /// The size this canvas is drawn at, in the backend's drawing
            /// pixels, measured now.
            ///
            /// GTK reports the allocation its container made; the wasm adapter
            /// measures the element's CSS box. Only a caller that can be
            /// resized *without* an app event needs this -- a browser can be,
            /// and hit-testing with a stale size is a click in the wrong cell.
            pub fn measure(&self) -> (i32, i32) {
                #[cfg(target_arch = "wasm32")]
                {
                    return self.inner.measure();
                }
                // Other backends: the caller's own bookkeeping
                // (`GuiState::canvas_size`) is already current, because every
                // resize there arrives as an app event with a measured size.
                #[cfg(not(target_arch = "wasm32"))]
                {
                    (0, 0)
                }
            }
            pub fn grab_focus(&self) { self.inner.grab_focus(); }
            pub fn set_can_focus(&self, can: bool) { self.inner.set_can_focus(can); }
            pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) { self.inner.on_key_raw(cb); }
            /// Force an immediate draw by rendering directly to the window
            /// surface.  `fallback_w`/`fallback_h` are used when the platform
            /// surface reports zero dimensions (the display server hasn't
            /// configured the surface yet).  On non-GTK backends this is a no-op.
            pub fn force_draw(&self, window_ptr: *mut std::os::raw::c_void, fallback_w: i32, fallback_h: i32) { self.inner.force_draw(window_ptr, fallback_w, fallback_h); }
        }
        impl Window {
            pub fn on_event(&self, cb: Box<dyn FnMut(*mut std::os::raw::c_void) -> i32>) { self.inner.on_event(cb); }
            pub fn on_event_key(&self, cb: Box<dyn FnMut(u32, u32) -> i32>) { self.inner.on_event_key(cb); }
            pub fn on_close(&self, cb: Box<dyn FnMut()>) { self.inner.on_close(cb); }
            pub fn queue_redraw(&self) { self.inner.queue_redraw(); }
        }
        impl AsRef<*mut std::os::raw::c_void> for Canvas {
            fn as_ref(&self) -> &*mut std::os::raw::c_void { self.inner.as_ref() }
        }
        impl Menu {
            pub fn append(&mut self, label: &str, detailed_action: &str) { self.inner.append(label, detailed_action); }
            pub fn append_submenu(&mut self, label: &str, submenu: &Menu) { self.inner.append_submenu(label, &submenu.inner); }
        }
        impl SimpleAction {
            pub fn connect_activate<F: FnMut(*mut std::os::raw::c_void) + 'static>(&self, f: F) -> Result<u64, crate::Error> { self.inner.connect_activate(f) }
        }
        impl MenuBar {
            pub fn activate_submenu_by_mnemonic(&self, keyval: u32) -> bool {
                self.inner.activate_submenu_by_mnemonic(keyval)
            }
            /// Open the submenu whose mnemonic is `keyval` at a screen position
            /// (the sheet-tab context menu). Unpositioned on backends without a
            /// position hook.
            ///
            /// **Prefer [`Self::popup_at`]**: this form makes the caller
            /// assemble screen coordinates itself, which means owning the
            /// widget's on-screen origin, rounding, and whatever vertical
            /// offset the menu's corner needs. It also forces the caller to
            /// invent a mnemonic for a menu it wants to open by name.
            pub fn popup_submenu_by_mnemonic_at(&self, keyval: u32, screen_x: i32, screen_y: i32) -> bool {
                self.inner.popup_submenu_by_mnemonic_at(keyval, screen_x, screen_y)
            }
            /// Pop up the top-level menu labelled `label`, anchored at
            /// (`x`, `y`) in `anchor`'s own coordinates -- typically the
            /// widget the user right-clicked.
            ///
            /// The top-left corner lands exactly on that point, unless that
            /// would put part of the menu off screen or under the taskbar, in
            /// which case it flips to the other side of that edge. Both the
            /// measuring and the flipping are handled here, so a caller never
            /// has to know where its widget sits on screen.
            ///
            /// `label` is the top-level entry's label in the menu model,
            /// including its mnemonic marker (`"_Sheet"` for a GTK-style menu
            /// that the backend has already translated, or the literal text if
            /// the model carries no marker).
            ///
            /// Returns false when the backend cannot pop up a menu at all, or
            /// when no top-level entry matches `label`. A false return is not a
            /// failure to place the menu -- only a refusal to open one -- so a
            /// caller should report it as "no context menu available" rather
            /// than retrying at another position.
            pub fn popup_at(&self, label: &str, anchor: &Canvas, x: f64, y: f64) -> bool {
                // `anchor` is this module's wrapper type; `self.inner` is the
                // platform `Canvas` underneath, which is what the backend takes.
                self.inner.popup_at(label, &anchor.inner, x, y)
            }
            pub fn activate_submenu_item_by_mnemonic(&self, keyval: u32) -> bool {
                self.inner.activate_submenu_item_by_mnemonic(keyval)
            }
            pub unsafe fn insert_action_group(&self, name: &str, group_ptr: *mut std::os::raw::c_void) {
                self.inner.insert_action_group(name, group_ptr);
            }
            /// Handle a mnemonic keypress when a submenu popover may be open.
            /// Returns true if the key was consumed (matched a menu item).
            /// Returns false if no match — caller should fall through to normal key handling.
            /// On NWG/WASM/Pancurses/Zork this is a no-op (returns false).
            pub fn handle_mnemonic_key(&self, keyval: u32) -> bool {
                self.inner.handle_mnemonic_key(keyval)
            }
            /// Handle any menu-related key event (Alt+letter to open submenu,
            /// Escape to close, printable to select, Up/Down to navigate).
            /// `modifiers` is a bitmask of GDK modifier state (1=Shift, 4=Control, 8=Alt/Meta).
            /// Returns true if the key was consumed by menu handling.
            /// Callers should also check `menu_active()` to prevent normal key handling.
            pub fn handle_menu_key(&self, keyval: u32, modifiers: u32) -> bool {
                self.inner.handle_menu_key(keyval, modifiers)
            }
            /// [`handle_menu_key`](Self::handle_menu_key) with the typed
            /// character supplied separately from the key code.
            ///
            /// A host whose key codes are not character values (the DOM
            /// reports the *physical* key, so on a QWERTZ layout the key
            /// marked `Z` arrives as keyCode 89) passes the character it
            /// decoded; a backend without such a distinction forwards the
            /// keyval, which is what it already does internally.
            pub fn handle_menu_key_char(&self, keyval: u32, ch: char, modifiers: u32) -> bool {
                self.inner.handle_menu_key_char(keyval, ch, modifiers)
            }
            /// Returns true if a keyboard menu is currently open (Alt+letter was pressed,
            /// menu is active for navigation).  Callers can use this to skip normal
            /// key handling when the menu is open.
            pub fn menu_active(&self) -> bool {
                self.inner.menu_active()
            }
            /// Close any open keyboard menu and dismiss visible popovers.
            pub fn menu_close(&self) {
                self.inner.menu_close();
            }
        }
        impl AsRef<*mut std::os::raw::c_void> for MenuBar {
            fn as_ref(&self) -> &*mut std::os::raw::c_void { self.inner.as_ref() }
        }
        impl Dialog {
            pub fn set_title(&self, title: &str) { self.inner.set_title(title); }
            pub fn set_default_size(&self, w: i32, h: i32) { self.inner.set_default_size(w, h); }
            /// Parent this dialog to `parent` so the WM places it as a child
            /// (centred on it) instead of an unparented window. No-op on
            /// backends without window parenting.
            pub fn set_transient_for(&self, parent: *mut std::os::raw::c_void) { self.inner.set_transient_for(parent); }
            pub fn append_content_area(&self, child: &impl AsRef<*mut std::os::raw::c_void>) { self.inner.append_content_area(child); }
            pub fn add_button(&self, text: &str, response_id: i32) { self.inner.add_button(text, response_id); }
            pub fn present(&self) { self.inner.present(); }
            /// Build the dialog's view hierarchy before showing it.
            ///
            /// A caller that attaches a custom view needs the dialog measured
            /// before it opens, or it opens at the height of nothing and the
            /// content appears a frame later. GTK and NWG both have a real
            /// `layout_dialog`; without this forward, a caller written for
            /// them could not compile against a backend that had one.
            pub fn layout_dialog(&self) { self.inner.layout_dialog(); }
            pub fn connect_response<F: FnMut(i32) + 'static>(&self, f: F) -> Result<u64, crate::Error> { self.inner.connect_response(f) }
            pub fn close(&self) { self.inner.close(); }
            /// Show or hide without destroying the dialog.
            pub fn set_visible(&self, visible: bool) { self.inner.set_visible(visible); }
            /// The container a dialog's children are added to, for a caller
            /// that needs to lay them out itself.
            pub fn get_content_area(&self) -> *mut std::os::raw::c_void { self.inner.get_content_area() }
            /// Run the dialog's nested loop and return the response id.
            /// See the Android adapter's note: there is no nested loop
            /// there, so this presents and returns the last response.
            pub fn run(&self) -> i32 { self.inner.run() }
        }
        impl AsRef<*mut std::os::raw::c_void> for Dialog {
            fn as_ref(&self) -> &*mut std::os::raw::c_void { self.inner.as_ref() }
        }
        impl AsRef<*mut std::os::raw::c_void> for ScrolledWindow {
            fn as_ref(&self) -> &*mut std::os::raw::c_void { self.inner.as_ref() }
        }
    }
}

// Common wrapper types with `inner` field for the platform-specific types
#[cfg(all(feature = "gtk4-rs", target_os = "linux", not(feature = "zork")))]
mod common_types {
    common_types_mod!();
    impl Canvas {
        pub fn on_key(&self, mut cb: Box<dyn FnMut(u32) -> bool>) {
            self.inner.on_key(Box::new(move |k: u32, _s: u32| -> bool { cb(k) }));
        }
    }
    impl ScrolledWindow {
        pub fn set_child(&self, child: &impl AsRef<*mut std::os::raw::c_void>) { self.inner.set_child(child); }
        pub fn set_policy(&self, hscroll: u32, vscroll: u32) { self.inner.set_policy(hscroll, vscroll); }
        pub fn set_vexpand(&self, expand: bool) { self.inner.set_vexpand(expand); }
        pub fn scroll_to(&self, hval: f64, hupper: f64, hpage: f64, vval: f64, vupper: f64, vpage: f64) {
            self.inner.scroll_to(hval, hupper, hpage, vval, vupper, vpage);
        }
        pub fn on_scroll(&self, cb: Box<dyn FnMut(bool, f64)>) { self.inner.on_scroll(cb); }
    }
}

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs")))]
mod common_types {
    common_types_mod!();
    impl Canvas {
        pub fn on_key(&self, mut cb: Box<dyn FnMut(u32) -> bool>) {
            self.inner.on_key(Box::new(move |k: u32, _s: u32| -> bool { cb(k) }));
        }
    }
    impl ScrolledWindow {
        pub fn set_child(&self, child: &impl AsRef<*mut std::os::raw::c_void>) { self.inner.set_child(child); }
        pub fn set_policy(&self, hscroll: u32, vscroll: u32) { self.inner.set_policy(hscroll, vscroll); }
        pub fn set_vexpand(&self, expand: bool) { self.inner.set_vexpand(expand); }
        pub fn scroll_to(&self, hval: f64, hupper: f64, hpage: f64, vval: f64, vupper: f64, vpage: f64) {
            self.inner.scroll_to(hval, hupper, hpage, vval, vupper, vpage);
        }
        pub fn on_scroll(&self, cb: Box<dyn FnMut(bool, f64)>) { self.inner.on_scroll(cb); }
    }
}

#[cfg(all(windows, not(feature = "zork")))]
mod common_types {
    common_types_mod!();
    impl Canvas {
        pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) { self.inner.on_key(cb); }
    }
    impl Entry {
        pub fn on_key(&self, f: Box<dyn FnMut(u32) -> bool>) { self.inner.on_key(f); }
    }
    impl TextView {
        pub fn set_hexpand(&self, _expand: bool) {}
        pub fn set_vexpand(&self, _expand: bool) {}
        /// Append a line to the buffer.
        ///
        /// NWG has no incremental insert on its TextView, so this is
        /// read-modify-write. It is correct but O(document) per call, which is
        /// fine for the interactive fields this backend covers; a high-rate log
        /// pane should reimplement it on the native handle instead.
        pub fn append_text(&self, text: &str) {
            let mut buf = self.get_text().unwrap_or_default();
            buf.push_str(text);
            self.set_text(&buf);
        }
    }
    impl ScrolledWindow {
        pub fn set_child(&self, child: &impl AsRef<*mut std::os::raw::c_void>) { self.inner.set_child(child); }
        pub fn set_policy(&self, hscroll: u32, vscroll: u32) { self.inner.set_policy(hscroll, vscroll); }
        pub fn set_vexpand(&self, expand: bool) { self.inner.set_vexpand(expand); }
        pub fn scroll_to(&self, hval: f64, hupper: f64, hpage: f64, vval: f64, vupper: f64, vpage: f64) {
            self.inner.scroll_to(hval, hupper, hpage, vval, vupper, vpage);
        }
        pub fn on_scroll(&self, cb: Box<dyn FnMut(bool, f64)>) { self.inner.on_scroll(cb); }
    }
}

#[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android", target_os = "ios"))))]
mod common_types { common_types_mod!(); }


#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
mod common_types {
    common_types_mod!();
    impl Canvas {
        pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) { self.inner.on_key(cb); }
    }
    impl ScrolledWindow {
        // The shared GUI nests the canvas here, so this takes a `Canvas`
        // (like the ios/macos/android arms) rather than a raw handle: the
        // adapter's `set_child` is generic over `AsElement` and the wrapper
        // is what knows which element that is.
        pub fn set_child(&self, child: &crate::common::Canvas) { self.inner.set_child(&child.inner); }
        // Forwarded rather than stubbed: the adapter has real CSS-backed
        // implementations of all three (`overflow-x`/`overflow-y`,
        // `flex-grow`/`align-self`), so `sync_scrollbars`
        // (`gui_backend.rs:3053`) now actually reaches the DOM.
        pub fn set_policy(&self, hscroll: u32, vscroll: u32) { self.inner.set_policy(hscroll as i32, vscroll as i32); }
        pub fn set_vexpand(&self, expand: bool) { self.inner.set_vexpand(expand); }
        pub fn scroll_to(&self, hval: f64, hupper: f64, hpage: f64, vval: f64, vupper: f64, vpage: f64) {
            self.inner.scroll_to(hval, hupper, hpage, vval, vupper, vpage);
        }
        pub fn on_scroll(&self, cb: Box<dyn FnMut(bool, f64)>) { self.inner.on_scroll(cb); }
    }
}

#[cfg(all(target_os = "android", not(feature = "zork")))]
mod common_types {
    common_types_mod!();
    impl Canvas {
        pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) { self.inner.on_key(cb); }
    }
    impl ScrolledWindow {
        pub fn set_child(&self, child: &crate::common::Canvas) { self.inner.attach_canvas(&child.inner); }
        pub fn set_policy(&self, _hscroll: u32, _vscroll: u32) {}
        pub fn set_vexpand(&self, _expand: bool) {}
        pub fn scroll_to(&self, _hval: f64, _hupper: f64, _hpage: f64, _vval: f64, _vupper: f64, _vpage: f64) {}
        pub fn on_scroll(&self, _cb: Box<dyn FnMut(bool, f64)>) {}
    }
}

#[cfg(all(target_os = "ios", not(feature = "zork")))]
mod common_types {
    common_types_mod!();
    impl Canvas {
        pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) { self.inner.on_key(cb); }
    }
    impl ScrolledWindow {
        pub fn set_child(&self, child: &crate::common::Canvas) { self.inner.attach_canvas(&child.inner); }
        pub fn set_policy(&self, _hscroll: u32, _vscroll: u32) {}
        pub fn set_vexpand(&self, _expand: bool) {}
        pub fn scroll_to(&self, _hval: f64, _hupper: f64, _hpage: f64, _vval: f64, _vupper: f64, _vpage: f64) {}
        pub fn on_scroll(&self, _cb: Box<dyn FnMut(bool, f64)>) {}
    }
}

#[cfg(all(target_os = "macos", not(feature = "zork")))]
mod common_types {
    common_types_mod!();
    impl Canvas {
        pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) { self.inner.on_key(cb); }
    }
    impl ScrolledWindow {
        pub fn set_child(&self, child: &crate::common::Canvas) { self.inner.attach_canvas(&child.inner); }
        pub fn set_policy(&self, _hscroll: u32, _vscroll: u32) {}
        pub fn set_vexpand(&self, _expand: bool) {}
        pub fn scroll_to(&self, _hval: f64, _hupper: f64, _hpage: f64, _vval: f64, _vupper: f64, _vpage: f64) {}
        pub fn on_scroll(&self, _cb: Box<dyn FnMut(bool, f64)>) {}
    }
}

#[cfg(feature = "zork")]
mod common_types {
    common_types_mod!();
    // The zork backend's ScrolledWindow is a real node, so these forward to
    // the model rather than being no-ops. They were absent here, which made
    // `common::ScrolledWindow` unusable for the very code that needs it —
    // `src/gui/gui_backend.rs` calls all of them.
    impl Canvas {
        pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) { self.inner.on_key(cb); }
        /// The zork model node id of this canvas. See [`Window::model_id`].
        pub fn model_id(&self) -> usize { self.inner.id() }
    }
    impl Window {
        /// The zork model node id of this window.
        ///
        /// The zork backend identifies a widget by a `usize` node id rather
        /// than a native handle, so an out-of-crate driver (corro's zork
        /// handoff, tests) needs a way to reach the model from a wrapper.
        /// GTK/NWG/macOS address widgets by real handles and do not define
        /// this, which is why it is zork-only.
        pub fn model_id(&self) -> usize { self.inner.id() }
    }
    impl ScrolledWindow {
        pub fn set_child(&self, child: &impl AsRef<*mut std::os::raw::c_void>) { self.inner.set_child(child); }
        pub fn set_policy(&self, hscroll: u32, vscroll: u32) { self.inner.set_policy(hscroll, vscroll); }
        pub fn set_vexpand(&self, expand: bool) { self.inner.set_vexpand(expand); }
        /// Drive the scroll position. The model clamps the value to
        /// `upper - page`, so a caller cannot scroll past the end.
        pub fn scroll_to(&self, hval: f64, hupper: f64, hpage: f64, vval: f64, vupper: f64, vpage: f64) {
            self.inner.scroll_to(hval, hupper, hpage, vval, vupper, vpage);
        }
        /// Register a scroll handler. A headless viewport produces no real
        /// scroll events, so this fires only when something explicitly drives
        /// one; it exists so shared code compiles and the handler is reachable.
        pub fn on_scroll(&self, cb: Box<dyn FnMut(bool, f64)>) { self.inner.on_scroll(cb); }
    }
    // Note: TextView has no `common::` wrapper (it is not in
    // `common_types_mod!`'s set), so it gets no impl block here. Callers that
    // use one go through the adapter types directly, as on every other backend.
}

#[cfg(all(feature = "gtk4-rs", target_os = "linux", not(feature = "zork")))]
pub use common_types::{Window, WidgetBox, Label, Entry, Canvas, Menu, SimpleAction, MenuBar, Dialog, ScrolledWindow};

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs")))]
pub use common_types::{Window, WidgetBox, Label, Entry, Canvas, Menu, SimpleAction, MenuBar, Dialog, ScrolledWindow};

#[cfg(all(windows, not(feature = "zork")))]
pub use common_types::{Window, WidgetBox, Label, Entry, Canvas, Menu, SimpleAction, MenuBar, Dialog, ScrolledWindow};


#[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
pub use common_types::{Window, WidgetBox, Label, Entry, Canvas, Menu, SimpleAction, MenuBar, Dialog, ScrolledWindow};

#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
pub use common_types::{Window, WidgetBox, Label, Entry, Canvas, Menu, SimpleAction, MenuBar, Dialog, ScrolledWindow};

#[cfg(all(target_os = "android", not(feature = "zork")))]
pub use common_types::{Window, WidgetBox, Label, Entry, Canvas, Menu, SimpleAction, MenuBar, Dialog, ScrolledWindow};

#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub use common_types::{Window, WidgetBox, Label, Entry, Canvas, Menu, SimpleAction, MenuBar, Dialog, ScrolledWindow};

#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub use common_types::{Window, WidgetBox, Label, Entry, Canvas, Menu, SimpleAction, MenuBar, Dialog, ScrolledWindow};

#[cfg(feature = "zork")]
pub use common_types::{Window, WidgetBox, Label, Entry, Canvas, Menu, SimpleAction, MenuBar, Dialog, ScrolledWindow};

// Re-export Orientation from the active platform backend
#[cfg(all(feature = "gtk4-rs", target_os = "linux", not(feature = "zork")))]
pub use self::platform::GtkOrientation as Orientation;
#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork"), not(feature = "gtk4-rs")))]
pub use self::platform::GtkOrientation as Orientation;
#[cfg(all(windows, not(feature = "zork")))]
pub use self::platform::NwgOrientation as Orientation;

#[cfg(all(feature = "pancurses", not(any(feature = "gtk", windows, target_arch = "wasm32", target_os = "android"))))]
pub use self::platform::PancursesOrientation as Orientation;
#[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
pub use self::platform::WasmOrientation as Orientation;
#[cfg(all(target_os = "android", not(feature = "zork")))]
pub use self::platform::AndroidOrientation as Orientation;
#[cfg(all(target_os = "ios", not(feature = "zork")))]
pub use self::platform::IosOrientation as Orientation;
#[cfg(all(target_os = "macos", not(feature = "zork")))]
pub use self::platform::MacosOrientation as Orientation;
#[cfg(feature = "zork")]
pub use self::platform::ZorkOrientation as Orientation;

// -- Shared menu definition types (no platform deps) --

/// A single menu item: label + action name + optional nested submenu.
#[derive(Clone, Copy)]
pub struct MenuItemDef {
    pub label: &'static str,
    pub action: &'static str,
    pub submenu: Option<&'static [MenuItemDef]>,
}

/// A submenu: label + prefix for action names + items.
#[derive(Clone, Copy)]
pub struct SubmenuDef {
    pub label: &'static str,
    pub prefix: &'static str,
    pub items: &'static [MenuItemDef],
}
