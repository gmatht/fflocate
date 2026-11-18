#[cfg(feature = "pancurses")]
mod pancurses_adapter {
    use std::os::raw::c_void;
    use crate::core::{Error, Widget};

    // -- Window --

    pub struct Window {
        pub(crate) id: usize,
    }

    impl Widget for Window {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for Window {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Clone for Window {
        fn clone(&self) -> Self { Window { id: self.id } }
    }

    impl Window {
        pub fn set_title(&self, title: &str) {
            crate::backends::pancurses::set_window_title(self.id, title);
        }

        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            let child_id = child_ptr as usize;
            crate::backends::pancurses::set_child(self.id, child_id);
        }

        pub fn set_child_box(&self, child: &BoxWidget) {
            self.set_child(child);
        }

        pub fn present(&self) {}

        pub fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}

        pub fn set_default_size(&self, _width: i32, _height: i32) {}

        /// No-op: a character terminal has no window to resize. The size
        /// comes from the tty (and from `CORRO_TERM_COLS`/`CORRO_TERM_ROWS`
        /// for a scripted run - see `core::terminal_size_with_override`), so
        /// a `resize` here would have to be ignored anyway, and honouring it
        /// would mean the widget tree and the terminal disagreed about the
        /// surface being drawn on.
        pub fn resize(&self, _w: i32, _h: i32) {}

        pub fn hwnd(&self) -> *mut c_void {
            std::ptr::null_mut()
        }
        pub fn on_event(&self, _cb: Box<dyn FnMut(*mut c_void) -> i32>) {}
        pub fn on_event_key(&self, _cb: Box<dyn FnMut(u32, u32) -> i32>) {}
        pub fn on_close(&self, _cb: Box<dyn FnMut()>) {}
        pub fn queue_redraw(&self) {}
    }

    // -- Button --

    pub struct Button {
        pub(crate) id: usize,
    }

    impl Widget for Button {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for Button {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Clone for Button {
        fn clone(&self) -> Self { Button { id: self.id } }
    }

    impl Button {
        pub fn on_click(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            crate::backends::pancurses::add_callback(self.id, Box::new(f));
            Ok(0)
        }

        pub fn emit_clicked(&self) -> Result<u64, Error> {
            // fire synchronously
            Ok(0)
        }

        pub fn set_font_style(&self, w: i32, it: bool) {
            crate::backends::pancurses::set_button_font_style(self.id, w, it);
        }
        pub fn set_size_request(&self, _w: i32, _h: i32) {}
        pub fn add_class(&self, _class_name: &str) {}
        pub fn remove_class(&self, _class_name: &str) {}
    }

    // -- Label --

    pub struct Label {
        pub(crate) id: usize,
    }

    impl AsRef<*mut c_void> for Label {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Clone for Label {
        fn clone(&self) -> Self { Label { id: self.id } }
    }

    impl Label {
        pub fn set_text(&self, text: &str) {
            crate::backends::pancurses::set_label_text(self.id, text);
        }

        pub fn get_text(&self) -> Option<String> {
            crate::backends::pancurses::get_label_text(self.id)
        }

        pub fn add_class(&self, _class_name: &str) {}
        pub fn remove_class(&self, _class_name: &str) {}
        pub fn set_markup(&self, _markup: &str) {}
        pub fn set_visible(&self, visible: bool) {
            crate::backends::pancurses::set_label_visible(self.id, visible);
        }
        /// Pin the label's laid-out width so changing its text never shifts
        /// the widgets packed after it. `w` is in character cells (the unit
        /// the terminal layout works in); `None` restores shrink-to-fit.
        pub fn set_fixed_width(&self, w: Option<i32>) {
            crate::backends::pancurses::set_label_fixed_width(self.id, w);
        }
        pub fn set_xalign(&self, _x: f32) {}
        /// Left inset of the label's text. A no-op in the terminal: the box
        /// layout there is cell-based and `set_fixed_width` already holds the
        /// slot, so there is no sub-cell margin to speak of. Present so the
        /// cross-backend `Label` surface matches the GUI backends.
        pub fn set_margin_start(&self, _px: i32) {}
        /// No-op: a character cell has no inset. The terminal backends align
        /// their own columns instead, and the shared renderer's margins are
        /// expressed in whole characters.
        pub fn set_margin_top(&self, _px: i32) {}
        pub fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    // -- BoxWidget --

    pub struct BoxWidget {
        pub(crate) id: usize,
        pub(crate) orientation: Orientation,
        pub(crate) spacing: i32,
    }

    #[derive(Clone, Copy, PartialEq)]
    pub enum Orientation {
        Horizontal,
        Vertical,
    }

