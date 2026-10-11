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
//! * Queries are debounced, and the socket is non-blocking: a request is stepped
//!   from the UI's timer pump rather than awaited, so a slow or wedged daemon
//!   cannot freeze the window.
//! * Every failure (daemon absent, protocol error, truncated reply) surfaces in
//!   the status line — the tool must never silently show an empty list for a
//!   broken connection.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use findex::config_paths::socket_path_from_env;
use findex::proto::{Request, Response};

/// Re-exported because `PendingRequest::records` hands these back to the GUI.
pub use findex::proto::Response as WireResponse;

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
        // Stamp the protocol version. A daemon built before a behaviour change
        // rejects the request rather than silently ignoring a field it does not
        // understand (that is how a stale daemon made the Case-sensitive
        // checkbox look broken).
        let mut v: serde_json::Value =
            serde_json::to_value(req).map_err(|e| GuiError::Protocol(e.to_string()))?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "v".into(),
                serde_json::Value::from(findex::proto::PROTOCOL_VERSION),
            );
        }
        let line = serde_json::to_string(&v).map_err(|e| GuiError::Protocol(e.to_string()))?;
        // FINDEX_DEBUG=1 prints each request, so a setting that "does nothing"
        // can be traced to what was actually sent (e.g. whether case_sensitive
        // was included at all).
        if std::env::var_os("FINDEX_DEBUG").is_some() {
            eprintln!("ff-gui -> {line}");
        }
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
        self.search_with(query, limit, &SearchOptions::default())
    }

    /// Run a search honouring the advanced-search controls.
    ///
    /// `count_only` asks the daemon for just the total (it skips path
    /// materialization), so `paths` comes back empty and only `total` is
    /// meaningful.
    pub fn search_with(
        &self,
        query: &str,
        limit: usize,
        opts: &SearchOptions,
    ) -> Result<SearchOutcome, GuiError> {
        let req = Self::query_request(query, limit, opts);
        let resp = self.request(&req)?;
        self.outcome_from_records(&resp)
    }

    /// Build the `query` request a search should send.
    ///
    /// Separate from sending it so the non-blocking path can serialize a request
    /// now and transmit it over several event-loop ticks.
    pub fn query_request(query: &str, limit: usize, opts: &SearchOptions) -> Request {
        Request::Query {
            q: query.to_string(),
            limit,
            json: false,
            count: opts.count_only,
            expression: None,
            glob: None,
            glob_includedotfiles: None,
            glob_noescape: None,
            suffix: None,
            path: None,
            relative_root: None,
            case_sensitive: opts.case_sensitive.then_some(true),
            fuzzy: opts.fuzzy,
            max_distance: if opts.fuzzy { opts.max_distance } else { None },
        }
    }

    /// Turn a complete set of response records into a [`SearchOutcome`].
    ///
    /// Shared by the blocking `search_with` and the non-blocking GUI path, so the
    /// two cannot interpret a reply differently.
    pub fn outcome_from_records(
        &self,
        resp: &[Response],
    ) -> Result<SearchOutcome, GuiError> {
        let mut paths = Vec::new();
        let mut total = 0u64;
        let mut fresh = true;
        let mut stale: Vec<String> = Vec::new();
        let mut catching_up = false;
        for r in resp {
            match r {
                Response::Meta { fresh: f, stale_roots, catching_up: c, .. } => {
                    fresh = *f;
                    stale = stale_roots.clone();
                    catching_up = c.unwrap_or(false);
                }
                Response::Hit { path, .. } => paths.push(path.clone()),
                Response::Done { count, .. } => total = *count,
                Response::Error { message } => {
                    // A version rejection means a *stale daemon* is running:
                    // say so and how to fix it, rather than surfacing a bare
                    // protocol string that looks like a bug in the GUI.
                    if message.contains("unsupported protocol version") {
                        return Err(GuiError::Protocol(format!(
                            "{message} — the running findexd predates this build; \
                             restart it (findexd reads --socket/--state-dir, or \
                             kill {} and let `ff` respawn one)",
                            self.socket.display()
                        )));
                    }
                    return Err(GuiError::Protocol(message.clone()));
                }
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

/// The advanced-search controls, as sent to the daemon.
///
/// Each field maps to a real `Query` protocol field — nothing here is
/// decorative. `Default` is the plain behaviour (smart-case, exact matching,
/// return the hits).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// Force case-sensitive matching, overriding the daemon's smart-case.
    pub case_sensitive: bool,
    /// Approximate (edit-distance) matching, ranked by score.
    pub fuzzy: bool,
    /// Maximum edit distance for `fuzzy`; `None` lets the daemon choose.
    pub max_distance: Option<u32>,
    /// Ask only for the total, skipping path materialization. `paths` is then
    /// empty and only `total` is meaningful.
    pub count_only: bool,
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
        self.status_line_opts(shown, false)
    }

    /// As [`status_line`](Self::status_line), but aware of a count-only search:
    /// the daemon then sends no paths, so "0 of 6" would be misleading — the
    /// total *is* the answer.
    pub fn status_line_opts(&self, shown: usize, count_only: bool) -> String {
        let mut s = if count_only {
            format!("{} matches (count only)", self.total)
        } else if self.total as usize == self.paths.len() {
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

/// The max edit distance the foldout offers before the user changes it.
/// Small numbers keep fuzzy matching useful rather than noisy.
pub const DEFAULT_MAX_DISTANCE: &str = "2";

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

/// One row of the results, as three columns: icon, name, path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Icon *name* to look up in the icon theme (e.g. `folder`,
    /// `text-x-generic`). The GUI loads it; the terminal fallback maps it to a
    /// glyph.
    pub icon: String,
    /// The file name (last path component).
    pub name: String,
    /// The containing directory (everything before the last `/`), or the whole
    /// path when there is no separator.
    pub dir: String,
    /// The full path, which is what Copy / Open / drag act on.
    pub path: String,
}

/// Split the daemon's paths into icon/name/dir rows.
///
/// The daemon returns absolute paths; everything here is derived client-side, so
/// no protocol change is needed. `is_dir_hint` marks paths the daemon reported as
/// directories (trailing slash): those always get the folder icon.
pub fn rows_from_paths(paths: &[String]) -> Vec<Row> {
    paths.iter().map(|p| row_from_path(p)).collect()
}

/// Build one row from a path.
pub fn row_from_path(path: &str) -> Row {
    let (dir, name) = match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    };
    // A path with a trailing slash is a directory; its name is the component
    // before the slash.
    let (dir, name) = if name.is_empty() {
        match dir.rfind('/') {
            Some(i) => (&dir[..i], &dir[i + 1..]),
            None => ("", dir),
        }
    } else {
        (dir, name)
    };
    let is_dir = path.ends_with('/');
    Row {
        icon: icon_name_for(name, is_dir).to_string(),
        name: name.to_string(),
        dir: dir.to_string(),
        path: path.to_string(),
    }
}

/// The icon-theme name for a file, from its name and type.
///
/// A small table of the common cases, falling back to `text-x-generic` (files)
/// or `folder` (directories). Kept deliberately simple: the icon theme decides
/// whether a name exists, and the GUI falls back if not.
pub fn icon_name_for(name: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return "folder";
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "rs" => "text-x-rust",
        "py" => "text-x-python",
        "js" | "mjs" | "cjs" => "text-x-javascript",
        "ts" | "tsx" => "text-x-typescript",
        "c" | "h" => "text-x-csrc",
        "cpp" | "cc" | "cxx" | "hpp" => "text-x-c++src",
        "go" => "text-x-go",
        "java" => "text-x-java",
        "rb" => "text-x-ruby",
        "sh" | "bash" | "zsh" => "text-x-script",
        "md" | "markdown" => "text-markdown",
        "json" => "application-json",
        "toml" | "yaml" | "yml" | "ini" | "cfg" => "text-x-generic",
        "html" | "htm" => "text-html",
        "css" => "text-css",
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" => "image-x-generic",
        "pdf" => "application-pdf",
        "zip" | "gz" | "xz" | "tar" | "zst" | "bz2" => "package-x-generic",
        "txt" | "log" => "text-x-generic",
        _ => "text-x-generic",
    }
}

/// Render the results view for a count-only search: the number on its own line.
///
/// A count-only query returns no paths, so this must not look like "no matches".
pub fn render_count(total: u64) -> String {
    format!("{total} matches (count only)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_splits_path_into_name_and_dir() {
        let r = row_from_path("/home/me/src/main.rs");
        assert_eq!(r.name, "main.rs");
        assert_eq!(r.dir, "/home/me/src");
        assert_eq!(r.path, "/home/me/src/main.rs");
        assert_eq!(r.icon, "text-x-rust");
    }

    #[test]
    fn row_handles_a_bare_name_and_a_root_child() {
        let bare = row_from_path("main.rs");
        assert_eq!(bare.name, "main.rs");
        assert_eq!(bare.dir, "");
        let root = row_from_path("/etc");
        assert_eq!(root.name, "etc");
        assert_eq!(root.dir, "");
    }

    #[test]
    fn row_recognises_directories_by_trailing_slash() {
        // The daemon marks directories with a trailing slash.
        let r = row_from_path("/home/me/src/");
        assert_eq!(r.name, "src");
        assert_eq!(r.dir, "/home/me");
        assert_eq!(r.icon, "folder");
    }

    #[test]
    fn icon_names_cover_common_types() {
        assert_eq!(icon_name_for("main.rs", false), "text-x-rust");
        assert_eq!(icon_name_for("notes.md", false), "text-markdown");
        assert_eq!(icon_name_for("data.json", false), "application-json");
        assert_eq!(icon_name_for("logo.png", false), "image-x-generic");
        assert_eq!(icon_name_for("README", false), "text-x-generic");
        assert_eq!(icon_name_for("src", true), "folder");
        // Extension matching is case-insensitive.
        assert_eq!(icon_name_for("MAIN.RS", false), "text-x-rust");
    }

    #[test]
    fn rows_from_paths_keeps_order() {
        let paths = vec!["/a/x.rs".to_string(), "/a/y.md".to_string()];
        let rows = rows_from_paths(&paths);
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["x.rs", "y.md"]);
    }

    #[test]
    fn count_only_status_does_not_say_zero_of_n() {
        // In count mode no paths come back, so the plain status line would
        // render a misleading "0 of 6".
        let o = SearchOutcome { paths: vec![], total: 6, fresh: true, ..Default::default() };
        assert_eq!(o.status_line_opts(0, true), "6 matches (count only)");
        assert_eq!(o.status_line(0), "0 of 6 matches");
    }

    #[test]
    fn render_count_is_not_an_empty_list() {
        // A count-only result must read as a number, never as "no matches".
        assert_eq!(render_count(0), "0 matches (count only)");
        assert_eq!(render_count(42), "42 matches (count only)");
    }

    #[test]
    fn search_options_default_is_plain() {
        let o = SearchOptions::default();
        assert_eq!(o, SearchOptions {
            case_sensitive: false,
            fuzzy: false,
            max_distance: None,
            count_only: false,
        });
    }

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

// ---------------------------------------------------------------------------
// As-you-type search.
//
// The parts that are easy to get wrong live here, in plain Rust with no widgets,
// so they can be tested without a display. The GUI supplies three things: a way
// to read the current query (main thread only — the widgets are `Rc`/`RefCell`),
// a way to render a result (also main thread only), and a clock.
// ---------------------------------------------------------------------------

/// A search ticket: the sequence number the gate assigned.
///
/// Deliberately carries no query text. The gate's job is *ordering*, and the query
/// is read from the widgets at the moment it is sent — putting a copy here would
/// invite a caller to render a result against a query that is no longer in the box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchTicket {
    /// Monotonic id, so a slow answer cannot overwrite a newer one.
    pub seq: u64,
}

