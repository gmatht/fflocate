//! `ff-gui` — a file-search browser for the findex daemon, built on
//! [rustxWidgets].
//!
//! The GUI talks to `findexd` over the same NDJSON protocol the CLI uses
//! (`findex::proto`, unix sockets on unix, TCP loopback on Windows): a query
//! box drives `query`, and the result list is
//! rendered as labels. Because the only rustxWidgets backend that builds without
//! a display server is `zork` (headless), the binary is usable in a terminal and
//! over ssh; on a machine with the GTK backends available the same widget code
//! runs against a real window.
//!
//! Design notes:
//! * The daemon is the source of truth; the GUI never walks the filesystem.
//! * Queries are debounced by the driver loop, not by timers in the UI layer.
//! * Every failure (daemon absent, protocol error) surfaces in the status line —
//!   the tool must never silently show an empty list for a broken connection.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use findex::config_paths::socket_path_from_env;
use findex::proto::{Request, Response};

/// Errors surfaced to the status line.
#[derive(Debug)]
pub enum GuiError {
    Connect(String),
    Io(String),
    Protocol(String),
    Backend(String),
}

impl std::error::Error for GuiError {}

impl std::fmt::Display for GuiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuiError::Connect(m) => write!(f, "cannot reach findexd: {m}"),
            GuiError::Io(m) => write!(f, "io: {m}"),
            GuiError::Protocol(m) => write!(f, "protocol: {m}"),
            GuiError::Backend(m) => write!(f, "widget backend: {m}"),
        }
    }
}

/// A connection to the daemon. Each call opens its own stream: the protocol is
/// request/response, and `findexd` closes a client after the terminal message,
/// so holding one socket open would leave a dead reader behind.
pub struct DaemonClient {
    socket: PathBuf,
}

impl Default for DaemonClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DaemonClient {
    /// Locate the daemon socket (honouring `$FINDEX_SOCKET`).
    pub fn new() -> DaemonClient {
        DaemonClient { socket: socket_path_from_env() }
    }

    pub fn with_socket(socket: PathBuf) -> DaemonClient {
        DaemonClient { socket }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Send one request, collect the response lines until the terminal message.
    pub fn request(&self, req: &Request) -> Result<Vec<Response>, GuiError> {
        let stream = findex::transport::connect(&self.socket).map_err(|e| {
            GuiError::Connect(format!("{}: {e}", self.socket.display()))
        })?;
        let mut writer = stream.try_clone().map_err(|e| GuiError::Io(e.to_string()))?;
        let line = serde_json::to_string(req).map_err(|e| GuiError::Protocol(e.to_string()))?;
        writeln!(writer, "{line}").map_err(|e| GuiError::Io(e.to_string()))?;
        writer.flush().map_err(|e| GuiError::Io(e.to_string()))?;

        let reader = BufReader::new(stream);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| GuiError::Io(e.to_string()))?;
            if line.trim().is_empty() {
                continue;
            }
            let resp: Response =
                serde_json::from_str(&line).map_err(|e| GuiError::Protocol(format!("{e}: {line}")))?;
            let terminal = matches!(
                resp,
                Response::Done { .. }
                    | Response::Error { .. }
                    | Response::Synced { .. }
                    | Response::Stats { .. }
                    | Response::Pong { .. }
                    | Response::Rescanning { .. }
            );
            out.push(resp);
            if terminal {
                break;
            }
        }
        Ok(out)
    }

    /// Run a search and return `(paths, total, meta_summary)`.
    pub fn search(&self, query: &str, limit: usize) -> Result<SearchOutcome, GuiError> {
        let resp = self.request(&Request::Query {
            q: query.to_string(),
            limit,
            json: false,
            count: false,
            expression: None,
            glob: None,
            glob_includedotfiles: None,
            glob_noescape: None,
            suffix: None,
            path: None,
            relative_root: None,
            case_sensitive: None,
            fuzzy: false,
            max_distance: None,
        })?;

        let mut paths = Vec::new();
        let mut total = 0u64;
        let mut fresh = true;
        let mut stale: Vec<String> = Vec::new();
        let mut catching_up = false;
        for r in &resp {
            match r {
                Response::Meta { fresh: f, stale_roots, catching_up: c, .. } => {
                    fresh = *f;
                    stale = stale_roots.clone();
                    catching_up = c.unwrap_or(false);
                }
                Response::Hit { path, .. } => paths.push(path.clone()),
                Response::Done { count, .. } => total = *count,
                Response::Error { message } => return Err(GuiError::Protocol(message.clone())),
                _ => {}
            }
        }
        Ok(SearchOutcome { paths, total, fresh, stale_roots: stale, catching_up })
    }

    /// Health counters for the status line.
    pub fn stats(&self) -> Result<serde_json::Value, GuiError> {
        let resp = self.request(&Request::Stats { roots: false })?;
        for r in resp {
            if let Response::Stats { stats } = r {
                return Ok(stats);
            }
        }
        Ok(serde_json::Value::Null)
    }

    pub fn hello(&self) -> bool {
        matches!(self.request(&Request::Hello), Ok(v) if !v.is_empty())
    }
}

