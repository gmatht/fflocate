//! Interactive text REPL driver over the [`crate::backends::zork::model`].
//!
//! This is deliberately *one* of possibly many drivers of the pure model. It is
//! kept for manual exploration and demos. For automated tests prefer
//! [`crate::backends::zork::harness::Harness`].

use std::io::{self, BufRead, Write};

use crate::backends::BackendApp;
use crate::backends::zork::model::{ZorkKind, ZorkNode, ZorkState};

/// Virtual milliseconds the model's clock advances by at each REPL prompt.
///
/// Nominal rather than measured: the point is that a turn is a tick, so a
/// timer a host installed actually comes due, not that the REPL is a real-time
/// scheduler. It is the same figure the model starts with in a test, so a REPL
/// session and a test see the same deadlines.
const REPL_TICK_MS: u64 = 50;

/// What a spreadsheet verb says when the model holds no `Spreadsheet`.
///
/// corro's own build never makes one, and that is the model this REPL starts
/// with, so this is the first thing a reader of `sheet` or `grid` meets. The
/// two verbs did not agree: `grid` returned on an unresolved target without
/// printing, so `grid` on corro's tree was silent where `sheet` explained
/// itself. Silence is the worst of the three possible answers -- it reads as
/// an empty sheet, a hang, or a verb that does not exist. One constant, so the
/// second verb written cannot quietly re-acquire the bug.
const NO_SPREADSHEET: &str = "There is no Spreadsheet in this model.";

/// What a spreadsheet verb says when the target named something else.
///
/// A distinct answer from [`NO_SPREADSHEET`]: `grid 1` on corro's tree names
/// the Window, which exists. "No Spreadsheet" would blame the model for a
/// target the reader chose, and "the sheet is empty" would blame the sheet
/// for a sheet that was never there.
const NOT_A_SPREADSHEET: &str = "That is not a Spreadsheet.";

pub struct ZorkApp {
    state: ZorkState,
    /// Standees already told that a `sheet` command aimed at a spreadsheet
    /// they are not standing on. Presentation state, not model state: it
    /// belongs to the *reader*, and it is deliberately not in the snapshot so
    /// it cannot affect a regression comparison.
    sheet_aim_notice: Vec<usize>,
}

impl ZorkApp {
    /// A REPL over a fresh, empty model — the standalone case.
    pub fn new() -> Self {
        ZorkApp { state: ZorkState::new(), sheet_aim_notice: Vec::new() }
    }

    /// A REPL over the *existing* singleton model.
    ///
    /// [`Self::new`] starts empty, so a host that has already built a widget
    /// tree through the adapter (corro builds ~100 widgets before handing off)
    /// would get a REPL that can see none of it. This adopts whatever is
    /// already there.
    pub fn from_existing_state(state: ZorkState) -> Self {
        ZorkApp { state, sheet_aim_notice: Vec::new() }
    }

    /// Take the singleton model's current contents.
    pub fn adopt_singleton() -> Self {
        ZorkApp::from_existing_state(super::model::take_state())
    }
}

impl Default for ZorkApp {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendApp for ZorkApp {
    /// A REPL owns its input loop: nothing else dispatches for us.
    fn owns_event_loop(&self) -> bool {
        true
    }

