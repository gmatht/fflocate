//! Per-widget adapter for the headless `zork` backend.
//!
//! Every widget is a newtype over a `usize` model node id, and every method is a
//! one-line delegation to the matching `backends::zork::*` free function (which
//! the [`facade`](crate::backends::zork::facade) maps onto
//! [`ZorkState`](crate::backends::zork::model::ZorkState) operations).
//!
//! The previous version of this file was a stub: `Canvas`, `Overlay` and
//! `ScrolledWindow` had no model node at all, and ~50 methods were empty bodies
//! that silently discarded their arguments, so the `zork` feature did not even
//! compile against `common.rs`/`core.rs`. Everything GTK/NWG expose that makes
//! sense for a headless model is now wired through.

use std::os::raw::c_void;
use crate::backends::zork::MenuItemData;
use crate::core::{Error, Widget};

/// Extract a model node id from a widget handle's raw pointer.
#[inline]
fn id_of(child: &impl AsRef<*mut c_void>) -> usize {
    *child.as_ref() as usize
}


impl Window {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl Button {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl Label {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl BoxWidget {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl Grid {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl Entry {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl Menu {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl MenuBar {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl SimpleAction {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl Dialog {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl DropDown {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl CheckButton {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl RadioButton {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl TextView {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl Canvas {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl Overlay {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl ScrolledWindow {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl Fixed {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl Spreadsheet {
    /// The model node id. Useful for diagnostics and for correlating a widget
    /// with a [`ZorkState::snapshot`](crate::backends::zork::model::ZorkState::snapshot)
    /// entry; the [`crate::backends::zork`] free functions take the same id.
    pub fn id(&self) -> usize {
        self.id
    }
}

/// Strip a GTK mnemonic marker: `&` before a character makes that character the
/// mnemonic, and `&_` is a literal underscore. A trailing `&` is kept.
///
/// `&Save` -> `Save`, `P_rint` -> `Print`, `&_&` -> `_&`.
fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        // `next()` consumes, so the marked character is not also pushed by the
        // loop head — peeking here would duplicate it.
        match chars.next() {
            Some('_') => out.push('_'),
            Some(next) => out.push(next),
            None => out.push('&'),
        }
    }
    out
}

// -- Window --

#[derive(Clone)]
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

impl Window {
    pub fn set_title(&self, title: &str) {
        crate::backends::zork::set_window_title(self.id, title);
    }

    pub fn get_title(&self) -> Option<String> {
        crate::backends::zork::get_window_title(self.id)
    }

    pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
        crate::backends::zork::set_child(self.id, id_of(child));
    }

    /// `common::Window::set_child_box` — a convenience alias for `set_child`.
    pub fn set_child_box(&self, bx: &BoxWidget) {
        crate::backends::zork::set_child(self.id, bx.id);
    }

    pub fn present(&self) {}

    /// Zork has no `GActionGroup`; the actions a menu names are registered in
    /// the model by `create_simple_action`, so this is a no-op like NWG's.
    pub fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}

    pub fn set_default_size(&self, width: i32, height: i32) {
        crate::backends::zork::set_size_request(self.id, width, height);
    }

    /// Immediate resize. Headless windows have no WM to honour
    /// `set_default_size`, so this records the same size; the model is the
    /// authority, so the distinction GTK needs does not exist here.
    pub fn resize(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }

    pub fn hwnd(&self) -> *mut c_void {
        std::ptr::null_mut()
    }

    pub fn on_event(&self, cb: Box<dyn FnMut(*mut c_void) -> i32>) {
        let mut cb = cb;
        crate::backends::zork::add_callback(self.id, Box::new(move || { cb(std::ptr::null_mut()); }));
    }

    pub fn on_event_key(&self, cb: Box<dyn FnMut(u32, u32) -> i32>) {
        let mut cb = cb;
        crate::backends::zork::add_callback(self.id, Box::new(move || { cb(0, 0); }));
    }

    pub fn on_close(&self, cb: Box<dyn FnMut()>) {
        crate::backends::zork::add_callback(self.id, cb);
    }

    pub fn queue_redraw(&self) {
        crate::backends::zork::queue_redraw(self.id);
    }

    /// Start a repeating timer owned by this window, under `id`.
    ///
    /// NWG's version returns the NWG handler id so the caller can cancel it
    /// with `KillTimer`; here the id is the caller's own handle and
    /// [`cancel_timer`] takes it. The timer fires when the driver advances the
    /// model's clock (the REPL does this each turn), so `ms` is model time
    /// rather than wall time — a headless backend has no clock to wait on.
    pub fn start_repeating_timer(&self, id: usize, ms: u32, f: Box<dyn FnMut() -> bool>) -> Result<(), Error> {
        crate::backends::zork::timeout_add_repeating_for_window(self.id, id, ms, f);
        Ok(())
    }

    /// A callback to run whenever the window is (re)laid out, as
    /// `(w, h)`.
    ///
    /// NWG hooks this to `WM_SIZE`; zork has no size events, but a `Fixed`
    /// container's children *are* positioned by a layout pass, and a host that
    /// wants to react to that has no other hook. The callback is stored rather
    /// than dropped, so `Harness::layout_box` can invoke it — see
    /// [`crate::backends::zork::fire_layout_callbacks`].
    pub fn set_layout_cb<F: FnMut(i32, i32) + 'static>(&self, f: F) {
        crate::backends::zork::set_layout_callback(self.id, Box::new(f));
    }
}

// -- Button --

#[derive(Clone)]
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

impl Button {
    pub fn on_click(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
        Ok(0)
    }

    /// Fire the click handlers from the model, so a test can drive the button
    /// without reaching for the harness. (The old version returned `Ok(0)`
    /// without doing anything.)
    pub fn emit_clicked(&self) -> Result<u64, Error> {
        crate::backends::zork::fire(self.id);
        Ok(0)
    }

    pub fn set_label(&self, label: &str) {
        crate::backends::zork::set_button_label(self.id, label);
    }
    pub fn get_label(&self) -> Option<String> {
        crate::backends::zork::get_button_label(self.id)
    }
    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_visible(&self, visible: bool) {
        crate::backends::zork::set_visible(self.id, visible);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }
    pub fn add_class(&self, class_name: &str) {
        crate::backends::zork::add_class(self.id, class_name);
    }
    pub fn remove_class(&self, class_name: &str) {
        crate::backends::zork::remove_class(self.id, class_name);
    }
    /// `font` is a family name; `style` is a [`crate::backends::zork::model::font_style`]
    /// bit set.
    pub fn set_font_style(&self, font: &str, size: f64, style: i32) {
        crate::backends::zork::set_font_style(self.id, font, size, style);
    }
    pub fn grab_focus(&self) {
        crate::backends::zork::set_focus(self.id);
    }
}

// -- Label --

#[derive(Clone)]
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

impl Label {
    /// Inherent `raw_handle`, so `common::Label::raw_handle` (and any caller
    /// with a `Label` value rather than a `&dyn Widget`) resolves without
    /// importing [`Widget`]. GTK/NWG both provide this inherent form.
    pub fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }

    pub fn set_text(&self, text: &str) {
        crate::backends::zork::set_label_text(self.id, text);
    }

    pub fn get_text(&self) -> Option<String> {
        crate::backends::zork::get_label_text(self.id)
    }

