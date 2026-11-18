//! Pure, I/O-free model for the headless `zork` backend.
//!
//! Everything here is plain data + operations on it: no terminal access, no
//! `rustyline`, and no `thread_local` global required. A [`ZorkState`] is just a
//! value you can own, clone, snapshot to JSON, and drive from either the
//! interactive REPL ([`crate::backends::zork::repl`]) or the typed test harness
//! ([`crate::backends::zork::harness`]).
//!
//! The free functions that the per-widget adapters call (e.g.
//! [`crate::backends::zork::facade::create_button`], `set_label_text`) operate on
//! a thread-local singleton so that the existing `backends_zork_adapter` shim
//! keeps working unchanged. That singleton is a thin wrapper over a `ZorkState`;
//! tests that want a clean, isolated instance should use [`ZorkState`] /
//! [`harness::Harness`] directly instead.
//!
//! # Model shape
//!
//! A node is *kind* + *properties*:
//!
//! * [`ZorkKind`] holds the type-specific data (a label's text, a grid's cell
//!   list, a menu's items).
//! * [`ZorkProps`] holds everything that applies to *every* widget regardless of
//!   type — geometry, visibility, expansion, margins, CSS classes, alignment,
//!   font, scroll, and the pointer/key callback lists. Keeping it in one
//!   serializable struct is what lets [`ZorkState::snapshot`] report the whole
//!   model and lets tests diff a property change without caring which widget it
//!   came from.

use std::cell::RefCell;
use std::collections::HashMap;

pub type Callback = Box<dyn FnMut()>;

/// A rectangle in model units (the same integer pixel contract the GUI
/// adapters use, so a caller can pass its values straight through).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Per-widget properties that are independent of the widget's type.
///
/// Defaults are the "unset" state: an unconstrained rect, visible, not
/// expanded, no margins, no classes, default alignment and font.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ZorkProps {
    /// Position/size set by `set_offset` / `set_size_request` / `layout` /
    /// `attach`. `None` fields mean "not set", so a caller can tell the
    /// difference between `set_size_request(0, 0)` and never calling it.
    pub offset_x: Option<i32>,
    pub offset_y: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    /// Whether the widget is shown. A hidden widget is skipped by
    /// [`ZorkState::describe`]-style introspection and by focus traversal.
    pub visible: bool,
    pub hexpand: bool,
    pub vexpand: bool,
    pub margin_start: i32,
    pub margin_top: i32,
    /// CSS style classes, in the order they were added (duplicates ignored).
    pub classes: Vec<String>,
    /// 0.0 = start/left, 1.0 = end/right for the horizontal axis.
    pub xalign: f32,
    /// -1 = fill, 0 = start, 1 = end (GTK alignment convention).
    pub halign: i32,
    pub valign: i32,
    /// Pin the widget's width so sibling reflow cannot change it. `None`
    /// releases the pin.
    pub fixed_width: Option<i32>,
    /// `Some(..)` when the caller has set a font/size, else the backend default.
    pub font: Option<String>,
    pub font_size: f64,
    /// Whether a `TextView` accepts user input.
    pub editable: bool,
    /// Scroll offsets, set via [`ZorkState::set_scroll`].
    pub hscroll: f64,
    pub vscroll: f64,
}

impl Default for ZorkProps {
    fn default() -> Self {
        ZorkProps {
            offset_x: None,
            offset_y: None,
            width: None,
            height: None,
            visible: true,
            hexpand: false,
            vexpand: false,
            margin_start: 0,
            margin_top: 0,
            classes: Vec::new(),
            xalign: 0.0,
            halign: 0,
            valign: 0,
            fixed_width: None,
            font: None,
            font_size: 0.0,
            editable: true,
            hscroll: 0.0,
            vscroll: 0.0,
        }
    }
}

/// Font style bits, matching the GTK `FontStyle` decomposition used by
/// `set_font_style` on the other adapters.
pub mod font_style {
    pub const NORMAL: i32 = 0;
    pub const BOLD: i32 = 1;
    pub const ITALIC: i32 = 2;
    pub const STRIKETHROUGH: i32 = 4;
    pub const UNDERLINE: i32 = 8;
}

/// One menu item. Extends the original label/action/submenu triple with the
/// state GTK/NWG menus actually carry: a kind (so separators and check/radio
/// items are representable), an accelerator, a checked flag and `enabled`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum MenuItemKind {
    Normal,
    /// A non-interactive divider. `label` is the visual text (often empty).
    Separator,
    /// A toggle item; `checked` is its state.
    Check,
    /// A radio item; `checked` is its state, `group` groups siblings.
    Radio,
    /// A non-interactive section header.
    Section,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct MenuItemData {
    pub label: String,
    pub action: String,
    pub submenu: Option<Vec<MenuItemData>>,
    /// What kind of item this is. Defaults to [`MenuItemKind::Normal`].
    pub kind: MenuItemKind,
    /// Keyboard shortcut, e.g. `"Ctrl+S"`. Empty when unset.
    pub accelerator: String,
    pub checked: bool,
    pub enabled: bool,
    /// Radio group id; `0` when the item is not a radio.
    pub group: usize,
}

impl MenuItemData {
    /// A plain, enabled, non-toggle item. The `menu_append` path produces
    /// exactly this so existing callers are unaffected by the new fields.
    pub fn normal(label: &str, action: &str) -> Self {
        MenuItemData {
            label: label.to_string(),
            action: action.to_string(),
            submenu: None,
            kind: MenuItemKind::Normal,
            accelerator: String::new(),
            checked: false,
            enabled: true,
            group: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub enum ZorkKind {
    Window { title: String },
    Button { label: String },
    Label { text: String },
    BoxWidget { horizontal: bool, spacing: i32 },
    Grid { cols: usize, rows: usize },
    Entry { buffer: String, cursor: usize },
    CheckButton { label: String, checked: bool },
    RadioButton { label: String, checked: bool, group_id: usize },
    Dialog { title: String },
    Menu,
    MenuBar,
    SimpleAction,
    DropDown { items: Vec<String>, selected: Option<usize> },
    TextView { text: String },
    /// A custom drawing surface. The draw callback is a `Box<dyn FnMut>`, so it
    /// lives on the node rather than in the kind (which must stay `Clone`-able
    /// for `Snapshot`); [`ZorkState::draw_canvas`] invokes it against a
    /// recording context.
    Canvas { w: i32, h: i32 },
    /// A stack of children drawn over a base child; `set_child` is the base and
    /// `add_overlay` pushes additional layers.
    Overlay,
    /// A clipped, scrollable viewport around a child.
    ScrolledWindow,
    /// A child positioned at an explicit (x, y).
    Fixed,
    /// A registered, named action group. Kept in the model so `create_menubar`
    /// can resolve the action names a menu references.
    Application,
    /// A grid-of-cells widget with a border title.
    Spreadsheet { border_title: String },
}

/// A single cell of a [`ZorkKind::Spreadsheet`], addressed 1-based.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct SheetCell {
    pub text: String,
    /// Coarse style id (0 = default). Enough for a model test to assert that a
    /// style was applied; rasterisation is the draw callback's job.
    pub style: u8,
    /// True when `text` is a literal the user typed rather than a formula.
    pub raw: bool,
}

/// The spreadsheet's chrome: everything a `Spreadsheet` shows *around* the
/// cells.
///
/// A cell grid is the data; this is the rest of the widget — the cursor, the
/// in-progress edit, the column layout, the row labels, the tab strip, the
/// status line, the formula bar. GTK's and pancurses' `Spreadsheet` all take
/// these through `set_*` calls, and corro's render path issues every one of
/// them on each frame, so a model without them can only answer "no" or
/// "silently ignore it". Holding them here makes each `set_*` answerable and
/// the whole state snapshot-comparable.
///
/// Every field defaults to the "nothing configured" value, so a spreadsheet
/// created and immediately snapshotted is identical to one that has never been
/// touched — the same discipline [`ZorkProps`] follows.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SheetChrome {
    /// The cursor cell, `(row, col)`.
    ///
    /// **The same coordinate space as the cells**, which are addressed 1-based
    /// (see [`SheetCell`]). corro relies on that: `src/gui/pnc_backend.rs` calls
    /// `spreadsheet.set_cell(cursor_display_ri, col, ..)` and
    /// `spreadsheet.set_cursor(cursor_display_ri, col)` with the *same* row, and
    /// on pancurses they are the same index into the same grid.
    ///
    /// Making the cursor 0-based while the cells stayed 1-based meant
    /// `set_cursor(1, 1)` landed on a different cell than `get_cell(1, 1)`
    /// returned — off by one in both axes, and invisible until something
    /// actually drove the cursor against cells. The default is therefore
    /// `(1, 1)`, the first header cell, which is also where a cursor belongs
    /// when a sheet has just been built.
    pub cursor: (u32, u32),
    /// The drag anchor, in the same 1-based space as [`Self::cursor`]. `None`
    /// until a selection starts.
    pub anchor: Option<(u32, u32)>,
    /// Whether the cursor cell is being edited.
    pub editing: bool,
    /// The in-progress edit buffer and the caret inside it.
    pub edit_buf: String,
    pub edit_pos: usize,
    /// `margin_cols` = the width of the row-label gutter; `main_cols` = the
    /// number of data columns.
    pub margin_cols: u32,
    pub main_cols: u32,
    /// `header_rows` (the frozen header band) and `main_rows` (data rows).
    pub header_rows: u32,
    pub main_rows: u32,
    /// Per-column layout as `(index, width_chars, title)`, in display order.
    pub column_layout: Vec<(u32, u32, String)>,
    /// Row labels as `(row_index, label)`.
    pub row_labels: Vec<(u32, String)>,
    /// The tab strip: titles plus the active tab's index.
    pub tabs: Vec<String>,
    pub active_tab: usize,
    /// The menu-bar line drawn along the top of the sheet.
    pub menu_text: String,
    /// The status line.
    pub status_text: String,
    /// The trailing part of the formula bar (e.g. a "· ready" note).
    pub formula_bar_trailing: String,
    /// The formula bar's address cell and entry text, set by `set_formula_bar`.
    pub formula_address: String,
    pub formula_entry: String,
}

impl SheetChrome {
    /// Every field at its zero value, including a `(0, 0)` cursor.
    ///
    /// Only [`Default`] should be called by ordinary code, and it moves the
    /// cursor to `(1, 1)`. This exists so [`Default`] can spell out the one
    /// field that differs rather than deriving a cursor into a cell that does
    /// not exist.
    fn empty() -> Self {
        SheetChrome {
            cursor: (0, 0),
            anchor: None,
            editing: false,
            edit_buf: String::new(),
            edit_pos: 0,
            margin_cols: 0,
            main_cols: 0,
            header_rows: 0,
            main_rows: 0,
            column_layout: Vec::new(),
            row_labels: Vec::new(),
            tabs: Vec::new(),
            active_tab: 0,
            menu_text: String::new(),
            status_text: String::new(),
            formula_bar_trailing: String::new(),
            formula_address: String::new(),
            formula_entry: String::new(),
        }
    }
}

/// The default is `(1, 1)`: the first header cell, in the same 1-based space
/// as the cells. `Default` cannot derive, because `0, 0` is not a cell in that
/// space and a cursor pointing outside the grid is a state the widget cannot be
/// in.
impl Default for SheetChrome {
    fn default() -> Self {
        SheetChrome {
            cursor: (1, 1),
            ..SheetChrome::empty()
        }
    }
}

/// The four input events a `Spreadsheet` accepts, as the other backends
/// define them.
///
/// This mirrors `backends_pancurses`' `add_key_callback` /
/// `add_goto_callback` / `add_cursor_move_callback` /
/// `add_commit_edit_callback` group, which is the one spreadsheet input model
/// in this crate that is not a set of bare setter calls. Each field is the list
/// of handlers for that event; every one is fired with the model's borrow
/// released, so a handler may call back into the widget.
#[allow(clippy::type_complexity)]
#[derive(Default)]
pub struct SheetInput {
    /// Fired for a bare keypress with no cell argument — a function key, a
    /// shortcut, anything that is not a cursor move.
    pub key: Vec<Box<dyn FnMut()>>,
    /// Fired when the user navigates to a cell (arrow keys, Tab, a name typed
    /// into the name box) *without* editing it.
    pub goto: Vec<Box<dyn FnMut()>>,
    /// Fired when the cursor lands on a new cell, as `(row, col)`.
    pub cursor_move: Vec<Box<dyn FnMut(u32, u32)>>,
    /// Fired when an in-progress edit is committed to a cell, as
    /// `(row, col, text)`.
    pub commit_edit: Vec<Box<dyn FnMut(u32, u32, String)>>,
}

impl std::fmt::Debug for SheetInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SheetInput")
            .field("key", &self.key.len())
            .field("goto", &self.goto.len())
            .field("cursor_move", &self.cursor_move.len())
            .field("commit_edit", &self.commit_edit.len())
            .finish()
    }
}

pub struct ZorkNode {
    pub id: usize,
    pub kind: ZorkKind,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub callbacks: Vec<Callback>,
    /// Type-independent properties (see [`ZorkProps`]).
    pub props: ZorkProps,
    /// Whether this node can take keyboard focus.
    pub can_focus: bool,
    /// The canvas draw callback, set by `Canvas::set_draw_callback`. Stored
    /// out-of-band from `kind` so `kind` can stay `Clone`.
    pub draw: Option<Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>>,
    /// Pointer/key callbacks for a `Canvas`. Kept separate from `callbacks`
    /// (which is "the thing the harness `click` fires") so clicking a canvas
    /// does not also fire its generic action callbacks.
    pub pointer: PointerHooks,
    /// `ScrolledWindow` scroll-notification handlers, called as
    /// `cb(vertical, pos)`.
    pub scroll_cbs: Vec<Box<dyn FnMut(bool, f64)>>,
    /// Canvas gesture handlers, called as `cb(Gesture)`.
    ///
    /// Separate from the four `PointerHooks` lists because a `Gesture` carries
    /// a button and modifiers where the raw hooks do not, and because GTK
    /// builds a drag out of motion events that a raw `on_motion` would only
    /// deliver with no button held. The two coexist: a host may register
    /// either, or both.
    pub gesture_cbs: Vec<Box<dyn FnMut(crate::core::Gesture)>>,
    /// `Dialog` response handlers, called as `cb(response_id)`. Separate from
    /// `callbacks` because a response must carry its id.
    pub response_cbs: Vec<Box<dyn FnMut(i32)>>,
    /// Spreadsheet cell contents, keyed 1-based by `(row, col)`.
    pub cells: HashMap<(u32, u32), SheetCell>,
    /// Spreadsheet chrome: cursor, edit state, column layout, labels, tabs,
    /// status and formula bar. See [`SheetChrome`].
    pub chrome: SheetChrome,
    /// The spreadsheet's *input* hooks — the four ways a driver tells a
    /// spreadsheet what the user did. Kept out of `cells`/`chrome` because they
    /// are behaviours, not state: GTK and pancurses route them through
    /// `on_key`/`on_gesture` signal handlers, and corro registers one of each.
    pub sheet_input: SheetInput,
    /// Dialog buttons as `(label, response_id)`, and the default response id.
    pub dialog_buttons: Vec<(String, i32)>,
    pub dialog_default_response: Option<i32>,
    pub dialog_transient_for: Option<usize>,
    /// Whether a dialog has been destroyed (`Dialog::close` / `mark_destroyed`).
    pub destroyed: bool,
    /// Overlay layers, in insertion order, and per-child pass-through flags.
    pub overlays: Vec<usize>,
    pub overlay_pass_through: HashMap<usize, bool>,
    /// The menu item that opened this node, e.g. `"_File"` for the File
    /// submenu.
    ///
    /// `ZorkKind::Menu` is a unit variant -- a menu node carries no name of
    /// its own -- so without this every menu in a corro session printed as
    /// "Menu", and the direction list degenerated to "To the east: Menu" for
    /// all twenty of them. Set from the owning `MenuItemData`'s label, so it
    /// is only ever `Some` for a submenu.
    ///
    /// This is display-only. It changes what the REPL calls a node and
    /// nothing else: the tree shape, the parent/child links and the item
    /// lists are untouched, because an earlier attempt to also de-duplicate
    /// the re-materialised submenu nodes cost 53 menu items (132 -> 79).
    pub menu_label: Option<String>,
}

impl ZorkKind {
    /// The kind's name, as the REPL and the snapshot both call it.
    ///
    /// One definition rather than a `match` in each place: the snapshot's
    /// `kind` string and `debug_dump_tree`'s prefix must not be able to drift
    /// apart, because a snapshot test that asserts `"Spreadsheet"` would then
    /// pass against a dump that prints something else.
    pub fn name(&self) -> &'static str {
        match self {
            ZorkKind::Window { .. } => "Window",
            ZorkKind::Button { .. } => "Button",
            ZorkKind::Label { .. } => "Label",
            ZorkKind::BoxWidget { .. } => "BoxWidget",
            ZorkKind::Grid { .. } => "Grid",
            ZorkKind::Entry { .. } => "Entry",
            ZorkKind::CheckButton { .. } => "CheckButton",
            ZorkKind::RadioButton { .. } => "RadioButton",
            ZorkKind::Dialog { .. } => "Dialog",
            ZorkKind::Menu => "Menu",
            ZorkKind::MenuBar => "MenuBar",
            ZorkKind::SimpleAction => "SimpleAction",
            ZorkKind::DropDown { .. } => "DropDown",
            ZorkKind::TextView { .. } => "TextView",
            ZorkKind::Canvas { .. } => "Canvas",
            ZorkKind::Overlay => "Overlay",
            ZorkKind::ScrolledWindow => "ScrolledWindow",
            ZorkKind::Fixed => "Fixed",
            ZorkKind::Application => "Application",
            ZorkKind::Spreadsheet { .. } => "Spreadsheet",
        }
    }
}

