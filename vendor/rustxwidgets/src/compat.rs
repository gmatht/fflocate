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
//!   did. It has no sorting UI and its icon column draws a glyph, not a themed
//!   pixbuf.
//! * [`Expander`] is a toggle button plus a child box.
//! * [`ContextMenu`] collects items, but popping one up is *not* implemented in
//!   this build: [`ContextMenu::popup_at_pointer`] returns an error, which
//!   clients surface to the user. That is deliberate — a menu that silently
//!   never appears is worse than one that says why.

use crate::common::{Canvas, ScrolledWindow, WidgetBox};
use crate::core::App;
use crate::Error;
use std::cell::{Cell, RefCell};
use std::os::raw::c_void;
use std::rc::Rc;

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
fn paint_icon(
    ctx: &mut dyn crate::core::DrawContext,
    x: f64,
    y: f64,
    size: f64,
    cell: &CellValue,
) -> bool {
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

/// Column widths as fractions of the canvas width: the icon column is narrow,
/// name and path share the rest.
const ICON_COLUMN_SHARE: f64 = 0.06;

type PressHandler = Box<dyn FnMut(u32, f64, f64, *mut c_void)>;

/// Called with the row index when a row is double-clicked.
type ActivateHandler = Box<dyn FnMut(usize)>;

/// Header titles are drawn in steps of this share of the canvas width.
const HEADER_COLUMN_SHARE: f64 = 0.32;

/// A three-column results list, drawn on a canvas and hit-tested by hand.
pub struct TreeView {
    canvas: Canvas,
    columns: Vec<(String, bool, bool)>,
    rows: Rc<RefCell<Vec<Vec<CellValue>>>>,
    selected: Rc<Cell<Option<usize>>>,
    press: Rc<RefCell<Vec<PressHandler>>>,
    width: Rc<Cell<f64>>,
    /// The active sort: `(column, ascending)`. `None` keeps the caller's order.
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
            let columns: Vec<(String, bool, bool)> = columns
                .iter()
                .map(|(t, icon, sortable)| (t.to_string(), *icon, *sortable))
                .collect();
            let width = width.clone();
            canvas.set_draw_callback(Box::new(move |ctx, w, _h| {
                let w = if w > 0 { f64::from(w) } else { width.get() };
                width.set(w);
                ctx.clear(1.0, 1.0, 1.0, 1.0);
                // Header
                ctx.fill_rect(0.0, 0.0, w, HEADER_HEIGHT, 0.93, 0.93, 0.95, 1.0);
                let mut x = 4.0;
                for (title, _icon, _sortable) in &columns {
                    ctx.draw_text_styled(
                        x,
                        15.0,
                        title,
                        "sans",
                        12.0,
                        0.15,
                        0.15,
                        0.18,
                        1.0,
                        0,
                        1,
                    );
                    x += w * 0.32;
                }
                // Rows
                let rows = rows.borrow();
                for (i, row) in rows.iter().enumerate() {
                    let y = HEADER_HEIGHT + i as f64 * ROW_HEIGHT;
                    if selected.get() == Some(i) {
                        ctx.fill_rect(0.0, y, w, ROW_HEIGHT, 0.22, 0.45, 0.85, 0.18);
                    }
                    let mut x = 4.0;
                    let icon_w = w * ICON_COLUMN_SHARE;
                    let text_w = (w - icon_w) / 2.0;
                    // A pictogram when the cell is an icon, its text otherwise:
                    // one letter per file kind read as "the icons are gone".
                    let drew_icon = row
                        .first()
                        .map(|cell| paint_icon(ctx, x, y + 4.5, 11.0, cell))
                        .unwrap_or(false);
                    if !drew_icon {
                        ctx.draw_text(
                            x,
                            y + 14.0,
                            &row.first().map(CellValue::render).unwrap_or_default(),
                            "sans",
                            11.0,
                            0.2,
                            0.2,
                            0.2,
                            1.0,
                        );
                    }
                    x += icon_w;
                    for cell in row.iter().skip(1).take(2) {
                        ctx.draw_text(
                            x,
                            y + 14.0,
                            &cell.render(),
                            "sans",
                            12.0,
                            0.1,
                            0.1,
                            0.1,
                            1.0,
                        );
                        x += text_w;
                    }
                }
            }));
        }

        {
            let press = press.clone();
            let selected = selected.clone();
            let rows = rows.clone();
            let sort = sort.clone();
            let width = width.clone();
            let sortable: Vec<bool> = columns.iter().map(|(_, _, s)| *s).collect();
            let canvas_for_redraw = canvas.clone();
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
    pub fn set_rows(&self, rows: &[Vec<CellValue>]) {
        let mut rows = rows.to_vec();
        // Rows arrive in the daemon's order; a table the user sorted by Name
        // stays sorted while more hits stream in.
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

/// Register the double-click handler on the backend that reports a click count.
///
/// The portable canvas has no click-count signal for the other backends, so
/// there the handler stays out of play rather than being invented.
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

/// Which header column a click at `x` lands on, using the same 32% steps the
/// titles are drawn with.
fn header_column_at(w: f64, x: f64, columns: usize) -> Option<usize> {
    let step = w * HEADER_COLUMN_SHARE;
    if step <= 0.0 || x < 0.0 {
        return None;
    }
    let column = ((x - 4.0).max(0.0) / step) as usize;
    if column < columns {
        Some(column)
    } else {
        None
    }
}

/// Handle a click at `x` in the header band: sort by the column under it, or
/// reverse the order if it is already sorted that way. Returns `true` when the
/// sort changed.
fn apply_header_click(
    rows: &RefCell<Vec<Vec<CellValue>>>,
    sort: &Cell<Option<(usize, bool)>>,
    sortable: &[bool],
    w: f64,
    x: f64,
) -> bool {
    let Some(column) = header_column_at(w, x, sortable.len()) else {
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
    Err(Error::Backend(
        "clipboard is not wired up on this backend".to_string(),
    ))
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
    Err(Error::Backend(
        "clipboard is not wired up on this backend".to_string(),
    ))
}


/// A disclosure: a toggle button with a child box under it.
#[derive(Clone)]
pub struct Expander {
    root: WidgetBox,
    expanded: Rc<Cell<bool>>,
    body: WidgetBox,
}

impl Expander {
    /// A titled section that can be shown collapsed. The header is a label:
    /// clicking it to toggle is not wired, because the portable layer has no
    /// button click callback; callers drive it with [`Expander::set_expanded`].
    pub fn new(label: &str) -> Result<Expander, Error> {
        let root = new_vbox(2)?;
        let body = new_vbox(2)?;
        let header = new_label(label)?;
        root.append(&header);
        root.append(&body);
        Ok(Expander {
            root,
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

    pub fn set_expanded(&self, expanded: bool) {
        self.expanded.set(expanded);
        let _ = &self.body; // collapsing is not rendered by this layer
    }

    /// Whether the section was last asked to be expanded. The body is always
    /// laid out, so this is the state a caller set, not a rendered state.
    pub fn is_expanded(&self) -> bool {
        self.expanded.get()
    }
}

/// The expander is itself a widget: it can be packed into a box like any
/// other child (its handle is the root box's).
impl AsRef<*mut c_void> for Expander {
    fn as_ref(&self) -> &*mut c_void {
        self.root.as_ref()
    }
}

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
    Err(Error::Backend(
        "this backend has no menu implementation".to_string(),
    ))
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
    Err(Error::Backend(
        "this backend has no menu implementation".to_string(),
    ))
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
    /// `sortable` is accepted for source compatibility and not acted on — the
    /// canvas list has no click-to-sort headers (see [`TreeView`]).
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
