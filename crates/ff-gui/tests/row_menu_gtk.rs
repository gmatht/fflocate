//! The row context menu on GTK, built from the app's own plan.
//!
//! Every item here comes from `ff_gui::row_menu_plan` — the call `main.rs` makes
//! on a right-click — so the native menu under test is the menu the app would
//! show: the model's items, at the model's per-item sensitivity. The list is
//! read back out of the built `GtkMenu` (`ContextMenu::items_of`) and compared
//! with the plan, an item is fired through GTK's own `activate` signal (the one
//! a click emits), and Copy path is read back with a second, different GTK call
//! (`gtk_clipboard_wait_for_text`), not the write side.
//!
//! The file's absence is simulated with the probe the app passes to
//! `row_menu_plan` (`Path::is_dir` for the second menu, where the row's
//! directory still exists and the file does not), so `Open`/`Open with…` come
//! out insensitive and the refusal is exercised without launching a helper.
//! Nothing here opens an application: the sensitive item that is fired is Copy
//! path, and an insensitive one cannot fire at all.
//!
//! The popup is `gtk_menu_popup`, the call the app's right-click handler
//! reaches; it is not modal on GTK, so it can be shown and left to the display.
//! Needs a display with GTK3, so on a headless run it returns early; CI runs it
//! under Xvfb.

#![cfg(unix)]

use std::path::Path;

use ff_gui::{row_menu_plan, run_row_action, RowAction};
// `Window::raw_handle` comes from the widget trait, as it does in the app.
use rswidgets::compat::ContextMenu;
use rswidgets::core::Widget as _;