/// A pending timer, as `timeout_add_once` / `timeout_add_repeating` register
/// one.
///
/// `due_at` is *virtual* elapsed milliseconds, not wall-clock: the model has
/// no clock, because a test that waits 200 ms of real time to observe a timer
/// is a slow test that can still flake. Instead the driver advances the clock
/// explicitly with [`ZorkState::advance_timers`], which is the headless
/// equivalent of the main loop reaching the timeout.
pub struct Timer {
    /// Virtual milliseconds since model creation when this fires next.
    pub due_at: u64,
    pub interval_ms: u32,
    /// The handler. `Some` for a repeating timer (it decides whether to stay
    /// registered); `None` for a one-shot, whose handler has already been
    /// consumed.
    pub handler: Option<Box<dyn FnMut() -> bool>>,
    /// The one-shot handler, consumed on the first firing.
    pub once: Option<Box<dyn FnOnce()>>,
    /// The window that owns this timer, when it was started through
    /// `Window::start_repeating_timer`. `None` for a process-wide timer.
    pub window_id: Option<usize>,
}

impl std::fmt::Debug for Timer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timer")
            .field("due_at", &self.due_at)
            .field("interval_ms", &self.interval_ms)
            .field("repeating", &self.handler.is_some())
            .field("window_id", &self.window_id)
            .finish()
    }
}

/// The pointer and key callbacks a `Canvas`/`Entry` can carry.
#[allow(clippy::type_complexity)]
#[derive(Default)]
pub struct PointerHooks {
    pub click: Vec<Box<dyn FnMut(f64, f64)>>,
    pub click_button: Vec<Box<dyn FnMut(f64, f64, u32, u32)>>,
    pub motion: Vec<Box<dyn FnMut(f64, f64, u32)>>,
    pub release: Vec<Box<dyn FnMut(f64, f64, u32, u32)>>,
    pub key: Vec<Box<dyn FnMut(u32) -> bool>>,
    pub key_raw: Vec<Box<dyn FnMut(u32, u32) -> bool>>,
    /// This canvas's top-left in "screen" coordinates, when known.
    pub screen_origin: Option<(i32, i32)>,
    /// The button currently held down, set by a press and cleared by a
    /// release. Read by the gesture stream, which has no other way to know
    /// which button a following `Drag` belongs to.
    pub down_button: Option<crate::core::Button>,
    /// Number of queued redraws, so a test can assert `queue_redraw` was called
    /// even though nothing is rasterised.
    pub redraws: u32,
}

/// A lifted-out canvas draw callback, carried across the borrow gap.
pub struct DrawSlot(Option<(Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>, i32, i32)>);

impl DrawSlot {
    /// Take the callback out for running. Leaves the slot empty, which
    /// [`Self::put_back`] then restores.
    pub fn take(&mut self) -> Option<(Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>, i32, i32)> {
        self.0.take()
    }
    /// Put a previously taken callback back into the slot.
    pub fn put_back(&mut self, d: (Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>, i32, i32)) {
        self.0 = Some(d);
    }
}

impl std::fmt::Debug for PointerHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PointerHooks")
            .field("click", &self.click.len())
            .field("click_button", &self.click_button.len())
            .field("motion", &self.motion.len())
            .field("release", &self.release.len())
            .field("key", &self.key.len())
            .field("key_raw", &self.key_raw.len())
            .field("screen_origin", &self.screen_origin)
            .field("redraws", &self.redraws)
            .finish()
    }
}

impl ZorkNode {
    /// Run all callbacks currently stored on this node. Callers must invoke this
    /// *outside* of any borrow of the owning [`ZorkState`], because a callback
    /// may re-enter the model.
    pub fn fire_callbacks(&mut self) {
        // Take the callbacks out so the borrow on `self` is released before each
        // (potentially re-entrant) call.
        let mut cbs = std::mem::take(&mut self.callbacks);
        for cb in cbs.iter_mut() {
            cb();
        }
        self.callbacks = cbs;
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SnapshotNode {
    pub id: usize,
    pub kind: String,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub title: Option<String>,
    pub text: Option<String>,
    pub label: Option<String>,
    pub checked: Option<bool>,
    pub selected: Option<usize>,
    pub items: Option<Vec<String>>,
    /// The full property bag, so a snapshot test can assert geometry,
    /// visibility, expansion, margins, classes and scroll.
    pub props: ZorkProps,
    pub can_focus: bool,
    /// Callback *counts*, never the closures themselves (not serializable).
    pub callbacks: usize,
    pub click_hooks: usize,
    pub redraws: u32,
    /// Spreadsheet cells as `(row, col, text, style, raw)`.
    pub cells: Vec<SheetCellEntry>,
    /// The spreadsheet's chrome. `None` for a node that is not a spreadsheet,
    /// so a non-spreadsheet node does not carry an all-defaults bag that
    /// would read as "configured" in a snapshot diff.
    pub chrome: Option<SheetChrome>,
    /// Spreadsheet input hook counts, as `(key, goto, cursor_move,
    /// commit_edit)`. `None` for a non-spreadsheet node.
    pub sheet_hooks: Option<(usize, usize, usize, usize)>,
    /// Dialog buttons as `(label, response_id)`.
    pub dialog_buttons: Vec<(String, i32)>,
    pub overlays: Vec<usize>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SheetCellEntry {
    pub row: u32,
    pub col: u32,
    pub text: String,
    pub style: u8,
    pub raw: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Snapshot {
    pub nodes: Vec<SnapshotNode>,
    pub menu_items: HashMap<usize, Vec<MenuItemData>>,
    pub current_id: usize,
    pub running: bool,
    /// The node id that holds keyboard focus, if any.
    pub focused: Option<usize>,
    /// Overlay child -> pass-through flag.
    pub overlay_pass_through: HashMap<usize, bool>,
    /// Spreadsheet cells keyed by `(row, col)`, flattened for easy lookup.
    pub cells: HashMap<(u32, u32), SheetCell>,
    /// Whether the dungeon is dark.
    pub night_mode: bool,
    /// Pending timers, for assertions on registration and (via `now_ms`)
    /// on what has fired.
    pub timers: Vec<TimerInfo>,
    /// The virtual clock in milliseconds.
    pub now_ms: u64,
    /// `Application` node id -> the action names registered with it.
    pub app_actions: HashMap<usize, Vec<String>>,
}

/// The serializable half of a [`Timer`]: the closure cannot be serialized, so
/// a snapshot reports what a timer *is* (when it is due, how often) and not
/// what it would do.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TimerInfo {
    pub due_at: u64,
    pub interval_ms: u32,
    /// True for a repeating timer, false for a one-shot.
    pub repeating: bool,
    pub window_id: Option<usize>,
}

pub struct ZorkState {
    pub nodes: Vec<ZorkNode>,
    pub next_id: usize,
    pub running: bool,
    pub current_id: usize,
    pub prev_location: Option<usize>,
    /// Menu model items keyed by menu node id.
    pub menu_items: HashMap<usize, Vec<MenuItemData>>,
    /// The node that currently holds keyboard focus.
    pub focused: Option<usize>,
    /// Named actions, so `MenuBar` dispatch can resolve an action name to the
    /// `SimpleAction` node that registered it.
    pub action_names: HashMap<String, usize>,
    /// Registered timers, in registration order. See [`ZorkState::timeout_add_*`].
    pub timers: Vec<Timer>,
    /// Caller-supplied timer handle -> index into `timers`. NWG's
    /// `start_repeating_timer` returns an id the caller cancels with, so the
    /// same handle space exists here; it is kept separately from the `Timer`
    /// list because removing a timer shifts every later index.
    pub timer_ids: HashMap<usize, usize>,
    /// Process-wide key handlers, keyed by the character they fire on. This is
    /// the one widget-independent input table a spreadsheet has: `add_key_callback`
    /// on the other backends takes a bare `char` and no widget, so it cannot be
    /// per-node and is keyed here instead.
    pub key_callbacks: HashMap<char, Vec<Callback>>,
    /// `Application` nodes and the actions registered with them, keyed by the
    /// application node id.
    pub app_actions: HashMap<usize, Vec<String>>,
    /// Per-node "was (re)laid out" callbacks, called as `cb(w, h)`.
    pub layout_cbs: HashMap<usize, Vec<Box<dyn FnMut(i32, i32)>>>,
    /// Whether the dungeon is dark. Starts `true` — a Zork dungeon with no
    /// light source is dark by definition, and the REPL warns about the Grue
    /// while it is. See [`ZorkState::set_night_mode`].
    pub night_mode: bool,
    /// Whether the player has already been warned about the Grue since the
    /// light last changed, so a repeated `look` does not print the same line
    /// forever.
    pub grue_warned: bool,
    /// Virtual clock, advanced only by [`ZorkState::advance_timers`]. Timers
    /// compare against this rather than the wall clock.
    pub now_ms: u64,
}

impl Default for ZorkState {
    fn default() -> Self {
        Self::new()
    }
}

impl ZorkState {
    pub fn new() -> Self {
        ZorkState {
            nodes: Vec::new(),
            next_id: 1,
            running: true,
            current_id: 0,
            prev_location: None,
            menu_items: HashMap::new(),
            focused: None,
            action_names: HashMap::new(),
            timers: Vec::new(),
            timer_ids: HashMap::new(),
            key_callbacks: HashMap::new(),
            app_actions: HashMap::new(),
            layout_cbs: HashMap::new(),
            // A dungeon is dark until lit.
            night_mode: true,
            grue_warned: false,
            now_ms: 0,
        }
    }

    pub fn alloc_id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn add_node(&mut self, kind: ZorkKind, parent: Option<usize>) -> usize {
        let id = self.alloc_id();
        // If this is the first node (Window), set current_id
        if self.nodes.is_empty() {
            self.current_id = id;
        }
        let can_focus = matches!(
            kind,
            ZorkKind::Button { .. }
                | ZorkKind::Entry { .. }
                | ZorkKind::CheckButton { .. }
                | ZorkKind::RadioButton { .. }
                | ZorkKind::DropDown { .. }
                | ZorkKind::TextView { .. }
                | ZorkKind::Canvas { .. }
        );
        self.nodes.push(ZorkNode {
            id,
            kind,
            parent,
            children: Vec::new(),
            callbacks: Vec::new(),
            props: ZorkProps::default(),
            can_focus,
            draw: None,
            pointer: PointerHooks::default(),
            scroll_cbs: Vec::new(),
            gesture_cbs: Vec::new(),
            response_cbs: Vec::new(),
            cells: HashMap::new(),
            chrome: SheetChrome::default(),
            sheet_input: SheetInput::default(),
            dialog_buttons: Vec::new(),
            dialog_default_response: None,
            dialog_transient_for: None,
            destroyed: false,
            overlays: Vec::new(),
            overlay_pass_through: HashMap::new(),
            menu_label: None,
        });
        if let Some(pid) = parent {
            if let Some(p) = self.nodes.iter_mut().find(|n| n.id == pid) {
                p.children.push(id);
            }
        }
        id
    }

    pub fn node_mut(&mut self, id: usize) -> Option<&mut ZorkNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn node(&self, id: usize) -> Option<&ZorkNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn find_window_id(&self) -> Option<usize> {
        self.nodes
            .iter()
            .find(|n| matches!(n.kind, ZorkKind::Window { .. } | ZorkKind::Dialog { .. }))
            .map(|n| n.id)
    }

    // -- Factory functions (operate on this state) --

    pub fn create_window(&mut self) -> usize {
        self.add_node(ZorkKind::Window { title: String::new() }, None)
    }

    pub fn create_button(&mut self, label: &str) -> usize {
        self.add_node(ZorkKind::Button { label: label.to_string() }, self.find_window_id())
    }

    pub fn create_label(&mut self, text: &str) -> usize {
        self.add_node(ZorkKind::Label { text: text.to_string() }, self.find_window_id())
    }

    pub fn create_box(&mut self, horizontal: bool, spacing: i32) -> usize {
        self.add_node(ZorkKind::BoxWidget { horizontal, spacing }, self.find_window_id())
    }

    pub fn create_grid(&mut self) -> usize {
        self.add_node(ZorkKind::Grid { cols: 0, rows: 0 }, self.find_window_id())
    }

    pub fn create_entry(&mut self) -> usize {
        self.add_node(ZorkKind::Entry { buffer: String::new(), cursor: 0 }, self.find_window_id())
    }

    pub fn create_menu(&mut self) -> usize {
        let id = self.add_node(ZorkKind::Menu, self.find_window_id());
        self.menu_items.insert(id, Vec::new());
        id
    }

    pub fn create_simple_action(&mut self, name: &str) -> usize {
        let id = self.add_node(ZorkKind::SimpleAction, self.find_window_id());
        self.action_names.insert(name.to_string(), id);
        id
    }

    pub fn create_menubar(&mut self, model_id: usize, _action_group: *mut std::os::raw::c_void) -> usize {
        // `model_id` is the `Menu` corro built the items into, and it is the
        // real bar: it already holds the six top-level items. Re-use that node
        // rather than minting a second one, which used to leave the tree with
        // a `Menu` (id 12) and a `MenuBar` (id 92) carrying identical items --
        // so the window's "north" led to an unnamed copy while the REPL's
        // `go 1` numbering started from the real one.
        let bar_id = model_id;
        if let Some(n) = self.node_mut(bar_id) {
            n.kind = ZorkKind::MenuBar;
            n.menu_label = Some("Menu Bar".to_string());
        }
        let items = self.menu_items.get(&model_id).cloned().unwrap_or_default();
        self.menu_items.insert(bar_id, items);
        // Re-use the `Menu` nodes `create_menu` already made, rather than
        // minting a second set. `build_common_menu` builds the tree
        // recursively, so every submenu already has a node -- parented under
        // the window, because that was the only parent available when it was
        // created. Minting new ones left the originals in the tree holding
        // the same items, so the REPL's direction list showed the File menu
        // twice (both lines reading a bare "Menu" until `menu_label` gave
        // them names). Re-parent each one onto the bar and drop the stale
        // window link.
        //
        // Paired by LABEL, not by index. corro builds recursively, so the Nth
        // node created is not the Nth submenu item: `build_common_menu` calls
        // `new_menu` for a submenu before returning to its parent. An
        // index-based pairing silently mis-assigned 53 of the 132 items.
        // `menu_append_submenu` already stored each node's label, and a
        // label is unique among one menu's items, so the match is exact.
        //
        // Walk the whole tree, not just the bar's immediate items: Export sits
        // under File and Scope under Format, so a single level left 6 of the
        // 13 submenu nodes still parented under the window and still holding
        // their items in a second place. Recursion is what pairs all 13.
        // Collected as (label, items, owning node id) so a grandchild can be
        // re-parented onto its own parent rather than flattened onto the bar.
        let mut nested: Vec<(String, Vec<MenuItemData>, Option<usize>)> = Vec::new();
        self.collect_submenus(bar_id, &mut nested);
        for (label, sub_items, owner) in nested {
            let Some(sub_id) = self
                .nodes
                .iter()
                .find(|n| {
                    matches!(n.kind, ZorkKind::Menu)
                        && n.menu_label.as_deref() == Some(label.as_str())
                        && n.id != bar_id
                })
                .map(|n| n.id)
            else {
                continue;
            };
            self.menu_items.insert(sub_id, sub_items);
            if let Some(n) = self.node_mut(sub_id) {
                n.parent = Some(owner.unwrap_or(bar_id));
                n.menu_label = Some(label);
            }
            if let Some(win) = self.find_window_id() {
                if let Some(w) = self.node_mut(win) {
                    w.children.retain(|c| *c != sub_id);
                }
            }
        }
        // The submenus have all been moved onto the bar. The bar itself stays
        // where `create_menu` put it -- `add_node` parents it under whatever
        // the window contained at the time, which for corro is the outer
        // layout Box, not the window directly. Forcing it into the window's
        // children would detach it from that Box and leave the window's
        // "north" pointing at a SimpleAction instead.
        bar_id
    }

    /// Collect `(label, items)` for every submenu at or below `menu_id`,
    /// depth-first.
    ///
    /// Deeper than one level on purpose: corro nests File > Export > TSV and
    /// Format > Scope > All, and a submenu's own submenus are themselves
    /// submenus that need re-parenting onto the bar.
    fn collect_submenus(
        &self,
        menu_id: usize,
        out: &mut Vec<(String, Vec<MenuItemData>, Option<usize>)>,
    ) {
        let Some(items) = self.menu_items.get(&menu_id) else { return };
        for item in items {
            let Some(sub) = item.submenu.clone() else { continue };
            // `owner` is the node holding this submenu, so `Expor_t` stays
            // under `_File` rather than being flattened onto the bar beside
            // it. The first level's owner *is* the bar.
            out.push((item.label.clone(), sub, Some(menu_id)));
            // Recurse into the copy we just took: it is the authoritative
            // item list for that submenu.
            if let Some(nested_id) = self
                .nodes
                .iter()
                .find(|n| {
                    matches!(n.kind, ZorkKind::Menu)
                        && n.menu_label.as_deref() == Some(item.label.as_str())
                })
                .map(|n| n.id)
            {
                self.collect_submenus(nested_id, out);
            }
        }
    }

    pub fn create_dialog(&mut self) -> usize {
        self.add_node(ZorkKind::Dialog { title: String::new() }, self.find_window_id())
    }

    pub fn create_dropdown(&mut self, items: &[&str]) -> usize {
        let items_str: Vec<String> = items.iter().map(|s| s.to_string()).collect();
        self.add_node(ZorkKind::DropDown { items: items_str, selected: None }, self.find_window_id())
    }

    pub fn create_checkbutton(&mut self, label: &str) -> usize {
        self.add_node(ZorkKind::CheckButton { label: label.to_string(), checked: false }, self.find_window_id())
    }

    pub fn create_radiobutton(&mut self, group_id: Option<usize>, label: &str) -> usize {
        let gid = group_id.unwrap_or(0);
        self.add_node(ZorkKind::RadioButton { label: label.to_string(), checked: false, group_id: gid }, self.find_window_id())
    }

    /// Put a radio button into `group`, joining existing members.
    ///
    /// [`Self::create_radiobutton`] takes the *seed's* id, so a group of N
    /// buttons is built by passing the same seed to each. This setter is the
    /// other direction — useful when a button was created without a group and
    /// is grouped later, and it is what `core::App::create_radiobutton` needs
    /// once it stops discarding its `group` argument.
    pub fn set_radiobutton_group(&mut self, id: usize, group_id: usize) {
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::RadioButton { group_id: ref mut g, .. } = n.kind {
                *g = group_id;
            }
        }
    }

    /// The group a radio button belongs to; `0` means "ungrouped".
    pub fn radiobutton_group(&self, id: usize) -> Option<usize> {
        self.node(id).and_then(|n| match n.kind {
            ZorkKind::RadioButton { group_id, .. } => Some(group_id),
            _ => None,
        })
    }

    pub fn create_textview(&mut self) -> usize {
        self.add_node(ZorkKind::TextView { text: String::new() }, self.find_window_id())
    }

    /// A custom drawing surface. Sizing goes through the same `props` bag as
    /// every other widget so `set_size_request` / `set_content_size` work
    /// uniformly.
    pub fn create_canvas(&mut self) -> usize {
        self.add_node(ZorkKind::Canvas { w: 0, h: 0 }, self.find_window_id())
    }

    /// An overlay stack. `set_child` is the base layer, `add_overlay` pushes
    /// further ones.
    pub fn create_overlay(&mut self) -> usize {
        self.add_node(ZorkKind::Overlay, self.find_window_id())
    }

    pub fn create_scrolled_window(&mut self) -> usize {
        self.add_node(ZorkKind::ScrolledWindow, self.find_window_id())
    }

    pub fn create_fixed(&mut self) -> usize {
        self.add_node(ZorkKind::Fixed, self.find_window_id())
    }

    pub fn create_application(&mut self) -> usize {
        self.add_node(ZorkKind::Application, None)
    }

    pub fn create_spreadsheet(&mut self) -> usize {
        self.add_node(ZorkKind::Spreadsheet { border_title: String::new() }, self.find_window_id())
    }

    // -- Menu model --

    pub fn menu_append(&mut self, menu_id: usize, label: &str, action: &str) {
        self.menu_items
            .entry(menu_id)
            .or_default()
            .push(MenuItemData::normal(label, action));
    }

    /// Append an item with a keyboard accelerator.
    pub fn menu_append_with_shortcut(&mut self, menu_id: usize, label: &str, action: &str, accelerator: &str) {
        let mut item = MenuItemData::normal(label, action);
        item.accelerator = accelerator.to_string();
        self.menu_items.entry(menu_id).or_default().push(item);
    }

    /// Append a non-interactive section header.
    pub fn menu_append_section(&mut self, menu_id: usize, label: &str) {
        let mut item = MenuItemData::normal(label, "");
        item.kind = MenuItemKind::Section;
        item.enabled = false;
        self.menu_items.entry(menu_id).or_default().push(item);
    }

    /// Append a divider. `label` is the visual text (usually empty).
    pub fn menu_append_separator(&mut self, menu_id: usize, label: &str) {
        let mut item = MenuItemData::normal(label, "");
        item.kind = MenuItemKind::Separator;
        item.enabled = false;
        self.menu_items.entry(menu_id).or_default().push(item);
    }

    /// Append a check item and return its index, so the caller can later
    /// [`Self::set_menu_item_checked`] it.
    pub fn menu_append_check(&mut self, menu_id: usize, label: &str, action: &str, checked: bool) -> usize {
        let mut item = MenuItemData::normal(label, action);
        item.kind = MenuItemKind::Check;
        item.checked = checked;
        let items = self.menu_items.entry(menu_id).or_default();
        items.push(item);
        items.len() - 1
    }

    /// Append a radio item in `group`, returning its index.
    pub fn menu_append_radio(&mut self, menu_id: usize, label: &str, action: &str, group: usize, checked: bool) -> usize {
        let mut item = MenuItemData::normal(label, action);
        item.kind = MenuItemKind::Radio;
        item.group = group;
        item.checked = checked;
        if checked {
            // Radio items are mutually exclusive: clear the rest of the group.
            if let Some(items) = self.menu_items.get_mut(&menu_id) {
                for other in items.iter_mut() {
                    if other.kind == MenuItemKind::Radio && other.group == group {
                        other.checked = false;
                    }
                }
            }
        }
        let items = self.menu_items.entry(menu_id).or_default();
        items.push(item);
        items.len() - 1
    }

    /// Set the `checked` state of a menu item, enforcing radio exclusivity.
    pub fn set_menu_item_checked(&mut self, menu_id: usize, index: usize, checked: bool) {
        let (kind, group) = match self.menu_items.get(&menu_id).and_then(|i| i.get(index)) {
            Some(it) => (it.kind, it.group),
            None => return,
        };
        let Some(items) = self.menu_items.get_mut(&menu_id) else { return };
        if let Some(it) = items.get_mut(index) {
            it.checked = checked;
        }
        if checked && kind == MenuItemKind::Radio {
            for (i, other) in items.iter_mut().enumerate() {
                if i == index {
                    continue;
                }
                if other.kind == MenuItemKind::Radio && other.group == group {
                    other.checked = false;
                }
            }
        }
    }

    /// Append `item` to a submenu without copying the submenu's items: the
    /// child `Menu` node is linked so later edits to it stay visible.
    pub fn menu_append_submenu(&mut self, menu_id: usize, label: &str, submenu_id: usize) {
        let mut item = MenuItemData::normal(label, "");
        item.submenu = self.menu_items.get(&submenu_id).cloned();
        if let Some(n) = self.node_mut(submenu_id) {
            n.parent = Some(menu_id);
            // Name the submenu after the item that opened it, so the REPL can
            // say "To the north: Edit Menu" instead of a bare "Menu".
            n.menu_label = Some(label.to_string());
        }
        if let Some(p) = self.node_mut(menu_id) {
            if !p.children.contains(&submenu_id) {
                p.children.push(submenu_id);
            }
        }
        self.menu_items.entry(menu_id).or_default().push(item);
    }

    /// Set the label of a `Button`/`CheckButton`/`RadioButton`.
    pub fn set_button_label(&mut self, id: usize, label: &str) {
        if let Some(n) = self.node_mut(id) {
            match &mut n.kind {
                ZorkKind::Button { label: l } => *l = label.to_string(),
                ZorkKind::CheckButton { label: l, .. } => *l = label.to_string(),
                ZorkKind::RadioButton { label: l, .. } => *l = label.to_string(),
                _ => {}
            }
        }
    }

    pub fn get_button_label(&self, id: usize) -> Option<String> {
        self.node(id).and_then(|n| match &n.kind {
            ZorkKind::Button { label }
            | ZorkKind::CheckButton { label, .. }
            | ZorkKind::RadioButton { label, .. } => Some(label.clone()),
            _ => None,
        })
    }

    // -- Setters / getters --

    pub fn set_window_title(&mut self, id: usize, title: &str) {
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::Window { title: ref mut t } | ZorkKind::Dialog { title: ref mut t } = n.kind {
                *t = title.to_string();
            }
        }
    }

    pub fn get_window_title(&self, id: usize) -> Option<String> {
        self.node(id).and_then(|n| match &n.kind {
            ZorkKind::Window { title } | ZorkKind::Dialog { title } => Some(title.clone()),
            _ => None,
        })
    }

    pub fn set_label_text(&mut self, id: usize, text: &str) {
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::Label { text: ref mut t } = n.kind {
                *t = text.to_string();
            }
        }
    }

