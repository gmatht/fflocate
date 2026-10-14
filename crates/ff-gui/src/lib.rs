//! `ff-gui` — a file-search browser for the fflocate daemon, built on
//! [rustxWidgets].
//!
//! The GUI talks to `fflocated` over the same NDJSON protocol the CLI uses
//! (`fflocate::proto`, unix sockets on unix, TCP loopback on Windows): a query
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

use fflocate::config_paths::socket_path_from_env;
use fflocate::proto::{Request, Response};

/// Re-exported because `PendingRequest::records` hands these back to the GUI.
pub use fflocate::proto::Response as WireResponse;

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
            GuiError::Connect(m) => write!(f, "cannot reach fflocated: {m}"),
            GuiError::Io(m) => write!(f, "io: {m}"),
            GuiError::Protocol(m) => write!(f, "protocol: {m}"),
            GuiError::Backend(m) => write!(f, "widget backend: {m}"),
        }
    }
}

/// A connection to the daemon. Each call opens its own stream: the protocol is
/// request/response, and `fflocated` closes a client after the terminal message,
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
    /// Locate the daemon socket (honouring `$FFLOCATE_SOCKET`).
    pub fn new() -> DaemonClient {
        DaemonClient {
            socket: socket_path_from_env(),
        }
    }

    pub fn with_socket(socket: PathBuf) -> DaemonClient {
        DaemonClient { socket }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Send one request, collect the response lines until the terminal message.
    pub fn request(&self, req: &Request) -> Result<Vec<Response>, GuiError> {
        let stream = fflocate::transport::connect(&self.socket)
            .map_err(|e| GuiError::Connect(format!("{}: {e}", self.socket.display())))?;
        let mut writer = stream
            .try_clone()
            .map_err(|e| GuiError::Io(e.to_string()))?;
        // Stamp the protocol version. A daemon built before a behaviour change
        // rejects the request rather than silently ignoring a field it does not
        // understand (that is how a stale daemon made the Case-sensitive
        // checkbox look broken).
        let mut v: serde_json::Value =
            serde_json::to_value(req).map_err(|e| GuiError::Protocol(e.to_string()))?;
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "v".into(),
                serde_json::Value::from(fflocate::proto::PROTOCOL_VERSION),
            );
        }
        let line = serde_json::to_string(&v).map_err(|e| GuiError::Protocol(e.to_string()))?;
        // FFLOCATE_DEBUG=1 prints each request, so a setting that "does nothing"
        // can be traced to what was actually sent (e.g. whether case_sensitive
        // was included at all).
        if std::env::var_os("FFLOCATE_DEBUG").is_some() {
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
            let resp: Response = serde_json::from_str(&line)
                .map_err(|e| GuiError::Protocol(format!("{e}: {line}")))?;
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
            // The GUI renders scan-fraction percent from these; `ff` leaves
            // it off (its output must stay byte-identical).
            progress: true,
        }
    }

    /// Turn a complete set of response records into a [`SearchOutcome`].
    ///
    /// Shared by the blocking `search_with` and the non-blocking GUI path, so the
    /// two cannot interpret a reply differently.
    pub fn outcome_from_records(&self, resp: &[Response]) -> Result<SearchOutcome, GuiError> {
        let mut paths = Vec::new();
        let mut total = 0u64;
        let mut fresh = true;
        let mut stale: Vec<String> = Vec::new();
        let mut catching_up = false;
        let mut lag_ms: Option<u64> = None;
        let mut elapsed_ms: Option<f64> = None;
        for r in resp {
            match r {
                Response::Meta {
                    fresh: f,
                    stale_roots,
                    catching_up: c,
                    lag_ms: l,
                    elapsed_ms: e,
                    ..
                } => {
                    fresh = *f;
                    stale = stale_roots.clone();
                    catching_up = c.unwrap_or(false);
                    lag_ms = *l;
                    elapsed_ms = Some(*e);
                }
                Response::Hit { path, .. } => paths.push(path.clone()),
                Response::Done { count, .. } => total = *count,
                Response::Error { message } => {
                    // A version rejection means a *stale daemon* is running:
                    // say so and how to fix it, rather than surfacing a bare
                    // protocol string that looks like a bug in the GUI.
                    if message.contains("unsupported protocol version") {
                        return Err(GuiError::Protocol(format!(
                            "{message} — the running fflocated predates this build; \
                             restart it (fflocated reads --socket/--state-dir, or \
                             kill {} and let `ff` respawn one)",
                            self.socket.display()
                        )));
                    }
                    return Err(GuiError::Protocol(message.clone()));
                }
                _ => {}
            }
        }
        Ok(SearchOutcome {
            paths,
            total,
            fresh,
            stale_roots: stale,
            catching_up,
            lag_ms,
            elapsed_ms,
        })
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

    /// Poll `hello()` until a freshly spawned daemon answers or `grace`
    /// elapses. Used once at startup after spawning: the daemon probes mounts
    /// and starts threads before binding, so its socket typically appears a
    /// moment after the child exists. Bounded, so a daemon that never comes
    /// up cannot stall startup for longer than `grace`.
    pub fn wait_for_hello(&self, grace: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            if self.hello() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        self.hello()
    }

    /// The startup status line for a reachable daemon: where it lives and how
    /// much it holds. Pure so the deferred startup probe and the tests agree
    /// on the wording without needing a daemon.
    pub fn connected_status_line(socket: &Path, stats: &serde_json::Value) -> String {
        let entries = stats.get("entries").and_then(|v| v.as_u64()).unwrap_or(0);
        let backend = stats.get("backend").and_then(|v| v.as_str()).unwrap_or("?");
        format!(
            "connected to fflocated at {} ({entries} entries, {backend})",
            socket.display()
        )
    }

    /// Build the `rescan` request for one root. Pure like `query_request`, so
    /// the request shape is testable without a daemon.
    pub fn rescan_request(root: &str) -> Request {
        Request::Rescan {
            root: root.to_string(),
        }
    }

    /// Ask the daemon to rescan one root; waits for the acknowledgement.
    pub fn rescan(&self, root: &str) -> Result<(), GuiError> {
        let resp = self.request(&Self::rescan_request(root))?;
        for r in &resp {
            match r {
                Response::Rescanning { .. } => return Ok(()),
                Response::Error { message } => return Err(GuiError::Protocol(message.clone())),
                _ => {}
            }
        }
        Err(GuiError::Protocol("rescan got no acknowledgement".into()))
    }

    /// Root paths the daemon indexes, from `stats` with roots enabled.
    pub fn root_paths(&self) -> Result<Vec<String>, GuiError> {
        let resp = self.request(&Request::Stats { roots: true })?;
        for r in &resp {
            if let Response::Stats { stats } = r {
                return Ok(parse_root_paths(stats));
            }
        }
        Ok(Vec::new())
    }
}

/// What one [`StartupProbe::poll`] step reports.
#[derive(Debug, PartialEq, Eq)]
pub enum StartupPoll {
    /// Nothing new; call again on the next tick.
    Pending,
    /// Set the status line to this, but keep polling (e.g. "still starting"
    /// while a freshly spawned daemon comes up).
    Note(String),
    /// Set the status line to this; the probe is finished ([`StartupProbe::is_done`]).
    Done(String),
}

/// The deferred startup connection check: "is the daemon there, and what does
/// it hold", answered without ever blocking the event loop.
///
/// `main.rs` used to run this *before* presenting the window — two sequential
/// blocking round-trips (`hello`, then `stats`) on the warm path, and a
/// `spawn_daemon` plus a blocking up-to-5 s `wait_for_hello` poll when the
/// daemon was missing. Every millisecond of that was a blank screen. The probe
/// instead rides the 30 ms timer pump: the window presents first with a
/// "connecting…" status, and the first pump ticks settle it.
///
/// The `stats` request alone proves the daemon is alive, so there is no
/// `hello` round-trip here at all. The request goes through [`PendingRequest`],
/// the same resumable non-blocking machine queries use, so a wedged daemon
/// costs one `WouldBlock` per tick rather than a frozen window. A single
/// overall deadline bounds the whole probe (spawn wait included): past it the
/// status says the daemon is still starting rather than hanging forever.
///
/// `allow_spawn` mirrors the old behaviour (spawn once when missing); tests
/// pass `false` so they never exec a daemon.
pub struct StartupProbe {
    socket: PathBuf,
    allow_spawn: bool,
    spawned: bool,
    announced_spawn: bool,
    pending: Option<PendingRequest>,
    deadline: std::time::Instant,
    finished: bool,
    /// When this process spawned the daemon (None if it was already running).
    /// Lets the in-flight line say "starting daemon…" with elapsed time
    /// instead of a bare "searching…", so a cold-started daemon reads as
    /// progress from the first keystroke.
    spawned_at: Option<std::time::Instant>,
}

impl StartupProbe {
    pub fn new(socket: PathBuf, grace: std::time::Duration, allow_spawn: bool) -> StartupProbe {
        StartupProbe {
            socket,
            allow_spawn,
            spawned: false,
            announced_spawn: false,
            pending: None,
            deadline: std::time::Instant::now() + grace,
            finished: false,
            spawned_at: None,
        }
    }

    /// True once this process spawned the daemon during this probe run.
    /// Sticky: stays true after the probe finishes, so the pump can keep
    /// reporting the cold-start case for the queries that follow.
    pub fn was_spawned(&self) -> bool {
        self.spawned
    }

    /// How long ago this process spawned the daemon, if it did.
    pub fn spawned_elapsed(&self) -> Option<std::time::Duration> {
        self.spawned_at.map(|t| t.elapsed())
    }

    /// True once [`poll`](Self::poll) has returned [`StartupPoll::Done`].
    pub fn is_done(&self) -> bool {
        self.finished
    }

    fn done(&mut self, line: String) -> StartupPoll {
        self.finished = true;
        StartupPoll::Done(line)
    }

    fn no_daemon_line(&self) -> String {
        format!(
            "no daemon at {} — start fflocated and search again",
            self.socket.display()
        )
    }

    fn still_starting_line(&self) -> String {
        // Same wording the blocking startup path used, so logs and muscle
        // memory keep matching.
        format!(
            "spawned fflocated at {} — still starting; search again shortly",
            self.socket.display()
        )
    }

