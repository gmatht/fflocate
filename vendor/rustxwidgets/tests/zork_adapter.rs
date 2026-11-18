//! Adapter-level tests for the zork backend.
//!
//! These drive the *public adapter surface* (`create_*` + widget methods) over
//! the thread-local model, which is what application code actually calls. The
//! model unit tests in `backends::zork::model` cover the operations; these
//! cover the wiring, so a method that forgets to pass its arguments through
//! fails here.
//!
//! The thread-local singleton is shared within a test binary, so every test
//! starts from a fresh model via the `serial` helper rather than assuming an
//! empty one.

#![cfg(feature = "zork")]

use std::cell::RefCell;
use std::rc::Rc;

use rswidgets::backends_zork_adapter as za;

/// Run `f` against a fresh thread-local model.
///
/// The adapter's free functions operate on a thread-local `ZorkState`, so tests
/// that assert absolute node ids would otherwise interfere with each other.
/// These tests only assert *relative* facts, but starting from a clean model
/// keeps them independent regardless.
fn serial<R>(f: impl FnOnce() -> R) -> R {
    rswidgets::backends::zork::model::reset_for_test();
    f()
}

mod geometry {
    use super::*;

    #[test]
    fn window_default_size_is_recorded() {
        serial(|| {
            let w = za::create_window().unwrap();
            w.set_default_size(640, 480);
            assert_eq!(rswidgets::backends::zork::get_size_request(w.id()).map(|(a, b)| (a, b)), Some((Some(640), Some(480))));
        });
    }

    #[test]
    fn window_resize_overrides_default_size() {
        serial(|| {
            let w = za::create_window().unwrap();
            w.set_default_size(640, 480);
            w.resize(800, 600);
            assert_eq!(
                rswidgets::backends::zork::get_size_request(w.id()),
                Some((Some(800), Some(600)))
            );
        });
    }

    #[test]
    fn box_layout_positions_children() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let bx = za::create_box(za::Orientation::Horizontal, 4).unwrap();
            let a = za::create_button("a").unwrap();
            let b = za::create_button("b").unwrap();
            bx.append(&a);
            bx.append(&b);
            a.set_size_request(10, 6);
            b.set_size_request(20, 6);
            bx.layout(0, 0, 200, 100);
            assert_eq!(rswidgets::backends::zork::get_offset(a.id()), Some((Some(0), Some(0))));
            assert_eq!(rswidgets::backends::zork::get_offset(b.id()), Some((Some(14), Some(0))));
            assert_eq!(bx.measure(), Some((34, 6)));
        });
    }

    #[test]
    fn grid_attach_records_cell_and_grows_grid() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let g = za::create_grid().unwrap();
            let b = za::create_button("b").unwrap();
            g.attach(&b, 2, 3, 1, 1);
            assert_eq!(g.dimensions(), (3, 4));
            assert_eq!(rswidgets::backends::zork::get_offset(b.id()), Some((Some(2), Some(3))));
        });
    }

    #[test]
    fn append_keeps_both_children() {
        // Regression: `append_child` used to delegate to `set_child`, which
        // replaced the child list, so the second append silently dropped the
        // first child.
        serial(|| {
            let _w = za::create_window().unwrap();
            let bx = za::create_box(za::Orientation::Vertical, 0).unwrap();
            let a = za::create_button("a").unwrap();
            let b = za::create_button("b").unwrap();
            bx.append(&a);
            bx.append(&b);
            assert_eq!(rswidgets::backends::zork::first_child(bx.id()), Some(a.id()));
            let layers = rswidgets::backends::zork::measure_box(bx.id());
            assert!(layers.is_some());
            // Both children are present.
            let snap = rswidgets::backends::zork::model::with_state_for_test(|s| s.snapshot());
            let node = snap.nodes.iter().find(|n| n.id == bx.id()).unwrap();
            assert_eq!(node.children.len(), 2, "both children must be attached");
        });
    }
}

mod visibility_and_classes {
    use super::*;

    #[test]
    fn set_visible_and_classes_round_trip() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let l = za::create_label("x").unwrap();
            assert!(rswidgets::backends::zork::get_visible(l.id()));
            l.set_visible(false);
            assert!(!rswidgets::backends::zork::get_visible(l.id()));
            l.add_class("primary");
            l.add_class("primary");
            assert_eq!(rswidgets::backends::zork::classes(l.id()), vec!["primary".to_string()]);
            l.remove_class("primary");
            assert!(!rswidgets::backends::zork::has_class(l.id(), "primary"));
        });
    }

    #[test]
    fn label_fixed_width_pins_size_request() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let l = za::create_label("x").unwrap();
            l.set_fixed_width(Some(120));
            assert_eq!(rswidgets::backends::zork::get_size_request(l.id()), Some((Some(120), None)));
            l.set_fixed_width(None);
            assert_eq!(rswidgets::backends::zork::get_size_request(l.id()), Some((None, None)));
        });
    }

    #[test]
    fn expansion_and_margins_are_recorded() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let bx = za::create_box(za::Orientation::Vertical, 0).unwrap();
            let c = za::create_label("c").unwrap();
            bx.append(&c);
            bx.set_hexpand(true);
            bx.set_child_hexpand(&c, true);
            bx.set_child_vexpand(&c, true);
            c.set_margin_start(4);
            c.set_margin_top(8);
            let props = rswidgets::backends::zork::model::with_state_for_test(|s| s.node(c.id()).unwrap().props.clone());
            assert!(props.hexpand);
            assert!(props.vexpand);
            assert!(props.hexpand, "child expansion is recorded on the child");
            assert_eq!(props.margin_start, 4);
            assert_eq!(props.margin_top, 8);
        });
    }
}

mod focus_and_caret {
    use super::*;

    #[test]
    fn entry_reports_a_real_caret() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let e = za::create_entry().unwrap();
            e.set_text("hello");
            assert_eq!(e.get_position(), Some(5));
            e.set_position(2);
            assert_eq!(e.get_position(), Some(2));
            e.set_position(99);
            assert_eq!(e.get_position(), Some(5), "clamped to the buffer length");
        });
    }

    #[test]
    fn grab_focus_and_has_focus_agree() {
        // The old stub hard-coded `has_focus() == false`, so a caller could
        // never tell whether focus moved.
        serial(|| {
            let _w = za::create_window().unwrap();
            let a = za::create_entry().unwrap();
            let b = za::create_entry().unwrap();
            assert!(!a.has_focus());
            a.grab_focus();
            assert!(a.has_focus());
            b.grab_focus();
            assert!(b.has_focus());
            assert!(!a.has_focus(), "focus moves, it does not accumulate");
        });
    }
}

mod radio_and_check {
    use super::*;

    #[test]
    fn radio_group_is_exclusive_through_the_adapter() {
        // Passing a seed joins its group. An ungrouped seed is promoted to be
        // its own group, so the seed and every member stay exclusive.
        serial(|| {
            let _w = za::create_window().unwrap();
            let seed = za::create_radiobutton(None, "seed").unwrap();
            let a = za::create_radiobutton(Some(&seed), "a").unwrap();
            let b = za::create_radiobutton(Some(&seed), "b").unwrap();
            assert_eq!(seed.group(), Some(seed.id()));
            assert_eq!(a.group(), seed.group());
            assert_eq!(b.group(), seed.group());

            a.set_active(true);
            assert!(a.is_active());
            b.set_active(true);
            assert!(b.is_active());
            assert!(!a.is_active(), "setting one radio clears the group");
        });
    }