    pub fn get_label_text(&self, id: usize) -> Option<String> {
        self.node(id).and_then(|n| {
            if let ZorkKind::Label { ref text } = n.kind {
                Some(text.clone())
            } else {
                None
            }
        })
    }

    pub fn add_callback(&mut self, id: usize, cb: Callback) {
        if let Some(n) = self.node_mut(id) {
            n.callbacks.push(cb);
        }
    }

    pub fn set_entry_text(&mut self, id: usize, text: &str) {
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::Entry { ref mut buffer, ref mut cursor } = n.kind {
                *buffer = text.to_string();
                *cursor = buffer.chars().count();
            }
        }
    }

    pub fn get_entry_text(&self, id: usize) -> Option<String> {
        self.node(id).and_then(|n| {
            if let ZorkKind::Entry { ref buffer, .. } = n.kind {
                Some(buffer.clone())
            } else {
                None
            }
        })
    }

    /// The entry caret, as a character index.
    pub fn get_entry_position(&self, id: usize) -> Option<usize> {
        self.node(id).and_then(|n| match &n.kind {
            ZorkKind::Entry { cursor, .. } => Some(*cursor),
            _ => None,
        })
    }

    /// Move the entry caret, clamped to the buffer length.
    pub fn set_entry_position(&mut self, id: usize, pos: usize) {
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::Entry { ref buffer, ref mut cursor } = n.kind {
                *cursor = pos.min(buffer.chars().count());
            }
        }
    }

    pub fn set_textview_text(&mut self, id: usize, text: &str) {
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::TextView { text: ref mut t } = n.kind {
                *t = text.to_string();
            }
        }
    }

    pub fn get_textview_text(&self, id: usize) -> Option<String> {
        self.node(id).and_then(|n| {
            if let ZorkKind::TextView { ref text } = n.kind {
                Some(text.clone())
            } else {
                None
            }
        })
    }

    /// Append to a `TextView` buffer. Unlike the adapters' read-modify-write
    /// fallback this is a real incremental insert, so a high-rate log pane
    /// stays O(appended) rather than O(document).
    pub fn append_textview_text(&mut self, id: usize, text: &str) {
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::TextView { text: ref mut t } = n.kind {
                t.push_str(text);
            }
        }
    }

    pub fn set_dropdown_items(&mut self, id: usize, items: &[&str]) {
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::DropDown { items: ref mut items_vec, ref mut selected } = n.kind {
                *items_vec = items.iter().map(|s| s.to_string()).collect();
                // Drop a now-out-of-range selection rather than reporting a
                // selection that points at nothing.
                if selected.is_some_and(|s| s >= items_vec.len()) {
                    *selected = None;
                }
            }
        }
    }

    pub fn set_dropdown_selected(&mut self, id: usize, idx: i32) {
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::DropDown { ref mut selected, ref items } = n.kind {
                *selected = if idx >= 0 && (idx as usize) < items.len() {
                    Some(idx as usize)
                } else {
                    None
                };
            }
        }
    }

    pub fn get_dropdown_selected(&self, id: usize) -> i32 {
        self.node(id)
            .and_then(|n| {
                if let ZorkKind::DropDown { ref selected, .. } = n.kind {
                    selected.map(|s| s as i32)
                } else {
                    None
                }
            })
            .unwrap_or(-1)
    }

    pub fn get_checkbutton_checked(&self, id: usize) -> bool {
        self.node(id)
            .map(|n| {
                if let ZorkKind::CheckButton { ref checked, .. } = n.kind {
                    *checked
                } else {
                    false
                }
            })
            .unwrap_or(false)
    }

    pub fn get_radiobutton_checked(&self, id: usize) -> bool {
        self.node(id)
            .map(|n| {
                if let ZorkKind::RadioButton { ref checked, .. } = n.kind {
                    *checked
                } else {
                    false
                }
            })
            .unwrap_or(false)
    }

    pub fn set_checkbutton_checked(&mut self, id: usize, checked: bool) {
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::CheckButton { checked: ref mut c, .. } = n.kind {
                *c = checked;
            }
        }
    }

    /// Check a radio button, clearing every other radio in the same group.
    /// A group id of `0` means "ungrouped" and is left alone.
    pub fn set_radiobutton_checked(&mut self, id: usize, checked: bool) {
        let group = match self.node(id) {
            Some(n) => match n.kind {
                ZorkKind::RadioButton { group_id, .. } => group_id,
                _ => return,
            },
            None => return,
        };
        if checked && group != 0 {
            // Collect the ids first so we are not mutating while iterating.
            let siblings: Vec<usize> = self
                .nodes
                .iter()
                .filter(|n| matches!(n.kind, ZorkKind::RadioButton { group_id: g, .. } if g == group))
                .map(|n| n.id)
                .collect();
            for sid in siblings {
                if sid == id {
                    continue;
                }
                if let Some(n) = self.node_mut(sid) {
                    if let ZorkKind::RadioButton { checked: ref mut c, .. } = n.kind {
                        *c = false;
                    }
                }
            }
        }
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::RadioButton { checked: ref mut c, .. } = n.kind {
                *c = checked;
            }
        }
    }

    /// Reparent `child_id` under `parent_id`, removing it from any previous
    /// parent. Unlike the old implementation this also drops the child's
    /// *own* `parent` pointer before re-linking, so a node cannot end up
    /// listed under two parents.
    pub fn set_child(&mut self, parent_id: usize, child_id: usize) {
        if parent_id == child_id {
            return;
        }
        // Detach from the current parent, if any.
        if let Some(old) = self.node(child_id).and_then(|n| n.parent) {
            if let Some(p) = self.node_mut(old) {
                p.children.retain(|c| *c != child_id);
                p.overlays.retain(|c| *c != child_id);
            }
        }
        // Drop the child's subtree from the global dedup so re-adding is clean.
        self.nodes.iter_mut().for_each(|n| {
            n.children.retain(|c| *c != child_id);
            n.overlays.retain(|c| *c != child_id);
        });
        if let Some(parent) = self.node_mut(parent_id) {
            if !parent.children.contains(&child_id) {
                parent.children.push(child_id);
            }
        }
        if let Some(child) = self.node_mut(child_id) {
            child.parent = Some(parent_id);
        }
    }

    /// Append a child to a container, keeping any existing children.
    ///
    /// This is genuinely an *append*: the old implementation delegated to
    /// `set_child`, which replaced the child list and so made
    /// `BoxWidget::append` twice a no-op for the first child.
    pub fn append_child(&mut self, parent_id: usize, child_id: usize) {
        if parent_id == child_id {
            return;
        }
        if let Some(old) = self.node(child_id).and_then(|n| n.parent) {
            if old == parent_id {
                return; // already a child here
            }
            if let Some(p) = self.node_mut(old) {
                p.children.retain(|c| *c != child_id);
                p.overlays.retain(|c| *c != child_id);
            }
        }
        self.nodes.iter_mut().for_each(|n| {
            n.children.retain(|c| *c != child_id);
            n.overlays.retain(|c| *c != child_id);
        });
        if let Some(parent) = self.node_mut(parent_id) {
            parent.children.push(child_id);
        }
        if let Some(child) = self.node_mut(child_id) {
            child.parent = Some(parent_id);
        }
    }

    /// Remove `child_id` from its parent entirely.
    pub fn remove_child(&mut self, parent_id: usize, child_id: usize) {
        if let Some(p) = self.node_mut(parent_id) {
            p.children.retain(|c| *c != child_id);
            p.overlays.retain(|c| *c != child_id);
        }
        if self.node(child_id).and_then(|n| n.parent) == Some(parent_id) {
            if let Some(c) = self.node_mut(child_id) {
                c.parent = None;
            }
        }
    }

    // -- Geometry / layout --

    pub fn set_offset(&mut self, id: usize, x: i32, y: i32) {
        if let Some(n) = self.node_mut(id) {
            n.props.offset_x = Some(x);
            n.props.offset_y = Some(y);
        }
    }

    pub fn set_size_request(&mut self, id: usize, w: i32, h: i32) {
        if let Some(n) = self.node_mut(id) {
            n.props.width = Some(w);
            n.props.height = Some(h);
        }
    }

    pub fn get_size_request(&self, id: usize) -> Option<(Option<i32>, Option<i32>)> {
        self.node(id).map(|n| (n.props.width, n.props.height))
    }

    pub fn get_offset(&self, id: usize) -> Option<(Option<i32>, Option<i32>)> {
        self.node(id).map(|n| (n.props.offset_x, n.props.offset_y))
    }

    /// Record a child's grid cell. Sets the position and size, and grows the
    /// grid's `cols`/`rows` to cover the cell.
    pub fn grid_attach(&mut self, grid_id: usize, child_id: usize, left: i32, top: i32, width: i32, height: i32) {
        self.set_offset(child_id, left, top);
        self.set_size_request(child_id, width, height);
        self.append_child(grid_id, child_id);
        // A cell spanning `width` columns starting at `left` ends at
        // `left + width`; a zero-width cell still occupies one column.
        let col = if width > 0 { left + width } else { left + 1 };
        let row = if height > 0 { top + height } else { top + 1 };
        let (col, row) = (col.max(0) as usize, row.max(0) as usize);
        if let Some(n) = self.node_mut(grid_id) {
            if let ZorkKind::Grid { cols: ref mut c, rows: ref mut r } = n.kind {
                *c = (*c).max(col);
                *r = (*r).max(row);
            }
        }
    }

    /// Compute the size a `BoxWidget` needs for its visible children: the sum
    /// of the children's extent along the *packing* axis (plus `spacing`
    /// between them) and the largest extent on the cross axis.
    ///
    /// A horizontal box packs along x, so its width is the sum of the child
    /// widths and its height the tallest child. Returns `None` for a non-box.
    pub fn measure_box(&self, id: usize) -> Option<(i32, i32)> {
        let n = self.node(id)?;
        let (horizontal, spacing) = match n.kind {
            ZorkKind::BoxWidget { horizontal, spacing } => (horizontal, spacing),
            _ => return None,
        };
        let kids: Vec<&ZorkNode> = n
            .children
            .iter()
            .filter_map(|c| self.node(*c))
            .filter(|c| c.props.visible)
            .collect();
        if kids.is_empty() {
            return Some((0, 0));
        }
        // `pick` selects a child's extent on the requested axis.
        let main_of = |k: &ZorkNode| if horizontal { k.props.width } else { k.props.height };
        let cross_of = |k: &ZorkNode| if horizontal { k.props.height } else { k.props.width };
        let main: i32 = kids.iter().filter_map(|k| main_of(k)).sum::<i32>()
            + spacing * (kids.len() as i32 - 1).max(0);
        let cross: i32 = kids.iter().filter_map(|k| cross_of(k)).max().unwrap_or(0);
        Some(if horizontal { (main, cross) } else { (cross, main) })
    }

    /// Position a box's visible children along the packing axis, starting at
    /// `(x, y)` and honouring `spacing`, and record the box's own size request.
    ///
    /// A child advances the cursor by its own extent on the packing axis, so
    /// a child with no size request advances by nothing. Each child keeps its
    /// own size request — this is a positioning pass, not a resize pass; use
    /// [`Self::set_size_request`] to size children first.
    ///
    /// A layout handler registered with [`Self::set_layout_callback`] fires at
    /// the end, with this box's new size. It is *not* fired here: the handler
    /// is a user closure and must not run under a `&mut ZorkState`, so the
    /// caller (the facade) lifts it out first. Use
    /// [`crate::backends::zork::layout_box`] rather than this method when
    /// driving a live model, and it will fire the handler for you.
    pub fn layout_box(&mut self, id: usize, x: i32, y: i32, w: i32, h: i32) {
        let (horizontal, spacing) = match self.node(id) {
            Some(n) => match n.kind {
                ZorkKind::BoxWidget { horizontal, spacing } => (horizontal, spacing),
                _ => return,
            },
            None => return,
        };
        self.set_size_request(id, w, h);
        let kids: Vec<usize> = self
            .node(id)
            .map(|n| n.children.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|c| self.node(*c).is_some_and(|n| n.props.visible))
            .collect();
        let mut main = if horizontal { x } else { y };
        for kid in kids {
            let extent = self
                .node(kid)
                .and_then(|n| if horizontal { n.props.width } else { n.props.height })
                .unwrap_or(0)
                .max(0);
            if horizontal {
                self.set_offset(kid, main, y);
            } else {
                self.set_offset(kid, x, main);
            }
            main += extent + spacing;
        }
    }

    // -- Visibility / expansion / margins / classes / alignment --

    pub fn set_visible(&mut self, id: usize, visible: bool) {
        if let Some(n) = self.node_mut(id) {
            n.props.visible = visible;
        }
    }

    pub fn get_visible(&self, id: usize) -> bool {
        self.node(id).map(|n| n.props.visible).unwrap_or(false)
    }

    pub fn set_hexpand(&mut self, id: usize, v: bool) {
        if let Some(n) = self.node_mut(id) {
            n.props.hexpand = v;
        }
    }

    pub fn set_vexpand(&mut self, id: usize, v: bool) {
        if let Some(n) = self.node_mut(id) {
            n.props.vexpand = v;
        }
    }

    /// Set a child's expansion flag. GTK/NWG route this through the parent
    /// (`gtk_widget_set_hexpand(child, ...)`); the model stores it on the child
    /// so the flag survives without needing the parent to be resolved.
    pub fn set_child_hexpand(&mut self, _parent_id: usize, child_id: usize, v: bool) {
        self.set_hexpand(child_id, v);
    }

    pub fn set_child_vexpand(&mut self, _parent_id: usize, child_id: usize, v: bool) {
        self.set_vexpand(child_id, v);
    }

    pub fn set_margin_start(&mut self, id: usize, px: i32) {
        if let Some(n) = self.node_mut(id) {
            n.props.margin_start = px;
        }
    }

    pub fn set_margin_top(&mut self, id: usize, px: i32) {
        if let Some(n) = self.node_mut(id) {
            n.props.margin_top = px;
        }
    }

    pub fn add_class(&mut self, id: usize, class_name: &str) {
        if let Some(n) = self.node_mut(id) {
            if !n.props.classes.iter().any(|c| c == class_name) {
                n.props.classes.push(class_name.to_string());
            }
        }
    }

    pub fn remove_class(&mut self, id: usize, class_name: &str) {
        if let Some(n) = self.node_mut(id) {
            n.props.classes.retain(|c| c != class_name);
        }
    }

    pub fn has_class(&self, id: usize, class_name: &str) -> bool {
        self.node(id).map(|n| n.props.classes.iter().any(|c| c == class_name)).unwrap_or(false)
    }

    pub fn classes(&self, id: usize) -> Vec<String> {
        self.node(id).map(|n| n.props.classes.clone()).unwrap_or_default()
    }

    pub fn set_xalign(&mut self, id: usize, x: f32) {
        if let Some(n) = self.node_mut(id) {
            n.props.xalign = x;
        }
    }

    pub fn set_halign(&mut self, id: usize, align: i32) {
        if let Some(n) = self.node_mut(id) {
            n.props.halign = align;
        }
    }

    pub fn set_valign(&mut self, id: usize, align: i32) {
        if let Some(n) = self.node_mut(id) {
            n.props.valign = align;
        }
    }

    pub fn set_fixed_width(&mut self, id: usize, w: Option<i32>) {
        if let Some(n) = self.node_mut(id) {
            n.props.fixed_width = w;
            // A pinned width is a width request, so the two agree. Releasing
            // the pin releases the width request too, otherwise the widget
            // would stay sized by a pin the caller has removed.
            n.props.width = w;
        }
    }

    pub fn set_font_style(&mut self, id: usize, font: Option<&str>, size: f64) {
        if let Some(n) = self.node_mut(id) {
            n.props.font = font.map(|f| f.to_string());
            n.props.font_size = size;
        }
    }

    /// Whether a font is bold. The model records the font *name* the caller
    /// passed, so this reads the `bold` bit out of the [`font_style`] flags the
    /// adapter decomposed the caller's style into.
    pub fn font_is_bold(&self, id: usize) -> bool {
        self.node(id)
            .map(|n| n.props.font.as_deref().is_some_and(|f| f.split_whitespace().any(|w| w.eq_ignore_ascii_case("bold"))))
            .unwrap_or(false)
    }

    pub fn set_editable(&mut self, id: usize, editable: bool) {
        if let Some(n) = self.node_mut(id) {
            n.props.editable = editable;
        }
    }

    pub fn get_editable(&self, id: usize) -> bool {
        self.node(id).map(|n| n.props.editable).unwrap_or(false)
    }

    // -- Focus --

    /// Move keyboard focus to `id`. Focus is cleared from the previous holder
    /// and refused for a hidden or non-focusable node.
    pub fn set_focus(&mut self, id: usize) {
        if self.node(id).is_none() {
            return;
        }
        if self.node(id).map(|n| n.props.visible && n.can_focus).unwrap_or(false) {
            self.focused = Some(id);
        }
    }

    pub fn get_focus(&self) -> Option<usize> {
        self.focused
    }

    pub fn has_focus(&self, id: usize) -> bool {
        self.focused == Some(id)
    }

    /// The focused node's id when it is focusable, else `None`. Used by
    /// `Entry::has_focus` and the adapter's `grab_focus`.
    pub fn focused_entry(&self) -> Option<usize> {
        self.focused
    }

    pub fn set_can_focus(&mut self, id: usize, can: bool) {
        if let Some(n) = self.node_mut(id) {
            n.can_focus = can;
        }
    }

    // -- Layout callbacks --

    /// Register a "was (re)laid out" handler, called as `cb(w, h)`.
    pub fn set_layout_callback(&mut self, id: usize, cb: Box<dyn FnMut(i32, i32)>) {
        self.layout_cbs.entry(id).or_default().push(cb);
    }

    /// How many layout handlers a node has.
    pub fn layout_callback_count(&self, id: usize) -> usize {
        self.layout_cbs.get(&id).map(|v| v.len()).unwrap_or(0)
    }

    /// Take a node's layout handlers, for running with no borrow held.
    pub fn take_layout_cbs(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut(i32, i32)>> {
        s.layout_cbs.remove(&id).unwrap_or_default()
    }

    /// Put a node's layout handlers back. They are persistent, so this is a
    /// plain re-insert rather than the `append` the sheet key table needs.
    pub fn put_layout_cbs(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut(i32, i32)>>) {
        if !cbs.is_empty() {
            s.layout_cbs.insert(id, cbs);
        }
    }

    /// Append layout handlers, for a handler that registered more while the
    /// previous set was being fired.
    pub fn put_layout_cbs_add(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut(i32, i32)>>) {
        if !cbs.is_empty() {
            s.layout_cbs.entry(id).or_default().extend(cbs);
        }
    }

    // -- Scroll --

    /// Record a scroll position on a `ScrolledWindow`. `hval`/`vval` are the
    /// values; the `*_upper`/`*_page` arguments are accepted and clamped so a
    /// caller can never scroll past the end of the document.
    pub fn set_scroll(&mut self, id: usize, hval: f64, hupper: f64, hpage: f64, vval: f64, vupper: f64, vpage: f64) {
        if let Some(n) = self.node_mut(id) {
            n.props.hscroll = clamp_scroll(hval, hupper, hpage);
            n.props.vscroll = clamp_scroll(vval, vupper, vpage);
        }
    }

    pub fn get_scroll(&self, id: usize) -> (f64, f64) {
        self.node(id).map(|n| (n.props.hscroll, n.props.vscroll)).unwrap_or((0.0, 0.0))
    }

    /// Register a scroll-notification handler, called as `cb(vertical, pos)`.
    pub fn add_scroll_callback(&mut self, id: usize, cb: Box<dyn FnMut(bool, f64)>) {
        if let Some(n) = self.node_mut(id) {
            n.scroll_cbs.push(cb);
        }
    }

    /// Record a scroll offset on one axis, without firing any handler.
    pub fn set_scroll_pos(&mut self, id: usize, vertical: bool, pos: f64) {
        if let Some(n) = self.node_mut(id) {
            if vertical {
                n.props.vscroll = pos.max(0.0);
            } else {
                n.props.hscroll = pos.max(0.0);
            }
        }
    }

    // -- Canvas / drawing --

    pub fn set_draw_callback(
        &mut self,
        id: usize,
        cb: Box<dyn FnMut(&mut dyn crate::core::DrawContext, i32, i32)>,
    ) {
        if let Some(n) = self.node_mut(id) {
            n.draw = Some(cb);
        }
    }

    pub fn clear_draw_callback(&mut self, id: usize) {
        if let Some(n) = self.node_mut(id) {
            n.draw = None;
        }
    }

    pub fn has_draw_callback(&self, id: usize) -> bool {
        self.node(id).map(|n| n.draw.is_some()).unwrap_or(false)
    }

    pub fn queue_redraw(&mut self, id: usize) {
        if let Some(n) = self.node_mut(id) {
            n.pointer.redraws += 1;
        }
    }

    pub fn redraw_count(&self, id: usize) -> u32 {
        self.node(id).map(|n| n.pointer.redraws).unwrap_or(0)
    }

    /// A canvas's drawable size, from `set_size_request` / `set_content_size`.
    pub fn canvas_size(&self, id: usize) -> (i32, i32) {
        self.node(id).map(|n| (n.props.width.unwrap_or(0), n.props.height.unwrap_or(0))).unwrap_or((0, 0))
    }

    /// Run a canvas's draw callback against a recording context and return the
    /// recorded ops. This is how a test asserts *what a draw callback paints*
    /// without a display server — the same trick
    /// `backends::pancurses_draw::render_model_to_grid` plays for the terminal.
    pub fn draw_canvas(&mut self, id: usize) -> Vec<crate::backends::headless::DrawOp> {
        let mut ctx = crate::backends::headless::RecordingDrawContext::new();
        let (w, h) = self.canvas_size(id);
        let mut cb = match self.node_mut(id) {
            Some(n) => n.draw.take(),
            None => return Vec::new(),
        };
        if let Some(cb) = cb.as_mut() {
            cb(&mut ctx, w, h);
        }
        // Put the callback back so the canvas stays drawable.
        if let Some(n) = self.node_mut(id) {
            n.draw = cb;
        }
        ctx.ops
    }

    // -- Pointer / key hooks --

    pub fn add_click_hook(&mut self, id: usize, cb: Box<dyn FnMut(f64, f64)>) {
        if let Some(n) = self.node_mut(id) {
            n.pointer.click.push(cb);
        }
    }

    pub fn add_click_button_hook(&mut self, id: usize, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
        if let Some(n) = self.node_mut(id) {
            n.pointer.click_button.push(cb);
        }
    }

    pub fn add_motion_hook(&mut self, id: usize, cb: Box<dyn FnMut(f64, f64, u32)>) {
        if let Some(n) = self.node_mut(id) {
            n.pointer.motion.push(cb);
        }
    }

    pub fn add_release_hook(&mut self, id: usize, cb: Box<dyn FnMut(f64, f64, u32, u32)>) {
        if let Some(n) = self.node_mut(id) {
            n.pointer.release.push(cb);
        }
    }

    pub fn add_key_hook(&mut self, id: usize, cb: Box<dyn FnMut(u32) -> bool>) {
        if let Some(n) = self.node_mut(id) {
            n.pointer.key.push(cb);
        }
    }

    pub fn add_key_raw_hook(&mut self, id: usize, cb: Box<dyn FnMut(u32, u32) -> bool>) {
        if let Some(n) = self.node_mut(id) {
            n.pointer.key_raw.push(cb);
        }
    }

    /// Register a gesture handler, called as `cb(Gesture)`.
    pub fn add_gesture_hook(&mut self, id: usize, cb: Box<dyn FnMut(crate::core::Gesture)>) {
        if let Some(n) = self.node_mut(id) {
            n.gesture_cbs.push(cb);
        }
    }

    /// How many gesture handlers a node has.
    pub fn gesture_hook_count(&self, id: usize) -> usize {
        self.node(id).map(|n| n.gesture_cbs.len()).unwrap_or(0)
    }

    /// The state a gesture is derived from: which button (if any) is down.
    ///
    /// A `Gesture::Button { pressed: true }` has to remember its button so the
    /// `Drag` events that follow can report which one it was — that is the
    /// whole reason `Gesture` carries one. The raw `on_click`/`on_motion` hooks
    /// are passed the button by the caller, so they need no state; the gesture
    /// stream has to derive it.
    pub fn gesture_button_down(&self, id: usize) -> Option<crate::core::Button> {
        self.node(id).map(|n| n.pointer.down_button).unwrap_or(None)
    }

    /// Translate a button number into the [`crate::core::Button`] the gesture
    /// vocabulary names.
    ///
    /// The raw numbering differs between GTK (1 = left), GDI (0 = left) and
    /// UIKit, which is why `core::Button` exists and why a backend is expected
    /// to map rather than pass a number through. GTK is the reference here
    /// because it is the platform whose numbering the other adapters' callers
    /// were written against; a number outside 1..=3 becomes
    /// `Button::Other(n - 1)`, and 0 (GDI's left) is treated as left as well,
    /// so a caller that passes either convention gets a press rather than a
    /// dropped event.
    pub fn gesture_button(n: u32) -> crate::core::Button {
        match n {
            0 | 1 => crate::core::Button::Primary,
            2 => crate::core::Button::Middle,
            3 => crate::core::Button::Secondary,
            other => crate::core::Button::Other((other - 1) as u8),
        }
    }

    /// Translate a GDK-style modifier bitmask into [`crate::core::Modifiers`].
    ///
    /// GDK's are 1 = shift, 4 = control (Mod1), 8 = alt (Mod2),
    /// 64 = super (Mod4). 2 (Mod3, "lock"/alt on some layouts) is
    /// deliberately ignored: treating it as a modifier would make a Caps Lock
    /// press look like Alt on the machines where Mod3 is bound to it.
    pub fn gesture_modifiers(state: u32) -> crate::core::Modifiers {
        crate::core::Modifiers {
            shift: state & 1 != 0,
            ctrl: state & 4 != 0,
            alt: state & 8 != 0,
            meta: state & 64 != 0,
        }
    }

    pub fn set_screen_origin(&mut self, id: usize, origin: Option<(i32, i32)>) {
        if let Some(n) = self.node_mut(id) {
            n.pointer.screen_origin = origin;
        }
    }

    /// Fire a click at model coordinates. Runs the node's pointer hooks and its
    /// generic callbacks, with the borrow released before each invocation so a
    /// hook may re-enter the model.
    pub fn pointer_click(&mut self, id: usize, x: f64, y: f64) {
        let mut cbs = self.node_mut(id).map(|n| std::mem::take(&mut n.pointer.click)).unwrap_or_default();
        for cb in cbs.iter_mut() {
            cb(x, y);
        }
        if let Some(n) = self.node_mut(id) {
            n.pointer.click = cbs;
        }
        self.click(id);
    }

    /// Fire a button/modifier-aware click, then the plain click hooks.
    pub fn pointer_click_button(&mut self, id: usize, x: f64, y: f64, button: u32, state: u32) {
        let mut cbs = self.node_mut(id).map(|n| std::mem::take(&mut n.pointer.click_button)).unwrap_or_default();
        for cb in cbs.iter_mut() {
            cb(x, y, button, state);
        }
        if let Some(n) = self.node_mut(id) {
            n.pointer.click_button = cbs;
        }
        // A press is the gesture stream's "the drag starts here". Recorded
        // before the plain click so a gesture handler that inspects the state
        // already sees the button down.
        if let Some(n) = self.node_mut(id) {
            n.pointer.down_button = Some(Self::gesture_button(button));
        }
        self.fire_gesture(
            id,
            crate::core::Gesture::Button {
                button: Self::gesture_button(button),
                pressed: true,
                x,
                y,
                mods: Self::gesture_modifiers(state),
            },
        );
        self.pointer_click(id, x, y);
    }

    /// Fire a motion event. Returns whether any hook consumed it.
    pub fn pointer_motion(&mut self, id: usize, x: f64, y: f64, state: u32) {
        let mut cbs = self.node_mut(id).map(|n| std::mem::take(&mut n.pointer.motion)).unwrap_or_default();
        for cb in cbs.iter_mut() {
            cb(x, y, state);
        }
        if let Some(n) = self.node_mut(id) {
            n.pointer.motion = cbs;
        }
        // The gesture stream distinguishes a drag (a button is held) from a
        // hover (none is), which is exactly the distinction the raw hook cannot
        // express — GTK delivers plain `motion-notify` only with no button
        // held, and a host that wanted drags had to add the button-motion mask
        // and reassemble the events by hand.
        let down = self.gesture_button_down(id);
        let gesture = match down {
            Some(_) => crate::core::Gesture::Drag {
                x,
                y,
                mods: Self::gesture_modifiers(state),
            },
            None => crate::core::Gesture::Hover {
                x,
                y,
                mods: Self::gesture_modifiers(state),
            },
        };
        self.fire_gesture(id, gesture);
    }

    /// Fire a button release.
    pub fn pointer_release(&mut self, id: usize, x: f64, y: f64, button: u32, state: u32) {
        let mut cbs = self.node_mut(id).map(|n| std::mem::take(&mut n.pointer.release)).unwrap_or_default();
        for cb in cbs.iter_mut() {
            cb(x, y, button, state);
        }
        if let Some(n) = self.node_mut(id) {
            n.pointer.release = cbs;
        }
        let gesture = crate::core::Gesture::Button {
            button: Self::gesture_button(button),
            pressed: false,
            x,
            y,
            mods: Self::gesture_modifiers(state),
        };
        self.fire_gesture(id, gesture);
        // Cleared *after* firing, so a handler that inspects the state during
        // the release still sees which button it was.
        if let Some(n) = self.node_mut(id) {
            n.pointer.down_button = None;
        }
    }

    /// Deliver a scroll to the gesture stream as a `Gesture::Scroll`.
    ///
    /// A widget's own `on_scroll` keeps its `(vertical, pos)` shape — that is
    /// a *position*, not a delta, and conflating the two is the mistake the
    /// `ScrollDelta` type exists to prevent. The gesture stream wants the
    /// delta, so `dy` is the movement and the two are separate on purpose.
    pub fn pointer_scroll(&mut self, id: usize, dx: f64, dy: f64, x: f64, y: f64, state: u32) {
        let gesture = crate::core::Gesture::Scroll {
            delta: crate::core::ScrollDelta { dx, dy },
            x,
            y,
            mods: Self::gesture_modifiers(state),
        };
        self.fire_gesture(id, gesture);
    }

    /// Fire a gesture at the node's gesture handlers, with the borrow released
    /// so a handler may re-enter the model.
    pub fn fire_gesture(&mut self, id: usize, g: crate::core::Gesture) {
        let mut cbs = self.node_mut(id).map(|n| std::mem::take(&mut n.gesture_cbs)).unwrap_or_default();
        for cb in cbs.iter_mut() {
            cb(g);
        }
        if let Some(n) = self.node_mut(id) {
            n.gesture_cbs = cbs;
        }
    }

    /// Deliver a key to the canvas. Returns true when a hook consumed it.
    /// The plain `key` hooks run first; if one returns true the `key_raw` hooks
    /// are skipped, matching the adapter chain order.
    pub fn key(&mut self, id: usize, keyval: u32) -> bool {
        let mut consumed = false;
        let mut cbs = self.node_mut(id).map(|n| std::mem::take(&mut n.pointer.key)).unwrap_or_default();
        for cb in cbs.iter_mut() {
            if cb(keyval) {
                consumed = true;
            }
        }
        if let Some(n) = self.node_mut(id) {
            n.pointer.key = cbs;
        }
        if consumed {
            return true;
        }
        let mut raw = self.node_mut(id).map(|n| std::mem::take(&mut n.pointer.key_raw)).unwrap_or_default();
        for cb in raw.iter_mut() {
            if cb(keyval, 0) {
                consumed = true;
            }
        }
        if let Some(n) = self.node_mut(id) {
            n.pointer.key_raw = raw;
        }
        consumed
    }

    // -- Overlay --

    /// Push a child onto an overlay's layer stack, above the base child.
    pub fn overlay_add(&mut self, overlay_id: usize, child_id: usize) {
        if overlay_id == child_id {
            return;
        }
        if let Some(old) = self.node(child_id).and_then(|n| n.parent) {
            if let Some(p) = self.node_mut(old) {
                p.children.retain(|c| *c != child_id);
            }
        }
        self.nodes.iter_mut().for_each(|n| {
            n.children.retain(|c| *c != child_id);
            n.overlays.retain(|c| *c != child_id);
        });
        if let Some(n) = self.node_mut(overlay_id) {
            if !n.overlays.contains(&child_id) {
                n.overlays.push(child_id);
            }
        }
        if let Some(c) = self.node_mut(child_id) {
            c.parent = Some(overlay_id);
        }
    }

    /// Set whether pointer events pass through an overlay layer to the base.
    pub fn overlay_set_pass_through(&mut self, overlay_id: usize, child_id: usize, pass: bool) {
        if let Some(n) = self.node_mut(overlay_id) {
            n.overlay_pass_through.insert(child_id, pass);
        }
    }

    /// The overlay's layers, base child first, then the stacked overlays.
    pub fn overlay_layers(&self, id: usize) -> Vec<usize> {
        self.node(id).map(|n| n.overlays.clone()).unwrap_or_default()
    }

    /// Make every descendant visible. Mirrors `GtkOverlay::show_all`.
    pub fn show_all(&mut self, id: usize) {
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            let kids = self.node(cur).map(|n| n.children.clone()).unwrap_or_default();
            if let Some(n) = self.node_mut(cur) {
                n.props.visible = true;
            }
            stack.extend(kids);
        }
    }

    // -- Dialog --

    pub fn dialog_add_button(&mut self, id: usize, label: &str, response_id: i32) {
        if let Some(n) = self.node_mut(id) {
            n.dialog_buttons.push((label.to_string(), response_id));
        }
    }

    pub fn dialog_set_default_response(&mut self, id: usize, response_id: i32) {
        if let Some(n) = self.node_mut(id) {
            n.dialog_default_response = Some(response_id);
        }
    }

    pub fn dialog_buttons(&self, id: usize) -> Vec<(String, i32)> {
        self.node(id).map(|n| n.dialog_buttons.clone()).unwrap_or_default()
    }

    pub fn dialog_set_transient_for(&mut self, id: usize, parent: Option<usize>) {
        if let Some(n) = self.node_mut(id) {
            n.dialog_transient_for = parent;
        }
    }

    /// The content extent of a dialog: the union of its children's size
    /// requests, so a caller laying the dialog out itself has something real to
    /// read. `None` when the dialog has no children.
    pub fn measure_dialog(&self, id: usize) -> Option<(i32, i32)> {
        let n = self.node(id)?;
        let mut any = false;
        let (mut w, mut h) = (0i32, 0i32);
        for c in &n.children {
            let Some(c) = self.node(*c) else { continue };
            if !c.props.visible {
                continue;
            }
            any = true;
            w = w.max(c.props.width.unwrap_or(0) + c.props.margin_start);
            h = h.max(c.props.height.unwrap_or(0) + c.props.margin_top);
        }
        any.then_some((w, h))
    }

    /// The default response id, used by `Dialog::run` when there is no nested
    /// loop to wait on.
    pub fn dialog_default_response(&self, id: usize) -> Option<i32> {
        self.node(id).and_then(|n| n.dialog_default_response)
    }

    pub fn dialog_transient_for(&self, id: usize) -> Option<usize> {
        self.node(id).and_then(|n| n.dialog_transient_for)
    }

    /// Mark a dialog as destroyed and hide it.
    pub fn dialog_mark_destroyed(&mut self, id: usize) {
        if let Some(n) = self.node_mut(id) {
            n.destroyed = true;
            n.props.visible = false;
        }
    }

    /// Register a dialog response handler, called as `cb(response_id)`.
    pub fn add_response_callback(&mut self, id: usize, cb: Box<dyn FnMut(i32)>) {
        if let Some(n) = self.node_mut(id) {
            n.response_cbs.push(cb);
        }
    }

    /// Deliver a dialog response. Only a response id registered with
    /// [`Self::dialog_add_button`] reaches the handlers — an unregistered id is
    /// ignored rather than silently firing, which is what the old
    /// `connect_response` no-op could not express.
    /// Dismiss a dialog with no button and report `dialog_default_response`.
    ///
    /// The counterpart to [`Self::dialog_respond`], which requires a
    /// registered button. GTK reports `GTK_RESPONSE_DELETE_EVENT` for a
    /// dismissal with no button when no default was set, so the same two
    /// answers exist here: the recorded default if there is one, and 0 --
    /// which is not a valid GTK response id, so a caller can tell "no
    /// default" from a real id unambiguously.
    ///
    /// This is what gives `Dialog::set_default_response` a reason to exist: it
    /// was previously stored and never read, so a dismissal reported nothing.
    pub fn dialog_dismiss(&mut self, id: usize) {
        let response = self
            .node(id)
            .and_then(|n| n.dialog_default_response)
            .unwrap_or(0);
        self.dialog_respond(id, response);
    }

    pub fn dialog_respond(&mut self, id: usize, response_id: i32) {
        let known = self
            .node(id)
            .map(|n| n.dialog_buttons.iter().any(|(_, r)| *r == response_id))
            .unwrap_or(false);
        if !known {
            return;
        }
        let mut cbs = self.node_mut(id).map(|n| std::mem::take(&mut n.response_cbs)).unwrap_or_default();
        for cb in cbs.iter_mut() {
            cb(response_id);
        }
        if let Some(n) = self.node_mut(id) {
            n.response_cbs = cbs;
        }
    }

    // -- Spreadsheet --

    pub fn sheet_set_cell(&mut self, id: usize, row: u32, col: u32, text: &str, style: u8) {
        if let Some(n) = self.node_mut(id) {
            n.cells.insert((row, col), SheetCell { text: text.to_string(), style, raw: false });
        }
    }

    pub fn sheet_set_raw_cell(&mut self, id: usize, row: u32, col: u32, text: &str) {
        if let Some(n) = self.node_mut(id) {
            n.cells.insert((row, col), SheetCell { text: text.to_string(), style: 0, raw: true });
        }
    }

    pub fn sheet_get_cell(&self, id: usize, row: u32, col: u32) -> Option<String> {
        self.node(id).and_then(|n| n.cells.get(&(row, col)).map(|c| c.text.clone()))
    }

    pub fn sheet_set_border_title(&mut self, id: usize, title: &str) {
        if let Some(n) = self.node_mut(id) {
            if let ZorkKind::Spreadsheet { border_title } = &mut n.kind {
                *border_title = title.to_string();
            }
        }
    }

    pub fn sheet_border_title(&self, id: usize) -> Option<String> {
        self.node(id).and_then(|n| match &n.kind {
            ZorkKind::Spreadsheet { border_title } => Some(border_title.clone()),
            _ => None,
        })
    }

    // -- Spreadsheet chrome -------------------------------------------------
    //
    // One setter per field of `SheetChrome`. They are deliberately separate
    // functions rather than a single `set_chrome` taking the whole struct: the
    // other backends' `Spreadsheet` surface is a list of narrow setters, and
    // the point of implementing them is that shared render code can issue the
    // same call on any backend.

    /// The whole chrome in one read, for a test or a draw callback.
    pub fn sheet_chrome(&self, id: usize) -> Option<SheetChrome> {
        self.node(id).map(|n| n.chrome.clone())
    }

    pub fn sheet_set_menu_text(&mut self, id: usize, text: &str) {
        if let Some(n) = self.node_mut(id) {
            n.chrome.menu_text = text.to_string();
        }
    }

    pub fn sheet_menu_text(&self, id: usize) -> Option<String> {
        self.node(id).map(|n| n.chrome.menu_text.clone())
    }

    pub fn sheet_set_status_text(&mut self, id: usize, text: &str) {
        if let Some(n) = self.node_mut(id) {
            n.chrome.status_text = text.to_string();
        }
    }

    pub fn sheet_status_text(&self, id: usize) -> Option<String> {
        self.node(id).map(|n| n.chrome.status_text.clone())
    }

    pub fn sheet_set_formula_bar_trailing(&mut self, id: usize, text: &str) {
        if let Some(n) = self.node_mut(id) {
            n.chrome.formula_bar_trailing = text.to_string();
        }
    }

    pub fn sheet_formula_bar_trailing(&self, id: usize) -> Option<String> {
        self.node(id).map(|n| n.chrome.formula_bar_trailing.clone())
    }

    /// The formula bar as a whole: the address cell and the entry text.
    ///
    /// One call rather than two, because the address and the entry are not
    /// independently meaningful — they describe the same cell. Taking them
    /// separately would let a caller set an address and an entry for two
    /// different cells, which is the state the widget can never be in.
    pub fn sheet_set_formula_bar(&mut self, id: usize, address: &str, entry: &str) {
        if let Some(n) = self.node_mut(id) {
            n.chrome.formula_address = address.to_string();
            n.chrome.formula_entry = entry.to_string();
        }
    }

    pub fn sheet_formula_bar(&self, id: usize) -> Option<(String, String)> {
        self.node(id).map(|n| (n.chrome.formula_address.clone(), n.chrome.formula_entry.clone()))
    }

    pub fn sheet_set_grid_config(&mut self, id: usize, margin_cols: u32, main_cols: u32) {
        if let Some(n) = self.node_mut(id) {
            n.chrome.margin_cols = margin_cols;
            n.chrome.main_cols = main_cols;
        }
    }

    pub fn sheet_set_row_counts(&mut self, id: usize, header_rows: u32, main_rows: u32) {
        if let Some(n) = self.node_mut(id) {
            n.chrome.header_rows = header_rows;
            n.chrome.main_rows = main_rows;
        }
    }

    /// Replace the column layout wholesale, which is what every frame does:
    /// column widths change as the user resizes, so an incremental setter
    /// would need a diff the callers have no reason to write.
    pub fn sheet_set_column_layout(&mut self, id: usize, layout: Vec<(u32, u32, String)>) {
        if let Some(n) = self.node_mut(id) {
            n.chrome.column_layout = layout;
        }
    }

    pub fn sheet_set_row_labels(&mut self, id: usize, labels: Vec<(u32, String)>) {
        if let Some(n) = self.node_mut(id) {
            n.chrome.row_labels = labels;
        }
    }

    /// The tab strip. `active` is clamped to the tab list so a caller that
    /// removed the active tab gets the last tab rather than an index that
    /// points past the end — the same reason [`Self::set_dropdown_selected`]
    /// drops an out-of-range selection.
    pub fn sheet_set_tab_data(&mut self, id: usize, titles: &[String], active: usize) {
        if let Some(n) = self.node_mut(id) {
            n.chrome.tabs = titles.to_vec();
            n.chrome.active_tab = if titles.is_empty() {
                0
            } else {
                active.min(titles.len() - 1)
            };
        }
    }

    /// Move the cursor, clamped to the sheet's known extent.
    ///
    /// The clamp is the load-bearing part: `set_cursor` is called with
    /// whatever the caller computed, and a cursor one row past the last row
    /// is a state the widget can never be in. When the extent is unknown
    /// (`main_rows == 0`, i.e. the caller never said) the value is kept as-is
    /// rather than collapsed to the origin, so a spreadsheet driven before its
    /// geometry is configured still remembers where it was put.
    pub fn sheet_set_cursor(&mut self, id: usize, row: u32, col: u32) {
        // Cells are addressed 1-based, so a cursor of `0` would be a position
        // no cell occupies. Floor at 1 in both axes; without this a caller
        // passing a 0-based row — the obvious mistake, and the convention the
        // cursor used to have — would silently park the cursor outside the grid.
        let (mut r, mut c) = (row.max(1), col.max(1));
        if let Some(n) = self.node_mut(id) {
            if n.chrome.main_rows > 0 && n.chrome.header_rows > 0 {
                let last = n.chrome.main_rows + n.chrome.header_rows;
                r = r.min(last);
            }
            if n.chrome.main_cols > 0 && n.chrome.margin_cols > 0 {
                let last = n.chrome.main_cols + n.chrome.margin_cols;
                c = c.min(last);
            }
            n.chrome.cursor = (r, c);
        }
    }

    /// Every set cell as `(row, col, text)`, in address order.
    ///
    /// Sorted, so a reader that renders or diffs the sheet gets the same order
    /// every time — a `HashMap` iteration order would make the grid reshuffle
    /// between runs, which defeats the point of printing it.
    pub fn sheet_cells(&self, id: usize) -> Vec<(u32, u32, String)> {
        let Some(n) = self.node(id) else { return Vec::new() };
        let mut out: Vec<(u32, u32, String)> = n
            .cells
            .iter()
            .map(|((r, c), cell)| (*r, *c, cell.text.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        out
    }

    /// How many cells are set on a spreadsheet.
    pub fn sheet_cell_count(&self, id: usize) -> usize {
        self.node(id).map(|n| n.cells.len()).unwrap_or(0)
    }

    pub fn sheet_cursor(&self, id: usize) -> Option<(u32, u32)> {
        self.node(id).map(|n| n.chrome.cursor)
    }

    /// Set the selection anchor, which the drag handlers extend from.
    pub fn sheet_set_anchor(&mut self, id: usize, anchor: Option<(u32, u32)>) {
        if let Some(n) = self.node_mut(id) {
            n.chrome.anchor = anchor;
        }
    }

    /// Begin (or end) editing the cursor cell.
    ///
    /// `edit_buf` seeds the in-progress text and `edit_pos` the caret inside
    /// it. Passing an empty buffer with `editing = true` starts an edit from
    /// the cell's current value, which is what a real "press F2" does — the
    /// alternative is a blank that silently destroys the cell's contents if
    /// the user types nothing and commits.
    pub fn sheet_set_editing(&mut self, id: usize, editing: bool, edit_buf: &str, edit_pos: usize) {
        let seed = {
            let Some(n) = self.node(id) else { return };
            n.cells.get(&n.chrome.cursor).map(|c| c.text.clone())
        };
        if let Some(n) = self.node_mut(id) {
            n.chrome.editing = editing;
            if editing {
                let buf = if edit_buf.is_empty() {
                    seed.unwrap_or_default()
                } else {
                    edit_buf.to_string()
                };
                n.chrome.edit_buf = buf;
                n.chrome.edit_pos = edit_pos.min(n.chrome.edit_buf.chars().count());
            } else {
                n.chrome.edit_buf.clear();
                n.chrome.edit_pos = 0;
            }
        }
    }

    pub fn sheet_edit_state(&self, id: usize) -> Option<(bool, String, usize)> {
        self.node(id).map(|n| (n.chrome.editing, n.chrome.edit_buf.clone(), n.chrome.edit_pos))
    }

    /// Commit the in-progress edit into the cursor cell.
    ///
    /// Writes both the display cell and the raw cell, because the raw cell is
    /// what the model saves and the display cell is what it paints; leaving
    /// them out of step is how a formula ends up rendering differently from
    /// what it evaluates to. A no-op when not editing, matching
    /// `core::Grid::commit_edit`.
    ///
    /// Does **not** fire the commit hook: this is the mutation, and
    /// [`Self::sheet_fire_commit_edit`] is the notification, so a caller that
    /// commits without a handler and a caller whose handler is absent take the
    /// same path through the model.
    pub fn sheet_commit_edit(&mut self, id: usize) -> Option<(u32, u32, String)> {
        let Some(n) = self.node_mut(id) else { return None };
        if !n.chrome.editing {
            return None;
        }
        let (row, col) = n.chrome.cursor;
        let text = n.chrome.edit_buf.clone();
        n.cells.insert((row, col), SheetCell { text: text.clone(), style: 0, raw: false });
        n.chrome.editing = false;
        n.chrome.edit_buf.clear();
        n.chrome.edit_pos = 0;
        Some((row, col, text))
    }

    // -- Spreadsheet input hooks -------------------------------------------

    /// Register a bare-keypress handler (a function key, a shortcut).
    pub fn sheet_add_key_callback(&mut self, id: usize, cb: Box<dyn FnMut()>) {
        if let Some(n) = self.node_mut(id) {
            n.sheet_input.key.push(cb);
        }
    }

    /// Register a "navigate to a cell" handler.
    pub fn sheet_add_goto_callback(&mut self, id: usize, cb: Box<dyn FnMut()>) {
        if let Some(n) = self.node_mut(id) {
            n.sheet_input.goto.push(cb);
        }
    }

    /// Register a cursor-moved handler, called as `cb(row, col)`.
    pub fn sheet_add_cursor_move_callback(&mut self, id: usize, cb: Box<dyn FnMut(u32, u32)>) {
        if let Some(n) = self.node_mut(id) {
            n.sheet_input.cursor_move.push(cb);
        }
    }

    /// Register a commit handler, called as `cb(row, col, text)`.
    pub fn sheet_add_commit_edit_callback(&mut self, id: usize, cb: Box<dyn FnMut(u32, u32, String)>) {
        if let Some(n) = self.node_mut(id) {
            n.sheet_input.commit_edit.push(cb);
        }
    }

    /// How many handlers each of the four input events has, for assertions.
    pub fn sheet_hook_counts(&self, id: usize) -> Option<(usize, usize, usize, usize)> {
        self.node(id).map(|n| {
            (
                n.sheet_input.key.len(),
                n.sheet_input.goto.len(),
                n.sheet_input.cursor_move.len(),
                n.sheet_input.commit_edit.len(),
            )
        })
    }

    // -- Menu / action dispatch --

    /// Fire the `SimpleAction` a menu item names, if the action was created
    /// with [`Self::create_simple_action`]. Returns true when an action fired.
    ///
    /// The old `Harness::select` fired the *menu node's* own callbacks, which
    /// meant a menu item's action string was decorative. Resolving the name is
    /// what makes the menu model meaningful.
    pub fn activate_action(&mut self, action_name: &str) -> bool {
        let id = match self.action_names.get(action_name) {
            Some(id) => *id,
            None => return false,
        };
        self.click(id);
        true
    }

    /// Select menu item `index` (0-based) on `menu_id` and dispatch its action.
    ///
    /// Returns the item's label, or `None` when the index is out of range or
    /// the item is a separator/section (which are not selectable).
    pub fn menu_select(&mut self, menu_id: usize, index: usize) -> Option<String> {
        let item = self.menu_items.get(&menu_id).and_then(|i| i.get(index)).cloned()?;
        if item.enabled == false || matches!(item.kind, MenuItemKind::Separator | MenuItemKind::Section) {
            return None;
        }
        // A check/radio item flips before its action runs, so a handler that
        // reads the menu sees the new state.
        if matches!(item.kind, MenuItemKind::Check | MenuItemKind::Radio) {
            let new_state = !item.checked;
            self.set_menu_item_checked(menu_id, index, new_state);
        }
        if !item.action.is_empty() {
            self.activate_action(&item.action);
        }
        Some(item.label)
    }

    /// Whether a keyboard menu is open. The model has no popup concept, so this
    /// is always false — matching the NWG/pancurses no-op — but it exists so
    /// the adapter can answer honestly instead of omitting the method.
    pub fn menu_active(&self) -> bool {
        false
    }

    // -- Fire --

    // -- Detach / reattach ------------------------------------------------
    //
    // A user callback may call back into the model (corro's draw callback
    // calls `scroll_to` on every frame). If it ran while a `&mut ZorkState`
    // were live, the nested access would either panic on a `RefCell` borrow or
    // mutate a detached copy whose writes are then thrown away. So the
    // discipline is: take the closure out, *release the borrow*, run it, then
    // put it back. The `take_*`/`put_*` pairs below are that, and the facade
    // sequences them around the call.

    /// Take a node's callbacks, leaving the node with none.
    pub fn take_callbacks(s: &mut ZorkState, id: usize) -> Vec<Callback> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.callbacks)).unwrap_or_default()
    }
    /// Put a node's callbacks back after they were run.
    pub fn put_callbacks(s: &mut ZorkState, id: usize, cbs: Vec<Callback>) {
        if let Some(n) = s.node_mut(id) {
            n.callbacks = cbs;
        }
    }

    /// Take a canvas's draw callback and its drawable size, for running with
    /// the model borrow released. Pairs with [`Self::put_draw`].
    pub fn take_draw(s: &mut ZorkState, id: usize) -> DrawSlot {
        let (w, h) = s.canvas_size(id);
        let d = s.node_mut(id).and_then(|n| n.draw.take());
        DrawSlot(d.map(|d| (d, w, h)))
    }
    /// Put a canvas's draw callback back, if the slot still holds one.
    pub fn put_draw(s: &mut ZorkState, id: usize, slot: DrawSlot) {
        if let Some(n) = s.node_mut(id) {
            n.draw = slot.0.map(|(d, _, _)| d);
        }
    }

    /// Take a canvas's plain click hooks.
    pub fn take_click_hooks(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut(f64, f64)>> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.pointer.click)).unwrap_or_default()
    }
    pub fn put_click_hooks(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut(f64, f64)>>) {
        if let Some(n) = s.node_mut(id) {
            n.pointer.click = cbs;
        }
    }
    /// Take a canvas's button/modifier click hooks.
    pub fn take_click_button_hooks(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut(f64, f64, u32, u32)>> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.pointer.click_button)).unwrap_or_default()
    }
    pub fn put_click_button_hooks(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut(f64, f64, u32, u32)>>) {
        if let Some(n) = s.node_mut(id) {
            n.pointer.click_button = cbs;
        }
    }
    /// Take a canvas's motion hooks.
    pub fn take_motion_hooks(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut(f64, f64, u32)>> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.pointer.motion)).unwrap_or_default()
    }
    pub fn put_motion_hooks(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut(f64, f64, u32)>>) {
        if let Some(n) = s.node_mut(id) {
            n.pointer.motion = cbs;
        }
    }
    /// Take a canvas's release hooks.
    pub fn take_release_hooks(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut(f64, f64, u32, u32)>> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.pointer.release)).unwrap_or_default()
    }
    pub fn put_release_hooks(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut(f64, f64, u32, u32)>>) {
        if let Some(n) = s.node_mut(id) {
            n.pointer.release = cbs;
        }
    }
    /// Take a canvas's key hooks, split into plain and raw.
    pub fn take_key_hooks(s: &mut ZorkState, id: usize) -> (Vec<Box<dyn FnMut(u32) -> bool>>, Vec<Box<dyn FnMut(u32, u32) -> bool>>) {
        let n = s.node_mut(id);
        match n {
            Some(n) => (
                std::mem::take(&mut n.pointer.key),
                std::mem::take(&mut n.pointer.key_raw),
            ),
            None => (Vec::new(), Vec::new()),
        }
    }
    pub fn put_key_hooks(s: &mut ZorkState, id: usize, key: Vec<Box<dyn FnMut(u32) -> bool>>, raw: Vec<Box<dyn FnMut(u32, u32) -> bool>>) {
        if let Some(n) = s.node_mut(id) {
            n.pointer.key = key;
            n.pointer.key_raw = raw;
        }
    }
    /// Take a canvas's gesture handlers.
    pub fn take_gesture_hooks(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut(crate::core::Gesture)>> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.gesture_cbs)).unwrap_or_default()
    }
    pub fn put_gesture_hooks(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut(crate::core::Gesture)>>) {
        if let Some(n) = s.node_mut(id) {
            n.gesture_cbs = cbs;
        }
    }
    /// Take a scrollable's scroll handlers.
    pub fn take_scroll_hooks(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut(bool, f64)>> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.scroll_cbs)).unwrap_or_default()
    }
    pub fn put_scroll_hooks(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut(bool, f64)>>) {
        if let Some(n) = s.node_mut(id) {
            n.scroll_cbs = cbs;
        }
    }
    /// Take a dialog's response handlers.
    pub fn take_response_hooks(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut(i32)>> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.response_cbs)).unwrap_or_default()
    }
    pub fn put_response_hooks(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut(i32)>>) {
        if let Some(n) = s.node_mut(id) {
            n.response_cbs = cbs;
        }
    }
    /// Take a spreadsheet's bare-key handlers.
    pub fn take_sheet_key_hooks(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut()>> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.sheet_input.key)).unwrap_or_default()
    }
    pub fn put_sheet_key_hooks(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut()>>) {
        if let Some(n) = s.node_mut(id) {
            n.sheet_input.key = cbs;
        }
    }
    /// Take a spreadsheet's "navigate to a cell" handlers.
    pub fn take_sheet_goto_hooks(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut()>> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.sheet_input.goto)).unwrap_or_default()
    }
    pub fn put_sheet_goto_hooks(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut()>>) {
        if let Some(n) = s.node_mut(id) {
            n.sheet_input.goto = cbs;
        }
    }
    /// Take a spreadsheet's cursor-moved handlers.
    pub fn take_sheet_cursor_move_hooks(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut(u32, u32)>> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.sheet_input.cursor_move)).unwrap_or_default()
    }
    pub fn put_sheet_cursor_move_hooks(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut(u32, u32)>>) {
        if let Some(n) = s.node_mut(id) {
            n.sheet_input.cursor_move = cbs;
        }
    }
    /// Take a spreadsheet's commit handlers.
    pub fn take_sheet_commit_hooks(s: &mut ZorkState, id: usize) -> Vec<Box<dyn FnMut(u32, u32, String)>> {
        s.node_mut(id).map(|n| std::mem::take(&mut n.sheet_input.commit_edit)).unwrap_or_default()
    }
    pub fn put_sheet_commit_hooks(s: &mut ZorkState, id: usize, cbs: Vec<Box<dyn FnMut(u32, u32, String)>>) {
        if let Some(n) = s.node_mut(id) {
            n.sheet_input.commit_edit = cbs;
        }
    }
    /// Whether a dialog has a response handler for `response_id` registered by
    /// `dialog_add_button`.
    pub fn dialog_has_response(&self, id: usize, response_id: i32) -> bool {
        self.node(id)
            .map(|n| n.dialog_buttons.iter().any(|(_, r)| *r == response_id))
            .unwrap_or(false)
    }

    /// Fire the callbacks registered on a node. Does nothing for a missing id.
    ///
    /// Safe only when no `&mut` to the model is live and the callbacks do not
    /// re-enter — which is the case for the harness (it owns its own
    /// `Rc<RefCell<_>>` and already releases the borrow) but *not* for the
    /// facade, so the facade uses the `take_*`/`put_*` pairs above instead.
    pub fn click(&mut self, id: usize) {
        let mut cbs = match self.node_mut(id) {
            Some(n) => std::mem::take(&mut n.callbacks),
            None => return,
        };
        for cb in cbs.iter_mut() {
            cb();
        }
        if let Some(n) = self.node_mut(id) {
            n.callbacks = cbs;
        }
    }

    /// Convenience: type into an entry and fire `changed` callbacks.
    pub fn type_into(&mut self, id: usize, text: &str) {
        self.set_entry_text(id, text);
        self.click(id);
    }

    /// Convenience: toggle a check/radio button and fire its callbacks.
    pub fn toggle(&mut self, id: usize) {
        let became = match self.node_mut(id) {
            Some(n) => match &mut n.kind {
                ZorkKind::CheckButton { ref mut checked, .. } => {
                    *checked = !*checked;
                    Some(*checked)
                }
                ZorkKind::RadioButton { ref mut checked, .. } => {
                    *checked = !*checked;
                    Some(*checked)
                }
                _ => None,
            },
            None => None,
        };
        if became.is_some() {
            // Re-apply through the radio-aware setter so the group stays
            // mutually exclusive.
            if let Some(ZorkKind::RadioButton { .. }) = self.node(id).map(|n| &n.kind) {
                let now = self.get_radiobutton_checked(id);
                self.set_radiobutton_checked(id, now);
            }
            self.click(id);
        }
    }

    // -- Light --

    /// Turn the light on or off.
    ///
    /// Going from dark to light clears [`Self::grue_warned`], so the next
    /// `look` in the dark warns again — a dungeon that was lit and then went
    /// dark is a new hazard, not the same one.
    pub fn set_night_mode(&mut self, night: bool) {
        self.night_mode = night;
        if !night {
            self.grue_warned = false;
        }
    }

    /// Whether the dungeon is dark.
    pub fn is_night(&self) -> bool {
        self.night_mode
    }

    /// The Grue warning, or `None` when there is nothing to say: it is light,
    /// or the player has already been warned since the light last changed.
    ///
    /// The warning is latched rather than repeated so a player poking around
    /// with `look` is not nagged on every command, but a light-then-dark cycle
    /// arms it again.
    pub fn grue_warning(&mut self) -> Option<&'static str> {
        if !self.night_mode || self.grue_warned {
            return None;
        }
        self.grue_warned = true;
        Some(
            "It is pitch dark, and you feel a presence. A grue lurks in the \
             darkness here, and it is very likely to eat you.",
        )
    }

    /// The dark-room description, used when the player cannot see. A lit room
    /// is described normally, so this is only reached under night mode.
    pub fn dark_room_description(&self) -> &'static str {
        "You are in a dark room you cannot see. If a grue is here, it will \
         surely eat you. Light the lantern with 'light' before you go further."
    }

    // -- Timers ------------------------------------------------------------
    //
    // GTK's `g_timeout_add` hands the closure to the main context and the loop
    // calls it when the time comes; NWG's `SetTimer` posts `WM_TIMER`. Zork
    // has no main context, so the closure is registered here and fired by
    // [`Self::advance_timers`] — the driver *is* the loop. A caller that never
    // advances the clock registers a timer that never fires, which is the same
    // situation as registering one and then exiting the loop.

    /// Run `f` once, `ms` virtual milliseconds from now.
    pub fn timeout_add_once(&mut self, ms: u32, f: Box<dyn FnOnce()>) {
        let due = self.now_ms + ms as u64;
        self.timers.push(Timer {
            due_at: due,
            interval_ms: ms,
            handler: None,
            once: Some(f),
            window_id: None,
        });
    }

    /// Run `f` every `ms` virtual milliseconds until it returns `false`.
    pub fn timeout_add_repeating(&mut self, ms: u32, f: Box<dyn FnMut() -> bool>) {
        let due = self.now_ms + ms as u64;
        self.timers.push(Timer {
            due_at: due,
            interval_ms: ms,
            handler: Some(f),
            once: None,
            window_id: None,
        });
    }

    /// A repeating timer owned by `window_id` — `Window::start_repeating_timer`.
    pub fn timeout_add_repeating_for_window(
        &mut self,
        window_id: usize,
        id: usize,
        ms: u32,
        f: Box<dyn FnMut() -> bool>,
    ) {
        let due = self.now_ms + ms as u64;
        self.timers.push(Timer {
            due_at: due,
            interval_ms: ms,
            handler: Some(f),
            once: None,
            window_id: Some(window_id),
        });
        // `id` is the *caller's* handle. NWG returns the handler id so the
        // caller can cancel the timer, and conflating it with the window would
        // make two timers on one window indistinguishable; record it so
        // [`Self::cancel_timer`] can find the right one.
        self.timer_ids.insert(id, self.timers.len() - 1);
    }

    /// Stop the timer the caller registered under `id`.
    pub fn cancel_timer(&mut self, id: usize) -> bool {
        let Some(idx) = self.timer_ids.remove(&id) else {
            return false;
        };
        // Indices shift when a timer is dropped, so rebuild the map rather than
        // decrementing the survivors: a stale index would cancel the wrong
        // timer, which is worse than failing to cancel one.
        self.timer_ids.clear();
        if idx < self.timers.len() {
            self.timers.remove(idx);
            for (handle, i) in (0..self.timers.len()).zip(self.handles_by_index()) {
                self.timer_ids.insert(handle, i);
            }
        }
        true
    }

    fn handles_by_index(&self) -> Vec<usize> {
        // Reverse of `timer_ids`, so a rebuilt map keeps each surviving
        // timer's handle pointing at its own new index.
        let mut by_index = vec![usize::MAX; self.timers.len()];
        for (handle, idx) in &self.timer_ids {
            if *idx < by_index.len() {
                by_index[*idx] = *handle;
            }
        }
        by_index
    }

    /// Remove every timer due at or before `now + ms` and return it with the
    /// clock left at `now + ms`.
    ///
    /// Draining rather than firing is deliberate, and it is the same
    /// discipline the callback paths use: a timer body routinely touches
    /// widgets (a poll that redraws, a tick that activates an action), so it
    /// must not run while a `&mut ZorkState` is live. The facade takes the
    /// timers, releases the borrow, runs them, and puts back the ones that
    /// asked to continue — see `facade::advance_timers`.
    pub fn take_due_timers(&mut self, ms: u64) -> Vec<Timer> {
        let target = self.now_ms + ms;
        let mut due = Vec::new();
        // Walk to the end rather than `swap_remove`, so the surviving timers
        // keep their relative order and `timer_ids` only has to shift the
        // indices *below* the removal points.
        let mut i = 0;
        while i < self.timers.len() {
            if self.timers[i].due_at <= target {
                due.push(self.timers.remove(i));
                // Every index at or above `i` shifted down by one.
                for (_, idx) in self.timer_ids.iter_mut() {
                    if *idx >= i {
                        *idx -= 1;
                    }
                }
            } else {
                i += 1;
            }
        }
        self.now_ms = target;
        due
    }

    /// Put a drained timer back, re-armed for its next firing.
    ///
    /// `fired_at` is the deadline the timer actually came due at, not the end
    /// of the jump: re-arming from the end would skip the firings in between,
    /// so advancing 200ms across a 50ms timer would fire it once where a real
    /// main loop fires it four times.
    pub fn put_back_timer(&mut self, mut timer: Timer, fired_at: u64) {
        timer.due_at = fired_at + timer.interval_ms as u64;
        self.timers.push(timer);
    }

    /// Fire a drained timer, and report whether it asked to continue.
    ///
    /// Takes `&mut Timer` rather than the timer by value so the caller can put
    /// the same one back: `Timer` owns its `Box<dyn FnMut>`, and a repeating
    /// timer that was dropped instead of re-armed would fire exactly once.
    /// A one-shot's `Box<dyn FnOnce>` is consumed here on purpose — it cannot
    /// run twice.
    pub fn run_timer(timer: &mut Timer) -> bool {
        let keep = match timer.handler.as_mut() {
            Some(h) => h(),
            None => false,
        };
        if let Some(once) = timer.once.take() {
            once();
        }
        keep
    }

    /// Fire every timer due within `ms` of model time; returns how many
    /// fired.
    ///
    /// The convenience form for a driver that *owns* the `ZorkState` (the
    /// REPL) rather than reaching it through the thread-local: it is the same
    /// take/run/put sequence [`Self::take_due_timers`] splits into, so a timer
    /// body may touch the model. It cannot be the primary implementation —
    /// running a handler needs a `&mut ZorkState`, and the borrow is what the
    /// handler is trying to re-enter — so the primitive is the drain and this
    /// is the loop around it.
    ///
    /// Returns the *total* number of firings, which for a repeating timer is
    /// more than the number of registered timers: advancing 200ms across a
    /// 50ms timer fires it four times, because that is what a real main loop
    /// reaching four timeouts would do. A handler that registers a zero-delay
    /// timer likewise sees it fire in the same call rather than needing a
    /// second `advance`.
    pub fn advance_timers(&mut self, ms: u64) -> usize {
        let mut due = self.take_due_timers(ms);
        let mut fired = 0usize;
        while !due.is_empty() {
            let mut batch = std::mem::take(&mut due);
            fired += batch.len();
            for mut t in batch.drain(..) {
                let keep = Self::run_timer(&mut t);
                if keep {
                    // `Self::run_timer` ran with no other borrow live, so this
                    // re-arm is safe; `t.due_at` is still the deadline it came
                    // due at, which is what a re-arm must be measured from.
                    let at = t.due_at;
                    self.put_back_timer(t, at);
                }
            }
            due = self.take_due_timers(0);
        }
        fired
    }

    /// How many timers are registered, for assertions.
    pub fn timer_count(&self) -> usize {
        self.timers.len()
    }

    /// The virtual clock, in milliseconds since model creation.
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    // -- Application -------------------------------------------------------

    /// Register an action with an `Application` node.
    ///
    /// GTK's `GApplication` holds an action map that a menu bar reads to
    /// resolve an item's action name. Zork's model already resolves names
    /// through `action_names`, so this records *which application* an action
    /// belongs to — which is what makes "does this action exist" answerable
    /// per-application rather than globally.
    ///
    /// Returns the action's name, or `None` if the id is not a
    /// `SimpleAction` node: registering an arbitrary widget as an action would
    /// let a menu item resolve to something that can never fire.
    pub fn application_add_action(&mut self, app_id: usize, action_id: usize) -> Option<String> {
        let name = match self.node(action_id) {
            Some(n) if matches!(n.kind, ZorkKind::SimpleAction) => self
                .action_names
                .iter()
                .find(|(_, id)| **id == action_id)
                .map(|(name, _)| name.clone()),
            _ => None,
        }?;
        self.app_actions.entry(app_id).or_default().push(name.clone());
        Some(name)
    }

    /// The action names registered with `app_id`, in registration order.
    pub fn application_actions(&self, app_id: usize) -> Vec<String> {
        self.app_actions.get(&app_id).cloned().unwrap_or_default()
    }

    // -- Global key callbacks ----------------------------------------------

    /// Register a process-wide handler for a keypress, as
    /// `add_key_callback` does on the other backends.
    ///
    /// Stored on the state rather than a node because the signature takes no
    /// widget: there is nowhere else for it to live.
    pub fn add_key_callback(&mut self, key: char, cb: Callback) {
        self.key_callbacks.entry(key).or_default().push(cb);
    }

    /// How many handlers are registered for `key`.
    pub fn key_callback_count(&self, key: char) -> usize {
        self.key_callbacks.get(&key).map(|v| v.len()).unwrap_or(0)
    }

    /// Take the handlers for `key`, leaving none registered.
    ///
    /// The `take`/`put` pair is what lets [`Self::key_callback_fired`] fire
    /// them with no borrow held; the handlers are persistent, so they are put
    /// straight back rather than consumed the way a one-shot is.
    pub fn take_key_callbacks(s: &mut ZorkState, key: char) -> Vec<Callback> {
        s.key_callbacks.remove(&key).unwrap_or_default()
    }

    pub fn put_key_callbacks(s: &mut ZorkState, key: char, cbs: Vec<Callback>) {
        if !cbs.is_empty() {
            s.key_callbacks.entry(key).or_default().extend(cbs);
        }
    }

    /// [`Self::take_key_callbacks`] paired with [`Self::put_key_callbacks`]:
    /// the facade calls them as two `with_state` borrows rather than through
    /// `with_callbacks`, which is shaped around a node id.

    /// Fire the handlers for `key`. Returns how many ran.
    pub fn key_callback_fired(&mut self, key: char) -> usize {
        let mut cbs = Self::take_key_callbacks(self, key);
        let n = cbs.len();
        for cb in cbs.iter_mut() {
            cb();
        }
        Self::put_key_callbacks(self, key, cbs);
        n
    }

    pub fn quit(&mut self) {
        self.running = false;
    }

    /// Produce a serialization-friendly snapshot of the current model.
    ///
    /// This is the *only* sanctioned JSON surface: it is used for snapshot
    /// regression tests and for external (non-Rust) drivers that want to inspect
    /// the model without parsing the REPL's prose. It deliberately does not
    /// include callbacks — only their counts.
    pub fn snapshot(&self) -> Snapshot {
        let nodes = self
            .nodes
            .iter()
            .map(|n| {
                let mut s = SnapshotNode {
                    id: n.id,
                    kind: n.kind.name().to_string(),
                    parent: n.parent,
                    children: n.children.clone(),
                    title: None,
                    text: None,
                    label: None,
                    checked: None,
                    selected: None,
                    items: None,
                    props: n.props.clone(),
                    can_focus: n.can_focus,
                    callbacks: n.callbacks.len(),
                    click_hooks: n.pointer.click.len() + n.pointer.click_button.len(),
                    redraws: n.pointer.redraws,
                    cells: n
                        .cells
                        .iter()
                        .map(|((row, col), c)| SheetCellEntry {
                            row: *row,
                            col: *col,
                            text: c.text.clone(),
                            style: c.style,
                            raw: c.raw,
                        })
                        .collect(),
                    dialog_buttons: n.dialog_buttons.clone(),
                    overlays: n.overlays.clone(),
                    chrome: matches!(n.kind, ZorkKind::Spreadsheet { .. }).then(|| n.chrome.clone()),
                    sheet_hooks: matches!(n.kind, ZorkKind::Spreadsheet { .. }).then(|| {
                        (
                            n.sheet_input.key.len(),
                            n.sheet_input.goto.len(),
                            n.sheet_input.cursor_move.len(),
                            n.sheet_input.commit_edit.len(),
                        )
                    }),
                };
                match &n.kind {
                    ZorkKind::Window { title } | ZorkKind::Dialog { title } => s.title = Some(title.clone()),
                    ZorkKind::Label { text } | ZorkKind::TextView { text } => s.text = Some(text.clone()),
                    ZorkKind::Spreadsheet { border_title } => s.title = Some(border_title.clone()),
                    ZorkKind::Button { label } => s.label = Some(label.clone()),
                    ZorkKind::CheckButton { label, checked } => {
                        s.label = Some(label.clone());
                        s.checked = Some(*checked);
                    }
                    ZorkKind::RadioButton { label, checked, .. } => {
                        s.label = Some(label.clone());
                        s.checked = Some(*checked);
                    }
                    ZorkKind::DropDown { items, selected } => {
                        s.items = Some(items.clone());
                        s.selected = *selected;
                    }
                    _ => {}
                }
                s
            })
            .collect();
        let mut cells = HashMap::new();
        for n in &self.nodes {
            for (k, v) in &n.cells {
                cells.insert(*k, v.clone());
            }
        }
        let mut overlay_pass_through = HashMap::new();
        for n in &self.nodes {
            for (k, v) in &n.overlay_pass_through {
                overlay_pass_through.insert(*k, *v);
            }
        }
        Snapshot {
            nodes,
            menu_items: self.menu_items.clone(),
            current_id: self.current_id,
            running: self.running,
            focused: self.focused,
            overlay_pass_through,
            cells,
            night_mode: self.night_mode,
            timers: self
                .timers
                .iter()
                .map(|t| TimerInfo {
                    due_at: t.due_at,
                    interval_ms: t.interval_ms,
                    repeating: t.handler.is_some(),
                    window_id: t.window_id,
                })
                .collect(),
            now_ms: self.now_ms,
            app_actions: self.app_actions.clone(),
        }
    }
}

