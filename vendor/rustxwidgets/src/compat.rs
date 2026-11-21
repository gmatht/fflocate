//! Widgets the merged backends lost, rebuilt on the ones they kept.
//!
//! corro's newer rustxWidgets dropped `TreeView`, `ContextMenu` and `Expander`
//! (and with them `CellValue`, `ScrollDirection`, placeholder text, sensitivity
//! and the clipboard helpers). Clients such as `ff-gui` are written against the
//! older surface, so this module re-provides it on top of the *portable* layer
//! (`Canvas`, `BoxWidget`, `Button`, `ScrolledWindow`, `Menu`) rather than per
//! backend — which is what makes one implementation serve GTK and AppKit.
//!
//! Fidelity is stated per widget rather than implied:
//!
//! * [`TreeView`] draws its header and rows on a canvas, hit-tests clicks,
//!   tracks a selection and exposes the same calls the old GtkTreeView wrapper
//!   did. Clicking a sortable header sorts by that column, a second click
//!   reverses it, and the header shows the direction; its icon column paints a
//!   pictogram per file kind (see `paint_icon`), not a themed pixbuf.
//! * [`Expander`] is the backend's own disclosure widget where it has one —
//!   GTK3's `GtkExpander`, arrow and clickable header included — and elsewhere
//!   a header line plus a child box, with the state in the header text and the
//!   caller doing the toggling (see [`Expander::set_expanded`]).
//! * [`ContextMenu`] collects items, but popping one up is *not* implemented in
//!   this build: [`ContextMenu::popup_at_pointer`] returns an error, which
//!   clients surface to the user. That is deliberate — a menu that silently
//!   never appears is worse than one that says why.

use crate::common::{Canvas, Label, ScrolledWindow, WidgetBox};
use crate::core::{App, DrawContext};
use crate::Error;
use std::cell::{Cell, RefCell};
use std::os::raw::c_void;
use std::rc::Rc;

/// The list's viewport geometry and offscreen per-row ink.
pub mod reflow;

/// One cell of a [`TreeView`] row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellValue {
    Text(String),
    /// A themed icon, named as the old backend named it. The canvas paints a
    /// pictogram from `name`, because a themed pixbuf is not reachable from the
    /// portable layer; `fallback` (a character or short word) is what a backend
    /// with no canvas — the `zork` text fallback — prints instead.
    Icon { name: String, fallback: String },
}

impl CellValue {
    fn render(&self) -> String {
        match self {
            CellValue::Text(t) => t.clone(),
            CellValue::Icon { fallback, .. } => fallback.clone(),
        }
    }
}

/// Wheel direction delivered to a scroll handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
    Smooth,
}

// --------------------------------------------------------------------------
// Construction helpers: one per platform, so the widgets below stay portable.
// --------------------------------------------------------------------------

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
fn new_canvas() -> Result<Canvas, Error> {
    Ok(Canvas {
        inner: crate::backends_gtk_adapter::create_canvas()?,
    })
}

#[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
fn new_canvas() -> Result<Canvas, Error> {
    Ok(Canvas {
        inner: crate::backends_macos_adapter::create_canvas()?,
    })
}

#[cfg(feature = "zork")]
fn new_canvas() -> Result<Canvas, Error> {
    Ok(Canvas {
        inner: crate::backends_zork_adapter::create_canvas()?,
    })
}

#[cfg(not(any(
    all(feature = "gtk", target_os = "linux", not(feature = "zork")),
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    feature = "zork"
)))]
fn new_canvas() -> Result<Canvas, Error> {
    Err(Error::Backend("no canvas on this backend".to_string()))
}

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
fn new_vbox(spacing: i32) -> Result<WidgetBox, Error> {
    Ok(WidgetBox {
        inner: crate::backends_gtk_adapter::create_box(
            crate::backends_gtk_adapter::Orientation::Vertical,
            spacing,
        )?,
    })
}

#[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
fn new_vbox(spacing: i32) -> Result<WidgetBox, Error> {
    Ok(WidgetBox {
        inner: crate::backends_macos_adapter::create_box(
            crate::backends_macos_adapter::Orientation::Vertical,
            spacing,
        )?,
    })
}

#[cfg(feature = "zork")]
fn new_vbox(spacing: i32) -> Result<WidgetBox, Error> {
    Ok(WidgetBox {
        inner: crate::backends_zork_adapter::create_box(
            crate::backends_zork_adapter::Orientation::Vertical,
            spacing,
        )?,
    })
}

#[cfg(not(any(
    all(feature = "gtk", target_os = "linux", not(feature = "zork")),
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    feature = "zork"
)))]
fn new_vbox(_spacing: i32) -> Result<WidgetBox, Error> {
    Err(Error::Backend("no box container on this backend".to_string()))
}

/// Show or hide a box and everything in it.
///
/// One body for the three backends that build an [`Expander`] (they all give
/// their box a `set_visible`); the backends that cannot build one never call
/// this. Hiding the body is what makes a disclosure collapse: a hidden box
/// takes no space in its parent.
#[cfg(any(
    all(feature = "gtk", target_os = "linux", not(feature = "zork")),
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    feature = "zork"
))]
fn set_box_visible(b: &WidgetBox, visible: bool) {
    b.inner.set_visible(visible);
}

#[cfg(not(any(
    all(feature = "gtk", target_os = "linux", not(feature = "zork")),
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    feature = "zork"
)))]
fn set_box_visible(_b: &WidgetBox, _visible: bool) {}

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
fn new_label(text: &str) -> Result<crate::common::Label, Error> {
    Ok(crate::common::Label {
        inner: crate::backends_gtk_adapter::create_label(text)?,
    })
}

#[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
fn new_label(text: &str) -> Result<crate::common::Label, Error> {
    Ok(crate::common::Label {
        inner: crate::backends_macos_adapter::create_label(text)?,
    })
}

#[cfg(feature = "zork")]
fn new_label(text: &str) -> Result<crate::common::Label, Error> {
    Ok(crate::common::Label {
        inner: crate::backends_zork_adapter::create_label(text)?,
    })
}

#[cfg(not(any(
    all(feature = "gtk", target_os = "linux", not(feature = "zork")),
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    feature = "zork"
)))]
fn new_label(_text: &str) -> Result<crate::common::Label, Error> {
    Err(Error::Backend("no label on this backend".to_string()))
}

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
fn new_scrolled() -> Result<ScrolledWindow, Error> {
    Ok(ScrolledWindow {
        inner: crate::backends_gtk_adapter::create_scrolled_window()?,
    })
}

#[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
fn new_scrolled() -> Result<ScrolledWindow, Error> {
    Ok(ScrolledWindow {
        inner: crate::backends_macos_adapter::create_scrolled_window()?,
    })
}

#[cfg(feature = "zork")]
fn new_scrolled() -> Result<ScrolledWindow, Error> {
    Ok(ScrolledWindow {
        inner: crate::backends_zork_adapter::create_scrolled_window()?,
    })
}

#[cfg(not(any(
    all(feature = "gtk", target_os = "linux", not(feature = "zork")),
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    feature = "zork"
)))]
fn new_scrolled() -> Result<ScrolledWindow, Error> {
    Err(Error::Backend("no scrolled window on this backend".to_string()))
}

/// Row height in logical pixels. Fixed: the canvas has no per-row layout.
const ROW_HEIGHT: f64 = 20.0;
/// Header height in logical pixels.
const HEADER_HEIGHT: f64 = 22.0;
/// Column widths as fractions of the canvas width: the icon column is narrow,
/// name and path share the rest.
const ICON_COLUMN_SHARE: f64 = 0.06;
/// Left inset of the first column, in logical pixels.
const PAD_X: f64 = 6.0;
/// The family the whole list is drawn in.
const FONT: &str = "sans";
/// Body text size in logical pixels.
const BODY_SIZE: f64 = 12.0;
/// Icon-glyph size: a step down from the body, so the glyph fits the narrow
/// icon column.
const ICON_SIZE: f64 = 11.0;
/// Header title size in logical pixels.
const HEADER_SIZE: f64 = 12.0;
/// Width of the list's own scrollbar strip, in logical pixels. Only drawn when
/// the rows are taller than the canvas.
const SCROLLBAR_WIDTH: f64 = 10.0;
/// Rows one wheel notch moves the list.
const WHEEL_ROWS: f64 = 3.0;
/// The height the canvas asks for, in logical pixels.
///
/// The canvas is never sized to the content (a 600 000-row list would ask for
/// 12 000 000 px, past what a `GdkWindow`/Cairo surface can hold), so it asks
/// for a screenful and every backend stretches it to the viewport it is given.
const CANVAS_REQUEST_H: i32 = 480;

type PressHandler = Box<dyn FnMut(u32, f64, f64, *mut c_void)>;

/// Called with the row index when a row is double-clicked.
type ActivateHandler = Box<dyn FnMut(usize)>;

/// A three-column results list, drawn on a canvas and hit-tested by hand.
pub struct TreeView {
    canvas: Canvas,
    columns: Vec<(String, bool, bool)>,
    rows: Rc<RefCell<Vec<Vec<CellValue>>>>,
    selected: Rc<Cell<Option<usize>>>,
    press: Rc<RefCell<Vec<PressHandler>>>,
    width: Rc<Cell<f64>>,
    /// The active sort: `(column, ascending)`. `None` keeps the order the
    /// caller passed to [`TreeView::set_rows`].
    sort: Rc<Cell<Option<(usize, bool)>>>,
    /// Row-activation handlers, fed by the backend's double click.
    activate: Rc<RefCell<Vec<ActivateHandler>>>,
    /// How far down the rows the canvas is scrolled, in pixels, in **content**
    /// coordinates: row `i` sits at `HEADER_HEIGHT + i * ROW_HEIGHT - offset`.
    offset: Rc<Cell<f64>>,
    /// The height of the canvas being drawn (the viewport), which is how much
    /// of the list is on screen and therefore all the painter draws.
    viewport: Rc<Cell<f64>>,
    /// True while the scrollbar's thumb is being dragged.
    dragging: Rc<Cell<bool>>,
}

impl Clone for TreeView {
    fn clone(&self) -> Self {
        TreeView {
            canvas: self.canvas.clone(),
            columns: self.columns.clone(),
            rows: self.rows.clone(),
            selected: self.selected.clone(),
            press: self.press.clone(),
            width: self.width.clone(),
            sort: self.sort.clone(),
            activate: self.activate.clone(),
            offset: self.offset.clone(),
            viewport: self.viewport.clone(),
            dragging: self.dragging.clone(),
        }
    }
}

/// One press at `(x, y)`, exactly as the backend's button signal handles it: a
/// press in the header band sorts by that column, a left press selects the row
/// under the pointer, and every registered press handler runs.
///
/// The canvas closure and [`TreeView::click_at`] both call this, so a test that
/// drives a click is driving the list, not a copy of its click logic.
fn handle_press(
    rows: &Rc<RefCell<Vec<Vec<CellValue>>>>,
    sort: &Rc<Cell<Option<(usize, bool)>>>,
    sortable: &[bool],
    width: f64,
    selected: &Rc<Cell<Option<usize>>>,
    press: &Rc<RefCell<Vec<PressHandler>>>,
    offset: f64,
    x: f64,
    y: f64,
    button: u32,
) -> Option<usize> {
    // The canvas is the viewport, so a press at `y` is at content `y + offset`.
    let y = y + offset.max(0.0);
    let row = row_index_at(y, rows.borrow().len());
    if button == 1 {
        if y < HEADER_HEIGHT {
            // A header click sorts by that column (the old `GtkTreeView` did;
            // the canvas list does it itself). It is not a row selection, so
            // the selection goes.
            if apply_header_click(rows, sort, sortable, width, x) {
                selected.set(None);
            }
        } else {
            selected.set(row);
        }
    }
    for handler in press.borrow_mut().iter_mut() {
        handler(button, x, y, std::ptr::null_mut());
    }
    row
}

/// The second press of a double click at `y`: select the row under the pointer
/// and run every activation handler. Returns the row, or `None` over the header
/// or blank space.
///
/// A double click is a second press on the same row, so its first press already
/// selected it; the selection is set again so a handler reading it sees this row.
fn handle_double_click(
    rows: &Rc<RefCell<Vec<Vec<CellValue>>>>,
    selected: &Rc<Cell<Option<usize>>>,
    activate: &Rc<RefCell<Vec<ActivateHandler>>>,
    offset: f64,
    y: f64,
) -> Option<usize> {
    let row = row_index_at(y + offset.max(0.0), rows.borrow().len())?;
    selected.set(Some(row));
    for handler in activate.borrow_mut().iter_mut() {
        handler(row);
    }
    Some(row)
}

