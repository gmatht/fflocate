//! The application icon.
//!
//! The art is the fflocate logo with the **wordmark cropped away**: at 16–32 px
//! a wordmark is unreadable, and the taskbar already carries the window title,
//! so only the mark — the magnifier with the `ff` — is kept. `assets/fflocate.png`
//! is that mark at 1024², on a transparent background with even margins;
//! `assets/hicolor/` holds the sizes GTK looks up by name.
//!
//! Two things have to happen for the icon to actually show:
//!
//! * **GTK** resolves the icon through the *icon theme* on GTK4 (GTK3 can also
//!   take a file), so the hicolor tree is installed into the per-user icon
//!   directory — see [`prepare`]. That must happen before the toolkit starts,
//!   because GTK reads the theme during initialisation; hence the separate call
//!   from [`apply`], which runs once there is a window.
//! * **macOS** has no window icon at all: the application owns one, so the Dock
//!   image is set from the PNG bytes.
//!
//! A backend with neither (the `zork` terminal fallback, Windows, a browser
//! tab) accepts the call and does nothing. No icon is ever an error: a
//! read-only data directory costs the icon, not the GUI.

use std::path::PathBuf;
#[cfg(all(unix, not(target_os = "macos")))]
use std::path::Path;
use std::sync::OnceLock;

/// Icon-theme name, and the file name each installed size is written under.
pub const ICON_NAME: &str = "fflocate";

/// The mark, cropped from the logo and centred on a transparent 1024² canvas.
pub const ICON_PNG: &[u8] = include_bytes!("../assets/fflocate.png");

/// The hicolor sizes, ascending — the last entry is the file GTK3 is handed by
/// path, so it is the largest.
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

/// Where the installed 512 px PNG ended up, resolved once.
static ICON_FILE: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Install the icon-theme files and the Linux desktop entry, and return the
/// installed 512 px PNG's path (empty when there is none). Call this **before**
/// the toolkit is initialised: GTK creates its icon theme while starting up, so
/// an icon written afterwards would not be found by name.
///
/// A no-op off GTK's platforms, and safe to call more than once.
///
/// The result is then handed to the window with
/// `win.set_icon(&path, ICON_NAME, ICON_PNG)`: GTK takes the file (GTK3) or the
/// theme name (GTK4), macOS takes the PNG bytes and sets the Dock image. A
/// plain function rather than a wrapper here because `App::create_window`
/// returns each backend's own window type.
pub fn prepare() -> String {
    icon_file()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn icon_file() -> Option<PathBuf> {
    ICON_FILE.get_or_init(install).clone()
}

/// Write the hicolor tree into the per-user icon directory and return the
/// 512 px file, for the backends that want a path.
///
/// Unchanged files are left alone (an ordinary launch costs a few `stat`s). If
/// the data directory is not writable it falls back to a temp tree, and then to
/// nothing: a window without an icon still works, so this never fails loudly.
///
/// The desktop entry is only written when the theme files reached the *real*
/// data directory: an entry pointing at `Icon=fflocate` with no hicolor icons
/// beside it would list the app with a blank icon instead of none.
#[cfg(all(unix, not(target_os = "macos")))]
fn install() -> Option<PathBuf> {
    if let Some(home) = data_home() {
        if let Ok(path) = install_into(&home.join("icons").join("hicolor")) {
            install_desktop_entry(&home);
            return Some(path);
        }
    }
    install_into(&std::env::temp_dir().join("fflocate-icons").join("hicolor")).ok()
}

/// Write the `.desktop` entry into the per-user applications directory.
/// Best-effort: a launcher entry is a convenience, and the window icon does not
/// depend on it, so a failure here is silent.
#[cfg(all(unix, not(target_os = "macos")))]
fn install_desktop_entry(data_home: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(exe) = exe.to_str() else {
        return;
    };
    let path = data_home.join("applications").join("fflocate.desktop");
    let _ = write_if_changed(&path, desktop_entry(exe).as_bytes());
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
         GenericName=File Search\n\
         Comment=Instant filename search over the fflocate index\n\
         Exec={}\n\
         Icon={ICON_NAME}\n\
         Terminal=false\n\
         Categories=Utility;FileTools;\n\
         Keywords=find;locate;search;file;filename;\n\
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

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn install() -> Option<PathBuf> {
    None
}

/// One icon tree. All-or-nothing, so a full disk cannot leave a half-written
/// theme behind for GTK to read. Returns the last (largest) file written.
#[cfg(all(unix, not(target_os = "macos")))]
fn install_into(base: &Path) -> std::io::Result<PathBuf> {
    let mut written = None;
    for (size, png) in HICOLOR {
        let path = base.join(size).join("apps").join(format!("{ICON_NAME}.png"));
        write_if_changed(&path, png)?;
        written = Some(path);
    }
    Ok(written.unwrap_or_default())
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

/// `$XDG_DATA_HOME`, else `~/.local/share` — where a user's icon theme lives.
#[cfg(all(unix, not(target_os = "macos")))]
fn data_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|home| PathBuf::from(home).join(".local").join("share"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded asset is what the window gets on macOS, so it must be a
    /// real PNG and not, say, a text placeholder.
    #[test]
    fn the_embedded_mark_is_a_png() {
        assert_eq!(&ICON_PNG[..8], b"\x89PNG\r\n\x1a\n");
        assert!(ICON_PNG.len() > 1024, "icon looks truncated: {} bytes", ICON_PNG.len());
    }

    /// Every hicolor size is installed, and a second run rewrites nothing.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn installs_every_hicolor_size_then_leaves_them_alone() {
        let dir = tempfile::tempdir().unwrap();
        let large = install_into(dir.path()).unwrap();
        assert!(large.ends_with("512x512/apps/fflocate.png"), "{large:?}");
        for (size, png) in HICOLOR {
            let path = dir.path().join(size).join("apps").join("fflocate.png");
            assert_eq!(&std::fs::read(&path).unwrap(), png, "{size} differs");
        }
        // Idempotent: the second run finds identical bytes and succeeds.
        assert_eq!(install_into(dir.path()).unwrap(), large);
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
        // `Icon=` and the installed theme files must agree.
        assert!(entry.contains(&format!("\nIcon={ICON_NAME}\n")));
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

    /// A file of the right size but the wrong bytes is replaced, so a botched
    /// earlier write cannot leave the wrong icon installed.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn replaces_a_different_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("16x16").join("apps").join("fflocate.png");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not the icon").unwrap();
        install_into(dir.path()).unwrap();
        assert_eq!(&std::fs::read(&path).unwrap(), HICOLOR[0].1);
    }
}
