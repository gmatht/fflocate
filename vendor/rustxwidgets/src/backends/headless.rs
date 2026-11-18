//! Headless `DrawContext` implementation for testing and behaviour comparison.
//!
//! It does not rasterise anything; it records every draw call into an ordered
//! list of [`DrawOp`]s. This lets parity tests assert *what* a backend paints
//! (which text lands in which cell/region, which rects are filled/stroked,
//! clear colour) without needing a display server. It uses the same pixel-based
//! coordinate contract as the cairo/`GtkDrawContext` so it is a drop-in for
//! `Spreadsheet::paint`.

use crate::core::DrawContext;

/// A single recorded drawing operation (pixel coordinates, like every other backend).
#[derive(Clone, Debug, PartialEq)]
pub enum DrawOp {
    Clear(f64, f64, f64, f64),
    FillRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        rgba: (f64, f64, f64, f64),
    },
    StrokeRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        rgba: (f64, f64, f64, f64),
        lw: f64,
    },
    Line {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        rgba: (f64, f64, f64, f64),
        lw: f64,
    },
    Circle {
        cx: f64,
        cy: f64,
        r: f64,
        rgba: (f64, f64, f64, f64),
        lw: f64,
    },
    Text {
        x: f64,
        y: f64,
        text: String,
        rgba: (f64, f64, f64, f64),
    },
    /// Styled text: same payload plus Pango slant/weight (0=normal, 1=bold).
    /// Recorded IN ADDITION to `Text` so existing `texts()` consumers keep
    /// seeing every string exactly as before.
    StyledText {
        x: f64,
        y: f64,
        text: String,
        rgba: (f64, f64, f64, f64),
        slant: i32,
        weight: i32,
    },
    /// A blit of a straight-alpha RGBA8 image, at `(x, y)` and scaled.
    ///
    /// Recorded with the source geometry, so a test can assert both that a
    /// blit happened and where it landed. The `r,g,b,a` fields are the
    /// four channels the loader derived from the image, carried alongside the
    /// samples so a caller can match an op without re-deriving them.
    Image {
        x: f64,
        y: f64,
        /// Source width in pixels, before `scale`.
        src_w: u32,
        /// Source height in pixels, before `scale`.
        src_h: u32,
        /// The `scale` argument, verbatim.
        scale: f64,
        /// RGBA8, row-major, 4 bytes per pixel, straight (not premultiplied)
        /// alpha — exactly the buffer the caller passed in.
        rgba: Vec<u8>,
    },
}

/// A `DrawContext` that records operations for later inspection.
pub struct RecordingDrawContext {
    pub ops: Vec<DrawOp>,
}

impl RecordingDrawContext {
    pub fn new() -> Self {
        RecordingDrawContext { ops: Vec::new() }
    }

    /// All `Text` payloads in draw order.
    pub fn texts(&self) -> Vec<&str> {
        self.ops
            .iter()
            .filter_map(|o| if let DrawOp::Text { text, .. } = o { Some(text.as_str()) } else { None })
            .collect()
    }

    /// True if any drawn text contains `needle`.
    pub fn has_text(&self, needle: &str) -> bool {
        self.texts().iter().any(|t| t.contains(needle))
    }

    /// All `FillRect` ops (useful for asserting backgrounds/regions).
    pub fn fill_rects(&self) -> Vec<FillRect> {
        self.ops
            .iter()
            .filter_map(|o| match o {
                DrawOp::FillRect { x, y, w, h, rgba } => Some(FillRect {
                    x: *x,
                    y: *y,
                    w: *w,
                    h: *h,
                    rgba: *rgba,
                }),
                _ => None,
            })
            .collect()
    }

    /// All `Image` blits, in draw order.
    pub fn images(&self) -> Vec<&DrawOp> {
        self.ops.iter().filter(|o| matches!(o, DrawOp::Image { .. })).collect()
    }
}

impl Default for RecordingDrawContext {
    fn default() -> Self {
        Self::new()
    }
}

