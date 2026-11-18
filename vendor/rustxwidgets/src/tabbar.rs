//! A reusable sheet-tab strip widget built on top of [`Canvas`].
//!
//! `TabBar` owns the layout, hit-testing, drag-reorder state machine, and
//! rendering of a horizontal row of selectable, reorderable tabs.  The app
//! feeds it titles and an active index; it fires callbacks when the user
//! selects a tab, drags one to a new position, or right-clicks for a context
//! menu.
//!
//! The widget is backend-agnostic: it draws through [`DrawContext`] and
//! receives pointer events through the [`Canvas`] callbacks, so it works on
//! every rswidgets backend that implements `Canvas` (GTK, NWG, …).
//!
//! # Usage
//!
//! ```ignore
//! let canvas = rxapp.new_canvas()?;
//! let tabbar = TabBar::new(canvas, TabBarConfig::default());
//! tabbar.on_select(Box::new(|idx| { /* switch sheet */ }));
//! tabbar.on_reorder(Box::new(|from, to| { /* move sheet */ }));
//! tabbar.set_tabs(&["Sheet1".into(), "Sheet2".into()], 0);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use crate::common::Canvas;
use crate::core::DrawContext;

// ── Configuration ──────────────────────────────────────────────────────────

/// Visual and behavioural configuration for a [`TabBar`].
///
/// Every field has a sensible default so `TabBarConfig::default()` is usable
/// without any setup.  The corro app overrides colours and font to match its
/// chrome.
#[derive(Clone, Debug)]
pub struct TabBarConfig {
    /// Height of the tab strip in device pixels.
    pub height: f64,
    /// Active-tab background fill (r, g, b).
    pub active_bg: (f64, f64, f64),
    /// Inactive-tab background fill.
    pub idle_bg: (f64, f64, f64),
    /// Tab divider line colour.
    pub divider: (f64, f64, f64),
    /// Drag-caret colour.
    pub caret: (f64, f64, f64),
    /// Background colour for the strip (cleared before painting).
    pub strip_bg: (f64, f64, f64),
    /// Active-tab text colour.
    pub active_text: (f64, f64, f64),
    /// Inactive-tab text colour.
    pub idle_text: (f64, f64, f64),
    /// Horizontal padding inside each tab (device px).
    pub pad_x: f64,
    /// Gap between tabs (device px).
    pub gap: f64,
    /// Font family name passed to DrawContext.
    pub font_family: String,
    /// Font size in device pixels.
    pub font_size: f64,
    /// Pointer travel (device px) before a press becomes a drag.
    pub drag_threshold: f64,
}

impl Default for TabBarConfig {
    fn default() -> Self {
        // Deliberately NOT `TabBarConfig::for_theme`: the historical defaults
        // (yellow active tab, 0.9 grey strip) predate theming and differ from
        // the shared `Role` palette. Changing them here would silently
        // restyle every existing app, so `default()` stays as it was and a
        // theming app opts in explicitly via `for_theme`.
        TabBarConfig {
            height: 24.0,
            active_bg: (1.0, 1.0, 0.6),
            idle_bg: (0.9, 0.9, 0.9),
            divider: (0.55, 0.55, 0.55),
            caret: (0.1, 0.35, 0.9),
            strip_bg: (0.94, 0.94, 0.94),
            active_text: (0.0, 0.0, 0.0),
            idle_text: (0.3, 0.3, 0.3),
            pad_x: 10.0,
            gap: 6.0,
            font_family: "monospace".into(),
            font_size: 13.0,
            drag_threshold: 4.0,
        }
    }
}

impl TabBarConfig {
    /// A config whose colours come from the ambient [`crate::core::theme`].
    ///
    /// Use this (and rebuild the config when the scheme changes) instead of
    /// `default()` in an app that participates in night mode: it is the only
    /// way a `TabBar` follows the global palette, since the struct is a plain
    /// value copied at construction time and cannot read the theme per frame.
    /// Non-colour fields are the defaults.
    pub fn for_theme() -> Self {
        use crate::core::Role;
        let t = crate::core::theme();
        let tab = |role: Role| {
            let c = t.color(role);
            (c.r, c.g, c.b)
        };
        Self {
            active_bg: tab(Role::TabActive),
            idle_bg: tab(Role::TabIdle),
            divider: tab(Role::Gridline),
            caret: tab(Role::TextAccent),
            strip_bg: tab(Role::TabStrip),
            active_text: tab(Role::Text),
            idle_text: tab(Role::TextMuted),
            ..Self::default()
        }
    }
}