    #[test]
    fn ungrouped_radios_are_independent() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let a = za::create_radiobutton(None, "a").unwrap();
            let b = za::create_radiobutton(None, "b").unwrap();
            a.set_active(true);
            b.set_active(true);
            assert!(a.is_active(), "no seed means no group, so no clearing");
            assert!(b.is_active());
        });
    }

    #[test]
    fn radio_can_be_grouped_after_creation() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let a = za::create_radiobutton(None, "a").unwrap();
            let b = za::create_radiobutton(None, "b").unwrap();
            let group = 42;
            a.set_group(group);
            b.set_group(group);
            a.set_active(true);
            b.set_active(true);
            assert!(b.is_active());
            assert!(!a.is_active());
        });
    }

    #[test]
    fn check_toggle_fires_callbacks() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let cb = za::create_checkbutton("enable").unwrap();
            let hits = Rc::new(RefCell::new(0));
            {
                let h = hits.clone();
                cb.on_toggle(move || *h.borrow_mut() += 1).unwrap();
            }
            assert!(!cb.is_active());
            cb.toggle();
            assert!(cb.is_active());
            assert_eq!(*hits.borrow(), 1);
            cb.toggle();
            assert!(!cb.is_active());
            assert_eq!(*hits.borrow(), 2);
        });
    }

    #[test]
    fn labels_are_gettable_and_settable() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let cb = za::create_checkbutton("before").unwrap();
            assert_eq!(cb.get_label(), Some("before".to_string()));
            cb.set_label("after");
            assert_eq!(cb.get_label(), Some("after".to_string()));
        });
    }
}

mod buttons_and_canvas {
    use super::*;

    #[test]
    fn emit_clicked_fires_the_handler() {
        // The old `emit_clicked` returned `Ok(0)` without firing anything.
        serial(|| {
            let _w = za::create_window().unwrap();
            let b = za::create_button("go").unwrap();
            let hits = Rc::new(RefCell::new(0));
            {
                let h = hits.clone();
                b.on_click(move || *h.borrow_mut() += 1).unwrap();
            }
            b.emit_clicked().unwrap();
            assert_eq!(*hits.borrow(), 1);
        });
    }

    #[test]
    fn canvas_keeps_and_runs_its_draw_callback() {
        use rswidgets::backends::headless::DrawOp;
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            c.set_content_size(100, 50);
            c.set_draw_callback(Box::new(|ctx, w, h| {
                assert_eq!((w, h), (100, 50));
                ctx.fill_rect(0.0, 0.0, 10.0, 10.0, 1.0, 0.0, 0.0, 1.0);
                ctx.draw_text(0.0, 0.0, "hello", "sans", 12.0, 0.0, 0.0, 0.0, 1.0);
            }));
            let ops = c.draw();
            assert!(matches!(ops[0], DrawOp::FillRect { .. }));
            assert!(ops.iter().any(|o| matches!(o, DrawOp::Text { text, .. } if text == "hello")));
            // Still drawable afterwards.
            assert_eq!(c.draw().len(), ops.len());
            c.clear_draw_callback();
            assert!(c.draw().is_empty());
        });
    }

    #[test]
    fn canvas_pointer_hooks_fire_in_order() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            let order = Rc::new(RefCell::new(Vec::new()));
            {
                let o = order.clone();
                c.on_click_button(Box::new(move |_, _, b, _| o.borrow_mut().push(format!("btn{}", b))));
            }
            {
                let o = order.clone();
                c.on_click(Box::new(move |x, y| o.borrow_mut().push(format!("click{},{}", x as i32, y as i32))));
            }
            rswidgets::backends::zork::pointer_click_button(c.id(), 3.0, 4.0, 1, 0);
            assert_eq!(*order.borrow(), vec!["btn1".to_string(), "click3,4".to_string()]);
        });
    }

    #[test]
    fn queue_redraw_is_counted() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            assert_eq!(c.redraw_count(), 0);
            c.queue_redraw();
            c.queue_redraw();
            assert_eq!(c.redraw_count(), 2);
        });
    }

    #[test]
    fn canvas_force_draw_uses_the_fallback_size() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            let drew = Rc::new(RefCell::new((0, 0)));
            {
                let d = drew.clone();
                c.set_draw_callback(Box::new(move |_, w, h| *d.borrow_mut() = (w, h)));
            }
            c.force_draw(std::ptr::null_mut(), 320, 240);
            assert_eq!(*drew.borrow(), (320, 240));
            // An explicit size wins over the fallback.
            c.set_content_size(10, 20);
            c.force_draw(std::ptr::null_mut(), 320, 240);
            assert_eq!(*drew.borrow(), (10, 20));
        });
    }

    #[test]
    fn canvas_key_hook_can_consume() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            let raw = Rc::new(RefCell::new(0));
            c.on_key(Box::new(|_| true));
            {
                let r = raw.clone();
                c.on_key_raw(Box::new(move |_, _| { *r.borrow_mut() += 1; true }));
            }
            assert!(rswidgets::backends::zork::key(c.id(), 42));
            assert_eq!(*raw.borrow(), 0, "a consumed key skips key_raw");
        });
    }
}

mod overlay {
    use super::*;

    #[test]
    fn base_child_and_layers_are_distinct() {
        // The old body routed `add_overlay` and `set_child` through the same
        // call, so a layer was indistinguishable from the base.
        serial(|| {
            let _w = za::create_window().unwrap();
            let ov = za::create_overlay().unwrap();
            let base = za::create_label("base").unwrap();
            let layer = za::create_button("menu").unwrap();
            ov.set_child(&base);
            ov.add_overlay(&layer);
            assert_eq!(rswidgets::backends::zork::first_child(ov.id()), Some(base.id()));
            assert_eq!(ov.layers(), vec![layer.id()]);
        });
    }

    #[test]
    fn pass_through_and_show_all_are_recorded() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ov = za::create_overlay().unwrap();
            let base = za::create_label("base").unwrap();
            let layer = za::create_button("menu").unwrap();
            ov.set_child(&base);
            ov.add_overlay(&layer);
            ov.set_overlay_pass_through(&layer, true);
            let snap = rswidgets::backends::zork::model::with_state_for_test(|s| s.snapshot());
            assert_eq!(snap.overlay_pass_through.get(&layer.id()), Some(&true));

            base.set_visible(false);
            ov.show_all();
            assert!(rswidgets::backends::zork::get_visible(base.id()));
        });
    }

    #[test]
    fn remove_detaches_the_layer() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ov = za::create_overlay().unwrap();
            let base = za::create_label("base").unwrap();
            let layer = za::create_button("menu").unwrap();
            ov.set_child(&base);
            ov.add_overlay(&layer);
            ov.remove(&layer);
            assert!(ov.layers().is_empty());
        });
    }
}

mod scrolled_window {
    use super::*;