    impl AsRef<*mut c_void> for BoxWidget {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Clone for BoxWidget {
        fn clone(&self) -> Self { BoxWidget { id: self.id, orientation: self.orientation, spacing: self.spacing } }
    }

    impl BoxWidget {
        pub fn append(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            let child_id = child_ptr as usize;
            crate::backends::pancurses::append_child(self.id, child_id);
        }

        pub fn layout(&self, _x: i32, _y: i32, _w: i32, _h: i32) {
            crate::backends::pancurses::layout_box(self.id);
        }

        pub fn set_child_vexpand(&self, _child: &impl AsRef<*mut c_void>, _expand: bool) {}
        pub fn set_child_hexpand(&self, _child: &impl AsRef<*mut c_void>, _expand: bool) {}
        pub fn set_hexpand(&self, _expand: bool) {}

        /// See `backends::pancurses::set_size_request`: a real size on the
        /// node, not a discarded argument.
        pub fn set_size_request(&self, w: i32, h: i32) {
            crate::backends::pancurses::set_size_request(self.id, w, h);
        }

        pub fn set_vexpand(&self, _expand: bool) {}
    }

    // -- Grid --

    pub struct Grid {
        pub(crate) id: usize,
    }

    impl AsRef<*mut c_void> for Grid {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Grid {
        pub fn attach(&self, child: &impl AsRef<*mut c_void>, _left: i32, _top: i32, _width: i32, _height: i32) {
            let child_ptr = *child.as_ref();
            let child_id = child_ptr as usize;
            crate::backends::pancurses::append_child(self.id, child_id);
        }

        pub fn layout(&self) {
            crate::backends::pancurses::layout_grid(self.id);
        }
    }

    // -- Entry --

    pub struct Entry {
        pub(crate) id: usize,
    }

    impl AsRef<*mut c_void> for Entry {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Clone for Entry {
        fn clone(&self) -> Self { Entry { id: self.id } }
    }

    impl Entry {
        pub fn set_text(&self, text: &str) {
            crate::backends::pancurses::entry_set_text(self.id, text);
        }

        pub fn text(&self) -> Option<String> {
            crate::backends::pancurses::entry_text(self.id)
        }

        pub fn get_text(&self) -> Option<String> {
            self.text()
        }

        pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            crate::backends::pancurses::add_callback(self.id, Box::new(f));
            Ok(0)
        }

        pub fn set_hexpand(&self, _expand: bool) {}
        pub fn set_vexpand(&self, _expand: bool) {}
        pub fn set_halign(&self, _align: i32) {}
        pub fn set_valign(&self, _align: i32) {}
        pub fn set_visible(&self, _visible: bool) {}
        pub fn set_size_request(&self, _w: i32, _h: i32) {}
        pub fn set_width_chars(&self, _w: i32) {}
        pub fn set_margin_start(&self, _margin: i32) {}
        pub fn set_margin_top(&self, _margin: i32) {}
        pub fn add_class(&self, _class_name: &str) {}
        pub fn remove_class(&self, _class_name: &str) {}
        /// Focus this entry (sets the widget tree's single `focus_id`).
        pub fn grab_focus(&self) {
            crate::backends::pancurses::entry_grab_focus(self.id);
        }

        /// The caret's byte offset, which the widget maintains as the user
        /// types. Exposing it means a host no longer needs a parallel copy.
        pub fn get_position(&self) -> Option<usize> {
            crate::backends::pancurses::get_entry_cursor(self.id)
        }

        /// Move the caret (clamped to a char boundary by the backend).
        pub fn set_position(&self, pos: usize) {
            crate::backends::pancurses::set_entry_cursor(self.id, pos);
        }

        /// Whether this entry currently holds focus.
        pub fn has_focus(&self) -> bool {
            crate::backends::pancurses::entry_has_focus(self.id)
        }
        /// No pointer clicks on terminal entries: accept and never fire.
        pub fn connect_button_press(&self, _f: impl FnMut() + 'static) -> Result<u64, Error> {
            Ok(0)
        }
        pub fn on_key_raw(&self, _cb: Box<dyn FnMut(u32, u32) -> bool>) {}

        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(&self, f: F) -> Result<u64, Error> {
            let mut f = f;
            crate::backends::pancurses::add_callback(self.id, Box::new(move || f(std::ptr::null_mut())));
            Ok(0)
        }

        pub fn connect_focus_in_event<F: FnMut(*mut c_void) -> i32 + 'static>(&self, f: F) -> Result<u64, Error> {
            let mut f = f;
            crate::backends::pancurses::add_callback(self.id, Box::new(move || { f(std::ptr::null_mut()); }));
            Ok(0)
        }