// ── Internal types ─────────────────────────────────────────────────────────

/// A painted tab from the last frame: strip position plus its index.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TabHit {
    x0: f64,
    x1: f64,
    index: usize,
}

/// An in-progress drag of a tab.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TabDrag {
    from: usize,
    to: usize,
    press_x: f64,
    moved: bool,
}

impl TabDrag {
    fn index_in_range(&self, len: usize) -> bool {
        len > 0 && self.from < len && self.to <= len
    }
}

// ── Shared state ───────────────────────────────────────────────────────────

struct TabBarState {
    titles: Vec<String>,
    active: usize,
    hits: Vec<TabHit>,
    drag: Option<TabDrag>,
    /// Pending right-click x position (armed on press, consumed on release).
    context_pending: Option<f64>,
    on_select: Option<Box<dyn FnMut(usize)>>,
    on_reorder: Option<Box<dyn FnMut(usize, usize)>>,
    on_context: Option<Box<dyn FnMut(usize, f64)>>,
}

// ── TabBar ─────────────────────────────────────────────────────────────────

/// A horizontal strip of selectable, reorderable tabs.
///
/// Backed by a [`Canvas`] — the widget draws itself through `DrawContext` and
/// receives pointer events through the canvas callbacks.  The app updates
/// titles/active via [`set_tabs`] and listens for user actions via
/// [`on_select`], [`on_reorder`], and [`on_context`].
#[derive(Clone)]
pub struct TabBar {
    canvas: Canvas,
    state: Rc<RefCell<TabBarState>>,
    /// Kept so [`set_visible`](Self::set_visible) can restore the strip's own
    /// height when the bar comes back — the host only ever learns the height
    /// through the config it passed to [`new`](Self::new).
    /// `Rc<RefCell<..>>`, not a plain `Rc`, so a shared `TabBar` handle can
    /// swap the palette after construction (see [`set_config`](Self::set_config)).
    /// `TabBar` is cloned into canvas callbacks, so this has to stay an
    /// interior-mutable cell rather than being re-owned per clone.
    config: Rc<RefCell<TabBarConfig>>,
}

impl TabBar {
    /// Create a new `TabBar` wrapping the given `canvas`.
    ///
    /// All draw and event callbacks are registered on the canvas immediately.
    /// The bar starts hidden — call [`set_visible`](Self::set_visible) once
    /// the workbook has 2+ sheets.
    pub fn new(canvas: Canvas, config: TabBarConfig) -> Self {
        let state = Rc::new(RefCell::new(TabBarState {
            titles: Vec::new(),
            active: 0,
            hits: Vec::new(),
            drag: None,
            context_pending: None,
            on_select: None,
            on_reorder: None,
            on_context: None,
        }));

        // The config cell is created once and shared with the draw callback,
        // so [`set_config`](Self::set_config) reaches the painter. A separate
        // clone captured by value would leave the bar rendering the palette it
        // was built with forever.
        let config = Rc::new(RefCell::new(config));
        let bar = TabBar {
            canvas: canvas.clone(),
            state: state.clone(),
            config: config.clone(),
        };

        // Draw callback.
        {
            let state = state.clone();
            let cfg = config.clone();
            canvas.set_draw_callback(Box::new(move |dc, w, h| {
                Self::render(dc, w, h, &state, &cfg.borrow());
            }));
        }

        // Click callback (left-click select).
        {
            let state = state.clone();
            canvas.on_click(Box::new(move |x, _y| {
                Self::handle_click(x, &state);
            }));
        }

        // Button-aware click (right-click context, left-click arm drag).
        {
            let state = state.clone();
            let cfg = config.clone();
            canvas.on_click_button(Box::new(move |x, _y, button, _mods| {
                Self::handle_click_button(x, button, &state, &cfg.borrow());
            }));
        }

        // Motion (drag update).
        {
            let state = state.clone();
            let canvas2 = canvas.clone();
            let cfg = config.clone();
            canvas.on_motion(Box::new(move |x, _y, _mods| {
                Self::handle_motion(x, &state, &canvas2, &cfg.borrow());
            }));
        }

        // Release (drag commit, context menu open).
        {
            let state = state.clone();
            canvas.on_release(Box::new(move |x, _y, button, _mods| {
                Self::handle_release(x, button, &state);
            }));
        }

        bar
    }