    /// Make whatever progress is possible without blocking. Safe to call every
    /// tick; stop once [`is_done`](Self::is_done) is true.
    pub fn poll(&mut self) -> StartupPoll {
        if self.finished {
            return StartupPoll::Pending;
        }
        if std::time::Instant::now() >= self.deadline {
            // Bounded, whatever the daemon is doing: accepting and never
            // replying, or spawned but never binding.
            let line = if self.spawned {
                self.still_starting_line()
            } else {
                format!(
                    "fflocated at {} — not responding; search again shortly",
                    self.socket.display()
                )
            };
            return self.done(line);
        }
        if self.pending.is_none() {
            // (Re)connect. A unix-socket `connect` to an absent path fails
            // immediately (ENOENT) rather than blocking; to a live daemon it
            // succeeds at once. Either way this does not stall the pump.
            let line = match encode_request(&Request::Stats { roots: false }) {
                Ok(l) => l,
                Err(e) => return self.done(format!("{e}")),
            };
            match fflocate::transport::connect(&self.socket)
                .map_err(|e| GuiError::Connect(format!("{}: {e}", self.socket.display())))
                .and_then(|stream| PendingRequest::start(stream, &line))
            {
                Ok(p) => self.pending = Some(p),
                Err(_) => {
                    if self.allow_spawn && !self.spawned {
                        // Same deaf-daemon fail-fast as the `ff` client: any
                        // connect failure against a live lock-holder means the
                        // holder serves nothing — spawning can never help (the
                        // child just exits "already running") and polling to
                        // the deadline only hides the cause. The lock probe is
                        // cheap and definitive, so no errno matching is needed.
                        let pidfile = fflocate::spawn::pidfile_for_socket(&self.socket);
                        if fflocate::spawn::daemon_lock_held(&pidfile) {
                            return self
                                .done(fflocate::spawn::deaf_daemon_message(&self.socket, &pidfile));
                        }
                        match fflocate::spawn::spawn_daemon(&self.socket) {
                            Ok(()) => {
                                self.spawned = true;
                                self.spawned_at = Some(std::time::Instant::now());
                                // fall through to return the Note below
                            }
                            Err(e) => {
                                return self.done(format!(
                                    "no daemon at {} — could not spawn fflocated: {e}",
                                    self.socket.display()
                                ));
                            }
                        }
                    } else if self.spawned {
                        // Spawned earlier, still no socket: keep polling until
                        // the deadline. The spawn announcement already set the
                        // status line, so no churn.
                        return StartupPoll::Pending;
                    } else {
                        return self.done(self.no_daemon_line());
                    }
                    if self.spawned && !self.announced_spawn {
                        self.announced_spawn = true;
                        return StartupPoll::Note(self.still_starting_line());
                    }
                    return StartupPoll::Pending;
                }
            }
        }
        // Advance the in-flight stats request. `WouldBlock` is the normal
        // case (come back next tick); an error drops the attempt and retries
        // from `connect` next tick, still bounded by the deadline.
        let finished = match self.pending.as_mut().map(|p| p.advance()) {
            Some(Ok(Progress::Finished)) => true,
            Some(Ok(_)) => false,
            Some(Err(_)) | None => {
                self.pending = None;
                return StartupPoll::Pending;
            }
        };
        if !finished {
            return StartupPoll::Pending;
        }
        let recs = self
            .pending
            .as_mut()
            .map(|p| p.take_records())
            .unwrap_or_default();
        self.pending = None;
        for r in &recs {
            match r {
                Response::Stats { stats } => {
                    let line = DaemonClient::connected_status_line(&self.socket, stats);
                    return self.done(line);
                }
                Response::Error { message } => {
                    return self.done(format!("{}", GuiError::Protocol(message.clone())));
                }
                _ => {}
            }
        }
        // The daemon answered something without a terminal stats record
        // (it closes after the terminal, so this is EOF-shaped weirdness):
        // retry next tick rather than declaring anything.
        StartupPoll::Pending
    }
}

/// Pull root paths out of a stats payload. Pure so the shape is testable
/// without a daemon; unknown shapes yield no roots, never a panic.
pub fn parse_root_paths(stats: &serde_json::Value) -> Vec<String> {
    stats
        .get("roots")
        .and_then(|v| v.as_array())
        .map(|roots| {
            roots
                .iter()
                .filter_map(|r| r.get("path").and_then(|v| v.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// One menubar item: what it says and what it does. The model is data so the
/// bar's contents are unit-testable headless; `main.rs` wires each action to
/// the same closures the buttons, checkboxes and context menu already use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    SearchNow,
    FocusQuery,
    ClearQuery,
    ToggleCase,
    ToggleFuzzy,
    ToggleCount,
    ToggleAdvanced,
    FilterAudio,
    FilterCompressed,
    FilterExecutable,
    FilterFolder,
    FilterPicture,
    FilterVideo,
    OpenContainingFolder,
    CopyPath,
    RefreshIndex,
    ShowSyntax,
    ShowDaemonStatus,
    ShowAbout,
    Quit,
}

impl MenuAction {
    /// Bare action name for `new_simple_action`; the menu model references it
    /// as `app.<name>`, which is also the form the NWG backend resolves.
    pub fn action_name(&self) -> &'static str {
        match self {
            MenuAction::SearchNow => "search-now",
            MenuAction::FocusQuery => "focus-query",
            MenuAction::ClearQuery => "clear-query",
            MenuAction::ToggleCase => "toggle-case",
            MenuAction::ToggleFuzzy => "toggle-fuzzy",
            MenuAction::ToggleCount => "toggle-count",
            MenuAction::ToggleAdvanced => "toggle-advanced",
            MenuAction::OpenContainingFolder => "open-containing-folder",
            MenuAction::CopyPath => "copy-path",
            MenuAction::RefreshIndex => "refresh-index",
            MenuAction::ShowSyntax => "show-syntax",
            MenuAction::ShowDaemonStatus => "show-daemon-status",
            MenuAction::ShowAbout => "show-about",
            MenuAction::Quit => "quit",
            MenuAction::FilterAudio => "filter-audio",
            MenuAction::FilterCompressed => "filter-compressed",
            MenuAction::FilterExecutable => "filter-executable",
            MenuAction::FilterFolder => "filter-folder",
            MenuAction::FilterPicture => "filter-picture",
            MenuAction::FilterVideo => "filter-video",
        }
    }

    /// The query a type-filter item puts in the search box (which the caller
    /// then runs): extension sets as one anchored, case-insensitive regex —
    /// the string language ANDs terms, so `ext:a ext:b` could never mean OR —
    /// and `type:dir` for folders, which only the engine can answer. The
    /// executable set is extension-based (the index carries no permission
    /// bits): an honest approximation, not a promise.
    pub fn preset_query(&self) -> Option<&'static str> {
        match self {
            MenuAction::FilterAudio => Some(
                r"regex:(?i)\.(mp3|wav|flac|ogg|oga|opus|m4a|aac|wma|aiff|aif|alac|ape|mid|midi|amr)$",
            ),
            MenuAction::FilterCompressed => Some(
                r"regex:(?i)\.(zip|tar|gz|tgz|bz2|tbz|xz|txz|7z|rar|cab|lz4|zst|zstd|deb|rpm|pkg|dmg|iso|img)$",
            ),
            MenuAction::FilterExecutable => Some(
                r"regex:(?i)\.(exe|msi|bat|cmd|com|ps1|sh|bash|zsh|fish|py|pyw|pl|rb|bin|run|elf|appimage|msc)$",
            ),
            MenuAction::FilterFolder => Some("type:dir"),
            MenuAction::FilterPicture => Some(
                r"regex:(?i)\.(jpg|jpeg|png|gif|bmp|svg|webp|tiff|tif|ico|heic|heif|avif|psd|xcf|dds|tga)$",
            ),
            MenuAction::FilterVideo => Some(
                r"regex:(?i)\.(mp4|mkv|avi|mov|webm|flv|wmv|mpg|mpeg|m4v|3gp|ts|m2ts|vob|ogv)$",
            ),
            _ => None,
        }
    }
}

/// One row of the menubar model: menu label + `(item label, action)` pairs.
pub fn menubar_model() -> Vec<(&'static str, Vec<(&'static str, MenuAction)>)> {
    vec![
        (
            "File",
            vec![
                ("Open Containing Folder", MenuAction::OpenContainingFolder),
                ("Copy Path", MenuAction::CopyPath),
                ("Refresh Index", MenuAction::RefreshIndex),
                ("Quit", MenuAction::Quit),
            ],
        ),
        (
            "Search",
            vec![
                ("Search Now", MenuAction::SearchNow),
                ("Focus Query", MenuAction::FocusQuery),
                ("Clear Query", MenuAction::ClearQuery),
                ("Case Sensitive", MenuAction::ToggleCase),
                ("Fuzzy Matching", MenuAction::ToggleFuzzy),
                ("Count Only", MenuAction::ToggleCount),
                ("Audio Files", MenuAction::FilterAudio),
                ("Compressed Files", MenuAction::FilterCompressed),
                ("Executable Files", MenuAction::FilterExecutable),
                ("Folders", MenuAction::FilterFolder),
                ("Picture Files", MenuAction::FilterPicture),
                ("Video Files", MenuAction::FilterVideo),
            ],
        ),
        (
            "View",
            vec![("Advanced Search", MenuAction::ToggleAdvanced)],
        ),
        (
            "Help",
            vec![
                ("Query Syntax", MenuAction::ShowSyntax),
                ("Daemon Status", MenuAction::ShowDaemonStatus),
                ("About", MenuAction::ShowAbout),
            ],
        ),
    ]
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
    /// How far the daemon is behind the filesystem: the age of its oldest
    /// unapplied change, as reported in `Meta.lag_ms`. `None` when the daemon
    /// is caught up (or is old enough not to report it). Shown in seconds so
    /// "catching up" says *how far* behind, not just that it is.
    pub lag_ms: Option<u64>,
    /// What the daemon itself reported in `Meta.elapsed_ms`: query + trigram
    /// build + lock waits, server-side. Shown when slow so a first search
    /// that took seconds explains itself instead of looking instant-but-late.
    pub elapsed_ms: Option<f64>,
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
        if let Some(suffix) = catching_up_suffix(self.catching_up, self.lag_ms) {
            s.push_str(&suffix);
        }
        if !self.stale_roots.is_empty() {
            s.push_str(&format!("  [stale roots: {}]", self.stale_roots.join(", ")));
        }
        // A first search against a cold daemon (crawl + trigram build) can
        // take seconds server-side while the status line sat on "searching…".
        // Naming the cost when it is big explains the wait after the fact;
        // under a second the line stays quiet.
        if let Some(ms) = self.elapsed_ms {
            if ms >= 1000.0 {
                s.push_str(&format!("  [{:.1}s server]", ms / 1000.0));
            }
        }
        s
    }
}

/// The in-flight line when this process just spawned the daemon.
///
/// Same shape as [`searching_status_line`] but names the cause: the daemon is
/// cold (crawling, warming its trigram index) rather than merely slow. Used
/// while [`StartupProbe::was_spawned`] is true so the first queries after a
/// spawn read as startup, not a hang.
pub fn starting_status_line(
    query: &str,
    elapsed: std::time::Duration,
    daemon_elapsed: Option<std::time::Duration>,
    hits_so_far: usize,
) -> String {
    let show = daemon_elapsed.unwrap_or(elapsed);
    let mut s = if query.is_empty() {
        format!("starting fflocated… {:.1}s", show.as_secs_f64())
    } else {
        format!("starting fflocated… {:.1}s (searching for {query})", show.as_secs_f64())
    };
    if hits_so_far > 0 {
        s.push_str(&format!(
            " ({} hit{} so far)",
            hits_so_far,
            if hits_so_far == 1 { "" } else { "s" }
        ));
    }
    s
}