    pub fn add_class(&self, class_name: &str) {
        crate::backends::zork::add_class(self.id, class_name);
    }
    pub fn remove_class(&self, class_name: &str) {
        crate::backends::zork::remove_class(self.id, class_name);
    }

    /// Zork labels are plain text; the markup is recorded as the label text so
    /// a test can see what was set, but nothing is parsed.
    pub fn set_markup(&self, markup: &str) {
        crate::backends::zork::set_label_text(self.id, markup);
    }
    pub fn set_visible(&self, visible: bool) {
        crate::backends::zork::set_visible(self.id, visible);
    }
    pub fn set_xalign(&self, x: f32) {
        crate::backends::zork::set_xalign(self.id, x);
    }
    pub fn set_halign(&self, align: i32) {
        crate::backends::zork::set_halign(self.id, align);
    }
    pub fn set_valign(&self, align: i32) {
        crate::backends::zork::set_valign(self.id, align);
    }
    pub fn set_fixed_width(&self, w: Option<i32>) {
        crate::backends::zork::set_fixed_width(self.id, w);
    }
    pub fn set_margin_start(&self, margin: i32) {
        crate::backends::zork::set_margin_start(self.id, margin);
    }
    pub fn set_margin_top(&self, margin: i32) {
        crate::backends::zork::set_margin_top(self.id, margin);
    }
    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }
}

// -- BoxWidget --

#[derive(Clone, Copy, PartialEq)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

#[derive(Clone)]
pub struct BoxWidget {
    pub(crate) id: usize,
    /// Kept so a caller can recover the orientation/spacing it asked for; the
    /// model is authoritative and is read back through `ZorkState`.
    #[allow(dead_code)]
    pub(crate) orientation: Orientation,
    #[allow(dead_code)]
    pub(crate) spacing: i32,
}

impl Widget for BoxWidget {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
}

impl AsRef<*mut c_void> for BoxWidget {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl BoxWidget {
    pub fn append(&self, child: &impl AsRef<*mut c_void>) {
        crate::backends::zork::append_child(self.id, id_of(child));
    }

    /// Pack the visible children along the box axis, honouring `spacing`, and
    /// record the box's own size. The old body dropped all four arguments.
    pub fn layout(&self, x: i32, y: i32, w: i32, h: i32) {
        crate::backends::zork::layout_box(self.id, x, y, w, h);
    }

    /// The size the box's visible children imply, or `None` for a non-box.
    pub fn measure(&self) -> Option<(i32, i32)> {
        crate::backends::zork::measure_box(self.id)
    }

    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_visible(&self, visible: bool) {
        crate::backends::zork::set_visible(self.id, visible);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }
    pub fn set_child_hexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
        crate::backends::zork::set_child_hexpand(self.id, id_of(child), expand);
    }
    pub fn set_child_vexpand(&self, child: &impl AsRef<*mut c_void>, expand: bool) {
        crate::backends::zork::set_child_vexpand(self.id, id_of(child), expand);
    }
    /// Ask the model to re-pack the children. Headless boxes have no deferred
    /// layout pass, so this is a hint a caller can ignore.
    pub fn request_layout(&self) {}
}

// -- Grid --

#[derive(Clone)]
pub struct Grid {
    pub(crate) id: usize,
}

impl Widget for Grid {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
}

impl AsRef<*mut c_void> for Grid {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl Grid {
    /// Record a child in the given cell, growing the grid to cover it.
    pub fn attach(&self, child: &impl AsRef<*mut c_void>, left: i32, top: i32, width: i32, height: i32) {
        crate::backends::zork::grid_attach(self.id, id_of(child), left, top, width, height);
    }

    pub fn layout(&self) {
        // A grid's cells are absolute, so a layout pass has nothing to reflow;
        // the recorded attachments already are the layout.
    }

    /// The grid's `(cols, rows)` as grown by `attach`.
    pub fn dimensions(&self) -> (usize, usize) {
        crate::backends::zork::grid_dimensions(self.id)
    }

    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_visible(&self, visible: bool) {
        crate::backends::zork::set_visible(self.id, visible);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }
}

// -- Entry --

#[derive(Clone)]
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

impl Entry {
    pub fn set_text(&self, text: &str) {
        crate::backends::zork::set_entry_text(self.id, text);
    }

    pub fn text(&self) -> Option<String> {
        crate::backends::zork::get_entry_text(self.id)
    }

    pub fn get_text(&self) -> Option<String> {
        self.text()
    }

    pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
        Ok(0)
    }

    pub fn set_halign(&self, align: i32) {
        crate::backends::zork::set_halign(self.id, align);
    }
    pub fn set_valign(&self, align: i32) {
        crate::backends::zork::set_valign(self.id, align);
    }
    pub fn set_visible(&self, visible: bool) {
        crate::backends::zork::set_visible(self.id, visible);
    }
    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_width_chars(&self, w: i32) {
        crate::backends::zork::set_size_request(self.id, w * 8, -1);
    }
    pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
        crate::backends::zork::add_key_raw_hook(self.id, cb);
    }
    pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) {
        crate::backends::zork::add_key_hook(self.id, cb);
    }
    pub fn set_margin_start(&self, margin: i32) {
        crate::backends::zork::set_margin_start(self.id, margin);
    }
    pub fn set_margin_top(&self, margin: i32) {
        crate::backends::zork::set_margin_top(self.id, margin);
    }
    pub fn add_class(&self, class_name: &str) {
        crate::backends::zork::add_class(self.id, class_name);
    }
    pub fn remove_class(&self, class_name: &str) {
        crate::backends::zork::remove_class(self.id, class_name);
    }
    pub fn grab_focus(&self) {
        crate::backends::zork::set_focus(self.id);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }

    /// The caret, as a character index. The model tracks it (it used to be
    /// hard-coded `None`, so `common::Entry`'s caret-override fallback was the
    /// only thing that ever worked here).
    pub fn get_position(&self) -> Option<usize> {
        crate::backends::zork::get_entry_position(self.id)
    }

    pub fn set_position(&self, pos: usize) {
        crate::backends::zork::set_entry_position(self.id, pos);
    }

    /// Zork entries have a real focus state, so this reports the model rather
    /// than the unconditional `false` the old stub returned.
    pub fn has_focus(&self) -> bool {
        crate::backends::zork::has_focus(self.id)
    }

