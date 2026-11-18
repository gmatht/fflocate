//! Backwards-compatible free-function facade over [`super::model`].
//!
//! The per-widget adapter ([`crate::backends_zork_adapter`]) was written against
//! the old `backends::zork::*` free functions, which mutably operated on a
//! thread-local model. This module re-exposes those names (each delegating to
//! [`super::model::with_state`]) so the adapter is unchanged. New code should use
//! the typed [`super::harness`] instead.
//!
//! The model grew a property bag ([`super::model::ZorkProps`]) and six new
//! node kinds, so the surface here is much wider than the original
//! label/button/entry set: every property the GUI adapters expose has a
//! free function so the adapter shim stays a thin `id`-passing layer.

use std::os::raw::c_void;

pub use super::model::{MenuItemData, MenuItemKind, ZorkKind};
use super::model::{put_callbacks, with_callbacks, with_state, ZorkState};

pub type Callback = Box<dyn FnMut()>;

// -- Factories --

pub fn create_window() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_window()))
}
pub fn create_button(label: &str) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_button(label)))
}
pub fn create_label(text: &str) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_label(text)))
}
pub fn create_box(horizontal: bool, spacing: i32) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_box(horizontal, spacing)))
}
pub fn create_grid() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_grid()))
}
pub fn create_entry() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_entry()))
}
pub fn create_menu() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_menu()))
}
pub fn create_simple_action(name: &str) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_simple_action(name)))
}
pub fn create_menubar(model_id: usize, action_group: *mut c_void) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_menubar(model_id, action_group)))
}
pub fn create_dialog() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_dialog()))
}
pub fn create_dropdown(items: &[&str]) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_dropdown(items)))
}
pub fn create_checkbutton(label: &str) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_checkbutton(label)))
}
pub fn create_radiobutton(group_id: Option<usize>, label: &str) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_radiobutton(group_id, label)))
}
pub fn create_textview() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_textview()))
}
pub fn create_canvas() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_canvas()))
}
pub fn create_overlay() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_overlay()))
}
pub fn create_scrolled_window() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_scrolled_window()))
}
pub fn create_fixed() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_fixed()))
}
pub fn create_application() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_application()))
}
pub fn create_spreadsheet() -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    Ok(with_state(|s| s.create_spreadsheet()))
}

// -- Menu model --

pub fn menu_append(menu_id: usize, label: &str, action: &str) {
    with_state(|s| s.menu_append(menu_id, label, action));
}
pub fn menu_append_submenu(menu_id: usize, label: &str, submenu_id: usize) {
    with_state(|s| s.menu_append_submenu(menu_id, label, submenu_id));
}
pub fn menu_append_with_shortcut(menu_id: usize, label: &str, action: &str, accelerator: &str) {
    with_state(|s| s.menu_append_with_shortcut(menu_id, label, action, accelerator));
}
pub fn menu_append_section(menu_id: usize, label: &str) {
    with_state(|s| s.menu_append_section(menu_id, label));
}
pub fn menu_append_separator(menu_id: usize, label: &str) {
    with_state(|s| s.menu_append_separator(menu_id, label));
}
pub fn menu_append_check(menu_id: usize, label: &str, action: &str, checked: bool) -> usize {
    with_state(|s| s.menu_append_check(menu_id, label, action, checked))
}
pub fn menu_append_radio(menu_id: usize, label: &str, action: &str, group: usize, checked: bool) -> usize {
    with_state(|s| s.menu_append_radio(menu_id, label, action, group, checked))
}
pub fn set_menu_item_checked(menu_id: usize, index: usize, checked: bool) {
    with_state(|s| s.set_menu_item_checked(menu_id, index, checked));
}
pub fn menu_items(menu_id: usize) -> Vec<MenuItemData> {
    with_state(|s| s.menu_items.get(&menu_id).cloned().unwrap_or_default())
}
/// Select a menu item by 0-based index and dispatch the action it names.
/// Returns the item's label, or `None` when the index is invalid or the item
/// is a non-selectable separator/section.
pub fn menu_select(menu_id: usize, index: usize) -> Option<String> {
    with_state(|s| s.menu_select(menu_id, index))
}
pub fn activate_action(action_name: &str) -> bool {
    with_state(|s| s.activate_action(action_name))
}

// -- Text / value setters and getters --