/// Decide whether a request may be *issued*: only if no newer one has been issued
/// and none is in flight.
///
/// Without this, holding down a key would queue one socket round-trip per repeat
/// and the answers would arrive out of order.
#[derive(Debug, Default)]
pub struct SearchGate {
    next_seq: u64,
    /// Requests issued but not necessarily applied yet.
    in_flight: u64,
    /// The highest seq applied to the UI.
    applied: u64,
    /// The seq most recently *issued*.
    issued: u64,
}

impl SearchGate {
    pub fn new() -> SearchGate {
        SearchGate::default()
    }

    /// Ask permission to issue a query. `None` means one is already in flight —
    /// the caller should re-arm its debounce and try again, not queue up.
    pub fn begin(&mut self) -> Option<SearchTicket> {
        if self.in_flight > 0 {
            return None;
        }
        self.next_seq += 1;
        self.issued = self.next_seq;
        self.in_flight += 1;
        Some(SearchTicket { seq: self.issued })
    }

    /// A finished request. Returns the outcome to render, or `None` when it is
    /// stale — an order the user has already moved past.
    pub fn finish<T>(&mut self, seq: u64, outcome: T) -> Option<T> {
        self.in_flight = self.in_flight.saturating_sub(1);
        if seq <= self.applied {
            return None; // a newer result is already on screen
        }
        self.applied = seq;
        Some(outcome)
    }