    /// Replace the tab labels and active index.
    ///
    /// Call this whenever the workbook's sheet list changes (add, rename,
    /// delete, reorder).  The bar does not read workbook state itself — the
    /// app pushes it.
    pub fn set_tabs(&self, titles: &[String], active: usize) {
        let mut s = self.state.borrow_mut();
        s.titles = titles.to_vec();
        s.active = active;
    }

    /// Show or hide the tab strip.
    ///
    /// Hiding must also drop the strip's height request, not just its
    /// visibility: a hidden widget still occupies its layout slot in a
    /// vertical box on the backends that size children from their requests
    /// (and `Window::present`'s `show_all` resurrects the visibility flag on
    /// GTK anyway).  Leaving the request in place parks an empty `height`-px
    /// gap under the grid — the widget is there, it just paints nothing, so
    /// the grid can never reclaim the space.
    pub fn set_visible(&self, visible: bool) {
        let cfg_h = self.config.borrow().height;
        let h = if visible { cfg_h } else { 0.0 };
        self.canvas.set_visible(visible);
        // Size last: the Win32 canvas `set_size_request` passes
        // `SWP_SHOWWINDOW`, which would undo the hide above.
        self.canvas.set_size_request(1, h as i32);
    }

    /// Request a repaint.
    pub fn queue_redraw(&self) {
        self.canvas.queue_redraw();
    }

    /// A copy of the config in effect.
    ///
    /// Returns by value because the config lives behind a `RefCell` the
    /// caller must not hold: borrowing it across an arbitrary caller would
    /// let a caller panic on a re-entrant draw. `TabBarConfig` is small and
    /// `Copy`-adjacent, so the clone is not worth an API that can panic.
    pub fn config(&self) -> TabBarConfig {
        self.config.borrow().clone()
    }

    /// Replace the config, so a colour-scheme switch can take effect on an
    /// already-constructed bar.
    ///
    /// Needed because [`TabBarConfig`] is a plain value copied at construction
    /// time: the bar cannot read [`crate::core::theme`] per frame the way
    /// `spreadsheet::paint` does. An app that themes itself therefore rebuilds
    /// the config (e.g. via [`TabBarConfig::for_theme`]) and pushes it here.
    ///
    /// The height is re-applied so a config whose `height` changed resizes the
    /// strip; a hidden bar stays hidden, since `set_visible` owns that and
    /// re-showing it here would resurrect a strip the app deliberately hid.
    pub fn set_config(&self, config: TabBarConfig) {
        let was_visible = self.config.borrow().height > 0.0;
        *self.config.borrow_mut() = config;
        if was_visible {
            let h = self.config.borrow().height;
            self.canvas.set_size_request(1, h as i32);
        }
        self.queue_redraw();
    }

    /// Register a callback fired when the user clicks a tab (select).
    pub fn on_select(&self, cb: Box<dyn FnMut(usize)>) {
        self.state.borrow_mut().on_select = Some(cb);
    }

    /// Register a callback fired when the user drags a tab to a new position.
    /// Arguments are `(from_index, to_index)`.
    pub fn on_reorder(&self, cb: Box<dyn FnMut(usize, usize)>) {
        self.state.borrow_mut().on_reorder = Some(cb);
    }

    /// Register a callback fired on right-click release over a tab.
    /// Arguments are `(tab_index, x_position)`.
    pub fn on_context(&self, cb: Box<dyn FnMut(usize, f64)>) {
        self.state.borrow_mut().on_context = Some(cb);
    }

    /// The underlying Canvas, for layout packing.
    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    // ── Internal ──────────────────────────────────────────────────────────

