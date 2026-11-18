//! Tests for the typed headless [`Harness`] over the `zork` model.
//!
//! These exercise the same in-memory model the REPL drives, but without a string
//! protocol: clicks/toggles fire callbacks synchronously, and state is asserted
//! through typed getters or a JSON snapshot. This is the recommended replacement
//! for a JSON backend.

#![cfg(feature = "zork")]

use std::cell::RefCell;
use std::rc::Rc;

use rswidgets::backends::zork::harness::Harness;

#[test]
fn button_click_fires_callback_and_updates_label() {
    let h = Harness::new();
    let label = h.create_label("count: 0");
    let btn = h.create_button("inc");

    let counter = Rc::new(RefCell::new(0));
    {
        let c = counter.clone();
        let l = label.clone();
        btn.on_click(move || {
            let n = *c.borrow() + 1;
            *c.borrow_mut() = n;
            l.set_text(&format!("count: {}", n));
        });
    }

    assert_eq!(h.label_text(&label), Some("count: 0".to_string()));
    h.click(&btn);
    h.click(&btn);
    h.click(&btn);
    assert_eq!(h.label_text(&label), Some("count: 3".to_string()));
    assert_eq!(*counter.borrow(), 3);
}

#[test]
fn entry_type_into_fires_changed_and_sets_text() {
    let h = Harness::new();
    let entry = h.create_entry();
    let seen = Rc::new(RefCell::new(String::new()));
    {
        let s = seen.clone();
        let e = entry.clone();
        entry.connect_changed(move || {
            *s.borrow_mut() = e.id().to_string();
        });
    }
    h.type_into(&entry, "hello world");
    assert_eq!(h.entry_text(&entry), Some("hello world".to_string()));
    assert!(!seen.borrow().is_empty());
}

#[test]
fn checkbutton_toggle_updates_state() {
    let h = Harness::new();
    let cb = h.create_checkbutton("enable");
    assert!(!h.checkbutton_checked(&cb));
    h.toggle(&cb);
    assert!(h.checkbutton_checked(&cb));
    h.toggle(&cb);
    assert!(!h.checkbutton_checked(&cb));
}

#[test]
fn dropdown_selection_and_getters() {
    let h = Harness::new();
    let dd = h.create_dropdown(&["a", "b", "c"]);
    dd.set_active(Some(1));
    assert_eq!(h.dropdown_selected(&dd), 1);
    dd.set_items(&["x", "y"]);
    dd.set_active(Some(1));
    assert_eq!(h.dropdown_selected(&dd), 1);
    // `None` clears the selection, matching the GTK/NWG signature.
    dd.set_active(None);
    assert_eq!(h.dropdown_selected(&dd), -1);
}

#[test]
fn menu_items_and_select() {
    let h = Harness::new();
    let menu = h.create_menu();
    menu.append("Open", "app.open");
    menu.append("Save", "app.save");
    assert_eq!(h.menu_items(&menu).len(), 2);

    let fired = Rc::new(RefCell::new(false));
    {
        let f = fired.clone();
        menu.on_click(move || *f.borrow_mut() = true);
    }
    h.select(&menu, 1);
    assert!(*fired.borrow());

    // Out-of-range select must be ignored (no panic, no fire).
    *fired.borrow_mut() = false;
    h.select(&menu, 99);
    assert!(!*fired.borrow());
}

#[test]
fn snapshot_is_serializable_json() {
    let h = Harness::new();
    let win = h.create_window();
    win.set_title("Demo");
    let lbl = h.create_label("hello");
    h.set_child(&win, &lbl);

    let json = h.snapshot_json();
    // The window title and label text must be present in the snapshot.
    assert!(json.contains("Demo"), "snapshot missing window title: {json}");
    assert!(json.contains("hello"), "snapshot missing label text: {json}");

    // Valid JSON round-trips.
    let value: serde_json::Value = serde_json::from_str(&json).expect("snapshot is valid JSON");
    assert!(value.get("nodes").is_some());
    assert!(value.get("current_id").is_some());
}