    /// The seq most recently issued, for diagnostics/tests.
    pub fn issued(&self) -> u64 {
        self.issued
    }

    /// Whether a request is outstanding.
    pub fn busy(&self) -> bool {
        self.in_flight > 0
    }
}

/// Is the query worth searching? An empty or whitespace-only box should not hit
/// the daemon on every backspace to empty.
pub fn query_is_searchable(q: &str) -> bool {
    !q.trim().is_empty()
}

// ---------------------------------------------------------------------------
// Non-blocking request.
//
// A query's reply is *multi-record* — a `Meta`, then one `Hit` per match, then a
// terminal record — so it arrives in several packets. That rules out "set the
// socket non-blocking and read once": the read must be resumable across event-loop
// ticks, remembering a partial line across calls.
//
// `PendingRequest` is that resumable machine. Every method returns promptly; none
// of them block. A caller drives it from whatever loop it has (the GUI's 30 ms
// timer pump) and keeps rendering in between, so a daemon that is slow — or wedged
// — cannot freeze the window.
//
// The socket is non-blocking for the write as well as the read: a request line can
// be large (a long expression query) and `write` is allowed to accept only part of
// it, or none of it.
// ---------------------------------------------------------------------------

/// How far a [`PendingRequest`] has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestState {
    /// Still sending the request line.
    Writing,
    /// Request sent; accumulating response records.
    Reading,
    /// A terminal record was seen. The result is ready.
    Done,
}

