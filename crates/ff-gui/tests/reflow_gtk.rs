//! Resize reflow, the scroll offset and per-row ink, measured offscreen on
//! GTK: the Linux counterpart to the macOS probe's resize/scroll/pixel block.
//!
//! The numbers come from `compat::reflow`, which paints the list into a
//! recording draw context with the painter the canvas itself runs, so the row
//! rects, the visible row count, the header positions and the per-row ink are
//! the list's own layout — not a copy of it. No window is opened and nothing
//! is compared against a stored image.
//!
//! Creating the list builds a GTK canvas, so this needs a display; CI runs it
//! under Xvfb.

#![cfg(unix)]

use rswidgets::compat::reflow::{ListGeometry, ListRender};
use rswidgets::compat::{CellValue, TreeView};

/// More rows than any viewport here shows, so scrolling and shrinking have
/// something to hide.
const ROWS: usize = 40;

fn rows(n: usize) -> Vec<Vec<CellValue>> {
    (0..n)
        .map(|i| {
            vec![
                CellValue::Icon {
                    name: "text-x-generic".to_string(),
                    fallback: "-".to_string(),
                },
                CellValue::Text(format!("file-{i:02}.rs")),
                CellValue::Text(format!("/tmp/fx/dir{}/file-{i:02}.rs", i % 7)),
            ]
        })
        .collect()
}

/// How many rows fit in a viewport, from the list's own band sizes.
fn fitting(geo: &ListGeometry) -> usize {
    (((geo.viewport_height - geo.header_height) / geo.row_height).ceil()).max(0.0) as usize
}