    /// Lay out tabs left-to-right, returning hit boxes.
    fn layout(
        titles: &[String],
        active: usize,
        cfg: &TabBarConfig,
        measure: &dyn Fn(&str, i32) -> f64,
    ) -> Vec<TabHit> {
        let mut hits = Vec::with_capacity(titles.len());
        let mut x = 2.0;
        for (index, title) in titles.iter().enumerate() {
            let weight = if index == active { 1 } else { 0 };
            let tw = measure(title, weight);
            let x0 = x;
            let x1 = x0 + cfg.pad_x + tw + cfg.pad_x;
            hits.push(TabHit { x0, x1, index });
            x = x1 + cfg.gap;
        }
        hits
    }

    /// Which tab slot a pointer at `x` would drop into.
    fn drop_index(hits: &[TabHit], x: f64) -> usize {
        if hits.is_empty() {
            return 0;
        }
        for (i, hit) in hits.iter().enumerate() {
            let mid = (hit.x0 + hit.x1) / 2.0;
            if x < mid {
                return i;
            }
        }
        hits.len()
    }

    /// Find the tab under `x`, if any.
    fn hit_test(hits: &[TabHit], x: f64) -> Option<usize> {
        hits.iter()
            .find(|h| x >= h.x0 && x < h.x1)
            .map(|h| h.index)
    }

    // ── Event handlers ────────────────────────────────────────────────────

    fn handle_click(x: f64, state: &Rc<RefCell<TabBarState>>) {
        let idx = {
            let s = state.borrow();
            Self::hit_test(&s.hits, x)
        };
        if let Some(idx) = idx {
            // Fire the callback outside the borrow.
            let mut s = state.borrow_mut();
            if let Some(ref mut cb) = s.on_select {
                cb(idx);
            }
        }
    }

    fn handle_click_button(
        x: f64,
        button: u32,
        state: &Rc<RefCell<TabBarState>>,
        _cfg: &TabBarConfig,
    ) {
        if button == 1 {
            // Left press: arm drag + select.
            {
                let mut s = state.borrow_mut();
                if let Some(hit_idx) = Self::hit_test(&s.hits, x) {
                    s.drag = Some(TabDrag {
                        from: hit_idx,
                        to: hit_idx,
                        press_x: x,
                        moved: false,
                    });
                }
            }
            Self::handle_click(x, state);
            return;
        }
        if button == 3 {
            // Right press: remember position, switch to that tab.
            let hit = {
                let s = state.borrow();
                Self::hit_test(&s.hits, x)
            };
            if hit.is_some() {
                state.borrow_mut().context_pending = Some(x);
                Self::handle_click(x, state);
            }
        }
    }

    fn handle_motion(
        x: f64,
        state: &Rc<RefCell<TabBarState>>,
        canvas: &Canvas,
        cfg: &TabBarConfig,
    ) {
        let mut s = state.borrow_mut();
        let Some(d) = s.drag.as_mut() else { return };
        if !d.moved && (x - d.press_x).abs() < cfg.drag_threshold {
            return;
        }
        d.moved = true;
        let old_to = d.to;
        let hits = s.hits.clone();
        let new_to = Self::drop_index(&hits, x);
        if old_to != new_to {
            if let Some(ref mut d) = s.drag {
                d.to = new_to;
            }
            drop(s);
            canvas.queue_redraw();
        }
    }

