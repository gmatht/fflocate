//! A real **Grid view widget**: the data model, the renderer and the pointer
//! contract assembled into one thing a host can use without touching a
//! `Canvas` itself.
//!
//! Before this module rswidgets had the pieces but not the assembly:
//!
//! * [`crate::core::Grid`] is the *model* (cells, cursor, viewport, column
//!   layout) and deliberately has no widget — `core.rs` says so, so that
//!   `crate::Grid` keeps meaning the active backend's layout container;
//! * the backend `Grid` widget is a *packing* container (`attach` only), not a
//!   data view;
//! * [`crate::spreadsheet::paint`] already renders a model through any
//!   [`DrawContext`], and [`crate::backends::pancurses_draw`] already adapts
//!   that to a terminal cell grid.
//!
//! So every host ended up hand-rolling the same glue — corro's `gui_backend`
//! wires `canvas.on_click(handle_click)` and re-derives the viewport per
//! frame, and `pnc_backend` re-derives it in three separate callbacks. This
//! module is that glue, once.
//!
//! # Shape
//!
//! [`GridView`] owns a [`SpreadsheetModel`] and renders it into a
//! [`DrawContext`]; it is deliberately *not* a backend object, so the same
//! value drives a GTK canvas, a terminal cell grid, or a headless recorder.
//! A host does:
//!
//! ```ignore
//! let mut grid = GridView::new(rows, cols);
//! grid.set_cell(2, 1, "hello");
//! grid.on_cell_click(|row, col| { /* selection logic */ });
//!
//! canvas.set_draw_callback(Box::new(move |dc, w, h| grid.paint(dc, w, h)));
//! canvas.on_click(Box::new(move |x, y| grid.click_at(x, y)));
//! ```
//!
//! The pointer side is the important half: `click_at` takes the *pixel*
//! coordinates every backend already produces (the terminal backend converts
//! its cell hit for us) and maps them through the same layout the renderer
//! used, so a click can never disagree with what is on screen.
//!
//! # Zoom
//!
//! [`GridView::zoom`] is a view scale the renderer *and* the hit-test both
//! honour (via [`SpreadsheetModel::zoom`]), so a pinch can never make what is
//! drawn disagree with what a finger hits. [`zoom_by`](GridView::zoom_by) is
//! the pinch entry point (an incremental factor, as a gesture recogniser
//! reports it), [`set_zoom`](GridView::set_zoom) the absolute one, and
//! [`reset_zoom`](GridView::reset_zoom) the double-tap. The scale is clamped
//! to `MIN_ZOOM..=MAX_ZOOM`.
//!
//! # Drag means different things on different pointers
//!
//! `click_at` is a *tap*. For a drag, the widget exposes a small state machine
//! driven by [`pointer_down`](GridView::pointer_down) /
//! [`pointer_move`](GridView::pointer_move) /
//! [`pointer_up`](GridView::pointer_up), with the platform passed in as a
//! [`PointerKind`]:
//!
//! * [`PointerKind::Mouse`] (a desktop) — a drag **extends the selection**,
//!   exactly like a spreadsheet on a monitor;
//! * [`PointerKind::Touch`] (a phone) — a drag **scrolls** the grid, because a
//!   pan is the only natural way to move a grid on a small screen. Selection
//!   needs a deliberate signal, so a **long press** (host timer →
//!   [`pointer_long_press`](GridView::pointer_long_press)) arms it and the
//!   drag that follows extends the selection instead.
//!
//! Each call returns a [`DragOutcome`], so a host learns whether to repaint a
//! selection, apply a pan (the pixel deltas are in the outcome), or do
//! nothing. A drag never re-fires as a tap on release; a press/release that
//! never moved is reported as [`DragOutcome::Tap`] and behaves exactly like
//! `click_at`.
//!
//! The Android and iOS hosts wire their platform gestures into exactly this
//! API; see `ANDROID_GUIDELINES.md` §3.1 and `IOS_GUIDELINES.md` §3.1.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::core::{DrawContext, Grid};
use crate::spreadsheet::{self, SpreadsheetModel};

/// What a pointer hit landed on. Lets a host distinguish "the user clicked a
/// data cell" from "the user clicked a header", which need different actions
/// (move the cursor vs. select the whole column).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridHit {
    /// A body cell: `(row, col)` in the model's own coordinates.
    Cell { row: u32, col: u32 },
    /// The column-title band above the grid.
    ColumnHeader { col: u32 },
    /// The row-label gutter down the left edge.
    RowHeader { row: u32 },
    /// Past the last column or row.
    Outside,
}

/// Which pointer produced a gesture. The host knows its platform; the widget
/// only needs to know whether a drag means "select" or "scroll".
///
/// * **Mouse** (desktop) — press-and-drag extends the selection, exactly like a
///   spreadsheet on a desktop. Nothing has to arm it first: a mouse button is
///   unambiguous, and there is no competing pan gesture.
/// * **Touch** (a phone or tablet) — a drag pans the content, because that is
///   the only way to move a grid on a small screen. Selection needs a
///   deliberate signal, which is a *long press* (the same vocabulary every
///   phone app uses): hold the finger still on a cell, then drag to extend the
///   selection.
///
/// This is the "platform-aware drag" rule: the same `pointer_move` means
/// "extend the selection" on a desktop and "scroll" on a phone, and only the
/// `PointerKind` the host passes decides which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    /// Mouse/trackpad: drag selects.
    Mouse,
    /// Touch/finger: drag scrolls unless a long press armed selection.
    Touch,
}

/// What a pointer event did, so a host can decide what to repaint and, for
/// [`DragOutcome::Scroll`], how far.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DragOutcome {
    /// Nothing happened (a move with no pointer down, a stray release).
    Ignored,
    /// The selection was extended/moved — repaint the grid.
    Select,
    /// The content should pan by `(dx, dy)` pixels. Touch drags only.
    Scroll { dx: f64, dy: f64 },
    /// A press/release with no drag: the same as [`GridView::click_at`].
    Tap(GridHit),
    /// A long press landed on a cell and armed selection; the host can show a
    /// hint or haptic feedback.
    LongPress(GridHit),
}

/// One grid's view state: the model plus the pointer/interaction callbacks.
///
/// `Clone` shares the model and the callback slots (like the backend widgets,
/// each of which is a cloneable handle around backend state), so a draw
/// callback and a click callback can each hold a `GridView` and see the same
/// data.
#[derive(Clone)]
pub struct GridView {
    model: Rc<RefCell<SpreadsheetModel>>,
    handlers: Rc<RefCell<GridHandlers>>,
    gesture: Rc<RefCell<GestureState>>,
}

/// The live pointer gesture, shared like the model and the callbacks so a
/// host's draw callback and its pointer callbacks see the same drag.
#[derive(Clone, Copy, Debug)]
struct GestureState {
    /// The pointer is down (a press arrived and no release has followed).
    active: bool,
    kind: Option<PointerKind>,
    /// Press origin in pixels; used for the slop test and to tell a tap from a
    /// drag.
    down_x: f64,
    down_y: f64,
    /// Last point seen by `pointer_move`, for incremental scroll deltas.
    last_x: f64,
    last_y: f64,
    /// Set once the pointer has travelled past the drag slop. Until then the
    /// gesture may still turn out to be a tap.
    dragging: bool,
    /// Touch only: a long press (or a move after one) has armed selection, so
    /// subsequent moves extend it instead of scrolling.
    select_armed: bool,
    /// Set by `pointer_down` on a touch and cleared by any movement: a long
    /// press is only a long press if the finger stayed still.
    long_press_pending: bool,
    /// Whether a long press arms selection at all (see
    /// [`GridView::set_long_press_select`]). Defaults to `true`.
    long_press_select: bool,
    /// The selection anchor captured at the press, in model coordinates.
    /// Held here (not on the model) until a drag actually begins, so a plain
    /// click stays a single-cell cursor move instead of leaving a one-cell
    /// selection band behind.
    press_anchor: Option<(u32, u32)>,
}