        pub fn connect_focus_out_event<F: FnMut(*mut c_void) -> i32 + 'static>(&self, f: F) -> Result<u64, Error> {
            let mut f = f;
            crate::backends::pancurses::add_callback(self.id, Box::new(move || { f(std::ptr::null_mut()); }));
            Ok(0)
        }
    }

    // -- Menu --

    /// Backend-agnostic menu model: a list of items (actions or submenus).
    /// The same model is used by every rswidgets backend, so an application
    /// can build one menu and hand it to any backend's `create_menubar`.
    pub struct Menu {
        pub(crate) id: usize,
        pub(crate) items: std::cell::RefCell<Vec<crate::MenuItem>>,
    }

    impl AsRef<*mut c_void> for Menu {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Menu {
        pub fn append(&self, label: &str, action_name: &str) {
            self.items.borrow_mut().push(crate::MenuItem::Action {
                label: label.into(),
                action: action_name.to_string(),
                shortcut: None,
            });
        }
        pub fn append_with_shortcut(&self, label: &str, action_name: &str, shortcut: &str) {
            self.items.borrow_mut().push(crate::MenuItem::Action {
                label: label.into(),
                action: action_name.to_string(),
                shortcut: Some(shortcut.to_string()),
            });
        }
        pub fn append_separator(&self) {
            self.items.borrow_mut().push(crate::MenuItem::Separator);
        }
        pub fn append_check(&self, label: &str, action_name: &str, checked: bool) {
            self.items.borrow_mut().push(crate::MenuItem::Check {
                label: label.into(),
                action: action_name.to_string(),
                checked,
            });
        }
        pub fn append_radio(&self, label: &str, action_name: &str, group: u32) {
            self.items.borrow_mut().push(crate::MenuItem::Radio {
                label: label.into(),
                action: action_name.to_string(),
                group,
            });
        }
        pub fn append_submenu(&self, label: &str, submenu: &Menu) {
            self.append_submenu_with_shortcut(label, "", submenu);
        }
        pub fn append_submenu_with_shortcut(&self, label: &str, shortcut: &str, submenu: &Menu) {
            let sub_items = submenu.items.borrow().clone();
            self.items.borrow_mut().push(crate::MenuItem::Submenu {
                label: label.into(),
                items: sub_items,
                shortcut: if shortcut.is_empty() { None } else { Some(shortcut.to_string()) },
            });
        }
        /// Append an action whose label is a deferred translation key
        /// (resolved at render time via the installed translator).
        pub fn append_key(&self, key: &'static str, action_name: &str) {
            self.items.borrow_mut().push(crate::MenuItem::Action {
                label: crate::Label::Key(key),
                action: action_name.to_string(),
                shortcut: None,
            });
        }
        pub fn append_submenu_key(&self, key: &'static str, submenu: &Menu) {
            let sub_items = submenu.items.borrow().clone();
            self.items.borrow_mut().push(crate::MenuItem::Submenu {
                label: crate::Label::Key(key),
                items: sub_items,
                shortcut: None,
            });
        }
        pub fn append_item(&self, _label: &str, _action: &SimpleAction) {}
        pub fn append_section(&self, _label: &str) {}
    }

    /// Extract the menubar model: (root label, items) for each root menu.
    pub(crate) fn collect_menu_items(menu: &Menu) -> Vec<(crate::Label, Vec<crate::MenuItem>)> {
        menu.items
            .borrow()
            .iter()
            .filter_map(|item| match item {
                crate::MenuItem::Submenu { label, items, .. } => Some((label.clone(), items.clone())),
                _ => None,
            })
            .collect()
    }

    // -- MenuBar --

    pub struct MenuBar {
        pub(crate) id: usize,
    }

    impl Widget for MenuBar {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for MenuBar {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Clone for Menu {
        fn clone(&self) -> Self {
            Menu {
                id: self.id,
                items: std::cell::RefCell::new(self.items.borrow().clone()),
            }
        }
    }

    impl Clone for MenuBar {
        fn clone(&self) -> Self { MenuBar { id: self.id } }
    }

    impl Clone for SimpleAction {
        fn clone(&self) -> Self { SimpleAction { id: self.id } }
    }