    fn run(mut self: Box<Self>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.state.nodes.is_empty() {
            return Ok(());
        }
        self.state.running = true;

        let mut rl = rustyline::DefaultEditor::new()?;

        while self.state.running {
            // The model has no wall clock, so a timer fires when the clock
            // moves. A REPL turn is the only place time passes here, so each
            // prompt advances it by a nominal 50ms — enough for a timer with
            // any sane interval to come due within a few turns, and it makes
            // `wait` a way to *skip* ahead rather than the only way forward.
            // Without this, a host that installs a timer and hands off to the
            // REPL would see a timer that never fires and no error.
            self.state.advance_timers(REPL_TICK_MS);

            if let Some(node) = self.state.node(self.state.current_id) {
                describe_room(&self.state, node);
            }

            let prompt = "> ";
            let readline = rl.readline(prompt);
            match readline {
                Ok(line) => {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() {
                        rl.add_history_entry(&trimmed)?;
                        execute_command(&mut self.state, &mut self.sheet_aim_notice, &trimmed);
                    }
                }
                Err(rustyline::error::ReadlineError::Interrupted)
                | Err(rustyline::error::ReadlineError::Eof) => {
                    self.state.running = false;
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }
}

/// Strip a menu label's accelerator marker, so `_File` reads as "File".
///
/// corro writes the mnemonic as an underscore anywhere in the label: `_File`
/// is Alt+F, `Fo_rmat` is Alt+O. The direction list names places for a human,
/// so the marker has to go. Every underscore is dropped except a trailing one,
/// which is never a mnemonic (a label cannot end mid-mnemonic) and would turn
/// `_File_` into `File` rather than mangling it.
fn menu_label_plain(label: &str) -> String {
    let chars: Vec<char> = label.chars().collect();
    let mut out = String::with_capacity(label.len());
    for (i, c) in chars.iter().enumerate() {
        if *c == '_' && i + 1 < chars.len() {
            continue;
        }
        out.push(*c);
    }
    out
}

fn short_desc(_state: &ZorkState, node: &ZorkNode) -> String {
    match &node.kind {
        ZorkKind::Window { title } => format!("Window \"{}\"", title),
        ZorkKind::Button { label } => format!("Button \"{}\"", label),
        ZorkKind::Label { text } => {
            let preview: String = text.chars().take(30).collect();
            let preview = if text.chars().count() > 30 { format!("{}...", preview) } else { text.clone() };
            format!("Label: \"{}\"", preview)
        }
        ZorkKind::Entry { .. } => "Entry".into(),
        ZorkKind::CheckButton { label, .. } => format!("CheckButton \"{}\"", label),
        ZorkKind::RadioButton { label, .. } => format!("RadioButton \"{}\"", label),
        ZorkKind::BoxWidget { horizontal, .. } => format!("{} Box", if *horizontal { "Horizontal" } else { "Vertical" }),
        ZorkKind::Grid { .. } => "Grid".into(),
        ZorkKind::Dialog { title } => format!("Dialog \"{}\"", title),
        ZorkKind::DropDown { items, selected } => {
            let current = selected.and_then(|s| items.get(s)).map(|s| s.as_str()).unwrap_or("(none)");
            format!("DropDown [{}]", current)
        }
        ZorkKind::TextView { .. } => "TextView".into(),
        ZorkKind::Canvas { .. } => "Canvas".into(),
        ZorkKind::Overlay => "Overlay".into(),
        ZorkKind::ScrolledWindow => "ScrolledWindow".into(),
        ZorkKind::Fixed => "Fixed".into(),
        ZorkKind::Application => "Application".into(),
        ZorkKind::Spreadsheet { .. } => "Spreadsheet".into(),
        ZorkKind::Menu => match &node.menu_label {
            Some(label) => format!("{} Menu", menu_label_plain(label)),
            None => "Menu".into(),
        },
        // The bar is not a submenu of anything, so it is not "X Menu" -- the
        // label already says what it is. Appending "Menu" here produced
        // "Menu Bar Menu".
        ZorkKind::MenuBar => match &node.menu_label {
            Some(label) => menu_label_plain(label),
            None => "MenuBar".into(),
        },
        ZorkKind::SimpleAction => "SimpleAction".into(),
    }
}

/// A one-line property summary, printed by `props` and `look -v`.
///
/// The model records geometry, visibility, expansion, margins, classes, focus
/// and scroll for every node; before this the REPL could not show any of them,
/// so a `set` had no visible effect from the player's side.
fn prop_summary(state: &ZorkState, node: &ZorkNode) -> String {
    let p = &node.props;
    let mut out: Vec<String> = Vec::new();
    if let (Some(x), Some(y)) = (p.offset_x, p.offset_y) {
        out.push(format!("at ({}, {})", x, y));
    }
    if let (Some(w), Some(h)) = (p.width, p.height) {
        out.push(format!("size {}x{}", w, h));
    }
    if !p.visible {
        out.push("hidden".to_string());
    }
    if p.hexpand {
        out.push("hexpand".to_string());
    }
    if p.vexpand {
        out.push("vexpand".to_string());
    }
    if p.margin_start != 0 {
        out.push(format!("margin-start {}", p.margin_start));
    }
    if p.margin_top != 0 {
        out.push(format!("margin-top {}", p.margin_top));
    }
    if !p.classes.is_empty() {
        out.push(format!("classes [{}]", p.classes.join(", ")));
    }
    if p.fixed_width.is_some() {
        out.push(format!("fixed-width {:?}", p.fixed_width));
    }
    if let Some(f) = &p.font {
        out.push(format!("font {:?} @ {}", f, p.font_size));
    }
    if p.hscroll != 0.0 || p.vscroll != 0.0 {
        out.push(format!("scroll ({}, {})", p.hscroll, p.vscroll));
    }
    if state.has_focus(node.id) {
        out.push("focused".to_string());
    }
    if node.destroyed {
        out.push("destroyed".to_string());
    }
    if out.is_empty() {
        "no properties set".to_string()
    } else {
        out.join(", ")
    }
}


fn describe_room(state: &ZorkState, node: &ZorkNode) {
    println!();
    // In the dark you cannot see the room, but you absolutely can be eaten.
    // This is checked before the room itself, because a player who is about to
    // be killed by a grue should be told so before they get a detailed tour of
    // the furniture.
    if state.is_night() {
        println!("{}", state.dark_room_description());
    } else {
        describe_room_lit(state, node);
    }
}

/// The normal, sighted description. Split out so the night-mode check in
/// [`describe_room`] reads as a gate rather than as another match arm.
fn describe_room_lit(state: &ZorkState, node: &ZorkNode) {
    match &node.kind {
        ZorkKind::Window { title } => {
            println!("You are in a Window{}", if title.is_empty() { ".".to_string() } else { format!(" titled \"{}\".", title) });
        }
        ZorkKind::Dialog { title } => {
            println!("You are in a Dialog{}", if title.is_empty() { ".".to_string() } else { format!(" titled \"{}\".", title) });
        }
        ZorkKind::Button { label } => {
            println!("You are standing on a Button labeled \"{}\".", label);
        }
        ZorkKind::Label { text } => {
            let preview: String = text.chars().take(40).collect();
            let preview = if text.chars().count() > 40 { format!("{}...", preview) } else { text.clone() };
            println!("You are looking at a Label: \"{}\".", preview);
        }
        ZorkKind::Entry { buffer, .. } => {
            let preview: String = buffer.chars().take(40).collect();
            let preview = if buffer.is_empty() {
                "empty".into()
            } else if buffer.chars().count() > 40 {
                format!("{}...", preview)
            } else {
                buffer.clone()
            };
            println!("You are at an Entry containing \"{}\".", preview);
        }
        ZorkKind::CheckButton { label, checked } => {
            println!("You are at a CheckButton labeled \"{}\" ({}).", label, if *checked { "checked" } else { "unchecked" });
        }
        ZorkKind::RadioButton { label, checked, .. } => {
            println!("You are at a RadioButton labeled \"{}\" ({}).", label, if *checked { "selected" } else { "not selected" });
        }
        ZorkKind::DropDown { items, selected } => {
            let current = selected.and_then(|s| items.get(s)).map(|s| s.as_str()).unwrap_or("(none)");
            println!("You are at a DropDown. Current selection: \"{}\".", current);
        }
        ZorkKind::TextView { text } => {
            let preview: String = text.chars().take(40).collect();
            let preview = if text.chars().count() > 40 { format!("{}...", preview) } else { text.clone() };
            println!("You are reading a TextView: \"{}\".", preview);
        }
        ZorkKind::BoxWidget { horizontal, .. } => {
            println!("You are in a {} Box.", if *horizontal { "horizontal" } else { "vertical" });
        }
        ZorkKind::Grid { .. } => {
            println!("You are in a Grid layout.");
        }
        ZorkKind::MenuBar => {
            match &node.menu_label {
                Some(label) => println!("You are at the {}.", menu_label_plain(label)),
                None => println!("You are at a MenuBar."),
            }
            let items = state.menu_items.get(&node.id).cloned().unwrap_or_default();
            if !items.is_empty() {
                println!("Menu items:");
                for (i, item) in items.iter().enumerate() {
                    println!("  {}. {}", i + 1, item.label);
                }
            }
        }
        ZorkKind::Menu => {
            match &node.menu_label {
                Some(label) => println!("You are at the {} Menu.", menu_label_plain(label)),
                None => println!("You are at a Menu."),
            }
            let items = state.menu_items.get(&node.id).cloned().unwrap_or_default();
            if !items.is_empty() {
                println!("Items:");
                for (i, item) in items.iter().enumerate() {
                    println!("  {}. {}", i + 1, item.label);
                }
            }
        }
        _ => {
            println!("You are in an unknown widget.");
        }
    }

    // Directions, from the shared helper so `navigate_find`, the printed
    // "To the <dir>" lines and the numbered list cannot disagree about which
    // node is north.
    let dirs: Vec<(&str, &ZorkNode)> = navigate_dirs(state, node);

    if !dirs.is_empty() {
        println!();
        for (dir, target) in &dirs {
            let desc = short_desc(state, target);
            println!("To the {}: {}", dir, desc);
        }
    }

    let summary = prop_summary(state, node);
    if summary != "no properties set" {
        println!();
        println!("({})", summary);
    }

    println!();
    println!("You see:");
    println!("  0. (yourself) {}", short_desc(state, node));
    let mut idx = 1;
    for id in visible_from(state, node) {
        // A node can be both a direction and a sibling; it is listed once, with
        // its direction noted, because the reader can only move to it one way.
        let dir = dirs
            .iter()
            .find(|(_, t)| t.id == id)
            .map(|(d, _)| format!(" ({})", d))
            .unwrap_or_default();
        if let Some(other) = state.node(id) {
            println!("  {}. {}{}", idx, short_desc(state, other), dir);
        }
        idx += 1;
    }
    println!();
}

/// The directions out of `node`, in the order the REPL names them.
///
/// One definition, used by the printed direction lines, the numbered "You see"
/// list, `navigate_find` and [`resolve_number_target`]. When those were four
/// separate computations they agreed only by coincidence, and the numbered
/// list — the one the reader actually uses to pick a target — was not the list
/// `go <n>` walked.
fn navigate_dirs<'a>(state: &'a ZorkState, node: &ZorkNode) -> Vec<(&'static str, &'a ZorkNode)> {
    let siblings: Vec<&ZorkNode> = if let Some(pid) = node.parent {
        state
            .node(pid)
            .map(|p| p.children.iter().filter_map(|cid| state.node(*cid)).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let my_idx = siblings.iter().position(|s| s.id == node.id);

    let mut dirs: Vec<(&'static str, &'a ZorkNode)> = Vec::new();
    if let Some(idx) = my_idx {
        if idx > 0 {
            if let Some(prev) = siblings.get(idx - 1) {
                dirs.push(("west", prev));
            }
        }
        if idx + 1 < siblings.len() {
            if let Some(next) = siblings.get(idx + 1) {
                dirs.push(("east", next));
            }
        }
    }
    if let Some(first) = node.children.iter().filter_map(|cid| state.node(*cid)).next() {
        dirs.push(("north", first));
    }
    if let Some(pid) = node.parent {
        if let Some(p) = state.node(pid) {
            dirs.push(("south", p));
        }
    }
    dirs
}

/// The numbered list the reader sees after "You see:", excluding yourself
/// (which is always `0`): the directions first, then the parent's other
/// children, then this node's own children — each node once.
fn visible_from(state: &ZorkState, node: &ZorkNode) -> Vec<usize> {
    let dirs = navigate_dirs(state, node);
    let mut items: Vec<usize> = Vec::new();
    let mut push = |id: usize| {
        if id != node.id && !items.contains(&id) {
            items.push(id);
        }
    };
    for (_, target) in &dirs {
        push(target.id);
    }
    if let Some(pid) = node.parent {
        if let Some(p) = state.node(pid) {
            for cid in &p.children {
                push(*cid);
            }
        }
    }
    for cid in &node.children {
        push(*cid);
    }
    items
}

/// `aim_notice` is the driver's standee-aimed-at-already list; see
/// [`ZorkApp::sheet_aim_notice`].
fn execute_command(state: &mut ZorkState, aim_notice: &mut Vec<usize>, line: &str) {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return;
    }

    let cmd = parts[0].to_lowercase();
    let args: Vec<&str> = parts[1..].to_vec();

    match cmd.as_str() {
        "look" | "l" => {
            if let Some(node) = state.node(state.current_id) {
                describe_room(state, node);
                warn_grue(state);
            }
        }
        "light" | "lantern" => {
            if state.is_night() {
                state.set_night_mode(false);
                println!("You light the lantern. The room comes into view, and you feel a great deal safer.");
            } else {
                println!("The lantern is already lit.");
            }
        }
        "dark" | "douse" => {
            if state.is_night() {
                println!("It is already pitch dark.");
            } else {
                state.set_night_mode(true);
                println!("You douse the lantern. The room vanishes.");
            }
            // Going dark is exactly when a warning is due.
            warn_grue(state);
        }
        "grue" => {
            // The player is checking for one. Answer honestly, and do not
            // latch the warning here: asking should always be answerable.
            if state.is_night() {
                println!("You feel it. The grue is here, lurking in the dark, and it is very likely to eat you.");
            } else {
                println!("No grue here. The light keeps it at bay.");
            }
        }
        "go" | "g" => {
            if args.is_empty() {
                println!("Go where? (north/south/east/west or a number)");
                return;
            }
            let dir = args[0].to_lowercase();
            navigate_to(state, &dir);
        }
        "north" | "n" => navigate_to(state, "north"),
        "south" | "s" => navigate_to(state, "south"),
        "east" | "e" => navigate_to(state, "east"),
        "west" | "w" => navigate_to(state, "west"),
        "click" | "press" => {
            let target_id = if !args.is_empty() {
                resolve_arg_to_id(state, args[0])
            } else {
                Some(state.current_id)
            };
            if let Some(id) = target_id {
                let kind_is_menu = state.node(id).map(|n| matches!(n.kind, ZorkKind::MenuBar | ZorkKind::Menu)).unwrap_or(false);
                if kind_is_menu {
                    {
                        let items = state.menu_items.get(&id).cloned().unwrap_or_default();
                        if items.is_empty() {
                            println!("This menu has no items.");
                        } else {
                            println!("Select an item:");
                            for (i, item) in items.iter().enumerate() {
                                let mark = match item.kind {
                                    crate::backends::zork::model::MenuItemKind::Separator => " (separator)",
                                    crate::backends::zork::model::MenuItemKind::Section => " (section)",
                                    crate::backends::zork::model::MenuItemKind::Check => {
                                        if item.checked { " [x]" } else { " [ ]" }
                                    }
                                    crate::backends::zork::model::MenuItemKind::Radio => {
                                        if item.checked { " (o)" } else { " ( )" }
                                    }
                                    crate::backends::zork::model::MenuItemKind::Normal => "",
                                };
                                println!("  {}. {}{}{}", i + 1, item.label, mark, if item.accelerator.is_empty() { String::new() } else { format!("  ({})", item.accelerator) });
                            }
                        }
                    }
                    return;
                }
                // Everything else that can be activated goes through
                // `pointer_click`, so its click hooks run before its
                // callbacks. The old match only handled Button and said
                // "You can't click that" for an Entry or CheckButton, which do
                // have click semantics.
                let clickable = state.node(id).is_some_and(|n| {
                    matches!(
                        n.kind,
                        ZorkKind::Button { .. }
                            | ZorkKind::Entry { .. }
                            | ZorkKind::CheckButton { .. }
                            | ZorkKind::RadioButton { .. }
                            | ZorkKind::DropDown { .. }
                            | ZorkKind::TextView { .. }
                            | ZorkKind::Canvas { .. }
                    )
                });
                if !clickable {
                    println!("You can't click that.");
                    return;
                }
                let desc = short_desc(state, state.node(id).unwrap());
                // A toggleable button flips; anything else just activates.
                if state.node(id).is_some_and(|n| {
                    matches!(n.kind, ZorkKind::CheckButton { .. } | ZorkKind::RadioButton { .. })
                }) {
                    state.toggle(id);
                    println!("You press the {}.", desc);
                } else {
                    state.pointer_click(id, 0.0, 0.0);
                    println!("You press the {}. It clicks!", desc);
                }
            }
        }
        "select" | "choose" => {
            if args.is_empty() {
                println!("Select what? Use 'select <number>'.");
                return;
            }
            let num: usize = match args[0].parse() {
                Ok(n) => n,
                Err(_) => {
                    println!("Usage: select <number>");
                    return;
                }
            };
            // Route through `menu_select`, which fires the SimpleAction the
            // item names. The old code only printed the action name, so nothing
            // ever happened when you picked an item.
            let idx = num.saturating_sub(1);
            match state.menu_select(state.current_id, idx) {
                Some(label) => println!("You selected \"{}\".", label),
                None => println!("Nothing selectable there."),
            }
        }
        "examine" | "exam" | "x" => {
            let target_id = if !args.is_empty() {
                resolve_arg_to_id(state, args[0])
            } else {
                Some(state.current_id)
            };
            if let Some(id) = target_id {
                examine(state, id);
            }
        }
        "type" | "write" => {
            // `type <n> [text]` targets a specific Entry; bare `type` uses the
            // one you are standing on. Requiring a walk to the target first
            // made the verb unusable for driving a form with two entries, which
            // is the only reason a form has more than one (R6 in
            // docs/ZORK_MISSING.md §5).
            //
            // A first word is read as a target only when it *resolves* to
            // something. Without that, `type hello there` on a non-Entry
            // answered `there is no "hello" here` — a nonsense complaint about
            // what the reader plainly meant, and one that hid the real problem
            // (you are not at an Entry) behind a remark about their text.
            let at_entry = matches!(state.node(state.current_id).map(|n| &n.kind), Some(ZorkKind::Entry { .. }));
            let is_address =
                |w: &str| w.chars().all(|c| c.is_ascii_digit()) || matches!(w.to_ascii_lowercase().as_str(), "north" | "south" | "east" | "west" | "n" | "s" | "e" | "w");
            let (target_id, rest) = match args.first() {
                Some(first) if resolve_arg_to_id(state, first).is_some() => {
                    (resolve_arg_to_id(state, first), args[1..].to_vec())
                }
                // Unresolvable, but we are at an Entry: the word is the text.
                _ if at_entry => (Some(state.current_id), args.to_vec()),
                // Unresolvable and not at an Entry. A bare number or a
                // direction that names nothing is a genuine targeting mistake
                // and is worth saying so about.
                Some(first) if is_address(first) => {
                    println!("There is no \"{}\" here.", first);
                    return;
                }
                // Otherwise the whole line is the text, and the check below
                // reports the actual problem.
                _ => (Some(state.current_id), args.to_vec()),
            };
            let Some(id) = target_id else { return };
            if !matches!(state.node(id).map(|n| &n.kind), Some(ZorkKind::Entry { .. })) {
                println!("That is not an Entry.");
                return;
            }
            // The text is whatever followed the target on the line, so
            // `type 3 hello` types "hello" without a second prompt and
            // `type 3` still opens one for a longer value.
            let text = if rest.is_empty() {
                println!("(Enter text, press Enter when done)");
                print!("> ");
                io::stdout().flush().ok();
                let mut input = String::new();
                io::stdin().lock().read_line(&mut input).ok();
                input.trim().to_string()
            } else {
                rest.join(" ")
            };
            state.set_entry_text(id, &text);
            println!("You inscribed \"{}\" into the Entry.", text);
            state.click(id);
        }
        "read" => {
            examine(state, state.current_id);
        }
        "toggle" => {
            // `ZorkState::toggle` is group-aware, so using it here means a
            // radio toggle clears its siblings (the old inline toggle just
            // flipped this button and left the group inconsistent).
            let id = state.current_id;
            let is_toggleable = state.node(id).is_some_and(|n| {
                matches!(n.kind, ZorkKind::CheckButton { .. } | ZorkKind::RadioButton { .. })
            });
            if !is_toggleable {
                println!("You can't toggle that.");
                return;
            }
            let was = match state.node(id) {
                Some(n) => match &n.kind {
                    ZorkKind::CheckButton { checked, .. } | ZorkKind::RadioButton { checked, .. } => *checked,
                    _ => false,
                },
                None => false,
            };
            state.toggle(id);
            let now = match state.node(id) {
                Some(n) => match &n.kind {
                    ZorkKind::CheckButton { checked, .. } | ZorkKind::RadioButton { checked, .. } => *checked,
                    _ => false,
                },
                None => false,
            };
            let noun = match state.node(id).map(|n| &n.kind) {
                Some(ZorkKind::CheckButton { .. }) => "CheckButton",
                Some(ZorkKind::RadioButton { .. }) => "RadioButton",
                _ => "widget",
            };
            println!("{} is now {}.", noun, if now { "on" } else { "off" });
            // A radio group only ever has one member on; if this toggle turned
            // something on, say which sibling it cleared.
            if now && !was {
                if let Some(ZorkKind::RadioButton { group_id, .. }) = state.node(id).map(|n| &n.kind) {
                    if *group_id != 0 {
                        let others_off = state
                            .nodes
                            .iter()
                            .filter(|n| matches!(n.kind, ZorkKind::RadioButton { group_id: g, .. } if g == *group_id && n.id != id))
                            .all(|n| !matches!(n.kind, ZorkKind::RadioButton { checked: true, .. }));
                        if others_off {
                            println!("The rest of the group is now unselected.");
                        }
                    }
                }
            }
        }
        "props" | "attributes" => {
            if let Some(node) = state.node(state.current_id) {
                println!("{}: {}", short_desc(state, node), prop_summary(state, node));
                if let ZorkKind::Entry { buffer, cursor } = &node.kind {
                    println!("  caret at {} of {} chars", cursor, buffer.chars().count());
                }
                if let ZorkKind::Canvas { .. } = &node.kind {
                    println!("  redraw requests: {}", node.pointer.redraws);
                }
                if !node.dialog_buttons.is_empty() {
                    let btns: Vec<String> = node
                        .dialog_buttons
                        .iter()
                        .map(|(l, r)| format!("{}={}", l, r))
                        .collect();
                    println!("  buttons: {}", btns.join(", "));
                }
                if !node.cells.is_empty() {
                    println!("  {} cell(s) set", node.cells.len());
                }
            }
        }
        "set" => {
            if args.len() < 2 {
                println!("Usage: set <property> <value>");
                println!("  properties: size, offset, margin-start, margin-top, class, xalign, fixed-width, font, scroll");
                return;
            }
            let prop = args[0].to_lowercase();
            let id = state.current_id;
            // The value is the rest of the line, so a class name or font can
            // contain spaces.
            let value = args[1..].join(" ");
            match prop.as_str() {
                "size" => {
                    if let Some((w, h)) = parse_pair(&value) {
                        state.set_size_request(id, w, h);
                        println!("Size set to {}x{}.", w, h);
                    } else {
                        println!("Usage: set size <w> <h>");
                    }
                }
                "offset" => {
                    if let Some((x, y)) = parse_pair(&value) {
                        state.set_offset(id, x, y);
                        println!("Offset set to ({}, {}).", x, y);
                    } else {
                        println!("Usage: set offset <x> <y>");
                    }
                }
                "margin-start" => {
                    if let Some(px) = value.parse::<i32>().ok() {
                        state.set_margin_start(id, px);
                        println!("Margin-start set to {}.", px);
                    } else {
                        println!("Usage: set margin-start <px>");
                    }
                }
                "margin-top" => {
                    if let Some(px) = value.parse::<i32>().ok() {
                        state.set_margin_top(id, px);
                        println!("Margin-top set to {}.", px);
                    } else {
                        println!("Usage: set margin-top <px>");
                    }
                }
                "class" => {
                    if let Some(cls) = value.strip_suffix('+') {
                        state.add_class(id, cls);
                        println!("Added class \"{}\".", cls);
                    } else if let Some(cls) = value.strip_suffix('-') {
                        state.remove_class(id, cls);
                        println!("Removed class \"{}\".", cls);
                    } else {
                        state.add_class(id, &value);
                        println!("Added class \"{}\" (use 'set class <name>-' to remove).", value);
                    }
                }
                "xalign" => {
                    if let Some(x) = value.parse::<f32>().ok() {
                        state.set_xalign(id, x);
                        println!("x-align set to {}.", x);
                    } else {
                        println!("Usage: set xalign <0.0-1.0>");
                    }
                }
                "fixed-width" => {
                    match value.parse::<i32>() {
                        Ok(w) => {
                            state.set_fixed_width(id, Some(w));
                            println!("Width pinned to {}.", w);
                        }
                        // "none" / "off" release the pin.
                        Err(_) if matches!(value.as_str(), "none" | "off") => {
                            state.set_fixed_width(id, None);
                            println!("Width pin released.");
                        }
                        _ => println!("Usage: set fixed-width <px> | none"),
                    }
                }
                "font" => {
                    // "font <size>" or "font <family> <size>".
                    let mut parts = value.rsplitn(2, ' ');
                    let size = parts.next().and_then(|s| s.parse::<f64>().ok());
                    let family = parts.next().unwrap_or("sans");
                    match size {
                        Some(sz) => {
                            state.set_font_style(id, Some(family), sz);
                            println!("Font set to {} @ {}.", family, sz);
                        }
                        None => println!("Usage: set font [family] <size>"),
                    }
                }
                "scroll" => {
                    if let Some((v, upper)) = parse_pair_f64(&value) {
                        state.set_scroll(id, 0.0, upper, 0.0, v, upper, 0.0);
                        println!("Scrolled to {}.", v);
                    } else {
                        println!("Usage: set scroll <value> <upper>");
                    }
                }
                "title" => {
                    state.set_window_title(id, &value);
                    println!("Title set to \"{}\".", value);
                }
                "text" => {
                    match &state.node(id).map(|n| &n.kind) {
                        Some(ZorkKind::Label { .. }) => {
                            state.set_label_text(id, &value);
                            println!("Label text set.");
                        }
                        Some(ZorkKind::TextView { .. }) => {
                            state.set_textview_text(id, &value);
                            println!("TextView text set.");
                        }
                        Some(ZorkKind::Entry { .. }) => {
                            state.set_entry_text(id, &value);
                            println!("Entry text set.");
                        }
                        _ => println!("That has no text."),
                    }
                }
                "check" => {
                    match value.as_str() {
                        "on" | "true" | "yes" | "1" => {
                            state.set_checkbutton_checked(id, true);
                            state.set_radiobutton_checked(id, true);
                            println!("Checked.");
                        }
                        "off" | "false" | "no" | "0" => {
                            state.set_checkbutton_checked(id, false);
                            state.set_radiobutton_checked(id, false);
                            println!("Unchecked.");
                        }
                        _ => println!("Usage: set check on|off"),
                    }
                }
                _ => println!("Unknown property \"{}\". Try 'help'.", prop),
            }
        }
        "hide" => {
            state.set_visible(state.current_id, false);
            println!("Hidden.");
        }
        "show" => {
            state.set_visible(state.current_id, true);
            println!("Shown.");
        }
        "focus" => {
            let id = state.current_id;
            state.set_focus(id);
            if state.has_focus(id) {
                println!("You focus it.");
            } else {
                println!("You can't focus that.");
            }
        }
        "layout" => {
            // Lay the current box's children out, and report what it needs.
            let id = state.current_id;
            if let Some((w, h)) = state.measure_box(id) {
                state.layout_box(id, 0, 0, w, h);
                println!("Box laid out: {}x{}.", w, h);
            } else {
                println!("You can only lay out a Box.");
            }
        }
        "sheet" => {
            // The F9/F10 surface has no other way to be driven or inspected
            // from here, so without this verb the chrome is reachable only
            // from Rust — and a REPL that can only `look` at a spreadsheet is
            // exactly the "model driver, not an interactive UI" limitation
            // docs/ZORK_MISSING.md §7 records.
            //
            // A *subcommand* is read before the target is resolved, because
            // `sheet cell 1 1` names a cell and not a widget: resolving `1` as
            // the target first made every cell command fail with "there is
            // nothing like that here", which is a bug that looks like a
            // missing feature.
            const SHEET_SUBS: &[&str] = &[
                "show", "cursor", "edit", "stop-edit", "commit", "cell",
            ];
            let head: &str = args.first().copied().unwrap_or("show");
            let (target, sub, rest): (Option<&str>, &str, &[&str]) = if SHEET_SUBS.contains(&head) {
                (None, head, if args.is_empty() { &[] } else { &args[1..] })
            } else {
                (Some(head), args.get(1).copied().unwrap_or("show"), if args.len() > 2 { &args[2..] } else { &[] })
            };
            let id = match target {
                Some(t) => match resolve_arg_to_id(state, t) {
                    Some(id) => id,
                    None => {
                        println!("There is nothing like \"{}\" here.", t);
                        return;
                    }
                },
                // No target: use the spreadsheet you are standing on, or —
                // failing that — the one the host built. A model that has
                // exactly one spreadsheet is not ambiguous, and refusing
                // because the player happened to be left standing on the
                // Window made `sheet` unusable in a corro session, where the
                // spreadsheet is a child of the window you start in. The note
                // is printed once per standee, not once per command, so
                // driving the grid does not scroll the explanation away.
                //
                // `resolve_spreadsheet` does exactly this, and returns the
                // sentence to print when there is nothing to aim at. It used
                // to be inlined here, so the untargeted path was written
                // twice and the two copies could disagree -- and did, once
                // `grid` was added and had no copy at all.
                None => match resolve_spreadsheet(state, None, aim_notice) {
                    Ok(id) => id,
                    Err(why) => {
                        println!("{why}");
                        return;
                    }
                },
            };
            if !matches!(state.node(id).map(|n| &n.kind), Some(ZorkKind::Spreadsheet { .. })) {
                println!("{NOT_A_SPREADSHEET}");
                return;
            }
            match sub {
                "show" => {
                    if let Some(c) = state.sheet_chrome(id) {
                        println!("Spreadsheet #{}: {}", id, state.sheet_border_title(id).unwrap_or_default());
                        println!("  cursor: R{}C{}", c.cursor.0, c.cursor.1);
                        println!("  gutter {} col(s), {} data col(s)", c.margin_cols, c.main_cols);
                        println!("  {} header row(s), {} data row(s)", c.header_rows, c.main_rows);
                        println!("  {} cell(s) set", state.node(id).map(|n| n.cells.len()).unwrap_or(0));
                        if !c.column_layout.is_empty() {
                            let cols: Vec<String> = c
                                .column_layout
                                .iter()
                                .map(|(i, w, t)| format!("{}:{} ({})", i, w, t))
                                .collect();
                            println!("  columns: {}", cols.join(", "));
                        }
                        if !c.row_labels.is_empty() {
                            let rows: Vec<String> = c.row_labels.iter().map(|(i, l)| format!("{}={}", i, l)).collect();
                            println!("  row labels: {}", rows.join(", "));
                        }
                        if !c.tabs.is_empty() {
                            let tabs: Vec<String> = c.tabs.iter().map(|t| format!("{}{}", if c.tabs.iter().position(|x| x == t) == Some(c.active_tab) { "*" } else { " " }, t)).collect();
                            println!("  tabs: {}", tabs.join(" "));
                        }
                        if !c.menu_text.is_empty() {
                            println!("  menu: {}", c.menu_text);
                        }
                        if !c.status_text.is_empty() {
                            println!("  status: {}", c.status_text);
                        }
                        if !c.formula_address.is_empty() || !c.formula_entry.is_empty() {
                            println!("  formula bar: {} {}", c.formula_address, c.formula_entry);
                        }
                        if !c.formula_bar_trailing.is_empty() {
                            println!("  trailing: {}", c.formula_bar_trailing);
                        }
                        if c.editing {
                            println!("  editing: \"{}\" (caret {})", c.edit_buf, c.edit_pos);
                        }
                    }
                }
                "cursor" => {
                    let Some((r, c)) = parse_pair(&rest.join(" ")) else {
                        println!("Usage: sheet cursor <row> <col>");
                        return;
                    };
                    if r < 0 || c < 0 {
                        println!("Rows and columns start at 0.");
                        return;
                    }
                    state.sheet_set_cursor(id, r as u32, c as u32);
                    let (r2, c2) = state.sheet_cursor(id).unwrap_or((0, 0));
                    println!("Cursor at R{}C{}.", r2, c2);
                }
                "edit" => {
                    let text = rest.join(" ");
                    state.sheet_set_editing(id, true, &text, text.chars().count());
                    println!("Editing {}", state.sheet_cursor(id).map(|(r, c)| format!("R{}C{}", r, c)).unwrap_or_default());
                }
                "stop-edit" => {
                    state.sheet_set_editing(id, false, "", 0);
                    println!("Stopped editing.");
                }
                "commit" => {
                    match state.sheet_commit_edit(id) {
                        Some((r, c, text)) => println!("Committed \"{}\" to R{}C{}.", text, r, c),
                        None => println!("Nothing was being edited."),
                    }
                }
                "cell" => {
                    // `sheet cell <row> <col> <text>` writes; omitting the text
                    // reads. A spreadsheet is a grid, so addressing a cell is
                    // the most natural thing to do with one.
                    if rest.len() < 2 {
                        println!("Usage: sheet cell <row> <col> [text]");
                        return;
                    }
                    let pair = parse_pair(&rest.join(" "));
                    if rest.len() == 2 {
                        let Some((r, c)) = pair else {
                            println!("Usage: sheet cell <row> <col> [text]");
                            return;
                        };
                        match state.sheet_get_cell(id, r as u32, c as u32) {
                            Some(t) => println!("R{}C{} = \"{}\"", r, c, t),
                            None => println!("R{}C{} is empty.", r, c),
                        }
                    } else {
                        let Some((r, c)) = parse_pair(&rest[..2].join(" ")) else {
                            println!("Usage: sheet cell <row> <col> <text>");
                            return;
                        };
                        let text = rest[2..].join(" ");
                        state.sheet_set_cell(id, r as u32, c as u32, &text, 0);
                        println!("R{}C{} = \"{}\".", r, c, text);
                    }
                }
                other => println!("Unknown sheet command \"{}\". Try 'sheet'.", other),
            }
        }
        "grid" => {
            // Render a Spreadsheet's cells as a table, with the cursor marked.
            //
            // `sheet cell 1 1` reads one cell; this reads the *sheet*, which is
            // the thing a reader actually wants to see. It exists because the
            // REPL could otherwise describe a spreadsheet's chrome in detail
            // and never show what was in it — the `docs/ZORK_WALKTHROUGH.txt`
            // section on the grid needed this verb to exist.
            let id = match resolve_spreadsheet(state, args.first().copied(), aim_notice) {
                Ok(id) => id,
                // `sheet` reports the absence; `grid` used to `return` here
                // bare, so on corro's own tree -- which builds no Spreadsheet,
                // and is the model this REPL starts with -- `grid` printed
                // nothing at all, and the reader could not tell an absent
                // sheet from an empty one. A named target (`grid 1`) lands
                // here too, so it is silent on the same model unless the
                // message is printed from this one arm.
                Err(why) => {
                    println!("{why}");
                    return;
                }
            };
            let Some(chrome) = state.sheet_chrome(id) else {
                println!("{NOT_A_SPREADSHEET}");
                return;
            };
            // The cells themselves decide the extent, because there is no one
            // index space: a caller may address a sheet 0-based (the examples
            // do) or 1-based (corro's own pnc backend does), and the model
            // stores whatever it was given. Reading from the minimum and taking
            // the maximum prints what is *there* instead of an empty band of
            // rows above it, which is what assuming 1-based printed.
            let cells = state.sheet_cells(id);
            if cells.is_empty() {
                println!("The sheet is empty.");
                return;
            }
            let (min_r, max_r) = cells.iter().fold((u32::MAX, 0), |(lo, hi), (r, _, _)| {
                (lo.min(*r), hi.max(*r))
            });
            let (min_c, max_c) = cells.iter().fold((u32::MAX, 0), |(lo, hi), (_, c, _)| {
                (lo.min(*c), hi.max(*c))
            });
            // One extra row/col when the cursor sits outside the written cells,
            // so a moved cursor is visible instead of silently off the table.
            let (cr, cc) = chrome.cursor;
            let max_r = max_r.max(cr);
            let max_c = max_c.max(cc);
            let at = |r: u32, c: u32| {
                cells
                    .iter()
                    .find(|(rr, ccc, _)| *rr == r && *ccc == c)
                    .map(|(_, _, t)| t.as_str())
                    .unwrap_or("")
            };
            let widths: Vec<usize> = (min_c..=max_c)
                .map(|c| {
                    let from_cells = cells
                        .iter()
                        .filter(|(_, cc, _)| *cc == c)
                        .map(|(_, _, t)| t.chars().count())
                        .max()
                        .unwrap_or(0);
                    // `column_layout` is 0-indexed and may name a column this
                    // sheet never writes; take it as a floor only.
                    let from_layout = chrome
                        .column_layout
                        .iter()
                        .find(|(i, _, _)| *i + min_c == c)
                        .map(|(_, w, _)| *w as usize)
                        .unwrap_or(0);
                    from_cells.max(from_layout).max(1)
                })
                .collect();
            // The label gutter is as wide as its widest label, so columns start
            // in the same place on every line. Labels are 0-indexed, cells are
            // addressed as given — the two spaces are deliberately not merged.
            let gutter = chrome
                .row_labels
                .iter()
                .map(|(_, l)| l.chars().count())
                .max()
                .unwrap_or(0)
                .max(4);
            for r in min_r..=max_r {
                let mut line = String::new();
                match chrome.row_labels.iter().find(|(i, _)| *i == r) {
                    Some((_, l)) => line.push_str(&format!("{:<width$}", l, width = gutter)),
                    None => line.push_str(&" ".repeat(gutter)),
                }
                for (i, c) in (min_c..=max_c).enumerate() {
                    let w = widths[i];
                    let mut v = at(r, c).to_string();
                    if v.chars().count() > w {
                        v = v.chars().take(w.saturating_sub(1)).collect::<String>() + "\u{2026}";
                    }
                    v = format!("{:<width$}", v, width = w);
                    // The cursor is marked in the text, not by colour, so the
                    // output survives a pipe and a log.
                    line.push_str(&if chrome.cursor == (r, c) {
                        format!("[{}]", v.trim_end())
                    } else {
                        v
                    });
                    line.push('|');
                }
                println!("{line}");
            }
            println!(
                "  cursor at ({cr},{cc}); {} row(s) x {} col(s) from ({min_r},{min_c}); {} cell(s) set",
                max_r - min_r + 1,
                max_c - min_c + 1,
                cells.len()
            );
            if chrome.editing {
                println!("  editing \"{}\" (caret {})", chrome.edit_buf, chrome.edit_pos);
            }
        }
        "timers" => {
            // Timers fire on a *virtual* clock, so a REPL user has to be told
            // how to move it — otherwise `timers` looks like a list of things
            // that will never happen.
            if state.timer_count() == 0 {
                println!("No timers are pending.");
            } else {
                println!("{} timer(s) pending, clock at {}ms:", state.timer_count(), state.now_ms);
                for t in &state.timers {
                    let kind = if t.handler.is_some() { "repeating" } else { "once" };
                    println!(
                        "  due at {}ms (every {}ms, {})",
                        t.due_at, t.interval_ms, kind
                    );
                }
            }
        }
        "wait" => {
            // Advance the virtual clock, firing whatever is due. The one verb
            // that makes `timeout_add_once`/`_repeating` observable from here.
            let ms: u64 = args.first().and_then(|a| a.parse().ok()).unwrap_or(100);
            let fired = state.advance_timers(ms);
            println!(
                "{}ms later, {} timer(s) fired; clock at {}ms.",
                ms, fired, state.now_ms
            );
        }
        "dump" => {
            // The REPL's view of `debug_dump_tree`: the same walk, written to
            // a file, but over the REPL's *own* state. `adopt_singleton` moved
            // the model out of the thread-local, so the facade's singleton
            // entry point would dump an empty tree — and reporting "0 nodes"
            // for a session that just built a hundred widgets is exactly the
            // kind of wrong answer a diagnostic must not give.
            let path: &str = args.first().copied().unwrap_or("zork_tree.txt");
            match super::facade::debug_dump_tree_of(state, path) {
                Ok(n) => println!("Wrote {} node(s) to \"{}\".", n, path),
                Err(e) => println!("Could not write \"{}\": {}", path, e),
            }
        }
        "inventory" | "i" => {
            println!("Current path:");
            let mut path: Vec<String> = Vec::new();
            let mut cur = Some(state.current_id);
            while let Some(id) = cur {
                if let Some(node) = state.node(id) {
                    path.push(short_desc(state, node));
                    cur = node.parent;
                } else {
                    break;
                }
            }
            path.reverse();
            for (i, desc) in path.iter().enumerate() {
                println!("  {}{}", "  ".repeat(i), desc);
            }
        }
        "back" => {
            if let Some(pid) = state.node(state.current_id).and_then(|n| n.parent) {
                state.current_id = pid;
                println!("You go back south.");
            } else {
                println!("You can't go back from here.");
            }
        }
        "quit" | "q" | "exit" => {
            println!("Goodbye!");
            state.running = false;
        }
        "help" | "?" => {
            println!("Commands:");
            println!("  look / l              - describe your surroundings");
            println!("  go north/south/east/west - move in a direction");
            println!("  go <number>           - go to numbered item");
            println!("  north/n, south/s, east/e, west/w - quick move");
            println!("  click / press [n]     - press a button or open menu (default: current)");
            println!("  select / choose <n>   - select a menu item from a MenuBar/Menu");
            println!("  examine / x [n]       - examine something in detail");
            println!("  type / write [n] [text] - type into an Entry (default: current)");
            println!("                        with <n> a target and text, types it;");
            println!("                        without text, opens a sub-prompt");
            println!("  light / lantern       - light the lantern (the grue fears it)");
            println!("  dark / douse          - douse the lantern, and meet the grue");
            println!("  grue                  - ask whether a grue is here");
            println!("  read                  - read text at current location");
            println!("  toggle                - toggle a CheckButton/RadioButton");
            println!("  props                 - show this widget's recorded properties");
            println!("  set <prop> <value>    - set size/offset/margin/class/xalign/");
            println!("                          fixed-width/font/scroll/title/text/check");
            println!("  hide / show           - toggle visibility");
            println!("  focus                 - take keyboard focus");
            println!("  layout                - lay out a Box's children");
            println!("  sheet [n]             - show a Spreadsheet's chrome (cursor, tabs, status)");
            println!("  grid [n]              - print a Spreadsheet's cells as a table");
            println!("  sheet cursor <r> <c>  - move the spreadsheet cursor");
            println!("  sheet cell <r> <c> [text] - read or write a cell");
            println!("  sheet edit <text>     - start editing the cursor cell");
            println!("  sheet commit          - commit the in-progress edit");
            println!("  sheet stop-edit       - abandon the in-progress edit");
            println!("  timers                - list pending timers and the virtual clock");
            println!("  wait [ms]             - advance the virtual clock, firing timers (default 100)");
            println!("  dump [path]           - write the widget tree to a file (default zork_tree.txt)");
            println!("  inventory / i         - show your path");
            println!("  back                  - go back the way you came");
            println!("  quit / q / exit       - exit the game");
            println!("  help / ?              - show this help");
        }
        _ => {
            if let Ok(num) = cmd.parse::<usize>() {
                let target = resolve_number_target(state, num);
                if let Some(id) = target {
                    let desc = state.node(id).map(|n| short_desc(state, n)).unwrap_or_default();
                    state.prev_location = Some(state.current_id);
                    state.current_id = id;
                    println!("You move to {}.", desc);
                } else {
                    println!("Invalid number.");
                }
            } else {
                println!("I don't understand \"{}\". Type 'help' for commands.", cmd);
            }
        }
    }
}

/// The spreadsheet to aim at, or the sentence explaining that there is none.
///
/// Both spreadsheet verbs resolve their target through here, so the absence is
/// reported from one place and the two verbs cannot answer it differently.
/// Returning the message rather than printing it keeps the answer assertable:
/// `grid` printing nothing is precisely the bug this exists to rule out, and
/// a test that cannot see stdout would pass again on the next regression.
fn resolve_spreadsheet(
    state: &ZorkState,
    target: Option<&str>,
    aim_notice: &mut Vec<usize>,
) -> Result<usize, &'static str> {
    let is_sheet = |id: usize| {
        matches!(state.node(id).map(|n| &n.kind), Some(ZorkKind::Spreadsheet { .. }))
    };
    if let Some(t) = target {
        // A named target that resolves to something real must still be *the
        // right kind of thing*. This check cannot be left to the caller: the
        // cell reads on a non-spreadsheet all answer "no such cell", so a
        // verb that asked first would answer "The sheet is empty." for a sheet
        // that does not exist.
        let id = resolve_arg_to_id(state, t).ok_or(NO_SPREADSHEET)?;
        return if is_sheet(id) { Ok(id) } else { Err(NOT_A_SPREADSHEET) };
    }
    let here = state.current_id;
    if is_sheet(here) {
        return Ok(here);
    }
    let n = state
        .nodes
        .iter()
        .find(|n| matches!(n.kind, ZorkKind::Spreadsheet { .. }))
        .ok_or(NO_SPREADSHEET)?;
    if !aim_notice.contains(&here) {
        aim_notice.push(here);
        println!(
            "(You are not standing on it; using spreadsheet #{}. Move there to aim at a different one.)",
            n.id
        );
    }
    Ok(n.id)
}

/// Parse `"<a> <b>"` as a pair of `i32`, for `set size` / `set offset`.
fn parse_pair(value: &str) -> Option<(i32, i32)> {
    let mut it = value.split_whitespace();
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    Some((a, b))
}

/// Parse `"<a> <b>"` as a pair of `f64`, for `set scroll`.
fn parse_pair_f64(value: &str) -> Option<(f64, f64)> {
    let mut it = value.split_whitespace();
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    Some((a, b))
}

/// Print the Grue warning, if one is due.
///
/// The model latches it per dark period, so a player who runs `look` in a loop
/// is warned once rather than every command — but lighting and dousing the
/// lantern re-arms it, because a room that was lit and then went dark is a new
/// hazard rather than the same one.
fn warn_grue(state: &mut ZorkState) {
    if let Some(text) = state.grue_warning() {
        println!();
        println!("{}", text);
    }
}

fn resolve_arg_to_id(state: &ZorkState, arg: &str) -> Option<usize> {    if let Ok(num) = arg.parse::<usize>() {
        resolve_number_target(state, num)
    } else {
        let dir = arg.to_lowercase();
        navigate_find(state, &dir)
    }
}

fn resolve_number_target(state: &ZorkState, num: usize) -> Option<usize> {
    if num == 0 {
        return Some(state.current_id);
    }
    let node = state.node(state.current_id)?;
    // `visible_from` is the *same* list `describe_room_lit` prints after "You
    // see:". It used to be a second, differently-ordered computation (all
    // siblings, then all children), which meant the number on screen was not
    // the number `go <n>` accepted: the reader was shown `3. Entry` and
    // `go 3` walked somewhere else. Deriving both from one function is the
    // fix, and the numbered list is the one that has to be right — it is how
    // every targetable verb names its argument.
    visible_from(state, node).get(num - 1).copied()
}

fn navigate_find(state: &ZorkState, dir: &str) -> Option<usize> {
    let node = state.node(state.current_id)?;
    let dirs = navigate_dirs(state, node);
    dirs
        .iter()
        .find(|(d, _)| *d == dir || (*d == "west" && dir == "w") || (*d == "north" && dir == "n") || (*d == "south" && dir == "s") || (*d == "east" && dir == "e"))
        .map(|(_, t)| t.id)
}

fn navigate_to(state: &mut ZorkState, dir: &str) {
    if let Some(target_id) = navigate_find(state, dir) {
        state.prev_location = Some(state.current_id);
        state.current_id = target_id;
        println!("You go {}.", dir);
        warn_grue(state);
    } else if let Ok(num) = dir.parse::<usize>() {
        if let Some(id) = resolve_number_target(state, num) {
            let desc = state.node(id).map(|n| short_desc(state, n)).unwrap_or_default();
            state.prev_location = Some(state.current_id);
            state.current_id = id;
            println!("You move to {}.", desc);
            warn_grue(state);
        } else {
            println!("You can't go that way.");
        }
    } else {
        println!("You can't go that way.");
    }
}

fn examine(state: &mut ZorkState, id: usize) {
    if let Some(node) = state.node(id) {
        if state.is_night() {
            // You cannot make out the details, but you can feel the grue.
            println!("You cannot see it in the dark.");
            if let Some(text) = state.grue_warning() {
                println!();
                println!("{}", text);
            }
            return;
        }
        match &node.kind {
            ZorkKind::Label { text } => {
                println!("The label reads:");
                println!("\"{}\"", text);
            }
            ZorkKind::Entry { buffer, .. } => {
                if buffer.is_empty() {
                    println!("The Entry is blank.");
                } else {
                    println!("The Entry contains:");
                    println!("\"{}\"", buffer);
                }
            }
            ZorkKind::TextView { text } => {
                println!("The TextView contains:");
                for line in text.lines() {
                    println!("  {}", line);
                }
            }
            ZorkKind::Button { label } => {
                println!("A button labeled \"{}\". It looks clickable.", label);
            }
            ZorkKind::CheckButton { label, checked } => {
                println!("CheckButton \"{}\": currently {}.", label, if *checked { "CHECKED" } else { "UNCHECKED" });
            }
            ZorkKind::RadioButton { label, checked, .. } => {
                println!("RadioButton \"{}\": currently {}.", label, if *checked { "SELECTED" } else { "NOT SELECTED" });
            }
            ZorkKind::DropDown { items, selected } => {
                println!("DropDown with {} items:", items.len());
                for (i, item) in items.iter().enumerate() {
                    let marker = if Some(i) == *selected { " <--" } else { "" };
                    println!("  {}. {}{}", i, item, marker);
                }
            }
            ZorkKind::Window { title } => {
                println!("A window titled \"{}\".", title);
            }
            ZorkKind::Dialog { title } => {
                println!("A dialog titled \"{}\".", title);
            }
            ZorkKind::BoxWidget { horizontal, spacing } => {
                println!("A {} box with spacing {}.", if *horizontal { "horizontal" } else { "vertical" }, spacing);
            }
            ZorkKind::Grid { cols, rows } => {
                println!("A grid with {} cols, {} rows.", cols, rows);
            }
            ZorkKind::MenuBar => {
                println!("A MenuBar.");
                let items = state.menu_items.get(&node.id).cloned().unwrap_or_default();
                if items.is_empty() {
                    println!("It has no items.");
                } else {
                    println!("It contains:");
                    for (i, item) in items.iter().enumerate() {
                        println!("  {}. {} ({})", i + 1, item.label, item.action);
                    }
                }
            }
            ZorkKind::Menu => {
                println!("A Menu.");
                let items = state.menu_items.get(&node.id).cloned().unwrap_or_default();
                if items.is_empty() {
                    println!("It has no items.");
                } else {
                    println!("It contains:");
                    for (i, item) in items.iter().enumerate() {
                        println!("  {}. {} ({})", i + 1, item.label, item.action);
                    }
                }
            }
            ZorkKind::Canvas { .. } => {
                let (w, h) = state.canvas_size(node.id);
                println!("A Canvas, {}x{}, with {} redraw request(s).", w, h, node.pointer.redraws);
                if node.draw.is_some() {
                    println!("It has a draw callback.");
                } else {
                    println!("It has no draw callback.");
                }
            }
            ZorkKind::Overlay => {
                println!("An Overlay with {} layer(s) over its base child.", node.overlays.len());
            }
            ZorkKind::ScrolledWindow => {
                println!("A ScrolledWindow, scrolled to ({}, {}).", node.props.hscroll, node.props.vscroll);
            }
            ZorkKind::Fixed => {
                println!("A Fixed container with {} child(ren).", node.children.len());
            }
            ZorkKind::Application => {
                println!("The Application.");
            }
            ZorkKind::Spreadsheet { .. } => {
                println!("A Spreadsheet with {} cell(s) set.", node.cells.len());
                let mut keys: Vec<&(u32, u32)> = node.cells.keys().collect();
                keys.sort();
                for k in keys {
                    if let Some(cell) = node.cells.get(k) {
                        println!("  R{}C{}: {}{}", k.0, k.1, cell.text, if cell.raw { " (raw)" } else { "" });
                    }
                }
            }
            _ => {
                println!("There's nothing special about this.");
            }
        }
        // Every node carries the same property bag, so show it for all of them.
        let summary = prop_summary(state, node);
        if summary != "no properties set" {
            println!();
            println!("Properties: {}", summary);
        }
    } else {
        println!("Nothing to examine.");
    }
}


#[cfg(test)]
mod menu_label_tests {
    use super::menu_label_plain;

    /// corro writes a menu mnemonic as an underscore anywhere in the label.
    /// The direction list names places for a human, so it has to come out the
    /// other side. Both positions occur in corro's real menu: `_File` is a
    /// leading one, `Fo_rmat` an interior one.
    #[test]
    fn accelerator_underscores_are_stripped() {
        assert_eq!(menu_label_plain("_File"), "File");
        assert_eq!(menu_label_plain("Fo_rmat"), "Format");
        assert_eq!(menu_label_plain("S_ave as"), "Save as");
        assert_eq!(menu_label_plain("E_xit"), "Exit");
        assert_eq!(menu_label_plain("Edi_t in Text Editor"), "Edit in Text Editor");
    }

    /// A label with no mnemonic must pass through unchanged, and a trailing
    /// underscore is not a mnemonic -- a label cannot end mid-mnemonic -- so
    /// it is kept rather than silently eating the last character.
    #[test]
    fn labels_without_mnemonics_are_untouched() {
        assert_eq!(menu_label_plain("Sheet"), "Sheet");
        assert_eq!(menu_label_plain("TSV"), "TSV");
        assert_eq!(menu_label_plain("ASCII table"), "ASCII table");
        assert_eq!(menu_label_plain(""), "");
        assert_eq!(menu_label_plain("Trailing_"), "Trailing_");
    }
}

/// A spreadsheet verb must answer when there is no spreadsheet to aim at.
///
/// corro's own build never makes a `Spreadsheet` node, and that is the model
/// this REPL starts with, so `grid` is typed into exactly this situation by
/// anyone following the walkthrough. It used to `return` on an unresolved
/// target without printing, so on corro's tree it produced no output at all --
/// silence that reads equally as an empty sheet, a hang, or a verb that does
/// not exist, while `sheet` on the same model explained itself.
#[cfg(test)]
mod no_spreadsheet_tests {
    use super::{resolve_arg_to_id, resolve_spreadsheet, NO_SPREADSHEET, NOT_A_SPREADSHEET};
    use crate::backends::zork::model::{ZorkKind, ZorkState};

    /// A model holding a window and nothing else -- no `Spreadsheet`, which is
    /// corro's own situation.
    fn bare_model() -> ZorkState {
        let mut s = ZorkState::new();
        s.add_node(ZorkKind::Window { title: "corro".into() }, None);
        s
    }

    /// The premise, asserted so a change to `bare_model` cannot quietly turn
    /// these tests into ones that pass for the wrong reason.
    fn assert_bare(s: &ZorkState) {
        assert!(
            !s.nodes
                .iter()
                .any(|n| matches!(n.kind, ZorkKind::Spreadsheet { .. })),
            "this model must hold no Spreadsheet: that is what these tests are about"
        );
    }

    /// With no spreadsheet, resolution fails *with the reason*.
    ///
    /// This is the assertion the bug would fail: `grid` reached this same
    /// function and discarded its answer, so the absence was reported
    /// nowhere. An untargeted `grid` and a target that resolves to nothing
    /// are both the model's absence; they are checked apart because they
    /// leave the function by different routes.
    #[test]
    fn a_model_without_a_spreadsheet_says_which_thing_is_missing() {
        for target in [None, Some("nope")] {
            let s = bare_model();
            assert_bare(&s);
            let mut notice = Vec::new();
            assert_eq!(
                resolve_spreadsheet(&s, target, &mut notice),
                Err(NO_SPREADSHEET),
                "grid {target:?} must be told the sheet is absent, not left silent"
            );
        }
    }

    /// A target that names a real node which is not a spreadsheet must be
    /// told *that*, not that the sheet is empty and not that there is none.
    ///
    /// `grid 1` on corro's own tree names the Window, which exists. The
    /// tempting wrong answer is "The sheet is empty.": every cell read on a
    /// non-spreadsheet returns `None`, so asking first and reporting the
    /// emptiness second blames a sheet that was never built. The other wrong
    /// answer is `NO_SPREADSHEET`, which blames the model for a target the
    /// reader chose. Both are ruled out here, and the premise is asserted so
    /// the test cannot pass because the target stopped resolving.
    #[test]
    fn a_target_that_is_not_a_spreadsheet_is_named_as_such() {
        let mut s = bare_model();
        // A child, so the numbered target `1` names something real.
        s.add_node(ZorkKind::Label { text: "not a sheet".into() }, Some(s.current_id));
        let mut notice = Vec::new();
        let resolved = resolve_arg_to_id(&s, "1");
        assert!(resolved.is_some(), "premise: `1` must name the label");
        assert_eq!(
            resolve_spreadsheet(&s, Some("1"), &mut notice),
            Err(NOT_A_SPREADSHEET),
            "a target that exists but is the wrong kind has its own answer"
        );
        assert_ne!(
            NOT_A_SPREADSHEET, NO_SPREADSHEET,
            "the two failures are distinguishable, or the reader is guessing"
        );
    }

    /// The same model with a spreadsheet in it must still resolve, so the
    /// failure above is about the absence and not about a resolver that
    /// never works. The spreadsheet is parented to the window so the numbered
    /// target `1` names it, which is the route `grid 1` takes.
    #[test]
    fn a_model_with_a_spreadsheet_still_resolves() {
        let mut s = bare_model();
        let window = s.current_id;
        let id = s.add_node(
            ZorkKind::Spreadsheet { border_title: String::new() },
            Some(window),
        );
        let mut notice = Vec::new();
        assert_eq!(resolve_spreadsheet(&s, None, &mut notice), Ok(id));
        assert_eq!(resolve_spreadsheet(&s, Some("1"), &mut notice), Ok(id));
    }

    /// The wording is pinned because `docs/ZORK_WALKTHROUGH.txt` quotes this
    /// sentence verbatim, as the answer `grid` gives on corro's own tree. If
    /// the message is reworded, that transcript is stale and this test is
    /// what says so.
    #[test]
    fn the_message_is_the_one_the_walkthrough_quotes() {
        assert_eq!(NO_SPREADSHEET, "There is no Spreadsheet in this model.");
    }
}
