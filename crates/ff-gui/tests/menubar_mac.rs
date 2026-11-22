//! The menu bar on AppKit, driven through the adapter's own install path.
//!
//! The counterpart of `tests/menubar_gtk.rs`: the bar is built from the app's
//! real model (`ff_gui::menubar_model()`, every item's `SimpleAction` registered
//! and `connect_activate`d exactly as `main.rs` does), then installed with
//! `App::new_menubar`, which on this backend really runs
//! `install_main_menu` → `[NSApplication setMainMenu:]` with a
//! `CorroMacMenuTarget` as each item's target.
//!
//! The menu is then read back out of AppKit — the main menu, each submenu and
//! each item's title and `isEnabled` — and an item is *chosen* the way a click
//! chooses it: `performActionForItemAtIndex:` sends the item's action to its
//! target, `-corroFired:`, which dispatches into the registry the Rust closures
//! live in. What that proves is not just "the closure ran": the action changes
//! widget state the user can see (View → Advanced Search expands the foldout,
//! Search → Clear Query empties the entry), so the assertion is on the state the
//! action owns.
//!
//! AppKit needs the main thread, which the normal test harness cannot give a
//! test (it runs each one on a spawned thread), so this target has
//! `harness = false` and its own `fn main` — see `[[test]]` in `Cargo.toml`.

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::c_void;
    use std::rc::Rc;

    use ff_gui::MenuAction;
    use rswidgets::compat::Expander;
    use rswidgets::prelude::*;

    // --- The few AppKit sends this test needs, by selector ---
    //
    // Deliberately AppKit's own calls rather than anything the adapter exports:
    // reading the tree back through `-mainMenu`, `-submenu`, `-title`,
    // `-isEnabled` and `-performActionForItemAtIndex:` is what a person opening
    // the menu and picking an item makes AppKit do.
    fn send0(obj: *mut c_void, sel: &str) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send_int(obj: *mut c_void, sel: &str) -> isize {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn send_bool(obj: *mut c_void, sel: &str) -> bool {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) -> bool =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }
    fn item_at(menu: *mut c_void, index: usize) -> *mut c_void {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, isize) -> *mut c_void =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe {
            f(
                menu,
                rswidgets::backends::apple::selector("itemAtIndex:"),
                index as isize,
            )
        }
    }
    fn choose(menu: *mut c_void, index: usize) {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, isize) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe {
            f(
                menu,
                rswidgets::backends::apple::selector("performActionForItemAtIndex:"),
                index as isize,
            )
        }
    }
    fn title_of(item: *mut c_void) -> String {
        rswidgets::backends::apple::nsstring_to_rust(send0(item, "title")).unwrap_or_default()
    }

    // AppKit's own classes before any of them is touched (`App::init` alone does
    // not load the framework; `new_menubar` would, but the window comes first).
    rswidgets::backends_macos_adapter::ensure_runtime_classes();
    let app = App::init()?;
    let win = app.create_window()?;
    win.set_title("ff-gui menubar test");

    // The two pieces of state the menu is supposed to change: the foldout View →
    // Advanced Search expands, and Search → Clear Query empties the entry.
    let advanced = Expander::new("Advanced search")?;
    let entry = app.create_entry()?;
    entry.set_text("stale query");
    advanced.set_child(&entry);
    let root = app.create_box(Orientation::Vertical, 2)?;
    root.append(&advanced);
    win.set_child(&root);
    win.present();

    // The app's own bar, built the way `main.rs` builds it: one `SimpleAction`
    // per model entry, `connect_activate`d to a closure that does that action's
    // work, registered, then named from the submenu as `app.<name>`.
    let ran: Rc<std::cell::RefCell<Vec<String>>> = Default::default();
    let group = app.ensure_action_group()?;
    let mut model = app.new_menu()?;
    for (menu_label, items) in ff_gui::menubar_model() {
        let mut sub = app.new_menu()?;
        for (label, action) in items {
            let ran = ran.clone();
            let advanced = advanced.clone();
            let entry = entry.clone();
            let act = app.new_simple_action(action.action_name())?;
            act.connect_activate(move |_| {
                ran.borrow_mut().push(label.to_string());
                match action {
                    MenuAction::ToggleAdvanced => advanced.set_expanded(!advanced.is_expanded()),
                    MenuAction::ClearQuery => entry.set_text(""),
                    _ => {}
                }
            })?;
            app.register_action(&act)?;
            sub.append(label, &format!("app.{}", action.action_name()));
        }
        model.append_submenu(menu_label, &sub);
    }
    let _bar = app.new_menubar(&model, group)?;

    // --- The bar is installed on the application ---
    let nsapp = send0(
        rswidgets::backends::apple::cls("NSApplication"),
        "sharedApplication",
    );
    assert!(!nsapp.is_null(), "there is no NSApplication");
    let main_menu = send0(nsapp, "mainMenu");
    assert!(
        !main_menu.is_null(),
        "the menu bar was not installed as the application's main menu"
    );

    // --- Every menu, item and title the model names is there and enabled ---
    let expected = ff_gui::menubar_model();
    let menus = send_int(main_menu, "numberOfItems");
    println!(
        "menubar: mainMenu has {menus} menu(s), expected {}",
        expected.len()
    );
    assert_eq!(
        menus as usize,
        expected.len(),
        "the installed bar has {menus} menus, not the model's {}",
        expected.len()
    );
    for (index, (menu_label, items)) in expected.iter().enumerate() {
        let holder = item_at(main_menu, index);
        assert!(!holder.is_null(), "menu {index} is missing");
        let holder_title = title_of(holder);
        assert_eq!(
            &holder_title, menu_label,
            "menu {index} is titled {holder_title:?}"
        );
        let sub = send0(holder, "submenu");
        assert!(!sub.is_null(), "{menu_label} has no submenu");
        let count = send_int(sub, "numberOfItems") as usize;
        assert_eq!(
            count,
            items.len(),
            "{menu_label} holds {count} items, not the model's {}",
            items.len()
        );
        let mut titles = Vec::new();
        for (item_index, (label, _)) in items.iter().enumerate() {
            let item = item_at(sub, item_index);
            assert!(!item.is_null(), "{menu_label}/item {item_index} is missing");
            let title = title_of(item);
            assert_eq!(&title, label, "{menu_label}/{item_index} is titled {title:?}");
            assert!(
                send_bool(item, "isEnabled"),
                "{menu_label}/{label} is disabled, so a user could not choose it"
            );
            // The item must be wired to the adapter's target and action, not
            // merely enabled: an item with a nil target, a target of the wrong
            // class, or a different action would look live and do nothing.
            let target = send0(item, "target");
            assert!(
                !target.is_null(),
                "{menu_label}/{label} has no target, so its action would go nowhere"
            );
            let target_class = rswidgets::backends::apple::nsstring_to_rust(send0(
                target,
                "className",
            ))
            .unwrap_or_default();
            assert_eq!(
                target_class, "CorroMacMenuTarget",
                "{menu_label}/{label} targets {target_class:?}, not the adapter's menu target, \
                 so the click would not reach Rust"
            );
            assert_eq!(
                send0(item, "action"),
                rswidgets::backends::apple::selector("corroFired:"),
                "{menu_label}/{label} sends a different action than -corroFired:, so its \
                 registered closure would never run"
            );
            titles.push(title);
        }
        println!("menubar: {menu_label}: {titles:?} (all enabled)");
    }

    // --- Choosing an item through the target's action dispatch ---
    let submenu_named = |name: &str| -> *mut c_void {
        let index = expected
            .iter()
            .position(|(label, _)| *label == name)
            .expect("the model has no such menu");
        send0(item_at(main_menu, index), "submenu")
    };
    {
        let view = submenu_named("View");
        assert!(!advanced.is_expanded(), "the foldout started expanded");
        // The query is what the *other* chosen item changes, so leaving it
        // alone is how a dispatch that runs the wrong command is caught.
        entry.set_text("stale query");
        choose(view, 0);
        let opened = advanced.is_expanded();
        let query_after_view = entry.get_text().unwrap_or_default();
        println!(
            "menubar: chose View → {:?} -> foldout expanded {opened}, entry {query_after_view:?}, actions ran {:?}",
            title_of(item_at(view, 0)),
            ran.borrow()
        );
        assert!(
            opened,
            "choosing View → Advanced Search did not expand the foldout"
        );
        assert_eq!(
            query_after_view, "stale query",
            "choosing View → Advanced Search also changed the query, so the wrong action ran"
        );
        choose(view, 0);
        assert!(
            !advanced.is_expanded(),
            "choosing Advanced Search again did not collapse it"
        );
    }
    {
        let search = submenu_named("Search");
        let clear_index = expected
            .iter()
            .find(|(label, _)| *label == "Search")
            .and_then(|(_, items)| items.iter().position(|(l, _)| *l == "Clear Query"))
            .expect("Search has no Clear Query item");
        entry.set_text("stale query");
        // ... and Clear Query must not touch the foldout it does not own.
        advanced.set_expanded(true);
        choose(search, clear_index);
        let text = entry.get_text().unwrap_or_default();
        let foldout_after = advanced.is_expanded();
        println!(
            "menubar: chose Search → {:?} -> entry text {text:?}, foldout still {foldout_after}, actions ran {:?}",
            title_of(item_at(search, clear_index)),
            ran.borrow()
        );
        assert!(
            text.is_empty(),
            "choosing Search → Clear Query left the entry holding {text:?}"
        );
        assert!(
            foldout_after,
            "choosing Search → Clear Query also collapsed the foldout, so the wrong action ran"
        );
    }
    assert_eq!(
        *ran.borrow(),
        vec![
            "Advanced Search".to_string(),
            "Advanced Search".to_string(),
            "Clear Query".to_string()
        ],
        "the chosen items did not dispatch through the menu target: {:?}",
        ran.borrow()
    );

    // --- The band itself opens and shuts, without the menu ---
    //
    // AppKit has no `GtkExpander`, so the foldout's header is a drawn canvas
    // band; the host reports a click on it as a canvas id. This is the click a
    // person makes when they open Advanced Search by clicking it, and it has to
    // reach the same state the menu item drives.
    {
        let header = advanced
            .header_handle()
            .expect("the foldout has no header band on AppKit");
        let header_id = rswidgets::backends_macos_adapter::Canvas(header).canvas_id();
        assert_ne!(
            header_id, 0,
            "the header band has no canvas id, so a click on it goes nowhere"
        );
        advanced.set_expanded(false);
        rswidgets::backends_macos_adapter::dispatch_canvas_click(header_id, 12.0, 8.0);
        let opened = advanced.is_expanded();
        println!("menubar: clicked the Advanced search band -> foldout expanded {opened}");
        assert!(
            opened,
            "a click on the foldout's header band did not expand it, so the foldout \
             cannot be opened on this desktop without the menu"
        );
        rswidgets::backends_macos_adapter::dispatch_canvas_click(header_id, 12.0, 8.0);
        assert!(
            !advanced.is_expanded(),
            "a second click on the foldout's header band did not collapse it"
        );
    }

    println!("menubar: ok — installed, enabled, and dispatched through the target");
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    // The AppKit backend is the only one with an `NSMenu` to install; the GTK
    // counterpart is `tests/menubar_gtk.rs`.
    println!("menubar_mac: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