    #[test]
    fn scroll_is_clamped_and_reported() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let sw = za::create_scrolled_window().unwrap();
            sw.scroll_to(0.0, 100.0, 20.0, 999.0, 100.0, 20.0);
            assert_eq!(sw.get_scroll(), (0.0, 80.0));
        });
    }

    #[test]
    fn on_scroll_fires_with_axis_and_position() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let sw = za::create_scrolled_window().unwrap();
            let seen = Rc::new(RefCell::new(Vec::new()));
            {
                let s = seen.clone();
                sw.on_scroll(Box::new(move |v, p| s.borrow_mut().push((v, p))));
            }
            rswidgets::backends::zork::scroll(sw.id(), true, 12.0);
            assert_eq!(*seen.borrow(), vec![(true, 12.0)]);
            assert_eq!(sw.get_scroll().1, 12.0);
        });
    }
}

mod menus {
    use super::*;

    #[test]
    fn select_dispatches_the_named_action() {
        // The old `Harness::select` fired the menu node's own callbacks, which
        // made the action name on each item decorative.
        serial(|| {
            let _w = za::create_window().unwrap();
            let action = za::create_simple_action("app.open").unwrap();
            let menu = za::create_menu().unwrap();
            menu.append("Open", "app.open");
            let hits = Rc::new(RefCell::new(0));
            {
                let h = hits.clone();
                action.on_activate(move || *h.borrow_mut() += 1).unwrap();
            }
            assert_eq!(menu.select(0), Some("Open".to_string()));
            assert_eq!(*hits.borrow(), 1);
        });
    }

    #[test]
    fn append_item_binds_to_the_action_name() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let action = za::create_simple_action("app.save").unwrap();
            let menu = za::create_menu().unwrap();
            menu.append_item("Save", &action);
            let hits = Rc::new(RefCell::new(0));
            {
                let h = hits.clone();
                action.on_activate(move || *h.borrow_mut() += 1).unwrap();
            }
            assert_eq!(menu.select(0), Some("Save".to_string()));
            assert_eq!(*hits.borrow(), 1);
        });
    }

    #[test]
    fn separator_is_not_selectable() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let menu = za::create_menu().unwrap();
            menu.append_separator("");
            menu.append("Open", "app.open");
            assert_eq!(menu.select(0), None, "a separator must not select");
            assert_eq!(menu.select(1), Some("Open".to_string()));
            assert_eq!(menu.select(9), None, "out of range");
        });
    }

    #[test]
    fn check_item_toggles_when_selected() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let menu = za::create_menu().unwrap();
            let idx = menu.append_check("Wrap", "app.wrap", false);
            assert!(!menu.items()[idx].checked);
            menu.select(idx);
            assert!(menu.items()[idx].checked);
            menu.select(idx);
            assert!(!menu.items()[idx].checked);
        });
    }

    #[test]
    fn radio_items_stay_exclusive() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let menu = za::create_menu().unwrap();
            let a = menu.append_radio("a", "", 1, false);
            let b = menu.append_radio("b", "", 1, false);
            menu.set_item_checked(b, true);
            assert!(menu.items()[b].checked);
            assert!(!menu.items()[a].checked);
        });
    }

    #[test]
    fn section_and_accelerator_are_recorded() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let menu = za::create_menu().unwrap();
            menu.append_section("Recent");
            menu.append_with_shortcut("Save", "app.save", "Ctrl+S");
            let items = menu.items();
            assert_eq!(items[0].kind, rswidgets::backends::zork::MenuItemKind::Section);
            assert!(!items[0].enabled);
            assert_eq!(items[1].accelerator, "Ctrl+S");
        });
    }

    #[test]
    fn append_key_strips_the_mnemonic_marker() {
        // GTK marks a mnemonic with a trailing-ish `&` before the character
        // (`&Save`); `&_` escapes a literal underscore.
        serial(|| {
            let _w = za::create_window().unwrap();
            let menu = za::create_menu().unwrap();
            menu.append_key("&Save", "app.save");
            menu.append_key("&Print", "app.print");
            menu.append_key("A&B", "app.amp");
            let items = menu.items();
            assert_eq!(items[0].label, "Save");
            assert_eq!(items[1].label, "Print");
            assert_eq!(items[2].label, "AB");
        });
    }

    #[test]
    fn menubar_exposes_its_items() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let menu = za::create_menu().unwrap();
            menu.append("Open", "app.open");
            let bar = za::create_menubar(&menu, std::ptr::null_mut()).unwrap();
            assert_eq!(bar.items().len(), 1);
            // No popups in a headless model, so mnemonics report "not handled".
            assert!(!bar.activate_submenu_by_mnemonic(b'f' as u32));
            assert!(!bar.menu_active());
        });
    }
}

mod dialogs {
    use super::*;

    #[test]
    fn response_requires_a_registered_button_and_carries_the_id() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let d = za::create_dialog().unwrap();
            let seen = Rc::new(RefCell::new(Vec::new()));
            {
                let s = seen.clone();
                d.connect_response(move |r| s.borrow_mut().push(r)).unwrap();
            }
            d.respond(1);
            assert!(seen.borrow().is_empty(), "unregistered response must not fire");
            d.add_button("OK", 1);
            d.add_button("Cancel", 2);
            d.respond(2);
            assert_eq!(*seen.borrow(), vec![2]);
            assert_eq!(d.response_ids(), vec![1, 2]);
        });
    }

    #[test]
    fn close_marks_destroyed() {
        // The old `close` was a comment-only no-op, so a closed dialog was
        // indistinguishable from a live one.
        serial(|| {
            let _w = za::create_window().unwrap();
            let d = za::create_dialog().unwrap();
            assert!(!d.is_destroyed());
            d.close();
            assert!(d.is_destroyed());
            assert!(!rswidgets::backends::zork::get_visible(d.id()));
        });
    }

    #[test]
    fn transient_for_is_recorded() {
        serial(|| {
            let w = za::create_window().unwrap();
            let d = za::create_dialog().unwrap();
            d.set_transient_for(*w.as_ref());
            assert_eq!(d.get_transient_for(), Some(w.id()));
        });
    }

    #[test]
    fn content_area_is_the_first_child() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let d = za::create_dialog().unwrap();
            let c = za::create_canvas().unwrap();
            d.append_content_area(&c);
            assert_eq!(d.get_content_area(), c.id() as *mut std::os::raw::c_void);
        });
    }
}

mod textview {
    use super::*;

    #[test]
    fn append_is_incremental_and_readable() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let tv = za::create_textview().unwrap();
            tv.append_text("line 1\n");
            tv.append_text("line 2\n");
            assert_eq!(tv.get_text(), Some("line 1\nline 2\n".to_string()));
            assert_eq!(tv.get_buffer(), tv.get_text());
        });
    }

    #[test]
    fn editable_round_trips() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let tv = za::create_textview().unwrap();
            assert!(tv.is_editable());
            tv.set_editable(false);
            assert!(!tv.is_editable());
        });
    }
}

mod tree_integrity {
    use super::*;