impl TreeView {
    pub fn new(columns: &[(&str, bool, bool)]) -> Result<TreeView, Error> {
        let canvas = new_canvas()?;
        let rows: Rc<RefCell<Vec<Vec<CellValue>>>> = Rc::new(RefCell::new(Vec::new()));
        let selected: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let press: Rc<RefCell<Vec<PressHandler>>> = Rc::new(RefCell::new(Vec::new()));
        let width: Rc<Cell<f64>> = Rc::new(Cell::new(640.0));
        let sort: Rc<Cell<Option<(usize, bool)>>> = Rc::new(Cell::new(None));
        let activate: Rc<RefCell<Vec<ActivateHandler>>> = Rc::new(RefCell::new(Vec::new()));
        let offset: Rc<Cell<f64>> = Rc::new(Cell::new(0.0));
        let viewport: Rc<Cell<f64>> = Rc::new(Cell::new(f64::from(CANVAS_REQUEST_H)));
        let dragging: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // A screenful, never the content: the list scrolls itself (see
        // `paint_list`), so the canvas only has to be as tall as its viewport.
        canvas.set_content_size(720, CANVAS_REQUEST_H);

        {
            let rows = rows.clone();
            let selected = selected.clone();
            let sort = sort.clone();
            let columns: Vec<(String, bool, bool)> = columns
                .iter()
                .map(|(t, icon, sortable)| (t.to_string(), *icon, *sortable))
                .collect();
            let width = width.clone();
            let offset = offset.clone();
            let viewport = viewport.clone();
            canvas.set_draw_callback(Box::new(move |ctx, w, h| {
                let w = if w > 0 { f64::from(w) } else { width.get() };
                width.set(w);
                // `h` is the viewport: the canvas is never the content, so this
                // is exactly how many rows are on screen.
                if h > 0 {
                    viewport.set(f64::from(h));
                }
                let vp = viewport.get();
                let content = content_height(rows.borrow().len());
                let off = clamp_offset(offset.get(), content, vp);
                offset.set(off);
                paint_list(
                    ctx,
                    w,
                    &columns,
                    &rows.borrow(),
                    selected.get(),
                    sort.get(),
                    off,
                    vp,
                );
            }));
        }

        {
            let press = press.clone();
            let selected = selected.clone();
            let rows = rows.clone();
            let sort = sort.clone();
            let width = width.clone();
            let canvas_for_redraw = canvas.clone();
            let sortable: Vec<bool> = columns.iter().map(|(_, _, sortable)| *sortable).collect();
            let offset = offset.clone();
            let viewport = viewport.clone();
            let dragging = dragging.clone();
            canvas.on_click_button(Box::new(move |x, y, button, _state| {
                let content = content_height(rows.borrow().len());
                let vp = viewport.get();
                // A press on the list's own scrollbar grabs the thumb there and
                // starts a drag; it is not a row click.
                if button == 1 && scrollbar_hit(width.get(), x, content, vp) {
                    dragging.set(true);
                    offset.set(offset_from_thumb(y, content, vp));
                    canvas_for_redraw.queue_redraw();
                    return;
                }
                dragging.set(false);
                handle_press(
                    &rows,
                    &sort,
                    &sortable,
                    width.get(),
                    &selected,
                    &press,
                    offset.get(),
                    x,
                    y,
                    button,
                );
                canvas_for_redraw.queue_redraw();
            }));
        }

        // Dragging the thumb: a press in the strip moves the window there, and
        // motion follows the pointer until the button comes up.
        {
            let drag_flag = dragging.clone();
            let offset = offset.clone();
            let viewport = viewport.clone();
            let rows = rows.clone();
            let canvas_for_redraw = canvas.clone();
            canvas.on_motion(Box::new(move |_x, y, _state| {
                if !drag_flag.get() {
                    return;
                }
                let content = content_height(rows.borrow().len());
                offset.set(offset_from_thumb(y, content, viewport.get()));
                canvas_for_redraw.queue_redraw();
            }));
            let drag_flag = dragging.clone();
            canvas.on_release(Box::new(move |_x, _y, _button, _state| drag_flag.set(false)));
        }

        // The wheel: three rows a notch, clamped to the content.
        {
            let offset = offset.clone();
            let viewport = viewport.clone();
            let rows = rows.clone();
            let canvas_for_redraw = canvas.clone();
            install_wheel(
                &canvas,
                Box::new(move |dy: f64| {
                    let content = content_height(rows.borrow().len());
                    let vp = viewport.get();
                    let next = clamp_offset(
                        offset.get() + dy * WHEEL_ROWS * ROW_HEIGHT,
                        content,
                        vp,
                    );
                    if (next - offset.get()).abs() > 0.01 {
                        offset.set(next);
                        canvas_for_redraw.queue_redraw();
                    }
                }),
            );
        }

        {
            let activate = activate.clone();
            let selected = selected.clone();
            let rows = rows.clone();
            let offset = offset.clone();
            let canvas_for_redraw = canvas.clone();
            register_double_click(
                &canvas,
                Box::new(move |_x, y| {
                    handle_double_click(&rows, &selected, &activate, offset.get(), y);
                    canvas_for_redraw.queue_redraw();
                }),
            );
        }

        Ok(TreeView {
            canvas,
            columns: columns
                .iter()
                .map(|(t, icon, sortable)| (t.to_string(), *icon, *sortable))
                .collect(),
            rows,
            selected,
            press,
            width,
            sort,
            activate,
            offset,
            viewport,
            dragging,
        })
    }

    /// Feed a press through the list the way the backend's button signal does.
    ///
    /// The canvas closure calls the same code; this is how a test can exercise
    /// a click where the toolkit has no way to synthesise a native event (the
    /// GTK interaction test), without re-implementing the hit-test.
    pub fn click_at(&self, x: f64, y: f64, button: u32) -> Option<usize> {
        let sortable: Vec<bool> = self.columns.iter().map(|(_, _, s)| *s).collect();
        let row = handle_press(
            &self.rows,
            &self.sort,
            &sortable,
            self.width.get(),
            &self.selected,
            &self.press,
            self.offset.get(),
            x,
            y,
            button,
        );
        self.canvas.queue_redraw();
        row
    }

    /// Feed the second press of a double click at `y`, running the activation
    /// handlers exactly as the canvas' double-click registration does.
    pub fn double_click_at(&self, y: f64) -> Option<usize> {
        let row = handle_double_click(
            &self.rows,
            &self.selected,
            &self.activate,
            self.offset.get(),
            y,
        );
        self.canvas.queue_redraw();
        row
    }

    /// Replace every row, then repaint.
    ///
    /// The rows are sorted on the way in, so a table sorted by Name stays
    /// sorted while a search streams in more hits.
    pub fn set_rows(&self, rows: &[Vec<CellValue>]) {
        let mut rows = rows.to_vec();
        sort_rows(&mut rows, self.sort.get());
        *self.rows.borrow_mut() = rows;
        self.after_rows_changed();
    }

    /// Scroll to `offset` pixels down the rows, clamped to the content.
    pub fn set_offset(&self, offset: f64) {
        let content = content_height(self.rows.borrow().len());
        let next = clamp_offset(offset, content, self.viewport.get());
        if (next - self.offset.get()).abs() > 0.01 {
            self.offset.set(next);
            self.canvas.queue_redraw();
        }
    }

    /// How far down the rows the list is scrolled, in pixels.
    pub fn offset(&self) -> f64 {
        self.offset.get()
    }

    /// The height of the viewport the list was last drawn in.
    pub fn viewport_height(&self) -> f64 {
        self.viewport.get()
    }

    /// Scroll by `rows` rows (positive scrolls down), as the wheel does.
    pub fn scroll_by_rows(&self, rows: f64) {
        self.set_offset(self.offset.get() + rows * ROW_HEIGHT);
    }

    /// The offset that puts the thumb at `y` in the strip: a track click or a
    /// drag. Exposed so a test drives the same arithmetic the canvas does.
    pub fn offset_at_thumb_y(&self, y: f64) -> f64 {
        let content = content_height(self.rows.borrow().len());
        offset_from_thumb(y, content, self.viewport.get())
    }

    /// Whether the list is drawing its own scrollbar at this viewport height.
    pub fn shows_scrollbar(&self) -> bool {
        let content = content_height(self.rows.borrow().len());
        content > self.viewport.get()
    }

    /// After the rows changed: keep the offset inside the content and repaint.
    ///
    /// The canvas is asked for a screenful, never for the content height: the
    /// list scrolls itself, so the drawing area must stay bounded no matter how
    /// many rows it holds.
    fn after_rows_changed(&self) {
        let content = content_height(self.rows.borrow().len());
        self.offset
            .set(clamp_offset(self.offset.get(), content, self.viewport.get()));
        self.canvas.set_content_size(
            self.width.get().max(320.0) as i32,
            CANVAS_REQUEST_H,
        );
        self.canvas.queue_redraw();
    }

    /// Append `batch` to the rows, keeping the active sort's order, and keep at
    /// most `limit` rows.
    ///
    /// The GUI's whole-set sort streams hits in crawl order: batches arrive
    /// unsorted and far more numerous than the list can draw. Sorting each
    /// batch and merging it ([`merge_rows`]) keeps the visible top `limit` rows
    /// correct without ever re-sorting or cloning the rows already there, so a
    /// reply with six figures of hits costs O(batch log batch + limit) per
    /// batch instead of O(n log n) plus a full clone.
    pub fn append_rows(&self, batch: &[Vec<CellValue>]) {
        if batch.is_empty() {
            return;
        }
        {
            let mut rows = self.rows.borrow_mut();
            merge_rows(&mut rows, batch, self.sort.get(), usize::MAX);
        }
        self.after_rows_changed();
    }

    /// Height of the header row, in canvas pixels.
    ///
    /// A press above this is a header click (which re-sorts) rather than a row
    /// click, so a caller can tell the two apart without duplicating the
    /// constant.
    pub fn header_height(&self) -> f64 {
        HEADER_HEIGHT
    }

    /// The row index under `(x, y)`, or `None` for the header/blank space.
    pub fn row_at(&self, _x: f64, y: f64) -> Option<usize> {
        // `y` is a viewport coordinate; the rows are laid out in content ones.
        row_index_at(y + self.offset.get(), self.rows.borrow().len())
    }

    pub fn select_row(&self, row: usize) {
        let len = self.rows.borrow().len();
        if row < len {
            self.selected.set(Some(row));
            self.canvas.queue_redraw();
        }
    }

    /// The text in `column` of the selected row.
    pub fn selected_path(&self, column: usize) -> Option<String> {
        let row = self.selected.get()?;
        let rows = self.rows.borrow();
        rows.get(row)?.get(column).map(CellValue::render)
    }

    /// `column`'s text for every row, in the order the rows are shown.
    ///
    /// The list sorts its own rows when a header is clicked, so a caller that
    /// needs to act on a row index (the right-click menu target, say) has to
    /// read the order back rather than assume the one it passed to
    /// [`TreeView::set_rows`] survived.
    pub fn displayed_column(&self, column: usize) -> Vec<String> {
        self.rows
            .borrow()
            .iter()
            .map(|row| row.get(column).map(CellValue::render).unwrap_or_default())
            .collect()
    }

    /// The sort a header click chose, or `None` while the caller's order holds.
    pub fn sort_state(&self) -> Option<(usize, bool)> {
        self.sort.get()
    }

    /// The text in `column` of `row`, in the order currently shown.
    pub fn cell_text(&self, row: usize, column: usize) -> Option<String> {
        self.rows
            .borrow()
            .get(row)?
            .get(column)
            .map(CellValue::render)
    }

    /// Call `f` with the row index when a row is double-clicked.
    pub fn connect_row_activated(&self, f: Box<dyn FnMut(usize)>) {
        self.activate.borrow_mut().push(f);
    }

    pub fn connect_button_press_event<F>(&self, mut f: F) -> Result<(), Error>
    where
        F: FnMut(u32, f64, f64, *mut c_void) + 'static,
    {
        self.press.borrow_mut().push(Box::new(move |b, x, y, ev| {
            f(b, x, y, ev)
        }));
        Ok(())
    }

    /// The container the list lives in.
    ///
    /// The list scrolls **itself**: the canvas is the viewport, draws only the
    /// band that is on screen and paints its own scrollbar, so the drawing area
    /// stays a screenful tall however many rows it holds. A backend whose
    /// scrolled window still produces a scroll value (macOS's `NSScrollView`
    /// does) drives the offset through `on_scroll`; where it does not, the
    /// wheel and the thumb drag do.
    pub fn into_scrolled(self) -> Result<ScrolledWindow, Error> {
        let sw = new_scrolled()?;
        sw.set_vexpand(true);
        {
            let offset = self.offset.clone();
            let viewport = self.viewport.clone();
            let rows = self.rows.clone();
            let canvas = self.canvas.clone();
            sw.on_scroll(Box::new(move |vertical, value| {
                if !vertical {
                    return;
                }
                let content = content_height(rows.borrow().len());
                let next = clamp_offset(value, content, viewport.get());
                if (next - offset.get()).abs() > 0.01 {
                    offset.set(next);
                    canvas.queue_redraw();
                }
            }));
        }
        sw.set_child(&self.canvas);
        Ok(sw)
    }