/// Clamp a scroll value into `[0, upper - page]`, treating a degenerate range
/// as `0` so a caller cannot scroll into a negative offset.
fn clamp_scroll(val: f64, upper: f64, page: f64) -> f64 {
    let max = (upper - page).max(0.0);
    if !val.is_finite() {
        return 0.0;
    }
    val.clamp(0.0, max)
}

// -- Thread-local singleton used by the adapter shim --

thread_local! {
    static ZORK_STATE: RefCell<ZorkState> = RefCell::new(ZorkState::new());
}

/// Run `f` against the singleton with the cell borrowed for its duration.
///
/// This is the *non*-re-entrant primitive. It is exactly right for the ~130
/// facade functions that only read or write model fields.
///
/// The model also fires *user* closures — a canvas draw, a click, a scroll
/// notification — and those closures may legitimately call back into the model.
/// Holding this borrow across a callback made every such call panic with
/// "RefCell already borrowed"; corro's draw callback calls `scroll_to` on every
/// frame and the first corro run on this backend died there. So the functions
/// that fire callbacks do **not** use this: they use the `with_callbacks`
/// pattern below, which releases the borrow before the closure runs.
pub(crate) fn with_state<F, R>(f: F) -> R
where
    F: FnOnce(&mut ZorkState) -> R,
{
    ZORK_STATE.with(|s| f(&mut s.borrow_mut()))
}

