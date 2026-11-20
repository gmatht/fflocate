//! The application icon.
//!
//! The art is the fflocate logo with the **wordmark cropped away**: at 16–32 px
//! a wordmark is unreadable, and the taskbar already carries the window title,
//! so only the mark — the magnifier with the `ff` — is kept. `assets/fflocate.png`
//! is that mark at 1024², on a transparent background with even margins, and is
//! embedded on macOS only; `assets/hicolor/` holds the sizes GTK installs.
//!
//! GTK resolves a window icon through the *icon theme* on GTK4 (GTK3 can also
//! take a file), so [`prepare`] installs the hicolor tree into the per-user icon
//! directory before the toolkit starts — plus, for an installed binary only, the
//! desktop entry a launcher or app grid actually reads. macOS has no window
//! icon: the application owns one, so its `set_icon` takes the PNG bytes and
//! sets the Dock image. No icon is ever an error: a read-only data directory
//! costs the icon, not the GUI.

#[cfg(all(unix, not(target_os = "macos")))]
use std::path::{Path, PathBuf};

/// Icon-theme name, and the file name each installed size is written under.
pub const ICON_NAME: &str = "fflocate";

/// The mark, cropped from the logo and centred on a transparent 1024² canvas:
/// what the macOS Dock draws. GTK takes the installed `HICOLOR` sizes instead,
/// so this is compiled in on macOS only rather than sitting unused in Linux
/// binaries.
#[cfg(target_os = "macos")]
pub const ICON_PNG: &[u8] = include_bytes!("../assets/fflocate.png");

/// The size GTK3 is handed by path: the largest installed.
#[cfg(all(unix, not(target_os = "macos")))]
const LARGE_SIZE: &str = "512x512";

/// The installed sizes, for the freedesktop layout GTK expects
/// (`<size>x<size>/apps`); GTK picks the nearest to the size it needs.
#[cfg(all(unix, not(target_os = "macos")))]
const HICOLOR: &[(&str, &[u8])] = &[
    ("16x16", include_bytes!("../assets/hicolor/16x16/apps/fflocate.png")),
    ("24x24", include_bytes!("../assets/hicolor/24x24/apps/fflocate.png")),
    ("32x32", include_bytes!("../assets/hicolor/32x32/apps/fflocate.png")),
    ("48x48", include_bytes!("../assets/hicolor/48x48/apps/fflocate.png")),
    ("64x64", include_bytes!("../assets/hicolor/64x64/apps/fflocate.png")),
    ("128x128", include_bytes!("../assets/hicolor/128x128/apps/fflocate.png")),
    ("256x256", include_bytes!("../assets/hicolor/256x256/apps/fflocate.png")),
    ("512x512", include_bytes!("../assets/hicolor/512x512/apps/fflocate.png")),
];

/// Install the icon files and return the largest installed PNG's path (empty
/// when there is none), which GTK3 takes.
///
/// Call this **before** the toolkit is initialised: GTK reads the icon theme
/// while starting up, so an icon written afterwards is not found by name. Safe
/// to call more than once; unchanged files are not rewritten.
///
/// Linux only: the other backends take the embedded PNG (macOS) or have no
/// icon at all.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn prepare() -> String {
    install()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The per-user icon tree, falling back to a temp tree, then to nothing.
#[cfg(all(unix, not(target_os = "macos")))]
fn install() -> Option<PathBuf> {
    if let Some(home) = data_home() {
        let hicolor = home.join("icons").join("hicolor");
        if install_into(&hicolor).is_ok() {
            install_desktop_entry(&home);
            return Some(png_path(&hicolor, LARGE_SIZE));
        }
    }
    // A read-only data directory still leaves the *window* icon possible (GTK3
    // takes a path), even though no launcher will find it there.
    let tmp = std::env::temp_dir().join("fflocate-icons").join("hicolor");
    install_into(&tmp).ok()?;
    Some(png_path(&tmp, LARGE_SIZE))
}

/// One icon tree. All-or-nothing, so a full disk cannot leave a half-written
/// theme behind for GTK to read.
#[cfg(all(unix, not(target_os = "macos")))]
fn install_into(base: &Path) -> std::io::Result<()> {
    for (size, png) in HICOLOR {
        write_if_changed(&png_path(base, size), png)?;
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn png_path(base: &Path, size: &str) -> PathBuf {
    base.join(size).join("apps").join(format!("{ICON_NAME}.png"))
}

/// Write `bytes` to `path` unless that exact content is already there.
#[cfg(all(unix, not(target_os = "macos")))]
fn write_if_changed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() == bytes.len() as u64 && std::fs::read(path).is_ok_and(|old| old == bytes) {
            return Ok(());
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)
}

/// `$XDG_DATA_HOME`, else `~/.local/share` — where a user's icons live.
#[cfg(all(unix, not(target_os = "macos")))]
fn data_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|home| PathBuf::from(home).join(".local").join("share"))
}

/// Write `applications/fflocate.desktop`, unless this is a cargo build
/// artifact: a launcher pointing into `target/` would outlive the build, break
/// on `cargo clean`, and name whichever checkout happened to run last.
#[cfg(all(unix, not(target_os = "macos")))]
fn install_desktop_entry(data_home: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if is_cargo_artifact(&exe) {
        return;
    }
    let Some(exe) = exe.to_str() else {
        return;
    };
    let path = data_home.join("applications").join("fflocate.desktop");
    // Best-effort: a launcher entry is a convenience, and the window icon does
    // not depend on it.
    let _ = write_if_changed(&path, desktop_entry(exe).as_bytes());
}

