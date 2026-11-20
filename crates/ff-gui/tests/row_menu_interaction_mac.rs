//! The row context menu and Copy path on AppKit itself.
//!
//! The sibling of `row_menu_mac.rs`, which can only test the menu model: a
//! `cargo test` harness has no AppKit on macOS, so `objc_getClass("NSMenu")` is
//! nil there. This one is `harness = false`, so `App::init` runs on the process's
//! main thread with the AppKit backend loaded, and the menu that reaches the
//! screen is the menu inspected here.
//!
//! What is asserted is exactly what a right-click depends on:
//!
//! * `ContextMenu::build` — the popup's first step — produces an `NSMenu` with
//!   the four row actions, all enabled;
//! * every `NSMenuItem` has a non-nil target of the adapter's
//!   `CorroMacMenuTarget` class and the `corroFired:` action, i.e. the item is
//!   wired into the Rust registry rather than to nothing;
//! * firing each item (AppKit's `performActionForItemAtIndex:`, the dispatch a
//!   click runs) calls the matching `RowAction`, and Copy path leaves the row's
//!   path on `NSPasteboard`, read back through AppKit directly instead of
//!   through the call that wrote it.
//!
//! The popup's own modal tracking loop is exercised by
//! `examples/macos_probe.rs`, which queues an Escape and pops the real menu up.

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    use std::rc::Rc;

    use ff_gui::RowAction;
    use rswidgets::backends::apple::{cls, msg_shim, nsstring, nsstring_to_rust, selector};
    use rswidgets::compat::ContextMenu;
    use rswidgets::prelude::App;

    fn send0(obj: *mut c_void, sel: &str) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(msg_shim()) };
        unsafe { f(obj, selector(sel)) }
    }
    fn send0i(obj: *mut c_void, sel: &str) -> isize {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize =
            unsafe { std::mem::transmute(msg_shim()) };
        unsafe { f(obj, selector(sel)) }
    }
    fn send1i(obj: *mut c_void, sel: &str, arg: isize) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, isize) -> *mut c_void =
            unsafe { std::mem::transmute(msg_shim()) };
        unsafe { f(obj, selector(sel), arg) }
    }
    fn send1p(obj: *mut c_void, sel: &str, arg: *mut c_void) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(msg_shim()) };
        unsafe { f(obj, selector(sel), arg) }
    }

    /// The general pasteboard's text, read with `NSPasteboard`'s own call.
    ///
    /// Deliberately not `compat::clipboard_get_text`: Copy writes through the
    /// adapter, so this asks AppKit the way a second application would.
    fn pasteboard_text() -> Option<String> {
        let pasteboard = send0(cls("NSPasteboard"), "generalPasteboard");
        if pasteboard.is_null() {
            return None;
        }
        let kind = nsstring("public.utf8-plain-text");
        nsstring_to_rust(send1p(pasteboard, "stringForType:", kind))
    }

    let _app = App::init()?;
    let win = _app.create_window()?;
    win.set_title("ff-gui row menu test");
    win.present();

    // A real file on disk: the copy round trip has to be a path another
    // application could open, not a string that happens to match.
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("ff-gui-row-menu.txt");
    std::fs::write(&file, "row menu\n")?;
    let path = file.to_string_lossy().into_owned();
    let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();

    let actions = [
        RowAction::Open,
        RowAction::OpenContainingFolder,
        RowAction::CopyPath,
        RowAction::OpenInTerminal,
    ];
    let ran: Rc<RefCell<Vec<String>>> = Default::default();
    let copies = Rc::new(Cell::new(0usize));
    let mut menu = ContextMenu::new();
    for action in actions {
        let ran = ran.clone();
        let copies = copies.clone();
        let path = path.clone();
        menu.append_item(
            action.label(),
            Box::new(move || {
                let line = ff_gui::run_row_action(action, &path, |text| {
                    copies.set(copies.get() + 1);
                    rswidgets::compat::clipboard_set_text(text).map_err(|e| e.to_string())
                });
                ran.borrow_mut().push(line);
            }),
        );
    }

    // `build` is the popup's first step: exactly the NSMenu
    // `popup_at_pointer` would put on screen.
    let handle = menu.build().expect("the row menu could not be built");
    let items = ContextMenu::items_of(handle);
    println!("row menu: NSMenu items {items:?}");
    assert_eq!(items.len(), 4, "the NSMenu does not offer four actions");
    assert_eq!(
        items.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>(),
        actions.iter().map(|a| a.label().to_string()).collect::<Vec<_>>(),
        "the NSMenu labels are not the four row actions"
    );
    for (label, enabled) in &items {
        assert!(*enabled, "the NSMenu item {label:?} is disabled");
    }

    // Every item's target/action: this is the wiring a click goes through, so a
    // nil target or a different selector means a menu that shows but does
    // nothing.
    let menu_obj = handle as *mut c_void;
    assert!(!menu_obj.is_null(), "the row menu handle is null");
    let array = send0(menu_obj, "itemArray");
    let count = send0i(array, "count");
    assert_eq!(count, 4, "AppKit's NSMenu holds {count} items");
    for index in 0..count as usize {
        let item = send1i(array, "objectAtIndex:", index as isize);
        assert!(!item.is_null(), "item {index} is null");
        let title = nsstring_to_rust(send0(item, "title")).unwrap_or_default();
        let target = send0(item, "target");
        assert!(
            !target.is_null(),
            "item {index} ({title:?}) has no target, so choosing it would do nothing"
        );
        let target_class = nsstring_to_rust(send0(target, "className")).unwrap_or_default();
        assert_eq!(
            target_class, "CorroMacMenuTarget",
            "item {index} ({title:?}) targets {target_class:?}, not the adapter's menu target"
        );
        let action = send0(item, "action");
        assert_eq!(
            action,
            selector("corroFired:"),
            "item {index} ({title:?}) does not send corroFired: to its target"
        );
        println!("row menu: item {index} {title:?} -> CorroMacMenuTarget/corroFired:");
    }

    // Fire each item the way a click does: AppKit's own dispatch, then the
    // registry action the item names.
    for (index, action) in actions.iter().enumerate() {
        assert!(
            ContextMenu::fire(handle, index),
            "{} could not be fired",
            action.label()
        );
        let line = ran.borrow().last().cloned().unwrap_or_default();
        println!("row menu: chose {:?} -> {line:?}", action.label());
        if *action == RowAction::CopyPath {
            assert_eq!(line, format!("copied {path}"), "Copy path reported {line:?}");
        } else {
            assert!(
                line.starts_with(action.label()),
                "choosing {:?} ran {line:?}",
                action.label()
            );
            assert!(
                line.contains(&name),
                "choosing {:?} did not name the row's file: {line:?}",
                action.label()
            );
        }
    }
    assert_eq!(ran.borrow().len(), 4, "not every NSMenu item ran its action");
    assert_eq!(
        copies.get(),
        1,
        "only Copy path should reach the clipboard, but it was called {} times",
        copies.get()
    );

    let read_back = pasteboard_text().expect("the pasteboard holds no text after Copy path");
    println!("clipboard: NSPasteboard holds {read_back:?}");
    assert_eq!(
        read_back, path,
        "Copy path did not leave the row's path on the pasteboard"
    );
    assert!(
        std::fs::metadata(&read_back).map(|m| m.is_file()).unwrap_or(false),
        "the copied path {read_back:?} is not a file on disk"
    );

    println!(
        "row menu: ok — 4 enabled items, each wired to CorroMacMenuTarget/corroFired:, Copy on the pasteboard"
    );
    println!("clipboard: ok — the pasteboard reads back the row's path");
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    // The AppKit menu and pasteboard are the subject; the GTK counterparts are
    // `tests/row_menu_gtk.rs`, and the portable model is `tests/row_menu_mac.rs`.
    println!("row_menu_interaction_mac: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
