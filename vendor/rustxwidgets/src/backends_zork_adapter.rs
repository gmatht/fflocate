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
    pub fn set_child_box(&self, _child: &BoxWidget) {}

    pub fn set_title(&self, title: &str) {
        crate::backends::zork::set_window_title(self.id, title);
    }

    pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
        let child_ptr = *child.as_ref();
        let child_id = child_ptr as usize;
        crate::backends::zork::set_child(self.id, child_id);
    }

    pub fn present(&self) {}
    pub fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}
    pub fn set_default_size(&self, _width: i32, _height: i32) {}
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
        crate::backends::zork::add_callback(self.id, Box::new(f));
        Ok(0)
    }

    pub fn emit_clicked(&self) -> Result<u64, Error> {
        Ok(0)
    }

    pub fn set_font_style(&self, _weight: i32, _italic: bool) {}
    pub fn set_size_request(&self, _w: i32, _h: i32) {}
}

// -- Label --

pub struct Label {
    pub(crate) id: usize,
}

impl Widget for Label {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
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
        crate::backends::zork::set_label_text(self.id, text);
    }

    pub fn get_text(&self) -> Option<String> {
        crate::backends::zork::get_label_text(self.id)
    }

    pub fn add_class(&self, _class_name: &str) {}
    pub fn remove_class(&self, _class_name: &str) {}
    pub fn set_markup(&self, _markup: &str) {}
    pub fn set_visible(&self, _visible: bool) {}
    pub fn set_xalign(&self, _x: f32) {}
}

// -- BoxWidget --

pub struct BoxWidget {
    pub(crate) id: usize,
    pub(crate) orientation: Orientation,
    pub(crate) spacing: i32,
}
impl Clone for BoxWidget {
    fn clone(&self) -> Self { BoxWidget { id: self.id.clone(), orientation: self.orientation.clone(), spacing: self.spacing.clone() } }
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

impl BoxWidget {
    pub fn set_hexpand(&self, _expand: bool) {}

    pub fn append(&self, child: &impl AsRef<*mut c_void>) {
        let child_ptr = *child.as_ref();
        let child_id = child_ptr as usize;
        crate::backends::zork::append_child(self.id, child_id);
    }

    pub fn layout(&self, _x: i32, _y: i32, _w: i32, _h: i32) {
        crate::backends::zork::layout_box(self.id);
    }

    pub fn set_child_vexpand(&self, _child: &impl AsRef<*mut c_void>, _expand: bool) {}
    pub fn set_child_hexpand(&self, _child: &impl AsRef<*mut c_void>, _expand: bool) {}
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
        crate::backends::zork::append_child(self.id, child_id);
    }

    pub fn layout(&self) {
        crate::backends::zork::layout_grid(self.id);
    }
}

// -- Entry --

pub struct Entry {
    pub(crate) id: usize,
}

impl Widget for Entry {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
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
    pub fn set_hexpand(&self, _expand: bool) {}
    pub fn set_vexpand(&self, _expand: bool) {}

    pub fn set_text(&self, text: &str) {
        crate::backends::zork::entry_set_text(self.id, text);
    }

    pub fn text(&self) -> Option<String> {
        crate::backends::zork::entry_text(self.id)
    }

    pub fn get_text(&self) -> Option<String> {
        self.text()
    }

    pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
        Ok(0)
    }

    pub fn set_halign(&self, _align: i32) {}
    pub fn set_valign(&self, _align: i32) {}
    pub fn set_visible(&self, _visible: bool) {}
        pub fn set_size_request(&self, _w: i32, _h: i32) {}
        pub fn set_width_chars(&self, _w: i32) {}
        pub fn on_key_raw(&self, _cb: Box<dyn FnMut(u32, u32) -> bool>) {}
        pub fn set_margin_start(&self, _margin: i32) {}
        pub fn set_margin_top(&self, _margin: i32) {}
    pub fn add_class(&self, _class_name: &str) {}
    pub fn remove_class(&self, _class_name: &str) {}
    pub fn grab_focus(&self) {}

    pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(&self, f: F) -> Result<u64, Error> {
        let mut f = f;
        crate::backends::zork::add_callback(self.id, Box::new(move || f(std::ptr::null_mut())));
        Ok(0)
    }

    pub fn connect_focus_in_event<F: FnMut(*mut c_void) -> i32 + 'static>(&self, f: F) -> Result<u64, Error> {
        let mut f = f;
        crate::backends::zork::add_callback(self.id, Box::new(move || { f(std::ptr::null_mut()); }));
        Ok(0)
    }

    pub fn connect_focus_out_event<F: FnMut(*mut c_void) -> i32 + 'static>(&self, f: F) -> Result<u64, Error> {
        let mut f = f;
        crate::backends::zork::add_callback(self.id, Box::new(move || { f(std::ptr::null_mut()); }));
        Ok(0)
    }
}

