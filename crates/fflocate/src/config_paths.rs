//! Client-side path discovery (§15).
//!
//! The socket and state-dir locations are part of the daemon/client contract,
//! so they live in the core library. Both honour the `FFLOCATE_SOCKET` /
//! `FFLOCATE_STATE_DIR` environment overrides (used by tests and by `ff` when it
//! auto-spawns a daemon).

use std::path::PathBuf;

/// The on-disk format version. Bumping it upgrades or recrawls the store's
/// *contents* in place instead of corrupting (§15).
///
/// 3 adds each entry's creation time (minutes since the epoch, `u32`) to the
/// snapshot and the journal's `add` record. 4 gives "unknown" its own sentinel
/// so minute 0 is a real time rather than a second meaning for "no birth
/// time". Older snapshots still decode (their sentinel is translated on read).
/// The version is recorded *inside* the store — the snapshot's version word and
/// the `store.meta` stamp — never in its directory name: [`STORE_DIR_NAME`] is
/// stable, so a bump opens the same directory and rewrites it.
pub const FORMAT_VERSION: u32 = 4;

/// The store directory used when the state root holds no store yet.
///
/// Frozen on purpose: it is **not** derived from [`FORMAT_VERSION`]. Deriving
/// the directory from the version would abandon an existing index on every
/// bump — and move where the daemon looks for it. `4` is the name the store has
/// had since creation times landed, so an existing install keeps the exact path
/// it already uses; [`resolve_state_dir`] still reuses a store that sits in an
/// older version-named directory.
pub const STORE_DIR_NAME: &str = "4";

pub fn runtime_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    #[cfg(windows)]
    {
        // No socket files on Windows; the runtime dir only holds the pidfile.
        if let Ok(d) = std::env::var("LOCALAPPDATA") {
            if !d.is_empty() {
                return PathBuf::from(d).join("fflocate");
            }
        }
        state_dir()
    }
    #[cfg(all(not(windows), target_os = "macos"))]
    {
        // macOS has no /run and no XDG_RUNTIME_DIR. The per-user socket lives
        // in Application Support, which is the platform's per-user state root
        // and is not world-writable (unlike $TMPDIR itself).
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h).join("Library/Application Support/fflocate");
        }
        // Last resort: the per-user temp dir ($TMPDIR on macOS is private to
        // the user, unlike /tmp).
        std::env::temp_dir().join("fflocate")
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        if let Ok(uid) = std::env::var("UID") {
            return PathBuf::from(format!("/run/user/{uid}"));
        }
        PathBuf::from("/run")
    }
}

/// The state root: the directory stores have always lived under. Its name never
/// changes with the format, and it is deliberately *not* versioned.
fn state_root() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_STATE_HOME") {
        if !d.is_empty() {
            return PathBuf::from(d).join("fflocate");
        }
    }
    #[cfg(windows)]
    {
        for var in ["LOCALAPPDATA", "APPDATA"] {
            if let Ok(d) = std::env::var(var) {
                if !d.is_empty() {
                    return PathBuf::from(d).join("fflocate");
                }
            }
        }
    }
    // macOS: the index is a cache — Caches is the platform-correct home for it
    // (never backed up, purgeable).
    #[cfg(target_os = "macos")]
    {
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h).join("Library/Caches/fflocate");
        }
        return std::env::temp_dir().join("fflocate");
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h).join(".local/state/fflocate");
        }
        #[cfg(windows)]
        {
            // Last resort: temp dir (Git-Bash/MSYS set HOME, so this is rare).
            std::env::temp_dir().join("fflocate")
        }
        #[cfg(not(windows))]
        {
            PathBuf::from("/var/lib/fflocate")
        }
    }
}

/// Resolve the store directory under `root`, reusing an existing store **in
/// place**.
///
/// Any child directory that already holds a store wins over creating a fresh
/// one, preferring the highest version-named store this build can read (a
/// newer sibling left behind by a downgrade must not shadow a readable one).
/// Nothing found means the stable [`STORE_DIR_NAME`]. The result therefore
/// never moves just because [`FORMAT_VERSION`] changed: the same directory is
/// opened and rewritten in the current layout.
pub fn resolve_state_dir(root: &std::path::Path) -> PathBuf {
    let mut readable: Option<(u64, PathBuf)> = None;
    let mut any: Option<(u64, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(root) {
        for ent in entries.flatten() {
            let path = ent.path();
            if !path.is_dir() || !crate::persist::Store::holds_store(&path) {
                continue;
            }
            let version = ent
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0);
            if any.as_ref().map_or(true, |(v, _)| version > *v) {
                any = Some((version, path.clone()));
            }
            if version <= u64::from(FORMAT_VERSION)
                && readable.as_ref().map_or(true, |(v, _)| version > *v)
            {
                readable = Some((version, path));
            }
        }
    }
    readable
        .or(any)
        .map(|(_, p)| p)
        .unwrap_or_else(|| root.join(STORE_DIR_NAME))
}