pub fn set_window_title(id: usize, title: &str) {
    with_state(|s| s.set_window_title(id, title));
}
pub fn get_window_title(id: usize) -> Option<String> {
    with_state(|s| s.get_window_title(id))
}
pub fn set_label_text(id: usize, text: &str) {
    with_state(|s| s.set_label_text(id, text));
}
pub fn get_label_text(id: usize) -> Option<String> {
    with_state(|s| s.get_label_text(id))
}
pub fn set_button_label(id: usize, label: &str) {
    with_state(|s| s.set_button_label(id, label));
}
pub fn get_button_label(id: usize) -> Option<String> {
    with_state(|s| s.get_button_label(id))
}
pub fn add_callback(id: usize, cb: Callback) {
    with_state(|s| s.add_callback(id, cb));
}
pub fn set_entry_text(id: usize, text: &str) {
    with_state(|s| s.set_entry_text(id, text));
}
pub fn get_entry_text(id: usize) -> Option<String> {
    with_state(|s| s.get_entry_text(id))
}
pub fn get_entry_position(id: usize) -> Option<usize> {
    with_state(|s| s.get_entry_position(id))
}
pub fn set_entry_position(id: usize, pos: usize) {
    with_state(|s| s.set_entry_position(id, pos));
}
pub fn set_textview_text(id: usize, text: &str) {
    with_state(|s| s.set_textview_text(id, text));
}
pub fn get_textview_text(id: usize) -> Option<String> {
    with_state(|s| s.get_textview_text(id))
}
pub fn append_textview_text(id: usize, text: &str) {
    with_state(|s| s.append_textview_text(id, text));
}
pub fn set_editable(id: usize, editable: bool) {
    with_state(|s| s.set_editable(id, editable));
}
pub fn get_editable(id: usize) -> bool {
    with_state(|s| s.get_editable(id))
}
pub fn set_dropdown_items(id: usize, items: &[&str]) {
    with_state(|s| s.set_dropdown_items(id, items));
}
pub fn set_dropdown_selected(id: usize, idx: i32) {
    with_state(|s| s.set_dropdown_selected(id, idx));
}
pub fn get_dropdown_selected(id: usize) -> i32 {
    with_state(|s| s.get_dropdown_selected(id))
}
pub fn get_checkbutton_checked(id: usize) -> bool {
    with_state(|s| s.get_checkbutton_checked(id))
}
pub fn get_radiobutton_checked(id: usize) -> bool {
    with_state(|s| s.get_radiobutton_checked(id))
}
pub fn set_checkbutton_checked(id: usize, checked: bool) {
    with_state(|s| s.set_checkbutton_checked(id, checked));
}
/// Join a radio button to `group`. Group `0` is "ungrouped": such a button
/// never clears a sibling.
pub fn set_radiobutton_group(id: usize, group_id: usize) {
    with_state(|s| s.set_radiobutton_group(id, group_id));
}
pub fn radiobutton_group(id: usize) -> Option<usize> {
    with_state(|s| s.radiobutton_group(id))
}
pub fn set_radiobutton_checked(id: usize, checked: bool) {
    with_state(|s| s.set_radiobutton_checked(id, checked));
}

// -- Tree --

pub fn set_child(parent_id: usize, child_id: usize) {
    with_state(|s| s.set_child(parent_id, child_id));
}
pub fn append_child(parent_id: usize, child_id: usize) {
    with_state(|s| s.append_child(parent_id, child_id));
}
pub fn remove_child(parent_id: usize, child_id: usize) {
    with_state(|s| s.remove_child(parent_id, child_id));
}
/// Lay a box out at `(x, y, w, h)`, then fire its layout handlers.
///
/// Two borrows on purpose: the positioning pass is a pure mutation, and the
/// handlers are user closures that may re-enter the model (a layout callback
/// that resizes a child is ordinary). See [`ZorkState::layout_box`].
pub fn layout_box(id: usize, x: i32, y: i32, w: i32, h: i32) {
    with_state(|s| s.layout_box(id, x, y, w, h));
    fire_layout_callbacks(id, w, h);
}
/// Fire `id`'s layout handlers with `(w, h)`.
pub fn fire_layout_callbacks(id: usize, w: i32, h: i32) {
    let mut cbs = with_state(|s| ZorkState::take_layout_cbs(s, id));
    for cb in cbs.iter_mut() {
        cb(w, h);
    }
    // `put_layout_cbs` *replaces*, so re-insert the same set. A handler that
    // registered a new one during the call would be lost — put the originals
    // back first and re-add the extra.
    let extra = with_state(|s| ZorkState::take_layout_cbs(s, id));
    with_state(|s| ZorkState::put_layout_cbs(s, id, cbs));
    with_state(|s| ZorkState::put_layout_cbs_add(s, id, extra));
}
/// Register a "was (re)laid out" handler, called as `cb(w, h)`.
pub fn set_layout_callback(id: usize, cb: Box<dyn FnMut(i32, i32)>) {
    with_state(|s| s.set_layout_callback(id, cb));
}
/// How many layout handlers a node has.
pub fn layout_callback_count(id: usize) -> usize {
    with_state(|s| s.layout_callback_count(id))
}
pub fn measure_box(id: usize) -> Option<(i32, i32)> {
    with_state(|s| s.measure_box(id))
}
pub fn grid_attach(grid_id: usize, child_id: usize, left: i32, top: i32, width: i32, height: i32) {
    with_state(|s| s.grid_attach(grid_id, child_id, left, top, width, height));
}

// -- Geometry --

pub fn set_offset(id: usize, x: i32, y: i32) {
    with_state(|s| s.set_offset(id, x, y));
}
pub fn get_offset(id: usize) -> Option<(Option<i32>, Option<i32>)> {
    with_state(|s| s.get_offset(id))
}
pub fn set_size_request(id: usize, w: i32, h: i32) {
    with_state(|s| s.set_size_request(id, w, h));
}
pub fn get_size_request(id: usize) -> Option<(Option<i32>, Option<i32>)> {
    with_state(|s| s.get_size_request(id))
}

// -- Presentation properties --