// ---------------------------------------------------------------------------
// The model grew a property bag, a real caret, menu action dispatch, pointer
// routing, a draw surface and a scrollable viewport. These cover the harness
// actions and getters that expose them.
// ---------------------------------------------------------------------------

#[test]
fn properties_are_recorded_and_readable() {
    let h = Harness::new();
    let b = h.create_button("x");
    assert!(h.visible(&b), "widgets start visible");

    b.set_visible(false);
    b.set_size_request(120, 40);
    b.set_hexpand(true);
    b.set_margin_start(6);
    b.add_class("primary");
    b.add_class("primary");

    assert!(!h.visible(&b));
    assert_eq!(h.size_request(&b), Some((Some(120), Some(40))));
    assert!(h.hexpand(&b));
    assert_eq!(h.margin_start(&b), 6);
    assert_eq!(h.classes(&b), vec!["primary".to_string()], "add_class dedupes");

    b.remove_class("primary");
    assert!(!h.has_class(&b, "primary"));
}

#[test]
fn hidden_widgets_take_no_space_in_a_box() {
    let h = Harness::new();
    h.create_window();
    let bx = h.create_box(true, 0);
    let a = h.create_button("a");
    let b = h.create_button("b");
    h.append(&bx, &a);
    h.append(&bx, &b);
    a.set_size_request(10, 6);
    b.set_size_request(20, 6);

    a.set_visible(false);
    assert_eq!(h.measure_box(&bx), Some((20, 6)), "only the visible child counts");

    h.layout_box(&bx, 0, 0, 100, 20);
    assert_eq!(h.offset(&b), Some((Some(0), Some(0))), "the hidden child takes no room");
}

#[test]
fn focus_moves_between_widgets() {
    let h = Harness::new();
    h.create_window();
    let a = h.create_entry();
    let b = h.create_entry();
    assert_eq!(h.focused(), None);
    a.grab_focus();
    assert!(h.has_focus(&a));
    assert_eq!(h.focused(), Some(a.id()));
    b.grab_focus();
    assert!(h.has_focus(&b));
    assert!(!h.has_focus(&a));
}

#[test]
fn focus_refused_for_hidden_widget() {
    let h = Harness::new();
    h.create_window();
    let e = h.create_entry();
    e.set_visible(false);
    e.grab_focus();
    assert_eq!(h.focused(), None);
}

#[test]
fn entry_caret_is_tracked() {
    let h = Harness::new();
    h.create_window();
    let e = h.create_entry();
    h.type_into(&e, "hello");
    assert_eq!(h.entry_position(&e), Some(5));
    e.set_position(2);
    assert_eq!(h.entry_position(&e), Some(2));
    e.set_position(99);
    assert_eq!(h.entry_position(&e), Some(5), "clamped to the buffer length");
}

#[test]
fn radio_group_is_exclusive() {
    let h = Harness::new();
    h.create_window();
    let a = h.create_radiobutton(None, "a");
    let b = h.create_radiobutton(Some(&a), "b");
    a.set_checked(true);
    assert!(h.radiobutton_checked(&a));
    b.set_checked(true);
    assert!(h.radiobutton_checked(&b));
    assert!(!h.radiobutton_checked(&a), "checking one clears the group");
}

#[test]
fn select_item_dispatches_the_named_action() {
    let h = Harness::new();
    h.create_window();
    let action = h.create_simple_action("app.open");
    let menu = h.create_menu();
    menu.append("Open", "app.open");

    let fired = Rc::new(RefCell::new(0));
    {
        let f = fired.clone();
        action.on_click(move || *f.borrow_mut() += 1);
    }
    assert_eq!(h.select_item(&menu, 0), Some("Open".to_string()));
    assert_eq!(*fired.borrow(), 1, "the action name resolved to a real action");
    assert_eq!(h.select_item(&menu, 9), None, "out of range");
}