    impl MenuBar {
        pub fn activate_submenu_by_mnemonic(&self, _keyval: u32) -> bool { false }

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
        }        pub fn activate_submenu_item_by_mnemonic(&self, _keyval: u32) -> bool { false }
        pub fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}
        pub fn handle_mnemonic_key(&self, _keyval: u32) -> bool { false }
        pub fn handle_menu_key(&self, _keyval: u32, _mod: u32) -> bool { false }
        /// [`handle_menu_key`](Self::handle_menu_key) with the typed character
        /// supplied separately; this backend's key codes are already characters.
        pub fn handle_menu_key_char(&self, keyval: u32, _ch: char, modifiers: u32) -> bool {
            self.handle_menu_key(keyval, modifiers)
        }
        pub fn menu_active(&self) -> bool { false }
        pub fn menu_close(&self) {}
    }

    // -- SimpleAction --

    pub struct SimpleAction {
        pub(crate) id: usize,
    }

    impl AsRef<*mut c_void> for SimpleAction {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl SimpleAction {
        pub fn on_activate(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            crate::backends::pancurses::add_callback(self.id, Box::new(f));
            Ok(0)
        }
        pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(&self, f: F) -> Result<u64, Error> {
            let mut f = f;
            crate::backends::pancurses::add_callback(self.id, Box::new(move || f(std::ptr::null_mut())));
            Ok(0)
        }
    }

    // -- Dialog --

    pub struct Dialog {
        pub(crate) id: usize,
    }

    impl Clone for Dialog {
        fn clone(&self) -> Self { Dialog { id: self.id } }
    }

    impl Widget for Dialog {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for Dialog {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Dialog {
        pub fn set_title(&self, title: &str) {
            crate::backends::pancurses::set_window_title(self.id, title);
        }
        pub fn set_default_size(&self, _w: i32, _h: i32) {}
        /// Terminal dialogs have no window parenting; no-op.
        pub fn set_transient_for(&self, _parent: *mut c_void) {}
        pub fn append_content_area(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            let child_id = child_ptr as usize;
            crate::backends::pancurses::set_child(self.id, child_id);
        }
        pub fn add_button(&self, _label: &str, _response_id: i32) {}
        pub fn connect_response(&self, _f: impl FnMut(i32) + 'static) -> Result<u64, Error> { Ok(0) }
        pub fn present(&self) {}
        pub fn close(&self) {}

        /// The dialog's content area. A terminal dialog has no separate
        /// content widget -- `append_content_area` hands its child straight to
        /// `set_child`, so the content *is* the first child. Returns null when
        /// nothing has been appended, which is the honest "no content area
        /// yet" rather than a dangling id.
        pub fn get_content_area(&self) -> *mut c_void {
            match crate::backends::pancurses::first_child(self.id) {
                Some(c) => c as *mut c_void,
                None => std::ptr::null_mut(),
            }
        }

        /// Lay out the dialog's content and button row.
        ///
        /// A no-op for the same reason `set_transient_for` above is: a
        /// terminal has no window to measure, and the shared GUI calls this to
        /// settle a size before showing. `set_default_size` is likewise inert
        /// here. Stated rather than silently missing so the call site compiles
        /// and the reason is on the record.
        pub fn layout_dialog(&self) {}

        /// Show or hide without destroying.
        ///
        /// Distinct from `close`, which is GTK's "destroy the window" and
        /// cannot be undone. See
        /// `backends::pancurses::set_visible` for what this actually changes.
        pub fn set_visible(&self, visible: bool) {
            crate::backends::pancurses::set_visible(self.id, visible);
        }

        /// The response id a dismiss reports when no button was chosen.
        ///
        /// The dialog code calls this to mark which button a plain dismissal
        /// (no button click) should answer as. GTK's equivalent is
        /// `GTK_RESPONSE_CANCEL`; recorded on the node so it is not simply
        /// discarded, which is what a bare no-op would mean for a value the
        /// caller has already computed.
        pub fn set_default_response(&self, response_id: i32) {
            crate::backends::pancurses::set_dialog_default_response(self.id, response_id);
        }

        /// Run the dialog's nested loop and return the response id.
        ///
        /// The terminal backend's `Dialog` is not a modal loop: `present` and
        /// `connect_response` are both inert, so there is no response to wait
        /// for. Returns 0, which is not a valid GTK response id (those start
        /// at 1), so a caller gets an unambiguous "none" rather than a
        /// plausible-looking wrong answer. Same as
        /// `backends_nwg_adapter.rs`.
        pub fn run(&self) -> i32 { 0 }
    }

    // -- DropDown --

    pub struct DropDown {
        pub(crate) id: usize,
    }

    impl Widget for DropDown {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for DropDown {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Clone for DropDown {
        fn clone(&self) -> Self { DropDown { id: self.id } }
    }