    /// Make the list a drag source for a file path.
    ///
    /// The argument shapes are the old `GtkTreeView` ones, so a client written
    /// against that surface compiles unchanged: `payload` yields the URI list
    /// plus the plain text, `icon` yields a theme name plus a drawn fallback.
    /// `payload` is asked at drop time, so it can read whichever row is selected
    /// when the drag ends its journey.
    ///
    /// The canvas is the widget that receives the press, so it is the widget
    /// that has to own the drag (see `install_uri_drag_source`). A backend
    /// without a drag source reports that, and the caller shows the reason,
    /// rather than starting a drag that carries nothing.
    pub fn enable_uri_drag_source<F, G>(&self, payload: F, icon: G) -> Result<(), Error>
    where
        F: FnMut() -> Option<(Vec<String>, String)> + 'static,
        G: FnMut() -> Option<(String, String)> + 'static,
    {
        install_uri_drag_source(&self.canvas, Box::new(payload), Box::new(icon))
    }

    pub fn clipboard_set_text(&self, text: &str) -> Result<(), Error> {
        clipboard_set_text(text)
    }

    pub fn raw_handle(&self) -> *mut c_void {
        let p: &*mut c_void = self.canvas.as_ref();
        *p
    }
}

/// The kinds of pictogram the icon column draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IconKind {
    Folder,
    Code,
    Script,
    Image,
    Audio,
    Video,
    Spreadsheet,
    Binary,
    Archive,
    Document,
    Pdf,
}

/// Which pictogram a themed icon name stands for.
///
/// The names are the freedesktop ones the search result carries; anything
/// unrecognised is a plain document rather than nothing at all.
fn icon_kind(name: &str) -> IconKind {
    match name {
        "folder" => IconKind::Folder,
        "text-x-script" => IconKind::Script,
        "image-x-generic" => IconKind::Image,
        "audio-x-generic" => IconKind::Audio,
        "video-x-generic" => IconKind::Video,
        "x-office-spreadsheet" => IconKind::Spreadsheet,
        "application-x-executable" | "application-x-object" => IconKind::Binary,
        "package-x-generic" => IconKind::Archive,
        "application-pdf" => IconKind::Pdf,
        "text-x-rust"
        | "text-x-python"
        | "text-x-javascript"
        | "text-x-typescript"
        | "application-json"
        | "text-x-csrc"
        | "text-x-c++src"
        | "text-x-go"
        | "text-x-java"
        | "text-x-ruby"
        | "text-html"
        | "text-css" => IconKind::Code,
        _ => IconKind::Document,
    }
}

/// Paint one icon cell as a pictogram in a `size`-wide box whose top left is
/// `(x, y)`. Returns `false` for a cell that is not an icon, so the caller can
/// draw whatever text it holds.
///
/// Only `fill_rect` and `stroke_rect` are used: the canvas has no paths, and
/// both desktops implement these two, so GTK and AppKit paint the same
/// picture. A font glyph is deliberately not used — a symbol the font lacks
/// comes out as a blank box, and one letter per file kind is not an icon.
fn paint_icon(ctx: &mut dyn DrawContext, x: f64, y: f64, size: f64, cell: &CellValue) -> bool {
    let CellValue::Icon { name, .. } = cell else {
        return false;
    };
    let s = size.max(6.0);
    match icon_kind(name) {
        // A folder: a tab behind a wider body, the way a file manager draws it.
        IconKind::Folder => {
            let (r, g, b) = (0.86, 0.62, 0.16);
            ctx.fill_rect(x, y + s * 0.16, s * 0.44, s * 0.18, r, g, b, 1.0);
            ctx.fill_rect(x, y + s * 0.28, s, s * 0.56, r, g, b, 1.0);
        }
        // A page with two ruled lines; code files get the blue tint.
        IconKind::Code | IconKind::Document => {
            let (r, g, b) = match icon_kind(name) {
                IconKind::Code => (0.22, 0.44, 0.78),
                _ => (0.45, 0.47, 0.52),
            };
            ctx.fill_rect(x + s * 0.06, y + s * 0.08, s * 0.88, s * 0.84, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.22, y + s * 0.30, s * 0.56, s * 0.10, 1.0, 1.0, 1.0, 1.0);
            ctx.fill_rect(x + s * 0.22, y + s * 0.52, s * 0.40, s * 0.10, 1.0, 1.0, 1.0, 1.0);
        }
        // A script: a page carrying a shell prompt — a stepped chevron with a
        // cursor bar — instead of the ruled lines source code gets. Rust, so it
        // is not mistaken for the amber folder at this size.
        IconKind::Script => {
            let (r, g, b) = (0.78, 0.40, 0.14);
            ctx.fill_rect(x + s * 0.10, y + s * 0.08, s * 0.80, s * 0.84, r, g, b, 1.0);
            // The chevron, in steps: the canvas has no paths.
            ctx.fill_rect(x + s * 0.24, y + s * 0.30, s * 0.12, s * 0.10, 1.0, 1.0, 1.0, 1.0);
            ctx.fill_rect(x + s * 0.34, y + s * 0.40, s * 0.12, s * 0.10, 1.0, 1.0, 1.0, 1.0);
            ctx.fill_rect(x + s * 0.34, y + s * 0.50, s * 0.12, s * 0.10, 1.0, 1.0, 1.0, 1.0);
            ctx.fill_rect(x + s * 0.24, y + s * 0.60, s * 0.12, s * 0.10, 1.0, 1.0, 1.0, 1.0);
            // The cursor a prompt ends with.
            ctx.fill_rect(x + s * 0.54, y + s * 0.62, s * 0.26, s * 0.08, 1.0, 1.0, 1.0, 1.0);
        }
        // A photo: a thick green frame around a white sky with a sun in it and
        // a mountain range rising from the frame's bottom edge. The mark this
        // replaces — a thin outline with a bar across the lower half and a small
        // square at the top right — read as a computer (screen, stand, badge).
        IconKind::Image => {
            let (r, g, b) = (0.20, 0.58, 0.36);
            // The frame, then the sky it surrounds.
            ctx.fill_rect(x + s * 0.04, y + s * 0.12, s * 0.92, s * 0.76, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.12, y + s * 0.21, s * 0.76, s * 0.58, 1.0, 1.0, 1.0, 1.0);
            // The sun, high on the right where the mountain cannot reach it.
            ctx.fill_rect(x + s * 0.62, y + s * 0.26, s * 0.16, s * 0.16, r, g, b, 1.0);
            // The mountain: bars that widen towards the base, since the canvas
            // has no paths. The widest sits exactly on the sky's bottom edge.
            for (i, w) in [0.14, 0.30, 0.46, 0.62].iter().enumerate() {
                let w = s * w;
                ctx.fill_rect(
                    x + s * 0.46 - w / 2.0,
                    y + s * (0.31 + i as f64 * 0.12),
                    w,
                    s * 0.12,
                    r,
                    g,
                    b,
                    1.0,
                );
            }
        }
        // A gear: a slotted square with one tooth per side, which is what a
        // program or an object file gets. Deliberately not a page — a page in
        // this column already means source code or a plain document.
        IconKind::Binary => {
            let (r, g, b) = (0.26, 0.31, 0.40);
            ctx.fill_rect(x + s * 0.18, y + s * 0.18, s * 0.64, s * 0.64, r, g, b, 1.0);
            // The hole in the middle, and a tooth on each edge.
            ctx.fill_rect(x + s * 0.38, y + s * 0.38, s * 0.24, s * 0.24, 1.0, 1.0, 1.0, 1.0);
            ctx.fill_rect(x + s * 0.06, y + s * 0.42, s * 0.12, s * 0.16, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.82, y + s * 0.42, s * 0.12, s * 0.16, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.42, y + s * 0.06, s * 0.16, s * 0.12, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.42, y + s * 0.82, s * 0.16, s * 0.12, r, g, b, 1.0);
        }
        // An archive: a carton — a lid wider than the body under it — with the
        // zip's cut down the middle of the lid.
        //
        // Drawn from `icon_sheet` renders rather than guessed: at 11 px the
        // original (one box with one band through the middle) read as a
        // struck-through square; three fine track segments with alternating
        // teeth vanished into an indistinct mark; a centred slider over a
        // track read as the letter T, and a split track as the letter H. The
        // silhouette of a box is what survives, so the zip is now a single
        // white cut in the lid.
        IconKind::Archive => {
            let (r, g, b) = (0.52, 0.36, 0.70);
            ctx.fill_rect(x + s * 0.06, y + s * 0.18, s * 0.88, s * 0.16, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.14, y + s * 0.34, s * 0.72, s * 0.52, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.46, y + s * 0.18, s * 0.08, s * 0.16, 1.0, 1.0, 1.0, 1.0);
        }
        // A red page, so a PDF is told apart from a text file at this size.
        IconKind::Pdf => {
            ctx.fill_rect(x + s * 0.06, y + s * 0.08, s * 0.88, s * 0.84, 0.72, 0.18, 0.18, 1.0);
            ctx.fill_rect(x + s * 0.22, y + s * 0.30, s * 0.56, s * 0.10, 1.0, 1.0, 1.0, 1.0);
            ctx.fill_rect(x + s * 0.22, y + s * 0.52, s * 0.40, s * 0.10, 1.0, 1.0, 1.0, 1.0);
        }
        // A note: head, stem and flag. Pink, which no other kind uses.
        IconKind::Audio => {
            let (r, g, b) = (0.78, 0.26, 0.55);
            ctx.fill_rect(x + s * 0.16, y + s * 0.54, s * 0.34, s * 0.26, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.46, y + s * 0.14, s * 0.10, s * 0.46, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.46, y + s * 0.14, s * 0.28, s * 0.12, r, g, b, 1.0);
        }
        // A play mark in a frame. The canvas has no paths, so the triangle is a
        // stack of bars that widen towards the middle.
        IconKind::Video => {
            let (r, g, b) = (0.12, 0.45, 0.66);
            ctx.stroke_rect(x + s * 0.04, y + s * 0.16, s * 0.92, s * 0.68, r, g, b, 1.0, 1.0);
            for (i, w) in [0.14, 0.26, 0.38, 0.26, 0.14].iter().enumerate() {
                ctx.fill_rect(
                    x + s * 0.30,
                    y + s * (0.24 + i as f64 * 0.12),
                    s * w,
                    s * 0.12,
                    r,
                    g,
                    b,
                    1.0,
                );
            }
        }
        // A ruled sheet: a page with a grid on it.
        IconKind::Spreadsheet => {
            let (r, g, b) = (0.28, 0.42, 0.14);
            ctx.fill_rect(x + s * 0.08, y + s * 0.10, s * 0.84, s * 0.80, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.08, y + s * 0.38, s * 0.84, s * 0.06, 1.0, 1.0, 1.0, 1.0);
            ctx.fill_rect(x + s * 0.08, y + s * 0.60, s * 0.84, s * 0.06, 1.0, 1.0, 1.0, 1.0);
            ctx.fill_rect(x + s * 0.46, y + s * 0.10, s * 0.06, s * 0.80, 1.0, 1.0, 1.0, 1.0);
        }
    }
    true
}