    /// Fires on pointer press into the entry. The callback is registered and
    /// fired by `Harness::click`; the old version discarded it.
    pub fn connect_button_press(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
        Ok(0)
    }

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

#[derive(Clone)]
pub struct Menu {
    pub(crate) id: usize,
}

impl Widget for Menu {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
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
    /// Append an item bound to a `SimpleAction` (whose name is the action).
    pub fn append_item(&self, label: &str, action: &SimpleAction) {
        crate::backends::zork::menu_append(self.id, label, action.name());
    }
    /// Append a non-interactive section header.
    pub fn append_section(&self, label: &str) {
        crate::backends::zork::menu_append_section(self.id, label);
    }
    /// Append an item with a keyboard accelerator, e.g. `"Ctrl+S"`.
    pub fn append_with_shortcut(&self, label: &str, action: &str, accelerator: &str) {
        crate::backends::zork::menu_append_with_shortcut(self.id, label, action, accelerator);
    }
    pub fn append_submenu_with_shortcut(&self, label: &str, submenu: &Menu, accelerator: &str) {
        crate::backends::zork::menu_append_submenu(self.id, label, submenu.id);
        if let Some(items) = crate::backends::zork::menu_items(self.id).last_mut() {
            items.accelerator = accelerator.to_string();
        }
    }
    /// Append an item whose *label* carries a GTK mnemonic marker (the
    /// `label_with_mnemonic` family). The `_` is stripped so the stored label
    /// is what a user would read.
    pub fn append_key(&self, label: &str, action: &str) {
        crate::backends::zork::menu_append(self.id, &strip_mnemonic(label), action);
    }
    /// See [`Self::append_key`].
    pub fn append_submenu_key(&self, label: &str, submenu: &Menu) {
        crate::backends::zork::menu_append_submenu(self.id, &strip_mnemonic(label), submenu.id);
    }
    pub fn append_separator(&self, label: &str) {
        crate::backends::zork::menu_append_separator(self.id, label);
    }
    /// Append a check item; returns its 0-based index for
    /// [`Self::set_item_checked`].
    pub fn append_check(&self, label: &str, action: &str, checked: bool) -> usize {
        crate::backends::zork::menu_append_check(self.id, label, action, checked)
    }
    /// Append a radio item in `group`; returns its 0-based index.
    pub fn append_radio(&self, label: &str, action: &str, group: usize, checked: bool) -> usize {
        crate::backends::zork::menu_append_radio(self.id, label, action, group, checked)
    }
    pub fn set_item_checked(&self, index: usize, checked: bool) {
        crate::backends::zork::set_menu_item_checked(self.id, index, checked);
    }
    pub fn items(&self) -> Vec<MenuItemData> {
        crate::backends::zork::menu_items(self.id)
    }
    /// Select item `index` (0-based) and dispatch the action it names.
    pub fn select(&self, index: usize) -> Option<String> {
        crate::backends::zork::menu_select(self.id, index)
    }
}

// -- MenuBar --

#[derive(Clone)]
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

impl MenuBar {
    pub fn items(&self) -> Vec<MenuItemData> {
        crate::backends::zork::menu_items(self.id)
    }
    /// Select a top-level item (0-based) or, for a submenu item, the nested
    /// item of the same 0-based index. Dispatches the action it names.
    pub fn select(&self, index: usize) -> Option<String> {
        crate::backends::zork::menu_select(self.id, index)
    }

    /// Zork has no popups, so a mnemonic cannot open a submenu. It reports
    /// "not handled" so the caller falls through to its own key handling —
    /// the same honest answer NWG and pancurses give.
    pub fn activate_submenu_by_mnemonic(&self, _keyval: u32) -> bool {
        false
    }
    /// See [`Self::activate_submenu_by_mnemonic`]: no popups, so no
    /// positioned popup either.
    pub fn popup_submenu_by_mnemonic_at(&self, _keyval: u32, _screen_x: i32, _screen_y: i32) -> bool {
        false
    }
    pub fn activate_submenu_item_by_mnemonic(&self, _keyval: u32) -> bool {
        false
    }
    /// See [`Window::insert_action_group`].
    pub fn insert_action_group(&self, _name: &str, _group_ptr: *mut c_void) {}
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
        crate::backends::zork::menu_active(self.id)
    }
    pub fn menu_close(&self) {}
}

// -- SimpleAction --

#[derive(Clone)]
pub struct SimpleAction {
    pub(crate) id: usize,
    pub(crate) name: String,
}

impl Widget for SimpleAction {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
}

impl AsRef<*mut c_void> for SimpleAction {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl SimpleAction {
    /// The action's name, as registered in the model. `Menu::append_item` uses
    /// it to resolve the item back to this action.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn on_activate(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
        Ok(0)
    }

    /// `common::SimpleAction` calls this name. Zork's action carries the
    /// `(name, model-id)` pair, so the pointer is not needed.
    pub fn connect_activate<F: FnMut(*mut c_void) + 'static>(&self, f: F) -> Result<u64, Error> {
        let mut f = f;
        crate::backends::zork::add_callback(self.id, Box::new(move || f(std::ptr::null_mut())));
        Ok(0)
    }

    /// Fire the action's handlers. Returns whether the name resolved, so a test
    /// can assert a menu item actually reached its action.
    pub fn activate(&self) -> bool {
        crate::backends::zork::activate_action(&self.name)
    }
}

// -- Dialog --

#[derive(Clone)]
pub struct Dialog {
    pub(crate) id: usize,
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
    pub fn set_default_size(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    /// Record the parent window. A headless dialog has no WM to place it, but
    /// the model keeps the relation so a test can assert it.
    pub fn set_transient_for(&self, parent: *mut c_void) {
        let pid = if parent.is_null() { None } else { Some(parent as usize) };
        crate::backends::zork::dialog_set_transient_for(self.id, pid);
    }
    pub fn get_transient_for(&self) -> Option<usize> {
        crate::backends::zork::dialog_transient_for(self.id)
    }
    pub fn append_content_area(&self, child: &impl AsRef<*mut c_void>) {
        crate::backends::zork::set_child(self.id, id_of(child));
    }
    /// The content area is the dialog's first child, so its id is the dialog's
    /// own child slot.
    pub fn get_content_area(&self) -> *mut c_void {
        let child = crate::backends::zork::first_child(self.id);
        match child {
            Some(c) => c as *mut c_void,
            None => std::ptr::null_mut(),
        }
    }
    pub fn add_button(&self, label: &str, response_id: i32) {
        crate::backends::zork::dialog_add_button(self.id, label, response_id);
    }
    /// Measure the dialog before it opens.
    ///
    /// GTK and NWG have a real `layout_dialog` because a dialog that opens at
    /// the height of nothing shows its content a frame late. The zork model has
    /// no compositor, so there is nothing to measure — but the model *can*
    /// record the size its children asked for, which is what a caller laying
    /// the dialog out itself actually needs to read back. So this is a real
    /// operation (it returns the measured content extent) rather than a no-op.
    pub fn layout_dialog(&self) {
        let (w, h) = crate::backends::zork::measure_dialog(self.id).unwrap_or((0, 0));
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    /// The dialog's content extent, or `None` if it has no children.
    pub fn measure_dialog(&self) -> Option<(i32, i32)> {
        crate::backends::zork::measure_dialog(self.id)
    }
    /// Run the dialog and return the response id.
    ///
    /// There is no nested event loop here (the same situation as the Android
    /// adapter): presenting is a state change, and without an input source
    /// there is nothing to wait for. The default response is returned, which is
    /// what a caller with no way to click a button should see.
    pub fn run(&self) -> i32 {
        self.present();
        crate::backends::zork::dialog_default_response(self.id).unwrap_or(0)
    }
    pub fn set_default_response(&self, response_id: i32) {
        crate::backends::zork::dialog_set_default_response(self.id, response_id);
    }
    pub fn buttons(&self) -> Vec<(String, i32)> {
        crate::backends::zork::dialog_buttons(self.id)
    }
    pub fn connect_response<F: FnMut(i32) + 'static>(&self, f: F) -> Result<u64, Error> {
        // The handler is stored on the node and `Dialog::respond` calls it with
        // the response id, so the old "always 0" behaviour is gone.
        crate::backends::zork::add_response_callback(self.id, Box::new(f));
        Ok(0)
    }
    /// Deliver `response_id`. Only a response registered with
    /// [`Self::add_button`] reaches the handler, and the handler sees the id.
    pub fn respond(&self, response_id: i32) {
        crate::backends::zork::dialog_respond(self.id, response_id);
    }
    pub fn present(&self) {
        crate::backends::zork::set_visible(self.id, true);
    }
    /// Hide and mark the dialog destroyed. The old body was a comment-only
    /// no-op, so a test could not tell a closed dialog from a live one.
    pub fn close(&self) {
        crate::backends::zork::dialog_mark_destroyed(self.id);
    }
    pub fn set_visible(&self, visible: bool) {
        crate::backends::zork::set_visible(self.id, visible);
    }
    pub fn is_destroyed(&self) -> bool {
        crate::backends::zork::is_destroyed(self.id)
    }
    /// GTK's name for [`Self::close`]: destroy the dialog's window. The model
    /// records the same state (destroyed + hidden), so the two are the same
    /// operation here.
    pub fn mark_destroyed(&self) {
        crate::backends::zork::dialog_mark_destroyed(self.id);
    }

    /// The response ids a test can fire, in registration order.
    pub fn response_ids(&self) -> Vec<i32> {
        crate::backends::zork::dialog_buttons(self.id).into_iter().map(|(_, r)| r).collect()
    }
}

// -- DropDown --

#[derive(Clone)]
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

impl DropDown {
    pub fn set_items(&self, items: &[&str]) {
        crate::backends::zork::set_dropdown_items(self.id, items);
    }
    /// GTK/NWG take `Option<u32>` (a `None` = "no selection"); the model stores
    /// an `Option<usize>`, so keep the same external contract.
    pub fn set_active(&self, index: Option<u32>) {
        let idx = match index {
            Some(i) => i as i32,
            None => -1,
        };
        crate::backends::zork::set_dropdown_selected(self.id, idx);
    }
    pub fn get_active(&self) -> i32 {
        crate::backends::zork::get_dropdown_selected(self.id)
    }
    /// NWG's name for `get_active`.
    pub fn active(&self) -> i32 {
        self.get_active()
    }
    pub fn connect_changed(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
        Ok(0)
    }
    pub fn grab_focus(&self) {
        crate::backends::zork::set_focus(self.id);
    }
    pub fn set_visible(&self, visible: bool) {
        crate::backends::zork::set_visible(self.id, visible);
    }
    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }
    /// Offset the popup list, matching GTK/NWG's `set_offset`.
    pub fn set_offset(&self, x: i32, y: i32) {
        crate::backends::zork::set_offset(self.id, x, y);
    }
}

// -- CheckButton --

#[derive(Clone)]
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

impl CheckButton {
    pub fn set_active(&self, active: bool) {
        crate::backends::zork::set_checkbutton_checked(self.id, active);
    }