pub fn set_visible(id: usize, visible: bool) {
    with_state(|s| s.set_visible(id, visible));
}
pub fn get_visible(id: usize) -> bool {
    with_state(|s| s.get_visible(id))
}
pub fn show_all(id: usize) {
    with_state(|s| s.show_all(id));
}
pub fn set_hexpand(id: usize, v: bool) {
    with_state(|s| s.set_hexpand(id, v));
}
pub fn set_vexpand(id: usize, v: bool) {
    with_state(|s| s.set_vexpand(id, v));
}
pub fn set_child_hexpand(parent_id: usize, child_id: usize, v: bool) {
    with_state(|s| s.set_child_hexpand(parent_id, child_id, v));
}
pub fn set_child_vexpand(parent_id: usize, child_id: usize, v: bool) {
    with_state(|s| s.set_child_vexpand(parent_id, child_id, v));
}
pub fn set_margin_start(id: usize, px: i32) {
    with_state(|s| s.set_margin_start(id, px));
}
pub fn set_margin_top(id: usize, px: i32) {
    with_state(|s| s.set_margin_top(id, px));
}
pub fn add_class(id: usize, class_name: &str) {
    with_state(|s| s.add_class(id, class_name));
}
pub fn remove_class(id: usize, class_name: &str) {
    with_state(|s| s.remove_class(id, class_name));
}
pub fn classes(id: usize) -> Vec<String> {
    with_state(|s| s.classes(id))
}
pub fn has_class(id: usize, class_name: &str) -> bool {
    with_state(|s| s.has_class(id, class_name))
}
pub fn set_xalign(id: usize, x: f32) {
    with_state(|s| s.set_xalign(id, x));
}
pub fn set_halign(id: usize, align: i32) {
    with_state(|s| s.set_halign(id, align));
}
pub fn set_valign(id: usize, align: i32) {
    with_state(|s| s.set_valign(id, align));
}
pub fn set_fixed_width(id: usize, w: Option<i32>) {
    with_state(|s| s.set_fixed_width(id, w));
}
/// Record a font family + size, plus the style bits the GTK adapter passes
/// through. See [`super::model::font_style`].
pub fn set_font_style(id: usize, family: &str, size: f64, style_bits: i32) {
    let mut parts: Vec<&str> = Vec::new();
    if style_bits & super::model::font_style::BOLD != 0 {
        parts.push("bold");
    }
    if style_bits & super::model::font_style::ITALIC != 0 {
        parts.push("italic");
    }
    if style_bits & super::model::font_style::UNDERLINE != 0 {
        parts.push("underline");
    }
    if style_bits & super::model::font_style::STRIKETHROUGH != 0 {
        parts.push("strikethrough");
    }
    let family = if parts.is_empty() { family.to_string() } else { format!("{} {}", parts.join(" "), family) };
    with_state(|s| s.set_font_style(id, Some(&family), size));
}

// -- Focus --

pub fn set_focus(id: usize) {
    with_state(|s| s.set_focus(id));
}
pub fn get_focus() -> Option<usize> {
    with_state(|s| s.get_focus())
}
pub fn has_focus(id: usize) -> bool {
    with_state(|s| s.has_focus(id))
}
pub fn set_can_focus(id: usize, can: bool) {
    with_state(|s| s.set_can_focus(id, can));
}

// -- Scroll --

pub fn scroll_to(id: usize, hval: f64, hupper: f64, hpage: f64, vval: f64, vupper: f64, vpage: f64) {
    with_state(|s| s.set_scroll(id, hval, hupper, hpage, vval, vupper, vpage));
}
pub fn get_scroll(id: usize) -> (f64, f64) {
    with_state(|s| s.get_scroll(id))
}

// -- Canvas / drawing --

pub fn set_draw_callback(id: usize, cb: Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>) {
    with_state(|s| s.set_draw_callback(id, cb));
}
pub fn clear_draw_callback(id: usize) {
    with_state(|s| s.clear_draw_callback(id));
}
pub fn has_draw_callback(id: usize) -> bool {
    with_state(|s| s.has_draw_callback(id))
}
pub fn queue_redraw(id: usize) {
    with_state(|s| s.queue_redraw(id));
}
pub fn redraw_count(id: usize) -> u32 {
    with_state(|s| s.redraw_count(id))
}
pub fn canvas_size(id: usize) -> (i32, i32) {
    with_state(|s| s.canvas_size(id))
}
/// Run the canvas's draw callback against a recording context and return the
/// recorded ops. This is the model-level `Canvas::force_draw` equivalent.
pub fn draw_canvas(id: usize) -> Vec<crate::backends::headless::DrawOp> {
    // The draw callback is the most re-entrant one there is: corro's calls
    // `scroll_to` on every frame from inside the paint. So it is lifted out,
    // run with the model borrow released, and put back.
    let mut ctx = crate::backends::headless::RecordingDrawContext::new();
    // `take_draw` moves the callback into the slot; run it from *inside* the
    // slot so the callback is put back afterwards. Calling `slot.take()`
    // first and putting the emptied slot back would discard the callback
    // permanently — the canvas would draw exactly once.
    let mut size = (0, 0);
    let mut ran = false;
    {
        let mut slot = with_callbacks(ZorkState::take_draw, id);
        if let Some((mut cb, w, h)) = slot.take() {
            size = (w, h);
            ran = true;
            cb(&mut ctx, w, h);        // <- model borrow is NOT held here
            slot.put_back((cb, w, h));
        }
        put_callbacks(ZorkState::put_draw, id, slot);
    }
    let _ = (size, ran);
    ctx.ops
}

// -- Pointer / key --