// -- Menu --

pub struct Menu {
    pub(crate) id: usize,
}
impl Clone for Menu {
    fn clone(&self) -> Self { Menu { id: self.id.clone() } }
}


impl AsRef<*mut c_void> for Menu {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl Menu {
    pub fn append(&self, label: &str, action_name: &str) {
        crate::backends::zork::menu_append(self.id, label, action_name);
    }
    pub fn append_submenu(&self, label: &str, submenu: &Menu) {
        crate::backends::zork::menu_append_submenu(self.id, label, submenu.id);
    }
    pub fn append_item(&self, _label: &str, _action: &SimpleAction) {}
    pub fn append_section(&self, _label: &str) {}
}

// -- MenuBar --

pub struct MenuBar {
    pub(crate) id: usize,
}
impl Clone for MenuBar {
    fn clone(&self) -> Self { MenuBar { id: self.id.clone() } }
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

// -- SimpleAction --

pub struct SimpleAction {
    pub(crate) id: usize,
}
impl Clone for SimpleAction {
    fn clone(&self) -> Self { SimpleAction { id: self.id.clone() } }
}


impl AsRef<*mut c_void> for SimpleAction {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl SimpleAction {
    pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(&self, mut f: F) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(move || f(std::ptr::null_mut())));
        Ok(0)
    }

    pub fn on_activate(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
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
        crate::backends::zork::set_window_title(self.id, title);
    }
    pub fn set_default_size(&self, _w: i32, _h: i32) {}
    pub fn append_content_area(&self, child: &impl AsRef<*mut c_void>) {
        let child_ptr = *child.as_ref();
        let child_id = child_ptr as usize;
        crate::backends::zork::set_child(self.id, child_id);
    }
    pub fn add_button(&self, _label: &str, _response_id: i32) {}
        pub fn connect_response(&self, _f: impl FnMut(i32) + 'static) -> Result<u64, Error> { Ok(0) }
        pub fn present(&self) {}
        pub fn close(&self) {}
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
    pub fn set_items(&self, items: &[&str]) {
        crate::backends::zork::set_dropdown_items(self.id, items);
    }
    pub fn set_active(&self, idx: i32) {
        crate::backends::zork::set_dropdown_selected(self.id, idx);
    }
    pub fn get_active(&self) -> i32 {
        crate::backends::zork::get_dropdown_selected(self.id)
    }
    pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
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
        crate::backends::zork::set_checkbutton_checked(self.id, active);
    }

    pub fn is_active(&self) -> bool {
        crate::backends::zork::get_checkbutton_checked(self.id)
    }

    pub fn on_toggle(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
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
        crate::backends::zork::set_radiobutton_checked(self.id, active);
    }

    pub fn is_active(&self) -> bool {
        crate::backends::zork::get_radiobutton_checked(self.id)
    }