    pub fn is_active(&self) -> bool {
        crate::backends::zork::get_checkbutton_checked(self.id)
    }

    pub fn set_label(&self, label: &str) {
        crate::backends::zork::set_button_label(self.id, label);
    }

    pub fn get_label(&self) -> Option<String> {
        crate::backends::zork::get_button_label(self.id)
    }

    pub fn on_toggle(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
        Ok(0)
    }

    pub fn connect_toggled(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        self.on_toggle(f)
    }

    /// Flip the check state and fire the handlers. This is what makes a
    /// `CheckButton` drivable from a test without the harness.
    pub fn toggle(&self) {
        crate::backends::zork::toggle(self.id);
    }

    pub fn set_visible(&self, visible: bool) {
        crate::backends::zork::set_visible(self.id, visible);
    }
    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }
    pub fn grab_focus(&self) {
        crate::backends::zork::set_focus(self.id);
    }
}

// -- RadioButton --

#[derive(Clone)]
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

impl RadioButton {
    /// Checking a radio clears every other radio in the same group, which the
    /// old `set_active` did not do.
    pub fn set_active(&self, active: bool) {
        crate::backends::zork::set_radiobutton_checked(self.id, active);
    }

    pub fn is_active(&self) -> bool {
        crate::backends::zork::get_radiobutton_checked(self.id)
    }

    pub fn set_label(&self, label: &str) {
        crate::backends::zork::set_button_label(self.id, label);
    }

    pub fn get_label(&self) -> Option<String> {
        crate::backends::zork::get_button_label(self.id)
    }

    pub fn on_toggle(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        crate::backends::zork::add_callback(self.id, Box::new(f));
        Ok(0)
    }

    pub fn connect_toggled(&self, f: impl FnMut() + 'static) -> Result<u64, Error> {
        self.on_toggle(f)
    }

    /// Flip and fire, keeping the group mutually exclusive.
    pub fn toggle(&self) {
        crate::backends::zork::toggle(self.id);
    }

    /// The group this button belongs to, or `None` if it is not a radio.
    /// Group `0` means "ungrouped".
    pub fn group(&self) -> Option<usize> {
        crate::backends::zork::radiobutton_group(self.id)
    }

    /// Join `group`. Lets a radio built without a seed be grouped later.
    pub fn set_group(&self, group_id: usize) {
        crate::backends::zork::set_radiobutton_group(self.id, group_id);
    }

    pub fn grab_focus(&self) {
        crate::backends::zork::set_focus(self.id);
    }
    pub fn set_visible(&self, visible: bool) {
        crate::backends::zork::set_visible(self.id, visible);
    }
    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }
}

// -- TextView --

#[derive(Clone)]
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

impl TextView {
    pub fn set_text(&self, text: &str) {
        crate::backends::zork::set_textview_text(self.id, text);
    }

    pub fn get_text(&self) -> Option<String> {
        crate::backends::zork::get_textview_text(self.id)
    }

    /// NWG's name for `get_text`.
    pub fn get_buffer(&self) -> Option<String> {
        self.get_text()
    }

    pub fn set_editable(&self, editable: bool) {
        crate::backends::zork::set_editable(self.id, editable);
    }

    pub fn is_editable(&self) -> bool {
        crate::backends::zork::get_editable(self.id)
    }

    pub fn set_wrap_mode(&self, _mode: i32) {}
    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_visible(&self, visible: bool) {
        crate::backends::zork::set_visible(self.id, visible);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }

    /// Append to the buffer.
    ///
    /// The model appends incrementally, so this is O(appended) rather than the
    /// read-modify-write the NWG adapter documents.
    pub fn append_text(&self, text: &str) {
        crate::backends::zork::append_textview_text(self.id, text);
    }
}

// -- Canvas --

#[derive(Clone)]
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

impl Canvas {
    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }

    /// Set the *content* size, which is also the size handed to the draw
    /// callback. The old body dropped both arguments.
    pub fn set_content_size(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }

    /// Record a redraw request so a test can assert the canvas was invalidated.
    pub fn queue_redraw(&self) {
        crate::backends::zork::queue_redraw(self.id);
    }

    pub fn redraw_count(&self) -> u32 {
        crate::backends::zork::redraw_count(self.id)
    }

    /// Install the draw callback. It is kept (not discarded as before) and is
    /// invoked by [`Self::draw`].
    pub fn set_draw_callback(&self, cb: Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>) {
        crate::backends::zork::set_draw_callback(self.id, cb);
    }

    pub fn clear_draw_callback(&self) {
        crate::backends::zork::clear_draw_callback(self.id);
    }

    /// Run the draw callback against a recording context and return the
    /// recorded ops. This is the headless equivalent of a real paint, and lets
    /// a test assert exactly what the canvas drew.
    pub fn draw(&self) -> Vec<crate::backends::headless::DrawOp> {
        crate::backends::zork::draw_canvas(self.id)
    }