/// Borrow the model, lift a user closure out of it, run the closure with **no
/// borrow held**, then put the closure back.
///
/// This is the re-entrancy-safe way to fire a callback. The closure may call
/// [`with_state`] freely — the borrow is released — and any model change it
/// makes is seen both by itself and by the next caller, because it went
/// through the same cell.
///
/// `take` yields the closures, `put` reinstalls them. The split exists so the
/// two borrow scopes are visibly separate rather than one hidden borrow
/// spanning a user call.
pub(crate) fn with_callbacks<T>(
    take: fn(&mut ZorkState, usize) -> T,
    id: usize,
) -> T {
    with_state(|s| take(s, id))
}

/// Re-attach a lifted closure set after it has run.
pub(crate) fn put_callbacks<T>(
    put: fn(&mut ZorkState, usize, T),
    id: usize,
    taken: T,
) {
    with_state(|s| put(s, id, taken));
}

/// Take the singleton's current contents, leaving an empty model behind.
///
/// For a driver that wants to *adopt* what a host already built (corro builds
/// its whole widget tree through the adapter before handing off to the REPL)
/// rather than start from nothing.
pub(crate) fn take_state() -> ZorkState {
    ZORK_STATE.with(|s| std::mem::replace(&mut *s.borrow_mut(), ZorkState::new()))
}