pub fn add_click_hook(id: usize, cb: Box<dyn FnMut(f64, f64)>) {
    with_state(|s| s.add_click_hook(id, cb));
}
pub fn add_click_button_hook(id: usize, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
    with_state(|s| s.add_click_button_hook(id, cb));
}
pub fn add_motion_hook(id: usize, cb: Box<dyn FnMut(f64, f64, u32)>) {
    with_state(|s| s.add_motion_hook(id, cb));
}
pub fn add_release_hook(id: usize, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
    with_state(|s| s.add_release_hook(id, cb));
}
pub fn add_key_hook(id: usize, cb: Box<dyn FnMut(u32) -> bool>) {
    with_state(|s| s.add_key_hook(id, cb));
}
pub fn add_key_raw_hook(id: usize, cb: Box<dyn FnMut(u32, u32) -> bool>) {
    with_state(|s| s.add_key_raw_hook(id, cb));
}
/// Register a gesture handler, called as `cb(Gesture)`. The canonical pointer
/// input: one callback for hover, drag, buttons and scroll, which
/// [`crate::core::DragTracker`] turns into a drag without each caller
/// reassembling one from three separate hooks.
pub fn add_gesture_hook(id: usize, cb: Box<dyn FnMut(crate::core::Gesture)>) {
    with_state(|s| s.add_gesture_hook(id, cb));
}
/// How many gesture handlers a node has.
pub fn gesture_hook_count(id: usize) -> usize {
    with_state(|s| s.gesture_hook_count(id))
}
/// Fire a gesture at `id` with the borrow released, so a handler may re-enter.
pub fn fire_gesture(id: usize, g: crate::core::Gesture) {
    let mut cbs = with_callbacks(ZorkState::take_gesture_hooks, id);
    for cb in cbs.iter_mut() {
        cb(g);
    }
    put_callbacks(ZorkState::put_gesture_hooks, id, cbs);
}
pub fn set_screen_origin(id: usize, origin: Option<(i32, i32)>) {
    with_state(|s| s.set_screen_origin(id, origin));
}
pub fn get_screen_origin(id: usize) -> Option<(i32, i32)> {
    with_state(|s| s.node(id).and_then(|n| n.pointer.screen_origin))
}
pub fn pointer_click(id: usize, x: f64, y: f64) {
    let mut cbs = with_callbacks(ZorkState::take_click_hooks, id);
    for cb in cbs.iter_mut() {
        cb(x, y);                        // <- model borrow is NOT held here
    }
    put_callbacks(ZorkState::put_click_hooks, id, cbs);
    fire(id);
}
pub fn pointer_click_button(id: usize, x: f64, y: f64, button: u32, state: u32) {
    let mut cbs = with_callbacks(ZorkState::take_click_button_hooks, id);
    for cb in cbs.iter_mut() {
        cb(x, y, button, state);         // <- model borrow is NOT held here
    }
    put_callbacks(ZorkState::put_click_button_hooks, id, cbs);
    // A press is the gesture stream's "the drag starts here"; recorded before
    // the plain click so a gesture handler sees the button already down.
    with_state(|s| s.node_mut(id).map(|n| n.pointer.down_button = Some(ZorkState::gesture_button(button))));
    fire_gesture(
        id,
        crate::core::Gesture::Button {
            button: ZorkState::gesture_button(button),
            pressed: true,
            x,
            y,
            mods: ZorkState::gesture_modifiers(state),
        },
    );
    pointer_click(id, x, y);
}
pub fn pointer_motion(id: usize, x: f64, y: f64, state: u32) {
    let mut cbs = with_callbacks(ZorkState::take_motion_hooks, id);
    for cb in cbs.iter_mut() {
        cb(x, y, state);
    }
    put_callbacks(ZorkState::put_motion_hooks, id, cbs);
    // Drag when a button is held, hover when none is — the distinction the raw
    // hook cannot express, and the one `DragTracker` needs.
    let down = with_state(|s| s.gesture_button_down(id));
    let g = match down {
        Some(_) => crate::core::Gesture::Drag { x, y, mods: ZorkState::gesture_modifiers(state) },
        None => crate::core::Gesture::Hover { x, y, mods: ZorkState::gesture_modifiers(state) },
    };
    fire_gesture(id, g);
}
pub fn pointer_release(id: usize, x: f64, y: f64, button: u32, state: u32) {
    let mut cbs = with_callbacks(ZorkState::take_release_hooks, id);
    for cb in cbs.iter_mut() {
        cb(x, y, button, state);
    }
    put_callbacks(ZorkState::put_release_hooks, id, cbs);
    fire_gesture(
        id,
        crate::core::Gesture::Button {
            button: ZorkState::gesture_button(button),
            pressed: false,
            x,
            y,
            mods: ZorkState::gesture_modifiers(state),
        },
    );
    // Cleared after firing, so a handler inspecting state during the release
    // still sees which button it was.
    with_state(|s| s.node_mut(id).map(|n| n.pointer.down_button = None));
}
/// Deliver a scroll to the gesture stream as a `Gesture::Scroll`, where `dx`/
/// `dy` are *deltas*. A widget's own `on_scroll` keeps its `(vertical, pos)`
/// position shape; the two are separate because conflating them is the mistake
/// the `ScrollDelta` type exists to prevent.
pub fn pointer_scroll(id: usize, dx: f64, dy: f64, x: f64, y: f64, state: u32) {
    fire_gesture(
        id,
        crate::core::Gesture::Scroll {
            delta: crate::core::ScrollDelta { dx, dy },
            x,
            y,
            mods: ZorkState::gesture_modifiers(state),
        },
    );
}
pub fn key(id: usize, keyval: u32) -> bool {
    with_state(|s| s.key(id, keyval))
}

// -- Scroll callbacks --

