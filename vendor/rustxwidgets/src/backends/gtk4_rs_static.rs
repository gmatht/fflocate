// TRUE gtk4-rs backend for DEBUGGING: the official gtk4 crate, statically
// linked (no unsafe dlopen).  This is the safe reference implementation used
// to debug the production gtk4-rs-dlopen backend.
use crate::backends::{BackendApp, BackendError};
use std::sync::LazyLock;

static MAIN_LOOP: LazyLock<gtk4_static::glib::MainLoop> = LazyLock::new(|| {
    gtk4_static::glib::MainLoop::new(None, false)
});

pub struct GtkApp;

impl BackendApp for GtkApp {
    fn run(self: Box<Self>) -> Result<(), BackendError> {
        MAIN_LOOP.run();
        Ok(())
    }
}

pub fn quit_main_loop() {
    MAIN_LOOP.quit();
}

pub fn init() -> Result<Box<dyn BackendApp>, BackendError> {
    if std::env::var("GDK_BACKEND").is_err() {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    if std::env::var("GSK_RENDERER").is_err() {
        std::env::set_var("GSK_RENDERER", "cairo");
    }
    if !gtk4_static::is_initialized() {
        gtk4_static::init().map_err(|e| format!("gtk4 init failed: {e}"))?;
    }
    Ok(Box::new(GtkApp))
}
