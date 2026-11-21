//! The row context menu and Copy path on AppKit, built from the app's plan.
//!
//! The macOS sibling of `tests/row_menu_gtk.rs`: the items come from
//! `ff_gui::row_menu_plan`, the menu under test is the real `NSMenu` the adapter
//! builds, its titles and `isEnabled` are read back out of AppKit, an item is
//! fired through `performActionForItemAtIndex:` (the target/action dispatch a
//! click inside the menu runs), and Copy path is read back from `NSPasteboard`
//! with a call that is not the one that wrote it.
//!
//! The file's absence is simulated with the probe the app passes to
//! `row_menu_plan` (`Path::is_dir` for the second menu, where the row's
//! directory still exists and the file does not), so `Open`/`Open with…` come
//! out insensitive and the refusal is exercised without launching a helper.
//! Nothing here opens an application: the sensitive item that is fired is Copy
//! path, and an insensitive one cannot fire at all.
//!
//! AppKit's classes are not loaded in a process that never touched the backend,
//! and the framework wants the process's main thread, which the normal test
//! harness cannot give a test — hence `harness = false` and its own `fn main`
//! (see `[[test]]` in `Cargo.toml`), as with the other AppKit binaries here.

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    appkit::run()
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    // The NSMenu and the pasteboard are AppKit's; the GTK counterpart is
    // `tests/row_menu_gtk.rs`.
    println!("row_menu_mac: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
mod appkit {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::path::Path;
    use std::rc::Rc;

    use ff_gui::{row_menu_plan, run_row_action, RowAction};
    use rswidgets::compat::ContextMenu;
    use rswidgets::prelude::App;

    fn send0(obj: *mut c_void, sel: &str) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send0i(obj: *mut c_void, sel: &str) -> isize {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send1i(obj: *mut c_void, sel: &str, arg: isize) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, isize) -> *mut c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel), arg) }
    }
    fn send1p(obj: *mut c_void, sel: &str, arg: *mut c_void) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel), arg) }
    }

    /// The pasteboard's text, read with `NSPasteboard`'s own call.
    ///
    /// Deliberately not `compat::clipboard_get_text`: Copy writes through the
    /// adapter, so this asks the pasteboard itself, the way another application
    /// would.
    fn pasteboard_text() -> Option<String> {
        let pasteboard = send0(
            rswidgets::backends::apple::cls("NSPasteboard"),
            "generalPasteboard",
        );
        if pasteboard.is_null() {
            return None;
        }
        let kind = rswidgets::backends::apple::nsstring("public.utf8-plain-text");
        rswidgets::backends::apple::nsstring_to_rust(send1p(pasteboard, "stringForType:", kind))
    }

    /// Add a plan's items to a menu: one per model entry, at that entry's
    /// sensitivity, each running the action through the app's runner.
    ///
    /// The menu is filled in place rather than built in here and returned: a
    /// menu is inspected and fired while the value the app holds stays put.
    fn fill_menu(
        menu: &mut ContextMenu,
        target: &str,
        items: &[ff_gui::RowMenuItem],
    ) -> Rc<RefCell<Vec<String>>> {
        let ran: Rc<RefCell<Vec<String>>> = Default::default();
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
        ran
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        // The application object comes first: it loads AppKit and creates the
        // process's main-thread application, which the menu's target hangs off.
        let app = rswidgets::App::init()?;
        let win = app.create_window()?;
        win.set_title("ff-gui row menu test");
        win.present();

        // A real file on disk, so the copy round trip is a real path rather than
        // a string nothing would find.
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("ff-gui-row-menu.txt");
        std::fs::write(&file, "row menu\n")?;
        let path = file.to_string_lossy().into_owned();
        let rows = vec![path.clone()];

        // --- The row's file exists: every item the plan offers is sensitive ---
        let plan = row_menu_plan(3, Some(0), &rows, |p| Path::new(p).exists())
            .ok_or("a right-click on a row produced no menu")?;
        assert_eq!(
            plan.0, path,
            "the plan targets {:?}, not the row's path {:?}",
            plan.0, path
        );
        let mut menu = ContextMenu::new();
        let ran = fill_menu(&mut menu, &plan.0, &plan.1);
        let handle = menu.build().expect("the row menu could not be built");
        let shown = ContextMenu::items_of(handle);
        println!("row menu: NSMenu items {shown:?}");
        assert_eq!(
            shown.len(),
            6,
            "the NSMenu does not offer the model's six actions: {shown:?}"
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
            "the NSMenu titles are not the shared plan's"
        );
        assert_eq!(
            shown.iter().map(|(_, enabled)| *enabled).collect::<Vec<_>>(),
            plan.1.iter().map(|item| item.enabled).collect::<Vec<_>>(),
            "the NSMenu's isEnabled is not the shared plan's"
        );
        for (label, enabled) in &shown {
            assert!(
                *enabled,
                "the NSMenuItem {label:?} is insensitive although the row's file exists"
            );
        }

        // Every item's target and action: the wiring a click goes through, so a
        // nil target or a different selector means a menu that shows and does
        // nothing.
        let menu_obj = handle as *mut c_void;
        assert!(!menu_obj.is_null(), "the row menu handle is null");
        let array = send0(menu_obj, "itemArray");
        let count = send0i(array, "count") as usize;
        assert_eq!(count, plan.1.len(), "AppKit's NSMenu holds {count} items");
        for index in 0..count {
            let item = send1i(array, "objectAtIndex:", index as isize);
            assert!(!item.is_null(), "item {index} is null");
            let title = rswidgets::backends::apple::nsstring_to_rust(send0(item, "title"))
                .unwrap_or_default();
            let target = send0(item, "target");
            assert!(
                !target.is_null(),
                "item {index} ({title:?}) has no target, so choosing it would do nothing"
            );
            let class = rswidgets::backends::apple::nsstring_to_rust(send0(target, "className"))
                .unwrap_or_default();
            assert_eq!(
                class, "CorroMacMenuTarget",
                "item {index} ({title:?}) targets {class:?}, not the adapter's menu target"
            );
            assert_eq!(
                send0(item, "action"),
                rswidgets::backends::apple::selector("corroFired:"),
                "item {index} ({title:?}) does not send corroFired: to its target"
            );
            println!("row menu: item {index} {title:?} -> CorroMacMenuTarget/corroFired:");
        }

        // Copy path is the sensitive item this test fires: it dispatches through
        // AppKit's own item dispatch and must leave the row's path on the
        // pasteboard.
        let copy = plan
            .1
            .iter()
            .position(|item| item.action == RowAction::CopyPath)
            .ok_or("the plan has no Copy path item")?;
        assert!(
            ContextMenu::fire(handle, copy),
            "Copy path ({}) could not be fired",
            plan.1[copy].label
        );
        let lines = ran.borrow().clone();
        println!("row menu: fired {lines:?}");
        assert_eq!(
            lines,
            vec![format!("copied {path}")],
            "choosing Copy path did not run its action"
        );
        assert_eq!(
            rswidgets::compat::clipboard_get_text().unwrap_or_default(),
            path.as_str(),
            "the clipboard does not hold the row's path after Copy path"
        );
        let read_back = pasteboard_text().ok_or("NSPasteboard holds no text after Copy path")?;
        println!("row menu: pasteboard holds {read_back:?}");
        assert_eq!(
            read_back, path,
            "Copy path did not leave the row's path on NSPasteboard"
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
            .ok_or("a right-click on a row produced no menu")?;
        let mut menu_gone = ContextMenu::new();
        let ran_gone = fill_menu(&mut menu_gone, &gone.0, &gone.1);
        let handle_gone = menu_gone
            .build()
            .expect("the row menu for a missing file could not be built");
        let shown_gone = ContextMenu::items_of(handle_gone);
        println!("row menu: a missing file shows {shown_gone:?}");
        assert_eq!(
            shown_gone,
            gone.1
                .iter()
                .map(|item| (item.label.to_string(), item.enabled))
                .collect::<Vec<_>>(),
            "the NSMenu does not match the plan for a row whose file is gone"
        );
        let open = gone
            .1
            .iter()
            .position(|item| item.action == RowAction::Open)
            .ok_or("the plan has no Open item")?;
        let open_with = gone
            .1
            .iter()
            .position(|item| item.action == RowAction::OpenWith)
            .ok_or("the plan has no Open with… item")?;
        assert!(
            !shown_gone[open].1 && !shown_gone[open_with].1,
            "Open and Open with… stay sensitive for a missing file: {shown_gone:?}"
        );
        assert_eq!(
            shown_gone.iter().filter(|(_, enabled)| !*enabled).count(),
            2,
            "more than the two file actions went insensitive: {shown_gone:?}"
        );

        // An insensitive item refuses: AppKit's dispatch does not reach the
        // action, so nothing opens and nothing is recorded.
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
        let copy_gone = gone
            .1
            .iter()
            .position(|item| item.action == RowAction::CopyPath)
            .ok_or("the plan has no Copy path item")?;
        assert!(
            ContextMenu::fire(handle_gone, copy_gone),
            "Copy path could not be fired for a row whose file is gone"
        );
        assert_eq!(
            ran_gone.borrow().clone(),
            vec![format!("copied {path}")],
            "Copy path on a missing file did not run its action"
        );
        assert_eq!(
            pasteboard_text().as_deref(),
            Some(path.as_str()),
            "Copy path on a missing file left the wrong text on the pasteboard"
        );
        println!(
            "row menu: missing file → {shown_gone:?}; Open refused, Copy path put {path} on the pasteboard"
        );

        drop((menu_gone, ran_gone));
        // The first menu stays alive until here: its items are the ones whose
        // dispatch ran above.
        drop((menu, ran));
        println!(
            "row menu: ok — six items from the shared plan on the real NSMenu, isEnabled from the model, an insensitive item refused, and Copy path is on NSPasteboard"
        );
        Ok(())
    }
}