pub fn add_scroll_callback(id: usize, cb: Box<dyn FnMut(bool, f64)>) {
    with_state(|s| s.add_scroll_callback(id, cb));
}
/// Drive a `ScrolledWindow`'s scroll handler as if the user had scrolled.
/// `vertical` selects the axis and `pos` is the new value. A headless viewport
/// never produces a real scroll, so this is how the handler gets exercised.
pub fn scroll(id: usize, vertical: bool, pos: f64) {
    with_state(|s| s.set_scroll_pos(id, vertical, pos));
    let mut cbs = with_callbacks(ZorkState::take_scroll_hooks, id);
    for cb in cbs.iter_mut() {
        cb(vertical, pos);                // <- model borrow is NOT held here
    }
    put_callbacks(ZorkState::put_scroll_hooks, id, cbs);
}

// -- Dialog response callbacks --

/// Register a dialog response handler. The handler receives the response id.
pub fn add_response_callback(id: usize, cb: Box<dyn FnMut(i32)>) {
    with_state(|s| s.add_response_callback(id, cb));
}

// -- Tree queries --

/// The first child of `id`, if any.
pub fn first_child(id: usize) -> Option<usize> {
    with_state(|s| s.node(id).and_then(|n| n.children.first().copied()))
}
/// The grid's `(cols, rows)` as grown by `grid_attach`.
pub fn grid_dimensions(id: usize) -> (usize, usize) {
    with_state(|s| match s.node(id) {
        Some(n) => match n.kind {
            super::model::ZorkKind::Grid { cols, rows } => (cols, rows),
            _ => (0, 0),
        },
        None => (0, 0),
    })
}
/// Whether a dialog has been closed (`Dialog::close`).
pub fn is_destroyed(id: usize) -> bool {
    with_state(|s| s.node(id).map(|n| n.destroyed).unwrap_or(false))
}
pub fn menu_active(id: usize) -> bool {
    with_state(|s| s.menu_active() || s.node(id).is_some_and(|n| n.overlays.len() > 1))
}

/// Fire a node's callbacks (`Button::emit_clicked`, `Harness::click`).
///
/// Re-entrancy-safe: the callbacks are lifted out and the borrow released
/// before they run, so a handler may call back into the model (e.g. update a
/// label) without hitting "RefCell already borrowed".
pub fn fire(id: usize) {
    let mut cbs = with_callbacks(ZorkState::take_callbacks, id);
    for cb in cbs.iter_mut() {
        cb();                            // <- model borrow is NOT held here
    }
    put_callbacks(ZorkState::put_callbacks, id, cbs);
}
/// Flip a `CheckButton`/`RadioButton` and fire its callbacks.
pub fn toggle(id: usize) {
    with_state(|s| s.toggle(id));
}

// -- Overlay --

pub fn overlay_add(overlay_id: usize, child_id: usize) {
    with_state(|s| s.overlay_add(overlay_id, child_id));
}
pub fn overlay_remove(overlay_id: usize, child_id: usize) {
    with_state(|s| s.remove_child(overlay_id, child_id));
}
pub fn overlay_set_pass_through(overlay_id: usize, child_id: usize, pass: bool) {
    with_state(|s| s.overlay_set_pass_through(overlay_id, child_id, pass));
}
pub fn overlay_layers(id: usize) -> Vec<usize> {
    with_state(|s| s.overlay_layers(id))
}

// -- Dialog --

pub fn dialog_add_button(id: usize, label: &str, response_id: i32) {
    with_state(|s| s.dialog_add_button(id, label, response_id));
}
pub fn dialog_set_default_response(id: usize, response_id: i32) {
    with_state(|s| s.dialog_set_default_response(id, response_id));
}
pub fn dialog_buttons(id: usize) -> Vec<(String, i32)> {
    with_state(|s| s.dialog_buttons(id))
}
/// The dialog's content extent: the union of its children's size requests.
/// `None` when the dialog has no children.
pub fn measure_dialog(id: usize) -> Option<(i32, i32)> {
    with_state(|s| s.measure_dialog(id))
}
/// The response id `Dialog::run` returns: the default response, or `None`.
pub fn dialog_default_response(id: usize) -> Option<i32> {
    with_state(|s| s.dialog_default_response(id))
}
pub fn dialog_set_transient_for(id: usize, parent: Option<usize>) {
    with_state(|s| s.dialog_set_transient_for(id, parent));
}
pub fn dialog_transient_for(id: usize) -> Option<usize> {
    with_state(|s| s.dialog_transient_for(id))
}
pub fn dialog_mark_destroyed(id: usize) {
    with_state(|s| s.dialog_mark_destroyed(id));
}
/// Dismiss a dialog with no button, reporting its default response id.
///
/// See [`ZorkState::dialog_dismiss`]: a dismissal that carries no button has
/// no `dialog_add_button` entry to match, so it goes through the recorded
/// default instead.
pub fn dialog_dismiss(id: usize) {
    let response = with_state(|s| s.node(id).and_then(|n| n.dialog_default_response)).unwrap_or(0);
    dialog_respond(id, response);
}

/// Deliver a response id to a dialog's `connect_response` handler. Only a
/// response registered with `dialog_add_button` fires.
pub fn dialog_respond(id: usize, response_id: i32) {
    if !with_state(|s| s.dialog_has_response(id, response_id)) {
        return;
    }
    let mut cbs = with_callbacks(ZorkState::take_response_hooks, id);
    for cb in cbs.iter_mut() {
        cb(response_id);                  // <- model borrow is NOT held here
    }
    put_callbacks(ZorkState::put_response_hooks, id, cbs);
}

// -- Spreadsheet --