/// Progress from one non-blocking step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Nothing more can be done until the socket is readable/writable again.
    /// The caller should return to its loop and call [`PendingRequest::advance`]
    /// on a later tick — *not* mark the request failed.
    WouldBlock,
    /// Work was done; call again immediately (there may be more buffered).
    Advanced,
    /// The terminal record arrived.
    Finished,
}

/// A request in flight over a non-blocking socket.
pub struct PendingRequest {
    stream: findex::transport::Stream,
    /// The request line, held until it has been fully written. `sent` is how many
    /// bytes of it the kernel has accepted so far.
    out: Vec<u8>,
    sent: usize,
    /// Bytes received but not yet a complete line (the tail of a partial record).
    partial: Vec<u8>,
    /// Records parsed so far, in arrival order.
    records: Vec<Response>,
    state: RequestState,
    /// Set when a terminal record has been seen, so `is_done` needs no re-parse.
    terminal_seen: bool,
}

/// A record that ends a reply. After one of these the daemon stops talking.
fn is_terminal(r: &Response) -> bool {
    matches!(
        r,
        Response::Done { .. }
            | Response::Error { .. }
            | Response::Synced { .. }
            | Response::Stats { .. }
            | Response::Pong { .. }
            | Response::Rescanning { .. }
    )
}