#[test]
fn menubar_dispatches_through_the_model() {
    let h = Harness::new();
    h.create_window();
    let action = h.create_simple_action("app.save");
    let menu = h.create_menu();
    menu.append("Save", "app.save");
    let bar = h.create_menubar(&menu);
    assert_eq!(bar.items().len(), 1);

    let fired = Rc::new(RefCell::new(false));
    {
        let f = fired.clone();
        action.on_click(move || *f.borrow_mut() = true);
    }
    assert_eq!(h.select_item(&bar, 0), Some("Save".to_string()));
    assert!(*fired.borrow());
}

#[test]
fn check_menu_item_toggles_on_select() {
    let h = Harness::new();
    h.create_window();
    let menu = h.create_menu();
    let idx = menu.append_check("Wrap", "app.wrap", false);
    assert!(!menu.items()[idx].checked);
    h.select_item(&menu, idx);
    assert!(menu.items()[idx].checked);
    h.select_item(&menu, idx);
    assert!(!menu.items()[idx].checked);
}

#[test]
fn separator_is_not_selectable() {
    let h = Harness::new();
    h.create_window();
    let menu = h.create_menu();
    menu.append_separator("");
    menu.append("Open", "app.open");
    assert_eq!(h.select_item(&menu, 0), None);
    assert_eq!(h.select_item(&menu, 1), Some("Open".to_string()));
}

#[test]
fn dialog_response_reaches_the_handler_with_its_id() {
    let h = Harness::new();
    h.create_window();
    let d = h.create_dialog();
    let seen = Rc::new(RefCell::new(Vec::new()));
    {
        let s = seen.clone();
        d.connect_response(move |r| s.borrow_mut().push(r));
    }
    d.add_button("OK", 1);
    d.add_button("Cancel", 2);
    h.respond(&d, 1);
    assert_eq!(*seen.borrow(), vec![1]);
    h.respond(&d, 7);
    assert_eq!(*seen.borrow(), vec![1], "an unregistered response never fires");
    assert_eq!(h.dialog_buttons(&d).len(), 2);
}

#[test]
fn dialog_close_is_observable() {
    let h = Harness::new();
    h.create_window();
    let d = h.create_dialog();
    assert!(!h.is_destroyed(&d));
    d.close();
    assert!(h.is_destroyed(&d));
}

#[test]
fn canvas_draw_callback_is_run_against_a_recorder() {
    use rswidgets::backends::headless::DrawOp;
    let h = Harness::new();
    h.create_window();
    let c = h.create_canvas();
    c.set_content_size(64, 32);
    c.set_draw_callback(Box::new(|ctx, w, h| {
        assert_eq!((w, h), (64, 32));
        ctx.fill_rect(0.0, 0.0, 4.0, 4.0, 1.0, 0.0, 0.0, 1.0);
        ctx.draw_text(0.0, 0.0, "tick", "sans", 10.0, 0.0, 0.0, 0.0, 1.0);
    }));
    let ops = h.draw(&c);
    assert!(matches!(ops[0], DrawOp::FillRect { .. }));
    assert!(ops.iter().any(|o| matches!(o, DrawOp::Text { text, .. } if text == "tick")));
    assert_eq!(h.canvas_size(&c), (64, 32));
    c.clear_draw_callback();
    assert!(h.draw(&c).is_empty());
}

#[test]
fn canvas_click_and_key_actions_route_to_hooks() {
    let h = Harness::new();
    h.create_window();
    let c = h.create_canvas();
    let seen = Rc::new(RefCell::new(Vec::new()));
    {
        let s = seen.clone();
        c.on_click_at(Box::new(move |x, y| s.borrow_mut().push(format!("click {}", x as i32))));
    }
    {
        let s = seen.clone();
        c.on_key(Box::new(move |k| {
            s.borrow_mut().push(format!("key {}", k));
            true
        }));
    }
    h.click_at(&c, 5.0, 6.0);
    assert!(h.key(&c, 7));
    assert_eq!(*seen.borrow(), vec!["click 5".to_string(), "key 7".to_string()]);
}

