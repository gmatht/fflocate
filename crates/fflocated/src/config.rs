//! Daemon configuration (§4, §15).

use std::path::{Path, PathBuf};

/// Runtime paths, derived from the environment with sane defaults.
#[derive(Clone, Debug)]
pub struct Config {
    /// `$XDG_RUNTIME_DIR/fflocate.sock`, or `/run/fflocate.sock` for a system
    /// instance. Never `/tmp` (stale-socket class of bugs, §15).
    pub socket_path: PathBuf,
    /// `$XDG_STATE_HOME/fflocate/<format-version>/`.
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
    pub excludes: fflocate::excludes::Excludes,
    /// Run as a **system** instance (§3): index owned/privileged paths and answer
    /// each query as the connecting user, filtering by per-entry ownership.
    ///
    /// Never the default: this mode runs as whatever user started it (in practice
    /// root) and must be asked for explicitly.
    pub system: bool,
    /// The group that owns the system socket (§15). Numeric gid; the daemon
    /// resolves `fflocate` from `/etc/group` when the flag is bare.
    pub system_group: u32,
    /// Opt into cross-OS store sharing for dual-boot machines (§15): open a
    /// store last written by another OS instead of refusing. Each OS keeps
    /// its own roots (matched by physical drive, not by path); foreign roots
    /// stay visible but are never queried, synced, rescanned or deleted here.
    ///
    /// Never the default: without it a foreign store is refused, loudly.
    pub shared_store: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendChoice {
    Inotify,
    /// fanotify-FID: whole-mount watch, file-handle events (§6, v1.5).
    Fanotify,
    /// FSEvents: the macOS whole-volume watcher (§6).
    Fsevents,
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
            BackendChoice::Fsevents => "fsevents",
            BackendChoice::Fake => "fake",
            BackendChoice::Auto => "auto",
        }
    }

    /// Parse a `--backend` / `$FFLOCATE_BACKEND` value.
    pub fn parse(s: &str) -> Option<BackendChoice> {
        Some(match s {
            "inotify" => BackendChoice::Inotify,
            "fanotify" => BackendChoice::Fanotify,
            "fsevents" | "fsevent" => BackendChoice::Fsevents,
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
                    if fflocate::mount::fanotify_available() {
                        Some(BackendChoice::Fanotify)
                    } else {
                        Some(BackendChoice::Inotify)
                    }
                }
                #[cfg(target_os = "macos")]
                {
                    Some(BackendChoice::Fsevents)
                }
                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                {
                    // No real watcher on this platform: `fake` keeps the
                    // daemon honest but static (the store still answers).
                    Some(BackendChoice::Fake)
                }
            }
            other => Some(other),
        }
    }
}

pub fn runtime_dir() -> PathBuf {
    fflocate::config_paths::runtime_dir()
}

pub fn state_dir() -> PathBuf {
    fflocate::config_paths::state_dir()
}

impl Config {
    /// Default configuration: `$FFLOCATE_ROOT` when set, else `$HOME`; socket
    /// under the runtime dir. Deliberately cheap (no mountinfo, no probes):
    /// the Linux filesystem discovery (`default_roots`) runs only when the
    /// daemon actually starts without roots (see `main`), never on every
    /// config construction.
    ///
    /// `$FFLOCATE_ROOT` overrides the root list; entries are separated by the
    /// platform path separator (`:` on Unix). This lets `ff` forward the roots
    /// it was asked to use when it auto-spawns a daemon, instead of the daemon
    /// silently falling back to indexing the whole home directory.
    pub fn defaults() -> Config {
        let rd = runtime_dir();
        let state = state_dir();
        let roots = match std::env::var_os("FFLOCATE_ROOT") {
            Some(v) if !v.is_empty() => std::env::split_paths(&v).collect(),
            _ => vec![home_dir()],
        };
        // §4/§6: exclusions are opt-in. `FFLOCATE_EXCLUDE` takes comma/colon
        // separated names (`build,node_modules`), absolute prefixes (`/proc`)
        // or suffixes (`*.pyc`).
        let excludes = match std::env::var("FFLOCATE_EXCLUDE") {
            Ok(v) if !v.trim().is_empty() => fflocate::excludes::Excludes::parse_list(&v),
            _ => fflocate::excludes::Excludes::new(),
        };
        Config {
            socket_path: fflocate::config_paths::socket_path(),
            pidfile: rd.join("fflocate.pid"),
            state_dir: state,
            roots,
            // No kernel watcher ported on Windows yet (ReadDirectoryChangesW
            // is future work): default to the fake backend — crawl on start
            // plus explicit rescans — rather than refusing to start.
            #[cfg(windows)]
            backend: BackendChoice::Fake,
            // inotify is a Linux API, and `new_backend` refuses it rather than
            // running unwatched; a daemon started by hand on macOS must
            // therefore default to the platform's own watcher, the same one
            // `ff` asks for when it auto-spawns (fflocate::spawn).
            #[cfg(target_os = "macos")]
            backend: BackendChoice::Fsevents,
            #[cfg(not(any(windows, target_os = "macos")))]
            backend: BackendChoice::Inotify,
            compaction_threshold: 100_000,
            sync_timeout_ms: 2_000,
            excludes,
            system: false,
            system_group: 0,
            shared_store: false,
        }
    }