impl PendingRequest {
    /// Start a request on `stream`, which is set non-blocking here.
    ///
    /// The socket must be non-blocking *before* any I/O, hence this constructor
    /// rather than a flag a caller could forget.
    pub fn start(
        stream: findex::transport::Stream,
        line: &str,
    ) -> Result<PendingRequest, GuiError> {
        stream
            .set_nonblocking(true)
            .map_err(|e| GuiError::Io(format!("set_nonblocking: {e}")))?;
        let mut out = String::with_capacity(line.len() + 1);
        out.push_str(line);
        out.push('\n');
        Ok(PendingRequest {
            stream,
            out: out.into_bytes(),
            sent: 0,
            partial: Vec::new(),
            records: Vec::new(),
            state: RequestState::Writing,
            terminal_seen: false,
        })
    }

    pub fn state(&self) -> RequestState {
        self.state
    }

    /// True once a terminal record has been parsed.
    pub fn is_done(&self) -> bool {
        self.terminal_seen
    }

    /// The records received so far (including the terminal one, once parsed).
    pub fn records(&self) -> &[Response] {
        &self.records
    }

    /// Take the finished records out, leaving the machine empty.
    pub fn take_records(&mut self) -> Vec<Response> {
        std::mem::take(&mut self.records)
    }

    /// Make whatever progress is possible right now, without blocking.
    ///
    /// Safe to call repeatedly and after completion (which returns `Finished`).
    /// `WouldBlock` is *not* an error and must not abort the request: it means the
    /// kernel has nothing for us yet and the caller should come back later.
    pub fn advance(&mut self) -> Result<Progress, GuiError> {
        if self.terminal_seen {
            return Ok(Progress::Finished);
        }
        let mut did_something = false;

        // 1. Finish the write. `write` may accept only part of the line.
        while self.sent < self.out.len() {
            match std::io::Write::write(&mut self.stream, &self.out[self.sent..]) {
                Ok(0) => {
                    return Err(GuiError::Io("daemon closed the socket while writing".into()))
                }
                Ok(n) => {
                    self.sent += n;
                    did_something = true;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(GuiError::Io(e.to_string())),
            }
        }
        if self.sent < self.out.len() {
            // Not all sent; the caller must wait for writability.
            return Ok(if did_something { Progress::Advanced } else { Progress::WouldBlock });
        }
        self.state = RequestState::Reading;

        // 2. Drain whatever is readable, parsing whole lines as they complete.
        let mut buf = [0u8; 8192];
        loop {
            match std::io::Read::read(&mut self.stream, &mut buf) {
                Ok(0) => {
                    // EOF. The daemon closes after the terminal record, so this is
                    // only a surprise when the terminal has not arrived.
                    if self.terminal_seen {
                        return Ok(Progress::Finished);
                    }
                    return Err(GuiError::Io(
                        "daemon closed the connection before the reply finished".into(),
                    ));
                }
                Ok(n) => {
                    did_something = true;
                    self.partial.extend_from_slice(&buf[..n]);
                    self.drain_lines()?;
                    if self.terminal_seen {
                        return Ok(Progress::Finished);
                    }
                    // Keep reading: more records may already be buffered.
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(GuiError::Io(e.to_string())),
            }
        }

        Ok(if did_something { Progress::Advanced } else { Progress::WouldBlock })
    }

    /// Turn every complete line in `partial` into a record, leaving any trailing
    /// fragment for the next packet.
    fn drain_lines(&mut self) -> Result<(), GuiError> {
        loop {
            let Some(nl) = self.partial.iter().position(|&b| b == b'\n') else {
                return Ok(()); // no complete line yet
            };
            let line: Vec<u8> = self.partial.drain(..=nl).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let rec: Response = serde_json::from_str(text)
                .map_err(|e| GuiError::Protocol(format!("{e}: {text}")))?;
            if is_terminal(&rec) {
                self.terminal_seen = true;
            }
            self.records.push(rec);
            if self.terminal_seen {
                return Ok(());
            }
        }
    }
}

/// Serialize a request, stamping the protocol version.
///
/// The version matters: a daemon built before a behaviour change rejects the
/// request rather than silently ignoring a field it does not understand (that is
/// how a stale daemon once made the Case-sensitive checkbox look broken).
pub fn encode_request(req: &Request) -> Result<String, GuiError> {
    let mut v: serde_json::Value =
        serde_json::to_value(req).map_err(|e| GuiError::Protocol(e.to_string()))?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "v".into(),
            serde_json::Value::from(findex::proto::PROTOCOL_VERSION),
        );
    }
    serde_json::to_string(&v).map_err(|e| GuiError::Protocol(e.to_string()))
}