impl Default for GestureState {
    fn default() -> Self {
        GestureState {
            active: false,
            kind: None,
            down_x: 0.0,
            down_y: 0.0,
            last_x: 0.0,
            last_y: 0.0,
            dragging: false,
            select_armed: false,
            long_press_pending: false,
            long_press_select: true,
            press_anchor: None,
        }
    }
}

#[derive(Default)]
struct GridHandlers {
    cell_click: Option<Box<dyn FnMut(u32, u32)>>,
    header_click: Option<Box<dyn FnMut(GridHit)>>,
    cursor_move: Option<Box<dyn FnMut(u32, u32)>>,
    /// Set when a click should move the cursor (the default when the host has
    /// not taken over). Kept separate from `cell_click` so a host can observe
    /// clicks *and* keep the built-in cursor behaviour.
    cursor_click: bool,
    /// Called with the new scale after every zoom change (see
    /// [`GridView::on_zoom`]).
    zoom: Option<Box<dyn FnMut(f64)>>,
}

impl Default for GridView {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

impl GridView {
    /// A grid with `rows` × `cols` body cells and the default chrome
    /// (one header row band, one margin column), matching the spreadsheet
    /// renderer's defaults.
    pub fn new(rows: u32, cols: u32) -> Self {
        let mut model = SpreadsheetModel::new(rows, cols);
        // `SpreadsheetModel::new` defaults to spreadsheet chrome (a header band
        // and a margin column). A plain data grid has neither, so body cell
        // (0,0) really is model (0,0) and a host never has to add offsets —
        // which is the whole point of this widget.
        model.set_row_counts(0, rows);
        model.set_grid_config(0, cols);
        // Same for the default column titles: numbered columns, not offsets.
        model.set_column_layout((0..cols).map(|c| (c, 12u32, format!("{}", c + 1))).collect());
        GridView {
            model: Rc::new(RefCell::new(model)),
            handlers: Rc::new(RefCell::new(GridHandlers { cursor_click: true, ..Default::default() })),
            gesture: Rc::new(RefCell::new(GestureState::default())),
        }
    }

    /// A grid over an existing shared model (e.g. one a backend `Spreadsheet`
    /// already owns), so the widget and the backend stay in sync.
    pub fn from_model(model: Rc<RefCell<SpreadsheetModel>>) -> Self {
        GridView {
            model,
            handlers: Rc::new(RefCell::new(GridHandlers { cursor_click: true, ..Default::default() })),
            gesture: Rc::new(RefCell::new(GestureState::default())),
        }
    }

    /// Wrap a [`Grid`] model — the backend-agnostic *data* type — by copying
    /// its cells/cursor/layout into a [`SpreadsheetModel`], which is what the
    /// renderer consumes. Use this when the data already lives in a
    /// `core::Grid` (e.g. a workbook sheet).
    pub fn from_grid(g: &Grid) -> Self {
        let view = GridView::new(g.main_row_count.max(g.total_rows), g.total_cols);
        {
            let mut m = view.model.borrow_mut();
            m.margin_cols = g.margin_cols;
            m.main_cols = g.main_cols;
            m.header_row_count = g.header_row_count;
            m.main_row_count = g.main_row_count;
            // `GridView` has no per-column default width of its own: the
            // renderer's default (12 chars) applies unless the caller supplies
            // `set_column_layout`, which is what the `core::Grid` layout maps
            // onto below.
            m.column_layout = g.column_layout.clone();
            m.row_labels = g.row_labels.clone();
            m.cursor_row = g.cursor_row;
            m.cursor_col = g.cursor_col;
            m.anchor = g.anchor;
            m.editing = g.editing;
            m.edit_buf = g.edit_buf.clone();
            m.edit_pos = g.edit_pos;
            for (k, v) in g.cells.borrow().iter() {
                m.cells.insert(*k, v.clone());
            }
            for (k, v) in g.raw_cells.borrow().iter() {
                m.raw_cells.insert(*k, v.clone());
            }
            for (k, v) in g.cell_styles.borrow().iter() {
                m.cell_styles.insert(*k, *v);
            }
        }
        view
    }

    /// The shared model, for a host that needs to read or mutate it directly.
    pub fn model(&self) -> Rc<RefCell<SpreadsheetModel>> {
        self.model.clone()
    }

    // ── Data ─────────────────────────────────────────────────────────────

    pub fn rows(&self) -> u32 {
        self.model.borrow().main_row_count
    }

    pub fn cols(&self) -> u32 {
        self.model.borrow().main_cols
    }

    /// Set a body cell's display text. `row`/`col` are **body** coordinates
    /// (0-based, header/margin excluded), which is what a host naturally has.
    pub fn set_cell(&self, row: u32, col: u32, text: &str) {
        let (r, c) = self.body_to_model(row, col);
        self.model.borrow_mut().set_cell(r, c, text);
    }

    pub fn cell(&self, row: u32, col: u32) -> String {
        let (r, c) = self.body_to_model(row, col);
        self.model.borrow().cells.get(&(r, c)).cloned().unwrap_or_default()
    }

    /// Style a body cell with a [`crate::spreadsheet::style`] bit.
    pub fn set_cell_style(&self, row: u32, col: u32, style: u8) {
        let (r, c) = self.body_to_model(row, col);
        self.model.borrow_mut().set_cell_style(r, c, style);
    }

    /// Set a body cell's *raw* (unformatted) value, which is what the formula
    /// bar and any host that needs the underlying datum read.
    ///
    /// Separate from [`set_cell`](Self::set_cell) because the renderer draws
    /// the formatted display text while a host edits the raw value; conflating
    /// them is how a `42` gets re-entered as `"42"`.
    pub fn set_raw_cell(&self, row: u32, col: u32, text: &str) {
        let (r, c) = self.body_to_model(row, col);
        self.model.borrow_mut().set_raw_cell(r, c, text);
    }

    /// A body cell's raw value.
    pub fn raw_cell(&self, row: u32, col: u32) -> String {
        let (r, c) = self.body_to_model(row, col);
        self.model
            .borrow()
            .raw_cells
            .get(&(r, c))
            .cloned()
            .unwrap_or_default()
    }

    /// Move the cursor to a body cell.
    pub fn set_cursor(&self, row: u32, col: u32) {
        let (r, c) = self.body_to_model(row, col);
        self.model.borrow_mut().set_cursor(r, c);
    }

    pub fn cursor(&self) -> (u32, u32) {
        let m = self.model.borrow();
        (
            m.cursor_row.saturating_sub(m.header_row_count),
            m.cursor_col.saturating_sub(m.margin_cols),
        )
    }