/// Paint the list's visible band: header strip, the rows on screen, and the
/// list's own scrollbar.
///
/// `offset` and `viewport` are in content coordinates: row `i` sits at
/// `HEADER_HEIGHT + i * ROW_HEIGHT`, and only the rows inside
/// `[offset, offset + viewport)` are emitted, shifted by `-offset` so they land
/// in the canvas. That is what makes a 600 000-row list cost the same per frame
/// as a 30-row one, and why the canvas can stay a screenful tall.
///
/// Separate from the canvas callback so the geometry can be driven offscreen
/// (the tests below paint into a `RecordingDrawContext`): the callback only
/// supplies the width and the shared state. A caller that passes an infinite
/// viewport (the offscreen probe does) gets every row, which is how the ink
/// per row stays measurable.
fn paint_list(
    ctx: &mut dyn DrawContext,
    w: f64,
    columns: &[(String, bool, bool)],
    rows: &[Vec<CellValue>],
    selected: Option<usize>,
    sort: Option<(usize, bool)>,
    offset: f64,
    viewport: f64,
) {
    ctx.clear(1.0, 1.0, 1.0, 1.0);

    let column_x = column_starts(w);
    let content = content_height(rows.len());
    let band = visible_band(rows.len(), offset, viewport);
    let shift = offset.max(0.0);

    // Header: each title over its own column, centred in the band, with the
    // sort direction on the column the list is sorted by. It scrolls with the
    // rows, as it did when the scrolled window moved the whole canvas.
    let header_top = -shift;
    ctx.fill_rect(0.0, header_top, w, HEADER_HEIGHT, 0.93, 0.93, 0.95, 1.0);
    let title_top = centred_text_top(ink_height(ctx, HEADER_SIZE, 1), HEADER_HEIGHT);
    for (i, (title, _icon, _sortable)) in columns.iter().enumerate() {
        let x = column_x.get(i).copied().unwrap_or(PAD_X);
        ctx.draw_text_styled(
            x,
            header_top + title_top,
            &sorted_title(title, i, sort),
            FONT,
            HEADER_SIZE,
            0.15,
            0.15,
            0.18,
            1.0,
            0,
            1,
        );
    }

    // Rows. Both offsets are measured from the text's top edge, which is what
    // the canvas takes — passing a baseline put the row text below its band.
    let body_top = centred_text_top(ink_height(ctx, BODY_SIZE, 0), ROW_HEIGHT);
    let icon_top = centred_text_top(ink_height(ctx, ICON_SIZE, 0), ROW_HEIGHT);
    for i in band.0..band.1 {
        let row = &rows[i];
        let y = HEADER_HEIGHT + i as f64 * ROW_HEIGHT - shift;
        if selected == Some(i) {
            ctx.fill_rect(0.0, y, w, ROW_HEIGHT, 0.22, 0.45, 0.85, 0.18);
        }
        if let Some(cell) = row.first() {
            // A pictogram, not the cell's letter: the letter was a font-glyph
            // stand-in, and it reads as "the icons are gone".
            if !paint_icon(ctx, column_x[0], y + icon_top, ICON_SIZE, cell) {
                ctx.draw_text(
                    column_x[0],
                    y + icon_top,
                    &cell.render(),
                    FONT,
                    ICON_SIZE,
                    0.2,
                    0.2,
                    0.2,
                    1.0,
                );
            }
        }
        for (column, cell) in row.iter().skip(1).take(2).enumerate() {
            ctx.draw_text(
                column_x[column + 1],
                y + body_top,
                &cell.render(),
                FONT,
                BODY_SIZE,
                0.1,
                0.1,
                0.1,
                1.0,
            );
        }
    }

    // The list's own scrollbar. The canvas is the viewport, so the scrolled
    // window has no range to draw one from; this is the range, and the thumb is
    // the viewport's share of the content, reaching the bottom when the offset
    // reaches the end.
    if viewport.is_finite() && viewport > 0.0 && content > viewport {
        let x = w - SCROLLBAR_WIDTH;
        ctx.fill_rect(x, 0.0, SCROLLBAR_WIDTH, viewport, 0.95, 0.95, 0.97, 1.0);
        let (_track, thumb_top, thumb_height) =
            reflow::scroll_thumb(content, viewport, shift);
        ctx.fill_rect(
            x + 2.0,
            thumb_top,
            SCROLLBAR_WIDTH - 4.0,
            thumb_height,
            0.60,
            0.62,
            0.68,
            1.0,
        );
    }
}

/// The height of `rows` rows, header included: the list's content height.
fn content_height(rows: usize) -> f64 {
    HEADER_HEIGHT + rows as f64 * ROW_HEIGHT
}

/// `offset` clamped so the viewport stays inside the content.
fn clamp_offset(offset: f64, content: f64, viewport: f64) -> f64 {
    if !offset.is_finite() {
        return 0.0;
    }
    offset.clamp(0.0, (content - viewport.max(0.0)).max(0.0))
}

/// The half-open range of rows the viewport `[offset, offset + viewport)` shows.
///
/// The same arithmetic `reflow::ListGeometry` publishes as `first_row` and
/// `visible_rows`, so the band the painter emits and the geometry a test asserts
/// cannot disagree. An infinite viewport means "all of them" (the offscreen
/// probe), and an empty viewport means none.
fn visible_band(rows: usize, offset: f64, viewport: f64) -> (usize, usize) {
    if rows == 0 {
        return (0, 0);
    }
    if !viewport.is_finite() {
        return (0, rows);
    }
    if viewport <= 0.0 {
        return (0, 0);
    }
    let top = offset.max(0.0);
    let first = if top <= HEADER_HEIGHT {
        0
    } else {
        (((top - HEADER_HEIGHT) / ROW_HEIGHT).floor() as usize).min(rows)
    };
    let end = top + viewport;
    let last = if end <= HEADER_HEIGHT {
        0
    } else {
        (((end - HEADER_HEIGHT) / ROW_HEIGHT).ceil() as usize).min(rows)
    };
    (first.min(last), last)
}

/// Whether a press at `x` is on the list's own scrollbar — only when there is
/// one, i.e. when the rows are taller than the viewport.
fn scrollbar_hit(w: f64, x: f64, content: f64, viewport: f64) -> bool {
    content > viewport && viewport > 0.0 && x >= w - SCROLLBAR_WIDTH
}

/// The offset that centres the thumb on `y`: a click in the strip or a drag.
fn offset_from_thumb(y: f64, content: f64, viewport: f64) -> f64 {
    if content <= viewport || viewport <= 0.0 {
        return 0.0;
    }
    let (_track, _top, thumb) = reflow::scroll_thumb(content, viewport, 0.0);
    let travel = viewport - thumb;
    if travel <= 0.0 {
        return 0.0;
    }
    let frac = ((y - thumb / 2.0) / travel).clamp(0.0, 1.0);
    frac * (content - viewport)
}

/// Install the wheel handler where the backend has a canvas scroll signal.
///
/// GTK (3), zork, wasm and nwg deliver the wheel as a `Gesture::Scroll`; macOS
/// and GTK4 have no canvas scroll hook in the portable layer, so the method is
/// `cfg`'d out there (exactly like `Canvas::on_double_click`). On those two the
/// list is scrolled by its thumb and by `ScrolledWindow::on_scroll` where that
/// backend produces a value.
fn install_wheel(canvas: &Canvas, cb: Box<dyn FnMut(f64)>) {
    #[cfg(any(
        all(feature = "gtk", target_os = "linux", not(feature = "zork")),
        feature = "zork",
        target_arch = "wasm32",
        windows
    ))]
    {
        let mut cb = cb;
        canvas.on_scroll(Box::new(move |_dx, dy| cb(dy)));
    }
    #[cfg(not(any(
        all(feature = "gtk", target_os = "linux", not(feature = "zork")),
        feature = "zork",
        target_arch = "wasm32",
        windows
    )))]
    {
        let _ = (canvas, cb);
    }
}

/// The ink height of a line of text at `size` and `weight`.
///
/// Measured from a sample with both an ascender and a descender, so one
/// measurement per size serves every row drawn at that size.
fn ink_height(ctx: &dyn DrawContext, size: f64, weight: i32) -> f64 {
    ctx.text_extents_styled("Ag", FONT, size, 0, weight).3
}

/// The `y` that centres a line with ink height `ink` in a band `height` tall.
///
/// The canvas takes `y` as the text's **top** edge, not its baseline (each
/// backend converts for its own text API), so this is plain ink-height
/// arithmetic rather than an ascent correction. Text taller than its band
/// starts at the top edge instead of being drawn above it.
fn centred_text_top(ink: f64, height: f64) -> f64 {
    ((height - ink) / 2.0).max(0.0)
}

/// Where the three columns start, for a list `w` pixels wide.
///
/// Header titles and row cells are both placed with this, so a title always
/// sits over the column it names.
fn column_starts(w: f64) -> [f64; 3] {
    let icon_w = w * ICON_COLUMN_SHARE;
    let text_w = (w - icon_w) / 2.0;
    [PAD_X, PAD_X + icon_w, PAD_X + icon_w + text_w]
}

/// Which column a header click at `x` lands on.
///
/// Same widths [`column_starts`] lays out, so the column that sorts is the one
/// whose title is under the pointer.
fn column_at(w: f64, x: f64) -> Option<usize> {
    let icon_w = w * ICON_COLUMN_SHARE;
    let text_w = (w - icon_w) / 2.0;
    let starts = column_starts(w);
    if let Some(column) = starts
        .iter()
        .zip([icon_w, text_w, text_w])
        .position(|(start, width)| x >= *start && x < start + width)
    {
        return Some(column);
    }
    // The padding before the first column is part of that column: a click on
    // the very left edge of the icon column is a click on the icon column, not
    // a dead spot.
    if x >= 0.0 && x < starts[0] {
        return Some(0);
    }
    None
}

/// A header title with its sort direction, if the list is sorted by that
/// column.
///
/// ASCII markers for the same reason the icon glyphs are ASCII: the canvas has
/// no glyph fallback, so a triangle the font lacks would draw as an empty box.
fn sorted_title(title: &str, column: usize, sort: Option<(usize, bool)>) -> String {
    match sort {
        Some((c, ascending)) if c == column => {
            format!("{title} {}", if ascending { "^" } else { "v" })
        }
        _ => title.to_string(),
    }
}

/// Handle a click at `x` in the header band: sort by the column under it, or
/// reverse the order if it is already sorted that way. Returns `true` when the
/// sort changed.
///
/// A free function over the shared state, so the click path is the same code a
/// test can drive without a widget (the canvas has no display in a unit test).
fn apply_header_click(
    rows: &RefCell<Vec<Vec<CellValue>>>,
    sort: &Cell<Option<(usize, bool)>>,
    sortable: &[bool],
    w: f64,
    x: f64,
) -> bool {
    let Some(column) = column_at(w, x) else {
        return false;
    };
    if !sortable.get(column).copied().unwrap_or(false) {
        return false;
    }
    // Same column again reverses; any other column starts ascending.
    let next = match sort.get() {
        Some((current, true)) if current == column => (column, false),
        _ => (column, true),
    };
    sort.set(Some(next));
    sort_rows(&mut rows.borrow_mut(), Some(next));
    true
}

/// The text `column` renders for `row`, lowercased: the sort key.
///
/// The key is what the column *shows*, not what it stores: an icon cell sorts
/// by its fallback glyph, so the pictogram column groups by kind.
fn row_key(row: &[CellValue], column: usize) -> String {
    row.get(column)
        .map(CellValue::render)
        .unwrap_or_default()
        .to_lowercase()
}

/// Sort `rows` by the rendered text of `column`, ignoring case.
///
/// Stable, so rows that compare equal keep the order the daemon sent them in.
/// `None` leaves the order alone.
fn sort_rows(rows: &mut Vec<Vec<CellValue>>, sort: Option<(usize, bool)>) {
    let Some((column, ascending)) = sort else {
        return;
    };
    let mut keyed: Vec<(String, Vec<CellValue>)> = std::mem::take(rows)
        .into_iter()
        .map(|row| (row_key(&row, column), row))
        .collect();
    keyed.sort_by(|a, b| {
        if ascending {
            a.0.cmp(&b.0)
        } else {
            b.0.cmp(&a.0)
        }
    });
    *rows = keyed.into_iter().map(|(_, row)| row).collect();
}