    impl DropDown {
        /// Terminal dropdowns take no GTK-style focus; no-op.
        pub fn grab_focus(&self) {}
        /// Terminal dropdowns are not pixel-positioned; no-op.
        pub fn set_offset(&self, _x: i32, _y: i32) {}
        /// Terminal dropdowns take no GTK-style flex expansion; no-op.
        ///
        /// The dialog code sets this on the in-dialog dropdown so it fills the
        /// dialog's width on GTK. A terminal dialog is drawn as text, so there
        /// is no width to expand into.
        pub fn set_hexpand(&self, _expand: bool) {}
        pub fn set_items(&self, items: &[&str]) {
            crate::backends::pancurses::set_dropdown_items(self.id, items);
        }
        /// `Option<u32>`, as on every other backend: `None` is "nothing
        /// selected", which this backend's own `set_dropdown_selected` already
        /// represents as a `None` on the node. The signature was `i32` here
        /// and nowhere else, so a caller passing `Some(i)` -- what GTK, NWG,
        /// zork and wasm all take -- did not compile against the terminal
        /// backend at all.
        pub fn set_active(&self, index: Option<u32>) {
            crate::backends::pancurses::set_dropdown_selected(
                self.id,
                index.map(|i| i as i32).unwrap_or(-1),
            );
        }
        pub fn get_active(&self) -> i32 {
            crate::backends::pancurses::get_dropdown_selected(self.id)
        }
        pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            crate::backends::pancurses::add_callback(self.id, Box::new(f));
            Ok(0)
        }
    }

    // -- CheckButton --

    pub struct CheckButton {
        pub(crate) id: usize,
    }

    impl Widget for CheckButton {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for CheckButton {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Clone for CheckButton {
        fn clone(&self) -> Self { CheckButton { id: self.id } }
    }

    impl CheckButton {
        pub fn set_active(&self, active: bool) {
            crate::backends::pancurses::set_checkbutton_checked(self.id, active);
        }

        pub fn is_active(&self) -> bool {
            crate::backends::pancurses::get_checkbutton_checked(self.id)
        }

        pub fn on_toggle(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            crate::backends::pancurses::add_callback(self.id, Box::new(f));
            Ok(0)
        }

        pub fn connect_toggled(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            self.on_toggle(f)
        }
    }

    // -- RadioButton --

    pub struct RadioButton {
        pub(crate) id: usize,
    }

    impl Widget for RadioButton {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    impl AsRef<*mut c_void> for RadioButton {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Clone for RadioButton {
        fn clone(&self) -> Self { RadioButton { id: self.id } }
    }

    impl RadioButton {
        pub fn set_active(&self, active: bool) {
            crate::backends::pancurses::set_radiobutton_checked(self.id, active);
        }

        pub fn is_active(&self) -> bool {
            crate::backends::pancurses::get_radiobutton_checked(self.id)
        }

        pub fn on_toggle(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            crate::backends::pancurses::add_callback(self.id, Box::new(f));
            Ok(0)
        }

        pub fn connect_toggled(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
            self.on_toggle(f)
        }
    }

    // -- TextView --

    pub struct TextView {
        pub(crate) id: usize,
    }

    impl AsRef<*mut c_void> for TextView {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Widget for TextView {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    impl TextView {
        pub fn set_text(&self, text: &str) {
            crate::backends::pancurses::set_textview_text(self.id, text);
        }
        pub fn get_text(&self) -> Option<String> {
            crate::backends::pancurses::get_textview_text(self.id)
        }
        pub fn set_wrap_mode(&self, _mode: i32) {}
        pub fn set_size_request(&self, _w: i32, _h: i32) {}
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

    // -- Spreadsheet --

    pub struct Spreadsheet {
        pub(crate) id: usize,
    }

    impl Clone for Spreadsheet {
        fn clone(&self) -> Self { Spreadsheet { id: self.id } }
    }

    impl AsRef<*mut c_void> for Spreadsheet {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Widget for Spreadsheet {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    // -- Canvas --

    pub struct Canvas {
        pub(crate) id: usize,
    }

    impl Clone for Canvas {
        fn clone(&self) -> Self { Canvas { id: self.id } }
    }

    impl AsRef<*mut c_void> for Canvas {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Widget for Canvas {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    impl Canvas {
        pub fn set_size_request(&self, _w: i32, _h: i32) {}
        pub fn set_content_size(&self, _w: i32, _h: i32) {}
        pub fn queue_redraw(&self) {}
        /// Register a draw callback — the same contract every GUI canvas
        /// implements. The callback gets a `DrawContext` backed by a terminal
        /// cell grid; the backend reduces that grid to ANSI and flushes it on
        /// each redraw, so a canvas-painted UI works on a character display.
        pub fn set_draw_callback(&self, cb: Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>) {
            crate::backends::pancurses::canvas_set_draw_callback(self.id, cb);
        }
        /// Register a plain click handler — the same contract the GTK/NWG
        /// canvases implement. The toolkit converts the terminal's cell hit
        /// into the pixel-space coordinates `DrawContext` uses, so a host's
        /// click math is backend-independent.
        pub fn on_click(&self, cb: Box<dyn FnMut(f64, f64)>) {
            crate::backends::pancurses::canvas_on_click(self.id, cb);
        }