    /// Configure the margin/body split (see `SpreadsheetModel::set_grid_config`).
    pub fn set_grid_config(&self, margin_cols: u32, main_cols: u32) {
        self.model.borrow_mut().set_grid_config(margin_cols, main_cols);
    }

    /// Configure the header/body row split.
    pub fn set_row_counts(&self, header_rows: u32, main_rows: u32) {
        self.model.borrow_mut().set_row_counts(header_rows, main_rows);
    }

    /// Column widths/titles: `(global_col, width_in_chars, title)`.
    pub fn set_column_layout(&self, layout: Vec<(u32, u32, String)>) {
        self.model.borrow_mut().set_column_layout(layout);
    }

    /// Row labels: `(row, label)`.
    pub fn set_row_labels(&self, labels: Vec<(u32, String)>) {
        self.model.borrow_mut().set_row_labels(labels);
    }

    /// The chrome around the grid (all optional; empty means "not drawn").
    pub fn set_chrome(&self, menu: &str, title: &str, status: &str) {
        let mut m = self.model.borrow_mut();
        m.set_menu_text(menu);
        m.set_border_title(title);
        m.set_status_text(status);
    }

    /// Tabs under the grid (sheet tabs).
    pub fn set_tabs(&self, titles: &[String], active: usize) {
        self.model.borrow_mut().set_tab_data(titles, active);
    }

    // ── Zoom (pinch) ─────────────────────────────────────────────────────

    /// The current view scale (1.0 = the base metrics).
    pub fn zoom(&self) -> f64 {
        self.model.borrow().zoom()
    }

    /// Set the view scale directly, clamped to the model's
    /// [`MIN_ZOOM`](SpreadsheetModel::MIN_ZOOM)..=[`MAX_ZOOM`](SpreadsheetModel::MAX_ZOOM)
    /// range. Returns the scale actually applied. Repainting is the host's job
    /// (it owns the canvas): the host calls `queue_redraw` after a gesture.
    pub fn set_zoom(&self, zoom: f64) -> f64 {
        let applied = self.model.borrow_mut().set_zoom(zoom);
        // A zoom change moves every hit target (headers, gutters, cells), and
        // the cursor cell is where the host's edit box is anchored, so the
        // change is announced on the same channel as a cursor move. Hosts that
        // do not care simply do not register the callback.
        self.fire_zoom(applied);
        applied
    }

    /// Multiply the current scale by `factor`, as a pinch does.
    ///
    /// `anchor_x`/`anchor_y` are the pinch's focus point in the same pixel
    /// space as [`paint`](Self::paint). The scale is applied around that point
    /// (see [`SpreadsheetModel::zoom_by`]), so the content under the fingers
    /// stays put. A `factor` that would leave the clamped range is a no-op
    /// beyond the clamp — the callback still fires so a host can show the
    /// limit.
    ///
    /// Returns the scale actually applied.
    pub fn zoom_by(&self, factor: f64, anchor_x: f64, anchor_y: f64) -> f64 {
        let applied = self.model.borrow_mut().zoom_by(factor, anchor_x, anchor_y);
        self.fire_zoom(applied);
        applied
    }

    /// Reset the view scale to 1.0 (a double-tap or a menu "reset zoom").
    /// Returns the scale actually applied (always 1.0).
    pub fn reset_zoom(&self) -> f64 {
        self.set_zoom(1.0)
    }

    /// Convenience clamp used by the gesture layer: `set_zoom` clamps too, but
    /// a host that keeps its own gesture multiplier wants the same bounds
    /// without a round trip through the model.
    pub fn zoom_limits(&self) -> (f64, f64) {
        (SpreadsheetModel::MIN_ZOOM, SpreadsheetModel::MAX_ZOOM)
    }

    /// Called with the new scale after every zoom change (a pinch, a direct
    /// `set_zoom`, a reset). Use it to mirror the scale into a status line or
    /// to know when to repaint.
    pub fn on_zoom(&self, cb: impl FnMut(f64) + 'static) {
        self.handlers.borrow_mut().zoom = Some(Box::new(cb));
    }

    // ── Callbacks ────────────────────────────────────────────────────────

    /// Called with **body** `(row, col)` when a body cell is clicked.
    pub fn on_cell_click(&self, cb: impl FnMut(u32, u32) + 'static) {
        self.handlers.borrow_mut().cell_click = Some(Box::new(cb));
    }

    /// Called for *every* hit (cells, headers, outside), so a host can drive
    /// column-select / row-select behaviour.
    pub fn on_header_click(&self, cb: impl FnMut(GridHit) + 'static) {
        self.handlers.borrow_mut().header_click = Some(Box::new(cb));
    }

    /// Called after the built-in cursor move from a cell click, so a host can
    /// mirror the cursor (e.g. into a workbook or a status line).
    pub fn on_cursor_move(&self, cb: impl FnMut(u32, u32) + 'static) {
        self.handlers.borrow_mut().cursor_move = Some(Box::new(cb));
    }

    /// Turn the built-in "click a cell moves the cursor" behaviour off, for a
    /// host that wants clicks to mean something else entirely.
    pub fn set_cursor_click(&self, enabled: bool) {
        self.handlers.borrow_mut().cursor_click = enabled;
    }

    // ── Rendering ────────────────────────────────────────────────────────

    /// Paint into any backend's drawing surface. This is the same renderer the
    /// GUI backends and the terminal cell grid use, so what a host sees and
    /// what `hit_test` reports can never drift apart.
    pub fn paint(&self, dc: &mut dyn DrawContext, w: i32, h: i32) {
        let m = self.model.borrow();
        spreadsheet::paint(&m, dc, w, h);
    }

    // ── Pointer ──────────────────────────────────────────────────────────

    /// Map a pixel coordinate to what is under it, using the renderer's own
    /// layout (the header band, the row-label gutter, per-column widths).
    pub fn hit_test(&self, x: f64, y: f64) -> GridHit {
        let m = self.model.borrow();
        // Zoom-aware metrics: the same accessors `spreadsheet::paint` uses, so
        // a pinch that redraws the grid also moves every hit target with it.
        let header_h = m.header_row_count as f64 * m.header_h();
        let row_label_w = m.row_label_w();
        let total_rows = m.header_row_count + m.main_row_count;
        let total_cols = m.margin_cols + m.main_cols;

        // Columns, mirroring `spreadsheet::paint` exactly:
        //   * margin columns sit inside the label gutter at
        //     `c * (ROW_LABEL_W / margin_cols)`;
        //   * a grid with no margin column has no gutter, so its first main
        //     column starts at x = 0 (the renderer's `col_x` still anchors the
        //     *layout* at ROW_LABEL_W, but there is nothing to its left);
        //   * main columns are `width_chars * CHAR_W` wide, in layout order.
        let col_at = |x: f64| -> Option<(u32, bool)> {
            if x < 0.0 {
                return None;
            }
            if m.margin_cols > 0 && x < row_label_w {
                let band = row_label_w / m.margin_cols as f64;
                let c = ((x / band) as u32).min(m.margin_cols - 1);
                return Some((c, true));
            }
            // Main columns. With no margin column the grid uses the full
            // width starting at 0, matching how `GridView::new` lays it out.
            let mut cx = if m.margin_cols > 0 { row_label_w } else { 0.0 };
            for c in m.margin_cols..total_cols {
                let cw = model_col_width(&m, c);
                if x < cx + cw {
                    return Some((c, false));
                }
                cx += cw;
            }
            None
        };

        let Some((col, in_margin)) = col_at(x) else {
            return GridHit::Outside;
        };
        // Header band: a click there picks the column.
        if y < header_h {
            return if in_margin {
                GridHit::Outside
            } else {
                GridHit::ColumnHeader { col }
            };
        }

        let row = ((y - header_h) / m.row_h()).floor();
        if row < 0.0 || row as u32 >= total_rows {
            return GridHit::Outside;
        }
        let row = row as u32;

        if in_margin {
            GridHit::RowHeader { row: row.saturating_sub(m.header_row_count) }
        } else if row < m.header_row_count {
            // The header band's own rows are chrome; treat as the column.
            GridHit::ColumnHeader { col }
        } else {
            GridHit::Cell { row: row - m.header_row_count, col }
        }
    }

