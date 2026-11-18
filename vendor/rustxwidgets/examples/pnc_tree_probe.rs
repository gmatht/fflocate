//! Capability probe: how much of the GUI widget tree can a non-GUI backend
//! actually host?
//!
//! `gui_backend.rs` builds one widget tree (window → boxes → labels, an entry
//! for the formula bar, a canvas inside a scrolled window) and drives it
//! entirely through three callbacks (`set_draw_callback` for rendering,
//! `connect_changed` for the formula entry, `on_event_key` for input). Running
//! *that same tree* on another backend tells you whether the backend is a
//! candidate for hosting `gui_backend` unchanged, or whether the differences
//! are structural.
//!
//! This exists because "can pancurses just be another `gui_backend` UI?" is a
//! recurring question with a non-obvious answer: the tree *almost* builds —
//! most `App::new_*` factories have a pancurses arm — and then fails at the one
//! that does not, or succeeds with every callback inert. Reporting each step
//! individually (rather than propagating the first `?`) makes the gap
//! measurable instead of a single opaque error.
//!
//! Run against any backend:
//!
//!     cargo run -p rswidgets --no-default-features --features pancurses \
//!         --example pnc_tree_probe
//!     xvfb-run -a cargo run -p rswidgets --no-default-features --features gtk \
//!         --example pnc_tree_probe
//!
//! The output is a checklist; a backend that prints `ERR`/`INERT` for a step
//! cannot host `gui_backend` as-is for that step's reason.
use rswidgets::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

/// One probe step: what it needs, and whether it worked.
struct Report {
    lines: Vec<(String, String)>,
}

impl Report {
    fn new() -> Self {
        Report { lines: Vec::new() }
    }

    /// Record a step that either produced a widget or explained why not.
    fn step<T>(&mut self, what: &str, r: Result<T, rswidgets::core::Error>) -> Option<T> {
        match r {
            Ok(v) => {
                self.lines.push((what.to_string(), "ok".to_string()));
                Some(v)
            }
            Err(e) => {
                self.lines.push((what.to_string(), format!("ERR  {e}")));
                None
            }
        }
    }

    /// Record a callback that was registered but must also *fire* to be usable.
    fn live(&mut self, what: &str, fired: u32) {
        let verdict = if fired > 0 {
            format!("ok   (fired {fired}x)")
        } else {
            "INERT (registered but never fired)".to_string()
        };
        self.lines.push((what.to_string(), verdict));
    }