        /// Button/modifier-aware click. See the GTK backend's
        /// `on_click_button`: kept alongside `on_click` so both can be
        /// registered (as on GTK, where they share one signal connection).
        /// `button` is 1-based; the terminal reports the primary button as 1.
        pub fn on_click_button(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            crate::backends::pancurses::canvas_on_click_button(self.id, cb);
        }

        /// Pointer motion over the canvas; see the GTK backend's `on_motion`.
        /// Terminals only report motion while a button is held (or as bare
        /// position reports); both arrive here, with `button` = 0 for the
        /// no-button case.
        pub fn on_motion(&self, cb: Box<dyn FnMut(f64, f64, u32)>) {
            crate::backends::pancurses::canvas_on_motion(self.id, cb);
        }

        /// This canvas's top-left in screen coordinates, or `None` when the
        /// backend cannot report one. Callers then open a context menu
        /// unpositioned rather than guessing. See the GTK backend's
        /// `screen_origin`.
        pub fn screen_origin(&self) -> Option<(i32, i32)> {
            None
        }

        /// Drop this canvas's draw callback.
        pub fn clear_draw_callback(&self) {
            crate::backends::pancurses::canvas_clear_draw_callback(self.id);
        }

        /// Pointer release; see the GTK backend's `on_release`.
        pub fn on_release(&self, _cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
            // No-op: terminal canvases have no buttons or pointer.
        }
        /// No-op: terminal canvases are virtual; visibility is meaningless.
        pub fn set_visible(&self, _v: bool) {}
        pub fn on_key(&self, _cb: Box<dyn FnMut(u32) -> bool>) {}
        pub fn on_key_raw(&self, _cb: Box<dyn FnMut(u32, u32) -> bool>) {}
        pub fn grab_focus(&self) {}
        pub fn set_can_focus(&self, _can: bool) {}
        pub fn force_draw(&self, _window_ptr: *mut c_void, _fallback_w: i32, _fallback_h: i32) {}
    }

    // -- Overlay --

    pub struct Overlay {
        pub(crate) id: usize,
    }

    impl Clone for Overlay {
        fn clone(&self) -> Self { Overlay { id: self.id } }
    }

    impl AsRef<*mut c_void> for Overlay {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Widget for Overlay {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    impl Overlay {
        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            let child_id = child_ptr as usize;
            crate::backends::pancurses::set_child(self.id, child_id);
        }
        pub fn add_overlay(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            let child_id = child_ptr as usize;
            crate::backends::pancurses::set_child(self.id, child_id);
        }
        pub fn set_overlay_pass_through(&self, _child: &impl AsRef<*mut c_void>, _pass: bool) {}
        pub fn remove(&self, _child: &impl AsRef<*mut c_void>) {}
        pub fn show_all(&self) {}
        pub fn set_size_request(&self, _w: i32, _h: i32) {}
    }

    // -- ScrolledWindow --

    #[derive(Clone)]
    pub struct ScrolledWindow {
        pub(crate) id: usize,
    }

    impl AsRef<*mut c_void> for ScrolledWindow {
        fn as_ref(&self) -> &*mut c_void {
            unsafe { &*(&self.id as *const usize as *const *mut c_void) }
        }
    }

    impl Widget for ScrolledWindow {
        fn raw_handle(&self) -> *mut c_void {
            &self.id as *const usize as *mut c_void
        }
    }

    impl ScrolledWindow {
        pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
            let child_ptr = *child.as_ref();
            let child_id = child_ptr as usize;
            crate::backends::pancurses::set_child(self.id, child_id);
        }
        pub fn set_policy(&self, _hscroll: u32, _vscroll: u32) {}
        pub fn set_vexpand(&self, _expand: bool) {}
        pub fn set_hexpand(&self, _expand: bool) {}
    }

    // ── Cell style constants (match cell_style_to_attrs in pancurses.rs) ──
    pub const CELL_STYLE_DEFAULT: u8 = 0;
    pub const CELL_STYLE_CURSOR: u8 = 1;
    pub const CELL_STYLE_AGGREGATE: u8 = 2;
    pub const CELL_STYLE_FOOTER_AGGREGATE: u8 = 3;
    pub const CELL_STYLE_SELECTED: u8 = 4;
    pub const CELL_STYLE_ACTIVE_HEADER: u8 = 5;
    pub const CELL_STYLE_INACTIVE_HEADER: u8 = 6;