    /// Handle a click at pixel coordinates: run the host callbacks and, unless
    /// disabled, move the cursor onto the clicked cell.
    ///
    /// Returns the hit so a caller can chain behaviour. Wire this straight
    /// into a canvas (`canvas.on_click(|x, y| { grid.click_at(x, y); })`) — it
    /// is the same call on every backend.
    pub fn click_at(&self, x: f64, y: f64) -> GridHit {
        let hit = self.hit_test(x, y);

        // Host callbacks first: they see every hit, including the chrome.
        // Each is taken out, called, and put back, so a callback that reaches
        // back into the grid (to read cells or move the cursor) never trips a
        // `RefCell` double borrow.
        let mut cb = self.handlers.borrow_mut().header_click.take();
        if let Some(cb) = cb.as_mut() {
            cb(hit);
        }
        self.handlers.borrow_mut().header_click = cb;

        if let GridHit::Cell { row, col } = hit {
            let mut cb = self.handlers.borrow_mut().cell_click.take();
            if let Some(cb) = cb.as_mut() {
                cb(row, col);
            }
            self.handlers.borrow_mut().cell_click = cb;

            if self.handlers.borrow().cursor_click {
                self.set_cursor(row, col);
                let mut cb = self.handlers.borrow_mut().cursor_move.take();
                if let Some(cb) = cb.as_mut() {
                    cb(row, col);
                }
                self.handlers.borrow_mut().cursor_move = cb;
            }
        }
        hit
    }

    // ── Platform-aware drag gestures ─────────────────────────────────────

    /// A pointer press. `kind` is the host's knowledge of its platform:
    /// [`PointerKind::Mouse`] on a desktop (drag selects) or
    /// [`PointerKind::Touch`] on a phone (drag scrolls until a long press).
    ///
    /// Returns [`DragOutcome::Select`] when the press immediately begins a
    /// selection (the mouse case), or [`DragOutcome::Ignored`] for a touch
    /// press that may still become a scroll or a long press.
    pub fn pointer_down(&self, x: f64, y: f64, kind: PointerKind) -> DragOutcome {
        {
            let mut g = self.gesture.borrow_mut();
            g.active = true;
            g.kind = Some(kind);
            g.down_x = x;
            g.down_y = y;
            g.last_x = x;
            g.last_y = y;
            g.dragging = false;
            g.select_armed = false;
            g.long_press_pending = kind == PointerKind::Touch;
        }
        // Capture the press anchor now (in model coordinates) whether or not
        // this press will become a drag: it is the origin of any selection the
        // gesture produces, and reading it later from the (possibly moved)
        // cursor would anchor the band in the wrong place.
        let press_anchor = match self.hit_test(x, y) {
            GridHit::Cell { row, col } => {
                let m = self.model.borrow();
                Some((row + m.header_row_count, col + m.margin_cols))
            }
            _ => None,
        };
        self.gesture.borrow_mut().press_anchor = press_anchor;
        match kind {
            // A mouse press-and-drag is a selection, so arm it immediately.
            PointerKind::Mouse => {
                self.gesture.borrow_mut().select_armed = true;
                // Not `commit_anchor`: a press may still turn out to be a
                // click, and only a drag should leave a selection behind.
                self.begin_selection(x, y, false);
                DragOutcome::Select
            }
            // A finger might scroll, so nothing is armed yet: the host calls
            // `pointer_long_press` once its timer fires (or lets
            // `pointer_move` scroll).
            PointerKind::Touch => DragOutcome::Ignored,
        }
    }

    /// The host's long-press timer fired for the touch that started at
    /// `pointer_down`. If the finger has not moved since, selection is armed
    /// and a following drag extends it instead of scrolling.
    ///
    /// Returns [`DragOutcome::LongPress`] with the hit under the finger when
    /// the press was still valid, else [`DragOutcome::Ignored`] (the finger
    /// moved, or there was no touch press).
    ///
    /// The long press itself does *not* move the cursor: arming selection on
    /// the cell the user is already on is the least surprising behaviour, and
    /// it lets a drag that follows extend from there. A host that wants a
    /// press to also select the cell can call [`click_at`](Self::click_at) on
    /// the outcome's hit.
    pub fn pointer_long_press(&self, x: f64, y: f64) -> DragOutcome {
        let mut g = self.gesture.borrow_mut();
        if !g.active
            || g.kind != Some(PointerKind::Touch)
            || !g.long_press_pending
            || g.dragging
            || !g.long_press_select
        {
            return DragOutcome::Ignored;
        }
        g.long_press_pending = false;
        g.select_armed = true;
        drop(g);
        DragOutcome::LongPress(self.hit_test(x, y))
    }

    /// A pointer move. What this means depends on the press that started the
    /// gesture:
    ///
    /// * a mouse drag (or a touch drag after a long press) extends the
    ///   selection to the cell under `(x, y)` → [`DragOutcome::Select`];
    /// * a plain touch drag pans the content by the movement since the last
    ///   event → [`DragOutcome::Scroll`];
    /// * a move with no press, or one still inside the drag slop, does nothing
    ///   → [`DragOutcome::Ignored`].
    ///
    /// The slop is measured from the press origin, so a shaky tap still counts
    /// as a tap; once it is exceeded the whole travel counts (a finger that
    /// spent 8px getting past slop has really moved 8px).
    pub fn pointer_move(&self, x: f64, y: f64) -> DragOutcome {
        let kind = self.gesture.borrow().kind;
        let Some(kind) = kind else {
            return DragOutcome::Ignored;
        };

        {
            let mut g = self.gesture.borrow_mut();
            if !g.active {
                return DragOutcome::Ignored;
            }
            if !g.dragging {
                let slop = Self::DRAG_SLOP;
                if (x - g.down_x).hypot(y - g.down_y) <= slop {
                    // Still inside slop: not a drag yet, and a touch's long
                    // press is still possible.
                    return DragOutcome::Ignored;
                }
                g.dragging = true;
                // A touch that moved before the long-press timer fired is a
                // scroll, not a selection — cancel the pending long press.
                g.long_press_pending = false;
                if !g.select_armed {
                    // Scrolling starts from where the gesture began, not from
                    // where slop was exceeded: the travel spent getting past
                    // slop is real drag distance.
                    g.last_x = g.down_x;
                    g.last_y = g.down_y;
                }
            }
        }
        let (armed, dx, dy) = {
            let mut g = self.gesture.borrow_mut();
            let dx = x - g.last_x;
            let dy = y - g.last_y;
            g.last_x = x;
            g.last_y = y;
            (g.select_armed, dx, dy)
        };

        match kind {
            PointerKind::Mouse => {
                // Desktop drag: always a selection, from the press-anchored
                // cell to the one under the pointer.
                self.begin_selection(x, y, true);
                DragOutcome::Select
            }
            PointerKind::Touch if armed => {
                self.begin_selection(x, y, true);
                DragOutcome::Select
            }
            PointerKind::Touch => DragOutcome::Scroll { dx, dy },
        }
    }

