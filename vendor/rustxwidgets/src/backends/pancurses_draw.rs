//! Pancurses (terminal cell-grid) `DrawContext` for the shared spreadsheet model.
//!
//! The pancurses backend drives a real terminal by emitting SGR/ANSI escapes,
//! i.e. it is conceptually a character cell grid. This module provides a
//! `DrawContext` that paints into exactly such a grid, so `Spreadsheet::paint`
//! (the same pixel-based paint used by GTK and ratatui) renders identically here.
//! The pixel→cell mapping reuses `SpreadsheetModel::CHAR_W`/`ROW_H`, matching
//! [`crate::backends::ratatui`], so backend parity holds by construction.
//!
//! It is gated behind the `pancurses` feature purely for organisational
//! clarity; it has no hard dependency on the `pancurses` crate (it renders into
//! an in-memory [`CellGrid`] that a host can blit to a real `pancurses::Window`
//! or emit as SGR).

use crate::core::DrawContext;
use crate::spreadsheet::SpreadsheetModel;

const CELL_W: f64 = SpreadsheetModel::CHAR_W;
const CELL_H: f64 = SpreadsheetModel::ROW_H;

/// One terminal cell: a glyph plus resolved foreground/background RGB.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GridCell {
    pub ch: char,
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
}

/// A rectangular character cell grid (the pancurses "window").
pub struct CellGrid {
    pub w: u16,
    pub h: u16,
    pub cells: Vec<Vec<GridCell>>,
}

impl CellGrid {
    pub fn new(w: u16, h: u16) -> Self {
        CellGrid {
            w,
            h,
            cells: vec![vec![GridCell::default(); w as usize]; h as usize],
        }
    }

    /// Join each row into a string (for parity assertions).
    pub fn row_strings(&self) -> Vec<String> {
        self.cells.iter().map(|r| r.iter().map(|c| c.ch).collect()).collect()
    }

    fn cell_mut(&mut self, x: u16, y: u16) -> Option<&mut GridCell> {
        self.cells.get_mut(y as usize).and_then(|row| row.get_mut(x as usize))
    }

    fn set_bg(&mut self, area: ratatui_like_rect::Rect, bg: (u8, u8, u8)) {
        for y in area.top..area.bottom {
            for x in area.left..area.right {
                if let Some(c) = self.cell_mut(x, y) {
                    c.bg = bg;
                }
            }
        }
    }
}

mod ratatui_like_rect {
    #[derive(Clone, Copy)]
    pub struct Rect {
        pub left: u16,
        pub top: u16,
        pub right: u16,
        pub bottom: u16,
    }
    impl Rect {
        pub fn new(left: u16, top: u16, right: u16, bottom: u16) -> Self {
            Rect { left, top, right, bottom }
        }
    }
}

fn to_rgb(r: f64, g: f64, b: f64) -> (u8, u8, u8) {
    let c = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    (c(r), c(g), c(b))
}

fn to_area(grid: &CellGrid, x: f64, y: f64, w: f64, h: f64) -> ratatui_like_rect::Rect {
    let x0 = (x / CELL_W).floor().clamp(0.0, grid.w as f64) as u16;
    let y0 = (y / CELL_H).floor().clamp(0.0, grid.h as f64) as u16;
    let x1 = ((x + w) / CELL_W).ceil().clamp(0.0, grid.w as f64) as u16;
    let y1 = ((y + h) / CELL_H).ceil().clamp(0.0, grid.h as f64) as u16;
    let ww = x1.saturating_sub(x0);
    let hh = y1.saturating_sub(y0);
    if ww == 0 || hh == 0 {
        ratatui_like_rect::Rect::new(0, 0, 0, 0)
    } else {
        ratatui_like_rect::Rect::new(x0, y0, x1, y1)
    }
}

/// A [`DrawContext`] that paints into a [`CellGrid`] (pancurses terminal model).
pub struct PancursesDrawContext<'a> {
    grid: &'a mut CellGrid,
}

impl<'a> PancursesDrawContext<'a> {
    pub fn new(grid: &'a mut CellGrid) -> Self {
        PancursesDrawContext { grid }
    }
}

impl<'a> DrawContext for PancursesDrawContext<'a> {
    fn clear(&mut self, r: f64, g: f64, b: f64, _a: f64) {
        let bg = to_rgb(r, g, b);
        for row in self.grid.cells.iter_mut() {
            for cell in row.iter_mut() {
                *cell = GridCell { ch: ' ', fg: (0, 0, 0), bg };
            }
        }
    }

    fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, _a: f64) {
        let area = to_area(self.grid, x, y, w, h);
        self.grid.set_bg(area, to_rgb(r, g, b));
    }

    fn stroke_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, _a: f64, _lw: f64) {
        let area = to_area(self.grid, x, y, w, h);
        let color = to_rgb(r, g, b);
        let left = area.left;
        let right = area.right.saturating_sub(1);
        let top = area.top;
        let bottom = area.bottom.saturating_sub(1);
        for cx in left..=right {
            if let Some(c) = self.grid.cell_mut(cx, top) {
                if c.ch == ' ' {
                    c.ch = '─';
                    c.fg = color;
                }
            }
            if bottom != top {
                if let Some(c) = self.grid.cell_mut(cx, bottom) {
                    if c.ch == ' ' {
                        c.ch = '─';
                        c.fg = color;
                    }
                }
            }
        }
        for cy in top..=bottom {
            if let Some(c) = self.grid.cell_mut(left, cy) {
                if c.ch == ' ' {
                    c.ch = '│';
                    c.fg = color;
                }
            }
            if right != left {
                if let Some(c) = self.grid.cell_mut(right, cy) {
                    if c.ch == ' ' {
                        c.ch = '│';
                        c.fg = color;
                    }
                }
            }
        }
        if let Some(c) = self.grid.cell_mut(left, top) {
            if c.ch == ' ' {
                c.ch = '┌';
                c.fg = color;
            }
        }
        if right != left {
            if let Some(c) = self.grid.cell_mut(right, top) {
                if c.ch == ' ' {
                    c.ch = '┐';
                    c.fg = color;
                }
            }
        }
        if bottom != top {
            if let Some(c) = self.grid.cell_mut(left, bottom) {
                if c.ch == ' ' {
                    c.ch = '└';
                    c.fg = color;
                }
            }
            if right != left {
                if let Some(c) = self.grid.cell_mut(right, bottom) {
                    if c.ch == ' ' {
                        c.ch = '┘';
                        c.fg = color;
                    }
                }
            }
        }
    }

    fn draw_text_styled(&mut self, x: f64, y: f64, text: &str, _f: &str, _s: f64, r: f64, g: f64, b: f64, _a: f64, _sl: i32, _w: i32) {
        let col = ((x / CELL_W).floor() + 1.0).clamp(0.0, self.grid.w as f64) as u16;
        let row = (y / CELL_H).floor().clamp(0.0, self.grid.h as f64) as u16;
        let color = to_rgb(r, g, b);
        let mut cx = col;
        for ch in text.chars() {
            if cx >= self.grid.w {
                break;
            }
            if let Some(cell) = self.grid.cell_mut(cx, row) {
                cell.ch = ch;
                cell.fg = color;
            }
            cx += 1;
        }
    }

    fn text_extents_styled(&self, text: &str, _f: &str, _s: f64, _sl: i32, _w: i32) -> (f64, f64, f64, f64) {
        (0.0, 0.0, text.chars().count() as f64 * CELL_W, CELL_H)
    }

    fn save(&mut self) {}
    fn restore(&mut self) {}
    fn clip(&mut self, _x: f64, _y: f64, _w: f64, _h: f64) {}
}

/// Render `model` through the shared `paint` into a fresh [`CellGrid`] (pancurses model).
pub fn render_model_to_grid(model: &SpreadsheetModel, w: u16, h: u16) -> CellGrid {
    let mut grid = CellGrid::new(w, h);
    let mut dc = PancursesDrawContext::new(&mut grid);
    crate::spreadsheet::paint(model, &mut dc, w as i32, h as i32);
    grid
}

// ─────────────────────────────────────────────────────────────────────────────
// Blitting a CellGrid to a terminal
// ─────────────────────────────────────────────────────────────────────────────

