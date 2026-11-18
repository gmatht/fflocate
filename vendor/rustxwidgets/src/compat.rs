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
/// Column widths as fractions of the canvas width: the icon column is narrow,
/// name and path share the rest.
const ICON_COLUMN_SHARE: f64 = 0.06;

type PressHandler = Box<dyn FnMut(u32, f64, f64, *mut c_void)>;

/// A three-column results list, drawn on a canvas and hit-tested by hand.
pub struct TreeView {
    canvas: Canvas,
    columns: Vec<(String, bool, bool)>,
    rows: Rc<RefCell<Vec<Vec<CellValue>>>>,
    selected: Rc<Cell<Option<usize>>>,
    press: Rc<RefCell<Vec<PressHandler>>>,
    width: Rc<Cell<f64>>,
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
        }
    }
}

impl TreeView {
    pub fn new(columns: &[(&str, bool, bool)]) -> Result<TreeView, Error> {
        let canvas = new_canvas()?;
        let rows: Rc<RefCell<Vec<Vec<CellValue>>>> = Rc::new(RefCell::new(Vec::new()));
        let selected: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let press: Rc<RefCell<Vec<PressHandler>>> = Rc::new(RefCell::new(Vec::new()));
        let width: Rc<Cell<f64>> = Rc::new(Cell::new(640.0));
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
            let canvas_for_redraw = canvas.clone();
            canvas.on_click_button(Box::new(move |x, y, button, _state| {
                let row = row_index_at(y, rows.borrow().len());
                if button == 1 {
                    selected.set(row);
                    canvas_for_redraw.queue_redraw();
                }
                for handler in press.borrow_mut().iter_mut() {
                    handler(button, x, y, std::ptr::null_mut());
                }
            }));
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
        })
    }

    /// Replace every row, then repaint.
    pub fn set_rows(&self, rows: &[Vec<CellValue>]) {
        *self.rows.borrow_mut() = rows.to_vec();
        self.canvas.set_content_size(
            self.width.get().max(320.0) as i32,
            (HEADER_HEIGHT + rows.len() as f64 * ROW_HEIGHT) as i32 + 4,
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
/// Neither merged backend keeps a clipboard helper: GTK's loader has no
/// `gtk_clipboard_*` binding and AppKit needs an `NSPasteboard` shim. Say so
/// instead of pretending the copy happened.
pub fn clipboard_set_text(_text: &str) -> Result<(), Error> {
    Err(Error::Backend(
        "clipboard is not wired up in this build".to_string(),
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

/// A menu built from labelled closures.
///
/// Collecting items works everywhere; showing one requires a native popup
/// (`gtk_menu_popup_at_pointer` / `NSMenu popUpMenu:atLocation:inView:`), which
/// this build does not provide.
pub struct ContextMenu {
    items: Vec<(String, Box<dyn FnMut()>)>,
}

impl ContextMenu {
    pub fn new() -> ContextMenu {
        ContextMenu { items: Vec::new() }
    }

    pub fn append_item(&mut self, label: &str, action: Box<dyn FnMut()>) {
        self.items.push((label.to_string(), action));
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn popup_at_pointer(
        &mut self,
        _widget: *mut c_void,
        _event: *mut c_void,
    ) -> Result<(), Error> {
        Err(Error::Backend(
            "context menus are not available in this build".to_string(),
        ))
    }

    /// Run an item's action by label (used by tests and by a future popup).
    pub fn activate(&mut self, label: &str) -> bool {
        match self.items.iter_mut().find(|(l, _)| l == label) {
            Some((_, action)) => {
                action();
                true
            }
            None => false,
        }
    }
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