impl DrawContext for RecordingDrawContext {
    fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, a: f64) {
        self.ops.push(DrawOp::FillRect {
            x,
            y,
            w,
            h,
            rgba: (r, g, b, a),
        });
    }
    fn stroke_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, g: f64, b: f64, a: f64, lw: f64) {
        self.ops.push(DrawOp::StrokeRect {
            x,
            y,
            w,
            h,
            rgba: (r, g, b, a),
            lw,
        });
    }
    fn draw_text_styled(&mut self, x: f64, y: f64, text: &str, _f: &str, _s: f64, r: f64, g: f64, b: f64, a: f64, sl: i32, w: i32) {
        self.ops.push(DrawOp::Text {
            x,
            y,
            text: text.to_string(),
            rgba: (r, g, b, a),
        });
        self.ops.push(DrawOp::StyledText {
            x,
            y,
            text: text.to_string(),
            rgba: (r, g, b, a),
            slant: sl,
            weight: w,
        });
    }
    fn text_extents_styled(&self, text: &str, _f: &str, _s: f64, _sl: i32, _w: i32) -> (f64, f64, f64, f64) {
        // Approximate: CHAR_W is 8px, one line tall.
        (0.0, 0.0, text.chars().count() as f64 * 8.0, 16.0)
    }
    fn clear(&mut self, r: f64, g: f64, b: f64, a: f64) {
        self.ops.push(DrawOp::Clear(r, g, b, a));
    }
    fn save(&mut self) {}
    fn restore(&mut self) {}
    fn clip(&mut self, _x: f64, _y: f64, _w: f64, _h: f64) {}

    /// Honour an image blit by recording it.
    ///
    /// The trait default does nothing and returns `false`, and six of the
    /// eight backends take it — a caller that ignores the result then draws
    /// nothing where GTK would have drawn the image, with no error. A recorder
    /// has no reason to refuse: it does not rasterise, it *records*, and
    /// recording the blit is the whole of what it does for every other
    /// primitive. So it records the op and returns `true`, which is the honest
    /// answer — the samples really are in `ops` afterwards.
    ///
    /// The one case it does refuse is a `pixels` buffer too short for
    /// `width * height * 4`. There the blit genuinely cannot be recorded
    /// faithfully, and returning `true` would be the "promised a draw that
    /// never happened" answer the trait's contract is written to prevent.
    fn draw_rgba_image(
        &mut self,
        x: f64,
        y: f64,
        pixels: &[u8],
        width: u32,
        height: u32,
        scale: f64,
    ) -> bool {
        let need = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        if need > pixels.len() {
            return false;
        }
        self.ops.push(DrawOp::Image {
            x,
            y,
            src_w: width,
            src_h: height,
            scale,
            rgba: pixels[..need].to_vec(),
        });
        true
    }
}

/// Owned view of a `FillRect` op.
pub struct FillRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub rgba: (f64, f64, f64, f64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_line_and_circle_ops() {
        let mut dc = RecordingDrawContext::new();
        dc.draw_line(0.0, 0.0, 10.0, 10.0, 1.0, 0.0, 0.0, 1.0, 1.0);
        dc.draw_circle(5.0, 5.0, 3.0, 0.0, 0.0, 1.0, 1.0, 1.0);
        assert_eq!(dc.ops.len(), 2);
        assert_eq!(dc.ops[0], DrawOp::Line { x1: 0.0, y1: 0.0, x2: 10.0, y2: 10.0, rgba: (1.0, 0.0, 0.0, 1.0), lw: 1.0 });
        assert_eq!(dc.ops[1], DrawOp::Circle { cx: 5.0, cy: 5.0, r: 3.0, rgba: (0.0, 0.0, 1.0, 1.0), lw: 1.0 });
    }

    #[test]
    fn image_blit_is_recorded_with_its_geometry() {
        let mut dc = RecordingDrawContext::new();
        // Two pixels: opaque red, half-transparent green.
        let px: Vec<u8> = vec![255, 0, 0, 255, 0, 255, 0, 128];
        assert!(dc.draw_rgba_image(3.0, 4.0, &px, 2, 1, 2.0));
        assert_eq!(dc.images().len(), 1);
        match &dc.ops[0] {
            DrawOp::Image { x, y, src_w, src_h, scale, rgba } => {
                assert_eq!((*x, *y), (3.0, 4.0));
                assert_eq!((*src_w, *src_h), (2, 1));
                assert_eq!(*scale, 2.0);
                // The samples must survive verbatim, alpha included: a
                // recorder that normalised or dropped the alpha byte would
                // make a translucent-edge assertion vacuous.
                assert_eq!(rgba, &px);
            }
            other => panic!("expected an Image op, got {other:?}"),
        }
    }

    #[test]
    fn image_blit_refuses_a_short_buffer() {
        let mut dc = RecordingDrawContext::new();
        // 2x2 needs 16 bytes; hand it 8.
        let px: Vec<u8> = vec![0; 8];
        assert!(!dc.draw_rgba_image(0.0, 0.0, &px, 2, 2, 1.0));
        assert!(
            dc.ops.is_empty(),
            "a refused blit must not record an op, or it would promise a draw \
             it never performed"
        );
    }

    #[test]
    fn a_zero_sized_image_is_a_valid_blit() {
        let mut dc = RecordingDrawContext::new();
        assert!(dc.draw_rgba_image(1.0, 2.0, &[], 0, 0, 1.0));
        assert_eq!(dc.images().len(), 1);
    }
}

impl RecordingDrawContext {
    /// Record a line op (inherent: `DrawContext` has no line primitive; the
    /// GUI padlock shackle is drawn from rects instead, so this stays as a
    /// recorder-side helper rather than a trait method).
    pub fn draw_line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, r: f64, g: f64, b: f64, a: f64, lw: f64) {
        self.ops.push(DrawOp::Line { x1, y1, x2, y2, rgba: (r, g, b, a), lw });
    }
    /// Record a circle op (inherent; see `draw_line`).
    pub fn draw_circle(&mut self, cx: f64, cy: f64, r: f64, red: f64, green: f64, blue: f64, alpha: f64, lw: f64) {
        self.ops.push(DrawOp::Circle { cx, cy, r, rgba: (red, green, blue, alpha), lw });
    }
}