    impl Spreadsheet {
        pub fn id(&self) -> usize { self.id }
        pub fn set_cell(&self, row: u32, col: u32, text: &str) {
            crate::backends::pancurses::spreadsheet_set_cell(self.id, row, col, text);
        }
        pub fn get_cell(&self, row: u32, col: u32) -> Option<String> {
            crate::backends::pancurses::spreadsheet_get_cell(self.id, row, col)
        }
        pub fn set_raw_cell(&self, row: u32, col: u32, text: &str) {
            crate::backends::pancurses::spreadsheet_set_raw_cell(self.id, row, col, text);
        }
        pub fn set_cell_style(&self, row: u32, col: u32, style: u8) {
            crate::backends::pancurses::spreadsheet_set_cell_style(self.id, row, col, style);
        }
        pub fn cursor_position(&self) -> Option<(u32, u32)> {
            crate::backends::pancurses::spreadsheet_cursor_position(self.id)
        }
        pub fn set_cursor(&self, row: u32, col: u32) {
            crate::backends::pancurses::spreadsheet_set_cursor(self.id, row, col);
        }
        pub fn set_editing(&self, editing: bool, edit_buf: &str, edit_pos: usize) {
            crate::backends::pancurses::spreadsheet_set_edit_state(self.id, editing, edit_buf, edit_pos);
        }
        /// Current edit state (generic mechanism for hosts that splice text
        /// into an in-progress edit, e.g. a picker commit).
        pub fn edit_state(&self) -> (bool, String, usize) {
            crate::backends::pancurses::spreadsheet_get_edit_state(self.id)
        }
        pub fn set_grid_config(&self, margin_cols: u32, main_cols: u32) {
            crate::backends::pancurses::spreadsheet_set_grid_config(self.id, margin_cols, main_cols);
        }
        pub fn set_row_counts(&self, header_rows: u32, main_rows: u32) {
            crate::backends::pancurses::spreadsheet_set_row_counts(self.id, header_rows, main_rows);
        }
        pub fn set_column_layout(&self, layout: Vec<(u32, u32, String)>) {
            crate::backends::pancurses::spreadsheet_set_column_layout(self.id, layout);
        }
        pub fn set_row_labels(&self, labels: Vec<(u32, String)>) {
            crate::backends::pancurses::spreadsheet_set_row_labels(self.id, labels);
        }
        pub fn set_menu_text(&self, text: &str) {
            crate::backends::pancurses::spreadsheet_set_menu_text(self.id, text);
        }
        pub fn set_border_title(&self, text: &str) {
            crate::backends::pancurses::spreadsheet_set_border_title(self.id, text);
        }
        pub fn set_status_text(&self, text: &str) {
            crate::backends::pancurses::spreadsheet_set_status_text(self.id, text);
        }
        pub fn set_formula_bar_trailing(&self, text: &str) {
            crate::backends::pancurses::spreadsheet_set_formula_bar_trailing(self.id, text);
        }
        pub fn set_tab_data(&self, titles: &[String], active: usize) {
            crate::backends::pancurses::spreadsheet_set_tab_data(self.id, titles, active);
        }
        pub fn set_formula_bar(&self, address_label: &Label, entry: &Entry) {
            crate::backends::pancurses::spreadsheet_set_formula_bar(
                self.id, address_label.id, entry.id,
            );
        }
        pub fn commit_formula_bar(&self) {
            crate::backends::pancurses::spreadsheet_commit_formula_bar(self.id);
        }
    }

    // -- Factory functions --