/// Reset the thread-local singleton. Only for tests that need a clean slate
/// between cases.
#[cfg(test)]
pub(crate) fn reset_state() {
    ZORK_STATE.with(|s| *s.borrow_mut() = ZorkState::new());
}

/// Read the singleton without the two-phase dance, for assertions.
///
/// A plain read cannot re-enter (it takes no user-closure call path), so it
/// does not need the guard — but it does go through `with_state` for the
/// consistency of the re-entrancy check.
pub fn with_state_for_test<R>(f: impl FnOnce(&ZorkState) -> R) -> R {
    with_state(|s| f(s))
}

/// Reset the thread-local singleton, for an *external* test binary driving the
/// adapter (which has no access to `#[cfg(test)]` items). Each test starts from
/// a fresh model so they do not interfere through the shared singleton.
pub fn reset_for_test() {
    ZORK_STATE.with(|s| *s.borrow_mut() = ZorkState::new());
}

/// Mutate the singleton, for an external driver that needs an operation taking
/// `&mut ZorkState`. Uses the same two-phase `with_state` path, so a callback
/// fired inside `f` may re-enter no more than anywhere else.
pub fn with_state_mut_for_test<R>(f: impl FnOnce(&mut ZorkState) -> R) -> R {
    with_state(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    // These tests drive the model through the *facade* on purpose: the
    // re-entrancy bug lived in `with_state`, not in `ZorkState`, so a test
    // that called the methods directly would not have caught it.
    use super::super::facade as f;
    use super::super::facade::{create_scrolled_window, set_label_text, set_size_request};

    #[test]
    fn radio_group_is_mutually_exclusive() {
        let mut s = ZorkState::new();
        s.create_window();
        let a = s.create_radiobutton(Some(7), "a");
        let b = s.create_radiobutton(Some(7), "b");
        s.set_radiobutton_checked(a, true);
        assert!(s.get_radiobutton_checked(a));
        s.set_radiobutton_checked(b, true);
        assert!(s.get_radiobutton_checked(b));
        assert!(!s.get_radiobutton_checked(a), "sibling in the same group must clear");
    }

    #[test]
    fn append_child_keeps_existing_children() {
        let mut s = ZorkState::new();
        s.create_window();
        let bx = s.create_box(true, 0);
        let a = s.create_button("a");
        let b = s.create_button("b");
        s.append_child(bx, a);
        s.append_child(bx, b);
        assert_eq!(s.node(bx).unwrap().children, vec![a, b]);
    }

    #[test]
    fn set_child_reparents_without_duplicating() {
        let mut s = ZorkState::new();
        s.create_window();
        let bx1 = s.create_box(true, 0);
        let bx2 = s.create_box(true, 0);
        let a = s.create_button("a");
        s.append_child(bx1, a);
        s.set_child(bx2, a);
        assert_eq!(s.node(bx1).unwrap().children, Vec::<usize>::new());
        assert_eq!(s.node(bx2).unwrap().children, vec![a]);
        assert_eq!(s.node(a).unwrap().parent, Some(bx2));
        // No other node may still list it.
        let listed: Vec<usize> = s.nodes.iter().filter(|n| n.children.contains(&a)).map(|n| n.id).collect();
        assert_eq!(listed, vec![bx2]);
    }

    #[test]
    fn layout_box_positions_children() {
        let mut s = ZorkState::new();
        s.create_window();
        let bx = s.create_box(true, 4);
        let a = s.create_button("a");
        let b = s.create_button("b");
        s.append_child(bx, a);
        s.append_child(bx, b);
        s.set_size_request(a, 10, 6);
        s.set_size_request(b, 20, 6);
        s.layout_box(bx, 0, 0, 200, 100);
        assert_eq!(s.get_offset(a), Some((Some(0), Some(0))));
        assert_eq!(s.get_offset(b), Some((Some(14), Some(0))), "second child sits after the first plus spacing");
    }

    #[test]
    fn layout_box_skips_hidden_children() {
        let mut s = ZorkState::new();
        s.create_window();
        let bx = s.create_box(true, 0);
        let a = s.create_button("a");
        let b = s.create_button("b");
        s.append_child(bx, a);
        s.append_child(bx, b);
        s.set_size_request(a, 10, 6);
        s.set_size_request(b, 10, 6);
        s.set_visible(a, false);
        s.layout_box(bx, 0, 0, 100, 10);
        assert_eq!(s.get_offset(b), Some((Some(0), Some(0))), "hidden first child takes no room");
    }

    #[test]
    fn measure_box_uses_children() {
        let mut s = ZorkState::new();
        s.create_window();
        let bx = s.create_box(false, 2);
        let a = s.create_button("a");
        let b = s.create_button("b");
        s.append_child(bx, a);
        s.append_child(bx, b);
        s.set_size_request(a, 30, 10);
        s.set_size_request(b, 30, 20);
        // A vertical box packs along y: height = 10 + 20 + 2*spacing,
        // width = the widest child (30).
        assert_eq!(s.measure_box(bx), Some((30, 32)));
    }

    #[test]
    fn measure_box_horizontal_sums_widths() {
        let mut s = ZorkState::new();
        s.create_window();
        let bx = s.create_box(true, 4);
        let a = s.create_button("a");
        let b = s.create_button("b");
        s.append_child(bx, a);
        s.append_child(bx, b);
        s.set_size_request(a, 10, 6);
        s.set_size_request(b, 20, 9);
        // Horizontal packs along x: width = 10 + 20 + spacing, height = tallest.
        assert_eq!(s.measure_box(bx), Some((34, 9)));
    }

    #[test]
    fn grid_attach_grows_grid() {
        let mut s = ZorkState::new();
        s.create_window();
        let g = s.create_grid();
        let b = s.create_button("b");
        s.grid_attach(g, b, 2, 3, 1, 1);
        assert_eq!(s.get_offset(b), Some((Some(2), Some(3))));
        match s.node(g).unwrap().kind {
            ZorkKind::Grid { cols, rows } => assert_eq!((cols, rows), (3, 4)),
            _ => panic!("not a grid"),
        }
    }

    #[test]
    fn scroll_is_clamped_to_document() {
        let mut s = ZorkState::new();
        s.create_window();
        let sw = s.create_scrolled_window();
        s.set_scroll(sw, 0.0, 100.0, 20.0, 999.0, 100.0, 20.0);
        assert_eq!(s.get_scroll(sw), (0.0, 80.0));
    }

    #[test]
    fn focus_moves_and_is_queryable() {
        let mut s = ZorkState::new();
        s.create_window();
        let e = s.create_entry();
        assert!(!s.has_focus(e));
        s.set_focus(e);
        assert!(s.has_focus(e));
        assert_eq!(s.get_focus(), Some(e));
    }

    #[test]
    fn focus_refused_for_invisible_node() {
        let mut s = ZorkState::new();
        s.create_window();
        let e = s.create_entry();
        s.set_visible(e, false);
        s.set_focus(e);
        assert_eq!(s.get_focus(), None);
    }

    #[test]
    fn classes_add_remove_and_dedup() {
        let mut s = ZorkState::new();
        s.create_window();
        let b = s.create_button("x");
        s.add_class(b, "primary");
        s.add_class(b, "primary");
        assert_eq!(s.classes(b), vec!["primary".to_string()]);
        s.add_class(b, "flat");
        s.remove_class(b, "primary");
        assert_eq!(s.classes(b), vec!["flat".to_string()]);
        assert!(!s.has_class(b, "primary"));
    }

    #[test]
    fn key_hooks_stop_at_first_consumer() {
        use std::cell::Cell;
        use std::rc::Rc;
        let mut s = ZorkState::new();
        s.create_window();
        let c = s.create_canvas();
        let raw_hits = Rc::new(Cell::new(0));
        s.add_key_hook(c, Box::new(|_| true));
        {
            let r = raw_hits.clone();
            s.add_key_raw_hook(c, Box::new(move |_, _| { r.set(r.get() + 1); true }));
        }
        assert!(s.key(c, b'a' as u32));
        assert_eq!(raw_hits.get(), 0, "key_raw must be skipped when key consumed");
    }

    #[test]
    fn draw_canvas_records_ops() {
        let mut s = ZorkState::new();
        s.create_window();
        let c = s.create_canvas();
        s.set_size_request(c, 100, 50);
        s.set_draw_callback(c, Box::new(|ctx, w, h| {
            assert_eq!((w, h), (100, 50));
            ctx.fill_rect(0.0, 0.0, 10.0, 10.0, 1.0, 0.0, 0.0, 1.0);
        }));
        let ops = s.draw_canvas(c);
        assert_eq!(ops.len(), 1);
        // The callback must still be installed afterwards.
        assert!(s.has_draw_callback(c));
        assert_eq!(s.draw_canvas(c).len(), 1);
    }

    #[test]
    fn menu_select_dispatches_named_action() {
        use std::cell::Cell;
        use std::rc::Rc;
        let mut s = ZorkState::new();
        s.create_window();
        let act = s.create_simple_action("app.open");
        let menu = s.create_menu();
        s.menu_append(menu, "Open", "app.open");
        let fired = Rc::new(Cell::new(false));
        {
            let f = fired.clone();
            s.add_callback(act, Box::new(move || f.set(true)));
        }
        assert_eq!(s.menu_select(menu, 0), Some("Open".to_string()));
        assert!(fired.get());
    }

    #[test]
    fn menu_select_refuses_separator() {
        let mut s = ZorkState::new();
        s.create_window();
        let menu = s.create_menu();
        s.menu_append_separator(menu, "");
        s.menu_append(menu, "Open", "app.open");
        assert_eq!(s.menu_select(menu, 0), None);
        assert_eq!(s.menu_select(menu, 1), Some("Open".to_string()));
    }

    #[test]
    fn menu_check_item_toggles_before_dispatch() {
        use std::cell::Cell;
        use std::rc::Rc;
        let mut s = ZorkState::new();
        s.create_window();
        let act = s.create_simple_action("app.wrap");
        let menu = s.create_menu();
        s.menu_append_check(menu, "Wrap", "app.wrap", false);
        let seen = Rc::new(Cell::new(false));
        {
            let sv = seen.clone();
            let st = s.menu_items[&menu].clone();
            let _ = st;
            s.add_callback(act, Box::new(move || {
                // The action sees the already-flipped state.
                sv.set(true);
            }));
        }
        s.menu_select(menu, 0);
        assert!(seen.get());
        assert!(s.menu_items[&menu][0].checked);
        s.menu_select(menu, 0);
        assert!(!s.menu_items[&menu][0].checked);
    }

    #[test]
    fn radio_menu_items_are_exclusive() {
        let mut s = ZorkState::new();
        s.create_window();
        let menu = s.create_menu();
        let i0 = s.menu_append_radio(menu, "a", "", 1, false);
        let i1 = s.menu_append_radio(menu, "b", "", 1, false);
        s.set_menu_item_checked(menu, i1, true);
        assert!(s.menu_items[&menu][i1].checked);
        assert!(!s.menu_items[&menu][i0].checked);
    }

    #[test]
    fn overlay_layers_and_pass_through() {
        let mut s = ZorkState::new();
        s.create_window();
        let ov = s.create_overlay();
        let base = s.create_label("base");
        let layer = s.create_button("menu");
        s.set_child(ov, base);
        s.overlay_add(ov, layer);
        assert_eq!(s.node(ov).unwrap().children, vec![base]);
        assert_eq!(s.overlay_layers(ov), vec![layer]);
        s.overlay_set_pass_through(ov, layer, true);
        let snap = s.snapshot();
        assert_eq!(snap.overlay_pass_through.get(&layer), Some(&true));
    }

    #[test]
    fn show_all_makes_descendants_visible() {
        let mut s = ZorkState::new();
        s.create_window();
        let bx = s.create_box(true, 0);
        let l = s.create_label("x");
        s.append_child(bx, l);
        s.set_visible(l, false);
        s.show_all(bx);
        assert!(s.get_visible(bx));
        assert!(s.get_visible(l));
    }

    #[test]
    fn dialog_response_requires_a_registered_button() {
        use std::cell::Cell;
        use std::rc::Rc;
        let mut s = ZorkState::new();
        s.create_window();
        let d = s.create_dialog();
        let fired = Rc::new(Cell::new(0));
        {
            let f = fired.clone();
            s.add_response_callback(d, Box::new(move |_| f.set(f.get() + 1)));
        }
        s.dialog_respond(d, 1);
        assert_eq!(fired.get(), 0, "unregistered response must not fire");
        s.dialog_add_button(d, "OK", 1);
        s.dialog_respond(d, 1);
        assert_eq!(fired.get(), 1);
    }

    #[test]
    fn dialog_response_handler_receives_the_id() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut s = ZorkState::new();
        s.create_window();
        let d = s.create_dialog();
        s.dialog_add_button(d, "OK", 1);
        s.dialog_add_button(d, "Cancel", 2);
        let seen = Rc::new(RefCell::new(Vec::new()));
        {
            let sv = seen.clone();
            s.add_response_callback(d, Box::new(move |r| sv.borrow_mut().push(r)));
        }
        s.dialog_respond(d, 2);
        s.dialog_respond(d, 7);
        assert_eq!(*seen.borrow(), vec![2], "unregistered id must not reach the handler");
    }

    #[test]
    fn scroll_callback_drives_offset() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let mut s = ZorkState::new();
        s.create_window();
        let sw = s.create_scrolled_window();
        let seen = Rc::new(RefCell::new(Vec::new()));
        {
            let sv = seen.clone();
            s.add_scroll_callback(sw, Box::new(move |vert, pos| sv.borrow_mut().push((vert, pos))));
        }
        s.set_scroll_pos(sw, true, 42.0);
        s.set_scroll_pos(sw, false, 7.0);
        let mut cbs = ZorkState::take_scroll_hooks(&mut s, sw);
        for cb in cbs.iter_mut() {
            cb(true, 42.0);
            cb(false, 7.0);
        }
        ZorkState::put_scroll_hooks(&mut s, sw, cbs);
        assert_eq!(*seen.borrow(), vec![(true, 42.0), (false, 7.0)]);
        assert_eq!(s.get_scroll(sw), (7.0, 42.0));
    }

    #[test]
    fn entry_caret_is_tracked() {
        let mut s = ZorkState::new();
        s.create_window();
        let e = s.create_entry();
        s.set_entry_text(e, "hello");
        assert_eq!(s.get_entry_position(e), Some(5));
        s.set_entry_position(e, 99);
        assert_eq!(s.get_entry_position(e), Some(5), "clamped to buffer length");
        s.set_entry_position(e, 2);
        assert_eq!(s.get_entry_position(e), Some(2));
    }

    #[test]
    fn dropdown_selection_is_dropped_when_items_shrink() {
        let mut s = ZorkState::new();
        s.create_window();
        let d = s.create_dropdown(&["a", "b", "c"]);
        s.set_dropdown_selected(d, 2);
        assert_eq!(s.get_dropdown_selected(d), 2);
        s.set_dropdown_items(d, &["x"]);
        assert_eq!(s.get_dropdown_selected(d), -1);
    }

    #[test]
    fn snapshot_includes_properties() {
        let mut s = ZorkState::new();
        s.create_window();
        let b = s.create_button("x");
        s.set_size_request(b, 10, 20);
        s.set_visible(b, false);
        s.add_class(b, "flat");
        s.set_hexpand(b, true);
        let snap = s.snapshot();
        let n = snap.nodes.iter().find(|n| n.id == b).unwrap();
        assert_eq!(n.props.width, Some(10));
        assert!(!n.props.visible);
        assert_eq!(n.props.classes, vec!["flat".to_string()]);
        assert!(n.props.hexpand);
    }

    #[test]
    fn spreadsheet_cells_round_trip() {
        let mut s = ZorkState::new();
        s.create_window();
        let sh = s.create_spreadsheet();
        s.sheet_set_cell(sh, 1, 1, "42", 0);
        s.sheet_set_raw_cell(sh, 1, 2, "=1+1");
        s.sheet_set_border_title(sh, "Sheet1");
        assert_eq!(s.sheet_get_cell(sh, 1, 1), Some("42".to_string()));
        assert_eq!(s.sheet_get_cell(sh, 1, 2), Some("=1+1".to_string()));
        assert_eq!(s.sheet_border_title(sh), Some("Sheet1".to_string()));
        assert!(s.snapshot().cells.contains_key(&(1, 2)));
    }

    #[test]
    fn append_textview_is_incremental() {
        let mut s = ZorkState::new();
        s.create_window();
        let tv = s.create_textview();
        s.append_textview_text(tv, "a");
        s.append_textview_text(tv, "b");
        assert_eq!(s.get_textview_text(tv), Some("ab".to_string()));
    }

    #[test]
    fn fixed_width_sets_size_request() {
        let mut s = ZorkState::new();
        s.create_window();
        let l = s.create_label("x");
        s.set_fixed_width(l, Some(120));
        assert_eq!(s.get_size_request(l), Some((Some(120), None)));
    }

    // -- Night mode / the Grue --

    #[test]
    fn dungeon_starts_dark() {
        let s = ZorkState::new();
        assert!(s.is_night(), "a dungeon with no lantern is dark");
    }

    #[test]
    fn grue_warning_appears_once_per_dark_period() {
        let mut s = ZorkState::new();
        assert!(s.grue_warning().is_some(), "the first look warns");
        assert!(s.grue_warning().is_none(), "and does not nag afterwards");
        assert!(s.grue_warning().is_none());
    }

    #[test]
    fn lighting_up_silences_the_grue() {
        let mut s = ZorkState::new();
        assert!(s.grue_warning().is_some());
        s.set_night_mode(false);
        assert!(s.grue_warning().is_none(), "no grue in a lit room");
    }

    #[test]
    fn going_dark_again_rewarns() {
        let mut s = ZorkState::new();
        assert!(s.grue_warning().is_some());
        s.set_night_mode(false);
        assert!(s.grue_warning().is_none());
        s.set_night_mode(true);
        assert!(s.grue_warning().is_some(), "a fresh darkness is a new hazard");
    }

    #[test]
    fn night_mode_is_in_the_snapshot() {
        let mut s = ZorkState::new();
        assert!(s.snapshot().night_mode);
        s.set_night_mode(false);
        assert!(!s.snapshot().night_mode);
    }

    #[test]
    fn dark_room_text_names_the_grue_and_the_way_out() {
        let s = ZorkState::new();
        let text = s.dark_room_description().to_lowercase();
        assert!(text.contains("grue"), "warns about the grue: {text}");
        assert!(text.contains("light"), "says what to do about it: {text}");
    }

    // -- Re-entrancy --
    //
    // The facade's `with_state` used to hold a `RefMut` across the whole
    // operation, so any model call from inside a user callback panicked with
    // "RefCell already borrowed". That is not an exotic pattern: corro's draw
    // callback calls `scroll_to` on every frame to keep the scrollbar in step
    // with the viewport, and the first corro run on this backend died there.

    #[test]
    fn a_draw_callback_may_re_enter_the_model() {
        // Exactly corro's shape: paint, then sync the scrollbar to the viewport
        // while still inside the draw. Before the re-entrancy fix this died
        // with "RefCell already borrowed" in `scroll_to`.
        let c = f::create_canvas().expect("node");
        f::set_size_request(c, 100, 100);
        let cid = c;
        f::set_draw_callback(c, Box::new(move |_ctx, _w, _h| {
            f::set_size_request(cid, 100, 100);
            let sc = f::create_scrolled_window().expect("node");
            f::scroll_to(sc, 0.0, 100.0, 20.0, 0.0, 100.0, 20.0);
        }));
        let ops = f::draw_canvas(c);
        assert!(ops.is_empty(), "the callback ran without panicking");
    }

    #[test]
    fn a_click_callback_may_re_enter_the_model() {
        use std::cell::Cell;
        use std::rc::Rc;
        let b = f::create_button("go").expect("node");
        let l = f::create_label("0").expect("node");
        let hits = Rc::new(Cell::new(0));
        let h = hits.clone();
        f::add_callback(b, Box::new(move || {
            f::set_label_text(l, "1");   // a click handler that updates a label
            h.set(1);
        }));
        f::fire(b);
        assert_eq!(hits.get(), 1);
        assert_eq!(f::get_label_text(l), Some("1".to_string()));
    }

    #[test]
    fn a_nested_call_keeps_both_writes() {
        // The subtle failure the fix has to avoid: a nested call must not
        // operate on a detached copy whose changes are thrown away. Here the
        // click handler *creates* a widget, and the outer state must see it.
        let b = f::create_button("go").expect("node");
        f::add_callback(b, Box::new(|| {
            f::create_label("made inside a callback").expect("node");
        }));
        let before = super::with_state_for_test(|s| s.nodes.len());
        f::fire(b);
        let after = super::with_state_for_test(|s| s.nodes.len());
        assert_eq!(after, before + 1, "the nested creation reached the real model");
    }

    #[test]
    fn grue_warning_mentions_being_eaten() {
        let mut s = ZorkState::new();
        let w = s.grue_warning().unwrap().to_lowercase();
        assert!(w.contains("grue"), "{w}");
        assert!(w.contains("eat"), "the threat is spelled out: {w}");
    }

    #[test]
    fn toggle_keeps_radio_group_exclusive() {
        let mut s = ZorkState::new();
        s.create_window();
        let a = s.create_radiobutton(Some(3), "a");
        let b = s.create_radiobutton(Some(3), "b");
        s.toggle(a);
        assert!(s.get_radiobutton_checked(a));
        s.toggle(b);
        assert!(s.get_radiobutton_checked(b));
        assert!(!s.get_radiobutton_checked(a));
    }
}

