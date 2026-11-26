//! `ffbusy` — one binary, three front ends, busybox style.
//!
//! Dispatch is by `argv[0]` basename, the convention `ff` already uses for its
//! `locate`/`find`/`ffind` aliases:
//!
//! ```text
//! ff        the CLI
//! fflocated the daemon
//! ff-gui    the window
//! ```
//!
//! `ffbusy --install DIR` writes those names into `DIR` as symlinks to this
//! executable, and nothing else needs wiring: the CLI's auto-spawn looks for
//! `fflocated` beside `argv[0]` and re-executes it, which is the symlink; the
//! GUI launch does the same for `ff-gui`. Running `ffbusy` under any other
//! name is a usage error rather than a silent guess.
//!
//! ## Keeping widget code off the daemon's hot path
//!
//! The three halves share one image, so the linker would otherwise interleave
//! the GUI's widget code with the daemon's. `scripts/build-ffbusy.sh` links
//! with `sections.ld`, which collects `ff_gui`/`rswidgets` text into a
//! `.text.gui` section placed after `.text`. The CLI gets the same treatment
//! in `.text.cli`, placed after the widget section. The daemon's executed
//! pages then stay contiguous at the front of the image, and demand paging
//! means those pages are faulted in only when that front end actually runs.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Names that turn this executable into a front end, including the CLI's
/// existing `locate`/`find` aliases.
const LINKS: [&str; 6] = ["ff", "fflocated", "ff-gui", "locate", "find", "ffind"];

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();

    if argv.get(1).map(|a| a == "--install").unwrap_or(false) {
        return install(argv.get(2).map(PathBuf::from));
    }

    let name = argv.first().map(|a| base_name(a)).unwrap_or_default();
    match name.as_str() {
        "fflocated" | "ffd" => fflocated::entry::main_entry(),
        "ff-gui" | "ffgui" => gui_entry(),
        "ff" | "locate" | "find" | "ffind" => ff_cli::main_entry(),
        // Bare `ffbusy` (or `cargo run -p ffbusy`) is the CLI: it is the only
        // front end that makes sense without a display or a socket.
        "ffbusy" => ff_cli::main_entry(),
        other => {
            eprintln!(
                "ffbusy: no front end for `{other}`: symlink this binary as \
                 ff, fflocated or ff-gui (`ffbusy --install DIR`)"
            );
            ExitCode::from(2)
        }
    }
}

#[cfg(feature = "gui")]
fn gui_entry() -> ExitCode {
    ff_gui::ui::main_entry()
}

#[cfg(not(feature = "gui"))]
fn gui_entry() -> ExitCode {
    eprintln!("ffbusy: built without the `gui` feature");
    ExitCode::from(2)
}

/// `ffbusy --install DIR`: create the symlinks that name the front ends.
fn install(dir: Option<PathBuf>) -> ExitCode {
    let Some(dir) = dir else {
        eprintln!("usage: ffbusy --install DIR");
        return ExitCode::from(2);
    };
    match install_into(&dir) {
        Ok(()) => {
            for name in LINKS {
                println!("{}", dir.join(name).display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ffbusy: install into {}: {e}", dir.display());
            ExitCode::from(2)
        }
    }
}

#[cfg(unix)]
fn install_into(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::symlink;

    let exe = std::env::current_exe()?;
    std::fs::create_dir_all(dir)?;
    for name in LINKS {
        let link = dir.join(name);
        // Replace one of our own symlinks; never clobber a real file.
        match std::fs::symlink_metadata(&link) {
            Ok(m) if m.file_type().is_symlink() => std::fs::remove_file(&link)?,
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a symlink", link.display()),
                ))
            }
            Err(_) => {}
        }
        symlink(&exe, &link)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn install_into(_dir: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "`--install` creates symlinks, which this platform does not have",
    ))
}

/// The last component of `argv[0]`, which is what names a multi-call binary.
fn base_name(arg: &str) -> String {
    Path::new(arg)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