    pub fn on_toggle(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
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

impl Widget for TextView {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
}

impl AsRef<*mut c_void> for TextView {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl Clone for TextView {
    fn clone(&self) -> Self { TextView { id: self.id } }
}

impl TextView {
    pub fn set_text(&self, text: &str) {
        crate::backends::zork::set_textview_text(self.id, text);
    }

    pub fn get_text(&self) -> Option<String> {
        crate::backends::zork::get_textview_text(self.id)
    }

    pub fn set_wrap_mode(&self, _mode: i32) {}
    pub fn set_size_request(&self, _w: i32, _h: i32) {}

    /// Register a button-press handler. The REPL has no pointer, so the
    /// `rightclick <line>` command fires it, passing button 3 and the row as
    /// the `y` coordinate. Returns a dummy handler id.
    pub fn connect_button_press_event(
        &self,
        mut cb: impl FnMut(u32, f64, f64, *mut std::ffi::c_void) + 'static,
    ) -> Result<u64, Error> {
        // The REPL has no event pointer; pass null through to the callback.
        crate::backends::zork::set_button_press_cb(
            self.id,
            Box::new(move |button, x, y| cb(button, x, y, std::ptr::null_mut())),
        );
        Ok(0)
    }

    /// In the REPL the `y` coordinate *is* the row (see
    /// `connect_button_press_event`), so this is the identity.
    pub fn line_at(&self, _x: f64, y: f64) -> Option<i32> {
        if y < 0.0 {
            None
        } else {
            Some(y as i32)
        }
    }

    /// There is no other window in a terminal, so nothing can be dropped on.
    /// The payload supplier is still stored so the `drag <line>` command can
    /// show what a drag would carry.
    pub fn enable_uri_drag_source(
        &self,
        payload: impl FnMut() -> Option<(Vec<String>, String)> + 'static,
    ) -> Result<(), Error> {
        crate::backends::zork::set_drag_payload_cb(self.id, Box::new(payload));
        Ok(())
    }

        /// Copy `text` using the terminal clipboard escape (OSC 52), so it works
    /// over ssh as well as locally. Does nothing (and reports so) when stdout is
    /// not a terminal.
    pub fn clipboard_set_text(&self, text: &str) -> Result<(), Error> {
        use std::io::{IsTerminal, Write};
        if !std::io::stdout().is_terminal() {
            return Err(Error::Backend(
                "no terminal to receive the clipboard".into(),
            ));
        }
        // OSC 52: ESC ] 52 ; c ; <base64> BEL. `c` selects the clipboard.
        let mut b64 = String::with_capacity(text.len().div_ceil(3) * 4);
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for chunk in text.as_bytes().chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            b64.push(ALPHABET[(n >> 18) as usize & 63] as char);
            b64.push(ALPHABET[(n >> 12) as usize & 63] as char);
            b64.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
            b64.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
        }
        let mut out = std::io::stdout();
        write!(out, "\x1b]52;c;{b64}\x07")
            .and_then(|_| out.flush())
            .map_err(|e| Error::Backend(format!("clipboard write failed: {e}")))
    }
}

// -- Factory functions --

pub fn create_window() -> Result<Window, Error> {
    crate::backends::zork::create_window()
        .map(|id| Window { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_button(label: &str) -> Result<Button, Error> {
    crate::backends::zork::create_button(label)
        .map(|id| Button { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_label(text: &str) -> Result<Label, Error> {
    crate::backends::zork::create_label(text)
        .map(|id| Label { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_box(orientation: Orientation, spacing: i32) -> Result<BoxWidget, Error> {
    let horizontal = orientation == Orientation::Horizontal;
    crate::backends::zork::create_box(horizontal, spacing)
        .map(|id| BoxWidget { id, orientation, spacing })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_grid() -> Result<Grid, Error> {
    crate::backends::zork::create_grid()
        .map(|id| Grid { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_entry() -> Result<Entry, Error> {
    crate::backends::zork::create_entry()
        .map(|id| Entry { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

// -- Context menu (right-click) --

/// A context menu for the REPL backend: it collects the items and, when popped
/// up, prints them and reads a choice from stdin. This lets the terminal
/// fallback exercise the same Copy / Open actions as the GUI.
pub struct ContextMenu {
    items: Vec<(String, Box<dyn FnMut()>)>,
}

impl ContextMenu {
    pub fn append_item(&mut self, label: &str, cb: Box<dyn FnMut()>) {
        self.items.push((label.to_string(), cb));
    }

    /// Show the menu on stderr and act on the choice read from stdin. `_widget`
    /// and `_event` are meaningless here (there is no pointer).
    pub fn popup_at_pointer(
        &mut self,
        _widget: *mut std::ffi::c_void,
        _event: *mut std::ffi::c_void,
    ) -> Result<(), Error> {
        use std::io::Write;
        if self.items.is_empty() {
            eprintln!("(context menu is empty)");
            return Ok(());
        }
        eprintln!("Context menu:");
        for (i, (label, _)) in self.items.iter().enumerate() {
            eprintln!("  {}. {}", i + 1, label);
        }
        eprint!("Choose an item (Enter to cancel): ");
        std::io::stderr().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).ok();
        match input.trim().parse::<usize>() {
            Ok(n) if n >= 1 && n <= self.items.len() => {
                // The closure may call back into the backend; nothing is
                // borrowed here, so that is safe.
                let (label, cb) = &mut self.items[n - 1];
                eprintln!("(chose: {})", label);
                cb();
            }
            _ => eprintln!("(cancelled)"),
        }
        Ok(())
    }
}

pub fn create_context_menu() -> Result<ContextMenu, Error> {
    Ok(ContextMenu { items: Vec::new() })
}

pub fn create_menu() -> Result<Menu, Error> {
    crate::backends::zork::create_menu()
        .map(|id| Menu { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_menubar(model: &Menu, _action_group: *mut c_void) -> Result<MenuBar, Error> {
    unsafe {
        crate::backends::zork::create_menubar(model.id, _action_group)
            .map(|id| MenuBar { id })
            .map_err(|e| Error::Backend(format!("{}", e)))
    }
}

pub fn create_simple_action(name: &str) -> Result<SimpleAction, Error> {
    crate::backends::zork::create_simple_action(name)
        .map(|id| SimpleAction { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_dialog() -> Result<Dialog, Error> {
    crate::backends::zork::create_dialog()
        .map(|id| Dialog { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_dropdown(items: &[&str]) -> Result<DropDown, Error> {
    crate::backends::zork::create_dropdown(items)
        .map(|id| DropDown { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_checkbutton(label: &str) -> Result<CheckButton, Error> {
    crate::backends::zork::create_checkbutton(label)
        .map(|id| CheckButton { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_radiobutton(group: Option<&RadioButton>, label: &str) -> Result<RadioButton, Error> {
    let gid = group.map(|r| r.id);
    crate::backends::zork::create_radiobutton(gid, label)
        .map(|id| RadioButton { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_textview() -> Result<TextView, Error> {
    crate::backends::zork::create_textview()
        .map(|id| TextView { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

// -- Stub types for app.rs compatibility --

pub struct Canvas {
    pub(crate) id: usize,
}

impl Widget for Canvas {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
}

impl AsRef<*mut c_void> for Canvas {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl Clone for Canvas {
    fn clone(&self) -> Self { Canvas { id: self.id } }
}

impl Canvas {
    pub fn set_size_request(&self, _w: i32, _h: i32) {}
    pub fn set_content_size(&self, _w: i32, _h: i32) {}
    pub fn queue_redraw(&self) {}
    pub fn set_draw_callback(&self, _cb: Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>) {}
    pub fn on_click(&self, _cb: Box<dyn FnMut(f64, f64)>) {}
    pub fn on_key(&self, _cb: Box<dyn FnMut(u32) -> bool>) {}
    pub fn on_key_raw(&self, _cb: Box<dyn FnMut(u32, u32) -> bool>) {}
    pub fn grab_focus(&self) {}
    pub fn set_can_focus(&self, _can: bool) {}
    pub fn force_draw(&self, _window_ptr: *mut c_void, _fallback_w: i32, _fallback_h: i32) {}
}

pub struct Overlay {
    pub(crate) id: usize,
}

impl Widget for Overlay {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
}

impl AsRef<*mut c_void> for Overlay {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl Clone for Overlay {
    fn clone(&self) -> Self { Overlay { id: self.id } }
}

impl Overlay {
    pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
        let child_ptr = *child.as_ref();
        let child_id = child_ptr as usize;
        crate::backends::zork::set_child(self.id, child_id);
    }
    pub fn add_overlay(&self, child: &impl AsRef<*mut c_void>) {
        let child_ptr = *child.as_ref();
        let child_id = child_ptr as usize;
        crate::backends::zork::set_child(self.id, child_id);
    }
    pub fn set_overlay_pass_through(&self, _child: &impl AsRef<*mut c_void>, _pass: bool) {}
    pub fn remove(&self, _child: &impl AsRef<*mut c_void>) {}
    pub fn show_all(&self) {}
    pub fn set_size_request(&self, _w: i32, _h: i32) {}
}

pub struct ScrolledWindow {
    pub(crate) id: usize,
}

impl Widget for ScrolledWindow {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
}

impl AsRef<*mut c_void> for ScrolledWindow {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl Clone for ScrolledWindow {
    fn clone(&self) -> Self { ScrolledWindow { id: self.id } }
}

impl ScrolledWindow {
    pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
        let child_ptr = *child.as_ref();
        let child_id = child_ptr as usize;
        crate::backends::zork::set_child(self.id, child_id);
    }
    pub fn set_policy(&self, _hscroll: u32, _vscroll: u32) {}
}

pub fn create_canvas() -> Result<Canvas, Error> {
    Ok(Canvas { id: crate::backends::zork::create_canvas() })
}

pub fn create_overlay() -> Result<Overlay, Error> {
    Ok(Overlay { id: crate::backends::zork::create_overlay() })
}

pub fn create_scrolled_window() -> Result<ScrolledWindow, Error> {
    Ok(ScrolledWindow { id: crate::backends::zork::create_scrolled_window() })
}

// -- Expander --

pub struct Expander {
    id: usize,
}

impl Widget for Expander {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
}

impl AsRef<*mut c_void> for Expander {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl Clone for Expander {
    fn clone(&self) -> Self { Expander { id: self.id } }
}

impl Expander {
    pub fn set_expanded(&self, expanded: bool) {
        crate::backends::zork::set_expander_expanded(self.id, expanded);
    }
    pub fn is_expanded(&self) -> bool {
        crate::backends::zork::expander_is_expanded(self.id)
    }
    pub fn set_label(&self, _label: &str) {}
    pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
        let child_id = *child.as_ref() as usize;
        crate::backends::zork::set_child(self.id, child_id);
    }
    pub fn set_visible(&self, _visible: bool) {}
    pub fn set_hexpand(&self, _expand: bool) {}
    pub fn set_vexpand(&self, _expand: bool) {}
    pub fn set_size_request(&self, _w: i32, _h: i32) {}
}

pub fn create_expander(label: &str) -> Result<Expander, Error> {
    Ok(Expander { id: crate::backends::zork::create_expander(label) })
}

pub fn open_file(_title: &str) -> Result<Option<String>, Error> {
    Ok(None)
}

pub fn save_file(_title: &str) -> Result<Option<String>, Error> {
    Ok(None)
}