#[cfg(test)]
mod menubar_dedup_tests {
    use super::*;

    /// `create_menubar` used to mint a *new* `Menu` node for every submenu
    /// instead of re-using the ones `create_menu` made, so the tree held two
    /// nodes with the File items and the REPL's direction list showed the File
    /// menu twice. Invisible until `menu_label` gave the nodes names.
    #[test]
    fn submenu_nodes_are_not_duplicated() {
        let mut s = ZorkState::new();
        s.create_window();
        let bar_model = s.create_menu();
        // File > (New, Export > (TSV)), Edit > (Cut)
        let file = s.create_menu();
        s.menu_append(file, "New", "app.new_file");
        let export = s.create_menu();
        s.menu_append(export, "TSV", "app.export_tsv");
        s.menu_append_submenu(bar_model, "_File", file);
        s.menu_append_submenu(file, "Expor_t", export);
        let edit = s.create_menu();
        s.menu_append(edit, "_Cut", "app.cut");
        s.menu_append_submenu(bar_model, "_Edit", edit);

        let bar = s.create_menubar(bar_model, std::ptr::null_mut());

        // One node per submenu, plus the bar itself.
        let menu_nodes = s
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, ZorkKind::Menu))
            .count();
        assert_eq!(
            menu_nodes,
            3,
            "expected File, Export, Edit and no duplicates, found {menu_nodes} Menu nodes"
        );

        // Every submenu hangs off the right parent: Export under File, not
        // flattened onto the bar.
        let by_label = |label: &str| {
            s.nodes
                .iter()
                .find(|n| n.menu_label.as_deref() == Some(label))
                .map(|n| n.id)
        };
        let file_id = by_label("_File").expect("File menu");
        let export_id = by_label("Expor_t").expect("Export menu");
        let edit_id = by_label("_Edit").expect("Edit menu");
        assert_eq!(s.node(export_id).unwrap().parent, Some(file_id));
        assert_eq!(s.node(file_id).unwrap().parent, Some(bar));
        assert_eq!(s.node(edit_id).unwrap().parent, Some(bar));

        // And the window is not left listing the submenus it gave them to.
        let win = s.find_window_id().unwrap();
        for id in [file_id, export_id, edit_id] {
            assert!(
                !s.node(win).unwrap().children.contains(&id),
                "node {id} is still a child of the window after the bar adopted it"
            );
        }
    }

    /// The item *count* corro reports is the sum over distinct nodes, and each
    /// node is counted once. It used to read 132 because the six top-level
    /// submenus existed as two nodes apiece, so their items were summed
    /// twice; 79 is the number of distinct items.
    ///
    /// The invariant is per-node, not a magic total: every node's list is
    /// stored once, and no two nodes share one.
    #[test]
    fn each_menu_node_stores_its_items_once() {
        let mut s = ZorkState::new();
        s.create_window();
        let bar_model = s.create_menu();
        for (label, action) in [("_File", "a"), ("_Edit", "b")] {
            let sub = s.create_menu();
            s.menu_append(sub, "One", action);
            s.menu_append(sub, "Two", action);
            s.menu_append_submenu(bar_model, label, sub);
        }
        let bar = s.create_menubar(bar_model, std::ptr::null_mut());

        // Three distinct menus: the bar plus two submenus.
        let mut nodes: Vec<usize> = s.nodes.iter().filter(|n| matches!(n.kind, ZorkKind::Menu | ZorkKind::MenuBar)).map(|n| n.id).collect();
        nodes.sort_unstable();
        nodes.dedup();
        assert_eq!(nodes.len(), 3, "bar plus two submenus, no duplicates");

        // The bar keeps its own two items; each submenu keeps its own two.
        // Nothing is stored twice on one node.
        for id in nodes {
            let items = s.menu_items.get(&id).expect("every menu node has an item list");
            assert!(
                items.len() == 2,
                "node {id} stores {} items, expected 2", items.len()
            );
        }
        assert!(
            matches!(s.node(bar).map(|n| &n.kind), Some(ZorkKind::MenuBar)),
            "the reused node became the bar"
        );
    }
}