pub fn state_dir() -> PathBuf {
    resolve_state_dir(&state_root())
}

/// Default TCP port for the loopback IPC endpoint on Windows.
#[cfg(windows)]
pub const DEFAULT_PORT: u16 = 18923;

pub fn socket_path() -> PathBuf {
    #[cfg(windows)]
    {
        // No unix sockets on Windows: the "socket path" carries
        // `127.0.0.1:PORT` text (see `transport`). `FFLOCATE_PORT` overrides.
        let port: u16 = std::env::var("FFLOCATE_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_PORT);
        PathBuf::from(format!("127.0.0.1:{port}"))
    }
    #[cfg(not(windows))]
    {
        runtime_dir().join("fflocate.sock")
    }
}

/// Socket path, honouring `FFLOCATE_SOCKET`.
pub fn socket_path_from_env() -> PathBuf {
    if let Ok(p) = std::env::var("FFLOCATE_SOCKET") {
        return PathBuf::from(p);
    }
    socket_path()
}

/// State dir, honouring `FFLOCATE_STATE_DIR`.
pub fn state_dir_from_env() -> PathBuf {
    if let Ok(p) = std::env::var("FFLOCATE_STATE_DIR") {
        return PathBuf::from(p);
    }
    state_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_paths_live_under_library() {
        // The per-user socket and store follow macOS conventions rather than
        // XDG paths that no macOS tool reads.
        std::env::remove_var("XDG_RUNTIME_DIR");
        std::env::remove_var("XDG_STATE_HOME");
        let s = socket_path();
        assert!(
            s.to_string_lossy()
                .contains("Library/Application Support/fflocate"),
            "macOS socket must live in Application Support: {s:?}"
        );
        let d = state_dir();
        assert!(
            d.to_string_lossy().contains("Library/Caches/fflocate"),
            "macOS store must live in Caches: {d:?}"
        );
    }

    #[test]
    fn socket_is_not_in_tmp() {
        let p = socket_path();
        #[cfg(not(windows))]
        {
            assert!(
                !p.starts_with("/tmp"),
                "socket must not live in /tmp: {p:?}"
            );
            assert!(p.ends_with("fflocate.sock"));
        }
        // On Windows the "socket path" is loopback TCP text, never a file.
        #[cfg(windows)]
        {
            let addr: std::net::SocketAddr = p.to_str().expect("utf-8").parse().expect("addr text");
            assert!(addr.ip().is_loopback(), "loopback only: {addr}");
        }
    }

    #[test]
    fn resolve_state_dir_reuses_an_existing_store_in_place() {
        let root = tempfile::tempdir().unwrap();
        // Nothing to reuse: a new store gets the stable name, which does not
        // move with the format version.
        assert_eq!(
            resolve_state_dir(root.path()),
            root.path().join(STORE_DIR_NAME)
        );

        // An existing store is opened where it already is, even when its
        // directory is named for an older format version — that is what keeps
        // an upgrade from abandoning the index (or moving the daemon).
        let old = root.path().join("3");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("snapshot.fnx"), b"v3").unwrap();
        assert_eq!(resolve_state_dir(root.path()), old, "in place, not a sibling");

        // A directory that merely shares a version name holds no store and is
        // never mistaken for one.
        let empty = root.path().join("2");
        std::fs::create_dir(&empty).unwrap();
        assert_eq!(resolve_state_dir(root.path()), old);

        // A store from a *newer* format cannot shadow the readable one (a
        // downgrade must reach the store it can actually read).
        let newer = root.path().join((FORMAT_VERSION + 1).to_string());
        std::fs::create_dir(&newer).unwrap();
        std::fs::write(newer.join("journal.log"), b"").unwrap();
        assert_eq!(resolve_state_dir(root.path()), old);
    }

    #[test]
    fn overrides_win() {
        std::env::set_var("FFLOCATE_SOCKET", "/run/user/7/f.sock");
        assert_eq!(socket_path_from_env(), PathBuf::from("/run/user/7/f.sock"));
        std::env::remove_var("FFLOCATE_SOCKET");
        std::env::set_var("FFLOCATE_STATE_DIR", "/var/state/x");
        assert_eq!(state_dir_from_env(), PathBuf::from("/var/state/x"));
        std::env::remove_var("FFLOCATE_STATE_DIR");
    }
}