#[cfg(test)]
mod asyou_type_tests {
    use super::*;

    #[test]
    fn a_second_request_is_refused_while_one_is_in_flight() {
        let mut g = SearchGate::new();
        assert!(g.begin().is_some(), "first is issued");
        assert!(g.begin().is_none(), "no pile-up while busy");
        assert!(g.busy());
    }

    #[test]
    fn a_new_request_is_allowed_once_the_first_finishes() {
        let mut g = SearchGate::new();
        let r = g.begin().unwrap();
        assert!(g.finish(r.seq, "A").is_some(), "the first result renders");
        assert!(!g.busy());
        assert!(g.begin().is_some(), "now the next may go");
    }

    #[test]
    fn a_stale_result_never_overwrites_a_newer_one() {
        // The ordering hazard: request A takes longer than B. B's answer lands
        // first; A's must then be dropped, not painted over the top.
        let mut g = SearchGate::new();
        let a = g.begin().unwrap();
        // Simulate A still running by finishing it out of order: reset busy so B can
        // be issued, which is what happens when the GUI re-arms.
        g.in_flight = 0;
        let b = g.begin().unwrap();
        assert!(b.seq > a.seq);
        assert_eq!(g.finish(b.seq, "B"), Some("B"), "the newer result renders");
        assert_eq!(g.finish(a.seq, "A"), None, "the older result is dropped");
    }

    #[test]
    fn an_empty_query_is_not_searchable() {
        assert!(!query_is_searchable(""));
        assert!(!query_is_searchable("   "));
        assert!(!query_is_searchable("\t"));
        assert!(query_is_searchable("a"));
        assert!(query_is_searchable(" main.rs "));
    }

    #[test]
    fn sequence_numbers_are_monotonic() {
        let mut g = SearchGate::new();
        let mut last = 0;
        for _ in 0..5 {
            let r = g.begin().unwrap();
            assert!(r.seq > last, "seq must increase");
            last = r.seq;
            g.finish(r.seq, ());
        }
        assert_eq!(g.issued(), last);
    }
}

#[cfg(test)]
mod nonblocking_tests {
    //! These use a real loopback socket pair rather than a mock: the whole point is
    //! `WouldBlock`, which only a real non-blocking fd produces.

    use super::*;
    use std::io::Write;
    use std::net::Shutdown;