/// Read the CLIPBOARD selection through GDK's own waiting read.
///
/// Deliberately not `compat::clipboard_get_text`: the copy side goes through
/// `gtk_clipboard_set_text`, so reading with `gtk_clipboard_wait_for_text` asks
/// GTK to serve the selection back the way another application would, instead
/// of trusting the same call that wrote it.
fn read_clipboard_directly(loader: &gtk_dynamic_loader::Loader) -> Option<String> {
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

/// Build the native menu from a plan: one item per model entry, at that
/// entry's sensitivity, each running the action through the app's own runner.
fn build_from_plan(
    target: &str,
    items: &[ff_gui::RowMenuItem],
) -> (
    ContextMenu,
    usize,
    std::rc::Rc<std::cell::RefCell<Vec<String>>>,
) {
    let ran: std::rc::Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let mut menu = ContextMenu::new();
    for item in items {
        let ran = ran.clone();
        let path = target.to_string();
        let action = item.action;
        menu.append_item_enabled(
            item.label,
            item.enabled,
            Box::new(move || {
                let line = run_row_action(action, &path, |text| {
                    rswidgets::compat::clipboard_set_text(text).map_err(|e| e.to_string())
                });
                ran.borrow_mut().push(line);
            }),
        );
    }
    let handle = menu.build().expect("the row menu could not be built");
    (menu, handle, ran)
}

#[test]
fn the_row_menu_is_the_shared_plan_and_copy_reaches_the_clipboard() {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        eprintln!("row menu: no display, skipping");
        return;
    }
    let loader = match gtk_dynamic_loader::Loader::new() {
        Ok(l) => l,
        Err(_) => {
            eprintln!("row menu: no GTK loader, skipping");
            return;
        }
    };
    if loader.version() != gtk_dynamic_loader::Version::Gtk3 {
        eprintln!("row menu: not GTK3, skipping");
        return;
    }
    let Ok(app) = rswidgets::App::init() else {
        eprintln!("row menu: GTK did not initialise, skipping");
        return;
    };

    // A real file on disk, so the copy round trip is a real path rather than a
    // string nothing would find.
    let dir = tempfile::tempdir().expect("no temp dir");
    let file = dir.path().join("ff-gui-row-menu.txt");
    std::fs::write(&file, "row menu\n").expect("could not write the row's file");
    let path = file.to_string_lossy().into_owned();
    let rows = vec![path.clone()];

    // --- The row's file exists: every item the plan offers is sensitive ---
    let plan = row_menu_plan(3, Some(0), &rows, |p| Path::new(p).exists())
        .expect("a right-click on a row produced no menu");
    assert_eq!(
        plan.0, path,
        "the plan targets {:?}, not the row's path {:?}",
        plan.0, path
    );
    let (mut menu, handle, ran) = build_from_plan(&plan.0, &plan.1);
    let shown = ContextMenu::items_of(handle);
    eprintln!("row menu: items {shown:?} (plan targets {:?})", plan.0);
    assert_eq!(
        shown.len(),
        6,
        "the menu does not offer the model's six actions: {shown:?}"
    );
    assert_eq!(
        shown
            .iter()
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>(),
        plan.1
            .iter()
            .map(|item| item.label.to_string())
            .collect::<Vec<_>>(),
        "the menu's labels are not the shared plan's"
    );
    assert_eq!(
        shown.iter().map(|(_, enabled)| *enabled).collect::<Vec<_>>(),
        plan.1.iter().map(|item| item.enabled).collect::<Vec<_>>(),
        "the menu's sensitivity is not the shared plan's"
    );
    for (label, enabled) in &shown {
        assert!(
            *enabled,
            "the menu item {label:?} is insensitive although the row's file exists"
        );
    }

    // Copy path is the sensitive item this test fires: it dispatches through
    // the app's runner and must leave the row's path in the real clipboard.
    let copy = plan
        .1
        .iter()
        .position(|item| item.action == RowAction::CopyPath)
        .expect("the plan has no Copy path item");
    assert!(
        ContextMenu::fire(handle, copy),
        "Copy path ({}) could not be fired",
        plan.1[copy].label
    );
    let lines = ran.borrow().clone();
    eprintln!("row menu: fired {lines:?}");
    assert_eq!(
        lines,
        vec![format!("copied {path}")],
        "choosing Copy path did not run its action"
    );

    // The clipboard is the observable half of a copy: ask GTK for it with the
    // read that is not the write.
    let read_back = read_clipboard_directly(&loader)
        .expect("the clipboard holds no text after Copy path");
    eprintln!("clipboard holds {read_back:?}");
    assert_eq!(
        read_back, path,
        "Copy path did not leave the row's path on the clipboard"
    );
    assert!(
        std::fs::metadata(&read_back)
            .map(|m| m.is_file())
            .unwrap_or(false),
        "the copied path {read_back:?} is not a file on disk"
    );

    // --- The row's file is gone: the plan greys two items out ---
    //
    // The app's probe is a real `Path::exists`; this passes `is_dir`, which is
    // the same answer for a row whose file has been deleted under a still
    // present directory.
    let gone = row_menu_plan(3, Some(0), &rows, |p| Path::new(p).is_dir())
        .expect("a right-click on a row produced no menu");
    let (menu_gone, handle_gone, ran_gone) = build_from_plan(&gone.0, &gone.1);
    let shown_gone = ContextMenu::items_of(handle_gone);
    eprintln!("row menu: a missing file shows {shown_gone:?}");
    assert_eq!(
        shown_gone,
        gone.1
            .iter()
            .map(|item| (item.label.to_string(), item.enabled))
            .collect::<Vec<_>>(),
        "the menu does not match the plan for a row whose file is gone"
    );
    let open = gone
        .1
        .iter()
        .position(|item| item.action == RowAction::Open)
        .expect("the plan has no Open item");
    let open_with = gone
        .1
        .iter()
        .position(|item| item.action == RowAction::OpenWith)
        .expect("the plan has no Open with… item");
    assert!(
        !shown_gone[open].1 && !shown_gone[open_with].1,
        "Open and Open with… stay sensitive for a missing file: {shown_gone:?}"
    );
    assert_eq!(
        shown_gone.iter().filter(|(_, enabled)| !*enabled).count(),
        2,
        "more than the two file actions went insensitive: {shown_gone:?}"
    );

    // An insensitive item refuses: no dispatch, nothing opened.
    assert!(
        !ContextMenu::fire(handle_gone, open),
        "an insensitive {} dispatched",
        gone.1[open].label
    );
    assert!(
        ran_gone.borrow().is_empty(),
        "choosing an insensitive item dispatched {:?}",
        ran_gone.borrow()
    );
    // …while Copy path still runs for that same row.
    let copy_gone = gone
        .1
        .iter()
        .position(|item| item.action == RowAction::CopyPath)
        .expect("the plan has no Copy path item");
    assert!(
        ContextMenu::fire(handle_gone, copy_gone),
        "Copy path could not be fired for a row whose file is gone"
    );
    assert_eq!(
        ran_gone.borrow().clone(),
        vec![format!("copied {path}")],
        "Copy path on a missing file did not run its action"
    );
    let read_back = read_clipboard_directly(&loader)
        .expect("the clipboard holds no text after Copy path");
    assert_eq!(read_back, path, "Copy path left the wrong text on the clipboard");
    eprintln!(
        "row menu: missing file → {shown_gone:?}; Open refused, Copy path put {read_back} on CLIPBOARD"
    );

    // The popup the app's right-click handler calls. GTK3 menus are not modal,
    // so this goes up and stays up; a window gives it a real parent to be shown
    // against, and the call's own answer says it was shown rather than refused.
    let window = app.create_window().expect("no GTK window");
    window.set_title("row menu");
    match menu.popup_at_pointer(window.raw_handle(), std::ptr::null_mut()) {
        Ok(()) => eprintln!("row menu: popped up at the pointer ({} items)", shown.len()),
        Err(e) => panic!("the row menu did not pop up: {e}"),
    }
    eprintln!(
        "row menu: ok — six items from the shared plan, sensitivity from the model, an insensitive item refused, and Copy path is on the CLIPBOARD selection"
    );
    drop((menu_gone, ran_gone));
}