    /// A pointer release. A gesture that never dragged is a tap
    /// ([`DragOutcome::Tap`], the same behaviour as
    /// [`click_at`](Self::click_at)); a drag just ends and is acknowledged with
    /// the matching outcome so the host can settle any inertia/fling.
    pub fn pointer_up(&self, x: f64, y: f64) -> DragOutcome {
        let (active, dragging) = {
            let g = self.gesture.borrow();
            (g.active, g.dragging)
        };
        if !active {
            return DragOutcome::Ignored;
        }
        {
            let mut g = self.gesture.borrow_mut();
            g.active = false;
            g.kind = None;
            g.dragging = false;
            g.select_armed = false;
            g.long_press_pending = false;
        }
        if dragging {
            // A drag ended where it ended: the host already applied the last
            // move, so there is nothing further to do (a fling/decay is the
            // host's business, and it has the deltas).
            DragOutcome::Ignored
        } else {
            // A press that never dragged is a click: `click_at` runs the host
            // callbacks and collapses any selection to the clicked cell, which
            // is also what clears the anchor a mouse press put in place.
            DragOutcome::Tap(self.click_at(x, y))
        }
    }

    /// Cancel an in-flight gesture (a phone call, a system gesture stealing
    /// the touch, the app losing focus). Never produces a tap.
    pub fn pointer_cancel(&self) {
        let mut g = self.gesture.borrow_mut();
        *g = GestureState::default();
    }

    /// Whether a long press on a touch device arms selection (the default).
    ///
    /// On by default because it is the only way to select a range on a phone.
    /// A host that gives long press another meaning (a context menu, say) can
    /// turn it off; a touch drag is then always a scroll and ranges are
    /// selected by the host's own means.
    pub fn set_long_press_select(&self, enabled: bool) {
        let mut g = self.gesture.borrow_mut();
        g.long_press_select = enabled;
    }

    /// Whether a drag is currently selecting (mouse drag in progress, or a
    /// touch drag after a long press). A host painting a drag cursor can ask.
    pub fn is_selecting_drag(&self) -> bool {
        let g = self.gesture.borrow();
        g.active && g.dragging && g.select_armed
    }

    /// Pixel travel, from the press origin, that turns a press into a drag.
    /// Roughly a finger's jitter on a phone and a pixel or two of mouse
    /// wobble: big enough that a tap is never a 1px scroll, small enough that
    /// a deliberate drag is immediate.
    const DRAG_SLOP: f64 = 6.0;

    /// Begin/extend a selection from the press anchor to `(x, y)`.
    ///
    /// The anchor is the cell under the press and the cursor follows the
    /// pointer; both live on the model so the renderer paints the band. Only a
    /// body cell can anchor or extend a selection (a header/gutter/outside hit
    /// leaves it alone), matching `click_at`'s rule that chrome is not a cell.
    /// `commit_anchor` is true only once the gesture has actually become a
    /// drag: a press must not leave a one-cell selection band behind, so the
    /// anchor captured at the press is published to the model only then.
    fn begin_selection(&self, x: f64, y: f64, commit_anchor: bool) {
        let hit = self.hit_test(x, y);
        if let GridHit::Cell { row, col } = hit {
            // The anchor is the cell the gesture *started* on, captured at the
            // press. Falling back to the current cursor (a gesture whose press
            // landed on chrome, or one started before this API existed) keeps
            // the band well-formed rather than empty.
            let anchor = self.gesture.borrow().press_anchor;
            let mut m = self.model.borrow_mut();
            let (r, c) = (row + m.header_row_count, col + m.margin_cols);
            if commit_anchor {
                m.anchor = Some(anchor.unwrap_or((
                    m.cursor_row.max(m.header_row_count),
                    m.cursor_col.max(m.margin_cols),
                )));
            }
            m.cursor_row = r;
            m.cursor_col = c;
            drop(m);
            let mut cb = self.handlers.borrow_mut().cursor_move.take();
            if let Some(cb) = cb.as_mut() {
                cb(row, col);
            }
            self.handlers.borrow_mut().cursor_move = cb;
        }
    }

    // ── internals ────────────────────────────────────────────────────────

    /// Run the zoom callback (if any) with the scale just applied. Taken out,
    /// called and put back, so a callback that reaches back into the grid
    /// (e.g. to read `zoom()`) never trips a `RefCell` double borrow.
    fn fire_zoom(&self, scale: f64) {
        let mut cb = self.handlers.borrow_mut().zoom.take();
        if let Some(cb) = cb.as_mut() {
            cb(scale);
        }
        self.handlers.borrow_mut().zoom = cb;
    }