pub fn sheet_set_cell(id: usize, row: u32, col: u32, text: &str, style: u8) {
    with_state(|s| s.sheet_set_cell(id, row, col, text, style));
}
pub fn sheet_set_raw_cell(id: usize, row: u32, col: u32, text: &str) {
    with_state(|s| s.sheet_set_raw_cell(id, row, col, text));
}
pub fn sheet_get_cell(id: usize, row: u32, col: u32) -> Option<String> {
    with_state(|s| s.sheet_get_cell(id, row, col))
}
pub fn sheet_set_border_title(id: usize, title: &str) {
    with_state(|s| s.sheet_set_border_title(id, title));
}
pub fn sheet_border_title(id: usize) -> Option<String> {
    with_state(|s| s.sheet_border_title(id))
}

// -- Spreadsheet chrome --
//
// Plain model mutations, so `with_state` is right: none of them fires a user
// closure. Only `sheet_commit_edit` needs the two-phase dance, because it
// notifies the commit handlers afterwards.

pub fn sheet_chrome(id: usize) -> Option<super::model::SheetChrome> {
    with_state(|s| s.sheet_chrome(id))
}
pub fn sheet_set_menu_text(id: usize, text: &str) {
    with_state(|s| s.sheet_set_menu_text(id, text));
}
pub fn sheet_menu_text(id: usize) -> Option<String> {
    with_state(|s| s.sheet_menu_text(id))
}
pub fn sheet_set_status_text(id: usize, text: &str) {
    with_state(|s| s.sheet_set_status_text(id, text));
}
pub fn sheet_status_text(id: usize) -> Option<String> {
    with_state(|s| s.sheet_status_text(id))
}
pub fn sheet_set_formula_bar_trailing(id: usize, text: &str) {
    with_state(|s| s.sheet_set_formula_bar_trailing(id, text));
}
pub fn sheet_formula_bar_trailing(id: usize) -> Option<String> {
    with_state(|s| s.sheet_formula_bar_trailing(id))
}
pub fn sheet_set_formula_bar(id: usize, address: &str, entry: &str) {
    with_state(|s| s.sheet_set_formula_bar(id, address, entry));
}
pub fn sheet_formula_bar(id: usize) -> Option<(String, String)> {
    with_state(|s| s.sheet_formula_bar(id))
}
pub fn sheet_set_grid_config(id: usize, margin_cols: u32, main_cols: u32) {
    with_state(|s| s.sheet_set_grid_config(id, margin_cols, main_cols));
}
pub fn sheet_set_row_counts(id: usize, header_rows: u32, main_rows: u32) {
    with_state(|s| s.sheet_set_row_counts(id, header_rows, main_rows));
}
pub fn sheet_set_column_layout(id: usize, layout: Vec<(u32, u32, String)>) {
    with_state(|s| s.sheet_set_column_layout(id, layout));
}
pub fn sheet_set_row_labels(id: usize, labels: Vec<(u32, String)>) {
    with_state(|s| s.sheet_set_row_labels(id, labels));
}
pub fn sheet_set_tab_data(id: usize, titles: &[String], active: usize) {
    with_state(|s| s.sheet_set_tab_data(id, titles, active));
}
/// Move the cursor **and** notify the cursor-move handlers.
///
/// Two steps, two borrows: the clamp is a pure mutation, and the notification
/// runs with nothing held so a handler may read the new cursor, write a cell,
/// or move the cursor again without tripping over the cell. Doing both inside
/// one `with_state` would make the handler see the model already borrowed.
pub fn sheet_set_cursor(id: usize, row: u32, col: u32) {
    with_state(|s| s.sheet_set_cursor(id, row, col));
    sheet_fire_cursor_move(id);
}
/// Every set cell as `(row, col, text)`, in address order.
pub fn sheet_cells(id: usize) -> Vec<(u32, u32, String)> {
    with_state(|s| s.sheet_cells(id))
}
/// How many cells are set on a spreadsheet.
pub fn sheet_cell_count(id: usize) -> usize {
    with_state(|s| s.sheet_cell_count(id))
}
pub fn sheet_cursor(id: usize) -> Option<(u32, u32)> {
    with_state(|s| s.sheet_cursor(id))
}
pub fn sheet_set_anchor(id: usize, anchor: Option<(u32, u32)>) {
    with_state(|s| s.sheet_set_anchor(id, anchor));
}
pub fn sheet_set_editing(id: usize, editing: bool, edit_buf: &str, edit_pos: usize) {
    with_state(|s| s.sheet_set_editing(id, editing, edit_buf, edit_pos));
}
pub fn sheet_edit_state(id: usize) -> Option<(bool, String, usize)> {
    with_state(|s| s.sheet_edit_state(id))
}

/// Commit the in-progress edit and notify the commit handlers.
///
/// The mutation happens first, under one borrow; the notification under a
/// second, released one. A handler therefore reads a model that is already
/// consistent — which is what `add_commit_edit_callback` promises, and what
/// `core::Grid::commit_edit` does (it writes the cells and the app reads them
/// afterwards).
pub fn sheet_commit_edit(id: usize) -> Option<(u32, u32, String)> {
    let committed = with_state(|s| s.sheet_commit_edit(id))?;
    let mut cbs = with_callbacks(ZorkState::take_sheet_commit_hooks, id);
    for cb in cbs.iter_mut() {
        cb(committed.0, committed.1, committed.2.clone());
    }
    put_callbacks(ZorkState::put_sheet_commit_hooks, id, cbs);
    Some(committed)
}

// -- Spreadsheet input hooks --

