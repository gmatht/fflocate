//! The list's viewport: where its rows land for a given viewport height and
//! scroll offset, how far the scroller's thumb has travelled, and what the
//! painter puts inside each row's band.
//!
//! A child module of [`super`], so the geometry is derived from the painter
//! (`compat::paint_list`) and the row constants it draws with, and not from a
//! second copy of the layout arithmetic. A probe or a test can therefore paint
//! the list offscreen and assert where the rows went, on any backend.

use super::{paint_list, CellValue, TreeView, HEADER_HEIGHT, ROW_HEIGHT};
use crate::backends::headless::{DrawOp, RecordingDrawContext};

/// The shortest a scroller's thumb is allowed to get, in points.
const MIN_THUMB: f64 = 12.0;

/// Where the list's rows, header and scroller sit for one viewport.
///
/// Everything here is in viewport coordinates: the header band is at `0` while
/// the view is scrolled to the top, and `row_rects` are the visible rows only.
#[derive(Clone, Debug, PartialEq)]
pub struct ListGeometry {
    /// The width the list was laid out for (its canvas' width).
    pub width: f64,
    /// The height of the viewport onto the list (the scrolled window's page).
    pub viewport_height: f64,
    /// How far down the content the viewport starts.
    pub offset: f64,
    pub header_height: f64,
    pub row_height: f64,
    /// The full height of the rows, header included.
    pub content_height: f64,
    /// The first row that is at least partly visible.
    pub first_row: usize,
    /// How many rows are at least partly visible.
    pub visible_rows: usize,
    /// `(top, height)` of each visible row, in viewport coordinates.
    pub row_rects: Vec<(f64, f64)>,
    /// The scroller's track along the viewport's edge.
    pub track_height: f64,
    /// Where the thumb starts inside the track.
    pub thumb_top: f64,
    /// How tall the thumb is: the fraction of the content on screen.
    pub thumb_height: f64,
}

impl ListGeometry {
    /// The rows that are at least partly visible, in content order.
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        self.first_row..self.first_row + self.visible_rows
    }
}

/// One offscreen paint of the list: the geometry, and the number of draw
/// operations the painter put inside each row's band.
///
/// The painter draws every row in content coordinates, so `ink[i]` is the ink
/// of row `i` whether or not it is on screen; [`ListGeometry::visible_range`]
/// says which of those the viewport actually shows.
#[derive(Clone, Debug)]
pub struct ListRender {
    pub geometry: ListGeometry,
    /// Draw operations inside the header band.
    pub header_ink: usize,
    /// Draw operations inside each row's band, one entry per row.
    pub ink: Vec<usize>,
    /// The `x` each header title was drawn at.
    pub header_xs: Vec<f64>,
}

/// The scroller's knob inside its track: `(track_height, thumb_top, thumb_height)`.
///
/// The thumb is the viewport's share of the content, never shorter than
/// [`MIN_THUMB`], and it travels the rest of the track as the offset runs from
/// the top of the content to its end — the way a real scrollbar behaves.
fn scroll_thumb(content_height: f64, viewport_height: f64, offset: f64) -> (f64, f64, f64) {
    let track = viewport_height.max(0.0);
    if track <= 0.0 || content_height <= track {
        return (track, 0.0, track);
    }
    let thumb = (track * track / content_height).max(MIN_THUMB).min(track);
    let travel = track - thumb;
    let max_offset = content_height - track;
    let top = if max_offset > 0.0 {
        (offset.clamp(0.0, max_offset) / max_offset) * travel
    } else {
        0.0
    };
    (track, top, thumb)
}

/// Which band a draw operation at `top` belongs to.
enum Band {
    Header,
    Row(usize),
    Outside,
}

fn band_of(top: f64, rows: usize) -> Band {
    if top < HEADER_HEIGHT {
        return Band::Header;
    }
    let row = ((top - HEADER_HEIGHT) / ROW_HEIGHT).floor();
    if row < 0.0 {
        return Band::Header;
    }
    let row = row as usize;
    if row < rows {
        Band::Row(row)
    } else {
        Band::Outside
    }
}

impl TreeView {
    /// The full height of the rows, header included: what the canvas is sized
    /// to, and what the scroller measures its thumb against.
    pub fn content_height(&self) -> f64 {
        HEADER_HEIGHT + self.rows.borrow().len() as f64 * ROW_HEIGHT
    }

    /// Paint the list into a recording context for a viewport `viewport_height`
    /// tall, `width` wide, scrolled to `offset`, and report where everything
    /// landed and how much the painter drew inside each row.
    ///
    /// The painter is the one the canvas runs, so this is the layout the list
    /// really draws, measured offscreen.
    pub fn render_offscreen(&self, width: f64, viewport_height: f64, offset: f64) -> ListRender {
        let rows = self.rows.borrow();
        let mut dc = RecordingDrawContext::new();
        paint_list(
            &mut dc,
            width,
            &self.columns,
            &rows,
            self.selected.get(),
            self.sort.get(),
        );

        let mut header_ink = 0usize;
        let mut header_xs: Vec<f64> = Vec::new();
        let mut ink = vec![0usize; rows.len()];
        for op in &dc.ops {
            let (top, x, text) = match *op {
                DrawOp::Text {
                    x,
                    y,
                    ref text,
                    ..
                } => (y, Some(x), Some(text.as_str())),
                DrawOp::FillRect { x, y, .. } | DrawOp::StrokeRect { x, y, .. } => {
                    (y, Some(x), None)
                }
                _ => continue,
            };
            match band_of(top, rows.len()) {
                Band::Header => {
                    header_ink += 1;
                    if let (Some(x), Some(text)) = (x, text) {
                        if !text.is_empty() {
                            header_xs.push(x);
                        }
                    }
                }
                Band::Row(i) => ink[i] += 1,
                Band::Outside => {}
            }
        }

        let content_height = HEADER_HEIGHT + rows.len() as f64 * ROW_HEIGHT;
        let first_row = if offset <= HEADER_HEIGHT {
            0
        } else {
            (((offset - HEADER_HEIGHT) / ROW_HEIGHT).floor() as usize).min(rows.len())
        };
        let mut row_rects = Vec::new();
        let mut row = first_row;
        while row < rows.len() {
            let top = HEADER_HEIGHT + row as f64 * ROW_HEIGHT - offset;
            if top >= viewport_height {
                break;
            }
            row_rects.push((top, ROW_HEIGHT));
            row += 1;
        }
        let (track_height, thumb_top, thumb_height) =
            scroll_thumb(content_height, viewport_height, offset);

        ListRender {
            geometry: ListGeometry {
                width,
                viewport_height,
                offset,
                header_height: HEADER_HEIGHT,
                row_height: ROW_HEIGHT,
                content_height,
                first_row,
                visible_rows: row_rects.len(),
                row_rects,
                track_height,
                thumb_top,
                thumb_height,
            },
            header_ink,
            ink,
            header_xs,
        }
    }

    /// The list's geometry for a viewport, without keeping the paint.
    pub fn layout(&self, width: f64, viewport_height: f64, offset: f64) -> ListGeometry {
        self.render_offscreen(width, viewport_height, offset)
            .geometry
    }
}