    /// Body `(row, col)` → the model's own coordinates, which include the
    /// header band and the margin column.
    fn body_to_model(&self, row: u32, col: u32) -> (u32, u32) {
        let m = self.model.borrow();
        (row + m.header_row_count, col + m.margin_cols)
    }
}

/// Build a [`GridView`] from a whole [`Grid`] and render it headlessly — the
/// smallest useful end-to-end path, and what the tests exercise.
///
/// Only available with the `pancurses` feature: the terminal cell grid lives in
/// `backends::pancurses_draw`, which is itself feature-gated. The
/// `DrawContext` path (`GridView::paint`) works on every backend.
#[cfg(feature = "pancurses")]
pub fn render_grid(g: &Grid, w: u16, h: u16) -> crate::backends::pancurses_draw::CellGrid {
    let view = GridView::from_grid(g);
    view.render_terminal(w, h)
}

impl GridView {
    /// Render into a terminal cell grid (pancurses model). This is what a
    /// terminal host gets instead of a GTK canvas — the widget, not the
    /// backend, decides the surface. Requires the `pancurses` feature.
    #[cfg(feature = "pancurses")]
    pub fn render_terminal(&self, w: u16, h: u16) -> crate::backends::pancurses_draw::CellGrid {
        let m = self.model.borrow();
        crate::backends::pancurses_draw::render_model_to_grid(&m, w, h)
    }
}

/// Column widths actually used by a model, for hosts that need to size their
/// own surface: `(global_col, width_in_chars)`.
/// Column widths actually used by a model, for hosts that need to size their
/// own surface: `(global_col, width_in_chars)`. These are *layout* units
/// (character counts), so they are deliberately zoom-independent: a host
/// sizing a canvas wants the model's column count, and multiplies by
/// `zoom * CHAR_W` itself when it needs pixels.
pub fn column_widths(model: &SpreadsheetModel) -> HashMap<u32, u32> {
    let total = model.margin_cols + model.main_cols;
    (0..total)
        .map(|c| (c, (layout_col_width(model, c) / SpreadsheetModel::CHAR_W).round() as u32))
        .collect()
}

/// Width of one column in *layout* pixels at zoom 1.0 — the character count
/// the host supplied, or the renderer's 12-character default.
fn layout_col_width(model: &SpreadsheetModel, col: u32) -> f64 {
    for (gc, w, _) in &model.column_layout {
        if *gc == col {
            return *w as f64 * SpreadsheetModel::CHAR_W;
        }
    }
    12.0 * SpreadsheetModel::CHAR_W
}


/// Width of one column in **device** pixels: the layout width scaled by the
/// model's zoom, matching `spreadsheet::paint` and `SpreadsheetModel::col_width`
/// (which is private to the spreadsheet module). Keeping this the only scaled
/// width helper is what guarantees the hit-test and the renderer agree.
fn model_col_width(model: &SpreadsheetModel, col: u32) -> f64 {
    layout_col_width(model, col) * model.zoom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::headless::RecordingDrawContext;

    const CHAR_W: f64 = SpreadsheetModel::CHAR_W;
    const ROW_H: f64 = SpreadsheetModel::ROW_H;
    const LABEL_W: f64 = SpreadsheetModel::ROW_LABEL_W;

    #[test]
    fn new_grid_has_no_header_band_and_body_coordinates_are_bare() {
        let g = GridView::new(5, 3);
        assert_eq!(g.rows(), 5);
        assert_eq!(g.cols(), 3);
        // A body cell addressed as (0,0) lands on model (0,0): no header band,
        // no margin column, so a host never has to know about the offsets.
        g.set_cell(2, 1, "hi");
        assert_eq!("hi", g.cell(2, 1));
        assert_eq!("hi", g.model().borrow().cells.get(&(2, 1)).cloned().unwrap());
    }

    #[test]
    fn hit_test_maps_the_renderer_layout() {
        let g = GridView::new(4, 3);
        g.set_column_layout(vec![(0, 4, "A".into()), (1, 4, "B".into()), (2, 8, "C".into())]);

        // A data grid has no margin column, so its first column starts at 0
        // and the label gutter is not part of it.
        assert_eq!(GridHit::Cell { row: 0, col: 0 }, g.hit_test(0.0, 0.0));
        assert_eq!(GridHit::Cell { row: 0, col: 1 }, g.hit_test(4.0 * CHAR_W, 0.0));
        assert_eq!(GridHit::Cell { row: 0, col: 2 }, g.hit_test(8.0 * CHAR_W, 0.0));
        assert_eq!(GridHit::Cell { row: 1, col: 0 }, g.hit_test(0.0, ROW_H));
        // Past the last column (4+4+8 chars wide) / row is outside.
        assert_eq!(GridHit::Outside, g.hit_test(16.0 * CHAR_W, 0.0));
        assert_eq!(GridHit::Outside, g.hit_test(0.0, 4.0 * ROW_H));
    }

    #[test]
    fn header_band_click_reports_a_column_header() {
        // With a header row band, a click in it selects the column rather than
        // a cell — the distinction the old canvas-only path had to hardcode.
        let g = GridView::new(3, 2);
        g.set_row_counts(1, 3);
        g.set_column_layout(vec![(0, 4, "1".into()), (1, 4, "2".into())]);
        // Inside the header band → the column.
        assert_eq!(GridHit::ColumnHeader { col: 0 }, g.hit_test(0.0, 0.0));
        // One band down is still header row 0 → also the column.
        assert_eq!(
            GridHit::ColumnHeader { col: 0 },
            g.hit_test(0.0, SpreadsheetModel::HEADER_H)
        );
        // The first body row is the row after the header band.
        assert_eq!(
            GridHit::Cell { row: 0, col: 0 },
            g.hit_test(0.0, SpreadsheetModel::HEADER_H + SpreadsheetModel::ROW_H)
        );
    }

    #[test]
    fn click_moves_the_cursor_and_notifies() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let g = GridView::new(4, 3);
        g.set_column_layout(vec![(0, 4, "A".into()), (1, 4, "B".into()), (2, 4, "C".into())]);

        let seen: Rc<RefCell<Vec<(u32, u32)>>> = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        g.on_cell_click(move |r, c| s.borrow_mut().push((r, c)));
        let moved: Rc<RefCell<Vec<(u32, u32)>>> = Rc::new(RefCell::new(Vec::new()));
        let m = moved.clone();
        g.on_cursor_move(move |r, c| m.borrow_mut().push((r, c)));

        let hit = g.click_at(4.0 * CHAR_W, ROW_H);
        assert_eq!(GridHit::Cell { row: 1, col: 1 }, hit);
        assert_eq!(vec![(1, 1)], *seen.borrow());
        assert_eq!(vec![(1, 1)], *moved.borrow());
        assert_eq!((1, 1), g.cursor());

        // A click outside the rows/columns is a no-op for the cursor.
        g.click_at(0.0, 99.0 * ROW_H);
        assert_eq!((1, 1), g.cursor());
    }

    /// With a margin column the label gutter is real chrome: clicking it
    /// reports a row header and must not move the cursor into a cell.
    #[test]
    fn margin_gutter_is_chrome_not_a_cell() {
        let g = GridView::new(4, 2);
        g.set_grid_config(1, 2);
        g.set_column_layout(vec![(0, 8, "\u{2502}A".into()), (1, 8, "B".into())]);

        assert_eq!(GridHit::RowHeader { row: 0 }, g.hit_test(0.0, 0.0));
        g.set_cursor(2, 0);
        g.click_at(0.0, 0.0);
        assert_eq!((2, 0), g.cursor(), "gutter click leaves the cursor alone");
        // The first main column starts after the gutter.
        assert_eq!(
            GridHit::Cell { row: 0, col: 1 },
            g.hit_test(SpreadsheetModel::ROW_LABEL_W, 0.0)
        );
    }

    #[test]
    fn cursor_click_can_be_disabled() {
        let g = GridView::new(3, 2);
        g.set_cursor_click(false);
        g.click_at(0.0, 0.0);
        assert_eq!((0, 0), g.cursor(), "cursor stays where it was");
    }

    #[test]
    fn grid_model_round_trips_and_paints() {
        // A `core::Grid` as a host would build one (its fields are public).
        // Not `mut`: the interior mutability is the point, and the binding
        // itself is never rebound. The unnecessary `mut` was invisible on the
        // Linux feature sets this file is normally built with and only showed
        // up once the macOS test target was compiled.
        let g = Grid {
            cells: Rc::new(RefCell::new(HashMap::new())),
            raw_cells: Rc::new(RefCell::new(HashMap::new())),
            cell_styles: Rc::new(RefCell::new(HashMap::new())),
            total_rows: 3,
            total_cols: 2,
            top_row: 0,
            left_col: 0,
            cursor_row: 0,
            cursor_col: 0,
            editing: false,
            edit_buf: String::new(),
            edit_pos: 0,
            col_width: 4,
            margin_cols: 0,
            main_cols: 2,
            anchor: None,
            header_row_count: 0,
            main_row_count: 3,
            column_layout: Vec::new(),
            row_labels: Vec::new(),
        };
        g.cells.borrow_mut().insert((1, 0), "x".into());

        let view = GridView::from_grid(&g);
        assert_eq!("x", view.cell(1, 0));

        // It renders with the shared renderer, into any DrawContext.
        let mut dc = RecordingDrawContext::new();
        view.paint(&mut dc, 200, 100);
        assert!(!dc.ops.is_empty(), "paint produced draw ops");

        // …and into a terminal cell grid (the `pancurses` feature supplies it).
        #[cfg(feature = "pancurses")]
        {
            let cells = view.render_terminal(40, 10);
            assert_eq!(40, cells.w);
        }
    }