    /// A listener on a private temp path, plus a connected client stream.
    fn pair() -> (tempfile::TempDir, findex::transport::Stream, findex::transport::Stream) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let listener = findex::transport::Listener::bind(&path).unwrap();
        let client = findex::transport::Stream::connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();
        (dir, client, server)
    }

    /// Serialize a real `Response`, so these fixtures cannot drift from the wire
    /// format (`Response` is tagged with a `type` field).
    fn line(r: &Response) -> String {
        format!("{}\n", serde_json::to_string(r).unwrap())
    }

    fn meta() -> Response {
        Response::Meta {
            gen: 1,
            lsn: 1,
            fresh: true,
            elapsed_ms: 0.1,
            stale_roots: vec![],
            catching_up: None,
            backend: None,
            entries: None,
        }
    }

    fn hit(path: &str, id: u32) -> Response {
        Response::Hit { path: path.into(), id }
    }

    fn done(count: u64) -> Response {
        Response::Done { count, cursor: "c1".into() }
    }

    fn pong() -> Response {
        Response::Pong { pid: 1, version: 2, backend: "fake".into() }
    }

    /// Drive a request to completion, giving the peer a chance to write between
    /// steps. Returns the number of ticks it took and the final progress.
    fn pump_until_done(
        p: &mut PendingRequest,
        server: &mut findex::transport::Stream,
        max_ticks: usize,
    ) -> (usize, Progress) {
        let mut last = Progress::WouldBlock;
        for i in 0..max_ticks {
            last = p.advance().unwrap();
            // Let the peer make progress too (it is non-blocking as well).
            let _ = server.flush();
            if last == Progress::Finished {
                return (i + 1, last);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        (max_ticks, last)
    }

    #[test]
    fn a_reply_split_across_packets_is_reassembled() {
        let (_d, client, mut server) = pair();
        let mut p = PendingRequest::start(client, "{\"op\":\"query\"}").unwrap();

        // Send a reply in pieces, including *mid-record* splits, which is the case
        // a one-shot read would get wrong.
        let whole = format!("{}{}{}", line(&meta()), line(&hit("/a/main.rs", 1)), line(&done(1)));
        let bytes = whole.as_bytes();
        for chunk in bytes.chunks(7) {
            server.write_all(chunk).unwrap();
            server.flush().unwrap();
            p.advance().unwrap();
        }

        let (_, last) = pump_until_done(&mut p, &mut server, 50);
        assert_eq!(last, Progress::Finished, "should finish once the terminal arrives");
        let recs = p.records();
        assert_eq!(recs.len(), 3, "Meta + Hit + Done");
        assert!(matches!(recs[0], Response::Meta { .. }));
        assert!(matches!(recs[1], Response::Hit { .. }));
        assert!(matches!(recs[2], Response::Done { count: 1, .. }));
    }

    #[test]
    fn would_block_is_not_an_error_and_is_resumable() {
        let (_d, client, mut server) = pair();
        let mut p = PendingRequest::start(client, "{\"op\":\"query\"}").unwrap();

        // Nothing written by the peer yet: we must get through the write phase and
        // then report WouldBlock, *not* fail.
        let mut saw_would_block = false;
        for _ in 0..5 {
            if p.advance().unwrap() == Progress::WouldBlock {
                saw_would_block = true;
                break;
            }
        }
        assert!(saw_would_block, "an idle socket must report WouldBlock, not an error");
        assert!(!p.is_done(), "and the request must still be pending");

        // Now the peer answers; the same object must pick up where it left off.
        server.write_all(line(&done(0)).as_bytes()).unwrap();
        server.flush().unwrap();
        let (_, last) = pump_until_done(&mut p, &mut server, 50);
        assert_eq!(last, Progress::Finished, "resumable across ticks");
        assert_eq!(p.records().len(), 1);
    }

    #[test]
    fn the_request_line_is_written_before_reading() {
        let (_d, client, mut server) = pair();
        let mut p = PendingRequest::start(client, "{\"op\":\"ping\"}").unwrap();
        // Nothing is sent merely by constructing the request: the socket may not be
        // writable yet, so the bytes are buffered until `advance` can flush them.
        // (This is why the write must be resumable and not one `writeln!`.)
        let _ = p.advance().unwrap();

        // The peer should now receive the request line, newline-terminated.
        use std::io::Read;
        server.set_nonblocking(true).unwrap();
        let mut got = Vec::new();
        for _ in 0..100 {
            let mut buf = [0u8; 64];
            match server.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    if got.ends_with(b"\n") {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(e) => panic!("read: {e}"),
            }
        }
        assert!(!got.is_empty(), "the request line must actually be sent");
        assert!(got.ends_with(b"\n"), "NDJSON: newline-terminated, got {got:?}");
        assert!(
            got.starts_with(b"{\"op\":\"ping\"}"),
            "and it must be the request we asked for, got {got:?}"
        );
    }

    #[test]
    fn a_clean_eof_after_the_terminal_is_not_an_error() {
        let (_d, client, mut server) = pair();
        let mut p = PendingRequest::start(client, "{\"op\":\"ping\"}").unwrap();
        server.write_all(line(&pong()).as_bytes()).unwrap();
        server.flush().unwrap();
        // The daemon closes once it has answered — a normal ending.
        server.shutdown(Shutdown::Write).unwrap();

        let (_, last) = pump_until_done(&mut p, &mut server, 50);
        assert_eq!(last, Progress::Finished);
        assert_eq!(p.records().len(), 1);
    }

    #[test]
    fn an_eof_before_the_terminal_is_a_loud_error() {
        let (_d, client, mut server) = pair();
        let mut p = PendingRequest::start(client, "{\"op\":\"ping\"}").unwrap();
        // A truncated reply: a Meta but no terminal record.
        server.write_all(line(&meta()).as_bytes()).unwrap();
        server.flush().unwrap();
        server.shutdown(Shutdown::Write).unwrap();

        let mut err = None;
        for _ in 0..50 {
            match p.advance() {
                Ok(Progress::Finished) => break,
                Ok(_) => {}
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let err = err.expect("a truncated reply must be reported, not silently accepted");
        assert!(
            format!("{err}").contains("before the reply finished"),
            "got: {err}"
        );
    }

    #[test]
    fn a_malformed_record_is_a_protocol_error() {
        let (_d, client, mut server) = pair();
        let mut p = PendingRequest::start(client, "{\"op\":\"ping\"}").unwrap();
        server.write_all(b"this is not json\n").unwrap();
        server.flush().unwrap();

        let mut err = None;
        for _ in 0..20 {
            if let Err(e) = p.advance() {
                err = Some(e);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let err = err.expect("garbage must not be silently dropped");
        assert!(matches!(err, GuiError::Protocol(_)), "got: {err:?}");
    }

    #[test]
    fn advance_after_completion_is_idempotent() {
        let (_d, client, mut server) = pair();
        let mut p = PendingRequest::start(client, "{\"op\":\"ping\"}").unwrap();
        server.write_all(line(&done(0)).as_bytes()).unwrap();
        server.flush().unwrap();
        let _ = pump_until_done(&mut p, &mut server, 50);
        assert_eq!(p.advance().unwrap(), Progress::Finished);
        assert_eq!(p.advance().unwrap(), Progress::Finished, "still Finished");
        assert_eq!(p.records().len(), 1, "records must not be duplicated");
    }

    #[test]
    fn a_large_request_line_survives_a_partial_write() {
        // A long expression query is bigger than one socket buffer flush in the
        // worst case; the writer must track its offset, not assume one write.
        let (_d, client, mut server) = pair();
        let big = format!("{{\"op\":\"query\",\"q\":\"{}\"}}", "x".repeat(200_000));
        let mut p = PendingRequest::start(client, &big).unwrap();
        let mut saw_block = false;
        for _ in 0..2000 {
            match p.advance().unwrap() {
                Progress::WouldBlock => {
                    // Drain the peer so the kernel buffer can accept more.
                    use std::io::Read;
                    server.set_nonblocking(true).unwrap();
                    let mut sink = [0u8; 65536];
                    while let Ok(n) = server.read(&mut sink) {
                        if n == 0 {
                            break;
                        }
                    }
                    saw_block = true;
                }
                Progress::Advanced => {}
                Progress::Finished => break,
            }
        }
        assert!(saw_block, "a 200 KB line should not fit in one write");
        server.write_all(line(&done(0)).as_bytes()).unwrap();
        server.flush().unwrap();
        let (_, last) = pump_until_done(&mut p, &mut server, 100);
        assert_eq!(last, Progress::Finished, "the whole line must still be sent");
    }
}