/// The `[daemon catching up…]` suffix for a status line, in seconds.
///
/// `lag_ms` is the daemon's own age for its oldest unapplied change, so the
/// number answers "*how far* behind is it" rather than merely that it is
/// behind. Three shapes:
///
/// * `catching_up` with an age → `[daemon catching up, 4s behind]`
/// * `catching_up` with no age (old daemon, or the age was too small to
///   measure) → `[daemon catching up]` — the honest wording from before
/// * not catching up → nothing at all
///
/// Seconds are rounded *down* to whole seconds: a sub-second lag rounds to
/// `0s behind`, which reads as "essentially caught up" rather than inventing
/// precision. Sub-second lags are reported as plain `[daemon catching up]`
/// instead, so a number never claims a granularity the clock does not have.
pub fn catching_up_suffix(catching_up: bool, lag_ms: Option<u64>) -> Option<String> {
    if !catching_up {
        return None;
    }
    match lag_ms {
        Some(ms) if ms >= 1000 => Some(format!("  [daemon catching up, {}s behind]", ms / 1000)),
        _ => Some("  [daemon catching up]".to_string()),
    }
}

/// Scan-fraction percent from a daemon `Progress` record: `scanned` of
/// `total` entries visited.
///
/// `None` when there is nothing to show: no Progress records yet (old
/// daemon, fast query, or the denominator announcement not yet arrived) or
/// a vacuous `total == 0`. Otherwise `0..=99` — capped at 99, because 100%
/// is `Done`'s privilege: a "100%" that sits while the total is still being
/// counted would be the same lie the streaming line was built to avoid.
/// Monotonicity across records is the caller's watermark, not this helper's.
pub fn scan_percent(scanned: u64, total: u64) -> Option<u8> {
    if total == 0 {
        return None;
    }
    let pct = (scanned as u128 * 100 / total as u128).min(99) as u8;
    Some(pct)
}

/// The status line while a request is in flight (no reply yet).
///
/// The daemon sends nothing until the query engine finishes — `Meta` is
/// computed after the scan, and the socket is flushed once — so a slow query
/// (a cold trigram index on first search, a daemon still crawling) looks
/// identical to a wedged daemon until the reply lands. This line is the honest
/// signal: the request left, the answer has not come back, and here is how
/// long that has been true. `hits_so_far` counts `Hit` records parsed from
/// partial packets (zero until the daemon flushes mid-reply, which it
/// currently does not — the count is future-proofing, not fiction).
pub fn searching_status_line(
    query: &str,
    elapsed: std::time::Duration,
    hits_so_far: usize,
    catching_up: bool,
    progress: Option<u8>,
    lag_ms: Option<u64>,
) -> String {
    let mut s = if query.is_empty() {
        format!("searching… {:.1}s", elapsed.as_secs_f64())
    } else {
        format!("searching for {query}… {:.1}s", elapsed.as_secs_f64())
    };
    if let Some(pct) = progress {
        s.push_str(&format!(" {pct}%"));
    }
    if hits_so_far > 0 {
        s.push_str(&format!(
            " ({} hit{} so far)",
            hits_so_far,
            if hits_so_far == 1 { "" } else { "s" }
        ));
    }
    if let Some(suffix) = catching_up_suffix(catching_up, lag_ms) {
        s.push_str(&suffix);
    }
    s
}

/// The status line while Hits stream in but `Done` (exact total) has not
/// landed yet. Hits are real rows on screen; the total is still being counted,
/// so the line says so — "201 hits… (counting total… 1.2s)" — rather than
/// claiming a total it does not have. Once `Done` arrives the pump renders the
/// final [`SearchOutcome::status_line_opts`] line as before.
pub fn streaming_status_line(
    hits_shown: usize,
    elapsed: std::time::Duration,
    progress: Option<u8>,
) -> String {
    let mut s = format!(
        "{} hit{}… (counting total… {:.1}s",
        hits_shown,
        if hits_shown == 1 { "" } else { "s" },
        elapsed.as_secs_f64()
    );
    if let Some(pct) = progress {
        s.push_str(&format!(", {pct}%"));
    }
    s.push(')');
    s
}

/// A short human label for the query in the two boxes, for the in-flight line.
///
/// Unlike [`effective_query`] this is display-only: no quoting, no `None` —
/// an unquotable Basic box still shows its raw text (the search itself will
/// report the error). Truncated to 40 characters so a pasted paragraph does
/// not blow out the status bar.
pub fn search_label(quick: &str, advanced: &str) -> String {
    const MAX_CHARS: usize = 40;
    let q = quick.trim();
    let a = advanced.trim();
    let combined = match (q.is_empty(), a.is_empty()) {
        (false, false) => format!("{q} {a}"),
        (false, true) => q.to_string(),
        (_, false) => a.to_string(),
        (true, true) => String::new(),
    };
    if combined.chars().count() <= MAX_CHARS {
        combined
    } else {
        let mut s: String = combined.chars().take(MAX_CHARS - 1).collect();
        s.push('…');
        s
    }
}

/// Quote Basic-box text so the engine reads it as one literal substring.
///
/// The engine disables modifiers (`ext:`/`path:`/`case:`/`regex:`/`type:`),
/// globs (`*`/`?`) and the `-` negation prefix inside `"..."` — quoting is
/// what makes the Basic box literal-only while the Advanced box keeps the
/// full language. Smart-case still applies to the needle (`Makefile` stays
/// case-sensitive), exactly as for any quoted phrase in `ff`.
///
/// There is no escape inside quotes: a `"` always terminates the phrase. A
/// name containing `"` therefore cannot be quoted — `None` tells the caller
/// to refuse with a nudge toward the Advanced box rather than send a mangled
/// query. Empty/whitespace input quotes to itself (`""`), which the caller
/// never sends (`query_is_searchable` filters it first).
pub fn quote_literal(text: &str) -> Option<String> {
    if text.contains('"') {
        return None;
    }
    Some(format!("\"{text}\""))
}

/// The query the daemon should actually run, built from the quick box and the
/// advanced foldout.
///
/// The Basic box is literal-only: its contents are quoted (see
/// [`quote_literal`]) so `ext:rs` in Basic searches for those exact
/// characters, while the Advanced box speaks the full language. When both
/// boxes hold text the two are ANDed — the engine ANDs space-separated terms
/// natively, so `"basic" ext:rs` needs the file to match both. The foldout's
/// contents pass through untouched, so the effective query stays one thing
/// the user can read, copy, and paste into `ff`.
///
/// `None` when the Basic text contains a `"` it cannot survive quoting —
/// the caller must refuse and point at the Advanced box.
pub fn effective_query(quick: &str, advanced: &str) -> Option<String> {
    let quick = quick.trim();
    let advanced = advanced.trim();
    match (quick.is_empty(), advanced.is_empty()) {
        (false, false) => quote_literal(quick).map(|q| format!("{q} {advanced}")),
        (false, true) => quote_literal(quick),
        (_, false) => Some(advanced.to_string()),
        (true, true) => Some(String::new()),
    }
}

/// Whether an edit to either box should clear the list at once instead of
/// searching.
///
/// True only when the *combined* query is empty — both boxes blank. A box
/// emptied while the other still holds text keeps searching (the list shows
/// only entries matching both boxes, i.e. the AND in [`effective_query`]),
/// and unquotable Basic text (`None`) counts as searchable so the caller
/// arms a request and surfaces `build_request`'s error instead of silently
/// clearing.
pub fn box_change_clears(quick: &str, advanced: &str) -> bool {
    match effective_query(quick, advanced) {
        Some(q) => !query_is_searchable(&q),
        // Unquotable: do not clear; let the search report it.
        None => false,
    }
}

/// The max edit distance the foldout offers before the user changes it.
/// Small numbers keep fuzzy matching useful rather than noisy.
pub const DEFAULT_MAX_DISTANCE: &str = "2";

/// The same default as a number, for stepping the field without parsing the
/// string constant every time.
pub const DEFAULT_MAX_DISTANCE_VALUE: u32 = 2;

/// Wheel-stepping the max-distance field saturates here: beyond this fuzzy
/// matching is noise, and an unbounded field invites runaway queries.
pub const MAX_FUZZY_DISTANCE: u32 = 99;

/// Wheel direction for stepping a numeric field: up increments, down decrements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelStep {
    Up,
    Down,
}

/// Step the fuzzy max-distance field by one wheel notch.
///
/// The field holds a plain number; a blank or unparseable field counts as the
/// default (`DEFAULT_MAX_DISTANCE_VALUE`) so wheeling from an empty field
/// lands on a concrete value instead of doing nothing. Steps saturate at 0
/// and [`MAX_FUZZY_DISTANCE`].
pub fn step_max_distance(text: &str, step: WheelStep) -> String {
    let current = text
        .trim()
        .parse::<u32>()
        .unwrap_or(DEFAULT_MAX_DISTANCE_VALUE);
    let next = match step {
        WheelStep::Up => current.saturating_add(1).min(MAX_FUZZY_DISTANCE),
        WheelStep::Down => current.saturating_sub(1),
    };
    next.to_string()
}

/// A one-line reminder of the advanced syntax, shown inside the foldout.
pub const ADVANCED_SYNTAX_HELP: &str = "ext:rs  path:src  case:Foo  regex:^m.*\\.rs  type:dir  \"exact phrase\"  *.rs  term1 term2 (all must match)";

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
            | b'-'
            | b'.'
            | b'_'
            | b'~'
            | b'/'
            | b'!'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b';'
            | b'='
            | b':'
            | b'@' => out.push(b as char),
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
    /// Copy the path of the directory containing the row's file.
    CopyFolderPath,
    /// Open the row's file with the desktop's default handler.
    Open,
    /// Choose an application to open the row's file with.
    OpenWith,
    /// Open the directory containing the row's file.
    OpenContainingFolder,
    /// Open a terminal in the directory containing the row's file.
    OpenInTerminal,
}

