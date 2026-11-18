//! Cross-platform Spreadsheet model + Canvas renderer.
//!
//! The model is backend-agnostic; rendering is done entirely through the
//! [`crate::core::DrawContext`] 2D API, so the same widget paints identically on
//! every backend that provides a `Canvas` (gtk3, gtk4, wasm, ...). The pancurses
//! backend keeps its own terminal renderer; this module is the pixel-based
//! counterpart used by the GUI backends' `Canvas` draw callback.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::core::DrawContext;

/// Cell display style bits (mirrors corro's `CellDisplayStyle::to_pancurses_style`).
pub mod style {
    pub const DEFAULT: u8 = 0;
    pub const CURSOR: u8 = 1;
    pub const AGGREGATE: u8 = 2;
    pub const FOOTER_AGGREGATE: u8 = 3;
    pub const SELECTED: u8 = 4;
    pub const ACTIVE_HEADER: u8 = 5;
    pub const INACTIVE_HEADER: u8 = 6;
    /// Hyperlink cell: blue text + underline (corro styles link cells so).
    pub const HYPERLINK: u8 = 7;
}

pub type CursorMoveCb = Box<dyn FnMut(u32, u32)>;
pub type CommitEditCb = Box<dyn FnMut(u32, u32, String)>;

// Global (backend-wide) callback registry, used by the free
// `add_cursor_move_callback` / `add_commit_edit_callback` entry points so a
// host app can observe navigation/edits without holding a Spreadsheet handle.
thread_local! {
    static GLOBAL_CURSOR_MOVE: RefCell<Vec<CursorMoveCb>> = RefCell::new(Vec::new());
    static GLOBAL_COMMIT_EDIT: RefCell<Vec<CommitEditCb>> = RefCell::new(Vec::new());
}

pub fn add_global_cursor_move_callback(f: CursorMoveCb) {
    GLOBAL_CURSOR_MOVE.with(|c| c.borrow_mut().push(f));
}
pub fn add_global_commit_edit_callback(f: CommitEditCb) {
    GLOBAL_COMMIT_EDIT.with(|c| c.borrow_mut().push(f));
}

fn fire_cursor_move(row: u32, col: u32) {
    let mut cbs = GLOBAL_CURSOR_MOVE.with(|c| std::mem::take(&mut *c.borrow_mut()));
    for cb in cbs.iter_mut() {
        cb(row, col);
    }
    GLOBAL_CURSOR_MOVE.with(|c| *c.borrow_mut() = cbs);
}
fn fire_commit_edit(row: u32, col: u32, text: String) {
    let mut cbs = GLOBAL_COMMIT_EDIT.with(|c| std::mem::take(&mut *c.borrow_mut()));
    for cb in cbs.iter_mut() {
        cb(row, col, text.clone());
    }
    GLOBAL_COMMIT_EDIT.with(|c| *c.borrow_mut() = cbs);
}

/// A cell map keyed by `(row, col)`, hashed for that shape.
///
/// The renderer looks up every painted cell, every frame -- ~5000 lookups per
/// keypress -- and `std::collections::HashMap` seeds SipHash per map, which was
/// the single largest entry in a browser profile of one arrow key (70 ms).
/// A cell key is two integers with no adversarial input, so the multiply-xor
/// hash below is enough, and it needs no dependency.
pub type CellMap<V> = HashMap<(u32, u32), V, BuildCellHasher>;

/// Builds [`CellHasher`]. See [`CellMap`].
#[derive(Clone, Copy, Default)]
pub struct BuildCellHasher;

impl std::hash::BuildHasher for BuildCellHasher {
    type Hasher = CellHasher;
    fn build_hasher(&self) -> CellHasher {
        CellHasher(0)
    }
}

/// See [`CellMap`].
pub struct CellHasher(u64);

impl std::hash::Hasher for CellHasher {
    fn finish(&self) -> u64 {
        // Avalanche the mixed word so the low bits (h1 of hashbrown) are uniform.
        let mut x = self.0;
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        x ^= x >> 33;
        x
    }

