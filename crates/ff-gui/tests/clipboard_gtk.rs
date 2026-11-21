//! The system clipboard on GTK: set → read, with a reader that is not the
//! writer, and Copy path driving it.
//!
//! The mission's other half — the row menu — is `tests/row_menu_row_gtk.rs`;
//! this binary is about the clipboard alone, so a broken pasteboard cannot hide
//! behind a menu assertion. Two reads are checked:
//!
//! * `compat::clipboard_get_text`, the app's own read path;
//! * `gtk_clipboard_wait_for_text` on the CLIPBOARD selection, i.e. GDK's own
//!   waiting read, the way another application would ask for it.
//!
//! A write that silently swallowed the text fails both. A read that echoed the
//! last write instead of the selection cannot pass the second text: the test
//! writes a different string and both reads must follow it. Finally
//! `run_row_action(RowAction::CopyPath, …)` — the app's Copy path handler — must
//! report the path it copied and leave exactly that path on the clipboard.
//!
//! Needs a display with GTK3, so a headless run returns early; CI runs it under
//! Xvfb.

#![cfg(unix)]

use ff_gui::{run_row_action, RowAction};

/// Read the CLIPBOARD selection with GDK's own waiting read.
///
/// Deliberately independent of `compat::clipboard_get_text`: the copy side goes
/// through `gtk_clipboard_set_text`, so reading with
/// `gtk_clipboard_wait_for_text` asks GTK to serve the selection back the way
/// another application would.
fn read_clipboard_gdk(loader: &gtk_dynamic_loader::Loader) -> Option<String> {
    let syms = &loader.symbols;
    let intern = syms.gdk_atom_intern?;
    let get = syms.gtk_clipboard_get?;
    let wait = syms.gtk_clipboard_wait_for_text?;
    // SAFETY: GDK3's own selection calls; every pointer is null-checked.
    unsafe {
        let atom = intern(b"CLIPBOARD\0".as_ptr(), 0);
        if atom.is_null() {
            return None;
        }
        let clipboard = get(atom);
        if clipboard.is_null() {
            return None;
        }
        let text = wait(clipboard);
        if text.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(text as *const std::ffi::c_char)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[test]
fn clipboard_set_text_round_trips_through_the_real_selection() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("clipboard: no display, skipping");
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => {
            eprintln!("clipboard: no GTK loader, skipping");
            return;
        }
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        eprintln!("clipboard: not GTK3, skipping");
        return;
    }
    if rswidgets::App::init().is_err() {
        eprintln!("clipboard: GTK did not initialise, skipping");
        return;
    }

    // A real file on disk, so what travels is a path another application could
    // actually use.
    let dir = tempfile::tempdir().expect("no temp dir");
    let file = dir.path().join("typing-alpha.txt");
    std::fs::write(&file, "clipboard fixture\n").expect("could not write the file");
    let path = file.to_string_lossy().into_owned();

    // --- Set through the app's call, read through the app's call ---
    rswidgets::compat::clipboard_set_text(&path).expect("the app's clipboard write failed");
    let through_app = rswidgets::compat::clipboard_get_text().expect("the app's clipboard read failed");
    assert_eq!(
        through_app, path,
        "the clipboard does not hold the text the app set"
    );
    assert_eq!(
        read_clipboard_gdk(&loader).as_deref(),
        Some(path.as_str()),
        "GDK's own read of CLIPBOARD does not hold the path the app set"
    );
    assert!(
        std::fs::metadata(&through_app).map(|m| m.is_file()).unwrap_or(false),
        "the text on the clipboard {:?} is not the file that was copied",
        through_app
    );
    eprintln!("clipboard: set {path:?} -> app read back {through_app:?}, GDK read it too");

    // --- A second, different text: a reader that echoes its own write fails ---
    let second = "ff-gui clipboard round trip";
    rswidgets::compat::clipboard_set_text(second).expect("the second clipboard write failed");
    let through_app = rswidgets::compat::clipboard_get_text().expect("the second app read failed");
    let through_gdk = read_clipboard_gdk(&loader).expect("GDK read nothing after the second copy");
    assert_eq!(
        through_app, second,
        "the app's read still returns {through_app:?} after a different text was set"
    );
    assert_eq!(
        through_gdk, second,
        "GDK's read still returns {through_gdk:?} after a different text was set"
    );
    eprintln!("clipboard: set {second:?} -> app and GDK both read it back");

    // --- Copy path, the app's own handler, through the same call ---
    let message = run_row_action(RowAction::CopyPath, &path, |text| {
        rswidgets::compat::clipboard_set_text(text).map_err(|e| e.to_string())
    });
    assert_eq!(
        message,
        format!("copied {path}"),
        "Copy path reported {message:?}"
    );
    let after_copy = rswidgets::compat::clipboard_get_text().expect("the read after Copy path failed");
    assert_eq!(
        after_copy, path,
        "Copy path did not leave the row's path on the clipboard (it holds {after_copy:?})"
    );
    assert_eq!(
        read_clipboard_gdk(&loader).as_deref(),
        Some(path.as_str()),
        "GDK's read after Copy path does not hold the row's path"
    );
    eprintln!("clipboard: Copy path reported {message:?} and the selection holds it");

    println!(
        "clipboard: ok — set/read round-tripped {path:?} through the app and GDK's own selection read, \
         a second text replaced it, and Copy path left it on the clipboard"
    );
}