    fn handle_release(x: f64, button: u32, state: &Rc<RefCell<TabBarState>>) {
        if button == 1 {
            // Left release: commit drag.
            let drag = state.borrow_mut().drag.take();
            let Some(drag) = drag else { return };
            if !drag.moved || drag.to == drag.from {
                return;
            }
            let from = drag.from;
            let count = state.borrow().titles.len();
            // Compute 1-based position (same logic as gui_backend).
            let mut pos = drag.to + 1;
            if drag.to > drag.from {
                pos = drag.to;
            }
            let pos = pos.min(count);
            let mut s = state.borrow_mut();
            if let Some(ref mut cb) = s.on_reorder {
                cb(from, pos);
            }
            return;
        }
        if button == 3 {
            // Right release: fire context menu.
            let pending = state.borrow_mut().context_pending.take();
            let Some(pending_x) = pending else { return };
            let at = if x.is_finite() && x >= 0.0 { x } else { pending_x };
            let tab_idx = {
                let s = state.borrow();
                Self::hit_test(&s.hits, at)
                    .or_else(|| Self::hit_test(&s.hits, pending_x))
            };
            if let Some(idx) = tab_idx {
                let mut s = state.borrow_mut();
                if let Some(ref mut cb) = s.on_context {
                    cb(idx, at);
                }
            }
        }
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    fn render(
        dc: &mut dyn DrawContext,
        w: i32,
        h: i32,
        state: &Rc<RefCell<TabBarState>>,
        cfg: &TabBarConfig,
    ) {
        let (titles, active, drag_snapshot) = {
            let s = state.borrow();
            if s.titles.len() < 2 {
                // Hide unconditionally (heals show_all resurrection on GTK).
                return;
            }
            (s.titles.clone(), s.active, s.drag)
        };

        dc.clear(cfg.strip_bg.0, cfg.strip_bg.1, cfg.strip_bg.2, 1.0);
        dc.clip(0.0, 0.0, w as f64, h as f64);

        let hits = {
            let cfg_ref = cfg;
            let measure = |t: &str, weight: i32| {
                dc.text_extents_styled(t, &cfg_ref.font_family, cfg_ref.font_size, 0, weight).2
            };
            Self::layout(&titles, active, cfg, &measure)
        };

        // Draw tabs.
        for hit in &hits {
            let is_active = hit.index == active;
            let (r, g, b) = if is_active { cfg.active_bg } else { cfg.idle_bg };
            dc.fill_rect(hit.x0, 2.0, hit.x1 - hit.x0, cfg.height - 4.0, r, g, b, 1.0);
            dc.fill_rect(hit.x1, 2.0, 1.0, cfg.height - 4.0, cfg.divider.0, cfg.divider.1, cfg.divider.2, 1.0);
            let (tr, tg, tb) = if is_active { cfg.active_text } else { cfg.idle_text };
            let weight = if is_active { 1 } else { 0 };
            dc.draw_text_styled(
                hit.x0 + cfg.pad_x,
                (cfg.height - cfg.font_size * 1.2) / 2.0,
                &titles[hit.index],
                &cfg.font_family,
                cfg.font_size,
                tr, tg, tb, 1.0, 0, weight,
            );
        }

        // Drag preview.
        if let Some(drag) = drag_snapshot {
            if drag.moved && drag.index_in_range(hits.len()) {
                Self::render_drag_preview(dc, &hits, drag, cfg);
            }
        }

        // Store hits for click handlers.
        state.borrow_mut().hits = hits;
    }

    fn render_drag_preview(
        dc: &mut dyn DrawContext,
        hits: &[TabHit],
        drag: TabDrag,
        cfg: &TabBarConfig,
    ) {
        // Drop caret.
        let caret_x = if drag.to == 0 {
            hits.first().map(|h| h.x0 - cfg.gap / 2.0).unwrap_or(2.0)
        } else {
            hits.get(drag.to)
                .map(|h| h.x0 - cfg.gap / 2.0)
                .or_else(|| hits.last().map(|h| h.x1 + cfg.gap / 2.0))
                .unwrap_or(2.0)
        };
        dc.fill_rect(caret_x - 1.0, 1.0, 2.0, cfg.height - 2.0, cfg.caret.0, cfg.caret.1, cfg.caret.2, 1.0);
        // Outline the dragged tab.
        if let Some(from) = hits.get(drag.from) {
            for (x, y, w, h) in [
                (from.x0, 2.0, from.x1 - from.x0, 1.0),
                (from.x0, cfg.height - 3.0, from.x1 - from.x0, 1.0),
                (from.x0, 2.0, 1.0, cfg.height - 4.0),
                (from.x1 - 1.0, 2.0, 1.0, cfg.height - 4.0),
            ] {
                dc.fill_rect(x, y, w, h, cfg.caret.0, cfg.caret.1, cfg.caret.2, 1.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TabBarConfig {
        TabBarConfig::default()
    }

    fn hits() -> Vec<TabHit> {
        vec![
            TabHit { x0: 0.0, x1: 100.0, index: 0 },
            TabHit { x0: 106.0, x1: 206.0, index: 1 },
            TabHit { x0: 212.0, x1: 312.0, index: 2 },
        ]
    }

    /// A drop lands on the slot whose *midpoint* the pointer has passed:
    /// drops happen between tabs, so using the boxes themselves would make
    /// the boundary depend on the dragged tab's own width.
    #[test]
    fn drop_index_picks_the_slot_under_the_pointer() {
        let hits = hits();
        assert_eq!(TabBar::drop_index(&hits, 0.0), 0);
        assert_eq!(TabBar::drop_index(&hits, 49.0), 0);
        assert_eq!(TabBar::drop_index(&hits, 51.0), 1);
        assert_eq!(TabBar::drop_index(&hits, 150.0), 1);
        assert_eq!(TabBar::drop_index(&hits, 260.0), 2);
        assert_eq!(
            TabBar::drop_index(&hits, 9999.0),
            3,
            "past every tab drops at the end"
        );
        assert_eq!(TabBar::drop_index(&[], 10.0), 0);
    }

    /// `to == len` is the slot after the last tab, a valid drop position.
    #[test]
    fn drag_index_range_guard() {
        let d = TabDrag { from: 1, to: 2, press_x: 0.0, moved: true };
        assert!(d.index_in_range(3));
        assert!(d.index_in_range(2), "to == len is the after-last slot");
        assert!(!d.index_in_range(0), "an empty strip has no valid slot");
    }

    /// Tabs lay out left to right from x=2, padded by pad_x on both sides
    /// with gap between. "Sheet1" at the stub 8px/char measures 48px, so
    /// tab 1 spans 2..70 and tab 2 starts at 76.
    #[test]
    fn layout_pads_and_gaps_tabs() {
        let titles = vec!["Sheet1".to_string(), "Sheet2".to_string()];
        let c = cfg();
        let hits = TabBar::layout(&titles, 1, &c, &|t: &str, _w: i32| t.chars().count() as f64 * 8.0);
        assert_eq!(hits.len(), 2, "one hit per sheet, got {hits:?}");
        assert_eq!((hits[0].x0, hits[0].x1), (2.0, 70.0));
        assert_eq!(hits[0].index, 0);
        assert_eq!((hits[1].x0, hits[1].x1), (76.0, 144.0));
        assert_eq!(hits[1].index, 1);
    }

    /// No titles, no hits (single-sheet workbooks hide the bar anyway).
    #[test]
    fn layout_empty_without_titles() {
        let c = cfg();
        assert!(TabBar::layout(&[], 0, &c, &|_: &str, _: i32| 0.0).is_empty());
        assert!(TabBar::layout(&["Only".to_string()], 0, &c, &|_: &str, _: i32| 0.0).len() == 1);
    }

    /// The active tab measures bold (weight 1): the measure closure must
    /// observe weight 1 exactly for the active index and 0 elsewhere, or
    /// bold titles would misalign their hit rects.
    #[test]
    fn layout_measures_active_bold() {
        let titles = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let seen = std::cell::RefCell::new(Vec::new());
        let c = cfg();
        let hits = TabBar::layout(&titles, 2, &c, &|t: &str, w: i32| {
            seen.borrow_mut().push((t.to_string(), w));
            10.0
        });
        assert_eq!(
            seen.borrow().clone(),
            vec![("A".into(), 0), ("B".into(), 0), ("C".into(), 1)]
        );
        assert_eq!(hits.len(), 3);
        // Each tab is 10 + 10 + 10 = 30 wide with 6px gaps: tab 0 spans
        // 2..32, tab 1 spans 38..68, tab 2 spans 74..104.
        assert_eq!((hits[2].x0, hits[2].x1), (74.0, 104.0));
    }

    /// Hit-testing is half-open: a pointer exactly on a tab's right edge
    /// belongs to the next tab (or to nothing past the last).
    #[test]
    fn hit_test_is_half_open() {
        let hits = hits();
        assert_eq!(TabBar::hit_test(&hits, 0.0), Some(0));
        assert_eq!(TabBar::hit_test(&hits, 99.9), Some(0));
        assert_eq!(TabBar::hit_test(&hits, 100.0), None, "gap between tabs");
        assert_eq!(TabBar::hit_test(&hits, 206.0), None);
        assert_eq!(TabBar::hit_test(&hits, 312.0), None, "past the last tab");
    }
}