/// Merge an unsorted `batch` into `rows`, keeping `sort`'s order, and keep at
/// most `limit` rows.
///
/// Both sides are ordered by the same key, so this is a linear merge rather
/// than the re-sort plus full clone that [`TreeView::set_rows`] does. A row that
/// sorts past the last kept one is dropped, which is what makes streaming a
/// 600 k-hit reply cheap: the batch is sorted once, the merge is linear, at
/// most `limit` rows survive, and the tails never cost anything.
fn merge_rows(
    rows: &mut Vec<Vec<CellValue>>,
    batch: &[Vec<CellValue>],
    sort: Option<(usize, bool)>,
    limit: usize,
) {
    let mut incoming = batch.to_vec();
    sort_rows(&mut incoming, sort);
    let Some((column, ascending)) = sort else {
        rows.extend(incoming);
        rows.truncate(limit);
        return;
    };
    let old = std::mem::take(rows);
    let capacity = (old.len() + incoming.len()).min(limit);
    let mut merged: Vec<Vec<CellValue>> = Vec::with_capacity(capacity);
    // Move the rows rather than clone them: a 600 k-row list merged in 50 k
    // batches would otherwise clone itself twelve times over.
    let mut old_iter = old.into_iter().peekable();
    let mut new_iter = incoming.into_iter().peekable();
    while merged.len() < limit {
        // Stable: on a tie the row already in the list stays first, so a batch
        // cannot reorder rows that compare equal.
        let take_old = match (old_iter.peek(), new_iter.peek()) {
            (Some(a), Some(b)) => {
                let (ka, kb) = (row_key(a, column), row_key(b, column));
                if ascending {
                    ka <= kb
                } else {
                    ka >= kb
                }
            }
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        match if take_old {
            old_iter.next()
        } else {
            new_iter.next()
        } {
            Some(row) => merged.push(row),
            None => break,
        }
    }
    *rows = merged;
}

/// Register the double-click handler on the backend that reports a click count.
///
/// The portable canvas surface has no click-count signal for the other
/// backends, so there the handler stays out of play rather than being invented.
fn register_double_click(canvas: &Canvas, cb: Box<dyn FnMut(f64, f64)>) {
    #[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
    {
        canvas.on_double_click(cb);
    }
    #[cfg(not(all(feature = "macos", target_os = "macos", not(feature = "zork"))))]
    {
        let _ = (canvas, cb);
    }
}

fn row_index_at(y: f64, row_count: usize) -> Option<usize> {
    if y < HEADER_HEIGHT {
        return None;
    }
    let row = ((y - HEADER_HEIGHT) / ROW_HEIGHT) as usize;
    if row < row_count {
        Some(row)
    } else {
        None
    }
}

/// Install a drag source on the canvas, on the backends that have one.
#[cfg(all(
    feature = "gtk",
    target_os = "linux",
    not(feature = "zork"),
    not(feature = "gtk4-rs")
))]
fn install_uri_drag_source(
    canvas: &Canvas,
    payload: Box<dyn FnMut() -> Option<(Vec<String>, String)>>,
    icon: Box<dyn FnMut() -> Option<(String, String)>>,
) -> Result<(), Error> {
    // `raw_handle` is a `Widget` trait method, and the trait is not otherwise
    // in scope in this module.
    use crate::core::Widget as _;
    // SAFETY: the handle is this canvas's live widget, and GTK keeps both
    // closures alive with the widget (the signal connection owns them).
    unsafe {
        let r = crate::backends_gtk_adapter::enable_uri_drag_source(
            canvas.inner.raw_handle(),
            payload,
            icon,
        );
        // The row-click handler consumes presses, and a consumed press never
        // reaches the drag source's own handler, so the canvas has to let them
        // through from now on. Only after the source is actually installed.
        if r.is_ok() {
            canvas.inner.mark_uri_drag_source();
        }
        r
    }
}

/// AppKit has no drag source on this layer yet, and a backend without one says
/// so instead of accepting a drag that carries nothing.
#[cfg(not(all(
    feature = "gtk",
    target_os = "linux",
    not(feature = "zork"),
    not(feature = "gtk4-rs")
)))]
fn install_uri_drag_source(
    _canvas: &Canvas,
    _payload: Box<dyn FnMut() -> Option<(Vec<String>, String)>>,
    _icon: Box<dyn FnMut() -> Option<(String, String)>>,
) -> Result<(), Error> {
    Err(Error::Backend(
        "dragging rows out of the window is not implemented on this backend".to_string(),
    ))
}

/// Which desktop backend this build routes widgets through.
///
/// Used by the messages for facilities a build without a desktop toolkit cannot
/// provide (clipboard, native menu), so the error says what is missing rather
/// than reading like an unimplemented stub.
pub fn backend_label() -> &'static str {
    #[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
    {
        "appkit"
    }
    #[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
    {
        "gtk"
    }
    #[cfg(not(any(
        all(feature = "macos", target_os = "macos", not(feature = "zork")),
        all(feature = "gtk", target_os = "linux", not(feature = "zork"))
    )))]
    {
        if cfg!(feature = "zork") {
            "display-free"
        } else {
            "no desktop toolkit"
        }
    }
}

/// Put `text` on the system clipboard.
///
/// Each desktop backend answers with its own pasteboard; a platform with
/// neither says so instead of pretending the copy happened.
#[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
pub fn clipboard_set_text(_text: &str) -> Result<(), Error> {
    // NEGATIVE CONTROL (temporary): the old stub.
    Err(Error::Backend(
        "clipboard is not wired up in this build".to_string(),
    ))
}

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
pub fn clipboard_set_text(text: &str) -> Result<(), Error> {
    crate::backends_gtk_adapter::clipboard_set_text(text)
}

#[cfg(not(any(
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    all(feature = "gtk", target_os = "linux", not(feature = "zork"))
)))]
pub fn clipboard_set_text(_text: &str) -> Result<(), Error> {
    // Reached only by a build with neither the GTK nor the AppKit backend, i.e.
    // no desktop toolkit to own a selection. Name what is missing instead of
    // leaving a "not implemented" marker: the two backends that can copy are
    // wired to their platform's clipboard (CLIPBOARD selection / NSPasteboard).
    Err(Error::Backend(format!(
        "no system clipboard without a desktop backend ({} build)",
        crate::compat::backend_label()
    )))
}

/// Read the text currently on the system clipboard.
///
/// The checkable half of a copy: a caller (and the probe) can ask what the
/// clipboard holds instead of trusting that the write returned `Ok`.
#[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
pub fn clipboard_get_text() -> Result<String, Error> {
    crate::backends_macos_adapter::clipboard_get_text()
}

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
pub fn clipboard_get_text() -> Result<String, Error> {
    crate::backends_gtk_adapter::clipboard_get_text()
}

#[cfg(not(any(
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    all(feature = "gtk", target_os = "linux", not(feature = "zork"))
)))]
pub fn clipboard_get_text() -> Result<String, Error> {
    Err(Error::Backend(format!(
        "no system clipboard without a desktop backend ({} build)",
        crate::compat::backend_label()
    )))
}

/// A disclosure: a header line with a child under it.
///
/// GTK3 has a real one — `GtkExpander`, with the toolkit's arrow and a header
/// that folds the section out when it is clicked — and this **uses it** rather
/// than imitating it. The other backends have no foldout widget on this layer
/// (AppKit's is an `NSButton` in a disclosing bezel, which this layer does not
/// build), so they get the same shape made of a label and a box, with the
/// state in the header text and [`Expander::set_expanded`] as the only way to
/// change it.
#[derive(Clone)]
pub struct Expander {
    /// What goes into the layout: the native expander, or the stand-in's box.
    handle: *mut c_void,
    /// Whether `handle` is a `GtkExpander` (see [`native_foldout`]).
    native: bool,
    /// The header text without its state marker (only a stand-in shows it).
    title: Rc<str>,
    /// Mirrors the state where there is no native widget to ask.
    expanded: Rc<Cell<bool>>,
    /// The stand-in's parts; `None` when the native widget is in use.
    stand_in: Option<Rc<StandIn>>,
}

/// The label-and-box disclosure used where the backend has no foldout widget.
///
/// The header is a label rather than a button: AppKit's click callbacks go
/// through a process-global registry that requires `Send`, and a widget handle
/// cannot cross a thread, so this layer has no click callback that works on
/// both desktops. The header therefore carries the state in its text, and the
/// caller toggles with [`Expander::set_expanded`].
struct StandIn {
    /// Kept for the layout: the handle in [`Expander::handle`] points into it.
    _root: WidgetBox,
    header: Label,
    body: WidgetBox,
}

impl Expander {
    /// A titled section, starting collapsed.
    ///
    /// On GTK3 this is the toolkit's own `GtkExpander`, so the header is
    /// clickable and the arrow is the real one. Elsewhere it is the stand-in
    /// described on [`StandIn`].
    ///
    /// On GTK, apply the collapsed state *after* `Window::present`: presenting
    /// runs `show_all`, which would show the body again.
    pub fn new(label: &str) -> Result<Expander, Error> {
        if let Some(handle) = native_foldout::new(label) {
            return Ok(Expander {
                handle,
                native: true,
                title: Rc::from(label),
                expanded: Rc::new(Cell::new(false)),
                stand_in: None,
            });
        }
        let root = new_vbox(2)?;
        let body = new_vbox(2)?;
        let header = new_label(&header_text(label, false))?;
        root.append(&header);
        root.append(&body);
        set_box_visible(&body, false);
        let handle = *root.as_ref();
        Ok(Expander {
            handle,
            native: false,
            title: Rc::from(label),
            expanded: Rc::new(Cell::new(false)),
            stand_in: Some(Rc::new(StandIn {
                _root: root,
                header,
                body,
            })),
        })
    }

    /// Append `child` under the header. Generic over the widget handle so the
    /// caller can pass the backend's own box type (what `App::create_box`
    /// returns) instead of this layer's wrapper.
    pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
        if self.native {
            native_foldout::add_child(self.handle, *child.as_ref());
        } else if let Some(parts) = &self.stand_in {
            parts.body.append(child);
        }
    }

    /// Fold the section out or shut.
    ///
    /// On GTK this drives the native widget; a click on its header does the
    /// same thing without coming through here, which is why
    /// [`Expander::is_expanded`] asks the widget rather than a cached flag.
    pub fn set_expanded(&self, expanded: bool) {
        self.expanded.set(expanded);
        if self.native {
            native_foldout::set_expanded(self.handle, expanded);
        }
        if let Some(parts) = &self.stand_in {
            set_box_visible(&parts.body, expanded);
            parts.header.set_text(&header_text(&self.title, expanded));
        }
    }

    /// Whether the section is expanded.
    pub fn is_expanded(&self) -> bool {
        if self.native {
            return native_foldout::is_expanded(self.handle);
        }
        self.expanded.get()
    }
}

/// The backend's own disclosure widget, where it has one.
///
/// Only GTK3 does: `GtkExpander` is what a GTK application uses, and hand-rolling
/// one there cost the click on the header (a label is not a button) and the
/// arrow, for no gain. The module is a pair so the call sites stay free of
/// `cfg` — here it is real, in the `not(...)` copy below it reports "none".
#[cfg(all(
    feature = "gtk",
    target_os = "linux",
    not(feature = "zork"),
    not(feature = "gtk4-rs")
))]
mod native_foldout {
    use std::ffi::c_void;

    pub fn new(label: &str) -> Option<*mut c_void> {
        crate::backends_gtk_adapter::create_expander(label).ok()
    }

    pub fn add_child(expander: *mut c_void, child: *mut c_void) {
        crate::backends_gtk_adapter::expander_set_child(expander, child)
    }

    pub fn set_expanded(expander: *mut c_void, expanded: bool) {
        crate::backends_gtk_adapter::expander_set_expanded(expander, expanded)
    }

    pub fn is_expanded(expander: *mut c_void) -> bool {
        crate::backends_gtk_adapter::expander_get_expanded(expander)
    }
}

#[cfg(not(all(
    feature = "gtk",
    target_os = "linux",
    not(feature = "zork"),
    not(feature = "gtk4-rs")
)))]
mod native_foldout {
    use std::ffi::c_void;

    pub fn new(_label: &str) -> Option<*mut c_void> {
        None
    }

    pub fn add_child(_expander: *mut c_void, _child: *mut c_void) {}

    pub fn set_expanded(_expander: *mut c_void, _expanded: bool) {}

    pub fn is_expanded(_expander: *mut c_void) -> bool {
        false
    }
}

/// The header line for a *stand-in* disclosure in the given state.
///
/// Stated in words, not a `[+]`/`[-]` marker: the stand-in's header is not
/// clickable, and a triangle or plus sign would promise a toggle that is not
/// there. The native `GtkExpander` needs none of this — its arrow is the
/// state, and it is clickable.
fn header_text(title: &str, expanded: bool) -> String {
    let state = if expanded { "expanded" } else { "collapsed" };
    format!("{title} ({state})")
}

/// The expander is itself a widget: it can be packed into a box like any
/// other child (its handle is the native widget's, or the stand-in's box).
impl AsRef<*mut c_void> for Expander {
    fn as_ref(&self) -> &*mut c_void {
        &self.handle
    }
}

/// A menu built from labelled closures.
///
/// Collecting items works everywhere; showing one requires a native popup
/// (`gtk_menu_popup_at_pointer` / `NSMenu popUpMenu:atLocation:inView:`), which
/// this build does not provide.
/// A menu of labelled actions for a right-click.
///
/// The actions live behind a shared cell because a shown menu runs them later:
/// GTK delivers the choice through a signal on the main loop, so the popup
/// returns before the action runs.
pub struct ContextMenu {
    labels: Vec<String>,
    actions: Rc<RefCell<Vec<Box<dyn FnMut()>>>>,
    /// The native menu the last [`ContextMenu::build`] made (0 before that, or
    /// where the backend cannot build one). Kept so a caller — the tests — can
    /// inspect the very menu `popup_at_pointer` puts on screen instead of
    /// building a look-alike.
    handle: usize,
}

impl ContextMenu {
    pub fn new() -> ContextMenu {
        ContextMenu {
            labels: Vec::new(),
            actions: Rc::new(RefCell::new(Vec::new())),
            handle: 0,
        }
    }