/// True when this binary is a `cargo` build artifact rather than an installed
/// program.
#[cfg(all(unix, not(target_os = "macos")))]
fn is_cargo_artifact(exe: &Path) -> bool {
    // `cargo run` (and the test harness) export this for the child.
    std::env::var_os("CARGO_MANIFEST_DIR").is_some() || is_under_target(exe)
}

/// `<workspace>/target/<profile>/ff-gui`, or a cross-compiled
/// `target/<triple>/<profile>/…`: a `target` directory beside a `Cargo.toml`.
#[cfg(all(unix, not(target_os = "macos")))]
fn is_under_target(exe: &Path) -> bool {
    exe.ancestors().any(|dir| {
        dir.file_name().is_some_and(|name| name == "target")
            && dir
                .parent()
                .is_some_and(|workspace| workspace.join("Cargo.toml").is_file())
    })
}

/// The desktop entry that gives the icon a presence in a launcher/app grid.
///
/// `Icon=` names the hicolor icon installed beside it; `StartupWMClass=` is the
/// window's `WM_CLASS` (the binary name, `ff-gui`), which is how GNOME and
/// friends match a running window to this entry. Without the `WM_CLASS` line a
/// compositor shows the running window as a second, iconless application.
#[cfg(all(unix, not(target_os = "macos")))]
fn desktop_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name=fflocate\n\
         Comment=Instant filename search over the fflocate index\n\
         Exec={}\n\
         Icon={ICON_NAME}\n\
         Terminal=false\n\
         Categories=Utility;FileTools;\n\
         StartupWMClass=ff-gui\n",
        exec_quote(exec)
    )
}

/// `Exec` quoting, per the desktop-entry spec: bare when the path needs no
/// quoting, otherwise double-quoted with `"`, `` ` ``, `$` and `\` escaped.
#[cfg(all(unix, not(target_os = "macos")))]
fn exec_quote(path: &str) -> String {
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";
    if !path.chars().any(|c| RESERVED.contains(c)) {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for c in path.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded asset is what the Dock draws, so it must be a real PNG and
    /// not, say, a text placeholder.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_embedded_mark_is_a_png() {
        assert_eq!(&ICON_PNG[..8], b"\x89PNG\r\n\x1a\n");
        assert!(ICON_PNG.len() > 1024, "icon looks truncated: {} bytes", ICON_PNG.len());
    }

    /// Every size GTK is given is a real PNG too.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn every_hicolor_size_is_a_png() {
        for (size, png) in HICOLOR {
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{size} is not a PNG");
        }
    }

    /// Every hicolor size is installed, and a second run rewrites nothing.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn installs_every_hicolor_size_then_leaves_them_alone() {
        let dir = tempfile::tempdir().unwrap();
        install_into(dir.path()).unwrap();
        for (size, png) in HICOLOR {
            let path = png_path(dir.path(), size);
            assert_eq!(&std::fs::read(&path).unwrap(), png, "{size} differs");
        }
        // Idempotent: the second run finds identical bytes and succeeds.
        install_into(dir.path()).unwrap();
    }

    /// A file of the right size but the wrong bytes is replaced, so a botched
    /// earlier write cannot leave the wrong icon installed.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn replaces_a_different_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = png_path(dir.path(), "16x16");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not the icon").unwrap();
        install_into(dir.path()).unwrap();
        assert_eq!(&std::fs::read(&path).unwrap(), HICOLOR[0].1);
    }

    /// The entry names the icon and the window class, i.e. the two things a
    /// compositor needs to match a running window to it.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn the_desktop_entry_names_the_icon_and_the_window_class() {
        let entry = desktop_entry("/home/me/.cargo/bin/ff-gui");
        assert!(entry.starts_with("[Desktop Entry]\n"), "{entry}");
        assert!(entry.contains("\nIcon=fflocate\n"), "{entry}");
        assert!(entry.contains("\nStartupWMClass=ff-gui\n"), "{entry}");
        assert!(entry.contains("\nExec=/home/me/.cargo/bin/ff-gui\n"), "{entry}");
        assert!(entry.contains("\nType=Application\n"), "{entry}");
        assert!(entry.ends_with('\n'), "{entry}");
    }

    /// A path with spaces or shell metacharacters is quoted; a plain one is
    /// left alone, so the common case stays readable.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn exec_is_quoted_only_when_it_has_to_be() {
        assert_eq!(exec_quote("/usr/local/bin/ff-gui"), "/usr/local/bin/ff-gui");
        assert_eq!(exec_quote("/tmp/my build/ff-gui"), "\"/tmp/my build/ff-gui\"");
        assert_eq!(exec_quote("/tmp/a$b/ff-gui"), "\"/tmp/a\\$b/ff-gui\"");
    }

    /// `<workspace>/target/debug/ff-gui` is a build artifact; a binary under
    /// `~/.cargo/bin` is not.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_target_directory_beside_a_cargo_toml_is_a_build_artifact() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        assert!(is_under_target(&dir.path().join("target/debug/ff-gui")));
        // Cross-compiled: target/<triple>/<profile>/ff-gui.
        assert!(is_under_target(
            &dir.path().join("target/aarch64-apple-darwin/debug/ff-gui")
        ));
        // An installed binary, and a `target` dir with no workspace beside it.
        assert!(!is_under_target(&dir.path().join("bin/ff-gui")));
        let other = tempfile::tempdir().unwrap();
        assert!(!is_under_target(&other.path().join("target/debug/ff-gui")));
    }
}