    pub fn on_click(&self, cb: Box<dyn FnMut(f64, f64)>) {
        crate::backends::zork::add_click_hook(self.id, cb);
    }

    /// The button/modifier-aware click. Registered and fired, unlike the old
    /// no-op body.
    pub fn on_click_button(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
        crate::backends::zork::add_click_button_hook(self.id, cb);
    }
    pub fn on_motion(&self, cb: Box<dyn FnMut(f64, f64, u32)>) {
        crate::backends::zork::add_motion_hook(self.id, cb);
    }
    pub fn on_release(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
        crate::backends::zork::add_release_hook(self.id, cb);
    }
    pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) {
        crate::backends::zork::add_key_hook(self.id, cb);
    }
    pub fn on_key_raw(&self, cb: Box<dyn FnMut(u32, u32) -> bool>) {
        crate::backends::zork::add_key_raw_hook(self.id, cb);
    }

    /// The canonical pointer input: one callback for hover, drag, buttons and
    /// scroll, as `crate::core::Gesture`.
    ///
    /// The recommended way to handle pointer input on a canvas, for the same
    /// reason as on GTK: a caller that needs a drag has to reassemble one from
    /// press/motion/release and work out the click-versus-drag threshold
    /// itself, which is the part that is easy to get subtly wrong.
    /// [`crate::core::DragTracker`] does that once, from a single stream.
    ///
    /// The gestures are synthesised from the same `pointer_*` entry points the
    /// raw hooks use, so a host that registers both gets both from one event,
    /// and a drag is only reported while a button is actually held.
    pub fn on_gesture(&self, cb: Box<dyn FnMut(crate::core::Gesture)>) {
        crate::backends::zork::add_gesture_hook(self.id, cb);
    }
    /// How many gesture handlers this canvas has.
    pub fn gesture_hook_count(&self) -> usize {
        crate::backends::zork::gesture_hook_count(self.id)
    }
    /// Deliver a scroll as a `Gesture::Scroll`, where `dx`/`dy` are deltas.
    ///
    /// Separate from `on_scroll` because that one reports a *position* and this
    /// one a *movement*; feeding one into the other loses a factor of the
    /// scroll rate, which is the mistake `ScrollDelta` exists to prevent.
    pub fn scroll_gesture(&self, dx: f64, dy: f64, x: f64, y: f64, state: u32) {
        crate::backends::zork::pointer_scroll(self.id, dx, dy, x, y, state);
    }

    /// This canvas's top-left in screen coordinates, or `None` when the backend
    /// cannot report one. Default `None`, matching the old behaviour;
    /// [`Self::set_screen_origin`] lets a test declare one.
    pub fn screen_origin(&self) -> Option<(i32, i32)> {
        crate::backends::zork::get_screen_origin(self.id)
    }

    pub fn set_screen_origin(&self, origin: Option<(i32, i32)>) {
        crate::backends::zork::set_screen_origin(self.id, origin);
    }

    pub fn set_visible(&self, v: bool) {
        crate::backends::zork::set_visible(self.id, v);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }
    pub fn set_margin_start(&self, margin: i32) {
        crate::backends::zork::set_margin_start(self.id, margin);
    }
    pub fn set_margin_top(&self, margin: i32) {
        crate::backends::zork::set_margin_top(self.id, margin);
    }
    pub fn grab_focus(&self) {
        crate::backends::zork::set_focus(self.id);
    }
    pub fn set_can_focus(&self, can: bool) {
        crate::backends::zork::set_can_focus(self.id, can);
    }

    /// Draw immediately against a recording context, ignoring the window
    /// pointer. Returns the recorded ops.
    pub fn force_draw(&self, _window_ptr: *mut c_void, fallback_w: i32, fallback_h: i32) {
        // A canvas with no explicit size uses the caller's fallback, matching
        // the "surface reports zero dimensions" case the other backends handle.
        if crate::backends::zork::canvas_size(self.id) == (0, 0) {
            crate::backends::zork::set_size_request(self.id, fallback_w, fallback_h);
        }
        let _ = self.draw();
    }
}

// -- Overlay --

#[derive(Clone)]
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

impl Overlay {
    /// The base child. Distinct from [`Self::add_overlay`], which stacks a layer
    /// above it — the old body routed both through `set_child`, so the layers
    /// were indistinguishable.
    pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
        crate::backends::zork::set_child(self.id, id_of(child));
    }

    /// Stack a child above the base.
    pub fn add_overlay(&self, child: &impl AsRef<*mut c_void>) {
        crate::backends::zork::overlay_add(self.id, id_of(child));
    }

    pub fn set_overlay_pass_through(&self, child: &impl AsRef<*mut c_void>, pass: bool) {
        crate::backends::zork::overlay_set_pass_through(self.id, id_of(child), pass);
    }

    pub fn remove(&self, child: &impl AsRef<*mut c_void>) {
        crate::backends::zork::overlay_remove(self.id, id_of(child));
    }

    /// The stacked layers, in insertion order.
    pub fn layers(&self) -> Vec<usize> {
        crate::backends::zork::overlay_layers(self.id)
    }

    /// Make the overlay and every descendant visible. The old body was empty.
    pub fn show_all(&self) {
        crate::backends::zork::show_all(self.id);
    }

    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }
}

// -- ScrolledWindow --

#[derive(Clone)]
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

impl ScrolledWindow {
    pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
        crate::backends::zork::set_child(self.id, id_of(child));
    }

    /// GTK policy bits (`GTK_POLICY_ALWAYS` = 0). The model has no scrollbars
    /// to configure, so the flags are recorded nowhere; accepted for parity.
    pub fn set_policy(&self, _hscroll: u32, _vscroll: u32) {}

    /// Drive the scroll position. The value is clamped to
    /// `upper - page` by the model, so a caller cannot scroll past the end.
    pub fn scroll_to(&self, hval: f64, hupper: f64, hpage: f64, vval: f64, vupper: f64, vpage: f64) {
        crate::backends::zork::scroll_to(self.id, hval, hupper, hpage, vval, vupper, vpage);
    }

    /// The current `(h, v)` scroll offsets.
    pub fn get_scroll(&self) -> (f64, f64) {
        crate::backends::zork::get_scroll(self.id)
    }