    #[test]
    fn column_widths_reports_the_layout() {
        let g = GridView::new(2, 3);
        g.set_column_layout(vec![(0, 5, "A".into()), (1, 7, "B".into()), (2, 9, "C".into())]);
        let m = g.model();
        let w = column_widths(&m.borrow());
        assert_eq!(Some(&5), w.get(&0));
        assert_eq!(Some(&7), w.get(&1));
        assert_eq!(Some(&9), w.get(&2));
    }

    // ── zoom ─────────────────────────────────────────────────────────────

    #[test]
    fn zoom_defaults_to_one_and_clamps() {
        let g = GridView::new(4, 3);
        assert_eq!(1.0, g.zoom());

        // Direct setter, inside and outside the range.
        assert_eq!(2.0, g.set_zoom(2.0));
        assert_eq!(2.0, g.zoom());
        assert_eq!(SpreadsheetModel::MAX_ZOOM, g.set_zoom(1000.0));
        assert_eq!(SpreadsheetModel::MIN_ZOOM, g.set_zoom(0.0001));
        // An infinity is clampable ("as big/small as possible"): it reaches the
        // bound rather than being treated as nonsense.
        assert_eq!(SpreadsheetModel::MAX_ZOOM, g.set_zoom(f64::INFINITY));
        assert_eq!(SpreadsheetModel::MIN_ZOOM, g.set_zoom(f64::NEG_INFINITY));
        // NaN has no ordering, so it means "no zoom" (the identity scale).
        assert_eq!(1.0, g.set_zoom(f64::NAN));
        assert_eq!(1.0, g.reset_zoom());
    }

    #[test]
    fn zoom_by_multiplies_and_the_callback_fires() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let g = GridView::new(4, 3);
        let seen: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        g.on_zoom(move |z| s.borrow_mut().push(z));

        // A pinch is a sequence of ratios around a focus point.
        assert!((g.zoom_by(1.5, 10.0, 10.0) - 1.5).abs() < 1e-9);
        assert!((g.zoom_by(2.0, 10.0, 10.0) - 3.0).abs() < 1e-9);
        assert_eq!(vec![1.5, 3.0], *seen.borrow());

        // Past the ceiling the reported scale is the clamp, not the product.
        let capped = g.zoom_by(10.0, 0.0, 0.0);
        assert_eq!(SpreadsheetModel::MAX_ZOOM, capped);
        assert_eq!(Some(&SpreadsheetModel::MAX_ZOOM), seen.borrow().last());