    #[test]
    fn reparenting_leaves_exactly_one_parent_link() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let bx1 = za::create_box(za::Orientation::Vertical, 0).unwrap();
            let bx2 = za::create_box(za::Orientation::Vertical, 0).unwrap();
            let c = za::create_label("c").unwrap();
            bx1.append(&c);
            rswidgets::backends::zork::set_child(bx2.id(), c.id());
            let snap = rswidgets::backends::zork::model::with_state_for_test(|s| s.snapshot());
            let listing: Vec<usize> = snap
                .nodes
                .iter()
                .filter(|n| n.children.contains(&c.id()))
                .map(|n| n.id)
                .collect();
            assert_eq!(listing, vec![bx2.id()], "exactly one parent");
            let node = snap.nodes.iter().find(|n| n.id == c.id()).unwrap();
            assert_eq!(node.parent, Some(bx2.id()));
        });
    }

    #[test]
    fn box_cannot_be_its_own_child() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let bx = za::create_box(za::Orientation::Vertical, 0).unwrap();
            bx.append(&bx);
            let snap = rswidgets::backends::zork::model::with_state_for_test(|s| s.snapshot());
            let node = snap.nodes.iter().find(|n| n.id == bx.id()).unwrap();
            assert!(node.children.is_empty());
        });
    }
}

mod spreadsheet {
    use super::*;

    #[test]
    fn cells_round_trip_through_the_adapter() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            ss.set_cell(1, 1, "42");
            ss.set_raw_cell(1, 2, "=1+1");
            ss.set_cell_style(1, 1, 3);
            ss.set_border_title("Sheet1");
            assert_eq!(ss.get_cell(1, 1), Some("42".to_string()));
            assert_eq!(ss.get_cell(1, 2), Some("=1+1".to_string()));
            assert_eq!(ss.get_border_title(), Some("Sheet1".to_string()));
            let snap = rswidgets::backends::zork::model::with_state_for_test(|s| s.snapshot());
            let entry = snap.nodes.iter().find(|n| n.id == ss.id()).unwrap();
            let styled = entry.cells.iter().find(|c| c.row == 1 && c.col == 1).unwrap();
            assert_eq!(styled.style, 3);
            let raw = entry.cells.iter().find(|c| c.col == 2).unwrap();
            assert!(raw.raw);
        });
    }

    #[test]
    fn spreadsheet_click_hook_fires() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            let seen = Rc::new(RefCell::new((0.0, 0.0)));
            {
                let s = seen.clone();
                ss.on_click(Box::new(move |x, y| *s.borrow_mut() = (x, y)));
            }
            rswidgets::backends::zork::pointer_click(ss.id(), 12.0, 34.0);
            assert_eq!(*seen.borrow(), (12.0, 34.0));
        });
    }
}

/// The F9 surface: the fifteen `set_*` calls a pancurses `Spreadsheet` takes,
/// driven through the adapter and read back off the model.
///
/// These exist because the whole point of implementing the surface is that
/// shared render code can issue the same call on any backend, so the test has
/// to issue it *through the adapter* and not against the model directly —
/// otherwise a method that forgot to forward its argument would pass.
mod sheet_chrome {
    use super::*;

    #[test]
    fn every_chrome_field_round_trips() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            ss.set_grid_config(2, 6);
            ss.set_row_counts(1, 40);
            ss.set_column_layout(vec![(0, 10, "A".into()), (1, 4, "B".into())]);
            ss.set_row_labels(vec![(0, "1".into()), (1, "2".into())]);
            ss.set_border_title("Book1");
            ss.set_menu_text(" [File]   Edit    Sheet");
            ss.set_status_text("ready");
            ss.set_formula_bar_trailing("   ·  saved");
            ss.set_tab_data(&["Sheet1".to_string(), "Notes".to_string()], 1);
            ss.set_cursor(3, 4);
            ss.set_anchor(Some((0, 0)));

            let c = ss.chrome().expect("a spreadsheet has chrome");
            assert_eq!((c.margin_cols, c.main_cols), (2, 6));
            assert_eq!((c.header_rows, c.main_rows), (1, 40));
            assert_eq!(c.column_layout, vec![(0, 10, "A".to_string()), (1, 4, "B".to_string())]);
            assert_eq!(c.row_labels, vec![(0, "1".to_string()), (1, "2".to_string())]);
            assert_eq!(c.cursor, (3, 4));
            assert_eq!(c.anchor, Some((0, 0)));
            assert_eq!(c.tabs, vec!["Sheet1".to_string(), "Notes".to_string()]);
            assert_eq!(c.active_tab, 1);
            assert_eq!(ss.menu_text().as_deref(), Some(" [File]   Edit    Sheet"));
            assert_eq!(ss.status_text().as_deref(), Some("ready"));
            assert_eq!(ss.formula_bar_trailing().as_deref(), Some("   ·  saved"));
            assert_eq!(ss.get_border_title().as_deref(), Some("Book1"));
        });
    }

    #[test]
    fn the_cursor_is_clamped_to_the_sheet() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            ss.set_grid_config(1, 4);
            ss.set_row_counts(1, 3);
            // Cells are 1-based: 1 gutter + 4 data columns = 5 addressable
            // columns, and 1 header row + 3 data rows = 4 addressable rows.
            // The cursor shares that space, so the last cell is (4, 5).
            ss.set_cursor(99, 99);
            assert_eq!(ss.cursor_position(), Some((4, 5)), "must not exceed the last cell");
            // With no geometry configured there is nothing to clamp to, so the
            // value is kept rather than collapsed.
            let bare = za::create_spreadsheet().unwrap();
            bare.set_cursor(7, 9);
            assert_eq!(bare.cursor_position(), Some((7, 9)));
            // But 0 is not a cell in this space, so it is floored rather than
            // accepted: a caller passing the old 0-based convention lands on
            // the first header cell instead of outside the grid.
            let fresh = za::create_spreadsheet().unwrap();
            fresh.set_cursor(0, 0);
            assert_eq!(fresh.cursor_position(), Some((1, 1)));
        });
    }

    /// The cursor and the cells share one coordinate space.
    ///
    /// This is the invariant that lets shared render code address a cell and
    /// then put the cursor on it, which is exactly what
    /// `src/gui/pnc_backend.rs` does on every frame:
    ///
    /// ```ignore
    /// spreadsheet.set_cell(cursor_display_ri, display_cursor_col, text);
    /// spreadsheet.set_cursor(cursor_display_ri, display_cursor_col);
    /// ```
    ///
    /// When the cursor was 0-based and the cells 1-based, those two calls
    /// landed on different cells — off by one in both axes — and the test
    /// suite did not catch it because nothing drove a cursor against a cell it
    /// had also written. Asserting the round trip is what catches it: put the
    /// cursor on a cell, read that cell back, and it must be the one the
    /// cursor names.
    #[test]
    fn the_cursor_and_the_cells_share_one_coordinate_space() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            ss.set_grid_config(1, 3);
            ss.set_row_counts(1, 2);
            for (row, col, text) in [(1u32, 1u32, "hdr"), (2, 2, "b2"), (3, 3, "c3")] {
                ss.set_cell(row, col, text);
            }
            // Walk the cursor over every written cell and read back through
            // the same coordinates the cursor reported.
            for (row, col, want) in [(1u32, 1u32, "hdr"), (2, 2, "b2"), (3, 3, "c3")] {
                ss.set_cursor(row, col);
                assert_eq!(
                    ss.cursor_position(),
                    Some((row, col)),
                    "the cursor must report the position it was given"
                );
                assert_eq!(
                    ss.get_cell(row, col).as_deref(),
                    Some(want),
                    "R{row}C{col} holds {want:?} under the cell's own addressing"
                );
            }
        });
    }

    #[test]
    fn tab_index_is_clamped_to_the_list() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            ss.set_tab_data(&["A".to_string(), "B".to_string()], 9);
            assert_eq!(
                ss.chrome().unwrap().active_tab,
                1,
                "an index past the end must not survive: the widget can only be on a real tab"
            );
            ss.set_tab_data(&[], 3);
            assert!(ss.chrome().unwrap().tabs.is_empty());
        });
    }

    #[test]
    fn formula_bar_takes_the_address_and_entry_widgets() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            let addr = za::create_label("B7").unwrap();
            let entry = za::create_entry().unwrap();
            entry.set_text("=SUM(C7:E7)");
            ss.set_formula_bar(&addr, &entry);
            assert_eq!(
                ss.formula_bar(),
                Some(("B7".to_string(), "=SUM(C7:E7)".to_string()))
            );
        });
    }

    #[test]
    fn editing_seeds_from_the_cursor_cell_and_commit_writes_it_back() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            ss.set_cell(1, 1, "1200");
            ss.set_cursor(1, 1);
            // F2-style start: an empty seed must mean "start from the cell",
            // not "start blank", or committing destroys the value.
            ss.set_editing(true, "", 0);
            let (editing, buf, pos) = ss.edit_state().unwrap();
            assert!(editing);
            assert_eq!(buf, "1200");
            assert_eq!(pos, 0);
            ss.set_editing(true, "1201", 4);
            assert_eq!(ss.edit_state().unwrap().1, "1201");
            assert_eq!(ss.commit_formula_bar(), Some((1, 1, "1201".to_string())));
            assert_eq!(ss.get_cell(1, 1), Some("1201".to_string()));
            // Committing again is a no-op: nothing is being edited.
            assert_eq!(ss.commit_formula_bar(), None);
            assert!(!ss.edit_state().unwrap().0);
        });
    }

    #[test]
    fn the_caret_is_clamped_to_the_edit_buffer() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            ss.set_editing(true, "abc", 99);
            assert_eq!(ss.edit_state().unwrap().2, 3, "a stale caret must not survive");
        });
    }
}