#[test]
fn resizing_reflows_and_scrolling_moves_the_visible_rows() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("reflow: no display, skipping");
        return;
    }
    let Ok(_app) = rswidgets::core::App::init() else {
        eprintln!("reflow: GTK did not initialise, skipping");
        return;
    };
    let list = match TreeView::new(&[("", true, false), ("Name", false, true), ("Path", false, true)])
    {
        Ok(list) => list,
        Err(e) => {
            eprintln!("reflow: no results list ({e}), skipping");
            return;
        }
    };
    list.set_rows(&rows(ROWS));

    // --- Grown, then shrunk: the visible rows follow the viewport ---
    let wide = 1000.0;
    let grown: ListRender = list.render_offscreen(wide, 600.0, 0.0);
    let shrunk: ListRender = list.render_offscreen(wide, 180.0, 0.0);

    let g: &ListGeometry = &grown.geometry;
    let s: &ListGeometry = &shrunk.geometry;
    assert_eq!(
        g.content_height,
        g.header_height + ROWS as f64 * g.row_height,
        "the content height is not the header plus {} rows",
        ROWS
    );
    assert_eq!(
        s.content_height, g.content_height,
        "shrinking the viewport changed the content height: the rows were not the same"
    );
    assert_eq!(
        g.visible_rows,
        fitting(g),
        "a 600pt viewport shows {} rows, not the {} that fit",
        g.visible_rows,
        fitting(g)
    );
    assert_eq!(
        s.visible_rows,
        fitting(s),
        "a 180pt viewport shows {} rows, not the {} that fit",
        s.visible_rows,
        fitting(s)
    );
    assert!(
        s.visible_rows < g.visible_rows,
        "shrinking to 180pt still shows {} rows (600pt showed {}): the list did not reflow",
        s.visible_rows,
        g.visible_rows
    );
    assert_eq!(
        s.row_rects.len(),
        s.visible_rows,
        "the row rects do not match the visible row count after shrinking"
    );
    for (i, (top, height)) in s.row_rects.iter().enumerate() {
        assert_eq!(*height, s.row_height, "row {i} is {height}pt tall, not one band");
        assert_eq!(
            *top,
            s.header_height + i as f64 * s.row_height,
            "row {i} does not start where its band does"
        );
    }

    // The knob: the thumb is the viewport's share of the content, so a shorter
    // viewport has a shorter thumb, and both stay inside the track.
    assert!(
        s.thumb_height < g.thumb_height,
        "the thumb is {}pt in a 180pt viewport and {}pt in a 600pt one",
        s.thumb_height,
        g.thumb_height
    );
    for geo in [g, s] {
        assert!(
            geo.thumb_height > 0.0 && geo.thumb_height <= geo.track_height,
            "the thumb is {}pt in a {}pt track",
            geo.thumb_height,
            geo.track_height
        );
        assert_eq!(geo.track_height, geo.viewport_height, "the track is not the viewport");
    }

    // The columns follow the width: the header titles spread out with it.
    let narrow = list.render_offscreen(700.0, 600.0, 0.0);
    assert!(
        grown.header_xs.len() >= 2 && narrow.header_xs.len() >= 2,
        "the header painted {} titles wide and {} narrow",
        grown.header_xs.len(),
        narrow.header_xs.len()
    );
    let spread = |r: &ListRender| r.header_xs[r.header_xs.len() - 1] - r.header_xs[0];
    assert!(
        spread(&grown) > spread(&narrow),
        "the header spans {}pt at 1000pt wide and {}pt at 700pt: the columns did not stretch",
        spread(&grown),
        spread(&narrow)
    );

    // --- Per-row ink from the rows the list was given ---
    assert_eq!(
        grown.ink.len(),
        ROWS,
        "the render reported ink for {} rows, not {}",
        grown.ink.len(),
        ROWS
    );
    for i in grown.geometry.visible_range() {
        assert!(
            grown.ink[i] > 0,
            "row {i} is on screen but the painter put no ink in it"
        );
    }
    assert!(
        grown.header_ink > 0,
        "the header band has no ink at all"
    );

    // An empty list has no rows to ink and nothing to scroll.
    list.set_rows(&[]);
    let empty = list.render_offscreen(wide, 180.0, 0.0);
    assert_eq!(
        empty.geometry.content_height, empty.geometry.header_height,
        "an empty list is taller than its header"
    );
    assert_eq!(empty.geometry.visible_rows, 0, "an empty list has visible rows");
    assert!(empty.ink.is_empty(), "an empty list reports row ink");
    assert!(
        (empty.geometry.thumb_height - empty.geometry.track_height).abs() < 1e-9,
        "an empty list's thumb is {}pt in a {}pt track: nothing left to scroll",
        empty.geometry.thumb_height,
        empty.geometry.track_height
    );
    list.set_rows(&rows(ROWS));

    // --- Scrolling moves the visible window, and its ink with it ---
    let top = list.render_offscreen(wide, 180.0, 0.0);
    let scrolled = list.render_offscreen(wide, 180.0, 62.0);
    assert_eq!(
        top.geometry.first_row, 0,
        "the list does not start at its first row"
    );
    assert_eq!(
        scrolled.geometry.first_row, 2,
        "a 62pt offset (the header plus two 20pt bands) starts at row {}, not row 2",
        scrolled.geometry.first_row
    );
    assert_ne!(
        top.geometry.visible_range(),
        scrolled.geometry.visible_range(),
        "scrolling did not change which rows are visible"
    );
    assert_eq!(
        scrolled.geometry.row_rects[0].0, 0.0,
        "the first visible row of a 62pt offset does not start at the viewport's top"
    );
    assert!(
        scrolled.geometry.thumb_top > top.geometry.thumb_top,
        "the thumb did not move down with the content: {} then {}",
        top.geometry.thumb_top,
        scrolled.geometry.thumb_top
    );
    for i in scrolled.geometry.visible_range() {
        assert!(
            scrolled.ink[i] > 0,
            "row {i} is visible after scrolling but has no ink"
        );
        assert_eq!(
            scrolled.ink[i], top.ink[i],
            "row {i}'s ink changed with the scroll offset: the row was repainted differently"
        );
    }

    // The scrolled window is the widget whose adjustment a knob drag moves;
    // driving it keeps the same terms a drag does.
    let content = scrolled.geometry.content_height;
    if let Ok(sw) = list.clone().into_scrolled() {
        sw.scroll_to(0.0, 0.0, 0.0, 62.0, content, 180.0);
    }

    println!(
        "reflow: 1000x600 shows {} row(s) of {ROWS}; 700x180 shows {}; 1000x180 scrolled 62pt starts at row {} (was {})",
        g.visible_rows, s.visible_rows, scrolled.geometry.first_row, top.geometry.first_row
    );
    println!(
        "reflow: thumb {}pt -> {}pt of a {}pt track (grow/shrink), then at {}pt when scrolled; header spans {:.0}pt wide, {:.0}pt narrow",
        g.thumb_height, s.thumb_height, s.track_height, scrolled.geometry.thumb_top, spread(&grown), spread(&narrow)
    );
    println!(
        "reflow: ink per visible row {:?} (empty list: {} row(s), header ink {}); rows {}..{} at offset 62",
        &scrolled.ink[scrolled.geometry.visible_range()],
        empty.geometry.visible_rows,
        empty.header_ink,
        scrolled.geometry.visible_range().start,
        scrolled.geometry.visible_range().end
    );
}