/// What a search returned, including the consistency metadata (§11).
#[derive(Clone, Debug, Default)]
pub struct SearchOutcome {
    pub paths: Vec<String>,
    pub total: u64,
    /// False when the daemon's generation moved on: results are a fresh view.
    pub fresh: bool,
    /// Roots the daemon reports as degraded/behind.
    pub stale_roots: Vec<String>,
    /// The daemon is still replaying its journal / initial crawl.
    pub catching_up: bool,
}

impl SearchOutcome {
    /// A one-line summary for the status bar. Staleness is stated explicitly:
    /// "loudly stale beats silently wrong" (DESIGN §7).
    pub fn status_line(&self, shown: usize) -> String {
        let mut s = if self.total as usize == self.paths.len() {
            format!("{} matches", self.total)
        } else {
            format!("{} of {} matches", self.paths.len().min(shown), self.total)
        };
        if !self.fresh {
            s.push_str("  [fresh view]");
        }
        if self.catching_up {
            s.push_str("  [daemon catching up]");
        }
        if !self.stale_roots.is_empty() {
            s.push_str(&format!("  [stale roots: {}]", self.stale_roots.join(", ")));
        }
        s
    }
}

/// Format one result row: the basename, then the containing directory, so the
/// narrow list stays readable.
pub fn format_hit(path: &str) -> String {
    let p = Path::new(path);
    let name = p.file_name().map(|s| s.to_string_lossy()).unwrap_or_default();
    match p.parent().map(|d| d.to_string_lossy()) {
        Some(dir) if !dir.is_empty() => format!("{name}   —   {dir}"),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_hit_splits_name_and_dir() {
        assert_eq!(format_hit("/home/me/src/main.rs"), "main.rs   —   /home/me/src");
        assert_eq!(format_hit("bare"), "bare");
    }

    #[test]
    fn status_line_reports_totals() {
        let o = SearchOutcome { paths: vec!["a".into()], total: 5, fresh: true, ..Default::default() };
        assert_eq!(o.status_line(1), "1 of 5 matches");
    }

    #[test]
    fn status_line_shouts_about_staleness() {
        let o = SearchOutcome {
            paths: vec![],
            total: 0,
            fresh: false,
            stale_roots: vec!["/mnt/ro".into()],
            catching_up: true,
        };
        let s = o.status_line(0);
        assert!(s.contains("fresh view"), "{s}");
        assert!(s.contains("catching up"), "{s}");
        assert!(s.contains("/mnt/ro"), "{s}");
    }

    #[test]
    fn status_line_quiet_when_healthy() {
        let o = SearchOutcome { paths: vec!["a".into()], total: 1, fresh: true, ..Default::default() };
        let s = o.status_line(1);
        assert_eq!(s, "1 matches");
        assert!(!s.contains('['), "no noise on the happy path: {s}");
    }
}
