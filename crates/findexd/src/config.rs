//! Daemon configuration (§4, §15).

use std::path::{Path, PathBuf};

/// Runtime paths, derived from the environment with sane defaults.
#[derive(Clone, Debug)]
pub struct Config {
    /// `$XDG_RUNTIME_DIR/findex.sock`, or `/run/findex.sock` for a system
    /// instance. Never `/tmp` (stale-socket class of bugs, §15).
    pub socket_path: PathBuf,
    /// `$XDG_STATE_HOME/findex/<format-version>/`.
    pub state_dir: PathBuf,
    /// Roots to index.
    pub roots: Vec<PathBuf>,
    /// Watch backend to use.
    pub backend: BackendChoice,
    /// Journal records between compactions.
    pub compaction_threshold: u64,
    /// Sync default timeout (ms).
    pub sync_timeout_ms: u64,
    /// Directory for the pidfile + lock (§15 stale-lock recovery).
    pub pidfile: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendChoice {
    Inotify,
    Fake,
}

impl BackendChoice {
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendChoice::Inotify => "inotify",
            BackendChoice::Fake => "fake",
        }
    }
}

/// The on-disk format version. Bumping it migrates or recrawls instead of
/// corrupting (§15).
pub const FORMAT_VERSION: u32 = findex::config_paths::FORMAT_VERSION;

pub fn runtime_dir() -> PathBuf {
    findex::config_paths::runtime_dir()
}

pub fn state_dir() -> PathBuf {
    findex::config_paths::state_dir()
}

impl Config {
    /// Default configuration: index the home dir, socket under the runtime dir.
    pub fn defaults() -> Config {
        let rd = runtime_dir();
        let state = state_dir();
        #[cfg(windows)]
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("C:/"));
        #[cfg(not(windows))]
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"));
        Config {
            socket_path: findex::config_paths::socket_path(),
            pidfile: rd.join("findex.pid"),
            state_dir: state,
            roots: vec![home],
            // No kernel watcher ported on Windows yet (ReadDirectoryChangesW
            // is future work): default to the fake backend — crawl on start
            // plus explicit rescans — rather than refusing to start.
            #[cfg(windows)]
            backend: BackendChoice::Fake,
            #[cfg(not(windows))]
            backend: BackendChoice::Inotify,
            compaction_threshold: 100_000,
            sync_timeout_ms: 2_000,
        }
    }

    pub fn with_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.roots = roots;
        self
    }
    pub fn with_state_dir(mut self, dir: PathBuf) -> Self {
        self.state_dir = dir;
        self
    }
    pub fn with_socket(mut self, p: PathBuf) -> Self {
        // On Windows the "socket path" is `127.0.0.1:PORT` text, which has no
        // usable parent/extension — keep the pidfile in the runtime dir.
        #[cfg(windows)]
        {
            self.pidfile = runtime_dir().join("findex.pid");
        }
        #[cfg(not(windows))]
        {
            self.pidfile = p.with_extension("pid");
        }
        self.socket_path = p;
        self
    }
    pub fn with_backend(mut self, b: BackendChoice) -> Self {
        self.backend = b;
        self
    }
}

/// The socket path a client should try, honouring an override env var so the
/// test harness can run an isolated daemon.
pub fn socket_path_from_env() -> PathBuf {
    findex::config_paths::socket_path_from_env()
}

pub fn state_dir_from_env() -> PathBuf {
    findex::config_paths::state_dir_from_env()
}

pub fn ensure_parent(p: &Path) -> std::io::Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_is_versioned() {
        let d = state_dir();
        assert!(
            d.to_string_lossy().contains(&FORMAT_VERSION.to_string()),
            "state dir must carry the format version: {d:?}"
        );
    }

    #[test]
    fn defaults_never_tmp() {
        let c = Config::defaults();
        assert!(!c.socket_path.starts_with("/tmp"), "socket must not live in /tmp");
    }

    #[test]
    fn env_override_respected() {
        std::env::set_var("FINDEX_SOCKET", "/run/user/1/x.sock");
        assert_eq!(socket_path_from_env(), PathBuf::from("/run/user/1/x.sock"));
        std::env::remove_var("FINDEX_SOCKET");
    }
}