    pub fn create_window() -> Result<Window, Error> {
        crate::backends::pancurses::create_window()
            .map(|id| Window { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_button(label: &str) -> Result<Button, Error> {
        crate::backends::pancurses::create_button(label)
            .map(|id| Button { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_label(text: &str) -> Result<Label, Error> {
        crate::backends::pancurses::create_label(text)
            .map(|id| Label { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_box(orientation: Orientation, spacing: i32) -> Result<BoxWidget, Error> {
        let horizontal = orientation == Orientation::Horizontal;
        crate::backends::pancurses::create_box(horizontal, spacing)
            .map(|id| BoxWidget { id, orientation, spacing })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_grid() -> Result<Grid, Error> {
        crate::backends::pancurses::create_grid()
            .map(|id| Grid { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_entry() -> Result<Entry, Error> {
        crate::backends::pancurses::create_entry()
            .map(|id| Entry { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_menu() -> Result<Menu, Error> {
        crate::backends::pancurses::create_menu()
            .map(|id| Menu { id, items: std::cell::RefCell::new(vec![]) })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_menubar(model: &Menu, _action_group: *mut c_void) -> Result<MenuBar, Error> {
        let submenu_items = collect_menu_items(model);
        let id = unsafe { crate::backends::pancurses::create_menubar(submenu_items, _action_group) }
            .map_err(|e| Error::Backend(format!("{}", e)))?;
        Ok(MenuBar { id })
    }

    pub fn create_simple_action(name: &str) -> Result<SimpleAction, Error> {
        crate::backends::pancurses::create_simple_action(name)
            .map(|id| SimpleAction { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_dialog() -> Result<Dialog, Error> {
        crate::backends::pancurses::create_dialog()
            .map(|id| Dialog { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_dropdown(items: &[&str]) -> Result<DropDown, Error> {
        crate::backends::pancurses::create_dropdown(items)
            .map(|id| DropDown { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_checkbutton(label: &str) -> Result<CheckButton, Error> {
        crate::backends::pancurses::create_checkbutton(label)
            .map(|id| CheckButton { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_radiobutton(group: Option<&RadioButton>, label: &str) -> Result<RadioButton, Error> {
        let gid = group.map(|r| r.id);
        crate::backends::pancurses::create_radiobutton(gid, label)
            .map(|id| RadioButton { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_textview() -> Result<TextView, Error> {
        crate::backends::pancurses::create_textview()
            .map(|id| TextView { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_spreadsheet(rows: u32, cols: u32) -> Result<Spreadsheet, Error> {
        crate::backends::pancurses::create_spreadsheet(rows, cols)
            .map(|id| Spreadsheet { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_canvas() -> Result<Canvas, Error> {
        crate::backends::pancurses::create_canvas()
            .map(|id| Canvas { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_overlay() -> Result<Overlay, Error> {
        crate::backends::pancurses::create_overlay()
            .map(|id| Overlay { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn create_scrolled_window() -> Result<ScrolledWindow, Error> {
        crate::backends::pancurses::create_scrolled_window()
            .map(|id| ScrolledWindow { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }

    pub fn open_file(_title: &str) -> Result<Option<String>, Error> {
        Ok(None)
    }

    pub fn save_file(_title: &str) -> Result<Option<String>, Error> {
        Ok(None)
    }

    pub fn add_key_callback(key: char, cb: Box<dyn FnMut()>) {
        crate::backends::pancurses::add_key_callback(key, cb);
    }
    pub fn add_goto_callback<F: FnMut() + 'static>(f: F) {
        crate::backends::pancurses::spreadsheet_add_goto_callback(f);
    }

    pub fn add_cursor_move_callback<F: FnMut(u32, u32) + 'static>(f: F) {
        crate::backends::pancurses::spreadsheet_add_cursor_move_callback(f);
    }
    pub fn add_commit_edit_callback<F: FnMut(u32, u32, String) + 'static>(f: F) {
        crate::backends::pancurses::spreadsheet_add_commit_edit_callback(f);
    }
    pub fn spreadsheet_set_cell(id: usize, r: u32, c: u32, text: &str) {
        crate::backends::pancurses::spreadsheet_set_cell(id, r, c, text);
    }
    pub fn spreadsheet_set_cell_style(id: usize, r: u32, c: u32, style: u8) {
        crate::backends::pancurses::spreadsheet_set_cell_style(id, r, c, style);
    }
    pub fn spreadsheet_set_column_layout(id: usize, layout: Vec<(u32, u32, String)>) {
        crate::backends::pancurses::spreadsheet_set_column_layout(id, layout);
    }
    pub fn spreadsheet_set_border_title(id: usize, text: &str) {
        crate::backends::pancurses::spreadsheet_set_border_title(id, text);
    }
    pub fn spreadsheet_set_row_labels(id: usize, labels: Vec<(u32, String)>) {
        crate::backends::pancurses::spreadsheet_set_row_labels(id, labels);
    }
    pub fn spreadsheet_set_grid_config(id: usize, margin_cols: u32, main_cols: u32) {
        crate::backends::pancurses::spreadsheet_set_grid_config(id, margin_cols, main_cols);
    }
    pub fn spreadsheet_set_edit_state(id: usize, editing: bool, edit_buf: &str, edit_pos: usize) {
        crate::backends::pancurses::spreadsheet_set_edit_state(id, editing, edit_buf, edit_pos);
    }
}

pub use pancurses_adapter::*;