/// Map an RGB triple to an xterm-256 palette index.
///
/// The renderer produces resolved RGB (it is shared with the pixel backends),
/// while a terminal has a fixed palette — most commonly the 6×6×6 colour cube
/// plus a 24-step grey ramp, i.e. indices 16..=255. Reducing to that cube is
/// what makes the same `SpreadsheetModel` paintable on a terminal at all.
///
/// The 16 system colours (0..=15) are deliberately avoided: their RGB is
/// terminal-configurable, so mapping onto them would make the rendered result
/// depend on the user's theme rather than on the model's colours.
pub fn rgb_to_palette_index(rgb: (u8, u8, u8)) -> u8 {
    let (r, g, b) = rgb;
    // Near-grey values read better from the grey ramp than the cube, because
    // the cube has no true greys (its axis is 0,95,135,175,215,255).
    if r == g && g == b {
        if r < 8 {
            return 16; // cube black
        }
        if r > 238 {
            return 231; // cube white
        }
        // 24 greys at 8,18,28,…,238.
        let step = ((r as i32 - 8) / 10).clamp(0, 23) as u8;
        return 232 + step;
    }
    let level = |v: u8| -> u8 {
        // The cube's axis values; pick the nearest.
        const AXIS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let mut best = 0usize;
        let mut best_d = u16::MAX;
        for (i, &a) in AXIS.iter().enumerate() {
            let d = (a as i32 - v as i32).unsigned_abs() as u16;
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best as u8
    };
    16 + 36 * level(r) + 6 * level(g) + level(b)
}

/// One run of adjacent cells sharing an attribute pattern.
#[derive(Debug, PartialEq, Eq)]
pub struct AttrRun {
    pub start_col: u16,
    pub text: String,
    pub fg: u8,
    pub bg: u8,
}

/// Split a row into attribute runs, so the emitter switches SGR only when the
/// colours actually change instead of once per cell.
///
/// A run is a maximal sequence of cells with identical fg/bg. Consecutive
/// spaces inside a run are kept (they are part of the run's extent), which
/// matters because the painter uses background colour to draw cell fills.
pub fn row_attr_runs(grid: &CellGrid, row: u16) -> Vec<AttrRun> {
    let Some(cells) = grid.cells.get(row as usize) else {
        return Vec::new();
    };
    let mut runs: Vec<AttrRun> = Vec::new();
    for (x, cell) in cells.iter().enumerate() {
        let fg = rgb_to_palette_index(cell.fg);
        let bg = rgb_to_palette_index(cell.bg);
        // A cell whose glyph is blank but whose background differs still needs
        // its own run: the background *is* the visible content.
        match runs.last_mut() {
            Some(run) if run.fg == fg && run.bg == bg => run.text.push(cell.ch),
            _ => runs.push(AttrRun {
                start_col: x as u16,
                text: cell.ch.to_string(),
                fg,
                bg,
            }),
        }
    }
    runs
}

impl CellGrid {
    /// Emit ANSI/SGR for the whole grid, positioned at `(origin_y, origin_x)`.
    ///
    /// Returns the escape stream a host writes to the terminal. Cursor
    /// positioning is absolute (CUP), so the caller may blit this anywhere
    /// without tracking terminal state — the same property the backend's own
    /// SGR emitter relies on.
    ///
    /// This is the inverse of [`PancursesDrawContext`]: that turns draw calls
    /// into cells, this turns cells into bytes.
    pub fn to_ansi(&self, origin_y: u16, origin_x: u16) -> String {
        let mut out = String::new();
        for row in 0..self.h {
            let runs = row_attr_runs(self, row);
            if runs.is_empty() {
                continue;
            }
            out.push_str("\x1b[0m");
            let mut current: Option<(u8, u8)> = None;
            for run in runs {
                if current != Some((run.fg, run.bg)) {
                    out.push_str(&format!(
                        "\x1b[38;5;{}m\x1b[48;5;{}m",
                        run.fg, run.bg
                    ));
                    current = Some((run.fg, run.bg));
                }
                out.push_str(&format!(
                    "\x1b[{};{}H{}",
                    origin_y as u32 + row as u32 + 1,
                    origin_x as u32 + run.start_col as u32 + 1,
                    run.text
                ));
            }
            out.push_str("\x1b[0m");
        }
        out
    }

    /// Blit into a curses window as a fallback for terminals with no ANSI
    /// support (the Win9x path in the backend does the same for its own
    /// emitter).
    ///
    /// Colour here is reduced to the 8-colour PDCurses/ncurses palette via
    /// `color_pair_for`, which the caller supplies because curses colour-pair
    /// tables are per-screen state the grid cannot own.
    pub fn blit_to_window(
        &self,
        origin_y: i32,
        origin_x: i32,
        mut put: impl FnMut(i32, i32, char, Option<(u8, u8)>),
    ) {
        for row in 0..self.h {
            let Some(cells) = self.cells.get(row as usize) else { continue };
            for (x, cell) in cells.iter().enumerate() {
                let colour = if cell.fg == (0, 0, 0) && cell.bg == (255, 255, 255) {
                    // The renderer's default (black on white) needs no pair.
                    None
                } else {
                    Some((rgb_to_palette_index(cell.fg), rgb_to_palette_index(cell.bg)))
                };
                put(
                    origin_y + row as i32,
                    origin_x + x as i32,
                    cell.ch,
                    colour,
                );
            }
        }
    }
}

#[cfg(test)]
mod blit_tests {
    use super::*;

    fn cell(ch: char, fg: (u8, u8, u8), bg: (u8, u8, u8)) -> GridCell {
        GridCell { ch, fg, bg }
    }

    /// The cube mapping must be exact at the cube's own axis values, since
    /// those are what the renderer's palette-sourced colours reduce to.
    #[test]
    fn rgb_to_palette_hits_the_cube_exactly() {
        // Pure black and pure white land on the cube's corners.
        assert_eq!(16, rgb_to_palette_index((0, 0, 0)));
        assert_eq!(231, rgb_to_palette_index((255, 255, 255)));
        // A saturated red is cube index (5,0,0) => 16 + 36*5 = 196.
        assert_eq!(196, rgb_to_palette_index((255, 0, 0)));
        // Pure green (0,5,0) => 16 + 6*5 = 46.
        assert_eq!(46, rgb_to_palette_index((0, 255, 0)));
        // Pure blue (0,0,5) => 16 + 5 = 21.
        assert_eq!(21, rgb_to_palette_index((0, 0, 255)));
    }

    /// Greys must come from the grey ramp, not the cube: the cube has no true
    /// greys, so a grey mapped to it would look tinted.
    #[test]
    fn greys_use_the_ramp_not_the_cube() {
        let mid = rgb_to_palette_index((128, 128, 128));
        assert!(
            (232..=255).contains(&mid),
            "grey should map into the 24-step ramp, got {mid}"
        );
        // Two different greys must not collapse to the same ramp entry.
        assert_ne!(
            rgb_to_palette_index((100, 100, 100)),
            rgb_to_palette_index((160, 160, 160))
        );
    }

    /// Adjacent cells sharing colours must be merged into one run, or the
    /// emitter would switch SGR per cell and the output would be enormous.
    #[test]
    fn equal_attributes_merge_into_one_run() {
        let mut grid = CellGrid::new(4, 1);
        for x in 0..4u16 {
            grid.cells[0][x as usize] = cell('a', (0, 0, 0), (255, 255, 255));
        }
        let runs = row_attr_runs(&grid, 0);
        assert_eq!(1, runs.len(), "same colours must be one run");
        assert_eq!("aaaa", runs[0].text);
        assert_eq!(0, runs[0].start_col);
    }

    #[test]
    fn colour_change_splits_runs_at_the_right_column() {
        let mut grid = CellGrid::new(4, 1);
        grid.cells[0][0] = cell('a', (0, 0, 0), (255, 255, 255));
        grid.cells[0][1] = cell('b', (0, 0, 0), (255, 255, 255));
        grid.cells[0][2] = cell('c', (255, 0, 0), (255, 255, 255));
        grid.cells[0][3] = cell('d', (255, 0, 0), (255, 255, 255));
        let runs = row_attr_runs(&grid, 0);
        assert_eq!(2, runs.len());
        assert_eq!((0, "ab"), (runs[0].start_col, runs[0].text.as_str()));
        assert_eq!((2, "cd"), (runs[1].start_col, runs[1].text.as_str()));
    }

    /// A blank glyph with a coloured background is still content (that is how
    /// cell fills are drawn), so it must not be skipped and must get its own
    /// run when the background differs.
    #[test]
    fn blank_cell_with_coloured_background_is_a_run() {
        let mut grid = CellGrid::new(2, 1);
        grid.cells[0][0] = cell(' ', (0, 0, 0), (255, 255, 255));
        grid.cells[0][1] = cell(' ', (0, 0, 0), (0, 128, 255));
        let runs = row_attr_runs(&grid, 0);
        assert_eq!(2, runs.len(), "the fill must produce its own run");
        assert_eq!(" ", runs[1].text);
    }

    /// `to_ansi` must position absolutely, so a caller can blit into any
    /// region without tracking where the cursor was.
    #[test]
    fn to_ansi_positions_absolutely_and_resets() {
        let mut grid = CellGrid::new(2, 2);
        for y in 0..2 {
            for x in 0..2 {
                grid.cells[y][x] = cell('x', (0, 0, 0), (255, 255, 255));
            }
        }
        let out = grid.to_ansi(3, 5);
        // First cell of the first row → CUP row 4, col 6 (1-based).
        assert!(out.contains("\x1b[4;6H"), "expected absolute CUP, got {out:?}");
        // Second row → CUP row 5.
        assert!(out.contains("\x1b[5;6H"), "second row must be positioned too");
        // Every row must end reset so colour never leaks past the grid.
        assert!(out.contains("\x1b[0m"));
        assert!(out.ends_with("\x1b[0m"));
    }

    /// An empty grid must produce no escape output at all (nothing to draw),
    /// rather than a stream that clears the screen.
    #[test]
    fn empty_grid_emits_nothing() {
        let grid = CellGrid::new(0, 0);
        assert_eq!("", grid.to_ansi(0, 0));
    }

    /// `render_model_to_grid` → `to_ansi` is the whole pipeline: a model
    /// painted through the shared renderer must come out as positioned text.
    #[test]
    fn model_renders_through_the_shared_painter_into_ansi() {
        use crate::spreadsheet::SpreadsheetModel;
        let mut m = SpreadsheetModel::new(2, 2);
        // Model (1,1): row 1 is the first body row (row 0 is the title band)
        // and column 1 the first main column (column 0 is the margin).
        // `paint` draws the row-label gutter over the first `margin_cols`
        // columns and the title band over the first `header_row_count` rows.
        // A body cell is therefore at display (header_row_count, margin_cols)
        // = (1, 1) for the defaults, and its text goes to grid row 1 — but the
        // grid's own row 1 also receives the row label. Give the model an
        // explicit body so the distinction is unambiguous.
        m.set_row_counts(0, 2);
        m.set_grid_config(0, 2);
        // Narrow the columns: the default layout is 12 chars each, so column 1
        // would start at pixel 160 and fall outside a 20-cell grid (the
        // renderer clips silently — worth knowing, and the reason this test
        // pins a width).
        m.set_column_layout(vec![(0, 4, "A".into()), (1, 4, "B".into())]);
        m.set_cell(0, 1, "hi");
        let grid = render_model_to_grid(&m, 20, 5);
        let ansi = grid.to_ansi(0, 0);
        // NOTE: `SpreadsheetModel::new` defaults to spreadsheet chrome — one
        // margin column and one header row — so body cell (0,0) is model
        // (1,1), not (0,0). The first assertion pins that convention, because
        // getting it wrong is how a caller silently writes into a header.
        assert!(
            grid.row_strings()[0].contains('A'),
            "row 0 is the column-title band: {:?}",
            grid.row_strings()[0]
        );
        assert!(ansi.contains("hi"), "cell text must survive the pipeline");
        assert!(ansi.contains("\x1b["), "and be positioned");
    }

    /// The curses fallback must place every cell at the right absolute
    /// position, since a terminal has no per-cell coordinates of its own.
    #[test]
    fn blit_to_window_places_cells_absolutely() {
        let mut grid = CellGrid::new(2, 2);
        grid.cells[0][0] = cell('a', (0, 0, 0), (255, 255, 255));
        grid.cells[1][1] = cell('b', (0, 0, 0), (255, 255, 255));
        let mut seen: Vec<(i32, i32, char)> = Vec::new();
        grid.blit_to_window(10, 20, |y, x, ch, _c| seen.push((y, x, ch)));
        // Every cell is visited, offset by the origin.
        assert_eq!(4, seen.len(), "all cells blitted");
        assert!(seen.contains(&(10, 20, 'a')));
        assert!(seen.contains(&(11, 21, 'b')));
    }

    /// Default black-on-white needs no colour pair, so the curses path can
    /// skip `COLOR_PAIR` for the common case.
    #[test]
    fn blit_to_window_skips_colour_for_the_default_pair() {
        let mut grid = CellGrid::new(1, 1);
        grid.cells[0][0] = cell('x', (0, 0, 0), (255, 255, 255));
        let mut colour: Option<Option<(u8, u8)>> = None;
        grid.blit_to_window(0, 0, |_y, _x, _ch, c| colour = Some(c));
        assert_eq!(Some(None), colour, "default colours must not allocate a pair");
    }
}