impl RowAction {
    /// The menu label.
    pub fn label(self) -> &'static str {
        match self {
            RowAction::CopyPath => "Copy full path",
            RowAction::CopyFolderPath => "Copy folder path",
            RowAction::Open => "Open",
            RowAction::OpenWith => "Open with…",
            RowAction::OpenContainingFolder => "Open containing folder",
            RowAction::OpenInTerminal => "Open in terminal",
        }
    }

    /// Whether this action is handled in-process (the clipboard) rather than by
    /// spawning a command.
    pub fn is_clipboard(self) -> bool {
        matches!(self, RowAction::CopyPath | RowAction::CopyFolderPath)
    }

    /// The program and arguments to run for this action on `path`.
    ///
    /// Clipboard actions return an empty program: the caller copies in-process.
    /// The rest spawn an external helper. Windows arms use the shell's own
    /// verbs (`start`, Explorer, the Open-With dialog): there is no xdg-open
    /// there, and spawning a unix helper name would fail at `spawn` time with
    /// a message that reads as a bug rather than a missing port.
    pub fn command(self, path: &str) -> (&'static str, Vec<String>) {
        #[cfg(windows)]
        {
            return match self {
                // Handled in-process.
                RowAction::CopyPath | RowAction::CopyFolderPath => ("", Vec::new()),
                // `start "" <file>`: the empty title swallows the quoted
                // path (start treats the first quoted word as a window
                // title), and the shell dispatches to the default handler.
                RowAction::Open => (
                    "cmd",
                    vec!["/c".into(), "start".into(), "".into(), path.to_string()],
                ),
                // Explorer with /select highlights the file in its folder.
                RowAction::OpenContainingFolder => ("explorer", vec![format!("/select,{}", path)]),
                // The system Open-With chooser (no default change).
                RowAction::OpenWith => (
                    "rundll32",
                    vec!["shell32.dll,OpenAs_RunDLL".into(), path.to_string()],
                ),
                // A console in the file's directory.
                RowAction::OpenInTerminal => (
                    "cmd",
                    vec![
                        "/c".into(),
                        "start".into(),
                        "".into(),
                        "/d".into(),
                        containing_dir(path),
                    ],
                ),
            };
        }
        #[cfg(not(windows))]
        match self {
            // Handled in-process.
            RowAction::CopyPath | RowAction::CopyFolderPath => ("", Vec::new()),
            // xdg-open dispatches on a file or a directory identically.
            RowAction::Open => ("xdg-open", vec![path.to_string()]),
            RowAction::OpenContainingFolder => ("xdg-open", vec![containing_dir(path)]),
            // `mimeopen -a` lists the applications that handle the file and asks
            // which to use, without changing the default handler -- i.e. an "open
            // with" chooser. It ships with File-MimeInfo; where it is absent the
            // caller falls back to the default application.
            RowAction::OpenWith => ("mimeopen", vec!["-a".into(), path.to_string()]),
            // x-terminal-emulator is the Debian alternatives link that every
            // terminal package registers; it takes a working directory via
            // `-e`/`--working-directory` inconsistently, so `cd` in a shell is
            // the portable form. The directory is shell-quoted: paths come
            // from the index and may hold spaces, quotes, or `$`.
            //
            // These are the *base* args: the caller uses
            // [`open_in_terminal_command`], which splices `-font` in front
            // (zutty needs it when its `9x18` default is not installed). Kept
            // separate so `command` stays pure and unit-testable without
            // touching the filesystem.
            RowAction::OpenInTerminal => (
                "x-terminal-emulator",
                vec![
                    "-e".into(),
                    "sh".into(),
                    "-c".into(),
                    terminal_shell_command(&containing_dir(path)),
                ],
            ),
        }
    }

    /// All actions, in menu order.
    pub fn all() -> [RowAction; 6] {
        [
            RowAction::CopyPath,
            RowAction::CopyFolderPath,
            RowAction::Open,
            RowAction::OpenWith,
            RowAction::OpenContainingFolder,
            RowAction::OpenInTerminal,
        ]
    }
}

/// The directory containing `path`, falling back to the path itself when it has
/// no usable parent (a bare name's parent is `""`, not `None`).
pub fn containing_dir(path: &str) -> String {
    match Path::new(path).parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_string_lossy().into_owned(),
        _ => path.to_string(),
    }
}

/// Single-quote `s` for POSIX `sh`: wrap in `'...'` and escape each embedded
/// `'` as `'\''`. Paths come from the index and may hold spaces, quotes, or
/// `$`, so interpolating them raw into `sh -c` would break or — worse — run
/// part of a filename as shell code.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The `sh -c` body "Open in terminal" runs: land in `dir`, then replace the
/// shell with the user's login shell (`$SHELL`, falling back to `sh` when it
/// is unset — e.g. under a bare desktop session).
///
/// The `cd` failure is deliberately non-fatal (`;`, not `&&`): if the
/// directory is gone the terminal still opens (in the GUI's own directory)
/// instead of flashing shut, and `sh` itself prints why the `cd` failed.
pub fn terminal_shell_command(dir: &str) -> String {
    format!("cd {}; exec \"${{SHELL:-sh}}\"", shell_quote(dir))
}

/// Monospace families zutty accepts for `-font`, most-widely-installed
/// first.
///
/// Zutty matches by filename stem under its font path — not fontconfig — so
/// these are filename prefixes (`DejaVuSansMono` matches
/// `DejaVuSansMono.ttf`, `DejaVuSansMono-Bold.ttf`, …). `9x18` comes last:
/// it is zutty's built-in default (shipped by `xfonts-base`), so naming it
/// explicitly is the same as naming nothing — it only serves as a fallback
/// when no scalable monospace is around but the bitmaps are.
pub const TERMINAL_FONT_CANDIDATES: &[&str] = &[
    "DejaVuSansMono",
    "NotoSansMono",
    "FreeMono",
    "LiberationMono",
    "UbuntuMono",
    "NotoMono",
    "NimbusMonoPS",
    "9x18",
];

/// Pick the first candidate present in `available` (filename stems, matched
/// the way zutty matches: case-insensitive prefix).
///
/// Pure so it is unit-testable without touching the filesystem;
/// [`terminal_monospace_font`] collects `available` from the real font path.
pub fn pick_terminal_font<'a>(candidates: &[&'a str], available: &[String]) -> Option<&'a str> {
    candidates.iter().copied().find(|c| {
        available
            .iter()
            .any(|f| f.len() >= c.len() && f[..c.len()].eq_ignore_ascii_case(c))
    })
}

/// Whether the Debian `x-terminal-emulator` alternative target is zutty.
/// Pure so the symlink-chasing in [`terminal_emulator_basename`] stays
/// testable.
pub fn is_zutty_basename(name: &str) -> bool {
    name.eq_ignore_ascii_case("zutty")
}

/// Resolve `x-terminal-emulator` through the alternatives symlinks to the
/// real terminal's basename (`zutty`, `xterm`, …).
///
/// `None` when it cannot be resolved — the caller then assumes a generic
/// terminal and passes no terminal-specific flags.
pub fn terminal_emulator_basename() -> Option<String> {
    let exe = find_on_path("x-terminal-emulator")?;
    let mut cur = exe;
    for _ in 0..8 {
        match std::fs::read_link(&cur) {
            Ok(target) => {
                cur = if target.is_absolute() {
                    target
                } else {
                    cur.parent().unwrap_or(Path::new("/")).join(target)
                };
            }
            Err(_) => break,
        }
    }
    cur.file_name().map(|s| s.to_string_lossy().into_owned())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return std::fs::symlink_metadata(&p).ok().map(|_| p);
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join(name);
        if std::fs::symlink_metadata(&cand).is_ok() {
            return Some(cand);
        }
    }
    None
}