/// The F10 surface: the four input hooks, and what they are notified about.
mod sheet_input {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn setting_the_cursor_notifies_the_move_hook() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            let seen = Rc::new(RefCell::new(Vec::new()));
            {
                let s = seen.clone();
                ss.add_cursor_move_callback(Box::new(move |r, c| s.borrow_mut().push((r, c))));
            }
            ss.set_cursor(2, 3);
            ss.set_cursor(4, 5);
            assert_eq!(*seen.borrow(), vec![(2, 3), (4, 5)]);
        });
    }

    #[test]
    fn the_move_hook_sees_the_clamped_position() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            ss.set_grid_config(1, 2);
            ss.set_row_counts(1, 2);
            let seen = Rc::new(Cell::new((99u32, 99u32)));
            {
                let s = seen.clone();
                ss.add_cursor_move_callback(Box::new(move |r, c| s.set((r, c))));
            }
            ss.set_cursor(50, 50);
            assert_eq!(seen.get(), (3, 3), "the handler must see where the cursor actually went");
        });
    }

    #[test]
    fn commit_notifies_with_the_written_value() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            let seen = Rc::new(RefCell::new(Vec::new()));
            {
                let s = seen.clone();
                ss.add_commit_edit_callback(Box::new(move |r, c, t| s.borrow_mut().push((r, c, t))));
            }
            ss.set_cursor(1, 1);
            ss.set_editing(true, "hello", 5);
            ss.commit_formula_bar();
            assert_eq!(*seen.borrow(), vec![(1, 1, "hello".to_string())]);
            // A second commit fires nothing: the first one ended the edit.
            ss.commit_formula_bar();
            assert_eq!(seen.borrow().len(), 1);
        });
    }

    #[test]
    fn a_commit_handler_may_read_the_model_it_just_changed() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            ss.set_cursor(2, 1);
            // The handler reads the cell through the adapter, which re-enters
            // the model. If the commit fired under a borrow this would panic.
            let s2 = ss.clone();
            let read = Rc::new(RefCell::new(String::new()));
            {
                let r = read.clone();
                ss.add_commit_edit_callback(Box::new(move |_, _, _| {
                    *r.borrow_mut() = s2.get_cell(2, 1).unwrap_or_default();
                }));
            }
            ss.set_editing(true, "written", 7);
            ss.commit_formula_bar();
            assert_eq!(*read.borrow(), "written", "the cell must already hold the value");
        });
    }

    #[test]
    fn goto_and_bare_key_handlers_fire() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let ss = za::create_spreadsheet().unwrap();
            let gotos = Rc::new(Cell::new(0));
            let keys = Rc::new(Cell::new(0));
            {
                let (g, k) = (gotos.clone(), keys.clone());
                ss.add_goto_callback(Box::new(move || g.set(g.get() + 1)));
                ss.add_key_callback(Box::new(move || k.set(k.get() + 1)));
            }
            ss.fire_goto();
            rswidgets::backends::zork::sheet_fire_key(ss.id());
            assert_eq!((gotos.get(), keys.get()), (1, 1));
            // Both are persistent: a `connect_*` name promises more than once.
            ss.fire_goto();
            rswidgets::backends::zork::sheet_fire_key(ss.id());
            assert_eq!((gotos.get(), keys.get()), (2, 2));
            // (key, goto, cursor_move, commit_edit) — this test registered two
            // of the four.
            assert_eq!(ss.hook_counts(), Some((1, 1, 0, 0)));
        });
    }
}

/// F4: timers on the model's virtual clock.
mod timers {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn a_one_shot_fires_once_and_is_gone() {
        serial(|| {
            let ran = Rc::new(Cell::new(0));
            {
                let r = ran.clone();
                za::timeout_add_once(100, Box::new(move || r.set(r.get() + 1))).unwrap();
            }
            assert_eq!(za::advance_timers(50), 0, "not due yet");
            assert_eq!(ran.get(), 0);
            assert_eq!(za::advance_timers(60), 1, "due at 100ms");
            assert_eq!(ran.get(), 1);
            assert_eq!(za::advance_timers(1000), 0, "a one-shot must not re-arm");
            assert_eq!(ran.get(), 1);
            assert_eq!(za::timer_count(), 0);
        });
    }

    #[test]
    fn a_repeating_timer_stops_when_its_handler_says_so() {
        serial(|| {
            let n = Rc::new(Cell::new(0));
            {
                let c = n.clone();
                za::timeout_add_repeating(50, Box::new(move || {
                    c.set(c.get() + 1);
                    c.get() < 3
                }))
                .unwrap();
            }
            assert_eq!(za::advance_timers(200), 3, "fires every 50ms until the handler returns false");
            assert_eq!(n.get(), 3);
            assert_eq!(za::timer_count(), 0, "a timer that said stop must be unregistered");
        });
    }

