//! The system clipboard on AppKit: set → read, with a reader that is not the
//! writer, and Copy path driving it.
//!
//! The macOS sibling of `tests/clipboard_gtk.rs`, and the clipboard half of what
//! `tests/row_menu_row_mac.rs` covers next to its NSMenu. Two reads are checked
//! besides the app's own (`compat::clipboard_get_text`): `NSPasteboard` asked
//! directly for `public.utf8-plain-text`, which is what another application
//! would read, and a second, different text, so a reader that echoes its own
//! write instead of the pasteboard fails. Finally
//! `run_row_action(RowAction::CopyPath, …)` — the app's Copy path handler — must
//! report the path it copied and leave exactly that path on the pasteboard.
//!
//! AppKit owns the general pasteboard and the process's main thread, hence
//! `harness = false` and its own `fn main` — see `[[test]]` in `Cargo.toml`.

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::c_void;

    use ff_gui::{run_row_action, RowAction};
    use rswidgets::prelude::*;

    fn send0(obj: *mut c_void, sel: &str) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    /// Read the general pasteboard the way another application would: ask it
    /// for the plain-text flavor directly, not through compat.
    fn read_pasteboard() -> Option<String> {
        let pb = send0(
            rswidgets::backends::apple::cls("NSPasteboard"),
            "generalPasteboard",
        );
        if pb.is_null() {
            return None;
        }
        // SAFETY: `stringForType:` on the general pasteboard, with a freshly
        // built NSString type name; the reply is an NSString or nil.
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        let value = unsafe {
            f(
                pb,
                rswidgets::backends::apple::selector("stringForType:"),
                rswidgets::backends::apple::nsstring("public.utf8-plain-text"),
            )
        };
        rswidgets::backends::apple::nsstring_to_rust(value)
    }

    // AppKit has to be initialised before the pasteboard can be reached.
    let _app = App::init()?;

    // A real file on disk, so what travels is a path another application could
    // actually use.
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("typing-alpha.txt");
    std::fs::write(&file, "clipboard fixture\n")?;
    let path = file.to_string_lossy().into_owned();

    // --- Set through the app's call, read through the app's call ---
    rswidgets::compat::clipboard_set_text(&path)?;
    let through_app = rswidgets::compat::clipboard_get_text()?;
    println!("clipboard: set {path:?} -> app read back {through_app:?}");
    assert_eq!(
        through_app, path,
        "the pasteboard does not hold the text the app set"
    );
    let direct = read_pasteboard().expect("NSPasteboard holds no string after the app's copy");
    assert_eq!(
        direct, path,
        "NSPasteboard asked directly does not hold the path the app set"
    );
    assert!(
        std::fs::metadata(&through_app).map(|m| m.is_file()).unwrap_or(false),
        "the text on the pasteboard {:?} is not the file that was copied",
        through_app
    );

    // --- A second, different text: a reader that echoes its own write fails ---
    let second = "ff-gui clipboard round trip";
    rswidgets::compat::clipboard_set_text(second)?;
    let through_app = rswidgets::compat::clipboard_get_text()?;
    let direct = read_pasteboard().expect("NSPasteboard read nothing after the second copy");
    println!("clipboard: set {second:?} -> app read back {through_app:?}, pasteboard {direct:?}");
    assert_eq!(
        through_app, second,
        "the app's read still returns {through_app:?} after a different text was set"
    );
    assert_eq!(
        direct, second,
        "NSPasteboard still holds {direct:?} after a different text was set"
    );

    // --- Copy path, the app's own handler, through the same call ---
    let message = run_row_action(RowAction::CopyPath, &path, |text| {
        rswidgets::compat::clipboard_set_text(text).map_err(|e| e.to_string())
    });
    assert_eq!(
        message,
        format!("copied {path}"),
        "Copy path reported {message:?}"
    );
    let after_copy = rswidgets::compat::clipboard_get_text()?;
    let direct = read_pasteboard().expect("NSPasteboard read nothing after Copy path");
    println!("clipboard: Copy path reported {message:?}, pasteboard holds {direct:?}");
    assert_eq!(
        after_copy, path,
        "Copy path did not leave the row's path on the pasteboard (it holds {after_copy:?})"
    );
    assert_eq!(
        direct, path,
        "NSPasteboard asked directly does not hold the row's path after Copy path"
    );

    println!(
        "clipboard: ok — set/read round-tripped {path:?} through the app and NSPasteboard's own read, \
         a second text replaced it, and Copy path left it on the pasteboard"
    );
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    // NSPasteboard is AppKit's; the GTK counterpart is `tests/clipboard_gtk.rs`.
    println!("clipboard_mac: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