/// Filename stems under zutty's default font path, for [`pick_terminal_font`]
/// to choose from.
///
/// Only the directories zutty itself searches by default are walked: a stem
/// found elsewhere (e.g. a user font dir) would still abort, because zutty
/// would not look there without an explicit `-fontpath`. Unreadable
/// directories yield fewer names, never a failure — the caller falls back to
/// launching without `-font`.
pub fn available_font_stems() -> Vec<String> {
    let dirs = [
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = dirs.into_iter().collect();
    // Paranoia bound: the tree is shallow, but never walk forever.
    let mut guard = 0usize;
    while let Some(dir) = stack.pop() {
        guard += 1;
        if guard > 5000 {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// The `-font` choice for this machine, if any: the first
/// [`TERMINAL_FONT_CANDIDATES`] entry present under the font path.
pub fn terminal_monospace_font() -> Option<&'static str> {
    let available = available_font_stems();
    pick_terminal_font(TERMINAL_FONT_CANDIDATES, &available)
}

/// The full `x-terminal-emulator` launch for "Open in terminal" on `path`:
/// the base args from [`RowAction::command`] with the host-specific font
/// prefix ([`terminal_font_arg`]) spliced in front of `-e`.
///
/// One entry point so the GUI and tests cannot disagree about what is
/// launched: `RowAction::command` stays pure (no filesystem touch), and the
/// environment-dependent part lives here.
pub fn open_in_terminal_command(path: &str) -> (&'static str, Vec<String>) {
    let (prog, base) = RowAction::OpenInTerminal.command(path);
    (prog, with_terminal_font_args(base, terminal_font_arg()))
}

/// The `-font` family to pass, if any: only when the alternatives target is
/// zutty (whose `9x18` default aborts when `xfonts-base` is missing) and an
/// installed monospace family was found.
pub fn terminal_font_arg() -> Option<&'static str> {
    let is_zutty = terminal_emulator_basename()
        .map(|b| is_zutty_basename(&b))
        .unwrap_or(false);
    if is_zutty {
        terminal_monospace_font()
    } else {
        None
    }
}

/// Splice optional `-font` flags in front of the `x-terminal-emulator` base
/// args. `-e` must stay the last option flag (zutty treats everything after
/// it as the command to run), so the font flags go first. Pure so the order
/// is pinned by a test; [`terminal_font_args`] supplies the host-specific
/// prefix at launch time.
pub fn with_terminal_font_args(mut base: Vec<String>, font: Option<&str>) -> Vec<String> {
    if let Some(f) = font {
        let mut full = vec!["-font".to_string(), f.to_string()];
        full.append(&mut base);
        full
    } else {
        base
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

/// Which path a mouse click should open a context menu for, if any.
///
/// Only a right-click (button 3) on a result row opens the menu — a left-click
/// (button 1) starts a drag instead, and any other button or a click on blank
/// space (past the last row, or a negative coordinate) opens nothing rather
/// than silently acting on the wrong file.
///
/// Button 2 never reaches this function: the toolkit normalizes a
/// middle-press to button 1 on delivery (see
/// `rswidgets::normalize_press_button`), so a middle-click picks the row
/// like a primary press instead of being dropped.
///
/// `row` is the widget's hit-test result (`row_at`): the 0-based row under the
/// pointer, or `None` when the pointer is not over a row. `current` holds the
/// paths actually on screen, in row order.
///
/// Returns the full path the menu should act on (Copy full path, Open, Open
/// containing folder, Open with…, …).
pub fn context_menu_target(button: u32, row: Option<i32>, current: &[String]) -> Option<String> {
    if button != 3 {
        return None;
    }
    let row = row?;
    if row < 0 {
        return None;
    }
    current.get(row as usize).cloned()
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

/// Sort key for the icon column: the file-name extension, lowercased so
/// `Main.RS` groups with `main.rs` (as their shared icon already does).
///
/// Uses the same split as [`icon_name_for`], so the sort order and the icons
/// agree on what "type" means. A name with no extension — including
/// directories, whose trailing slash [`row_from_path`] strips first — sorts
/// as `""`, ahead of any extension.
pub fn extension_sort_key(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default()
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
    fn rescan_request_names_the_root() {
        match DaemonClient::rescan_request("/home/me/src") {
            Request::Rescan { root } => assert_eq!(root, "/home/me/src"),
            other => panic!("expected a Rescan request, got {other:?}"),
        }
    }

    #[test]
    fn parse_root_paths_reads_the_stats_shape() {
        let stats = serde_json::json!({
            "entries": 3,
            "roots": [{"path": "/a"}, {"path": "/b"}, {"nopath": 1}],
        });
        assert_eq!(
            parse_root_paths(&stats),
            vec!["/a".to_string(), "/b".to_string()]
        );
        assert!(parse_root_paths(&serde_json::json!({})).is_empty());
        assert!(parse_root_paths(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn menubar_model_covers_every_action_exactly_once() {
        use MenuAction::*;
        let model = menubar_model();
        assert_eq!(model.len(), 4, "File, Search, View, Help");
        let mut seen = Vec::new();
        for (menu, items) in &model {
            assert!(!items.is_empty(), "{menu} must not be empty");
            for (label, action) in items {
                assert!(!label.is_empty());
                assert!(!action.action_name().is_empty());
                assert!(!seen.contains(action), "{label:?} is listed twice");
                seen.push(*action);
            }
        }
        let all = [
            SearchNow,
            FocusQuery,
            ClearQuery,
            ToggleCase,
            ToggleFuzzy,
            ToggleCount,
            ToggleAdvanced,
            FilterAudio,
            FilterCompressed,
            FilterExecutable,
            FilterFolder,
            FilterPicture,
            FilterVideo,
            OpenContainingFolder,
            CopyPath,
            RefreshIndex,
            ShowSyntax,
            ShowDaemonStatus,
            ShowAbout,
            Quit,
        ];
        assert_eq!(
            seen.len(),
            all.len(),
            "every action is on the bar: {seen:?}"
        );
        for a in all {
            assert!(seen.contains(&a), "{a:?} is missing from the bar");
        }
    }

    #[test]
    fn filter_presets_compile_and_match() {
        use MenuAction::*;
        // Every preset must be a valid query on its own ...
        for action in [
            FilterAudio,
            FilterCompressed,
            FilterExecutable,
            FilterFolder,
            FilterPicture,
            FilterVideo,
        ] {
            let q = action
                .preset_query()
                .unwrap_or_else(|| panic!("{action:?} needs a preset"));
            fflocate::query::Query::compile(q)
                .unwrap_or_else(|e| panic!("preset {q:?} does not compile: {e}"));
        }
        // ... and select the right things: case-insensitive extensions, and
        // directories only for the folder preset.
        let audio = fflocate::query::Query::compile(FilterAudio.preset_query().unwrap()).unwrap();
        assert!(audio.matches(b"song.mp3", false, false, || None));
        assert!(audio.matches(b"song.MP3", false, false, || None));
        assert!(!audio.matches(b"song.mp3.bak", false, false, || None));
        assert!(!audio.matches(b"song.txt", false, false, || None));
        let folder = fflocate::query::Query::compile(FilterFolder.preset_query().unwrap()).unwrap();
        assert!(folder.matches(b"src", true, false, || None));
        assert!(
            folder.matches(b"src.rs", true, false, || None),
            "dirs match whatever they are called"
        );
        assert!(!folder.matches(b"src", false, false, || None));
        // Non-filter actions carry no preset.
        assert_eq!(SearchNow.preset_query(), None);
        assert_eq!(Quit.preset_query(), None);
    }

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
    fn extension_sort_key_matches_the_icon_split() {
        // The sort key and the icon agree on what "type" means: same split,
        // case-insensitive, so grouped extensions share an icon.
        assert_eq!(extension_sort_key("main.rs"), "rs");
        assert_eq!(extension_sort_key("archive.tar.gz"), "gz");
        assert_eq!(extension_sort_key("MAIN.RS"), "rs");
        // No extension sorts as empty (ahead of any extension).
        assert_eq!(extension_sort_key("README"), "");
        assert_eq!(extension_sort_key("src"), "");
    }

    #[test]
    fn rows_from_paths_keeps_order() {
        let paths = vec!["/a/x.rs".to_string(), "/a/y.md".to_string()];
        let rows = rows_from_paths(&paths);
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["x.rs", "y.md"]
        );
    }

    #[test]
    fn count_only_status_does_not_say_zero_of_n() {
        // In count mode no paths come back, so the plain status line would
        // render a misleading "0 of 6".
        let o = SearchOutcome {
            paths: vec![],
            total: 6,
            fresh: true,
            ..Default::default()
        };
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
        assert_eq!(
            o,
            SearchOptions {
                case_sensitive: false,
                fuzzy: false,
                max_distance: None,
                count_only: false,
            }
        );
    }

    #[test]
    fn quote_literal_wraps_in_quotes() {
        // Plain text quotes verbatim: the engine then disables modifiers,
        // globs and negation inside the phrase.
        assert_eq!(quote_literal("main"), Some("\"main\"".to_string()));
        assert_eq!(quote_literal("ext:rs"), Some("\"ext:rs\"".to_string()));
        assert_eq!(
            quote_literal("report (final) *.rs -old"),
            Some("\"report (final) *.rs -old\"".to_string())
        );
        // Backslashes need no escape: quotes are raw-delimited, not
        // backslash-delimited, so Windows paths quote verbatim.
        assert_eq!(
            quote_literal("C:\\tree\\a b"),
            Some("\"C:\\tree\\a b\"".to_string())
        );
        // An embedded quote cannot be quoted — there is no escape inside
        // "...", so refusing beats sending a mangled query.
        assert_eq!(quote_literal("say \"hi\""), None);
        // Empty quotes to itself; the caller never sends it (not searchable).
        assert_eq!(quote_literal(""), Some("\"\"".to_string()));
    }

    #[test]
    fn quoted_basic_compiles_to_a_single_substring() {
        // The contract `quote_literal` promises, against the real engine:
        // syntax-looking Basic text becomes one literal needle, never a
        // modifier, glob or negation.
        for raw in ["ext:rs", "type:dir", "regex:a.*", "*.rs", "-main", "a b"] {
            let q = quote_literal(raw).unwrap();
            let query = fflocate::query::Query::compile(&q).unwrap();
            assert_eq!(query.raw, q);
            // Exactly one required literal, and it is the raw text itself.
            let lits = query.required_literals();
            assert_eq!(
                lits.len(),
                1,
                "Basic {raw:?} must be one literal (got {lits:?})"
            );
            assert_eq!(lits[0].0, raw.as_bytes(), "Basic {raw:?} needle");
        }
    }

    #[test]
    fn effective_query_quotes_basic_and_passes_advanced() {
        // Only the quick box: quoted.
        assert_eq!(effective_query("main", ""), Some("\"main\"".to_string()));
        // Syntax-looking Basic text is quoted into inert text.
        assert_eq!(
            effective_query("ext:rs", ""),
            Some("\"ext:rs\"".to_string())
        );
        // Only the advanced box: untouched (full language).
        assert_eq!(effective_query("", "ext:rs"), Some("ext:rs".to_string()));
        // Whitespace is not content.
        assert_eq!(effective_query("  ", "\t"), Some(String::new()));
        // Unquotable Basic text refuses.
        assert_eq!(effective_query("say \"hi\"", ""), None);
    }

    #[test]
    fn effective_query_ands_both_boxes() {
        // Both set: the quoted Basic term ANDs with the Advanced expression.
        assert_eq!(
            effective_query("report", "ext:rs path:src"),
            Some("\"report\" ext:rs path:src".to_string())
        );
        // A preset in Advanced narrows the Basic text, not vice versa.
        assert_eq!(
            effective_query("song", "type:dir"),
            Some("\"song\" type:dir".to_string())
        );
        // Unquotable Basic refuses even when Advanced is fine.
        assert_eq!(effective_query("say \"hi\"", "ext:rs"), None);
    }

    #[test]
    fn anded_query_compiles_to_both_predicates() {
        // The AND is real: the engine must enforce the literal AND the
        // modifier, not either alone.
        let q = effective_query("report", "ext:rs").unwrap();
        let query = fflocate::query::Query::compile(&q).unwrap();
        assert!(query.matches(b"report.rs", false, false, || None));
        assert!(
            !query.matches(b"report.txt", false, false, || None),
            "ext must still filter"
        );
        assert!(
            !query.matches(b"other.rs", false, false, || None),
            "literal must still filter"
        );
    }

    #[test]
    fn clearing_one_box_keeps_searching_the_other() {
        // Regression test: the list shows only entries matching *both* boxes,
        // so emptying one box while the other still holds text must re-search
        // (narrow to the remaining box), not blank the list. Only both-empty
        // clears at once.
        assert!(!box_change_clears("", "ext:rs"), "advanced still filters");
        assert!(!box_change_clears("report", ""), "basic still filters");
        assert!(
            !box_change_clears("\"report\"", "  "),
            "quoted basic still filters"
        );
        assert!(box_change_clears("", ""), "both empty clears");
        assert!(box_change_clears("  ", "\t"), "both whitespace clears");
        // Unquotable Basic text arms a request (surfacing the error) rather
        // than silently clearing.
        assert!(
            !box_change_clears("say \"hi\"", ""),
            "unquotable must not clear"
        );
        assert!(
            !box_change_clears("say \"hi\"", "ext:rs"),
            "unquotable must not clear"
        );
    }

    #[test]
    fn wheel_steps_the_max_distance_by_one() {
        // Wheel-up increments, wheel-down decrements, like a spin button.
        assert_eq!(step_max_distance("2", WheelStep::Up), "3");
        assert_eq!(step_max_distance("2", WheelStep::Down), "1");
        assert_eq!(step_max_distance("0", WheelStep::Up), "1");
        // Surrounding whitespace is ignored, not carried into the field.
        assert_eq!(step_max_distance("  4  ", WheelStep::Up), "5");
    }

    #[test]
    fn wheel_steps_saturate_at_the_bounds() {
        // Distances below zero are meaningless; above the cap they are noise.
        assert_eq!(step_max_distance("0", WheelStep::Down), "0");
        assert_eq!(
            step_max_distance(&MAX_FUZZY_DISTANCE.to_string(), WheelStep::Up),
            MAX_FUZZY_DISTANCE.to_string()
        );
        assert_eq!(
            step_max_distance(&MAX_FUZZY_DISTANCE.to_string(), WheelStep::Down),
            (MAX_FUZZY_DISTANCE - 1).to_string()
        );
    }

    #[test]
    fn wheel_from_a_blank_or_bad_field_starts_at_the_default() {
        // A blank or unparseable field counts as the default, so wheeling
        // from an empty field lands on a concrete value instead of doing
        // nothing.
        assert_eq!(step_max_distance("", WheelStep::Up), "3");
        assert_eq!(step_max_distance("   ", WheelStep::Down), "1");
        assert_eq!(step_max_distance("abc", WheelStep::Up), "3");
        assert_eq!(step_max_distance("-1", WheelStep::Down), "1");
    }

    #[test]
    fn max_distance_string_and_number_defaults_agree() {
        assert_eq!(DEFAULT_MAX_DISTANCE, DEFAULT_MAX_DISTANCE_VALUE.to_string());
    }

    #[test]
    fn file_uri_encodes_special_characters() {
        assert_eq!(
            path_to_file_uri("/home/me/src/main.rs"),
            "file:///home/me/src/main.rs"
        );
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
    fn right_click_on_a_result_row_opens_a_context_menu() {
        // Regression test: clicking (right-clicking) a search result item must
        // produce a context menu acting on that row's full path — offering
        // Copy full path, Open, Open containing folder, Open with…, etc.
        // A handler that ignores the click (e.g. a button-press callback that
        // is never registered, or a hit-test that never hits a row) leaves
        // the user with no way to copy/open a result.
        let current = vec!["/a/main.rs".to_string(), "/a/lib.rs".to_string()];

        // Right-click (button 3) on the first row targets that row's path.
        let target = context_menu_target(3, Some(0), &current);
        assert_eq!(target.as_deref(), Some("/a/main.rs"));

        // Right-click on the second row targets the second path.
        let target = context_menu_target(3, Some(1), &current);
        assert_eq!(target.as_deref(), Some("/a/lib.rs"));

        // And the menu opened for that path offers the full action set.
        let labels: Vec<_> = RowAction::all().iter().map(|a| a.label()).collect();
        for want in [
            "Copy full path",
            "Open",
            "Open containing folder",
            "Open with…",
        ] {
            assert!(
                labels.contains(&want),
                "menu must offer {want:?}, got {labels:?}"
            );
        }
    }

    #[test]
    fn only_right_click_on_a_row_opens_a_context_menu() {
        let current = vec!["/a/main.rs".to_string()];
        // A plain (left) click starts a drag, not a menu.
        assert_eq!(context_menu_target(1, Some(0), &current), None);
        // Any other button is ignored too.
        assert_eq!(context_menu_target(2, Some(0), &current), None);
        // Right-click on blank space (no hit-test row) opens nothing rather
        // than acting on a neighbour.
        assert_eq!(context_menu_target(3, None, &current), None);
        // ... as does a right-click past the last row or above the first.
        assert_eq!(context_menu_target(3, Some(1), &current), None);
        assert_eq!(context_menu_target(3, Some(-1), &current), None);
        // And with no rows on screen there is nothing to act on.
        let empty: Vec<String> = Vec::new();
        assert_eq!(context_menu_target(3, Some(0), &empty), None);
    }

    #[test]
    fn middle_click_on_a_result_row_picks_it() {
        // Regression test, inverted: a middle-click used to abort handling
        // entirely (button 2 hit the early return, so nothing happened). The
        // toolkit now delivers a middle-press as a primary press (see
        // `rswidgets::normalize_press_button`), so it picks the row under
        // the pointer exactly like a left-press — clients never observe a
        // bare 2 and cannot drop it.
        use rswidgets::normalize_press_button;

        let current = vec!["/a/main.rs".to_string(), "/a/lib.rs".to_string()];

        // What the handler actually sees for a middle-click.
        assert_eq!(normalize_press_button(2), 1);
        // ...and that primary press lands on the row: the press picks a path
        // rather than aborting.
        assert_eq!(row_at_line(&current, 0), Some("/a/main.rs"));
        assert_eq!(row_at_line(&current, 1), Some("/a/lib.rs"));
        // It still opens no context menu: only a right-click does that.
        let delivered = normalize_press_button(2);
        assert_eq!(context_menu_target(delivered, Some(0), &current), None);
        // And the raw value stays menu-less too, defence in depth — delivery
        // is what guarantees a 2 never arrives.
        assert_eq!(context_menu_target(2, Some(0), &current), None);
        assert_eq!(context_menu_target(2, Some(1), &current), None);
    }

    #[test]
    fn row_action_labels_and_commands() {
        assert_eq!(RowAction::CopyPath.label(), "Copy full path");
        assert_eq!(RowAction::CopyFolderPath.label(), "Copy folder path");
        assert_eq!(RowAction::OpenWith.label(), "Open with…");
        assert_eq!(RowAction::OpenInTerminal.label(), "Open in terminal");
        // Clipboard actions need no external command.
        assert_eq!(RowAction::CopyPath.command("/a/b.rs").0, "");
        assert_eq!(RowAction::CopyFolderPath.command("/a/b.rs").0, "");
        assert!(RowAction::CopyPath.is_clipboard());
        assert!(RowAction::CopyFolderPath.is_clipboard());
        assert!(!RowAction::Open.is_clipboard());
        #[cfg(not(windows))]
        {
            // Open launches the file itself...
            let (prog, args) = RowAction::Open.command("/a/b.rs");
            assert_eq!(prog, "xdg-open");
            assert_eq!(args, vec!["/a/b.rs".to_string()]);
            // ...while "containing folder" launches the parent directory.
            let (prog, args) = RowAction::OpenContainingFolder.command("/a/b.rs");
            assert_eq!(prog, "xdg-open");
            assert_eq!(args, vec!["/a".to_string()]);
            // "Open with" asks mimeopen to list handlers for this very file.
            let (prog, args) = RowAction::OpenWith.command("/a/b.rs");
            assert_eq!(prog, "mimeopen");
            assert_eq!(args, vec!["-a".to_string(), "/a/b.rs".to_string()]);
            // A terminal is launched in the file's directory, with a shell.
            let (prog, args) = RowAction::OpenInTerminal.command("/a/b.rs");
            assert_eq!(prog, "x-terminal-emulator");
            assert_eq!(args.first().map(String::as_str), Some("-e"));
            assert_eq!(
                args.last().map(String::as_str),
                Some("cd '/a'; exec \"${SHELL:-sh}\"")
            );
        }
        #[cfg(windows)]
        {
            // Windows speaks the shell's own verbs: `start` for files, Explorer
            // for folders, the Open-With dialog for choice, a console for shells.
            let (prog, args) = RowAction::Open.command("C:\\t\\b.rs");
            assert_eq!(prog, "cmd");
            assert_eq!(args[1], "start");
            assert_eq!(args.last().map(String::as_str), Some("C:\\t\\b.rs"));
            let (prog, args) = RowAction::OpenContainingFolder.command("C:\\t\\b.rs");
            assert_eq!(prog, "explorer");
            assert_eq!(args, vec!["/select,C:\\t\\b.rs".to_string()]);
            let (prog, args) = RowAction::OpenWith.command("C:\\t\\b.rs");
            assert_eq!(prog, "rundll32");
            assert!(args[0].contains("OpenAs_RunDLL"), "{args:?}");
            let (prog, _) = RowAction::OpenInTerminal.command("C:\\t\\b.rs");
            assert_eq!(prog, "cmd");
        }
    }

    #[test]
    fn open_in_terminal_lands_in_the_containing_dir() {
        // "Open in terminal" promises the *directory containing* the file.
        #[cfg(not(windows))]
        {
            // the `cd` target is the parent, not the file itself.
            let (_, args) = RowAction::OpenInTerminal.command("/home/me/src/main.rs");
            let body = args.last().unwrap();
            assert!(body.starts_with("cd '/home/me/src';"), "{body}");
            // A bare name has no usable parent; the fallback is the name itself.
            let (_, args) = RowAction::OpenInTerminal.command("b.rs");
            assert!(args.last().unwrap().starts_with("cd 'b.rs';"), "{args:?}");
        }
        #[cfg(windows)]
        {
            // `start /d <dir>` sets the console's working directory.
            let (_, args) = RowAction::OpenInTerminal.command("C:\\t\\main.rs");
            assert!(args.contains(&"/d".to_string()), "{args:?}");
            assert_eq!(args.last().map(String::as_str), Some("C:\\t"));
        }
    }

    #[test]
    fn shell_quote_survives_hostile_paths() {
        assert_eq!(shell_quote("/a/b"), "'/a/b'");
        assert_eq!(shell_quote("/a/my dir"), "'/a/my dir'");
        assert_eq!(shell_quote("/a/b's"), "'/a/b'\\''s'");
        // `$` and backticks stay literal inside single quotes.
        assert_eq!(shell_quote("/a/$(rm -rf ~)"), "'/a/$(rm -rf ~)'");
    }

    #[test]
    fn terminal_shell_command_falls_back_when_cd_fails() {
        // `;` not `&&`: a deleted directory still yields a terminal.
        let cmd = terminal_shell_command("/gone/dir");
        assert!(cmd.starts_with("cd '/gone/dir'; "), "{cmd}");
        // Unset $SHELL still yields a shell.
        assert!(cmd.ends_with("exec \"${SHELL:-sh}\""), "{cmd}");
    }

    #[test]
    fn pick_terminal_font_prefers_the_first_installed_candidate() {
        let files = vec![
            "NotoSansMono-Regular.ttf".to_string(),
            "FreeMono.ttf".to_string(),
        ];
        assert_eq!(
            pick_terminal_font(&["DejaVuSansMono", "NotoSansMono", "FreeMono"], &files),
            Some("NotoSansMono")
        );
        // Zutty matches filename stems case-insensitively ...
        let files = vec!["dejavusansmono.ttf".to_string()];
        assert_eq!(
            pick_terminal_font(&["DejaVuSansMono"], &files),
            Some("DejaVuSansMono")
        );
        // ... but a stem prefix must match at a boundary: "NotoSans" alone is
        // not the "NotoSansMono" family.
        let files = vec!["NotoSans-Regular.ttf".to_string()];
        assert_eq!(pick_terminal_font(&["NotoSansMono"], &files), None);
        // Nothing installed: no font flag, so the launch is unchanged.
        let empty: Vec<String> = Vec::new();
        assert_eq!(pick_terminal_font(TERMINAL_FONT_CANDIDATES, &empty), None);
    }

    #[test]
    fn is_zutty_basename_matches_the_alternative_target() {
        assert!(is_zutty_basename("zutty"));
        assert!(is_zutty_basename("ZUTTY"));
        assert!(!is_zutty_basename("xterm"));
        assert!(!is_zutty_basename("gnome-terminal"));
        assert!(!is_zutty_basename(""));
    }

    #[test]
    fn with_terminal_font_args_keeps_e_last() {
        // zutty treats everything after `-e` as the command to run, so the
        // font flags must come first: `-font F -e sh -c ...`, never after.
        let base = vec![
            "-e".to_string(),
            "sh".to_string(),
            "-c".to_string(),
            "exec sh".to_string(),
        ];
        let full = with_terminal_font_args(base.clone(), Some("DejaVuSansMono"));
        assert_eq!(
            full,
            vec![
                "-font".to_string(),
                "DejaVuSansMono".to_string(),
                "-e".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "exec sh".to_string()
            ]
        );
        // No font on this machine (or not zutty): the launch is byte-identical
        // to before.
        assert_eq!(with_terminal_font_args(base.clone(), None), base);
    }

    #[test]
    fn open_in_terminal_command_matches_the_live_launch() {
        // Unix: the one entry point is base `cd` body plus the optional font
        // prefix in front of `-e`.
        #[cfg(not(windows))]
        {
            // On this machine the alternative is zutty with
            // DejaVuSansMono installed, so the prefix is present — and the test
            // would catch a caller that launched the base args raw.
            let (prog, args) = open_in_terminal_command("/a/b.rs");
            assert_eq!(prog, "x-terminal-emulator");
            let e = args
                .iter()
                .position(|a| a == "-e")
                .expect("-e must be present");
            assert!(args[e + 1..].join(" ").contains("cd '/a'"), "{args:?}");
            if terminal_font_arg().is_some() {
                assert_eq!(
                    &args[..2],
                    &["-font", terminal_font_arg().unwrap()],
                    "{args:?}"
                );
            }
        }
        // Windows: no font dance — the entry point is the `cmd /c start`
        // launch, and the caller (`run_row_action`) uses it directly.
        #[cfg(windows)]
        {
            let (prog, args) = RowAction::OpenInTerminal.command("C:\\a\\b.rs");
            assert_eq!(prog, "cmd");
            assert!(args.contains(&"/d".to_string()), "{args:?}");
        }
    }

    #[test]
    fn row_actions_are_all_listed_in_menu_order() {
        // The menu and the tests iterate the same list, so the two cannot drift.
        let all = RowAction::all();
        assert_eq!(all.len(), 6);
        assert_eq!(all[0], RowAction::CopyPath);
        assert_eq!(all[5], RowAction::OpenInTerminal);
    }

    #[test]
    fn containing_dir_handles_bare_names() {
        assert_eq!(containing_dir("/a/b.rs"), "/a");
        // A bare name has no usable parent; the path itself is the best answer.
        assert_eq!(containing_dir("b.rs"), "b.rs");
    }

    #[test]
    fn open_containing_folder_of_a_bare_name_is_the_name() {
        // A path with no parent directory falls back to the path itself.
        #[cfg(not(windows))]
        {
            let (_, args) = RowAction::OpenContainingFolder.command("b.rs");
            assert_eq!(args, vec!["b.rs".to_string()]);
        }
        // Windows wraps even the fallback in Explorer's /select.
        #[cfg(windows)]
        {
            let (_, args) = RowAction::OpenContainingFolder.command("b.rs");
            assert_eq!(args, vec!["/select,b.rs".to_string()]);
        }
    }

    #[test]
    fn status_line_reports_totals() {
        let o = SearchOutcome {
            paths: vec!["a".into()],
            total: 5,
            fresh: true,
            ..Default::default()
        };
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
            ..Default::default()
        };
        let s = o.status_line(0);
        assert!(s.contains("fresh view"), "{s}");
        assert!(s.contains("catching up"), "{s}");
        assert!(s.contains("/mnt/ro"), "{s}");
    }

    #[test]
    fn status_line_quiet_when_healthy() {
        let o = SearchOutcome {
            paths: vec!["a".into()],
            total: 1,
            fresh: true,
            ..Default::default()
        };
        let s = o.status_line(1);
        assert_eq!(s, "1 matches");
        assert!(!s.contains('['), "no noise on the happy path: {s}");
    }

    #[test]
    fn searching_line_shows_query_and_elapsed() {
        let s = searching_status_line(
            "main",
            std::time::Duration::from_millis(1340),
            0,
            false,
            None,
            None,
        );
        assert_eq!(s, "searching for main… 1.3s");
    }

    #[test]
    fn searching_line_counts_hits_and_catching_up() {
        let s = searching_status_line(
            "main",
            std::time::Duration::from_millis(300),
            1,
            true,
            None,
            None,
        );
        assert!(s.contains("(1 hit so far)"), "{s}");
        assert!(s.contains("[daemon catching up]"), "{s}");
        let s = searching_status_line(
            "main",
            std::time::Duration::from_millis(300),
            12,
            false,
            None,
            None,
        );
        assert!(s.contains("(12 hits so far)"), "{s}");
    }

    #[test]
    fn searching_line_without_a_query_still_shows_time() {
        // The label is display-only; an empty box still reports elapsed.
        let s = searching_status_line(
            "",
            std::time::Duration::from_millis(200),
            0,
            false,
            None,
            None,
        );
        assert_eq!(s, "searching… 0.2s");
    }

    #[test]
    fn slow_finished_line_names_the_server_cost() {
        // A cold first search (crawl + trigram build) reports seconds in
        // Meta.elapsed_ms; the finished line repeats it so the wait explains
        // itself after the fact.
        let o = SearchOutcome {
            paths: vec!["a".into()],
            total: 1,
            fresh: true,
            elapsed_ms: Some(2340.0),
            ..Default::default()
        };
        let s = o.status_line(1);
        assert!(s.contains("[2.3s server]"), "{s}");
    }

    #[test]
    fn fast_finished_line_stays_quiet() {
        // Under a second: no suffix, the happy path is unchanged.
        let o = SearchOutcome {
            paths: vec!["a".into()],
            total: 1,
            fresh: true,
            elapsed_ms: Some(12.0),
            ..Default::default()
        };
        assert_eq!(o.status_line(1), "1 matches");
        let o = SearchOutcome {
            paths: vec!["a".into()],
            total: 1,
            fresh: true,
            elapsed_ms: None,
            ..Default::default()
        };
        assert_eq!(o.status_line(1), "1 matches");
    }

    #[test]
    fn starting_line_names_the_daemon_not_the_query() {
        let s = starting_status_line(
            "main",
            std::time::Duration::from_millis(2100),
            Some(std::time::Duration::from_millis(3400)),
            0,
        );
        // The daemon age (since spawn), not the query age, is the headline:
        // the query only just started, the daemon has been cold for longer.
        assert_eq!(s, "starting fflocated… 3.4s (searching for main)");
    }

    #[test]
    fn starting_line_falls_back_to_query_elapsed() {
        let s = starting_status_line(
            "",
            std::time::Duration::from_millis(500),
            None,
            2,
        );
        assert_eq!(s, "starting fflocated… 0.5s (2 hits so far)");
    }

    #[test]
    fn streaming_line_counts_hits_without_a_total() {
        assert_eq!(
            streaming_status_line(1, std::time::Duration::from_millis(300), None),
            "1 hit… (counting total… 0.3s)"
        );
        assert_eq!(
            streaming_status_line(201, std::time::Duration::from_millis(1200), None),
            "201 hits… (counting total… 1.2s)"
        );
    }

    #[test]
    fn catching_up_suffix_reports_seconds_behind() {
        // Behind with a measured age: the number IS the answer to "how many
        // seconds behind".
        assert_eq!(
            catching_up_suffix(true, Some(4200)).as_deref(),
            Some("  [daemon catching up, 4s behind]")
        );
        // Seconds round down: 1999 ms is still "1s behind", not 2.
        assert_eq!(
            catching_up_suffix(true, Some(1999)).as_deref(),
            Some("  [daemon catching up, 1s behind]")
        );
        // Sub-second lag: no number, because the clock cannot honestly claim
        // one — the original wording survives.
        assert_eq!(
            catching_up_suffix(true, Some(400)).as_deref(),
            Some("  [daemon catching up]")
        );
        assert_eq!(
            catching_up_suffix(true, Some(0)).as_deref(),
            Some("  [daemon catching up]")
        );
        // Old daemon (no field at all): same wording as before.
        assert_eq!(
            catching_up_suffix(true, None).as_deref(),
            Some("  [daemon catching up]")
        );
        // Caught up: silent, regardless of a stale age.
        assert_eq!(catching_up_suffix(false, Some(9000)), None);
        assert_eq!(catching_up_suffix(false, None), None);
    }

    #[test]
    fn final_line_shows_lag_seconds() {
        let o = SearchOutcome {
            paths: vec!["a".into()],
            total: 1,
            fresh: true,
            catching_up: true,
            lag_ms: Some(12_400),
            ..Default::default()
        };
        assert_eq!(o.status_line(1), "1 matches  [daemon catching up, 12s behind]");
        // No age → the pre-existing wording, byte for byte.
        let o = SearchOutcome {
            fresh: true,
            catching_up: true,
            lag_ms: None,
            ..Default::default()
        };
        assert_eq!(o.status_line(0), "0 matches  [daemon catching up]");
        // Caught up → no suffix at all.
        let o = SearchOutcome {
            paths: vec!["a".into()],
            total: 1,
            fresh: true,
            ..Default::default()
        };
        assert_eq!(o.status_line(1), "1 matches");
    }

    #[test]
    fn in_flight_line_shows_lag_seconds() {
        let s = searching_status_line(
            "comp",
            std::time::Duration::from_millis(1300),
            0,
            true,
            Some(42),
            Some(7300),
        );
        assert_eq!(s, "searching for comp… 1.3s 42%  [daemon catching up, 7s behind]");
    }

    #[test]
    fn outcome_from_records_carries_lag() {
        let recs = vec![
            Response::Meta {
                gen: 1,
                lsn: 2,
                fresh: true,
                elapsed_ms: 3.0,
                stale_roots: vec![],
                catching_up: Some(true),
                lag_ms: Some(2500),
                backend: None,
                entries: None,
            },
            Response::Done {
                count: 0,
                cursor: "c:1:2".into(),
            },
        ];
        let o = DaemonClient::new().outcome_from_records(&recs).unwrap();
        assert_eq!(o.lag_ms, Some(2500));
        assert!(o.catching_up);
        assert!(o.status_line(0).contains("2s behind"), "{}", o.status_line(0));
    }

    #[test]
    fn scan_percent_caps_at_99_and_rejects_empty() {
        // 100% is Done's privilege: a "100%" that sits while the total is
        // still being counted would be the same lie the streaming line was
        // built to avoid.
        assert_eq!(scan_percent(0, 1000), Some(0));
        assert_eq!(scan_percent(500, 1000), Some(50));
        assert_eq!(scan_percent(999, 1000), Some(99));
        assert_eq!(scan_percent(1000, 1000), Some(99));
        assert_eq!(scan_percent(5000, 1000), Some(99), "overshoot still caps");
        assert_eq!(scan_percent(0, 0), None, "no denominator, no percent");
        assert_eq!(scan_percent(5, 0), None);
    }

    #[test]
    fn progress_percent_appears_in_both_inflight_lines() {
        let s = searching_status_line(
            "comp",
            std::time::Duration::from_millis(1300),
            0,
            false,
            Some(42),
            None,
        );
        assert_eq!(s, "searching for comp… 1.3s 42%");
        // None keeps the legacy wording byte-for-byte (old daemon,
        // fast query, no Progress yet).
        let s = searching_status_line(
            "comp",
            std::time::Duration::from_millis(1300),
            0,
            false,
            None,
            None,
        );
        assert_eq!(s, "searching for comp… 1.3s");
        let s = streaming_status_line(7, std::time::Duration::from_millis(1200), Some(3));
        assert_eq!(s, "7 hits… (counting total… 1.2s, 3%)");
        let s = streaming_status_line(7, std::time::Duration::from_millis(1200), None);
        assert_eq!(s, "7 hits… (counting total… 1.2s)");
    }

    #[test]
    fn query_request_opts_into_scan_progress() {
        // The GUI asks for Progress; `ff` never does (its output must stay
        // byte-identical, and old daemons ignore the unknown field anyway).
        match DaemonClient::query_request("comp", 201, &SearchOptions::default()) {
            Request::Query { progress, .. } => assert!(progress),
            other => panic!("expected a Query, got {other:?}"),
        }
    }

    #[test]
    fn outcome_ignores_progress_records() {
        // Progress is non-terminal and carries no Web-facing meaning: the
        // final line is Meta/Hits/Done exactly as before.
        let recs = vec![
            Response::Meta {
                gen: 1,
                lsn: 2,
                fresh: true,
                elapsed_ms: 3.0,
                stale_roots: vec![],
                catching_up: None,
                lag_ms: None,
                backend: None,
                entries: None,
            },
            Response::Progress {
                scanned: 100,
                total: 1000,
            },
            Response::Hit {
                path: "/a/b".into(),
                id: 9,
            },
            Response::Progress {
                scanned: 1000,
                total: 1000,
            },
            Response::Done {
                count: 1,
                cursor: "c:1:2".into(),
            },
        ];
        let o = DaemonClient::new().outcome_from_records(&recs).unwrap();
        assert_eq!(o.paths, vec!["/a/b".to_string()]);
        assert_eq!(o.total, 1);
    }

    #[test]
    fn search_label_combines_and_truncates() {
        assert_eq!(search_label("main", ""), "main");
        assert_eq!(search_label("", "ext:rs"), "ext:rs");
        assert_eq!(search_label("rep", "ext:rs"), "rep ext:rs");
        assert_eq!(search_label("  ", " "), "");
        let long = "a".repeat(100);
        let s = search_label(&long, "");
        assert_eq!(s.chars().count(), 40, "{s}");
        assert!(s.ends_with('…'), "{s}");
    }

    #[test]
    fn wait_for_hello_gives_up_on_an_absent_daemon() {
        // No listener on this socket: the wait must expire (bounded) rather
        // than hang, reporting the daemon still missing.
        let dir = tempfile::tempdir().unwrap();
        let client = DaemonClient::with_socket(dir.path().join("nobody.sock"));
        let start = std::time::Instant::now();
        assert!(!client.wait_for_hello(std::time::Duration::from_millis(120)));
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "the wait must be bounded, took {:?}",
            start.elapsed()
        );
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

    /// Retire the in-flight request without rendering it, freeing the gate
    /// for a newer query. The caller drops the old socket alongside this;
    /// stragglers that beat the close are dropped by `finish` (their seq
    /// sits beneath the new watermark). Returns the retired seq, or `None`
    /// when idle.
    ///
    /// Honesty bound: this frees the *client*, not the daemon — the orphaned
    /// server-side scan runs to Done (its writes fail into the closed fd).
    /// The latency win is issuing the fresh query now instead of after the
    /// stale scan completes, not saving daemon CPU.
    pub fn supersede(&mut self) -> Option<u64> {
        if self.in_flight == 0 {
            return None;
        }
        self.in_flight = 0;
        let old = self.issued;
        self.applied = self.applied.max(old);
        Some(old)
    }

    /// The seq most recently issued, for diagnostics/tests.
    pub fn issued(&self) -> u64 {
        self.issued
    }

    /// Would a partial update for `seq` still be welcome on screen? True when
    /// no newer result has been applied — lets streaming renders paint early
    /// hits without letting a slow old request scribble over a newer answer.
    /// [`finish`](Self::finish) remains the only thing that retires a ticket.
    pub fn is_current(&self, seq: u64) -> bool {
        seq > self.applied
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
    stream: fflocate::transport::Stream,
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
        stream: fflocate::transport::Stream,
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
                    return Err(GuiError::Io(
                        "daemon closed the socket while writing".into(),
                    ))
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
            return Ok(if did_something {
                Progress::Advanced
            } else {
                Progress::WouldBlock
            });
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

        Ok(if did_something {
            Progress::Advanced
        } else {
            Progress::WouldBlock
        })
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
            serde_json::Value::from(fflocate::proto::PROTOCOL_VERSION),
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
    fn a_stale_result_never_overwrites_a_newer_one() {        // The ordering hazard: request A takes longer than B. B's answer lands
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
    fn stale_partials_are_not_current() {
        let mut g = SearchGate::new();
        let a = g.begin().unwrap();
        assert!(g.is_current(a.seq), "unanswered ticket is current");
        g.in_flight = 0;
        let b = g.begin().unwrap();
        assert!(g.is_current(b.seq));
        // A newer result applied: older partials must stay off screen.
        assert!(g.finish(b.seq, "B").is_some());
        assert!(!g.is_current(a.seq), "superseded partial must not paint");
        // The applied ticket itself needs no more partials (final render
        // happened): not current either.
        assert!(!g.is_current(b.seq), "finished ticket takes no partials");
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

    #[test]
    fn supersede_retires_the_in_flight_request_for_immediate_reissue() {
        // The as-you-type supersede path: a slow short-prefix scan is in
        // flight, a newer keystroke arrives. Dropping the socket and
        // retiring the ticket must free the gate on the same tick — the
        // fresh query issues immediately, and anything from the old ticket
        // (stragglers that beat the socket close) is dropped, never painted.
        let mut g = SearchGate::new();
        let old = g.begin().unwrap();
        assert!(g.busy());
        assert_eq!(g.supersede(), Some(old.seq), "retires the live ticket");
        assert!(!g.busy(), "gate is free on the same tick");
        let fresh = g.begin().expect("fresh query issues immediately");
        assert!(fresh.seq > old.seq);
        assert_eq!(
            g.finish(old.seq, "STALE"),
            None,
            "stragglers from the dropped scan never paint"
        );
        assert_eq!(
            g.finish(fresh.seq, "FRESH"),
            Some("FRESH"),
            "the fresh answer renders"
        );
    }

    #[test]
    fn supersede_when_idle_is_a_no_op() {
        let mut g = SearchGate::new();
        assert_eq!(g.supersede(), None, "nothing in flight");
        assert!(!g.busy());
        // A normal issue/finish cycle still works afterwards.
        let r = g.begin().unwrap();
        assert_eq!(g.finish(r.seq, "A"), Some("A"));
    }

    #[test]
    fn superseded_partials_are_not_current_but_finish_still_retires() {
        // Partial renders consult `is_current`; the retired ticket's seq
        // sits at/beneath the watermark, so its Hits never paint — while
        // `finish` on it still releases the (already-free) slot harmlessly.
        let mut g = SearchGate::new();
        let old = g.begin().unwrap();
        g.supersede();
        assert!(!g.is_current(old.seq), "retired partials must not paint");
        let fresh = g.begin().unwrap();
        assert!(g.is_current(fresh.seq));
        // Double-finish of the retired ticket: slot math saturates, newer
        // answer unaffected.
        assert_eq!(g.finish(old.seq, "STALE"), None);
        assert_eq!(g.finish(fresh.seq, "FRESH"), Some("FRESH"));
    }
}

#[cfg(test)]
mod nonblocking_tests {
    //! These use a real loopback socket pair rather than a mock: the whole point is
    //! `WouldBlock`, which only a real non-blocking fd produces.

    use super::*;
    use std::io::Write;
    use std::net::Shutdown;

    /// A listener on a private temp path (unix) or an ephemeral loopback
    /// port (Windows, where the "path" carries `127.0.0.1:PORT` text),
    /// plus a connected client stream.
    fn pair() -> (
        tempfile::TempDir,
        fflocate::transport::Stream,
        fflocate::transport::Stream,
    ) {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let path = {
            let _ = &dir;
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("ephemeral port");
            let port = l.local_addr().expect("bound addr").port();
            std::path::PathBuf::from(format!("127.0.0.1:{port}"))
        };
        #[cfg(not(windows))]
        let path = dir.path().join("s.sock");
        let listener = fflocate::transport::bind(&path).unwrap();
        let client = fflocate::transport::connect(&path).unwrap();
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
            lag_ms: None,
            backend: None,
            entries: None,
        }
    }

    fn hit(path: &str, id: u32) -> Response {
        Response::Hit {
            path: path.into(),
            id,
        }
    }

    fn done(count: u64) -> Response {
        Response::Done {
            count,
            cursor: "c1".into(),
        }
    }

    fn pong() -> Response {
        Response::Pong {
            pid: 1,
            version: 2,
            backend: "fake".into(),
        }
    }

    /// Drive a request to completion, giving the peer a chance to write between
    /// steps. Returns the number of ticks it took and the final progress.
    fn pump_until_done(
        p: &mut PendingRequest,
        server: &mut fflocate::transport::Stream,
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
        let whole = format!(
            "{}{}{}",
            line(&meta()),
            line(&hit("/a/main.rs", 1)),
            line(&done(1))
        );
        let bytes = whole.as_bytes();
        for chunk in bytes.chunks(7) {
            server.write_all(chunk).unwrap();
            server.flush().unwrap();
            p.advance().unwrap();
        }

        let (_, last) = pump_until_done(&mut p, &mut server, 50);
        assert_eq!(
            last,
            Progress::Finished,
            "should finish once the terminal arrives"
        );
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
        assert!(
            saw_would_block,
            "an idle socket must report WouldBlock, not an error"
        );
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
        assert!(
            got.ends_with(b"\n"),
            "NDJSON: newline-terminated, got {got:?}"
        );
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
        assert_eq!(
            last,
            Progress::Finished,
            "the whole line must still be sent"
        );
    }
}
