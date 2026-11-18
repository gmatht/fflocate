//! Drive the zork spreadsheet and timer surface from a real program.
//!
//! Everything here goes through the same `rswidgets::backends_zork_adapter`
//! a host would use, then prints what the model recorded. It exists so the
//! F9/F10/F4 surface can be exercised from a run rather than only from a
//! test, and so the REPL handed off at the end has something to describe.
//!
//! Run with:
//!     cargo run -p rswidgets --no-default-features --features zork \
//!         --example zork-sheet

use rswidgets::backends_zork_adapter as za;
use std::rc::Rc;
use std::cell::{Cell, RefCell};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _win = za::create_window()?;
    let ss = za::create_spreadsheet()?;

    // -- F9: the chrome every pancurses render frame sets ------------------
    let (r, c0) = ss.cursor_position().unwrap_or((0, 0));
    ss.set_cursor(r, c0);
    ss.set_anchor(Some((0, 0)));
    ss.set_grid_config(1, 4);
    ss.set_row_counts(1, 3);
    ss.set_column_layout(vec![
        (0, 12, "A".into()),
        (1, 8, "B".into()),
        (2, 8, "C".into()),
        (3, 8, "D".into()),
    ]);
    ss.set_row_labels(vec![(0, "1".into()), (1, "2".into()), (2, "3".into())]);
    ss.set_border_title("Book1");
    ss.set_menu_text(" [File]   Edit    Insert    Format    Sheet    Help");
    ss.set_status_text("ready");
    ss.set_formula_bar_trailing("   ·  saved");
    ss.set_tab_data(&["Sheet1".to_string(), "Notes".to_string()], 0);
    // A second set_tab_data with an out-of-range active index must clamp.
    ss.set_tab_data(&["Sheet1".to_string(), "Notes".to_string()], 7);

    let addr = za::create_label("A1")?;
    let entry = za::create_entry()?;
    entry.set_text("=SUM(B1:D1)");
    ss.set_formula_bar(&addr, &entry);

    za::spreadsheet_set_cell(ss.id(), 0, 0, "Revenue");
    za::spreadsheet_set_cell(ss.id(), 1, 1, "1200");
    za::spreadsheet_set_cell(ss.id(), 2, 1, "800");

    // -- F10: the four input hooks ------------------------------------------
    let moves = Rc::new(RefCell::new(Vec::new()));
    let commits = Rc::new(RefCell::new(Vec::new()));
    let (m, k) = (moves.clone(), commits.clone());
    ss.add_cursor_move_callback(Box::new(move |row, col| {
        m.borrow_mut().push(format!("R{}C{}", row, col));
    }));
    let kc = k.clone();
    ss.add_commit_edit_callback(Box::new(move |row, col, text| {
        kc.borrow_mut().push(format!("R{}C{}={}", row, col, text));
    }));
    let gseen = Rc::new(Cell::new(0));
    let g2 = gseen.clone();
    ss.add_goto_callback(Box::new(move || g2.set(g2.get() + 1)));

    // Setting the cursor is a navigation, so it must notify.
    ss.set_cursor(1, 1);
    ss.fire_goto();

    // -- editing and committing ---------------------------------------------
    ss.set_cursor(1, 1);
    ss.set_editing(true, "1201", 4);
    let (editing, buf, pos) = ss.edit_state().unwrap_or((false, String::new(), 0));
    let committed = ss.commit_formula_bar();

    // -- F4: timers on a virtual clock ---------------------------------------
    let once = Rc::new(Cell::new(0));
    let o = once.clone();
    za::timeout_add_once(100, Box::new(move || o.set(o.get() + 1)))?;
    let ticks = Rc::new(Cell::new(0));
    let t = ticks.clone();
    za::timeout_add_repeating(50, Box::new(move || {
        t.set(t.get() + 1);
        t.get() < 3 // stop after three
    }))?;
    // A window-owned timer, cancelled by its handle.
    let w = za::create_window()?;
    w.start_repeating_timer(7, 10_000, Box::new(|| true))?;
    let cancelled = za::cancel_timer(7);

    println!("== F9 chrome ==");
    let c = ss.chrome().unwrap_or_default();
    println!("  cursor          R{}C{}", c.cursor.0, c.cursor.1);
    println!("  anchor          {:?}", c.anchor);
    println!("  grid            {} gutter col, {} data cols", c.margin_cols, c.main_cols);
    println!("  rows            {} header, {} data", c.header_rows, c.main_rows);
    println!("  columns         {:?}", c.column_layout);
    println!("  row labels      {:?}", c.row_labels);
    println!("  tabs            {:?} active={}", c.tabs, c.active_tab);
    println!("  status          {:?}", ss.status_text());
    println!("  formula bar     {:?} trailing {:?}", ss.formula_bar(), ss.formula_bar_trailing());
    println!("  A1              {:?}", ss.get_cell(1, 1));

    println!("== F10 hooks ==");
    println!("  hook counts     {:?}", ss.hook_counts());
    println!("  cursor moves    {:?}", moves.borrow());
    println!("  goto fired      {}", gseen.get());
    println!("  edit state      editing={} buf={:?} pos={}", editing, buf, pos);
    println!("  commit          {:?}", committed);
    println!("  commit hooks    {:?}", commits.borrow());

    println!("== F4 timers ==");
    println!("  pending         {}", za::timer_count());
    let f1 = za::advance_timers(60);
    println!("  after 60ms      fired={f1} once={} repeats={}", once.get(), ticks.get());
    let f2 = za::advance_timers(60);
    println!("  after 120ms     fired={f2} once={} repeats={}", once.get(), ticks.get());
    let f3 = za::advance_timers(200);
    println!("  after 320ms     fired={f3} once={} repeats={} pending={}", once.get(), ticks.get(), za::timer_count());
    println!("  cancel_timer(7) -> {cancelled}");

    println!("== F2 application ==");
    let app = za::create_application()?;
    let act = za::create_simple_action("app.save")?;
    let hit = Rc::new(Cell::new(0));
    let h = hit.clone();
    act.connect_activate(Box::new(move |_| h.set(h.get() + 1)))?;
    app.add_action(&act)?;
    println!("  registered      {:?}", app.actions());
    act.activate();
    println!("  activation fired {} time(s)", hit.get());

    println!("== F8 dump ==");
    let path = std::env::var("ZORK_DUMP").unwrap_or_else(|_| "/tmp/zork_tree.txt".to_string());
    za::debug_dump_native_tree(std::ptr::null_mut(), &path);
    println!("  wrote the widget tree to {path}");
    if let Ok(text) = std::fs::read_to_string(&path) {
        for line in text.lines().take(14) {
            println!("  {line}");
        }
    }

    // Hand off to the REPL over the *same* model, so the grid, the cursor and
    // the tabs the calls above recorded are what `sheet` prints. This is the
    // same adoption `backends::zork::init()` does, reached from an example.
    println!("\nHanding off to the REPL. 'help' lists the verbs; 'sheet' shows the grid.");
    use rswidgets::backends::BackendApp as _;
    Box::new(rswidgets::backends::zork::repl::ZorkApp::adopt_singleton())
        .run()
        .map_err(|e| e)
}
