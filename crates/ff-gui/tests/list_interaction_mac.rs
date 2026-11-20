//! Row click, header click and double click on AppKit, through the canvas.
//!
//! The macOS sibling of `tests/interaction_gtk.rs`'s click block, and of
//! `tests/menubar_mac.rs` in shape: a `harness = false` binary so the test runs
//! on the process's main thread, where AppKit lets a window exist.
//!
//! Nothing here calls a Rust handler directly. Every gesture is synthesised as
//! the events the canvas layer itself receives — `dispatch_canvas_press` (the
//! click callback the sheet view fires from `mouseDown:`/`mouseUp:`) and
//! `dispatch_canvas_double_click` (the second press AppKit reports as
//! `clickCount` 2) — using the canvas id the list registered under, and
//! coordinates found by asking the list's own hit test where row 1 and the
//! header band are. compat's hit-testing and sorting then do the work, and the
//! assertions read back the observable state: the selected path, the sort state
//! and the row order it produces, and the Open the app's double-click handler
//! runs (the real `connect_open_on_double_click`, i.e. command, spawn and
//! status line).

#[cfg(all(target_os = "macos", feature = "macos", not(feature = "zork")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::rc::Rc;

    use rswidgets::compat::CellValue;
    use rswidgets::prelude::*;

    fn send0v(obj: *mut c_void, sel: &str) {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void) =
            unsafe { std::mem::transmute(rswidgets::backends::apple::msg_shim()) };
        unsafe { f(obj, rswidgets::backends::apple::selector(sel)) }
    }

    let app = App::init()?;
    let win = app.create_window()?;
    win.set_title("ff-gui list interaction test");

    // The app's results list: three columns, the icon column sortable too.
    let list = app.create_treeview(&[("", true, true), ("Name", false, true), ("Path", false, true)])?;
    // Real files, inserted out of name order so a Name sort has something to
    // change, and so "opened" names a file that really exists.
    let dir = tempfile::tempdir()?;
    let names = ["c.rs", "a.md", "b.txt", "d.png"];
    let mut rows = Vec::new();
    for name in names {
        let path = dir.path().join(name);
        std::fs::write(&path, "list interaction fixture\n")?;
        rows.push(vec![
            CellValue::Icon {
                name: "text-x-generic".to_string(),
                fallback: "-".to_string(),
            },
            CellValue::Text(name.to_string()),
            CellValue::Text(path.to_string_lossy().into_owned()),
        ]);
    }
    list.set_rows(&rows);

    let painted = list.clone().into_scrolled()?;
    let column = app.create_box(Orientation::Vertical, 2)?;
    column.append(&painted);
    win.set_child(&column);
    win.present();
    // Lay the list out at a real width: a header click is hit-tested against the
    // width the header was last drawn at, and a list that has never been drawn
    // uses the default, which would sort the wrong column.
    let view = win.raw_handle();
    let _ = rswidgets::backends_macos_adapter::capture(view, Some((900.0, 560.0)));
    send0v(view, "layoutSubtreeIfNeeded");

    // The id the sheet view sends with every click: a mismatch here is a click
    // that goes nowhere.
    let canvas_id = rswidgets::backends_macos_adapter::Canvas(list.raw_handle()).canvas_id();
    assert_ne!(
        canvas_id, 0,
        "the list's canvas has no id, so no click can reach it"
    );

    // Where row 1 is: ask the list's own hit test rather than assuming a row
    // height, and prove the header band is not a row while we are there.
    assert_eq!(
        list.row_at(60.0, 2.0),
        None,
        "a point in the header band hit-tests as a row"
    );
    let mut header = 1.0;
    while header < 400.0 && list.row_at(60.0, header) != Some(0) {
        header += 1.0;
    }
    assert_eq!(
        list.row_at(60.0, header),
        Some(0),
        "no point in the first row's band hit-tests as row 0"
    );
    assert!(
        header > 3.0,
        "the first row starts at y={header}, so the list drew no header band"
    );
    let mut row_y = header + 1.0;
    while row_y < header + 200.0 && list.row_at(60.0, row_y) != Some(1) {
        row_y += 1.0;
    }
    assert_eq!(
        list.row_at(60.0, row_y),
        Some(1),
        "no point in the second row's band hit-tests as row 1"
    );
    let row1_name = list.cell_text(1, 1).expect("row 1 has no name");
    let row1_path = list.cell_text(1, 2).expect("row 1 has no path");

    // --- A click selects the row under it ---
    list.select_row(0);
    rswidgets::backends_macos_adapter::dispatch_canvas_press(canvas_id, 60.0, row_y, 1);
    let selected = list.selected_path(2).unwrap_or_default();
    println!(
        "list: clicked row 1 ({row1_name:?}) at (60.0, {row_y}) -> selection {selected:?}"
    );
    assert_eq!(
        selected, row1_path,
        "a click at (60.0, {row_y}) selected {selected:?}, not row 1 ({row1_path:?})"
    );

    // --- A header click sorts the rows it draws ---
    //
    // The header band is the top of the canvas, so this lands on a header cell
    // and never on a row. The columns are drawn in 32% steps of the canvas
    // width, which makes x=400 the Name column at the 900pt width above. Two
    // clicks must flip the direction, and the *order of the rows* has to
    // follow, which is what "sorted" means to a reader of the list.
    const HEADER_X: f64 = 400.0;
    let first_row = |list: &rswidgets::compat::TreeView| -> String {
        list.cell_text(1, 0).unwrap_or_default()
    };
    let before_state = list.sort_state();
    let before_first = first_row(&list);
    rswidgets::backends_macos_adapter::dispatch_canvas_press(canvas_id, HEADER_X, 2.0, 1);
    let first_state = list.sort_state();
    let first_first = first_row(&list);
    rswidgets::backends_macos_adapter::dispatch_canvas_press(canvas_id, HEADER_X, 2.0, 1);
    let second_state = list.sort_state();
    let second_first = first_row(&list);
    println!(
        "list: header click on Name at ({HEADER_X}, 2.0) -> sort {before_state:?} -> {first_state:?} -> {second_state:?}, first row {before_first:?} -> {first_first:?} -> {second_first:?}"
    );
    let (Some(first), Some(second)) = (first_state, second_state) else {
        panic!(
            "a header click did not sort the list: {before_state:?} -> {first_state:?} -> {second_state:?}"
        );
    };
    assert_eq!(
        first.0, 1,
        "a click at x={HEADER_X} sorted by column {}, not Name",
        first.0
    );
    assert_eq!(
        first.0, second.0,
        "the two header clicks sorted by different columns: {first:?} then {second:?}"
    );
    assert_ne!(
        first.1, second.1,
        "the second header click did not reverse the direction: {first:?} then {second:?}"
    );
    assert_ne!(
        first_first, second_first,
        "the sort flag changed but the rows are in the same order"
    );
    // Each direction has to put the row it means first: the file names are
    // a.md < b.txt < c.rs < d.png, and the rows were inserted as c.rs, a.md,
    // b.txt, d.png.
    let (ascending_first, descending_first) = if first.1 {
        (&first_first, &second_first)
    } else {
        (&second_first, &first_first)
    };
    assert_eq!(
        ascending_first.as_str(),
        "a.md",
        "sorting by Name ascending put {ascending_first:?} first"
    );
    assert_eq!(
        descending_first.as_str(),
        "d.png",
        "sorting by Name descending put {descending_first:?} first"
    );
    assert_eq!(
        list.sort_state(),
        Some(second),
        "the list's sort state changed after the clicks were read"
    );

    // --- A double click opens the row under it ---
    //
    // The handler is `ff_gui::connect_open_on_double_click`, the function the
    // app installs, so this runs the real Open action rather than a stand-in.
    {
        let opened: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let seen = opened.clone();
        ff_gui::connect_open_on_double_click(&list, 2, move |line| {
            seen.borrow_mut().push(line);
        });
        // Row 1 again: the sort may have moved which file is there, so re-read
        // both the point and the row rather than naming one.
        let mut y = header + 1.0;
        while y < header + 200.0 && list.row_at(60.0, y) != Some(1) {
            y += 1.0;
        }
        let name = list.cell_text(1, 1).expect("row 1 has no name");
        // AppKit sends the first press, then a second press with `clickCount`
        // 2; the adapter forwards the count, so a single press must not open.
        rswidgets::backends_macos_adapter::dispatch_canvas_press(canvas_id, 60.0, y, 1);
        let selected_once = list.selected_path(2).unwrap_or_default();
        let opened_once = opened.borrow().clone();
        assert!(
            opened_once.is_empty(),
            "one press opened the row: {opened_once:?}"
        );
        rswidgets::backends_macos_adapter::dispatch_canvas_press(canvas_id, 60.0, y, 2);
        let lines = opened.borrow().clone();
        println!(
            "list: first press selected {selected_once:?}; double click on row 1 ({name:?}) -> {lines:?}"
        );
        assert_eq!(
            lines.len(),
            1,
            "the double click activated {} handlers: {lines:?}",
            lines.len()
        );
        assert!(
            lines[0].starts_with("Open"),
            "the double click ran {:?}, not the Open action",
            lines[0]
        );
        assert!(
            lines[0].contains(&name),
            "the open handler ran for {:?}, not the double-clicked row {name:?}",
            lines[0]
        );
        assert_eq!(
            list.selected_path(2).unwrap_or_default(),
            list.cell_text(1, 2).unwrap_or_default(),
            "the double click left a different row selected"
        );
    }

    println!("list: ok — click selected, header sorted, double click opened");
    Ok(())
}

#[cfg(not(all(target_os = "macos", feature = "macos", not(feature = "zork"))))]
fn main() {
    // AppKit is the only backend whose canvas dispatch is exercised here; the
    // GTK counterpart is the click block in `tests/interaction_gtk.rs`.
    println!("list_interaction_mac: needs the AppKit backend (macOS, `macos` on, `zork` off)");
}