    pub fn append_item(&mut self, label: &str, action: Box<dyn FnMut()>) {
        self.labels.push(label.to_string());
        self.actions.borrow_mut().push(action);
    }

    pub fn len(&self) -> usize {
        self.labels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// The labels this menu offers, in the order they were appended.
    pub fn labels(&self) -> Vec<String> {
        self.labels.clone()
    }

    fn dispatcher(&self) -> Box<dyn FnMut(usize)> {
        let actions = self.actions.clone();
        Box::new(move |index: usize| {
            if let Some(action) = actions.borrow_mut().get_mut(index) {
                action();
            }
        })
    }

    /// Build the native menu without showing it.
    ///
    /// This is the popup's first step, exposed so a test can inspect exactly
    /// the menu the popup would put on screen and fire its items through the
    /// same plumbing a click uses. A handle of `0` means the backend cannot
    /// build one.
    pub fn build(&mut self) -> Result<usize, Error> {
        let handle = context_menu_build(&self.labels, self.dispatcher())?;
        self.handle = handle;
        Ok(handle)
    }

    /// The native handle of the menu last built (and, for a popup, shown).
    /// `0` when nothing has been built.
    pub fn handle(&self) -> usize {
        self.handle
    }

    /// The items of a built menu and whether each is enabled.
    pub fn items_of(handle: usize) -> Vec<(String, bool)> {
        if handle == 0 {
            return Vec::new();
        }
        context_menu_items(handle)
    }

    /// Fire item `index` of a built menu, the way choosing it does.
    pub fn fire(handle: usize, index: usize) -> bool {
        if handle == 0 {
            return false;
        }
        context_menu_fire(handle, index)
    }

    pub fn popup_at_pointer(
        &mut self,
        widget: *mut c_void,
        event: *mut c_void,
    ) -> Result<(), Error> {
        let _ = (widget, event);
        // NEGATIVE CONTROL (temporary): the old stub.
        Err(Error::Backend(
            "context menus are not available in this build".to_string(),
        ))
    }

    /// Run an item's action by label (used by tests and by the popup).
    pub fn activate(&mut self, label: &str) -> bool {
        match self.labels.iter().position(|l| l == label) {
            Some(index) => {
                if let Some(action) = self.actions.borrow_mut().get_mut(index) {
                    action();
                }
                true
            }
            None => false,
        }
    }
}

/// Build the native menu for `labels`, routing a choice to `on_choose`.
#[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
fn context_menu_build(
    labels: &[String],
    on_choose: Box<dyn FnMut(usize)>,
) -> Result<usize, Error> {
    crate::backends_macos_adapter::context_menu_build(labels, on_choose)
}

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
fn context_menu_build(
    labels: &[String],
    on_choose: Box<dyn FnMut(usize)>,
) -> Result<usize, Error> {
    crate::backends_gtk_adapter::context_menu_build(labels, on_choose)
}

#[cfg(not(any(
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    all(feature = "gtk", target_os = "linux", not(feature = "zork"))
)))]
fn context_menu_build(
    _labels: &[String],
    _on_choose: Box<dyn FnMut(usize)>,
) -> Result<usize, Error> {
    // As with the clipboard: only a build with neither GTK nor AppKit gets
    // here. Say why rather than "not implemented" -- both desktop backends
    // build a real native menu (GtkMenu / NSMenu).
    Err(Error::Backend(format!(
        "no native menu without a desktop backend ({} build)",
        crate::compat::backend_label()
    )))
}

#[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
fn context_menu_items(handle: usize) -> Vec<(String, bool)> {
    crate::backends_macos_adapter::context_menu_items(handle)
}

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
fn context_menu_items(handle: usize) -> Vec<(String, bool)> {
    crate::backends_gtk_adapter::context_menu_items(handle)
}

#[cfg(not(any(
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    all(feature = "gtk", target_os = "linux", not(feature = "zork"))
)))]
fn context_menu_items(_handle: usize) -> Vec<(String, bool)> {
    Vec::new()
}

#[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
fn context_menu_fire(handle: usize, index: usize) -> bool {
    crate::backends_macos_adapter::context_menu_fire(handle, index)
}

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
fn context_menu_fire(handle: usize, index: usize) -> bool {
    crate::backends_gtk_adapter::context_menu_fire(handle, index)
}

#[cfg(not(any(
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    all(feature = "gtk", target_os = "linux", not(feature = "zork"))
)))]
fn context_menu_fire(_handle: usize, _index: usize) -> bool {
    false
}

#[cfg(all(feature = "macos", target_os = "macos", not(feature = "zork")))]
fn context_menu_show(
    handle: usize,
    widget: *mut c_void,
    event: *mut c_void,
) -> Result<(), Error> {
    crate::backends_macos_adapter::context_menu_show(handle, widget, event)
}

#[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
fn context_menu_show(
    handle: usize,
    widget: *mut c_void,
    event: *mut c_void,
) -> Result<(), Error> {
    crate::backends_gtk_adapter::context_menu_show(handle, widget, event)
}

#[cfg(not(any(
    all(feature = "macos", target_os = "macos", not(feature = "zork")),
    all(feature = "gtk", target_os = "linux", not(feature = "zork"))
)))]
fn context_menu_show(
    _handle: usize,
    _widget: *mut c_void,
    _event: *mut c_void,
) -> Result<(), Error> {
    // As with the clipboard: only a build with neither GTK nor AppKit gets
    // here. Say why rather than "not implemented" -- both desktop backends
    // build a real native menu (GtkMenu / NSMenu).
    Err(Error::Backend(format!(
        "no native menu without a desktop backend ({} build)",
        crate::compat::backend_label()
    )))
}

impl Default for ContextMenu {
    fn default() -> Self {
        Self::new()
    }
}

// --------------------------------------------------------------------------
// Constructors on `App`.
//
// The widgets above are built on the portable layer rather than on a backend
// adapter, so the usual `App::create_*` shape is restored here: a client can
// keep calling them uniformly instead of branching per backend.
// --------------------------------------------------------------------------

impl App {
    /// A three-column results list: `(title, is_icon, sortable)` per column.
    ///
    /// A `sortable` column sorts when its header is clicked (see
    /// [`TreeView::displayed_column`] for reading the order back).
    pub fn create_treeview(
        &self,
        columns: &[(&str, bool, bool)],
    ) -> Result<TreeView, Error> {
        TreeView::new(columns)
    }

    /// A native disclosure section; see [`Expander`].
    pub fn create_expander(&self, label: &str) -> Result<Expander, Error> {
        Expander::new(label)
    }