pub fn sheet_add_key_callback(id: usize, cb: Box<dyn FnMut()>) {
    with_state(|s| s.sheet_add_key_callback(id, cb));
}
pub fn sheet_add_goto_callback(id: usize, cb: Box<dyn FnMut()>) {
    with_state(|s| s.sheet_add_goto_callback(id, cb));
}
pub fn sheet_add_cursor_move_callback(id: usize, cb: Box<dyn FnMut(u32, u32)>) {
    with_state(|s| s.sheet_add_cursor_move_callback(id, cb));
}
pub fn sheet_add_commit_edit_callback(id: usize, cb: Box<dyn FnMut(u32, u32, String)>) {
    with_state(|s| s.sheet_add_commit_edit_callback(id, cb));
}
/// `(key, goto, cursor_move, commit_edit)` handler counts.
pub fn sheet_hook_counts(id: usize) -> Option<(usize, usize, usize, usize)> {
    with_state(|s| s.sheet_hook_counts(id))
}

/// Fire the "navigated to a cell" handlers.
pub fn sheet_fire_goto(id: usize) {
    let mut cbs = with_callbacks(ZorkState::take_sheet_goto_hooks, id);
    for cb in cbs.iter_mut() {
        cb();
    }
    put_callbacks(ZorkState::put_sheet_goto_hooks, id, cbs);
}

/// Fire the bare-keypress handlers.
pub fn sheet_fire_key(id: usize) {
    let mut cbs = with_callbacks(ZorkState::take_sheet_key_hooks, id);
    for cb in cbs.iter_mut() {
        cb();
    }
    put_callbacks(ZorkState::put_sheet_key_hooks, id, cbs);
}

/// Fire the cursor-moved handlers at the cursor's current position.
pub fn sheet_fire_cursor_move(id: usize) {
    let pos = with_state(|s| s.sheet_cursor(id));
    let Some((row, col)) = pos else { return };
    let mut cbs = with_callbacks(ZorkState::take_sheet_cursor_move_hooks, id);
    for cb in cbs.iter_mut() {
        cb(row, col);
    }
    put_callbacks(ZorkState::put_sheet_cursor_move_hooks, id, cbs);
}

// -- Global key callbacks --
//
// `add_key_callback` on the other backends is process-wide and takes no
// widget, so zork's lives here rather than on a node.

pub fn add_key_callback(key: char, cb: Box<dyn FnMut()>) {
    with_state(|s| s.add_key_callback(key, cb));
}
pub fn key_callback_count(key: char) -> usize {
    with_state(|s| s.key_callback_count(key))
}
/// Fire the handlers for `key`; returns how many ran.
///
/// Uses `with_state` twice rather than `with_callbacks`, because the key table
/// is keyed by `char` and not by node id — `with_callbacks` is shaped around a
/// node. The borrow is still released across the call, which is the part that
/// matters: a handler may register another handler or touch any widget.
pub fn fire_key_callback(key: char) -> usize {
    let mut cbs = with_state(|s| ZorkState::take_key_callbacks(s, key));
    let n = cbs.len();
    for cb in cbs.iter_mut() {
        cb();
    }
    // `put_key_callbacks` *extends* rather than replaces, so a handler that
    // registered a new callback for this key during the call keeps it.
    with_state(|s| ZorkState::put_key_callbacks(s, key, cbs));
    n
}

// -- Timers --

/// Register a one-shot timer. The model has no wall clock, so it fires when
/// the driver calls [`advance_timers`] past its deadline.
pub fn timeout_add_once(ms: u32, f: Box<dyn FnOnce()>) {
    with_state(|s| s.timeout_add_once(ms, f));
}
/// Register a repeating timer; fires until its handler returns `false`.
pub fn timeout_add_repeating(ms: u32, f: Box<dyn FnMut() -> bool>) {
    with_state(|s| s.timeout_add_repeating(ms, f));
}
/// A repeating timer owned by a window, under a caller-chosen handle.
pub fn timeout_add_repeating_for_window(window_id: usize, id: usize, ms: u32, f: Box<dyn FnMut() -> bool>) {
    with_state(|s| s.timeout_add_repeating_for_window(window_id, id, ms, f));
}
pub fn cancel_timer(id: usize) -> bool {
    with_state(|s| s.cancel_timer(id))
}
/// Fire every timer due within `ms` of virtual time. Returns how many fired.
pub fn advance_timers(ms: u64) -> usize {
    let mut due = with_state(|s| s.take_due_timers(ms));
    let mut fired = 0usize;
    while !due.is_empty() {
        let mut batch = std::mem::take(&mut due);
        fired += batch.len();
        for mut t in batch.drain(..) {
            let keep = ZorkState::run_timer(&mut t);
            if keep {
                // Nothing is borrowed here, so reading the deadline is safe.
                // It is the time this timer was *due*, not the end of the
                // jump, so a 50ms timer advanced across 200ms fires four times
                // as a real main loop would.
                let at = t.due_at;
                with_state(|s| s.put_back_timer(t, at));
            }
        }
        // A handler may have registered a zero-delay timer, or re-armed one
        // that is already due; drain those too rather than stranding them
        // until the next advance.
        due = with_state(|s| s.take_due_timers(0));
    }
    fired
}
pub fn timer_count() -> usize {
    with_state(|s| s.timer_count())
}
/// The virtual clock, in milliseconds since model creation.
pub fn now_ms() -> u64 {
    with_state(|s| s.now_ms())
}

// -- Application --

pub fn application_add_action(app_id: usize, action_id: usize) -> Option<String> {
    with_state(|s| s.application_add_action(app_id, action_id))
}
pub fn application_actions(app_id: usize) -> Vec<String> {
    with_state(|s| s.application_actions(app_id))
}

