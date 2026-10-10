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

/// The query the daemon should actually run, built from the quick box and the
/// advanced foldout.
///
/// The foldout's contents are *merged* into the query string rather than sent
/// as separate protocol fields, so the effective query is one thing the user can
/// read, copy, and paste into `ff`. When both boxes are filled the advanced one
/// wins (it is the more specific expression) and the simple one is ignored; the
/// status line says so.
pub fn effective_query(quick: &str, advanced: &str) -> (String, bool) {
    let quick = quick.trim();
    let advanced = advanced.trim();
    match (quick.is_empty(), advanced.is_empty()) {
        // Advanced wins when both are set.
        (_, false) => (advanced.to_string(), !quick.is_empty()),
        (false, true) => (quick.to_string(), false),
        (true, true) => (String::new(), false),
    }
}

/// A one-line reminder of the advanced syntax, shown inside the foldout.
pub const ADVANCED_SYNTAX_HELP: &str = "ext:rs  path:src  case:Foo  regex:^m.*\\.rs  \"exact phrase\"  *.rs  term1 term2 (all must match)";

/// Build a `file://` URI from an absolute path, percent-encoding the bytes that
/// are not allowed raw in a URI path.
///
/// The drag payload uses this so a file manager or editor receiving the drop
/// treats it as a real file. Only the characters that actually need escaping in
/// a path segment are encoded; `/` is left as the separator.
pub fn path_to_file_uri(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 7);
    out.push_str("file://");
    for &b in path.as_bytes() {
        match b {
            // Unreserved (RFC 3986) plus the path separators and sub-delims that
            // are legal unescaped in a path.
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-' | b'.' | b'_' | b'~'
            | b'/' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')'
            | b'*' | b'+' | b',' | b';' | b'=' | b':' | b'@' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The `(uris, text)` payload for dragging `path`: a `file://` URI list (what a
/// file manager wants) and the plain path (what a terminal or text field
/// wants).
pub fn drag_payload(path: &str) -> (Vec<String>, String) {
    (vec![path_to_file_uri(path)], path.to_string())
}

/// What the results context menu offers for a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    /// Copy the row's absolute path to the clipboard.
    CopyPath,
    /// Open the row's file with the desktop's default handler.
    Open,
    /// Open the directory containing the row's file.
    OpenContainingFolder,
}

impl RowAction {
    /// The menu label.
    pub fn label(self) -> &'static str {
        match self {
            RowAction::CopyPath => "Copy path",
            RowAction::Open => "Open",
            RowAction::OpenContainingFolder => "Open containing folder",
        }
    }

    /// The program and arguments to run for this action on `path`.
    ///
    /// Both Open and Open-containing-folder go through `xdg-open`, which
    /// dispatches on a file or a directory identically.
    pub fn command(self, path: &str) -> (&'static str, Vec<String>) {
        match self {
            // Copy is handled in-process (clipboard); no external command.
            RowAction::CopyPath => ("", Vec::new()),
            RowAction::Open => ("xdg-open", vec![path.to_string()]),
            RowAction::OpenContainingFolder => {
                // A bare name's parent is "" (not None), so fall back to the
                // path itself in that case too: xdg-open "." would be worse.
                let dir = match Path::new(path).parent() {
                    Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
                    _ => path.to_string(),
                };
                ("xdg-open", vec![dir])
            }
        }
    }
}

/// The result row a click at `line` (0-based) lands on, given `paths` and the
/// number of extra trailer lines the view appends (the "… N more" line).
///
/// Returns `None` when the line is past the last row — clicking blank space (or
/// the trailer) opens nothing rather than silently acting on the wrong file.
pub fn row_at_line(paths: &[String], line: i32) -> Option<&str> {
    if line < 0 {
        return None;
    }
    paths.get(line as usize).map(|s| s.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_query_prefers_whichever_is_filled() {
        // Only the quick box.
        assert_eq!(effective_query("main", ""), ("main".to_string(), false));
        // Only the advanced box.
        assert_eq!(effective_query("", "ext:rs"), ("ext:rs".to_string(), false));
        // Whitespace is not content.
        assert_eq!(effective_query("  ", "\t"), (String::new(), false));
    }

    #[test]
    fn effective_query_advanced_wins_and_is_flagged() {
        // Both set: the advanced one runs, and the caller is told the quick box
        // was shadowed so the status line can say so.
        let (q, shadowed) = effective_query("main", "ext:rs path:src");
        assert_eq!(q, "ext:rs path:src");
        assert!(shadowed);
    }

    #[test]
    fn file_uri_encodes_special_characters() {
        assert_eq!(path_to_file_uri("/home/me/src/main.rs"), "file:///home/me/src/main.rs");
        // Spaces and other reserved characters are percent-encoded.
        assert_eq!(path_to_file_uri("/a/my file.rs"), "file:///a/my%20file.rs");
        assert_eq!(path_to_file_uri("/a/b#c?d"), "file:///a/b%23c%3Fd");
        // A leading slash is preserved (three slashes after the scheme).
        assert_eq!(path_to_file_uri("/"), "file:///");
    }

    #[test]
    fn drag_payload_carries_both_forms() {
        let (uris, text) = drag_payload("/a/b.rs");
        assert_eq!(uris, vec!["file:///a/b.rs".to_string()]);
        assert_eq!(text, "/a/b.rs");
    }

    #[test]
    fn row_at_line_picks_the_clicked_row() {
        let rows = vec!["/a.rs".to_string(), "/b.rs".to_string()];
        assert_eq!(row_at_line(&rows, 0), Some("/a.rs"));
        assert_eq!(row_at_line(&rows, 1), Some("/b.rs"));
        // Past the last row (blank space, or the "… N more" trailer line).
        assert_eq!(row_at_line(&rows, 2), None);
        // A click above the first row, or a negative coordinate.
        assert_eq!(row_at_line(&rows, -1), None);
    }

    #[test]
    fn row_action_labels_and_commands() {
        assert_eq!(RowAction::CopyPath.label(), "Copy path");
        // Copy needs no external command.
        assert_eq!(RowAction::CopyPath.command("/a/b.rs").0, "");
        // Open launches the file itself...
        let (prog, args) = RowAction::Open.command("/a/b.rs");
        assert_eq!(prog, "xdg-open");
        assert_eq!(args, vec!["/a/b.rs".to_string()]);
        // ...while "containing folder" launches the parent directory.
        let (prog, args) = RowAction::OpenContainingFolder.command("/a/b.rs");
        assert_eq!(prog, "xdg-open");
        assert_eq!(args, vec!["/a".to_string()]);
    }

    #[test]
    fn open_containing_folder_of_a_bare_name_is_the_name() {
        // A path with no parent directory falls back to the path itself.
        let (_, args) = RowAction::OpenContainingFolder.command("b.rs");
        assert_eq!(args, vec!["b.rs".to_string()]);
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