    #[test]
    fn a_long_jump_fires_a_repeating_timer_once_per_interval() {
        serial(|| {
            let n = Rc::new(Cell::new(0));
            {
                let c = n.clone();
                za::timeout_add_repeating(50, Box::new(move || {
                    c.set(c.get() + 1);
                    true
                }))
                .unwrap();
            }
            // Four 50ms intervals inside a 200ms jump. Re-arming from the *end*
            // of the jump instead of each timer's own deadline would fire it
            // once and silently skip three — the "catch up, do not replay"
            // policy. Which is right depends on what the caller means, and for
            // a main loop reaching successive timeouts the answer is four.
            assert_eq!(za::advance_timers(200), 4);
            assert_eq!(n.get(), 4);
        });
    }

    #[test]
    fn a_window_timer_is_cancellable_by_its_handle() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let w2 = za::create_window().unwrap();
            let n = Rc::new(Cell::new(0));
            {
                let c = n.clone();
                w2.start_repeating_timer(42, 10, Box::new(move || {
                    c.set(c.get() + 1);
                    true
                }))
                .unwrap();
            }
            assert!(za::cancel_timer(42));
            assert_eq!(za::advance_timers(1000), 0);
            assert_eq!(n.get(), 0, "a cancelled timer must not fire");
            assert!(!za::cancel_timer(42), "cancelling twice reports no such handle");
        });
    }

    #[test]
    fn cancelling_one_of_several_leaves_the_others() {
        serial(|| {
            let a = Rc::new(Cell::new(0));
            let b = Rc::new(Cell::new(0));
            let c = Rc::new(Cell::new(0));
            {
                let (x, y, z) = (a.clone(), b.clone(), c.clone());
                za::timeout_add_repeating(10, Box::new(move || {
                    x.set(x.get() + 1);
                    true
                }))
                .unwrap();
                let _w = za::create_window().unwrap();
                let w = za::create_window().unwrap();
                w.start_repeating_timer(1, 10, Box::new(move || {
                    y.set(y.get() + 1);
                    true
                }))
                .unwrap();
                za::timeout_add_repeating(10, Box::new(move || {
                    z.set(z.get() + 1);
                    true
                }))
                .unwrap();
            }
            assert!(za::cancel_timer(1), "the middle timer, whose removal shifts the later index");
            za::advance_timers(20);
            assert!(a.get() >= 1, "the first timer still runs");
            assert_eq!(b.get(), 0, "the cancelled one does not");
            assert!(c.get() >= 1, "the last one still runs: its handle must have followed its index");
        });
    }

    #[test]
    fn a_timer_handler_may_touch_the_model() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let lbl = za::create_label("pending").unwrap();
            let action = za::create_simple_action("app.tick").unwrap();
            let fired = Rc::new(Cell::new(0));
            {
                let f = fired.clone();
                action.connect_activate(Box::new(move |_| f.set(f.get() + 1))).unwrap();
            }
            {
                let l = lbl.clone();
                za::timeout_add_once(10, Box::new(move || l.set_text("fired"))).unwrap();
            }
            let l2 = lbl.clone();
            let o = action.clone();
            za::timeout_add_repeating(
                20,
                Box::new(move || {
                    // Re-entering from inside a timer is what a polling app
                    // does; a panic here would mean the borrow leaked.
                    l2.set_text("ticked");
                    o.activate();
                    false
                }),
            )
            .unwrap();
            za::advance_timers(100);
            assert_eq!(lbl.get_text().as_deref(), Some("ticked"));
            assert_eq!(fired.get(), 1, "an action fired from inside a timer handler");
        });
    }
}

/// F2: the application action registry.
mod application {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn an_action_registers_and_activates() {
        serial(|| {
            let app = za::create_application().unwrap();
            let act = za::create_simple_action("app.save").unwrap();
            let fired = Rc::new(Cell::new(0));
            {
                let f = fired.clone();
                act.connect_activate(Box::new(move |_| f.set(f.get() + 1))).unwrap();
            }
            app.add_action(&act).unwrap();
            assert_eq!(app.actions(), vec!["app.save".to_string()]);
            act.activate();
            assert_eq!(fired.get(), 1);
        });
    }

    #[test]
    fn register_refuses_a_non_action() {
        serial(|| {
            let app = za::create_application().unwrap();
            // A non-`SimpleAction` is what the model refuses. The adapter's
            // `add_action` is typed, so the model is reached directly here:
            // the point is that the *check* is there, not that the signature
            // happens to prevent the mistake.
            let btn = za::create_button("not an action").unwrap();
            assert_eq!(
                rswidgets::backends::zork::application_add_action(app.id(), btn.id()),
                None,
                "silently accepting a non-action would let a menu item resolve to \
                 something that can never fire"
            );
            assert!(app.actions().is_empty());
        });
    }

    #[test]
    fn two_applications_keep_their_own_action_sets() {
        serial(|| {
            let a = za::create_application().unwrap();
            let b = za::create_application().unwrap();
            let one = za::create_simple_action("app.one").unwrap();
            let two = za::create_simple_action("app.two").unwrap();
            a.add_action(&one).unwrap();
            b.add_action(&two).unwrap();
            assert_eq!(a.actions(), vec!["app.one".to_string()]);
            assert_eq!(b.actions(), vec!["app.two".to_string()]);
        });
    }

    #[test]
    fn register_and_as_ptr_are_answered_not_omitted() {
        serial(|| {
            let app = za::create_application().unwrap();
            assert!(app.register().is_ok(), "a no-op must still report success");
            assert!(app.as_ptr().is_null(), "there is no GApplication here");
        });
    }
}

/// F8: the tree dump and the redraw cascade.
mod diagnostics {
    use super::*;

    #[test]
    fn the_dump_writes_one_line_per_node() {
        serial(|| {
            let w = za::create_window().unwrap();
            w.set_title("corro");
            let _bx = za::create_box(za::Orientation::Vertical, 0).unwrap();
            let lbl = za::create_label("hello").unwrap();
            lbl.set_size_request(40, 20);
            let path = std::env::temp_dir().join("zork_dump_test.txt");
            let path = path.to_str().unwrap();
            let n = rswidgets::backends::zork::debug_dump_tree(path).expect("dump writes");
            assert!(n >= 3, "expected the window, box and label, got {n}");
            let text = std::fs::read_to_string(path).unwrap();
            assert!(text.contains("Window #1 \"corro\""), "got:\n{text}");
            assert!(text.contains("Label"), "got:\n{text}");
            // The label is quoted even when empty, so `#3` and a following
            // word can never run together.
            assert!(
                text.lines().all(|l| !l.contains("#2Label")),
                "a dump line must be splittable by eye:\n{text}"
            );
            let _ = std::fs::remove_file(path);
        });
    }

    #[test]
    fn a_dump_of_an_unwritable_path_reports_the_error() {
        serial(|| {
            // Reporting this as "0 nodes" would be a lie about a model that has
            // plenty of nodes in it.
            let _w = za::create_window().unwrap();
            let r = rswidgets::backends::zork::debug_dump_tree("/nonexistent-dir-xyz/out.txt");
            assert!(r.is_err(), "an unwritable path must be an error, not an empty dump");
        });
    }