    /// A menu of labelled actions for a right-click; see [`ContextMenu`].
    ///
    /// Always succeeds: collecting items needs no toolkit, and a menu that
    /// cannot be shown reports that from [`ContextMenu::popup_at_pointer`],
    /// which is where the caller can surface it.
    pub fn new_context_menu(&self) -> Result<ContextMenu, Error> {
        Ok(ContextMenu::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_hit_testing_skips_header_and_blank_space() {
        assert_eq!(row_index_at(0.0, 5), None, "header");
        assert_eq!(row_index_at(HEADER_HEIGHT - 1.0, 5), None);
        assert_eq!(row_index_at(HEADER_HEIGHT + 1.0, 5), Some(0));
        assert_eq!(row_index_at(HEADER_HEIGHT + ROW_HEIGHT + 1.0, 5), Some(1));
        assert_eq!(
            row_index_at(HEADER_HEIGHT + 5.0 * ROW_HEIGHT, 5),
            None,
            "past the last row"
        );
    }

    #[test]
    fn text_is_centred_inside_its_band() {
        // 14px of ink in the 20px row band and the 22px header band.
        assert_eq!(3.0, centred_text_top(14.0, ROW_HEIGHT));
        assert_eq!(4.0, centred_text_top(14.0, HEADER_HEIGHT));
        // Ink taller than its band is pinned to the top edge, never drawn above
        // it (which is how the titles used to hang below the header band).
        assert_eq!(0.0, centred_text_top(30.0, ROW_HEIGHT));
    }

    #[test]
    fn columns_start_inside_the_list_and_do_not_overlap() {
        let x = column_starts(1000.0);
        assert_eq!(PAD_X, x[0]);
        let icon_w = x[1] - x[0];
        let text_w = x[2] - x[1];
        // The icon column is the narrow one; name and path split the rest.
        assert!(icon_w < text_w);
        assert!((text_w - (1000.0 - icon_w) / 2.0).abs() < 1e-9);
        // Three columns, which is what the rows draw (icon + two text cells).
        assert_eq!(3, x.len());
    }

    /// The reported Linux symptom: titles hanging below the grey header band,
    /// rows sitting low in their band, and a title that does not sit over the
    /// column it names. Painted offscreen, so no display is needed.
    #[test]
    fn titles_and_cells_share_columns_and_stay_in_their_bands() {
        use crate::backends::headless::{DrawOp, RecordingDrawContext};

        let columns = vec![
            (String::new(), true, true),
            ("Name".to_string(), false, true),
            ("Path".to_string(), false, true),
        ];
        let rows = vec![vec![
            CellValue::Text("R".into()),
            CellValue::Text("main.rs".into()),
            CellValue::Text("/a/main.rs".into()),
        ]];
        let width = 1000.0;
        let mut dc = RecordingDrawContext::new();
        paint_list(
            &mut dc,
            width,
            &columns,
            &rows,
            None,
            None,
            0.0,
            f64::INFINITY,
        );

        let find = |needle: &str| -> (f64, f64) {
            dc.ops
                .iter()
                .find_map(|op| match op {
                    DrawOp::Text { x, y, text, .. } if text == needle => Some((*x, *y)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no text op for {needle:?}"))
        };

        // Every title sits over the column it names, and every cell under it.
        let x = column_starts(width);
        for (text, column) in [
            ("R", 0),
            ("Name", 1),
            ("main.rs", 1),
            ("Path", 2),
            ("/a/main.rs", 2),
        ] {
            assert_eq!(x[column], find(text).0, "{text:?} is not in column {column}");
        }

        // And each line stays inside its band rather than hanging below it.
        // 16.0 is the ink height this context reports for any string.
        let ink = 16.0;
        for (text, band_top, band_height) in [
            ("Name", 0.0, HEADER_HEIGHT),
            ("main.rs", HEADER_HEIGHT, ROW_HEIGHT),
        ] {
            let top = find(text).1;
            assert!(top >= band_top, "{text:?} starts above its band");
            assert!(
                top + ink <= band_top + band_height,
                "{text:?} pokes below its band: {top} + {ink} > {}",
                band_top + band_height
            );
        }
    }

    /// The reported regression: the icon column drew one letter per file kind,
    /// so the icons were gone. It must paint a pictogram instead.
    #[test]
    fn icons_paint_pictures_not_their_letters() {
        use crate::backends::headless::{DrawOp, RecordingDrawContext};

        let cell = CellValue::Icon {
            name: "folder".into(),
            fallback: "d".into(),
        };
        let mut dc = RecordingDrawContext::new();
        assert!(paint_icon(&mut dc, 4.0, 24.0, 11.0, &cell));
        // A picture: more than one filled rectangle...
        let fills = dc
            .ops
            .iter()
            .filter(|op| matches!(op, DrawOp::FillRect { .. }))
            .count();
        assert!(fills >= 2, "the folder drew {fills} rectangle(s): {:?}", dc.ops);
        // ...and no text at all, so the fallback letter is not what is shown.
        assert!(
            !dc
                .ops
                .iter()
                .any(|op| matches!(op, DrawOp::Text { .. })),
            "the icon still draws text: {:?}",
            dc.ops
        );
        // Everything stays inside the 11pt box it was given.
        for op in &dc.ops {
            let (x, y, w, h) = match *op {
                DrawOp::FillRect { x, y, w, h, .. }
                | DrawOp::StrokeRect { x, y, w, h, .. } => (x, y, w, h),
                _ => continue,
            };
            assert!(x >= 4.0 && y >= 24.0, "{op:?} starts outside the box");
            assert!(x + w <= 4.0 + 11.0 + 1e-9, "{op:?} runs past the box");
            assert!(y + h <= 24.0 + 11.0 + 1e-9, "{op:?} runs past the box");
        }
    }

    #[test]
    fn different_kinds_draw_different_pictures() {
        use crate::backends::headless::{DrawOp, RecordingDrawContext};

        let first_colour = |name: &str| {
            let cell = CellValue::Icon {
                name: name.into(),
                fallback: "?".into(),
            };
            let mut dc = RecordingDrawContext::new();
            assert!(paint_icon(&mut dc, 0.0, 0.0, 11.0, &cell));
            dc.ops
                .iter()
                .find_map(|op| match op {
                    DrawOp::FillRect { rgba, .. } => Some(*rgba),
                    DrawOp::StrokeRect { rgba, .. } => Some(*rgba),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name} drew nothing"))
        };

        let names = [
            "folder",
            "text-x-rust",
            "text-x-script",
            "image-x-generic",
            "audio-x-generic",
            "video-x-generic",
            "x-office-spreadsheet",
            "application-x-executable",
            "package-x-generic",
            "text-x-generic",
            "application-pdf",
        ];
        let colours: Vec<_> = names.iter().map(|n| first_colour(n)).collect();
        for (i, a) in colours.iter().enumerate() {
            for (j, b) in colours.iter().enumerate().skip(i + 1) {
                assert_ne!(a, b, "{} and {} look the same", names[i], names[j]);
            }
        }
        // Object files share the program pictogram on purpose — one kind, two
        // theme names, so a real icon theme can still tell them apart.
        assert_eq!(
            first_colour("application-x-executable"),
            first_colour("application-x-object"),
            "the two binary names must draw the same picture"
        );
        // A cell that is not an icon is not painted as one.
        let mut dc = RecordingDrawContext::new();
        assert!(!paint_icon(&mut dc, 0.0, 0.0, 11.0, &CellValue::Text("x".into())));
        assert!(dc.ops.is_empty());
    }

    /// And the list itself uses it: nothing is drawn as text in the icon
    /// column any more.
    #[test]
    fn the_icon_column_of_a_painted_list_holds_no_text() {
        use crate::backends::headless::{DrawOp, RecordingDrawContext};

        let columns = vec![
            (String::new(), true, false),
            ("Name".to_string(), false, true),
            ("Path".to_string(), false, true),
        ];
        let rows = vec![vec![
            CellValue::Icon {
                name: "folder".into(),
                fallback: "d".into(),
            },
            CellValue::Text("src".into()),
            CellValue::Text("/a/src".into()),
        ]];
        let width = 1000.0;
        let mut dc = RecordingDrawContext::new();
        paint_list(
            &mut dc,
            width,
            &columns,
            &rows,
            None,
            None,
            0.0,
            f64::INFINITY,
        );
        let x = column_starts(width);
        // Header titles are drawn in the header band, and the icon column's own
        // title is empty; what matters is that no *row* draws text there.
        assert!(
            !dc.ops.iter().any(|op| match op {
                DrawOp::Text { x: tx, y: ty, text, .. } =>
                    !text.is_empty() && *ty >= HEADER_HEIGHT && *tx < x[1],
                _ => false,
            }),
            "the icon column still draws text: {:?}",
            dc.ops
        );
    }

    fn text_rows(values: &[&str]) -> Vec<Vec<CellValue>> {
        values
            .iter()
            .map(|v| vec![CellValue::Text((*v).into())])
            .collect()
    }

    #[test]
    fn header_clicks_hit_the_column_under_the_pointer() {
        let w = 1000.0;
        let x = column_starts(w);
        assert_eq!(Some(0), column_at(w, x[0] + 1.0), "icon column");
        assert_eq!(Some(1), column_at(w, x[1] + 1.0), "name column");
        assert_eq!(Some(2), column_at(w, x[2] + 1.0), "path column");
        // The padding before the first column belongs to it: the list's left
        // edge sorts by the icon column instead of doing nothing.
        assert_eq!(Some(0), column_at(w, 1.0));
        assert_eq!(None, column_at(w, -1.0));
        // And past the path column (only reachable if the list is wider than
        // its columns) is outside.
        assert_eq!(None, column_at(w, x[2] + 0.6 * w));
    }

    #[test]
    fn sorting_ignores_case_and_keeps_ties_in_arrival_order() {
        let mut rows = text_rows(&["c", "A", "b", "B"]);
        sort_rows(&mut rows, Some((0, true)));
        assert_eq!(
            vec!["A", "b", "B", "c"],
            rows.iter().map(|r| r[0].render()).collect::<Vec<_>>()
        );
        // Clicking the same header again reverses it.
        sort_rows(&mut rows, Some((0, false)));
        assert_eq!(
            vec!["c", "b", "B", "A"],
            rows.iter().map(|r| r[0].render()).collect::<Vec<_>>()
        );
        // No sort is the daemon's order, untouched.
        sort_rows(&mut rows, None);
        assert_eq!(
            vec!["c", "b", "B", "A"],
            rows.iter().map(|r| r[0].render()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn sorting_by_a_missing_column_does_not_panic() {
        // A short row (a count-only line has empty cells) sorts as empty text.
        let mut rows = vec![vec![CellValue::Text("b".into())], vec![]];
        sort_rows(&mut rows, Some((0, true)));
        assert!(rows[0].is_empty(), "the empty row sorts first");
    }

    /// The names (column 1) of the rows, in list order.
    fn names(rows: &RefCell<Vec<Vec<CellValue>>>) -> Vec<String> {
        rows.borrow().iter().map(|r| r[1].render()).collect()
    }

    /// The click path itself: which column sorts, that the rows really move,
    /// and that a second click reverses it.
    #[test]
    fn header_clicks_toggle_the_sort_and_respect_sortable() {
        // Three real columns per row: icon, name, path.
        let rows: RefCell<Vec<Vec<CellValue>>> = RefCell::new(
            ["c", "a", "b"]
                .iter()
                .map(|n| {
                    vec![
                        CellValue::Text("-".into()),
                        CellValue::Text((*n).into()),
                        CellValue::Text(format!("/x/{n}")),
                    ]
                })
                .collect(),
        );
        let sort: Cell<Option<(usize, bool)>> = Cell::new(None);
        let sortable = [false, true, true];
        let w = 1000.0;
        let x = column_starts(w);

        // The icon column is not sortable, and a click left of the columns is
        // not a header at all.
        assert!(!apply_header_click(&rows, &sort, &sortable, w, x[0] + 1.0));
        assert!(!apply_header_click(&rows, &sort, &sortable, w, 1.0));
        assert_eq!(None, sort.get());
        assert_eq!(vec!["c", "a", "b"], names(&rows));

        // First click: ascending by Name, and the rows move.
        assert!(apply_header_click(&rows, &sort, &sortable, w, x[1] + 1.0));
        assert_eq!(Some((1, true)), sort.get());
        assert_eq!(vec!["a", "b", "c"], names(&rows));

        // Second click on the same header: descending.
        assert!(apply_header_click(&rows, &sort, &sortable, w, x[1] + 1.0));
        assert_eq!(Some((1, false)), sort.get());
        assert_eq!(vec!["c", "b", "a"], names(&rows));

        // A different column starts ascending again — here the path column.
        assert!(apply_header_click(&rows, &sort, &sortable, w, x[2] + 1.0));
        assert_eq!(Some((2, true)), sort.get());
        assert_eq!(vec!["a", "b", "c"], names(&rows));
    }

    #[test]
    fn the_sorted_header_shows_its_direction() {
        assert_eq!("Name ^", sorted_title("Name", 1, Some((1, true))));
        assert_eq!("Name v", sorted_title("Name", 1, Some((1, false))));
        assert_eq!("Path", sorted_title("Path", 2, Some((1, true))));
        assert_eq!("Name", sorted_title("Name", 1, None));
    }

    #[test]
    fn the_header_states_whether_the_body_is_shown() {
        assert_eq!("Advanced search (collapsed)", header_text("Advanced search", false));
        assert_eq!("Advanced search (expanded)", header_text("Advanced search", true));
    }

    #[test]
    fn cell_value_renders_its_fallback_for_icons() {
        let icon = CellValue::Icon {
            name: "folder".into(),
            fallback: "D".into(),
        };
        assert_eq!(icon.render(), "D");
        assert_eq!(CellValue::Text("x".into()).render(), "x");
    }

    #[test]
    fn context_menu_collects_and_activates_items() {
        let mut menu = ContextMenu::new();
        let hits = Rc::new(Cell::new(0));
        let h = hits.clone();
        menu.append_item("Open", Box::new(move || h.set(h.get() + 1)));
        assert_eq!(menu.len(), 1);
        assert!(menu.activate("Open"));
        assert_eq!(hits.get(), 1);
        assert!(!menu.activate("Missing"));
    }

    /// A raster target for [`icon_sheet`]: coordinates scaled by `k` and written
    /// straight over the pixels, which is sound because every pictogram op is
    /// opaque.
    struct Raster {
        w: usize,
        h: usize,
        k: f64,
        px: Vec<[f64; 3]>,
    }

    impl Raster {
        fn new(w: usize, h: usize, k: f64) -> Raster {
            Raster {
                w,
                h,
                k,
                px: vec![[1.0; 3]; w * h],
            }
        }

        fn put(&mut self, x: f64, y: f64, w: f64, h: f64, c: [f64; 3]) {
            let x0 = (x * self.k).floor().max(0.0) as usize;
            let y0 = (y * self.k).floor().max(0.0) as usize;
            let x1 = (((x + w) * self.k).ceil().max(0.0) as usize).min(self.w);
            let y1 = (((y + h) * self.k).ceil().max(0.0) as usize).min(self.h);
            for row in y0..y1 {
                for col in x0..x1 {
                    self.px[row * self.w + col] = c;
                }
            }
        }
    }

    impl crate::core::DrawContext for Raster {
        fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, _a: f64) {
            self.put(x, y, w, h, [r, g, b]);
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
            _a: f64,
            lw: f64,
        ) {
            let c = [r, g, b];
            self.put(x, y, w, lw, c);
            self.put(x, y + h - lw, w, lw, c);
            self.put(x, y, lw, h, c);
            self.put(x + w - lw, y, lw, h, c);
        }

        // The icon column paints no text; these only satisfy the trait.
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
            _text: &str,
            _font: &str,
            _size: f64,
            _slant: i32,
            _weight: i32,
        ) -> (f64, f64, f64, f64) {
            (0.0, 0.0, 0.0, 0.0)
        }

        fn clear(&mut self, r: f64, g: f64, b: f64, _a: f64) {
            for p in &mut self.px {
                *p = [r, g, b];
            }
        }

        fn save(&mut self) {}
        fn restore(&mut self) {}
        fn clip(&mut self, _x: f64, _y: f64, _w: f64, _h: f64) {}
    }

    /// Every icon kind, rendered at the list's real 11 pt, into a PNG.
    ///
    /// Run it with the output path; without that variable it does nothing, so an
    /// ordinary `cargo test` writes no files:
    ///
    /// ```text
    /// ICON_SHEET=/tmp/icons.png cargo test --manifest-path vendor/rustxwidgets/Cargo.toml \
    ///     --no-default-features --features gtk,macos --lib -- icon_sheet --nocapture
    /// ```
    ///
    /// Each mark is drawn at 8× and box-filtered down — what the canvas' own
    /// antialiasing does to it — then blown back up 6× with hard pixels so the
    /// 11 px result can be looked at, with the same pixels repeated at 1:1 in a
    /// strip underneath. A test rather than an example because `paint_icon` is
    /// private to this module.
    #[test]
    fn icon_sheet() {
        let Ok(out) = std::env::var("ICON_SHEET") else {
            return;
        };
        const NAMES: &[&str] = &[
            "folder",
            "text-x-rust",
            "text-x-script",
            "text-x-generic",
            "image-x-generic",
            "audio-x-generic",
            "video-x-generic",
            "x-office-spreadsheet",
            "application-x-executable",
            "package-x-generic",
            "application-pdf",
        ];
        const SS: usize = 8; // supersampling
        let px = ICON_SIZE as usize; // 11: what a row actually draws
        let zoom = 6; // display magnification
        let cell = px * zoom + 12;
        let w = cell * NAMES.len();
        let h = cell + px + 8;
        let mut rgba = vec![255u8; w * h * 4];
        println!("icon sheet column order: {NAMES:?}");

        for (i, name) in NAMES.iter().enumerate() {
            let cell_value = CellValue::Icon {
                name: (*name).into(),
                fallback: "?".into(),
            };
            let mut big = Raster::new(px * SS, px * SS, SS as f64);
            assert!(
                paint_icon(&mut big, 0.0, 0.0, ICON_SIZE, &cell_value),
                "{name} did not paint"
            );
            // Down to the real 11 px: average each 8×8 block.
            let mut small = vec![[255.0f64; 3]; px * px];
            for y in 0..px {
                for x in 0..px {
                    let mut acc = [0.0f64; 3];
                    for sy in 0..SS {
                        for sx in 0..SS {
                            let p = big.px[(y * SS + sy) * px * SS + x * SS + sx];
                            for c in 0..3 {
                                acc[c] += p[c] / (SS * SS) as f64;
                            }
                        }
                    }
                    small[y * px + x] = acc;
                }
            }
            let rgb = |p: [f64; 3]| {
                [
                    (p[0] * 255.0).round() as u8,
                    (p[1] * 255.0).round() as u8,
                    (p[2] * 255.0).round() as u8,
                ]
            };
            for y in 0..px {
                for x in 0..px {
                    let colour = rgb(small[y * px + x]);
                    // The zoomed copy...
                    for dy in 0..zoom {
                        for dx in 0..zoom {
                            let ox = i * cell + 6 + x * zoom + dx;
                            let oy = 6 + y * zoom + dy;
                            let o = (oy * w + ox) * 4;
                            rgba[o..o + 3].copy_from_slice(&colour);
                        }
                    }
                    // ...and the same pixel at 1:1 underneath it.
                    let o = ((cell + 8 + y) * w + i * cell + 6 + x) * 4;
                    rgba[o..o + 3].copy_from_slice(&colour);
                }
            }
        }

        write_png(&out, w, h, &rgba).expect("the icon sheet could not be written");
        println!("icon sheet written to {out} ({w}x{h})");
    }

    /// A minimal PNG writer: 8-bit RGBA, no filtering, and uncompressed
    /// ("stored") deflate blocks so a debugging sheet needs no dependency.
    fn write_png(path: &str, w: usize, h: usize, rgba: &[u8]) -> std::io::Result<()> {
        fn crc32(data: &[u8]) -> u32 {
            let mut crc = 0xffff_ffffu32;
            for &byte in data {
                crc ^= byte as u32;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xedb8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        fn adler32(data: &[u8]) -> u32 {
            let (mut a, mut b) = (1u32, 0u32);
            for &byte in data {
                a = (a + byte as u32) % 65521;
                b = (b + a) % 65521;
            }
            (b << 16) | a
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let mut crc_input = Vec::with_capacity(4 + body.len());
            crc_input.extend_from_slice(kind);
            crc_input.extend_from_slice(body);
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        }

        let mut raw = Vec::with_capacity(h * (1 + w * 4));
        for y in 0..h {
            raw.push(0); // filter type: none
            raw.extend_from_slice(&rgba[y * w * 4..(y + 1) * w * 4]);
        }
        let mut zlib = vec![0x78, 0x01]; // deflate, default window, no dictionary
        let mut rest: &[u8] = &raw;
        loop {
            let n = rest.len().min(65535);
            let last = n == rest.len();
            zlib.push(if last { 1 } else { 0 });
            zlib.extend_from_slice(&(n as u16).to_le_bytes());
            zlib.extend_from_slice(&(!(n as u16)).to_le_bytes());
            zlib.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            if last {
                break;
            }
        }
        zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(w as u32).to_be_bytes());
        ihdr.extend_from_slice(&(h as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &ihdr);
        chunk(&mut png, b"IDAT", &zlib);
        chunk(&mut png, b"IEND", &[]);
        std::fs::write(path, png)
    }
}

#[cfg(test)]
mod append_rows_tests {
    use super::*;

    /// A row whose only meaningful text is `name`, in column 1: the column the
    /// Name header sorts by.
    fn named(name: &str) -> Vec<CellValue> {
        vec![
            CellValue::Text(String::new()),
            CellValue::Text(name.to_string()),
            CellValue::Text(format!("/tmp/{name}")),
        ]
    }

    fn names(rows: &[Vec<CellValue>]) -> Vec<String> {
        rows.iter()
            .map(|r| r.get(1).map(CellValue::render).unwrap_or_default())
            .collect()
    }

    #[test]
    fn unsorted_batches_merge_into_the_active_sort_order() {
        // The daemon streams hits in crawl order, so each batch arrives
        // unsorted. Appending must not disturb the column's order.
        let mut rows = vec![named("b"), named("d")];
        merge_rows(&mut rows, &[named("a"), named("c")], Some((1, true)), 100);
        assert_eq!(names(&rows), ["a", "b", "c", "d"]);
        // A second batch, also out of order, merges the same way.
        merge_rows(&mut rows, &[named("f"), named("e")], Some((1, true)), 100);
        assert_eq!(names(&rows), ["a", "b", "c", "d", "e", "f"]);
    }

    #[test]
    fn a_reverse_sort_merges_in_the_reverse_order() {
        let mut rows = vec![named("d"), named("b")];
        merge_rows(&mut rows, &[named("c"), named("a")], Some((1, false)), 100);
        assert_eq!(names(&rows), ["d", "c", "b", "a"]);
    }

    #[test]
    fn the_limit_keeps_the_sorted_prefix() {
        // 1 000 rows is what the list can draw; a batch of hits that sorts past
        // the end must cost nothing beyond the merge.
        let mut rows = vec![named("b"), named("d")];
        merge_rows(&mut rows, &[named("a"), named("c"), named("e")], Some((1, true)), 3);
        assert_eq!(names(&rows), ["a", "b", "c"]);
    }

    #[test]
    fn an_empty_batch_leaves_the_rows_alone() {
        let mut rows = vec![named("b")];
        merge_rows(&mut rows, &[], Some((1, true)), 10);
        assert_eq!(names(&rows), ["b"]);
    }

    #[test]
    fn without_a_sort_batches_are_appended_in_arrival_order() {
        // No header has been clicked: the list keeps the daemon's order, and a
        // later batch lands after the earlier one.
        let mut rows = vec![named("z")];
        merge_rows(&mut rows, &[named("a")], None, 10);
        assert_eq!(names(&rows), ["z", "a"]);
    }

    #[test]
    fn a_tie_keeps_the_rows_already_on_screen_first() {
        // Stable: rows that compare equal stay in the order they arrived, so a
        // batch cannot shuffle equal names in front of the ones on screen.
        let mut rows = vec![named("same"), named("z")];
        merge_rows(&mut rows, &[named("same")], Some((1, true)), 10);
        assert_eq!(names(&rows), ["same", "same", "z"]);
    }
}

#[cfg(test)]
mod virtual_list_tests {
    //! The list at a six-figure row count: only the band on screen is painted,
    //! and the offset reaches the last row.

    use super::*;
    use crate::backends::headless::{DrawOp, RecordingDrawContext};

    const BIG: usize = 600_000;

    fn big_rows(n: usize) -> Vec<Vec<CellValue>> {
        (0..n)
            .map(|i| {
                vec![
                    CellValue::Text(String::new()),
                    CellValue::Text(format!("file-{i:06}")),
                    CellValue::Text(format!("/tmp/file-{i:06}")),
                ]
            })
            .collect()
    }

    fn columns() -> Vec<(String, bool, bool)> {
        vec![
            ("".to_string(), true, true),
            ("Name".to_string(), false, true),
            ("Path".to_string(), false, true),
        ]
    }

    /// The row numbers whose name cell the painter emitted.
    fn painted(dc: &RecordingDrawContext) -> Vec<usize> {
        let mut rows: Vec<usize> = dc
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => text
                    .strip_prefix("file-")
                    .and_then(|n| n.parse::<usize>().ok()),
                _ => None,
            })
            .collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    #[test]
    fn the_band_is_the_rows_inside_the_viewport() {
        // 600 000 rows, a 620pt viewport: ~31 rows, from wherever the offset is.
        let (first, last) = visible_band(BIG, 0.0, 620.0);
        assert_eq!(first, 0);
        assert_eq!(last, 30, "620pt of rows is not 30 bands");

        let (first, last) = visible_band(BIG, 6_000_000.0, 620.0);
        assert_eq!(first, ((6_000_000.0 - HEADER_HEIGHT) / ROW_HEIGHT).floor() as usize);
        assert!((29..=32).contains(&(last - first)), "the band is {} rows", last - first);

        // The end of the content shows the last row and nothing past it.
        let content = content_height(BIG);
        let (first, last) = visible_band(BIG, content - 620.0, 620.0);
        assert_eq!(last, BIG, "the last row is not in the band at the end");
        assert!((29..=32).contains(&(last - first)), "the band is {} rows", last - first);

        // An empty viewport paints nothing; an infinite one paints everything
        // (the offscreen probe).
        assert_eq!(visible_band(BIG, 0.0, 0.0), (0, 0));
        assert_eq!(visible_band(BIG, 0.0, f64::INFINITY), (0, BIG));
        assert_eq!(visible_band(0, 0.0, 620.0), (0, 0));
    }

    #[test]
    fn a_six_figure_list_paints_only_the_visible_band() {
        let rows = big_rows(BIG);
        let columns = columns();

        // Top: the first ~31 rows, not the other 599 969.
        let mut dc = RecordingDrawContext::new();
        paint_list(&mut dc, 900.0, &columns, &rows, None, Some((1, true)), 0.0, 620.0);
        let top = painted(&dc);
        assert_eq!(top.first().copied(), Some(0));
        assert_eq!(top.len(), 30, "the top of the list painted {} rows", top.len());

        // Middle: a window 6 000 000pt down, still ~31 rows.
        let mut dc = RecordingDrawContext::new();
        let mid_offset = 6_000_000.0;
        paint_list(
            &mut dc,
            900.0,
            &columns,
            &rows,
            None,
            Some((1, true)),
            mid_offset,
            620.0,
        );
        let mid = painted(&dc);
        let expected_first = ((mid_offset - HEADER_HEIGHT) / ROW_HEIGHT).floor() as usize;
        assert_eq!(mid.first().copied(), Some(expected_first));
        assert_eq!(mid.len(), 32, "the middle of the list painted {} rows", mid.len());
        assert!(
            mid.last().copied().unwrap() < expected_first + 32,
            "the band ran past the viewport"
        );

        // End: the last row is painted, and it is the last one.
        let content = content_height(BIG);
        let mut dc = RecordingDrawContext::new();
        paint_list(
            &mut dc,
            900.0,
            &columns,
            &rows,
            None,
            Some((1, true)),
            content - 620.0,
            620.0,
        );
        let end = painted(&dc);
        assert_eq!(end.last().copied(), Some(BIG - 1), "the last row was not painted");
        assert_eq!(end.len(), 31);
    }

    #[test]
    fn the_scroller_travels_from_the_first_row_to_the_last() {
        let content = content_height(BIG);
        let viewport = 620.0;

        // At the top the thumb sits at the top of the track.
        let (track, top, thumb) = reflow::scroll_thumb(content, viewport, 0.0);
        assert_eq!(track, viewport);
        assert_eq!(top, 0.0);
        assert!(thumb >= 12.0, "the thumb shrank to {thumb}pt");

        // Scrolled to the end, the thumb's bottom is the track's bottom: the
        // proportion reaches the last row rather than stopping short.
        let max = content - viewport;
        let (_track, top, thumb) = reflow::scroll_thumb(content, viewport, max);
        assert!(
            (top + thumb - track).abs() < 1e-9,
            "the thumb ends at {} in a {}pt track: it cannot reach the last row",
            top + thumb,
            track
        );

        // Dragging the thumb to the bottom of the strip is the last row, and a
        // click anywhere maps proportionally.
        assert!((offset_from_thumb(viewport, content, viewport) - max).abs() < 1e-9);
        assert!(offset_from_thumb(0.0, content, viewport).abs() < 1e-9);
        let half = offset_from_thumb(viewport / 2.0, content, viewport);
        assert!(half > 0.0 && half < max, "the middle of the strip is {half}");

        // The offset is clamped, so a wheel or drag past the end stops at the
        // last row instead of scrolling into nothing.
        assert_eq!(clamp_offset(max + 10_000.0, content, viewport), max);
        assert_eq!(clamp_offset(-50.0, content, viewport), 0.0);
    }

    #[test]
    fn a_press_on_the_scrollbar_is_not_a_row_click() {
        let content = content_height(BIG);
        let viewport = 620.0;
        // Inside the strip, only when there is something to scroll.
        assert!(scrollbar_hit(900.0, 895.0, content, viewport));
        assert!(!scrollbar_hit(900.0, 400.0, content, viewport));
        assert!(!scrollbar_hit(900.0, 895.0, 620.0, viewport), "a short list has no strip");
        assert!(!scrollbar_hit(900.0, 895.0, content, 0.0));
    }

    #[test]
    fn a_press_maps_through_the_offset_to_a_content_row() {
        // The canvas is the viewport, so the press that lands on the first
        // visible row must select that row, not row 0.
        let rows: Rc<RefCell<Vec<Vec<CellValue>>>> = Rc::new(RefCell::new(big_rows(50)));
        let sort: Rc<Cell<Option<(usize, bool)>>> = Rc::new(Cell::new(None));
        let selected: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let press: Rc<RefCell<Vec<PressHandler>>> = Rc::new(RefCell::new(Vec::new()));
        let offset = 2.0 * ROW_HEIGHT;
        let row = handle_press(
            &rows,
            &sort,
            &[true, true, true],
            900.0,
            &selected,
            &press,
            offset,
            400.0,
            HEADER_HEIGHT + 2.0,
            1,
        );
        assert_eq!(row, Some(2), "the press landed on the wrong row");
        assert_eq!(selected.get(), Some(2));
    }
}