    /// Switch to system mode: a socket under `/run`, state under `/var/lib`, and
    /// per-entry ownership tracking. The caller is responsible for actually
    /// having the privilege this implies.
    pub fn with_system(mut self, on: bool) -> Self {
        self.system = on;
        if on {
            #[cfg(target_os = "macos")]
            {
                // macOS has no /run and no /var/lib: the system socket lives in
                // /var/run (the platform's daemon runtime dir) and the store in
                // /Library/Application Support, which root owns.
                self.socket_path = PathBuf::from("/var/run/fflocated.sock");
                self.pidfile = PathBuf::from("/var/run/fflocated.pid");
                self.state_dir = fflocate::config_paths::resolve_state_dir(Path::new(
                    "/Library/Application Support/fflocate",
                ));
            }
            #[cfg(not(target_os = "macos"))]
            {
                self.socket_path = PathBuf::from("/run/fflocated.sock");
                self.pidfile = PathBuf::from("/run/fflocated.pid");
                self.state_dir =
                    fflocate::config_paths::resolve_state_dir(Path::new("/var/lib/fflocate"));
            }
        }
        self
    }

    /// The `fflocate` group's gid, or `None` when it does not exist. A system
    /// instance refuses to start without it rather than silently widening the
    /// socket's mode.
    ///
    /// Resolved through the platform's account database (`getgrnam`), not by
    /// parsing `/etc/group`: macOS serves groups from OpenDirectory and often
    /// has no `/etc/group` at all.
    pub fn fflocate_group_gid() -> Option<u32> {
        if let Some(gid) = fflocate::ids::gid_for_name("fflocate") {
            return Some(gid);
        }
        let txt = std::fs::read_to_string("/etc/group").ok()?;
        for line in txt.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() > 2 && f[0] == "fflocate" {
                return f[2].parse().ok();
            }
        }
        None
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
            self.pidfile = runtime_dir().join("fflocate.pid");
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
    pub fn with_shared_store(mut self, on: bool) -> Self {
        self.shared_store = on;
        self
    }

    /// Roots to index when neither `--root` nor `$FFLOCATE_ROOT` says otherwise.
    ///
    /// On Linux every readable native filesystem gets its own root (§4: one
    /// database, one root per filesystem), so the whole machine is indexed
    /// with each filesystem exactly once; the mount-aware crawl enforces the
    /// boundaries. Mounts the daemon cannot list are skipped loudly rather
    /// than degrading every status line, and when discovery yields nothing
    /// usable (containers, exotic fstypes) the historical `$HOME` root
    /// applies. Elsewhere `$HOME` is the default, unchanged.
    pub fn default_roots() -> Vec<PathBuf> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let mounts = fflocate::mount::read_mountinfo();
            let mut roots: Vec<PathBuf> = Vec::new();
            for mp in fflocate::mount::default_root_candidates(&mounts) {
                let p = PathBuf::from(&mp);
                if fflocate::mount::is_readable_dir(&p) {
                    roots.push(p);
                } else {
                    eprintln!(
                        "fflocated: skipping default root {mp} (not listable; run as a user that can read it, or index it explicitly with --root)"
                    );
                }
            }
            if roots.is_empty() {
                eprintln!("fflocated: no readable native filesystems discovered; indexing $HOME");
                roots.push(home_dir());
            }
            roots
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            vec![home_dir()]
        }
    }
}

/// The historical single default: the user's home directory.
fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("C:/"))
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/"))
    }
}

/// The socket path a client should try, honouring an override env var so the
/// test harness can run an isolated daemon.
pub fn socket_path_from_env() -> PathBuf {
    fflocate::config_paths::socket_path_from_env()
}

pub fn state_dir_from_env() -> PathBuf {
    fflocate::config_paths::state_dir_from_env()
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
    fn state_dir_is_stable_and_reused_in_place() {
        // The path is not derived from the format version: a fresh root gets the
        // stable store name, and an existing version-named store is reused where
        // it is, so a format bump neither abandons nor relocates the index.
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            fflocate::config_paths::resolve_state_dir(root.path()),
            root.path().join(fflocate::config_paths::STORE_DIR_NAME)
        );
        let old = root.path().join("3");
        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("snapshot.fnx"), b"v3").unwrap();
        assert_eq!(fflocate::config_paths::resolve_state_dir(root.path()), old);
    }

    #[test]
    fn defaults_never_tmp() {
        let c = Config::defaults();
        assert!(
            !c.socket_path.starts_with("/tmp"),
            "socket must not live in /tmp"
        );
    }

    #[test]
    fn default_roots_never_empty() {
        // Discovery or the $HOME fallback: starting with nowhere to index is
        // never an option, on any platform.
        assert!(!Config::default_roots().is_empty());
    }

    #[test]
    fn env_override_respected() {
        std::env::set_var("FFLOCATE_SOCKET", "/run/user/1/x.sock");
        assert_eq!(socket_path_from_env(), PathBuf::from("/run/user/1/x.sock"));
        std::env::remove_var("FFLOCATE_SOCKET");
    }

    #[test]
    fn backend_choice_parses_and_round_trips() {
        for name in ["inotify", "fanotify", "fake", "auto"] {
            let c = BackendChoice::parse(name).unwrap_or_else(|| panic!("{name} must parse"));
            assert_eq!(c.as_str(), name);
        }
        assert!(BackendChoice::parse("nope").is_none());
        assert!(
            BackendChoice::parse("Inotify").is_none(),
            "case-sensitive, like the CLI flag"
        );
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
        for c in [
            BackendChoice::Inotify,
            BackendChoice::Fanotify,
            BackendChoice::Fake,
        ] {
            assert_eq!(c.resolve(), Some(c));
        }
    }
}