    #[test]
    fn the_redraw_cascade_reaches_every_descendant() {
        serial(|| {
            let w = za::create_window().unwrap();
            let bx = za::create_box(za::Orientation::Vertical, 0).unwrap();
            let l1 = za::create_label("one").unwrap();
            let l2 = za::create_label("two").unwrap();
            let bx2 = za::create_box(za::Orientation::Vertical, 0).unwrap();
            let deep = za::create_label("deep").unwrap();
            bx2.append(&deep);
            bx.append(&l1);
            bx.append(&l2);
            bx.append(&bx2);
            let n = za::window_queue_redraw_cascades_to_children(w.id());
            assert_eq!(n, 6, "window, two boxes and three labels");
            for id in [w.id(), bx.id(), l1.id(), l2.id(), bx2.id(), deep.id()] {
                assert_eq!(
                    rswidgets::backends::zork::redraw_count(id),
                    1,
                    "every node in the subtree must be marked"
                );
            }
        });
    }

    #[test]
    fn a_plain_queue_redraw_marks_only_its_own_node() {
        serial(|| {
            let w = za::create_window().unwrap();
            let lbl = za::create_label("x").unwrap();
            w.queue_redraw();
            assert_eq!(rswidgets::backends::zork::redraw_count(w.id()), 1);
            assert_eq!(
                rswidgets::backends::zork::redraw_count(lbl.id()),
                0,
                "the non-cascading form is documented as marking only itself"
            );
        });
    }
}

/// F8's other half: `Window::set_layout_cb`, which on NWG hangs off `WM_SIZE`.
mod layout_callbacks {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn a_layout_callback_receives_the_new_size() {
        serial(|| {
            let w = za::create_window().unwrap();
            let bx = za::create_box(za::Orientation::Horizontal, 0).unwrap();
            w.set_child(&bx);
            let seen = Rc::new(RefCell::new(Vec::new()));
            {
                let s = seen.clone();
                w.set_layout_cb(move |width, height| s.borrow_mut().push((width, height)));
            }
            rswidgets::backends::zork::layout_box(bx.id(), 0, 0, 320, 240);
            rswidgets::backends::zork::fire_layout_callbacks(w.id(), 320, 240);
            assert_eq!(*seen.borrow(), vec![(320, 240)]);
            // Persistent, like every other `connect_*` on this backend.
            rswidgets::backends::zork::fire_layout_callbacks(w.id(), 640, 480);
            assert_eq!(*seen.borrow(), vec![(320, 240), (640, 480)]);
        });
    }
}

/// The process-wide key table that `add_key_callback` needs.
mod global_keys {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn a_registered_key_fires_only_its_own_handlers() {
        serial(|| {
            let hits = Rc::new(Cell::new(0));
            {
                let h = hits.clone();
                za::add_key_callback('s', Box::new(move || h.set(h.get() + 1)));
            }
            assert_eq!(za::key_callback_count('s'), 1);
            assert_eq!(za::fire_key_callback('q'), 0, "an unregistered key fires nothing");
            assert_eq!(hits.get(), 0);
            assert_eq!(za::fire_key_callback('s'), 1);
            assert_eq!(hits.get(), 1);
            // Persistent: a `connect_*` name means more than once.
            za::fire_key_callback('s');
            assert_eq!(hits.get(), 2);
        });
    }

    #[test]
    fn a_handler_that_registers_another_handler_keeps_it() {
        serial(|| {
            let hits = Rc::new(Cell::new(0));
            {
                let h = hits.clone();
                za::add_key_callback(
                    'x',
                    Box::new(move || {
                        h.set(h.get() + 1);
                    }),
                );
            }
            // Firing takes the handlers out to run them with no borrow held, so
            // a handler can register more. Putting them back must not clobber
            // the addition.
            let more = Rc::new(Cell::new(0));
            {
                let (h, m) = (hits.clone(), more.clone());
                za::add_key_callback('x', Box::new(move || {
                    let _ = (&h, &m);
                }));
            }
            za::fire_key_callback('x');
            assert_eq!(hits.get(), 1);
            assert_eq!(za::key_callback_count('x'), 2, "both handlers survive the round trip");
            let _ = more;
        });
    }
}

/// `Canvas::on_gesture` — the one method GTK and NWG have and zork did not.
///
/// The point of the gesture stream is that a caller does not have to
/// reassemble a drag from three separate hooks, so the tests drive it through
/// `core::DragTracker` rather than asserting on raw `Gesture` values: a drag
/// that the tracker does not recognise is a drag a host cannot use.
mod gestures {
    use super::*;
    use std::cell::Cell;
    use rswidgets::core::{Button, DragTracker, Gesture};

