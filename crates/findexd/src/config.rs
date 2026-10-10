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
    /// Exclusion rules (§4/§6): directory names whose subtrees are never
    /// indexed. Empty by default so nothing is filtered unless asked for.
    pub excludes: findex::excludes::Excludes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendChoice {
    Inotify,
    /// fanotify-FID: whole-mount watch, file-handle events (§6, v1.5).
    Fanotify,
    Fake,
    /// Pick the best available backend for this system (§4): fanotify when the
    /// kernel supports it, else inotify on Linux, else nothing (caller errors).
    Auto,
}

impl BackendChoice {
    pub fn as_str(&self) -> &'static str {
        match self {
            BackendChoice::Inotify => "inotify",
            BackendChoice::Fanotify => "fanotify",
            BackendChoice::Fake => "fake",
            BackendChoice::Auto => "auto",
        }
    }

    /// Parse a `--backend` / `$FINDEX_BACKEND` value.
    pub fn parse(s: &str) -> Option<BackendChoice> {
        Some(match s {
            "inotify" => BackendChoice::Inotify,
            "fanotify" => BackendChoice::Fanotify,
            "fake" => BackendChoice::Fake,
            "auto" => BackendChoice::Auto,
            _ => return None,
        })
    }

    /// Resolve `Auto` against what this system actually supports. Returns the
    /// concrete choice, or `None` when nothing usable exists.
    pub fn resolve(self) -> Option<BackendChoice> {
        match self {
            BackendChoice::Auto => {
                #[cfg(target_os = "linux")]
                {
                    if findex::mount::fanotify_available() {
                        Some(BackendChoice::Fanotify)
                    } else {
                        Some(BackendChoice::Inotify)
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    // No kernel watcher is ported off Linux yet (FSEvents is
                    // v1.5); `fake` is the only honest option.
                    Some(BackendChoice::Fake)
                }
            }
            other => Some(other),
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
    /// Default configuration: index `$HOME`, socket under the runtime dir.
    ///
    /// `$FINDEX_ROOT` overrides the root list; entries are separated by the
    /// platform path separator (`:` on Unix). This lets `ff` forward the roots
    /// it was asked to use when it auto-spawns a daemon, instead of the daemon
    /// silently falling back to indexing the whole home directory.
    pub fn defaults() -> Config {
        let rd = runtime_dir();
        let state = state_dir();
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"));
        let roots = match std::env::var_os("FINDEX_ROOT") {
            Some(v) if !v.is_empty() => std::env::split_paths(&v).collect(),
            _ => vec![home],
        };
        // §4/§6: exclusions are opt-in. `FINDEX_EXCLUDE` takes comma/colon
        // separated names (`build,node_modules`), absolute prefixes (`/proc`)
        // or suffixes (`*.pyc`).
        let excludes = match std::env::var("FINDEX_EXCLUDE") {
            Ok(v) if !v.trim().is_empty() => findex::excludes::Excludes::parse_list(&v),
            _ => findex::excludes::Excludes::new(),
        };
        Config {
            socket_path: rd.join("findex.sock"),
            pidfile: rd.join("findex.pid"),
            state_dir: state,
            roots,
            backend: BackendChoice::Inotify,
            compaction_threshold: 100_000,
            sync_timeout_ms: 2_000,
            excludes,
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
        self.pidfile = p.with_extension("pid");
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

    #[test]
    fn backend_choice_parses_and_round_trips() {
        for name in ["inotify", "fanotify", "fake", "auto"] {
            let c = BackendChoice::parse(name).unwrap_or_else(|| panic!("{name} must parse"));
            assert_eq!(c.as_str(), name);
        }
        assert!(BackendChoice::parse("nope").is_none());
        assert!(BackendChoice::parse("Inotify").is_none(), "case-sensitive, like the CLI flag");
    }

    #[test]
    fn auto_resolves_to_a_concrete_backend() {
        // Auto must never stay Auto past resolution: the daemon needs a
        // concrete choice, and on a supported Linux box that is fanotify or
        // inotify, never neither.
        let r = BackendChoice::Auto.resolve();
        assert!(r.is_some(), "this platform must resolve Auto to something");
        assert_ne!(r, Some(BackendChoice::Auto));
    }

    #[test]
    fn explicit_choices_resolve_to_themselves() {
        // Asking for a backend must not silently become a different one; if it
        // is unavailable, `make_backend` errors instead.
        for c in [BackendChoice::Inotify, BackendChoice::Fanotify, BackendChoice::Fake] {
            assert_eq!(c.resolve(), Some(c));
        }
    }
}
