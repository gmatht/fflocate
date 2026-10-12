//! Client-side path discovery (§15).
//!
//! The socket and state-dir locations are part of the daemon/client contract,
//! so they live in the core library. Both honour the `FINDEX_SOCKET` /
//! `FINDEX_STATE_DIR` environment overrides (used by tests and by `ff` when it
//! auto-spawns a daemon).

use std::path::PathBuf;

/// The on-disk format version. Bumping it migrates or recrawls instead of
/// corrupting (§15).
pub const FORMAT_VERSION: u32 = 2;

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
                return PathBuf::from(d).join("findex");
            }
        }
        state_dir()
    }
    #[cfg(not(windows))]
    {
        if let Ok(uid) = std::env::var("UID") {
            return PathBuf::from(format!("/run/user/{uid}"));
        }
        PathBuf::from("/run")
    }
}

pub fn state_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_STATE_HOME") {
        if !d.is_empty() {
            return PathBuf::from(d)
                .join("findex")
                .join(FORMAT_VERSION.to_string());
        }
    }
    #[cfg(windows)]
    {
        for var in ["LOCALAPPDATA", "APPDATA"] {
            if let Ok(d) = std::env::var(var) {
                if !d.is_empty() {
                    return PathBuf::from(d)
                        .join("findex")
                        .join(FORMAT_VERSION.to_string());
                }
            }
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h)
            .join(".local/state/findex")
            .join(FORMAT_VERSION.to_string());
    }
    #[cfg(windows)]
    {
        // Last resort: temp dir (Git-Bash/MSYS set HOME, so this is rare).
        std::env::temp_dir()
            .join("findex")
            .join(FORMAT_VERSION.to_string())
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(format!("/var/lib/findex/{FORMAT_VERSION}"))
    }
}

/// Default TCP port for the loopback IPC endpoint on Windows.
#[cfg(windows)]
pub const DEFAULT_PORT: u16 = 18923;

pub fn socket_path() -> PathBuf {
    #[cfg(windows)]
    {
        // No unix sockets on Windows: the "socket path" carries
        // `127.0.0.1:PORT` text (see `transport`). `FINDEX_PORT` overrides.
        let port: u16 = std::env::var("FINDEX_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_PORT);
        PathBuf::from(format!("127.0.0.1:{port}"))
    }
    #[cfg(not(windows))]
    {
        runtime_dir().join("findex.sock")
    }
}

/// Socket path, honouring `FINDEX_SOCKET`.
pub fn socket_path_from_env() -> PathBuf {
    if let Ok(p) = std::env::var("FINDEX_SOCKET") {
        return PathBuf::from(p);
    }
    socket_path()
}

/// State dir, honouring `FINDEX_STATE_DIR`.
pub fn state_dir_from_env() -> PathBuf {
    if let Ok(p) = std::env::var("FINDEX_STATE_DIR") {
        return PathBuf::from(p);
    }
    state_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_is_not_in_tmp() {
        let p = socket_path();
        #[cfg(not(windows))]
        {
            assert!(
                !p.starts_with("/tmp"),
                "socket must not live in /tmp: {p:?}"
            );
            assert!(p.ends_with("findex.sock"));
        }
        // On Windows the "socket path" is loopback TCP text, never a file.
        #[cfg(windows)]
        {
            let addr: std::net::SocketAddr = p.to_str().expect("utf-8").parse().expect("addr text");
            assert!(addr.ip().is_loopback(), "loopback only: {addr}");
        }
    }

    #[test]
    fn state_dir_carries_format_version() {
        let d = state_dir();
        assert!(d.to_string_lossy().contains(&FORMAT_VERSION.to_string()));
    }

    #[test]
    fn overrides_win() {
        std::env::set_var("FINDEX_SOCKET", "/run/user/7/f.sock");
        assert_eq!(socket_path_from_env(), PathBuf::from("/run/user/7/f.sock"));
        std::env::remove_var("FINDEX_SOCKET");
        std::env::set_var("FINDEX_STATE_DIR", "/var/state/x");
        assert_eq!(state_dir_from_env(), PathBuf::from("/var/state/x"));
        std::env::remove_var("FINDEX_STATE_DIR");
    }
}