#[test]
fn scroll_action_drives_the_handler() {
    let h = Harness::new();
    h.create_window();
    let sw = h.create_scrolled_window();
    let seen = Rc::new(RefCell::new(Vec::new()));
    {
        let s = seen.clone();
        sw.on_scroll(Box::new(move |v, p| s.borrow_mut().push((v, p))));
    }
    h.scroll(&sw, true, 12.0);
    assert_eq!(*seen.borrow(), vec![(true, 12.0)]);
    assert_eq!(h.scroll_offsets(&sw), (0.0, 12.0));
}

#[test]
fn scroll_to_is_clamped_to_the_document() {
    let h = Harness::new();
    h.create_window();
    let sw = h.create_scrolled_window();
    sw.scroll_to(0.0, 100.0, 20.0, 999.0, 100.0, 20.0);
    assert_eq!(h.scroll_offsets(&sw), (0.0, 80.0), "cannot scroll past the end");
}

#[test]
fn overlay_base_and_layers_are_distinct() {
    let h = Harness::new();
    h.create_window();
    let ov = h.create_overlay();
    let base = h.create_label("base");
    let layer = h.create_button("menu");
    ov.set_child(&base);
    h.add_overlay(&ov, &layer);
    assert_eq!(h.children(&ov), vec![base.id()]);
    assert_eq!(h.overlay_layers(&ov), vec![layer.id()]);
}

#[test]
fn grid_attach_records_the_cell() {
    let h = Harness::new();
    h.create_window();
    let g = h.create_grid();
    let b = h.create_button("b");
    h.attach(&g, &b, 2, 3, 1, 1);
    assert_eq!(h.grid_dimensions(&g), (3, 4));
    assert_eq!(h.offset(&b), Some((Some(2), Some(3))));
}

#[test]
fn tree_integrity_holds_under_reparenting() {
    let h = Harness::new();
    h.create_window();
    let bx1 = h.create_box(false, 0);
    let bx2 = h.create_box(false, 0);
    let c = h.create_label("c");
    h.append(&bx1, &c);
    h.set_child(&bx2, &c);
    assert!(h.children(&bx1).is_empty());
    assert_eq!(h.children(&bx2), vec![c.id()]);
    assert_eq!(h.parent(&c), Some(bx2.id()));
}

#[test]
fn spreadsheet_cells_round_trip() {
    let h = Harness::new();
    h.create_window();
    let ss = h.create_spreadsheet();
    ss.set_cell(1, 1, "42");
    ss.set_raw_cell(1, 2, "=1+1");
    ss.set_border_title("Sheet1");
    assert_eq!(h.sheet_cell(&ss, 1, 1), Some("42".to_string()));
    assert_eq!(h.sheet_cell(&ss, 1, 2), Some("=1+1".to_string()));
    assert_eq!(ss.border_title(), Some("Sheet1".to_string()));
}

#[test]
fn snapshot_records_properties_so_regressions_are_visible() {
    let h = Harness::new();
    h.create_window();
    let b = h.create_button("x");
    b.set_visible(false);
    b.set_size_request(10, 20);
    b.add_class("flat");
    let json = h.snapshot_json();
    // These used to be invisible to a snapshot test, so a regression in
    // geometry or visibility could not be caught at all.
    assert!(json.contains("\"visible\":false"), "visibility in snapshot: {json}");
    assert!(json.contains("\"flat\""), "classes in snapshot: {json}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let node = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["label"] == "x")
        .expect("button node in snapshot");
    assert_eq!(node["props"]["width"], 10);
    assert_eq!(node["props"]["height"], 20);
}