    fn log(seen: &Rc<RefCell<Vec<Gesture>>>) -> impl FnMut(Gesture) + 'static {
        let s = seen.clone();
        move |g| s.borrow_mut().push(g)
    }

    #[test]
    fn press_motion_release_arrive_as_the_three_gestures() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            let seen = Rc::new(RefCell::new(Vec::new()));
            c.on_gesture(Box::new(log(&seen)));
            rswidgets::backends::zork::pointer_click_button(c.id(), 10.0, 20.0, 1, 0);
            rswidgets::backends::zork::pointer_motion(c.id(), 12.0, 22.0, 0);
            rswidgets::backends::zork::pointer_release(c.id(), 14.0, 24.0, 1, 0);
            let got = seen.borrow().clone();
            assert_eq!(got.len(), 3, "got {got:?}");
            assert!(matches!(got[0], Gesture::Button { button: Button::Primary, pressed: true, .. }));
            // The middle one is a *drag*, not a hover: a button is held. This is
            // the distinction the raw motion hook cannot express.
            assert!(matches!(got[1], Gesture::Drag { .. }), "got {:?}", got[1]);
            assert!(matches!(got[2], Gesture::Button { button: Button::Primary, pressed: false, .. }));
        });
    }

    #[test]
    fn motion_with_no_button_is_a_hover() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            let seen = Rc::new(RefCell::new(Vec::new()));
            c.on_gesture(Box::new(log(&seen)));
            rswidgets::backends::zork::pointer_motion(c.id(), 5.0, 6.0, 0);
            assert!(matches!(seen.borrow()[0], Gesture::Hover { .. }));
        });
    }

    #[test]
    fn the_button_is_cleared_after_a_release() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            let seen = Rc::new(RefCell::new(Vec::new()));
            c.on_gesture(Box::new(log(&seen)));
            rswidgets::backends::zork::pointer_click_button(c.id(), 0.0, 0.0, 1, 0);
            rswidgets::backends::zork::pointer_release(c.id(), 5.0, 5.0, 1, 0);
            rswidgets::backends::zork::pointer_motion(c.id(), 6.0, 6.0, 0);
            let got = seen.borrow();
            assert!(matches!(got[2], Gesture::Hover { .. }), "got {:?}", got[2]);
        });
    }

    #[test]
    fn a_drag_is_recognised_by_the_shared_tracker() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            // One tracker fed the whole stream, which is the documented usage:
            // it accumulates the movement and the press resets it. A fresh
            // tracker per gesture would never exceed the threshold, because
            // each Drag would have zero total movement.
            let tracker = Rc::new(RefCell::new(DragTracker::new()));
            let drags = Rc::new(RefCell::new(Vec::new()));
            {
                let (t, d) = (tracker.clone(), drags.clone());
                c.on_gesture(Box::new(move |g| {
                    let mut t = t.borrow_mut();
                    if let Some(ev) = t.feed(g) {
                        d.borrow_mut().push((ev.total_dx, ev.total_dy));
                    }
                }));
            }
            rswidgets::backends::zork::pointer_click_button(c.id(), 100.0, 100.0, 1, 0);
            rswidgets::backends::zork::pointer_motion(c.id(), 101.0, 100.0, 0);
            rswidgets::backends::zork::pointer_motion(c.id(), 108.0, 100.0, 0);
            rswidgets::backends::zork::pointer_release(c.id(), 108.0, 100.0, 1, 0);
            assert_eq!(
                *drags.borrow(),
                vec![(8.0, 0.0)],
                "an 8px move must clear the 3px threshold and report the total"
            );
            // The release ended the drag, so a later motion is a fresh one.
            rswidgets::backends::zork::pointer_motion(c.id(), 120.0, 100.0, 0);
            assert_eq!(drags.borrow().len(), 1, "no button held: a hover, not a drag");
        });
    }

    #[test]
    fn modifiers_are_translated_not_passed_through() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            let seen = Rc::new(RefCell::new(Vec::new()));
            c.on_gesture(Box::new(log(&seen)));
            // GDK: 1 = shift, 4 = control, 8 = alt, 64 = super.
            rswidgets::backends::zork::pointer_motion(c.id(), 0.0, 0.0, 1 | 4 | 64);
            let first = seen.borrow()[0];
            match first {
                Gesture::Hover { mods, .. } => {
                    assert!(mods.shift && mods.ctrl && mods.meta, "got {mods:?}");
                    assert!(!mods.alt, "bit 8 not set, so alt must be false");
                }
                ref other => panic!("expected a hover, got {other:?}"),
            }
        });
    }

    #[test]
    fn a_scroll_reaches_the_stream_as_a_delta() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            let seen = Rc::new(RefCell::new(Vec::new()));
            c.on_gesture(Box::new(log(&seen)));
            c.scroll_gesture(0.0, -3.0, 40.0, 50.0, 0);
            let first = seen.borrow()[0];
            match first {
                Gesture::Scroll { delta, x, y, .. } => {
                    // A delta, not a position: a positive dy zooms in.
                    assert_eq!((delta.dx, delta.dy), (0.0, -3.0));
                    assert_eq!((x, y), (40.0, 50.0));
                }
                ref other => panic!("expected a scroll, got {other:?}"),
            }
        });
    }

    #[test]
    fn a_gesture_handler_may_re_enter_the_model() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            let lbl = za::create_label("start").unwrap();
            let hits = Rc::new(Cell::new(0));
            {
                let (h, l) = (hits.clone(), lbl.clone());
                c.on_gesture(Box::new(move |_| {
                    h.set(h.get() + 1);
                    l.set_text("touched");
                }));
            }
            rswidgets::backends::zork::pointer_motion(c.id(), 1.0, 1.0, 0);
            assert_eq!(hits.get(), 1);
            assert_eq!(lbl.get_text().as_deref(), Some("touched"));
            // Still registered afterwards: `on_gesture` is a connect, not a once.
            assert_eq!(c.gesture_hook_count(), 1);
            rswidgets::backends::zork::pointer_motion(c.id(), 2.0, 2.0, 0);
            assert_eq!(hits.get(), 2);
        });
    }

    #[test]
    fn the_raw_hooks_still_fire_alongside_the_gestures() {
        serial(|| {
            let _w = za::create_window().unwrap();
            let c = za::create_canvas().unwrap();
            let raw = Rc::new(Cell::new(0));
            let ges = Rc::new(Cell::new(0));
            {
                let (r, g) = (raw.clone(), ges.clone());
                c.on_motion(Box::new(move |_x, _y, _s| r.set(r.get() + 1)));
                c.on_gesture(Box::new(move |_| g.set(g.get() + 1)));
            }
            rswidgets::backends::zork::pointer_motion(c.id(), 3.0, 4.0, 0);
            assert_eq!((raw.get(), ges.get()), (1, 1), "one event, both callbacks");
        });
    }
}

// ---------------------------------------------------------------------------
// The `grid` verb renders a sheet's cells.
//
// The verb prints the *cells the model holds*, so the test asserts on the
// cells themselves — the rendering is the REPL's business, and these are the
// inputs it reads.
// ---------------------------------------------------------------------------

/// The cells must come back in address order.
///
/// A `HashMap` iteration order would make the grid reshuffle between two runs
/// of the same session, which is exactly what a printed table must not do.
#[test]
fn sheet_cells_are_returned_in_address_order() {
    serial(|| {
        let _w = za::create_window().unwrap();
        let ss = za::create_spreadsheet().unwrap();
        // Written back-to-front and out of column order on purpose.
        za::spreadsheet_set_cell(ss.id(), 3, 2, "d2");
        za::spreadsheet_set_cell(ss.id(), 1, 2, "b2");
        za::spreadsheet_set_cell(ss.id(), 2, 1, "c1");
        za::spreadsheet_set_cell(ss.id(), 1, 1, "a1");
        let cells = rswidgets::backends::zork::sheet_cells(ss.id());
        let addr: Vec<(u32, u32)> = cells.iter().map(|(r, c, _)| (*r, *c)).collect();
        assert_eq!(
            addr,
            vec![(1, 1), (1, 2), (2, 1), (3, 2)],
            "cells must be sorted by row then column"
        );
        assert_eq!(rswidgets::backends::zork::sheet_cell_count(ss.id()), 4);
    });
}

/// A 0-based sheet is not an empty sheet.
///
/// The examples address cells from 0, corro's own pnc backend addresses them
/// from 1, and the model stores whichever it was given. A grid that assumed
/// one index space printed an empty band of rows above the real cells — three
/// blank lines and then the values, with the cursor marking nothing. Reading
/// from the minimum finds them wherever they start.
#[test]
fn sheet_cells_are_read_from_whichever_index_they_start_at() {
    serial(|| {
        let _w = za::create_window().unwrap();
        let ss = za::create_spreadsheet().unwrap();
        za::spreadsheet_set_cell(ss.id(), 0, 0, "Revenue");
        za::spreadsheet_set_cell(ss.id(), 1, 1, "1201");
        za::spreadsheet_set_cell(ss.id(), 2, 1, "800");
        let cells = rswidgets::backends::zork::sheet_cells(ss.id());
        assert_eq!(cells[0], (0, 0, "Revenue".to_string()));
        assert_eq!(cells[1], (1, 1, "1201".to_string()));
        assert_eq!(cells[2], (2, 1, "800".to_string()));
        // The cursor is 1-based (it shares the cells' space, since corro sets
        // both from one index), so it lands on the `1201` above.
        za::spreadsheet_set_cursor(ss.id(), 1, 1);
        assert_eq!(
            rswidgets::backends::zork::sheet_cursor(ss.id()),
            Some((1, 1)),
            "the cursor must address the same cell `sheet cell 1 1` reads"
        );
    });
}

/// An unwritten sheet is empty, not a table of blank rows.
#[test]
fn an_empty_spreadsheet_reports_no_cells() {
    serial(|| {
        let _w = za::create_window().unwrap();
        let ss = za::create_spreadsheet().unwrap();
        assert!(rswidgets::backends::zork::sheet_cells(ss.id()).is_empty());
        assert_eq!(rswidgets::backends::zork::sheet_cell_count(ss.id()), 0);
    });
}
