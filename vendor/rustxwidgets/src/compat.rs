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
//!   reverses it, and the header shows the direction; its icon column draws a
//!   glyph, not a themed pixbuf.
//! * [`Expander`] is a header line plus a child box it shows or hides as one
//!   unit. The header is a label, so the state is in its text; the caller
//!   toggles it (see [`Expander::set_expanded`]).
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
    /// A themed icon, named as the old backend named it. The canvas draws
    /// `fallback` (a character or short word) because a themed pixbuf is not
    /// reachable from the portable layer.
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
    x: f64,
    y: f64,
    button: u32,
) -> Option<usize> {
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
    y: f64,
) -> Option<usize> {
    let row = row_index_at(y, rows.borrow().len())?;
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
        canvas.set_content_size(720, 320);

        {
            let rows = rows.clone();
            let selected = selected.clone();
            let sort = sort.clone();
            let columns: Vec<(String, bool, bool)> = columns
                .iter()
                .map(|(t, icon, sortable)| (t.to_string(), *icon, *sortable))
                .collect();
            let width = width.clone();
            canvas.set_draw_callback(Box::new(move |ctx, w, _h| {
                let w = if w > 0 { f64::from(w) } else { width.get() };
                width.set(w);
                paint_list(ctx, w, &columns, &rows.borrow(), selected.get(), sort.get());
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
            canvas.on_click_button(Box::new(move |x, y, button, _state| {
                handle_press(
                    &rows, &sort, &sortable, width.get(), &selected, &press, x, y, button,
                );
                canvas_for_redraw.queue_redraw();
            }));
        }

        {
            let activate = activate.clone();
            let selected = selected.clone();
            let rows = rows.clone();
            let canvas_for_redraw = canvas.clone();
            register_double_click(
                &canvas,
                Box::new(move |_x, y| {
                    handle_double_click(&rows, &selected, &activate, y);
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
        let row = handle_double_click(&self.rows, &self.selected, &self.activate, y);
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
        let count = rows.len();
        *self.rows.borrow_mut() = rows;
        self.canvas.set_content_size(
            self.width.get().max(320.0) as i32,
            (HEADER_HEIGHT + count as f64 * ROW_HEIGHT) as i32 + 4,
        );
        self.canvas.queue_redraw();
    }

    /// The row index under `(x, y)`, or `None` for the header/blank space.
    pub fn row_at(&self, _x: f64, y: f64) -> Option<usize> {
        row_index_at(y, self.rows.borrow().len())
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

    /// The scrolled container the old GTK tree view needed. The canvas scrolls
    /// because the scrolled window is given its full content size.
    pub fn into_scrolled(self) -> Result<ScrolledWindow, Error> {
        let sw = new_scrolled()?;
        sw.set_vexpand(true);
        sw.set_child(&self.canvas);
        Ok(sw)
    }

    /// Dragging rows out as file URIs needs native drag-and-drop, which the
    /// portable layer does not expose. Report it; the caller shows the reason.
    ///
    /// The argument shapes are the old `GtkTreeView` ones, so a client written
    /// against that surface compiles unchanged: `payload` yields the URI list
    /// plus the plain text, `icon` yields a theme name plus a drawn fallback.
    pub fn enable_uri_drag_source<F, G>(&self, _payload: F, _icon: G) -> Result<(), Error>
    where
        F: FnMut() -> Option<(Vec<String>, String)> + 'static,
        G: FnMut() -> Option<(String, String)> + 'static,
    {
        Err(Error::Backend(
            "drag-and-drop is not available in this build".to_string(),
        ))
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
    Image,
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
        "image-x-generic" => IconKind::Image,
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
        | "text-x-script"
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
        // A framed picture: a sun over a horizon bar.
        IconKind::Image => {
            let (r, g, b) = (0.20, 0.58, 0.36);
            ctx.stroke_rect(x, y + s * 0.12, s, s * 0.72, r, g, b, 1.0, 1.0);
            ctx.fill_rect(x + s * 0.14, y + s * 0.56, s * 0.70, s * 0.16, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.62, y + s * 0.24, s * 0.20, s * 0.20, r, g, b, 1.0);
        }
        // A box with a band across it: an archive.
        IconKind::Archive => {
            let (r, g, b) = (0.52, 0.36, 0.70);
            ctx.fill_rect(x + s * 0.04, y + s * 0.16, s * 0.92, s * 0.68, r, g, b, 1.0);
            ctx.fill_rect(x + s * 0.04, y + s * 0.40, s * 0.92, s * 0.12, 1.0, 1.0, 1.0, 1.0);
        }
        // A red page, so a PDF is told apart from a text file at this size.
        IconKind::Pdf => {
            ctx.fill_rect(x + s * 0.06, y + s * 0.08, s * 0.88, s * 0.84, 0.72, 0.18, 0.18, 1.0);
            ctx.fill_rect(x + s * 0.22, y + s * 0.30, s * 0.56, s * 0.10, 1.0, 1.0, 1.0, 1.0);
            ctx.fill_rect(x + s * 0.22, y + s * 0.52, s * 0.40, s * 0.10, 1.0, 1.0, 1.0, 1.0);
        }
    }
    true
}

/// Paint the whole list: header band, selection and every row.
///
/// Separate from the canvas callback so the geometry can be driven offscreen
/// (the tests below paint into a `RecordingDrawContext`): the callback only
/// supplies the width and the shared state.
fn paint_list(
    ctx: &mut dyn DrawContext,
    w: f64,
    columns: &[(String, bool, bool)],
    rows: &[Vec<CellValue>],
    selected: Option<usize>,
    sort: Option<(usize, bool)>,
) {
    ctx.clear(1.0, 1.0, 1.0, 1.0);

    let column_x = column_starts(w);

    // Header: each title over its own column, centred in the band, with the
    // sort direction on the column the list is sorted by.
    ctx.fill_rect(0.0, 0.0, w, HEADER_HEIGHT, 0.93, 0.93, 0.95, 1.0);
    let title_top = centred_text_top(ink_height(ctx, HEADER_SIZE, 1), HEADER_HEIGHT);
    for (i, (title, _icon, _sortable)) in columns.iter().enumerate() {
        let x = column_x.get(i).copied().unwrap_or(PAD_X);
        ctx.draw_text_styled(
            x,
            title_top,
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
    for (i, row) in rows.iter().enumerate() {
        let y = HEADER_HEIGHT + i as f64 * ROW_HEIGHT;
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
        .map(|row| {
            let key = row
                .get(column)
                .map(CellValue::render)
                .unwrap_or_default()
                .to_lowercase();
            (key, row)
        })
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
pub fn clipboard_set_text(text: &str) -> Result<(), Error> {
    crate::backends_macos_adapter::clipboard_set_text(text)
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
        backend_label()
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
        backend_label()
    )))
}

/// A disclosure: a header line with a child box under it.
#[derive(Clone)]
pub struct Expander {
    root: WidgetBox,
    header: Label,
    /// The header text without its state marker.
    title: Rc<str>,
    expanded: Rc<Cell<bool>>,
    body: WidgetBox,
}

impl Expander {
    /// A titled section, starting collapsed.
    ///
    /// The header is a label rather than a button: AppKit's click callbacks go
    /// through a process-global registry that requires `Send`, and a widget
    /// handle cannot cross a thread, so the portable layer has no click
    /// callback that works on both. The header therefore carries the state in
    /// its text and the caller toggles with [`Expander::set_expanded`] —
    /// `ff-gui` drives it from its Search menu.
    ///
    /// On GTK, apply the collapsed state *after* `Window::present`: presenting
    /// runs `show_all`, which would show the body again.
    pub fn new(label: &str) -> Result<Expander, Error> {
        let root = new_vbox(2)?;
        let body = new_vbox(2)?;
        let header = new_label(&header_text(label, false))?;
        root.append(&header);
        root.append(&body);
        set_box_visible(&body, false);
        Ok(Expander {
            root,
            header,
            title: Rc::from(label),
            expanded: Rc::new(Cell::new(false)),
            body,
        })
    }

    /// Append `child` under the header. Generic over the widget handle so the
    /// caller can pass the backend's own box type (what `App::create_box`
    /// returns) instead of this layer's wrapper.
    pub fn set_child(&self, child: &impl AsRef<*mut c_void>) {
        self.body.append(child);
    }

    /// Show or hide the body, and say which in the header.
    pub fn set_expanded(&self, expanded: bool) {
        self.expanded.set(expanded);
        set_box_visible(&self.body, expanded);
        self.header.set_text(&header_text(&self.title, expanded));
    }

    /// Whether the section is expanded.
    pub fn is_expanded(&self) -> bool {
        self.expanded.get()
    }
}

/// The header line for a disclosure in the given state.
///
/// Stated in words, not a `[+]`/`[-]` marker: the header is not clickable, and
/// a triangle or plus sign would promise a toggle that is not there.
fn header_text(title: &str, expanded: bool) -> String {
    let state = if expanded { "expanded" } else { "collapsed" };
    format!("{title} ({state})")
}

/// The expander is itself a widget: it can be packed into a box like any
/// other child (its handle is the root box's).
impl AsRef<*mut c_void> for Expander {
    fn as_ref(&self) -> &*mut c_void {
        self.root.as_ref()
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
}

impl ContextMenu {
    pub fn new() -> ContextMenu {
        ContextMenu {
            labels: Vec::new(),
            actions: Rc::new(RefCell::new(Vec::new())),
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
        context_menu_build(&self.labels, self.dispatcher())
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
        if self.labels.is_empty() {
            return Ok(());
        }
        let handle = self.build()?;
        context_menu_show(handle, widget, event)
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
        backend_label()
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
        backend_label()
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
        paint_list(&mut dc, width, &columns, &rows, None, None);

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
            "image-x-generic",
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
        paint_list(&mut dc, width, &columns, &rows, None, None);
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
}