    fn write(&mut self, bytes: &[u8]) {
        // Not used by `CellMap`'s keys, but a `Hasher` must implement it:
        // fold the bytes in rather than silently ignoring them.
        for &b in bytes {
            self.0 = (self.0.rotate_left(7) ^ b as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }

    fn write_u32(&mut self, n: u32) {
        // Row then column arrive as two `write_u32` calls (that is how
        // `(u32, u32)` hashes); mix each in with an odd multiplier.
        self.0 = self.0.rotate_left(32) ^ (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = self.0.rotate_left(32) ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

/// Backend-agnostic spreadsheet data model.
pub struct SpreadsheetModel {
    pub cells: CellMap<String>,
    pub cell_styles: CellMap<u8>,
    pub raw_cells: CellMap<String>,
    pub cursor_row: u32,
    pub cursor_col: u32,
    pub anchor: Option<(u32, u32)>,
    /// Number of leading "margin" columns (left label/outlier area).
    pub margin_cols: u32,
    /// Number of main data columns.
    pub main_cols: u32,
    pub header_row_count: u32,
    pub main_row_count: u32,
    /// `(global_col, width_in_chars, title)` for each displayed column.
    pub column_layout: Vec<(u32, u32, String)>,
    /// `(row, label)` for the left margin labels.
    pub row_labels: Vec<(u32, String)>,
    pub menu_text: String,
    pub border_title: String,
    pub status_text: String,
    pub formula_bar_trailing: String,
    pub tab_titles: Vec<String>,
    pub tab_active: usize,
    pub editing: bool,
    pub edit_buf: String,
    pub edit_pos: usize,
    pub formula_bar_address: Option<String>,
    pub formula_bar_entry: Option<String>,
    pub cursor_move_callbacks: Vec<CursorMoveCb>,
    pub commit_edit_callbacks: Vec<CommitEditCb>,
    /// View scale, 1.0 = the base metrics. Pinch-to-zoom on a phone, or any
    /// host that wants a bigger/smaller sheet, sets this; the renderer, the
    /// column layout and the hit-test all multiply their metrics by it, so
    /// what is drawn and what a pointer lands on can never disagree. Clamped
    /// into [`MIN_ZOOM`](Self::MIN_ZOOM)..=[`MAX_ZOOM`](Self::MAX_ZOOM) by
    /// [`set_zoom`](Self::set_zoom).
    pub zoom: f64,
}

impl Default for SpreadsheetModel {
    fn default() -> Self {
        SpreadsheetModel {
            cells: CellMap::default(),
            cell_styles: CellMap::default(),
            raw_cells: CellMap::default(),
            cursor_row: 0,
            cursor_col: 0,
            anchor: None,
            margin_cols: 0,
            main_cols: 0,
            header_row_count: 0,
            main_row_count: 0,
            column_layout: Vec::new(),
            row_labels: Vec::new(),
            menu_text: String::new(),
            border_title: String::new(),
            status_text: String::new(),
            formula_bar_trailing: String::new(),
            tab_titles: Vec::new(),
            tab_active: 0,
            editing: false,
            edit_buf: String::new(),
            edit_pos: 0,
            formula_bar_address: None,
            formula_bar_entry: None,
            cursor_move_callbacks: Vec::new(),
            commit_edit_callbacks: Vec::new(),
            // The identity scale, not zero: a manually built model (the widget
            // does this) must render at the base metrics.
            zoom: 1.0,
        }
    }
}

impl SpreadsheetModel {
    pub fn new(rows: u32, cols: u32) -> Self {
        SpreadsheetModel {
            main_row_count: rows,
            main_cols: cols,
            margin_cols: 1,
            header_row_count: 1,
            column_layout: (0..cols).map(|c| (c, 12u32, format!("{}", c + 1))).collect(),
            zoom: 1.0,
            ..Default::default()
        }
    }

    pub fn set_cell(&mut self, row: u32, col: u32, text: &str) {
        self.cells.insert((row, col), text.to_string());
    }
    pub fn set_raw_cell(&mut self, row: u32, col: u32, text: &str) {
        self.raw_cells.insert((row, col), text.to_string());
    }
    pub fn get_cell(&self, row: u32, col: u32) -> Option<String> {
        self.cells.get(&(row, col)).cloned()
    }
    pub fn set_cell_style(&mut self, row: u32, col: u32, s: u8) {
        self.cell_styles.insert((row, col), s);
    }
    pub fn set_cursor(&mut self, row: u32, col: u32) {
        self.cursor_row = row;
        self.cursor_col = col;
        let mut cbs = std::mem::take(&mut self.cursor_move_callbacks);
        for cb in cbs.iter_mut() {
            cb(row, col);
        }
        self.cursor_move_callbacks = cbs;
        fire_cursor_move(row, col);
    }
    pub fn cursor_position(&self) -> Option<(u32, u32)> {
        Some((self.cursor_row, self.cursor_col))
    }
    pub fn set_editing(&mut self, editing: bool, buf: &str, pos: usize) {
        self.editing = editing;
        self.edit_buf = buf.to_string();
        self.edit_pos = pos;
    }
    pub fn set_grid_config(&mut self, margin_cols: u32, main_cols: u32) {
        self.margin_cols = margin_cols;
        self.main_cols = main_cols;
    }
    pub fn set_row_counts(&mut self, header_rows: u32, main_rows: u32) {
        self.header_row_count = header_rows;
        self.main_row_count = main_rows;
    }
    pub fn set_column_layout(&mut self, layout: Vec<(u32, u32, String)>) {
        self.column_layout = layout;
    }
    pub fn set_row_labels(&mut self, labels: Vec<(u32, String)>) {
        self.row_labels = labels;
    }
    pub fn set_menu_text(&mut self, text: &str) {
        self.menu_text = text.to_string();
    }
    pub fn set_border_title(&mut self, text: &str) {
        self.border_title = text.to_string();
    }
    pub fn set_status_text(&mut self, text: &str) {
        self.status_text = text.to_string();
    }
    pub fn set_formula_bar_trailing(&mut self, text: &str) {
        self.formula_bar_trailing = text.to_string();
    }
    pub fn set_tab_data(&mut self, titles: &[String], active: usize) {
        self.tab_titles = titles.to_vec();
        self.tab_active = active;
    }
    pub fn set_formula_bar(&mut self, address: &str, entry: &str) {
        self.formula_bar_address = Some(address.to_string());
        self.formula_bar_entry = Some(entry.to_string());
    }
    pub fn commit_formula_bar(&mut self) {
        if let (Some(addr), Some(text)) = (self.formula_bar_address.clone(), self.formula_bar_entry.clone()) {
            // address like "B3" -> (row, col); fall back to cursor if unparseable
            if let Some((r, c)) = parse_address(&addr) {
                self.cells.insert((r, c), text.clone());
                let mut cbs = std::mem::take(&mut self.commit_edit_callbacks);
                for cb in cbs.iter_mut() {
                    cb(r, c, text.clone());
                }
                self.commit_edit_callbacks = cbs;
                fire_commit_edit(r, c, text);
            }
        }
    }
    pub fn add_cursor_move_callback(&mut self, f: CursorMoveCb) {
        self.cursor_move_callbacks.push(f);
    }
    pub fn add_commit_edit_callback(&mut self, f: CommitEditCb) {
        self.commit_edit_callbacks.push(f);
    }

    /// Pixel layout constants (kept in one place so backends agree).
    pub const CHAR_W: f64 = 8.0;
    pub const ROW_H: f64 = 22.0;
    pub const HEADER_H: f64 = 22.0;
    pub const ROW_LABEL_W: f64 = 64.0;

    /// Smallest/largest view scale a pinch may reach. 0.4 keeps a whole
    /// spreadsheet legible on a phone; 4.0 is beyond what a finger gesture
    /// comfortably reaches and stops a runaway multiplier from growing the
    /// layout without bound.
    pub const MIN_ZOOM: f64 = 0.4;
    pub const MAX_ZOOM: f64 = 4.0;

    /// The view scale, with the degenerate values (0 from a `Default`-built
    /// struct, NaN from a bad multiplication) normalised to 1.0. Reading the
    /// scale always goes through here so a nonsense `zoom` cannot collapse
    /// the layout to zero-sized cells.
    pub fn zoom(&self) -> f64 {
        if self.zoom.is_finite() && self.zoom > 0.0 {
            self.zoom
        } else {
            1.0
        }
    }

    /// Set the view scale, clamped to [`MIN_ZOOM`](Self::MIN_ZOOM)..=
    /// [`MAX_ZOOM`](Self::MAX_ZOOM). Returns the scale actually applied.
    ///
    /// NaN has no ordering and so cannot be clamped; it means "no zoom" rather
    /// than poisoning the metrics. A +/- infinity *is* clampable ("as big/small
    /// as possible"), and matches the host-side `set_view_zoom`.
    pub fn set_zoom(&mut self, zoom: f64) -> f64 {
        let z = if zoom.is_nan() { 1.0 } else { zoom };
        self.zoom = z.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        self.zoom()
    }

    /// Multiply the current scale by `factor` (a pinch's span ratio), keeping
    /// the point under the fingers fixed.
    ///
    /// `anchor_x`/`anchor_y` are in the same pixel space as
    /// [`paint`](fn@paint) (origin at the grid's top-left). The model has no
    /// independent scroll offset (the cursor *is* the viewport in corro's GUI,
    /// and the widget renders the whole model), so anchoring is expressed by
    /// the caller: the gesture layer re-derives its scroll offsets from the
    /// same pixel, and the widget's `hit_test` keeps pointing at the cell that
    /// was under the fingers because it scales from the same origin. The
    /// parameters are accepted here so a host with an offset can implement
    /// true focus-preserving zoom without changing this signature.
    ///
    /// Returns the scale actually applied after clamping.
    pub fn zoom_by(&mut self, factor: f64, anchor_x: f64, anchor_y: f64) -> f64 {
        let _ = (anchor_x, anchor_y);
        if !factor.is_finite() || factor <= 0.0 {
            return self.zoom();
        }
        self.set_zoom(self.zoom() * factor)
    }

    /// Convenience: pixel metrics at the current scale. The renderer and the
    /// hit-test both use these rather than the raw constants, so a zoomed grid
    /// keeps drawing and pointing in agreement.
    pub fn char_w(&self) -> f64 {
        Self::CHAR_W * self.zoom()
    }
    pub fn row_h(&self) -> f64 {
        Self::ROW_H * self.zoom()
    }
    pub fn header_h(&self) -> f64 {
        Self::HEADER_H * self.zoom()
    }
    pub fn row_label_w(&self) -> f64 {
        Self::ROW_LABEL_W * self.zoom()
    }

    /// X pixel offset of a given global column (after the left label area).
    fn col_x(&self, global_col: u32) -> f64 {
        let mut x = self.row_label_w();
        for &(gc, w, _) in &self.column_layout {
            if gc >= global_col {
                break;
            }
            x += w as f64 * self.char_w();
        }
        x
    }
    fn col_width(&self, global_col: u32) -> f64 {
        for &(gc, w, _) in &self.column_layout {
            if gc == global_col {
                return w as f64 * self.char_w();
            }
        }
        12.0 * self.char_w()
    }
    fn col_title(&self, global_col: u32) -> String {
        for (gc, _, t) in &self.column_layout {
            if *gc == global_col {
                return t.clone();
            }
        }
        format!("{}", global_col + 1)
    }
}

fn parse_address(a: &str) -> Option<(u32, u32)> {
    let a = a.trim();
    let mut col_chars = String::new();
    let mut rest = a;
    let mut chars = a.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphabetic() {
            col_chars.push(c);
            rest = &a[col_chars.len()..];
            chars.next();
        } else {
            break;
        }
    }
    let col: u32 = col_chars
        .chars()
        .rev()
        .enumerate()
        .map(|(i, c)| {
            let v = (c.to_ascii_uppercase() as u32) - b'A' as u32 + 1;
            v * 26u32.pow(i as u32)
        })
        .sum::<u32>()
        .saturating_sub(1);
    let row: u32 = rest.trim().parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col))
}

/// Paint the whole spreadsheet into `dc` using the cross-platform 2D API.
pub fn paint(model: &SpreadsheetModel, dc: &mut dyn DrawContext, _w: i32, _h: i32) {
    // One lookup per frame, then pass the copy down: this function fills
    // hundreds of rects and draws hundreds of strings, and re-taking the theme
    // lock per call would put a shared-lock acquisition in the innermost loop.
    let t = crate::core::theme();
    let paper = t.color(crate::core::Role::Paper);
    dc.clear(paper.r, paper.g, paper.b, 1.0);

    let header_h = model.header_row_count as f64 * model.header_h();
    let row_label_w = model.row_label_w();

    // Total columns to draw = margin + main.
    let total_cols = model.margin_cols + model.main_cols;
    let total_rows = model.header_row_count + model.main_row_count;

    // ---- grid cells ----
    for r in 0..total_rows {
        let ry = header_h + r as f64 * model.row_h();
        let is_header_row = r < model.header_row_count;
        for c in 0..total_cols {
            let cx = if c < model.margin_cols {
                // margin / label column
                c as f64 * (row_label_w / model.margin_cols.max(1) as f64)
            } else {
                model.col_x(c)
            };
            let cw = if c < model.margin_cols {
                row_label_w / model.margin_cols.max(1) as f64
            } else {
                model.col_width(c)
            };

            let style = model.cell_styles.get(&(r, c)).copied().unwrap_or(style::DEFAULT);
            let is_cursor = r == model.cursor_row && c == model.cursor_col;
            let is_selected = style == style::SELECTED;

            // background
            let bg = bg_for(&t, style, is_cursor, model.editing);
            dc.fill_rect(cx, ry, cw, model.row_h(), bg.0, bg.1, bg.2, bg.3);

            // text
            let text = if is_header_row {
                model.col_title(c)
            } else if c < model.margin_cols {
                row_label_for(model, r)
            } else {
                model.cells.get(&(r, c)).cloned().unwrap_or_default()
            };
            if !text.is_empty() {
                // Text on a highlight fill takes the highlight ink, not body
                // ink: in night mode the caret/selection fills are *lighter*
                // than the body, so body ink would be low-contrast on the very
                // cell the user is looking at.
                let (fr, fg, fb) = fg_for(&t, style, is_cursor || is_selected);
                if style_bold(style) {
                    // Draw twice with a 1px offset to fake bold (no font weight API yet).
                    dc.draw_text(cx + 2.0, ry + 3.0, &text, "monospace", 13.0, fr, fg, fb, 1.0);
                    dc.draw_text(cx + 3.0, ry + 3.0, &text, "monospace", 13.0, fr, fg, fb, 1.0);
                } else {
                    dc.draw_text(cx + 2.0, ry + 3.0, &text, "monospace", 13.0, fr, fg, fb, 1.0);
                }
                // Hyperlinks render blue and underlined by default (same
                // rule as the terminal backends); no font-underline API
                // exists, so rule the line with a 1px fill under the text.
                if style == style::HYPERLINK {
                    let (_, _, tw, th) = dc.text_extents(&text, "monospace", 13.0);
                    if tw > 0.0 {
                        dc.fill_rect(cx + 2.0, ry + 3.0 + th, tw, 1.0, fr, fg, fb, 1.0);
                    }
                }
            }

            // cursor outline. Drawn in the accent colour rather than a
            // hardcoded blue: a saturated blue that reads as "focused" on a
            // white sheet disappears on a near-black one.
            if is_cursor && !model.editing {
                let c = t.color(crate::core::Role::TextAccent);
                dc.stroke_rect(cx, ry, cw, model.row_h(), c.r, c.g, c.b, 1.0, 2.0);
            }

            // grid line
            let g = t.color(crate::core::Role::Gridline);
            dc.stroke_rect(cx, ry, cw, model.row_h(), g.r, g.g, g.b, 1.0, 0.5);
        }
    }

    // ---- formula bar (top strip above the grid) ----
    let fb_y = 0.0;
    let hbg = t.color(crate::core::Role::Header);
    dc.fill_rect(0.0, fb_y, 4096.0, model.header_h(), hbg.r, hbg.g, hbg.b, 1.0);
    let addr = model
        .formula_bar_address
        .clone()
        .or_else(|| Some(cell_addr(model.cursor_row, model.cursor_col)))
        .unwrap_or_default();
    let entry = if model.editing {
        model.edit_buf.clone()
    } else {
        model.formula_bar_entry.clone().unwrap_or_default()
    };
    let fb_text = format!("{}  {}", addr, entry);
    let txt = t.color(crate::core::Role::Text);
    dc.draw_text(4.0, 4.0, &fb_text, "monospace", 13.0, txt.r, txt.g, txt.b, 1.0);
    if !model.formula_bar_trailing.is_empty() {
        let m = t.color(crate::core::Role::TextMuted);
        dc.draw_text(
            row_label_w,
            4.0,
            &model.formula_bar_trailing,
            "monospace",
            13.0,
            m.r,
            m.g,
            m.b,
            1.0,
        );
    }

    // ---- tabs (bottom strip) ----
    if !model.tab_titles.is_empty() {
        let tab_h = model.header_h();
        let ty = header_h + total_rows as f64 * model.row_h();
        let strip = t.color(crate::core::Role::TabStrip);
        dc.fill_rect(0.0, ty, 4096.0, tab_h, strip.r, strip.g, strip.b, 1.0);
        let tab_text = t.color(crate::core::Role::Text);
        let mut tx = 4.0;
        for (i, title) in model.tab_titles.iter().enumerate() {
            let active = i == model.tab_active;
            let c = t.color(if active {
                crate::core::Role::TabActive
            } else {
                crate::core::Role::TabIdle
            });
            let w = 80.0;
            dc.fill_rect(tx, ty + 2.0, w, tab_h - 4.0, c.r, c.g, c.b, 1.0);
            dc.draw_text(
                tx + 4.0,
                ty + 5.0,
                title,
                "monospace",
                12.0,
                tab_text.r,
                tab_text.g,
                tab_text.b,
                1.0,
            );
            tx += w + 4.0;
        }
    }

    // ---- border title / status (left gutter bottom) ----
    if !model.border_title.is_empty() {
        let m = t.color(crate::core::Role::TextMuted);
        dc.draw_text(
            4.0,
            header_h + 2.0,
            &model.border_title,
            "monospace",
            12.0,
            m.r,
            m.g,
            m.b,
            1.0,
        );
    }
    if !model.status_text.is_empty() {
        let m = t.color(crate::core::Role::TextMuted);
        dc.draw_text(
            4.0,
            header_h + (total_rows as f64 + 1.0) * model.row_h(),
            &model.status_text,
            "monospace",
            11.0,
            m.r,
            m.g,
            m.b,
            1.0,
        );
    }
}

fn row_label_for(model: &SpreadsheetModel, r: u32) -> String {
    for (rr, label) in &model.row_labels {
        if *rr == r {
            return label.clone();
        }
    }
    format!("{}", r + 1)
}

fn cell_addr(r: u32, c: u32) -> String {
    let mut s = String::new();
    let mut c = c;
    loop {
        let ch = (b'A' + (c % 26) as u8) as char;
        s.insert(0, ch);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s + &format!("{}", r + 1)
}

fn style_bold(s: u8) -> bool {
    matches!(s, style::ACTIVE_HEADER | style::INACTIVE_HEADER | style::AGGREGATE | style::FOOTER_AGGREGATE)
}

fn bg_for(t: &crate::core::Theme, s: u8, is_cursor: bool, editing: bool) -> (f64, f64, f64, f64) {
    use crate::core::Role;
    let c = if is_cursor {
        if editing {
            t.color(Role::CursorEditing)
        } else {
            t.color(Role::Cursor)
        }
    } else if matches!(s, style::ACTIVE_HEADER | style::INACTIVE_HEADER) {
        t.color(Role::Header)
    } else {
        t.color(Role::CellBody)
    };
    c.rgba()
}

fn fg_for(t: &crate::core::Theme, s: u8, on_highlight: bool) -> (f64, f64, f64) {
    use crate::core::Role;
    let c = match s {
        style::AGGREGATE | style::FOOTER_AGGREGATE => t.color(Role::Aggregate),
        style::HYPERLINK => t.color(Role::TextAccent),
        // A link on a highlight fill would otherwise be light-blue-on-light.
        _ if on_highlight => t.color(Role::TextOnHighlight),
        _ => t.color(Role::Text),
    };
    (c.r, c.g, c.b)
}

/// Map a pixel coordinate (from a click) back to a cell, if any.
pub fn cell_at(model: &SpreadsheetModel, x: f64, y: f64) -> Option<(u32, u32)> {
    let header_h = model.header_row_count as f64 * model.header_h();
    if y < header_h {
        return None;
    }
    let r = ((y - header_h) / model.row_h()).floor() as u32;
    if r >= model.header_row_count + model.main_row_count {
        return None;
    }
    let total_cols = model.margin_cols + model.main_cols;
    let mut cx = model.row_label_w();
    for c in 0..total_cols {
        let cw = if c < model.margin_cols {
            model.row_label_w() / model.margin_cols.max(1) as f64
        } else {
            model.col_width(c)
        };
        if x >= cx && x < cx + cw {
            return Some((r, c));
        }
        cx += cw;
    }
    None
}

/// Build a shareable `SpreadsheetModel` handle for a backend `Spreadsheet`.
pub type SharedModel = Rc<RefCell<SpreadsheetModel>>;

/// Construct a fresh shared model handle.
pub fn new_shared_model(rows: u32, cols: u32) -> SharedModel {
    Rc::new(RefCell::new(SpreadsheetModel::new(rows, cols)))
}

#[cfg(test)]
mod tests {
    use super::style;
    use super::{paint, SpreadsheetModel};
    use crate::backends::headless::{DrawOp, RecordingDrawContext};

    /// Style bit 7 (hyperlink) paints blue text plus a 1px underline rule
    /// in the same blue; other cells keep the default dark text and no
    /// rule. Structural: asserts the recorded draw ops, not just that the
    /// text was drawn.
    #[test]
    fn hyperlink_style_paints_blue_text_with_underline() {
        let mut model = SpreadsheetModel::new(3, 3);
        model.set_cell(1, 1, "https://example.com");
        model.set_cell_style(1, 1, style::HYPERLINK);
        model.set_cell(1, 2, "plain");
        let mut dc = RecordingDrawContext::new();
        paint(&model, &mut dc, 400, 300);
        let blue = (0.0, 0.0, 0.85, 1.0);
        let link_texts: Vec<(f64, f64, f64, f64)> = dc
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, rgba, .. } if text.contains("https") => Some(*rgba),
                _ => None,
            })
            .collect();
        assert_eq!(link_texts, vec![blue], "link text must be blue, got {link_texts:?}");
        let rules: Vec<(f64, f64, f64, f64)> = dc
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::FillRect { h, rgba, .. } if *h == 1.0 && *rgba == blue => Some(*rgba),
                _ => None,
            })
            .collect();
        assert_eq!(rules.len(), 1, "exactly one blue underline rule expected");
        let plain_texts: Vec<(f64, f64, f64, f64)> = dc
            .ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, rgba, .. } if text == "plain" => Some(*rgba),
                _ => None,
            })
            .collect();
        assert_eq!(
            plain_texts,
            vec![(0.05, 0.05, 0.1, 1.0)],
            "plain cell must keep default dark text"
        );
    }
}
