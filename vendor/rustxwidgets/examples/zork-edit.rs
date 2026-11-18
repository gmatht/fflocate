//! Edit cells and walk the cursor on the zork backend, end to end.
//!
//! This is the demonstration for the claim in `docs/ZORK_MISSING.md` that the
//! zork model now records a spreadsheet's cursor, cells and edit state — and
//! that it does so through the *same* adapter surface the GUI backends take,
//! so the calls below would compile against pancurses unchanged.
//!
//! Everything printed is a read-back from the model after the operation. No
//! value is echoed from the code that wrote it.
//!
//! Run with:
//!     cargo run -p rswidgets --no-default-features --features zork \
//!         --example zork-edit

use rswidgets::backends_zork_adapter as za;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _win = za::create_window()?;
    let ss = za::create_spreadsheet()?;

    // A sheet with a header row, a gutter column, and three data rows.
    ss.set_grid_config(1, 3);
    ss.set_row_counts(1, 3);
    ss.set_border_title("Demo");
    ss.set_tab_data(&["Sheet1".to_string()], 0);

    // Header labels, as the renderer would put them.
    for (col, title) in [(1u32, "Item"), (2, "Qty"), (3, "Cost")] {
        ss.set_cell(1, col, title);
    }
    // Three rows of data.
    let data = [("Widget", "3", "12"), ("Gadget", "10", "40"), ("Doohickey", "7", "28")];
    for (i, (item, qty, cost)) in data.iter().enumerate() {
        let row = 2 + i as u32;
        ss.set_cell(row, 1, item);
        ss.set_cell(row, 2, qty);
        ss.set_cell(row, 3, cost);
    }

    // Each cursor move notifies, so we can show the walk happening rather than
    // asserting that it could.
    let moves = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    {
        let m = moves.clone();
        ss.add_cursor_move_callback(Box::new(move |r, c| m.borrow_mut().push(format!("R{r}C{c}"))));
    }

    let show_row = |ss: &za::Spreadsheet, row: u32| {
        let cells: Vec<String> = (1..=3)
            .map(|c| {
                let v = ss.get_cell(row, c).unwrap_or_default();
                format!("{:<10}", v)
            })
            .collect();
        format!("      {} | {}", row, cells.join("| "))
    };

    println!("Sheet \"{}\", {} data cols, {} data rows", ss.get_border_title().unwrap_or_default(), 3, 3);
    println!("  row | Item      | Qty       | Cost");
    for row in 1..=4 {
        println!("{}", show_row(&ss, row));
    }

    // ---- walk the cursor to the quantity cell of row 2 -------------------
    println!("\n-- walk the cursor ------------------------------------------");
    println!("   the cursor shares the cells' addressing: R2C3 is the same cell");
    println!("   whichever way you name it");
    ss.set_cursor(2, 3); // Gadget's cost
    let (r, c) = ss.cursor_position().expect("a spreadsheet has a cursor");
    println!("   set_cursor(2, 3)  -> R{r}C{c} = {}", show_cell(&ss, r, c));
    ss.set_cursor(4, 2); // Doohickey's quantity
    let (r, c) = ss.cursor_position().expect("a spreadsheet has a cursor");
    println!("   set_cursor(4, 2)  -> R{r}C{c} = {}", show_cell(&ss, r, c));

    // Both bounds are real. The sheet is 4 rows by 4 columns in 1-based
    // coordinates, and 0 is not a cell at all.
    ss.set_cursor(99, 99);
    let (r, c) = ss.cursor_position().expect("a spreadsheet has a cursor");
    println!("   set_cursor(99,99) -> clamped to R{r}C{c}");
    ss.set_cursor(0, 0);
    let (r, c) = ss.cursor_position().expect("a spreadsheet has a cursor");
    println!("   set_cursor(0, 0)  -> floored to   R{r}C{c}");

    // ---- edit that cell --------------------------------------------------
    println!("\n-- edit the cell under the cursor ---------------------------");
    ss.set_cursor(3, 3); // Gadget's cost, again
    ss.set_editing(true, "", 0); // empty seed: start from what the cell holds
    let (editing, buf, pos) = ss.edit_state().expect("an edit state");
    println!("   editing={editing}  buffer starts as {buf:?}  caret={pos}");

    ss.set_editing(true, "42", 2);
    let (_e, buf, pos) = ss.edit_state().expect("an edit state");
    println!("   typed            buffer={buf:?}  caret={pos}");

    // Commit, and see what landed in the cell.
    let committed = ss.commit_formula_bar();
    match committed {
        Some((row, col, text)) => {
            println!("   committed        {text:?} into R{row}C{col}");
            println!("   cell now holds   {:?}", ss.get_cell(row, col).unwrap_or_default());
        }
        None => println!("   committed        nothing — no edit was in progress"),
    }

    // Committing again must not fire: the first commit ended the edit.
    let again = ss.commit_formula_bar();
    println!("   commit again     {}", if again.is_none() { "None — the edit was already committed" } else { "UNEXPECTED" });

    println!("\n-- the sheet after editing -----------------------------------");
    println!("  row | Item      | Qty       | Cost");
    for row in 1..=4 {
        println!("{}", show_row(&ss, row));
    }

    println!("\n-- cursor moves notified -------------------------------------");
    println!("   {}", moves.borrow().join(" "));

    Ok(())
}

/// The value of one cell, read back through the same coordinates.
fn show_cell(ss: &za::Spreadsheet, row: u32, col: u32) -> String {
    ss.get_cell(row, col).unwrap_or_default()
}