// -- Diagnostics --

/// Whether `id` names a live node.
pub fn node_exists(id: usize) -> bool {
    with_state(|s| s.node(id).is_some())
}

/// Write the model as a human-readable tree to `path`, one line per node.
///
/// The zork counterpart of NWG's `debug_dump_native_tree`, which walks
/// Win32 windows. There is no window manager here, so what exists to dump is
/// the widget tree itself: id, kind, label/title/text, geometry, visibility
/// and the child's ids. That is the thing worth inspecting when a layout bug
/// needs ground truth, and on a headless backend it is the *only* thing that
/// can be ground truth.
///
/// Returns the number of nodes written. An unwritable path is reported as 0
/// rather than panicking, so a diagnostic that cannot run does not take the
/// process with it.
pub fn debug_dump_tree(path: &str) -> std::io::Result<usize> {
    with_state(|s| dump_tree(s, None, path))
}

/// [`debug_dump_tree`] rooted at one node instead of at the tree's roots.
/// Returns 0 when `id` is not a node.
pub fn debug_dump_tree_from(id: usize, path: &str) -> std::io::Result<usize> {
    with_state(|s| dump_tree(s, Some(id), path))
}

/// [`debug_dump_tree`] over a state the caller owns.
///
/// Needed because the REPL holds its `ZorkState` directly:
/// `ZorkApp::adopt_singleton` *moves* the singleton's contents into the REPL and
/// leaves an empty model behind, so the `with_state` entry points above would
/// dump nothing. Going through the REPL's own state is what makes `dump` show
/// the widgets the host actually built.
pub fn debug_dump_tree_of(state: &ZorkState, path: &str) -> std::io::Result<usize> {
    dump_tree(state, None, path)
}

/// The shared walk behind every entry point. `from` of `None` means "start at
/// every parentless node", `Some(id)` means "start at that node".
///
/// Returns `Err` for an unwritable path — the caller should be able to tell
/// "could not write" from "wrote an empty file", because a headless backend
/// hitting a read-only or non-existent directory is a plausible thing to do
/// and reporting it as "0 nodes" would hide it.
fn dump_tree(s: &ZorkState, from: Option<usize>, path: &str) -> std::io::Result<usize> {
    use std::fmt::Write as _;
    let mut out = String::new();
    let roots: Vec<usize> = match from {
        Some(id) => vec![id],
        // Roots are nodes with no parent. Walking from them gives each node
        // one line and one place in the tree; a walk over `nodes` would print
        // orphans out of order and lose the nesting the reader needs.
        None => s.nodes.iter().filter(|n| n.parent.is_none()).map(|n| n.id).collect(),
    };
    let mut written = 0usize;
    for root in roots {
        let mut stack = vec![(root, 0usize)];
        while let Some((id, depth)) = stack.pop() {
            let Some(n) = s.node(id) else { continue };
            let label = match &n.kind {
                ZorkKind::Window { title } | ZorkKind::Dialog { title } => title.clone(),
                ZorkKind::Label { text } | ZorkKind::TextView { text } => text.clone(),
                ZorkKind::Button { label }
                | ZorkKind::CheckButton { label, .. }
                | ZorkKind::RadioButton { label, .. } => label.clone(),
                ZorkKind::DropDown { items, .. } => format!("{} items", items.len()),
                ZorkKind::Menu => n.menu_label.clone().unwrap_or_default(),
                ZorkKind::Spreadsheet { border_title } => border_title.clone(),
                _ => String::new(),
            };
            let geo = format!(
                "at ({},{}) {}x{}",
                n.props.offset_x.unwrap_or(0),
                n.props.offset_y.unwrap_or(0),
                n.props.width.unwrap_or(0),
                n.props.height.unwrap_or(0),
            );
            let vis = if n.props.visible { "visible" } else { "hidden" };
            // The label is quoted and always present, even when empty. Leaving
            // it out when a widget has no text ran `#2Book1` together — a dump
            // line you cannot split by eye is a dump you cannot read.
            let _ = writeln!(
                out,
                "{:indent$}{} #{} \"{}\" {} {} [{} child(ren), {} callback(s)]",
                "",
                n.kind.name(),
                n.id,
                label,
                geo,
                vis,
                n.children.len(),
                n.callbacks.len(),
                indent = depth * 2,
            );
            written += 1;
            // Push in reverse so the children come out in sibling order.
            for c in n.children.iter().rev() {
                stack.push((*c, depth + 1));
            }
        }
    }
    std::fs::write(path, out)?;
    Ok(written)
}

/// Bump the queued-redraw counter on `id` **and every descendant**.
///
/// `Window::queue_redraw` on GTK invalidates the window and GTK then
/// repaints the subtree; a caller that only expected the window to be marked
/// dirty would find nothing happens on a backend with no compositor. Doing the
/// cascade explicitly is what makes the same call meaningful here, and the
/// return value says how many nodes it reached.
pub fn queue_redraw_cascading(id: usize) -> usize {
    with_state(|s| {
        let mut stack = vec![id];
        let mut n = 0usize;
        while let Some(cur) = stack.pop() {
            let kids = match s.node_mut(cur) {
                Some(node) => {
                    node.pointer.redraws += 1;
                    n += 1;
                    node.children.clone()
                }
                None => continue,
            };
            stack.extend(kids);
        }
        n
    })
}

// -- Lifecycle --

pub fn quit() {
    with_state(|s| s.quit());
}
pub fn is_running() -> bool {
    with_state(|s| s.running)
}