        // Degenerate factors are ignored (a pinch of zero span, or NaN).
        let before = g.zoom();
        assert_eq!(before, g.zoom_by(0.0, 0.0, 0.0));
        assert_eq!(before, g.zoom_by(f64::NAN, 0.0, 0.0));
    }

    /// The whole point of a zoom that lives in the model: the hit-test and the
    /// renderer move together, so a tap still lands on the cell under it.
    #[test]
    fn hit_test_follows_the_zoom() {
        let g = GridView::new(4, 3);
        g.set_column_layout(vec![(0, 4, "A".into()), (1, 4, "B".into()), (2, 4, "C".into())]);

        // At zoom 1 the second column starts 4 chars in.
        assert_eq!(GridHit::Cell { row: 0, col: 1 }, g.hit_test(4.0 * CHAR_W, 0.0));
        // At 2x it starts twice as far in, and the old pixel is now column 0.
        g.set_zoom(2.0);
        assert_eq!(GridHit::Cell { row: 0, col: 1 }, g.hit_test(8.0 * CHAR_W, 0.0));
        assert_eq!(GridHit::Cell { row: 0, col: 0 }, g.hit_test(4.0 * CHAR_W, 0.0));
        assert_eq!(GridHit::Cell { row: 1, col: 0 }, g.hit_test(0.0, 2.0 * ROW_H));
        // Past the (now doubled) content is still outside.
        assert_eq!(GridHit::Outside, g.hit_test(32.0 * CHAR_W, 0.0));
    }

    /// `paint` and `hit_test` must scale by the same factor: the renderer's
    /// cell fills grow by `zoom` and the pointer math that maps a pixel back to
    /// a cell grows with them.
    ///
    /// The two are asserted on their own terms because a *margin-less* grid
    /// deliberately hit-tests from x = 0 while the renderer anchors its layout
    /// at `row_label_w` (documented in `hit_test_maps_the_renderer_layout`);
    /// that pre-existing offset is orthogonal to zoom, so it is not what this
    /// test is about.
    #[test]
    fn paint_and_hit_test_agree_at_a_zoom() {
        let g = GridView::new(4, 2);
        g.set_column_layout(vec![(0, 4, "A".into()), (1, 4, "B".into())]);
        g.set_cell(1, 1, "hi");

        // Renderer side: at 1x the second column is 4 chars in and 4 wide.
        let mut dc = RecordingDrawContext::new();
        g.paint(&mut dc, 400, 200);
        let at_1x = dc
            .fill_rects()
            .into_iter()
            .find(|r| r.y == ROW_H && r.x == LABEL_W + 4.0 * CHAR_W)
            .expect("row 1, column 1 fill");
        assert_eq!(LABEL_W + 4.0 * CHAR_W, at_1x.x);
        assert_eq!(4.0 * CHAR_W, at_1x.w);
        assert_eq!(ROW_H, at_1x.h);

        // Pointer side: the same point reports column 1 (x=0 is column 0, so
        // the widget's own origin).
        assert_eq!(GridHit::Cell { row: 1, col: 1 }, g.hit_test(4.0 * CHAR_W, ROW_H));

        // Double the scale: every metric in both paths doubles.
        g.set_zoom(2.0);
        let mut dc = RecordingDrawContext::new();
        g.paint(&mut dc, 400, 200);
        let at_2x = dc
            .fill_rects()
            .into_iter()
            .find(|r| r.y == ROW_H * 2.0 && r.x == LABEL_W * 2.0 + 8.0 * CHAR_W)
            .expect("row 1, column 1 fill at the scaled position");
        assert_eq!(LABEL_W * 2.0 + 8.0 * CHAR_W, at_2x.x);
        assert_eq!(8.0 * CHAR_W, at_2x.w);
        assert_eq!(ROW_H * 2.0, at_2x.h);

        assert_eq!(
            GridHit::Cell { row: 1, col: 1 },
            g.hit_test(8.0 * CHAR_W, ROW_H * 2.0)
        );
        // The old 1x pixel now falls inside column 0, half a column wide.
        assert_eq!(GridHit::Cell { row: 0, col: 0 }, g.hit_test(4.0 * CHAR_W, ROW_H));
    }

    #[test]
    fn zoom_callback_can_read_back_the_scale() {
        // A callback that re-enters the widget (reads `zoom()`) must not trip
        // a RefCell double borrow: the handler is taken out before it runs.
        use std::cell::Cell;
        use std::rc::Rc;
        let g = GridView::new(2, 2);
        let g2 = g.clone();
        let seen = Rc::new(Cell::new(0.0));
        let s = seen.clone();
        g.on_zoom(move |z| s.set(g2.zoom() * 2.0 + z * 0.0));
        g.set_zoom(1.5);
        assert!((seen.get() - 3.0).abs() < 1e-9);
    }

    // ── platform-aware drag ──────────────────────────────────────────────

    fn layout(g: &GridView) {
        g.set_column_layout(vec![
            (0, 4, "A".into()),
            (1, 4, "B".into()),
            (2, 4, "C".into()),
            (3, 4, "D".into()),
        ]);
    }

    /// Desktop: press-and-drag selects a range immediately, exactly like a
    /// spreadsheet on a monitor.
    #[test]
    fn mouse_drag_extends_the_selection() {
        let g = GridView::new(4, 4);
        layout(&g);
        g.set_cell(0, 0, "a");

        assert_eq!(DragOutcome::Select, g.pointer_down(0.0, 0.0, PointerKind::Mouse));
        assert_eq!((0, 0), g.cursor());
        // A press alone is not yet a *drag* (it could still be a tap), but the
        // desktop gesture is already committed to selecting.
        assert!(!g.is_selecting_drag());

        // Drag right/down two cells: the anchor stays at the press origin and
        // the cursor follows the pointer.
        assert_eq!(
            DragOutcome::Select,
            g.pointer_move(2.0 * 4.0 * CHAR_W, 2.0 * ROW_H)
        );
        assert_eq!((2, 2), g.cursor());
        assert!(g.is_selecting_drag());
        let m = g.model();
        let m = m.borrow();
        assert_eq!(Some((0, 0)), m.anchor, "anchor is the press cell");

        // Release ends the drag without emitting a tap.
        assert_eq!(DragOutcome::Ignored, g.pointer_up(2.0 * 4.0 * CHAR_W, 2.0 * ROW_H));
        assert!(!g.is_selecting_drag());
    }

    /// A desktop press that does not move is still a click.
    #[test]
    fn mouse_press_without_movement_is_a_tap() {
        let g = GridView::new(4, 4);
        layout(&g);
        g.set_cursor(3, 3);
        assert_eq!(DragOutcome::Select, g.pointer_down(4.0 * CHAR_W, ROW_H, PointerKind::Mouse));
        // The press itself moved the cursor onto the pressed cell.
        assert_eq!((1, 1), g.cursor());
        // A press alone must not leave a selection band behind: the anchor a
        // press captures is only committed once a drag begins.
        let m = g.model();
        assert_eq!(None, m.borrow().anchor);
        assert_eq!(
            DragOutcome::Tap(GridHit::Cell { row: 1, col: 1 }),
            g.pointer_up(4.0 * CHAR_W, ROW_H)
        );
        // …and the release (a real click) collapses any selection.
        assert_eq!(None, m.borrow().anchor);
    }

    /// A shaky tap (inside the slop) is still a tap, not a 1px scroll.
    #[test]
    fn touch_tap_inside_slop_is_a_tap() {
        let g = GridView::new(4, 4);
        layout(&g);
        assert_eq!(DragOutcome::Ignored, g.pointer_down(40.0, 40.0, PointerKind::Touch));
        assert_eq!(DragOutcome::Ignored, g.pointer_move(42.0, 41.0));
        assert_eq!(
            DragOutcome::Tap(GridHit::Cell { row: 1, col: 1 }),
            g.pointer_up(42.0, 41.0)
        );
    }

    /// Touch: a drag scrolls by default, reporting pixel deltas rather than
    /// moving the selection.
    #[test]
    fn touch_drag_scrolls_unless_long_pressed() {
        let g = GridView::new(4, 4);
        layout(&g);
        g.set_cursor(0, 0);

        assert_eq!(DragOutcome::Ignored, g.pointer_down(0.0, 0.0, PointerKind::Touch));
        // First move past slop: scrolled from the *press* origin, so the whole
        // travel counts (30px down/right, not just the ~6px of slop).
        let out = g.pointer_move(30.0, 30.0);
        assert_eq!(DragOutcome::Scroll { dx: 30.0, dy: 30.0 }, out);
        // The cursor did NOT move: the drag panned, it did not select.
        assert_eq!((0, 0), g.cursor());
        assert!(!g.is_selecting_drag());

        // Subsequent moves report only the incremental delta.
        assert_eq!(DragOutcome::Scroll { dx: -10.0, dy: 5.0 }, g.pointer_move(20.0, 35.0));
        // Release after a drag is not a tap.
        assert_eq!(DragOutcome::Ignored, g.pointer_up(20.0, 35.0));
    }

    /// Touch: a long press arms selection, so the *following* drag extends it
    /// instead of scrolling.
    #[test]
    fn touch_long_press_arms_selection_for_the_drag() {
        let g = GridView::new(4, 4);
        layout(&g);
        g.set_cursor(2, 2);

        g.pointer_down(0.0, 0.0, PointerKind::Touch);
        // The host's timer fires with the finger still down and still.
        assert_eq!(
            DragOutcome::LongPress(GridHit::Cell { row: 0, col: 0 }),
            g.pointer_long_press(0.0, 0.0)
        );
        // The long press itself does not jump the cursor: it has only armed
        // selection for the drag that follows.
        assert_eq!((2, 2), g.cursor());
        // The drag then selects, anchored on the cell the finger went down on
        // — that is what the user is pointing at, not wherever the cursor
        // happened to be — and extends to the pointer.
        assert_eq!(
            DragOutcome::Select,
            g.pointer_move(2.0 * 4.0 * CHAR_W, 2.0 * ROW_H)
        );
        assert_eq!((2, 2), g.cursor());
        assert!(g.is_selecting_drag());
        let m = g.model();
        assert_eq!(Some((0, 0)), m.borrow().anchor, "anchored on the press cell");
    }

    /// A touch that moved before the long-press timer fired is a scroll: the
    /// pending long press is cancelled, so a late timer cannot turn a pan into
    /// a selection half way through.
    #[test]
    fn movement_cancels_a_pending_long_press() {
        let g = GridView::new(4, 4);
        layout(&g);
        g.pointer_down(0.0, 0.0, PointerKind::Touch);
        assert!(matches!(g.pointer_move(50.0, 0.0), DragOutcome::Scroll { .. }));
        // The timer fires late; the gesture is already a scroll, so it is
        // ignored and later moves keep scrolling.
        assert_eq!(DragOutcome::Ignored, g.pointer_long_press(50.0, 0.0));
        assert!(matches!(g.pointer_move(60.0, 0.0), DragOutcome::Scroll { .. }));
    }

    /// A host can give long press another meaning: with it off, a touch drag
    /// is always a scroll.
    #[test]
    fn long_press_select_can_be_disabled() {
        let g = GridView::new(4, 4);
        layout(&g);
        g.set_long_press_select(false);
        g.pointer_down(0.0, 0.0, PointerKind::Touch);
        assert_eq!(DragOutcome::Ignored, g.pointer_long_press(0.0, 0.0));
        assert!(matches!(g.pointer_move(30.0, 0.0), DragOutcome::Scroll { .. }));
    }

    /// A cancelled gesture (system interruption) never becomes a tap.
    #[test]
    fn pointer_cancel_drops_the_gesture() {
        let g = GridView::new(4, 4);
        layout(&g);
        g.set_cursor(3, 3);
        g.pointer_down(0.0, 0.0, PointerKind::Touch);
        g.pointer_cancel();
        assert_eq!(DragOutcome::Ignored, g.pointer_up(0.0, 0.0));
        assert_eq!((3, 3), g.cursor(), "the cancelled tap moved nothing");
    }
}
