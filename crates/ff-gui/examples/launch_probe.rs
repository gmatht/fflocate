//! Run a row-menu action against this host's real programs.
//!
//! The row menu calls `ff_gui::run_row_action`; this does exactly the same
//! thing from a one-shot process, so the launch can be observed from outside
//! (window list, process tree) without clicking in the GUI. It prints the
//! status line the app would put in its status bar, and the command that was
//! resolved for this host.
//!
//! ```text
//! cargo run -p ff-gui --example launch_probe -- folder   /path/to/file
//! cargo run -p ff-gui --example launch_probe -- terminal /path/to/file
//! ```

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = match args.first().map(String::as_str) {
        Some("folder") => ff_gui::RowAction::OpenContainingFolder,
        Some("terminal") => ff_gui::RowAction::OpenInTerminal,
        _ => {
            eprintln!("usage: launch_probe <folder|terminal> <path>");
            std::process::exit(2);
        }
    };
    let path = match args.get(1) {
        Some(p) => p.clone(),
        None => {
            eprintln!("usage: launch_probe <folder|terminal> <path>");
            std::process::exit(2);
        }
    };
    let (program, argv) = ff_gui::launch_command(action, &path);
    println!("probe: {} resolved to {program} {argv:?}", action.label());
    // The clipboard is not involved in either action; no widget is needed.
    let status = ff_gui::run_row_action(action, &path, |_| Ok(()));
    println!("probe: status line {status:?}");
}