    /// Register a scroll-notification handler. The old adapter had no
    /// `on_scroll` at all, so `common::ScrolledWindow::on_scroll` could not
    /// compile. A headless viewport never generates a user scroll, so this is
    /// driven explicitly by `Harness::scroll`.
    pub fn on_scroll(&self, cb: Box<dyn FnMut(bool, f64)>) {
        crate::backends::zork::add_scroll_callback(self.id, cb);
    }

    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
}

// -- Fixed --

/// A child positioned at an explicit (x, y) — GTK's `GtkFixed`.
#[derive(Clone)]
pub struct Fixed {
    pub(crate) id: usize,
}

impl Widget for Fixed {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
}

impl AsRef<*mut c_void> for Fixed {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl Fixed {
    pub fn put(&self, child: &impl AsRef<*mut c_void>, x: i32, y: i32) {
        crate::backends::zork::set_offset(id_of(child), x, y);
        crate::backends::zork::append_child(self.id, id_of(child));
    }
    pub fn show_all(&self) {
        crate::backends::zork::show_all(self.id);
    }
    pub fn set_size_request(&self, w: i32, h: i32) {
        crate::backends::zork::set_size_request(self.id, w, h);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }
}

// -- Spreadsheet --

/// A grid-of-cells widget. Backed by the model's per-node cell map, so it is a
/// real data structure rather than a rasterisation shortcut.
#[derive(Clone)]
pub struct Spreadsheet {
    pub(crate) id: usize,
}

impl Widget for Spreadsheet {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
}

impl AsRef<*mut c_void> for Spreadsheet {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl Spreadsheet {
    /// Set a cell's value (1-based row/col).
    pub fn set_cell(&self, row: u32, col: u32, text: &str) {
        crate::backends::zork::sheet_set_cell(self.id, row, col, text, 0);
    }
    pub fn set_raw_cell(&self, row: u32, col: u32, text: &str) {
        crate::backends::zork::sheet_set_raw_cell(self.id, row, col, text);
    }
    pub fn get_cell(&self, row: u32, col: u32) -> Option<String> {
        crate::backends::zork::sheet_get_cell(self.id, row, col)
    }
    pub fn set_cell_style(&self, row: u32, col: u32, style: u8) {
        let text = self.get_cell(row, col).unwrap_or_default();
        crate::backends::zork::sheet_set_cell(self.id, row, col, &text, style);
    }
    pub fn set_border_title(&self, title: &str) {
        crate::backends::zork::sheet_set_border_title(self.id, title);
    }
    pub fn get_border_title(&self) -> Option<String> {
        crate::backends::zork::sheet_border_title(self.id)
    }
    /// A canvas for drawing the grid on top of the cell model. This is what
    /// GTK's and pancurses' `Spreadsheet` expose, so shared painting code
    /// compiles unchanged.
    pub fn canvas(&self) -> Canvas {
        Canvas { id: self.id }
    }
    pub fn overlay(&self) -> Overlay {
        Overlay { id: self.id }
    }
    pub fn queue_redraw(&self) {
        crate::backends::zork::queue_redraw(self.id);
    }
    pub fn set_draw_callback(&self, cb: Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>) {
        crate::backends::zork::set_draw_callback(self.id, cb);
    }
    /// Click-to-edit. The spreadsheet is backed by a real cell map, so this is
    /// the same hook a `Canvas` has — a test can click a cell and assert which
    /// cell the handler saw.
    pub fn on_click(&self, cb: Box<dyn FnMut(f64, f64)>) {
        crate::backends::zork::add_click_hook(self.id, cb);
    }
    pub fn on_click_button(&self, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
        crate::backends::zork::add_click_button_hook(self.id, cb);
    }
    pub fn on_motion(&self, cb: Box<dyn FnMut(f64, f64, u32)>) {
        crate::backends::zork::add_motion_hook(self.id, cb);
    }
    pub fn on_key(&self, cb: Box<dyn FnMut(u32) -> bool>) {
        crate::backends::zork::add_key_hook(self.id, cb);
    }
    pub fn set_hexpand(&self, expand: bool) {
        crate::backends::zork::set_hexpand(self.id, expand);
    }
    pub fn set_vexpand(&self, expand: bool) {
        crate::backends::zork::set_vexpand(self.id, expand);
    }

    // -- Chrome (the F9 surface) --
    //
    // Every one of these is the counterpart of a `pancurses` call
    // `src/gui/pnc_backend.rs` already issues, so shared render code can now
    // target zork without a per-backend `#if`.

    /// The cursor, 0-based. Setting it notifies the cursor-move handlers, so
    /// a host that drives the cursor from Rust gets the same notification a
    /// keypress would produce.
    pub fn set_cursor(&self, row: u32, col: u32) {
        crate::backends::zork::sheet_set_cursor(self.id, row, col);
    }
    pub fn cursor_position(&self) -> Option<(u32, u32)> {
        crate::backends::zork::sheet_cursor(self.id)
    }
    /// The selection anchor a drag extends from.
    pub fn set_anchor(&self, anchor: Option<(u32, u32)>) {
        crate::backends::zork::sheet_set_anchor(self.id, anchor);
    }
    /// Begin/end editing the cursor cell.
    pub fn set_editing(&self, editing: bool, edit_buf: &str, edit_pos: usize) {
        crate::backends::zork::sheet_set_editing(self.id, editing, edit_buf, edit_pos);
    }
    pub fn edit_state(&self) -> Option<(bool, String, usize)> {
        crate::backends::zork::sheet_edit_state(self.id)
    }
    /// Commit the in-progress edit and notify the commit handlers. Returns the
    /// committed `(row, col, text)`, or `None` when nothing was being edited.
    pub fn commit_formula_bar(&self) -> Option<(u32, u32, String)> {
        crate::backends::zork::sheet_commit_edit(self.id)
    }
    /// The gutter width and the number of data columns.
    pub fn set_grid_config(&self, margin_cols: u32, main_cols: u32) {
        crate::backends::zork::sheet_set_grid_config(self.id, margin_cols, main_cols);
    }
    /// The frozen header band and the number of data rows.
    pub fn set_row_counts(&self, header_rows: u32, main_rows: u32) {
        crate::backends::zork::sheet_set_row_counts(self.id, header_rows, main_rows);
    }
    /// Per-column `(index, width_chars, title)`, in display order.
    pub fn set_column_layout(&self, layout: Vec<(u32, u32, String)>) {
        crate::backends::zork::sheet_set_column_layout(self.id, layout);
    }
    pub fn set_row_labels(&self, labels: Vec<(u32, String)>) {
        crate::backends::zork::sheet_set_row_labels(self.id, labels);
    }
    /// The menu-bar line drawn along the top of the sheet.
    pub fn set_menu_text(&self, text: &str) {
        crate::backends::zork::sheet_set_menu_text(self.id, text);
    }
    pub fn menu_text(&self) -> Option<String> {
        crate::backends::zork::sheet_menu_text(self.id)
    }
    /// The status line.
    pub fn set_status_text(&self, text: &str) {
        crate::backends::zork::sheet_set_status_text(self.id, text);
    }
    pub fn status_text(&self) -> Option<String> {
        crate::backends::zork::sheet_status_text(self.id)
    }
    /// The trailing part of the formula bar, kept separate from the entry so a
    /// status note never ends up in the cell the user is editing.
    pub fn set_formula_bar_trailing(&self, text: &str) {
        crate::backends::zork::sheet_set_formula_bar_trailing(self.id, text);
    }
    pub fn formula_bar_trailing(&self) -> Option<String> {
        crate::backends::zork::sheet_formula_bar_trailing(self.id)
    }
    /// The formula bar's address cell and entry text.
    pub fn set_formula_bar(&self, address_label: &Label, entry: &Entry) {
        let address = address_label.get_text().unwrap_or_default();
        let text = entry.get_text().unwrap_or_default();
        crate::backends::zork::sheet_set_formula_bar(self.id, &address, &text);
    }
    pub fn formula_bar(&self) -> Option<(String, String)> {
        crate::backends::zork::sheet_formula_bar(self.id)
    }
    /// The tab strip, and which tab is active. The active index is clamped to
    /// the list, so removing the active tab cannot leave a dangling index.
    pub fn set_tab_data(&self, titles: &[String], active: usize) {
        crate::backends::zork::sheet_set_tab_data(self.id, titles, active);
    }
    /// The whole chrome in one read.
    pub fn chrome(&self) -> Option<crate::backends::zork::model::SheetChrome> {
        crate::backends::zork::sheet_chrome(self.id)
    }