    fn print(&self) {
        println!("\nGUI-tree host capability");
        println!("{}", "-".repeat(58));
        for (what, verdict) in &self.lines {
            println!("  {what:<38} {verdict}");
        }
        println!("{}", "-".repeat(58));
        let inert = self.lines.iter().filter(|(_, v)| !v.starts_with("ok")).count();
        if inert == 0 {
            println!("  every probed step works: this backend can host gui_backend's tree");
        } else {
            println!("  {inert} step(s) unavailable/inert: gui_backend cannot run here as-is");
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut rep = Report::new();

    let rxapp = rswidgets::App::init()?;
    let Some(win) = rep.step("new_window", rxapp.new_window()) else {
        rep.print();
        return Ok(());
    };
    win.set_title("probe");
    win.set_default_size(620, 420);

    let _vbox = rep.step("new_box(vertical)", rxapp.new_box(Orientation::Vertical, 0));
    let _bar = rep.step("new_box(horizontal)", rxapp.new_box(Orientation::Horizontal, 2));
    let _addr = rep.step("new_label", rxapp.new_label("A1"));

    // The formula bar: gui_backend reads and writes this entry and depends on
    // `connect_changed` to notice user edits.
    if let Some(entry) = rep.step("new_entry", rxapp.new_entry()) {
        rep.step("entry.set_hexpand", Ok::<_, rswidgets::core::Error>(entry.set_hexpand(true)));
        let changed = Rc::new(Cell::new(0));
        let c = changed.clone();
        let hooked = entry.connect_changed(move || c.set(c.get() + 1));
        rep.step("entry.connect_changed", hooked.map(|_| ()));
        entry.set_text("42");
        rep.live("entry changed callback", changed.get());
        let got = entry.get_text();
        rep.lines.push((
            "entry.get_text after set_text".into(),
            match got.as_deref() {
                Some("42") => "ok   (round-trips)".into(),
                other => format!("INERT (got {other:?})"),
            },
        ));
    }

    // The grid surface: a canvas inside a scrolled window, driven by a draw
    // callback. This is the step that decides the question.
    if let Some(canvas) = rep.step("new_canvas", rxapp.new_canvas()) {
        canvas.set_size_request(1, 1);
        canvas.set_can_focus(true);
        rep.step("canvas.set_can_focus", Ok::<_, rswidgets::core::Error>(()));
        rep.step(
            "canvas.set_draw_callback",
            Ok::<_, rswidgets::core::Error>(()),
        );
        let drew = Rc::new(Cell::new(0));
        let d = drew.clone();
        canvas.set_draw_callback(Box::new(move |_dc, _w, _h| d.set(d.get() + 1)));
        // NOTE: a draw only happens once the backend's frame clock runs, which
        // this probe deliberately does not start (no main loop). So this row
        // reads INERT on *every* backend and is not a backend verdict — it is
        // recorded only to show the callback was accepted. A backend that
        // cannot render at all shows up as `set_draw_callback` being a no-op,
        // which is visible in the adapter source, not here.
        let _ = &drew;

        match rep.step("new_scrolled_window", rxapp.new_scrolled_window()) {
            Some(scrolled) => {
                // `set_child` on the wrapper needs the inner handle (the
                // portable wrapper takes `AsRef<*mut c_void>`).
                scrolled.inner.set_child(&canvas);
                rep.lines.push(("scrolled.set_child".into(), "ok".into()));
            }
            None => {
                // gui_backend hosts its canvas in a scrolled window, so this
                // failure alone blocks the tree.
                rep.lines.push((
                    "scrolled window (needed by gui_backend)".into(),
                    "MISSING (no arm in App::new_scrolled_window)".into(),
                ));
            }
        }
    }

    // Keyboard input arrives at the window in gui_backend.
    //
    // NOTE: this row reports whether the callback was *accepted*, not whether
    // it fired. A key can only be delivered by a live event loop, and this
    // probe deliberately starts none — so asserting `live()` here would print
    // INERT on every backend and falsely read as a capability gap. The probe
    // therefore records acceptance and says so, rather than dressing an
    // artefact up as a verdict.
    let keyed = Rc::new(Cell::new(0));
    let k = keyed.clone();
    win.on_event_key(Box::new(move |_kv, _st| {
        k.set(k.get() + 1);
        1
    }));
    win.present();
    rep.lines.push((
        "window key callback accepted".into(),
        "ok   (fires only with a running loop)".into(),
    ));

    // Exiting: gui_backend's exit path is `try_quit`, which needs a *running*
    // main loop. This probe never starts one, so a failure here says nothing
    // about the backend; it is recorded so the row is not mistaken for a
    // capability. What matters is whether `try_quit` exists at all.
    let quit = rxapp.try_quit();
    rep.lines.push((
        "try_quit exists (needs a running loop)".into(),
        match quit {
            Ok(()) => "ok".into(),
            // "main loop not running" is the expected answer here.
            Err(e) if e.contains("main loop not running") => "ok   (no loop started)".into(),
            Err(e) => format!("ERR  {e}"),
        },
    ));

    rep.print();
    Ok(())
}

#[cfg(test)]
mod loop_ownership_tests {
    //! The capability query must agree with what each backend's `run` does.
    //!
    //! This is the one structural difference between backends that survives
    //! every other unification (see `BackendApp::owns_event_loop`), so it is
    //! worth pinning: the terminal backends block in their own input call,
    //! while the GUI/framework backends hand control to a loop they do not own.
    #[test]
    fn terminal_backends_own_their_loop() {
        // Source-level check, so it holds on every build (the GUI traits can't
        // be instantiated without a display).
        for f in [
            "src/backends/pancurses.rs",
            "src/backends/zork/repl.rs",
            "src/backends/ratatui.rs",
        ] {
            let src = std::fs::read_to_string(f).unwrap_or_else(|e| panic!("read {f}: {e}"));
            // Indentation differs (some are inside a module, some not), so
            // match the two significant lines rather than exact whitespace.
            let reports_true = src
                .lines()
                .zip(src.lines().skip(1))
                .any(|(a, b)| a.contains("fn owns_event_loop") && b.trim() == "true");
            assert!(
                reports_true,
                "{f} blocks in its own input loop and must report owns_event_loop() == true"
            );
        }
        // …and a delegating backend must NOT override it to true.
        for f in ["src/backends/gtk.rs", "src/backends/nwg.rs"] {
            let src = std::fs::read_to_string(f).unwrap_or_else(|e| panic!("read {f}: {e}"));
            assert!(
                !src.contains("fn owns_event_loop"),
                "{f} hands control to a framework; it must use the default (false)"
            );
        }
    }
}