    // -- Input (the F10 surface) --

    /// Register a bare-keypress handler. The free `add_key_callback` below is
    /// the process-wide form every backend has; this is the per-spreadsheet
    /// form, because the model has a table for it and a spreadsheet is the
    /// only widget that has one.
    pub fn add_key_callback(&self, cb: Box<dyn FnMut()>) {
        crate::backends::zork::sheet_add_key_callback(self.id, cb);
    }
    /// Register a "navigated to a cell" handler.
    pub fn add_goto_callback(&self, cb: Box<dyn FnMut()>) {
        crate::backends::zork::sheet_add_goto_callback(self.id, cb);
    }
    /// Register a cursor-moved handler, called as `cb(row, col)`.
    pub fn add_cursor_move_callback(&self, cb: Box<dyn FnMut(u32, u32)>) {
        crate::backends::zork::sheet_add_cursor_move_callback(self.id, cb);
    }
    /// Register a commit handler, called as `cb(row, col, text)`.
    pub fn add_commit_edit_callback(&self, cb: Box<dyn FnMut(u32, u32, String)>) {
        crate::backends::zork::sheet_add_commit_edit_callback(self.id, cb);
    }
    /// `(key, goto, cursor_move, commit_edit)` handler counts.
    pub fn hook_counts(&self) -> Option<(usize, usize, usize, usize)> {
        crate::backends::zork::sheet_hook_counts(self.id)
    }
    /// Fire the "navigated to a cell" handlers, for a host that navigates
    /// without going through [`Self::set_cursor`] (a name typed into the
    /// address box, a jump-to-cell menu action).
    pub fn fire_goto(&self) {
        crate::backends::zork::sheet_fire_goto(self.id);
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

pub fn create_menu() -> Result<Menu, Error> {
    crate::backends::zork::create_menu()
        .map(|id| Menu { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_menubar(model: &Menu, _action_group: *mut c_void) -> Result<MenuBar, Error> {
    crate::backends::zork::create_menubar(model.id, std::ptr::null_mut())
        .map(|id| MenuBar { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_simple_action(name: &str) -> Result<SimpleAction, Error> {
    crate::backends::zork::create_simple_action(name)
        .map(|id| SimpleAction { id, name: name.to_string() })
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
    // A group is named by an id every member shares. The seed supplies it: a
    // seed that is itself ungrouped (group 0) *becomes* the group, so the seed
    // and every button created from it stay mutually exclusive. Without a seed
    // the button is ungrouped and never clears a sibling — the honest
    // behaviour for an independent radio.
    let gid = group.and_then(|r| r.group()).filter(|g| *g != 0);
    let id = match (gid, group) {
        (Some(g), _) => crate::backends::zork::create_radiobutton(Some(g), label),
        (None, Some(seed)) => {
            // Promote the seed to its own group, then join it.
            crate::backends::zork::set_radiobutton_group(seed.id(), seed.id());
            crate::backends::zork::create_radiobutton(Some(seed.id()), label)
        }
        (None, None) => crate::backends::zork::create_radiobutton(None, label),
    };
    id.map(|id| RadioButton { id }).map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_textview() -> Result<TextView, Error> {
    crate::backends::zork::create_textview()
        .map(|id| TextView { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_canvas() -> Result<Canvas, Error> {
    crate::backends::zork::create_canvas()
        .map(|id| Canvas { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_overlay() -> Result<Overlay, Error> {
    crate::backends::zork::create_overlay()
        .map(|id| Overlay { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_scrolled_window() -> Result<ScrolledWindow, Error> {
    crate::backends::zork::create_scrolled_window()
        .map(|id| ScrolledWindow { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_fixed() -> Result<Fixed, Error> {
    crate::backends::zork::create_fixed()
        .map(|id| Fixed { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

pub fn create_spreadsheet() -> Result<Spreadsheet, Error> {
    crate::backends::zork::create_spreadsheet()
        .map(|id| Spreadsheet { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

// -- Application (the action group a menu bar resolves names against) --

/// An action group host, the counterpart of GTK's `GApplication`.
///
/// Zork has no GActionGroup: `create_simple_action` already registers each
/// action's name in the model, which is what a menu item needs in order to
/// dispatch. This type exists so a host that creates an application and adds
/// actions to it — the GTK pattern in `create_application`/`add_action` — has
/// something to call, and so "is this action registered *here*" is answerable.
#[derive(Clone)]
pub struct Application {
    pub(crate) id: usize,
}

impl Widget for Application {
    fn raw_handle(&self) -> *mut c_void {
        &self.id as *const usize as *mut c_void
    }
}

impl AsRef<*mut c_void> for Application {
    fn as_ref(&self) -> &*mut c_void {
        unsafe { &*(&self.id as *const usize as *const *mut c_void) }
    }
}

impl Application {
    /// The node id, for a driver that addresses widgets numerically.
    pub fn id(&self) -> usize {
        self.id
    }

    /// No-op, as on GTK: there is no D-Bus or GApplication registration to
    /// perform, and the model's action registry is already live. It exists
    /// because a host that skips it on GTK and calls it on zork should not
    /// have to be written twice.
    pub fn register(&self) -> Result<(), Error> {
        Ok(())
    }

    /// GTK returns a `GApplication*` for D-Bus/DBus integration. There is no
    /// such object here, so this is the null handle, which every caller that
    /// passes it onward already has to tolerate.
    pub fn as_ptr(&self) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// Register `action` with this application.
    ///
    /// Errors rather than silently ignoring, because GTK errors and a caller
    /// may be relying on that to notice a mistyped action name. The only
    /// refusal is a non-`SimpleAction` argument.
    pub fn add_action(&self, action: &SimpleAction) -> Result<(), Error> {
        crate::backends::zork::application_add_action(self.id, action.id)
            .map(|_| ())
            .ok_or_else(|| Error::Backend(format!("add_action: node {} is not a SimpleAction", action.id)))
    }

    /// The action names registered with this application, in order.
    pub fn actions(&self) -> Vec<String> {
        crate::backends::zork::application_actions(self.id)
    }
}

pub fn create_application() -> Result<Application, Error> {
    crate::backends::zork::create_application()
        .map(|id| Application { id })
        .map_err(|e| Error::Backend(format!("{}", e)))
}

// -- Timers --

/// Run `f` once after `ms` milliseconds of *model* time.
///
/// The headless answer to `g_timeout_add`: the model has no wall clock, so
/// "after `ms` milliseconds" means "when the driver advances the clock past
/// the deadline" — see [`advance_timers`]. A host that never advances the
/// clock registers a timer that never fires, which is the same situation as
/// registering one and then leaving the main loop before it elapses.
pub fn timeout_add_once(ms: u32, f: Box<dyn FnOnce()>) -> Result<(), Error> {
    crate::backends::zork::timeout_add_once(ms, f);
    Ok(())
}

/// Run `f` every `ms` model-milliseconds until it returns `false`.
pub fn timeout_add_repeating(ms: u32, f: Box<dyn FnMut() -> bool>) -> Result<(), Error> {
    crate::backends::zork::timeout_add_repeating(ms, f);
    Ok(())
}

/// Fire every timer due within `ms` of model time; returns how many fired.
///
/// The model's stand-in for a main loop reaching its timeouts. Called
/// automatically at the top of each REPL turn, so a host that installs a
/// timer and hands off to the REPL sees it fire without writing a loop.
pub fn advance_timers(ms: u64) -> usize {
    crate::backends::zork::advance_timers(ms)
}

/// Stop the timer registered under `id`. Returns false for an unknown id.
pub fn cancel_timer(id: usize) -> bool {
    crate::backends::zork::cancel_timer(id)
}

/// How many timers are pending.
pub fn timer_count() -> usize {
    crate::backends::zork::timer_count()
}

// -- Spreadsheet input, the process-wide half (the F10 free functions) --

/// Register a process-wide handler for a keypress.
///
/// The signature matches `pancurses`' free `add_key_callback`, which is also
/// widget-free: a key table cannot be per-widget when the call site has no
/// widget to pass. [`Spreadsheet::add_key_callback`] is the per-spreadsheet
/// form for hosts that do have one.
pub fn add_key_callback(key: char, cb: Box<dyn FnMut()>) {
    crate::backends::zork::add_key_callback(key, cb);
}

/// Fire the handlers registered for `key`; returns how many ran.
pub fn fire_key_callback(key: char) -> usize {
    crate::backends::zork::fire_key_callback(key)
}

/// How many handlers are registered for `key`.
pub fn key_callback_count(key: char) -> usize {
    crate::backends::zork::key_callback_count(key)
}

pub fn spreadsheet_add_goto_callback(id: usize, cb: Box<dyn FnMut()>) {
    crate::backends::zork::sheet_add_goto_callback(id, cb);
}
pub fn spreadsheet_add_cursor_move_callback(id: usize, cb: Box<dyn FnMut(u32, u32)>) {
    crate::backends::zork::sheet_add_cursor_move_callback(id, cb);
}
pub fn spreadsheet_add_commit_edit_callback(id: usize, cb: Box<dyn FnMut(u32, u32, String)>) {
    crate::backends::zork::sheet_add_commit_edit_callback(id, cb);
}
pub fn spreadsheet_fire_goto(id: usize) {
    crate::backends::zork::sheet_fire_goto(id);
}
pub fn spreadsheet_set_cell(id: usize, r: u32, c: u32, text: &str) {
    crate::backends::zork::sheet_set_cell(id, r, c, text, 0);
}
pub fn spreadsheet_set_cell_style(id: usize, r: u32, c: u32, style: u8) {
    // Preserve the text: a style change must not blank the cell. The model's
    // `set_cell` overwrites both text and style together, so the text has to
    // be read back and written again.
    let text = crate::backends::zork::sheet_get_cell(id, r, c).unwrap_or_default();
    crate::backends::zork::sheet_set_cell(id, r, c, &text, style);
}
pub fn spreadsheet_set_column_layout(id: usize, layout: Vec<(u32, u32, String)>) {
    crate::backends::zork::sheet_set_column_layout(id, layout);
}
pub fn spreadsheet_set_border_title(id: usize, text: &str) {
    crate::backends::zork::sheet_set_border_title(id, text);
}
pub fn spreadsheet_set_row_labels(id: usize, labels: Vec<(u32, String)>) {
    crate::backends::zork::sheet_set_row_labels(id, labels);
}
pub fn spreadsheet_set_grid_config(id: usize, margin_cols: u32, main_cols: u32) {
    crate::backends::zork::sheet_set_grid_config(id, margin_cols, main_cols);
}
pub fn spreadsheet_set_row_counts(id: usize, header_rows: u32, main_rows: u32) {
    crate::backends::zork::sheet_set_row_counts(id, header_rows, main_rows);
}
pub fn spreadsheet_set_edit_state(id: usize, editing: bool, edit_buf: &str, edit_pos: usize) {
    crate::backends::zork::sheet_set_editing(id, editing, edit_buf, edit_pos);
}
pub fn spreadsheet_set_cursor(id: usize, row: u32, col: u32) {
    crate::backends::zork::sheet_set_cursor(id, row, col);
}
pub fn spreadsheet_set_status_text(id: usize, text: &str) {
    crate::backends::zork::sheet_set_status_text(id, text);
}
pub fn spreadsheet_set_menu_text(id: usize, text: &str) {
    crate::backends::zork::sheet_set_menu_text(id, text);
}
pub fn spreadsheet_set_tab_data(id: usize, titles: &[String], active: usize) {
    crate::backends::zork::sheet_set_tab_data(id, titles, active);
}
pub fn spreadsheet_set_formula_bar_trailing(id: usize, text: &str) {
    crate::backends::zork::sheet_set_formula_bar_trailing(id, text);
}
pub fn spreadsheet_commit_formula_bar(id: usize) -> Option<(u32, u32, String)> {
    crate::backends::zork::sheet_commit_edit(id)
}
pub fn spreadsheet_cursor_position(id: usize) -> Option<(u32, u32)> {
    crate::backends::zork::sheet_cursor(id)
}
pub fn spreadsheet_edit_state(id: usize) -> Option<(bool, String, usize)> {
    crate::backends::zork::sheet_edit_state(id)
}

// -- Diagnostics --

/// Write the widget tree to `path`. See
/// [`crate::backends::zork::debug_dump_tree`]; returns the node count written,
/// or 0 if the path could not be written.
pub fn debug_dump_native_tree(root: *mut c_void, path: &str) {
    // The NWG signature takes a root HWND because that is what it walks. Zork
    // addresses widgets by an integer id in a `usize` handed out through
    // `raw_handle`, so the pointer is read back as the node id and only used
    // when it names a real node. Passing null dumps the whole tree from its
    // roots, which is what a caller that has no particular node in mind wants.
    let root_id = if root.is_null() {
        None
    } else {
        Some(root as usize)
    };
    if let Some(id) = root_id {
        if crate::backends::zork::node_exists(id) {
            let _ = crate::backends::zork::debug_dump_tree_from(id, path);
            return;
        }
    }
    let _ = crate::backends::zork::debug_dump_tree(path);
}

/// Mark `id` and every descendant as needing a redraw. Returns how many nodes
/// were reached.
pub fn window_queue_redraw_cascades_to_children(id: usize) -> usize {
    crate::backends::zork::queue_redraw_cascading(id)
}

/// Whether `id` names a live node. Used by [`debug_dump_native_tree`] to tell
/// a real root pointer from a null one.
pub fn node_exists(id: usize) -> bool {
    crate::backends::zork::node_exists(id)
}

// -- Event-loop control --

/// Quit the running app. The REPL owns its loop, so there is no loop pointer to
/// publish; this sets the model's `running` flag, which the REPL polls.
pub fn quit() {
    crate::backends::zork::quit();
}

/// Whether the backend is still running.
pub fn is_running() -> bool {
    crate::backends::zork::is_running()
}

// -- File dialogs --

/// A headless backend has no file chooser, so this always reports "cancelled".
/// See `docs/ZORK_MISSING.md` §5 for why a real path needs a terminal dialog.
pub fn open_file(_title: &str) -> Result<Option<String>, Error> {
    Ok(None)
}

pub fn save_file(_title: &str) -> Result<Option<String>, Error> {
    Ok(None)
}

/// `core::App::open_file_filtered`/`save_file_filtered` route here; the filters
/// are meaningless without a chooser.
pub fn open_file_filtered(_title: &str, _filters: &[(&str, &[&str])]) -> Result<Option<String>, Error> {
    Ok(None)
}

pub fn save_file_filtered(_title: &str, _filters: &[(&str, &[&str])], _current_name: &str) -> Result<Option<String>, Error> {
    Ok(None)
}
