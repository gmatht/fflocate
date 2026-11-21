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

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use fflocate::config_paths::socket_path_from_env;
use fflocate::proto::{Request, Response};

/// Re-exported because `PendingRequest::records` hands these back to the GUI.
pub use fflocate::proto::Response as WireResponse;

/// The application icon: the logo's mark with the wordmark cropped away, and
/// how each backend is handed it.
pub mod icon;

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
        let mut created: HashMap<String, u32> = HashMap::new();
        let mut total = 0u64;
        let mut fresh = true;
        let mut stale: Vec<String> = Vec::new();
        let mut catching_up = false;
        let mut lag_ms: Option<u64> = None;
        let mut elapsed_ms: Option<f64> = None;
        let mut phase: Option<fflocate::health::Phase> = None;
        let mut crawl_entries: Option<u64> = None;
        let mut crawl_dirs_pending: Option<u64> = None;
        for r in resp {
            match r {
                Response::Meta {
                    fresh: f,
                    stale_roots,
                    catching_up: c,
                    lag_ms: l,
                    elapsed_ms: e,
                    phase: p,
                    crawl_entries: ce,
                    crawl_dirs_pending: cdp,
                    ..
                } => {
                    fresh = *f;
                    stale = stale_roots.clone();
                    catching_up = c.unwrap_or(false);
                    lag_ms = *l;
                    elapsed_ms = Some(*e);
                    // The daemon's own phase, if a new-enough daemon sent one.
                    // An old daemon omits it: `Phase::parse` of the absent
                    // value yields `None`, and the GUI then falls back to
                    // `catching_up`/`lag_ms` rather than inventing a phase.
                    phase = p.as_deref().and_then(fflocate::health::Phase::parse);
                    crawl_entries = *ce;
                    crawl_dirs_pending = *cdp;
                }
                Response::Hit { path, created: c, .. } => {
                    // The daemon's own creation time for this entry (minutes
                    // since the epoch). Kept beside the path so the created-date
                    // filter never has to `stat` a row the daemon already dated.
                    if let Some(min) = c {
                        created.insert(path.clone(), *min);
                    }
                    paths.push(path.clone())
                }
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
            created,
            total,
            fresh,
            stale_roots: stale,
            catching_up,
            lag_ms,
            elapsed_ms,
            phase,
            crawl_entries,
            crawl_dirs_pending,
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
        }
    }

    /// True once this process spawned the daemon during this probe run.
    ///
    /// Used only *within* the probe now (to decide whether the failure wording
    /// should name a spawn). It deliberately no longer drives the in-flight
    /// search line: keying that on a sticky "we spawned once" flag was the bug
    /// that made a warm daemon mid-search read as "starting fflocated… 44.0s".
    /// The daemon's own `phase` says what it is doing instead.
    pub fn was_spawned(&self) -> bool {
        self.spawned
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

/// The advanced foldout's **created from / to** range.
///
/// Both bounds are inclusive unix seconds; `None` means that end is unbounded.
/// The index stores each entry's creation time in whole minutes since the epoch
/// and the daemon sends it with the hit (protocol v5), so the usual range is
/// filtered from data that already arrived — no per-row `stat`. A daemon that
/// predates the field, or an entry whose filesystem has no birth time, falls
/// back to [`created_secs`]; [`filter_by_created`] takes whichever clock it is
/// given and the caller decides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CreatedRange {
    pub from: Option<i64>,
    pub to: Option<i64>,
}

impl CreatedRange {
    /// Whether `secs` (unix seconds) falls inside the range.
    pub fn contains(&self, secs: i64) -> bool {
        self.from.map_or(true, |f| secs >= f) && self.to.map_or(true, |t| secs <= t)
    }

    /// The range as the status line spells it: `2026-01-02–2026-02-01`,
    /// `since 2026-01-02`, or `until 2026-02-01`.
    pub fn label(&self) -> String {
        match (self.from, self.to) {
            (Some(f), Some(t)) => format!("{}–{}", format_day(f), format_day(t)),
            (Some(f), None) => format!("since {}", format_day(f)),
            (None, Some(t)) => format!("until {}", format_day(t)),
            (None, None) => String::new(),
        }
    }
}

/// What one [`filter_by_created`] pass examined and kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CreatedStats {
    /// Rows the range was tested against.
    pub examined: usize,
    /// Rows that fell inside it.
    pub kept: usize,
    /// Rows whose filesystem reported no creation time (excluded: an unknown
    /// time is not a match, the same rule `size`/`since` terms follow).
    pub undated: usize,
}

impl CreatedStats {
    /// Rows that had a creation time outside the range.
    pub fn outside(&self) -> usize {
        self.examined - self.kept - self.undated
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`), so the calendar arithmetic is exact without a date crate.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`]: `(year, month, day)` for a day number.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// `YYYY-MM-DD` for a unix timestamp, in UTC like the bounds themselves.
pub fn format_day(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Parse one bound: the first second of the day, or its last (`end_of_day`).
fn parse_day_bound(text: &str, field: &str, end_of_day: bool) -> Result<Option<i64>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let bad = || format!("{field}: \"{text}\" is not a date — use YYYY-MM-DD");
    let bytes = text.as_bytes();
    // Validate the shape on bytes first, so every slice below is ASCII (a
    // multi-byte character can never split a date string then).
    let shaped = bytes.len() == 10
        && bytes.iter().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    if !shaped {
        return Err(bad());
    }
    let (y, m, d) = match (
        text[0..4].parse::<i64>(),
        text[5..7].parse::<i64>(),
        text[8..10].parse::<i64>(),
    ) {
        (Ok(y), Ok(m), Ok(d)) => (y, m, d),
        _ => return Err(bad()),
    };
    if m < 1 || m > 12 || d < 1 || d > days_in_month(y, m) {
        return Err(bad());
    }
    let start = days_from_civil(y, m, d) * 86_400;
    Ok(Some(if end_of_day { start + 86_399 } else { start }))
}

/// Parse the foldout's two created-date fields.
///
/// Both are `YYYY-MM-DD` in UTC, blank for unbounded, and the range is
/// inclusive at the day level (the `to` day's last second is included). A
/// blank pair means no range at all (`Ok(None)`). Errors name the field and
/// the offending text so the status line can repeat them verbatim.
pub fn parse_created_range(from_text: &str, to_text: &str) -> Result<Option<CreatedRange>, String> {
    let from = parse_day_bound(from_text, "created from", false)?;
    let to = parse_day_bound(to_text, "created to", true)?;
    match (from, to) {
        (None, None) => Ok(None),
        (f, t) => {
            if let (Some(f), Some(t)) = (f, t) {
                if f > t {
                    return Err(format!(
                        "created range: from {} is after to {}",
                        format_day(f),
                        format_day(t)
                    ));
                }
            }
            Ok(Some(CreatedRange { from: f, to: t }))
        }
    }
}

/// A file's creation (birth) time in unix seconds, read with `stat`.
///
/// This is the fallback now that hits carry the daemon's stored creation time:
/// it answers for a daemon that predates protocol v5, and for entries the
/// filesystem would not date at index time (the file may have been created
/// since, or an older index may simply not know). `None` when the path is gone
/// or the filesystem does not record a birth time — an unknown time never
/// matches a range, and the status line counts those rows separately rather
/// than silently dropping them.
pub fn created_secs(path: &str) -> Option<i64> {
    let md = std::fs::metadata(path).ok()?;
    let created = md.created().ok()?;
    let since_epoch = created
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    i64::try_from(since_epoch.as_secs()).ok()
}

/// Keep only the paths whose creation time falls in `range`.
///
/// The clock is injected so the rule can be tested without creating files with
/// chosen birth times (which no portable API allows): production passes
/// [`created_secs`]. Order is preserved, so the daemon's hit order survives.
pub fn filter_by_created(
    paths: &mut Vec<String>,
    range: &CreatedRange,
    created: impl Fn(&str) -> Option<i64>,
) -> CreatedStats {
    let mut stats = CreatedStats::default();
    paths.retain(|path| {
        stats.examined += 1;
        match created(path) {
            Some(t) if range.contains(t) => {
                stats.kept += 1;
                true
            }
            Some(_) => false,
            None => {
                stats.undated += 1;
                false
            }
        }
    });
    stats
}

/// The status-line suffix for an active created range.
///
/// It states what was actually checked, because the daemon's `total` is the
/// count for the *name* query: the range can only be applied to the page that
/// came back. `fetched` is how many paths the reply held and `total` the
/// daemon's match count; when the reply was truncated the suffix says how many
/// further matches went unchecked instead of letting the filtered count read
/// as the total. A count-only search has no paths to filter, so the suffix says
/// the range was not applied rather than implying it was.
pub fn created_note(
    range: &CreatedRange,
    stats: &CreatedStats,
    fetched: usize,
    total: u64,
    count_only: bool,
) -> String {
    if count_only {
        return "  [created range not applied to a count-only search]".to_string();
    }
    let mut s = format!("  [created {}", range.label());
    if stats.kept != stats.examined {
        s.push_str(&format!(": {} of {} matched", stats.kept, stats.examined));
    }
    if stats.undated > 0 {
        s.push_str(&format!("; {} had no creation time", stats.undated));
    }
    let unread = total.saturating_sub(fetched as u64);
    if unread > 0 {
        s.push_str(&format!("; {unread} more matches not checked"));
    }
    s.push(']');
    s
}

/// The range a search should apply, or the status-line refusal that names the
/// field which is wrong.
///
/// This is what `main.rs`'s request builder calls, and what
/// `tests/created_range_gtk.rs` calls, so the refusal the user sees in the
/// status line and the refusal the test reads are the same string.
pub fn created_request_range(from: &str, to: &str) -> Result<Option<CreatedRange>, GuiError> {
    parse_created_range(from, to).map_err(GuiError::Protocol)
}

/// Whether an edit to a created-date field should re-run the search.
///
/// True once the pair parses (or both fields are blank). A half-typed date
/// does not search — `Search`/`Return` reports the error instead, so typing
/// `2026-0` does not flash an error on every keystroke.
pub fn created_edit_searches(from: &str, to: &str) -> bool {
    parse_created_range(from, to).is_ok()
}

/// Apply `range` to a reply and return the status-line suffix that states what
/// was checked (`""` when no range is active).
///
/// The daemon's total describes the *name* query, so the range can only be
/// applied to the page that came back; [`created_note`] says so. A count-only
/// reply carries no paths, so the note says the range was not applied rather
/// than implying it was.
pub fn apply_created_range(
    outcome: &mut SearchOutcome,
    range: Option<CreatedRange>,
    count_only: bool,
) -> String {
    let Some(range) = range else {
        return String::new();
    };
    if count_only {
        return created_note(&range, &CreatedStats::default(), 0, outcome.total, true);
    }
    // `fetched` is the page the daemon sent, before the range removes rows from
    // it: "more matches not checked" is about rows the reply never held, so it
    // must not count the ones the range itself hid.
    let fetched = outcome.paths.len();
    // The daemon dated these rows at index time; only a row it could not date
    // costs a `stat`. Taking the map out first keeps the closure from borrowing
    // `outcome` twice, and it is put back so the field stays meaningful.
    let times = std::mem::take(&mut outcome.created);
    let stats = filter_by_created(&mut outcome.paths, &range, |path| {
        times
            .get(path)
            .map(|min| *min as i64 * 60)
            .or_else(|| created_secs(path))
    });
    outcome.created = times;
    created_note(&range, &stats, fetched, outcome.total, false)
}

/// The rows a streaming reply should paint under `range`.
///
/// Filtering the as-you-type paint with the same rule as the final render is
/// what stops rows outside the range from flashing up and then vanishing.
pub fn filter_partial_paths(hits: &[String], range: Option<CreatedRange>) -> Vec<String> {
    match range {
        Some(r) => hits
            .iter()
            .filter(|p| created_secs(p).map_or(false, |t| r.contains(t)))
            .cloned()
            .collect(),
        None => hits.to_vec(),
    }
}

/// The Advanced foldout's created-date controls, built by one function so the
/// app and the GTK regression test create the very same widgets.
///
/// The row lives here rather than in `main.rs` because `main.rs` is a binary:
/// nothing can link against it, so a test that builds its own look-alike row
/// would prove nothing about the foldout the user actually opens.
pub mod created_ui {
    use rswidgets::prelude::App;

    /// The foldout's concrete widget types on this backend.
    ///
    /// The portable prelude re-exports the `common` wrappers of these names,
    /// but `App::create_entry`/`create_label`/`create_box` hand back the
    /// adapter's own types — the ones `main.rs` names in its trait impls — so
    /// the row must name those too. One arm per backend, mirroring
    /// `main.rs`'s `widget_traits`.
    #[cfg(all(feature = "gtk", target_os = "linux", not(feature = "zork")))]
    mod widgets {
        pub type Entry = rswidgets::backends_gtk_adapter::Entry;
        pub type Label = rswidgets::backends_gtk_adapter::Label;
        pub type Row = rswidgets::backends_gtk_adapter::BoxWidget;
    }
    #[cfg(feature = "zork")]
    mod widgets {
        pub type Entry = rswidgets::backends_zork_adapter::Entry;
        pub type Label = rswidgets::backends_zork_adapter::Label;
        pub type Row = rswidgets::backends_zork_adapter::BoxWidget;
    }
    #[cfg(all(windows, not(feature = "zork")))]
    mod widgets {
        pub type Entry = rswidgets::backends_nwg_adapter::Entry;
        pub type Label = rswidgets::backends_nwg_adapter::Label;
        pub type Row = rswidgets::backends_nwg_adapter::BoxWidget;
    }
    #[cfg(all(target_arch = "wasm32", not(feature = "zork")))]
    mod widgets {
        pub type Entry = rswidgets::backends_wasm_adapter::Entry;
        pub type Label = rswidgets::backends_wasm_adapter::Label;
        pub type Row = rswidgets::backends_wasm_adapter::BoxWidget;
    }
    #[cfg(all(target_os = "macos", not(feature = "zork")))]
    mod widgets {
        pub type Entry = rswidgets::backends_macos_adapter::Entry;
        pub type Label = rswidgets::backends_macos_adapter::Label;
        pub type Row = rswidgets::backends_macos_adapter::BoxWidget;
    }

    use widgets::{Entry, Label, Row};

    /// The `created from: [date] to: [date]` row of the foldout.
    pub struct CreatedRow {
        pub from: Entry,
        pub to: Entry,
        pub from_label: Label,
        pub to_label: Label,
        /// The horizontal box holding the two labels and both fields.
        pub row: Row,
    }

    /// The labels the row shows. Named so the GTK test can assert that the
    /// text on screen is the text it is looking for.
    pub const FROM_LABEL: &str = "created from:";
    pub const TO_LABEL: &str = "to:";
    /// The grey hint shown inside either field while it is empty.
    pub const PLACEHOLDER: &str = "YYYY-MM-DD";

    /// Build the row, ready to append to the foldout's vertical box.
    pub fn build(app: &App) -> Result<CreatedRow, rswidgets::Error> {
        let from_label = app.create_label(FROM_LABEL)?;
        let to_label = app.create_label(TO_LABEL)?;
        let from = app.create_entry()?;
        from.set_text("");
        from.set_placeholder_text(PLACEHOLDER);
        let to = app.create_entry()?;
        to.set_text("");
        to.set_placeholder_text(PLACEHOLDER);
        let row = app.create_box(rswidgets::prelude::Orientation::Horizontal, 2)?;
        row.append(&from_label);
        row.append(&from);
        row.append(&to_label);
        row.append(&to);
        Ok(CreatedRow {
            from,
            to,
            from_label,
            to_label,
            row,
        })
    }

    /// Re-run the search when a date edit leaves a readable range (or both
    /// fields blank). The decision is [`super::created_edit_searches`], the
    /// function `main.rs` uses, so a half-typed date cannot silently search.
    pub fn wire(row: &CreatedRow, arm: impl Fn() + Clone + 'static) {
        let from = row.from.clone();
        let to = row.to.clone();
        for entry in [row.from.clone(), row.to.clone()] {
            let arm = arm.clone();
            let from = from.clone();
            let to = to.clone();
            entry
                .connect_changed(move || {
                    let f = from.get_text().unwrap_or_default();
                    let t = to.get_text().unwrap_or_default();
                    if super::created_edit_searches(&f, &t) {
                        arm();
                    }
                })
                .ok();
        }
    }
}

/// What a search returned, including the consistency metadata (§11).
#[derive(Clone, Debug, Default)]
pub struct SearchOutcome {
    pub paths: Vec<String>,
    /// Creation time per returned path, in whole minutes since the unix epoch,
    /// as the daemon reported it. Absent for a path the daemon could not date
    /// (or a daemon too old to send it); [`apply_created_range`] falls back to a
    /// `stat` for those only.
    pub created: HashMap<String, u32>,
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
    /// What the daemon said it was doing when it answered — its own word, not
    /// a client guess (`Meta.phase`, see `fflocate::health::Phase`). `None`
    /// when the daemon predates the field; callers then fall back to
    /// `catching_up`/`lag_ms` and never invent a phase.
    pub phase: Option<fflocate::health::Phase>,
    /// Entries discovered so far, when `phase == Crawling` (`Meta.crawl_entries`).
    pub crawl_entries: Option<u64>,
    /// Directories still queued to enumerate, when `phase == Crawling`.
    pub crawl_dirs_pending: Option<u64>,
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
        // The daemon's own phase ("what is it doing") takes precedence over the
        // generic catching-up flag: if it told us it is crawling, say that.
        if let Some(suffix) = phase_suffix(self.phase, self.crawl_entries, self.crawl_dirs_pending)
        {
            s.push_str(&suffix);
        } else if let Some(suffix) = catching_up_suffix(self.catching_up, self.lag_ms) {
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

/// The daemon's self-reported state as a status-line suffix, or `None` when
/// there is nothing honest to add (idle daemon).
///
/// This replaces the old "starting fflocated… Ns" line. That line's number was
/// the *client's* process age since it spawned a daemon — it kept climbing for
/// the life of the GUI and was applied to any query that had not yet produced
/// a Hit, so a warm daemon mid-search read as "starting fflocated… 44.0s".
/// The daemon itself already knows better, and now says so: `phase` names what
/// it is doing (replaying the journal, crawling, re-verifying) and, while
/// crawling, how far along the discovery is.
///
/// Shapes:
/// * crawling with progress → `[daemon crawling — 1,234,567 entries, 41 dirs left]`
/// * crawling without progress → `[daemon crawling]`
/// * replaying → `[daemon replaying journal]`
/// * re-verifying → `[daemon re-verifying]`
/// * idle → `None` (a caught-up daemon needs no excuse)
///
/// An old daemon sends no `phase`: `None` in, `None` out — the caller then
/// falls back to [`catching_up_suffix`] and never invents a cause.
pub fn phase_suffix(
    phase: Option<fflocate::health::Phase>,
    crawl_entries: Option<u64>,
    crawl_dirs_pending: Option<u64>,
) -> Option<String> {
    let phase = phase?;
    match phase {
        fflocate::health::Phase::Idle => None,
        fflocate::health::Phase::Crawling => {
            let mut s = String::from("  [daemon crawling");
            if let Some(entries) = crawl_entries {
                s.push_str(&format!(" — {} entries", thousands(entries)));
                if let Some(dirs) = crawl_dirs_pending {
                    s.push_str(&format!(", {} dirs left", thousands(dirs)));
                }
            }
            s.push(']');
            Some(s)
        }
        fflocate::health::Phase::Replaying => Some("  [daemon replaying journal]".to_string()),
        fflocate::health::Phase::Rescanning => Some("  [daemon re-verifying]".to_string()),
    }
}

/// Group a count with thin spaces every three digits (`1234567` →
/// `1 234 567`), so a seven-figure crawl count is readable at a glance rather
/// than a wall of digits. Pure and total; used only for display.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// Most rows a whole-set sort load will pull before it stops.
///
/// The list draws every row it is given (it paints only the band on screen),
/// but the load itself costs memory: a million paths is roughly 60-100 MB in
/// this process (the dedupe set holds its own copy). Past that the load stops
/// and the status line says so rather than growing without bound.
pub const MAX_SORTED_ROWS: usize = 1_000_000;

/// A whole-result-set load, started when a header click has to sort matches
/// that the first page never fetched.
///
/// The daemon treats `limit == 0` as "no limit" (that is what `ff -l 0`
/// documents), so one request returns every hit, streamed as they are found.
/// One request rather than paging: the daemon's per-request cap means each
/// extra page rescans from the start, so N pages cost O(N²/page) scans and N
/// acquisitions of the index read lock. One unlimited request is one scan, one
/// snapshot, and the hits arrive while it runs.
#[derive(Debug)]
pub struct FullSet {
    query: String,
    options: SearchOptions,
    /// Matches the daemon reported. Seeded from the first page's `Done`, then
    /// corrected by this reply's own `Done`.
    total: u64,
    /// Distinct hits absorbed so far.
    loaded: usize,
    cap: usize,
    /// Paths already counted, so a hit is never shown twice.
    seen: HashSet<String>,
    done: bool,
    capped: bool,
    failed: Option<String>,
}

impl FullSet {
    pub fn new(
        query: impl Into<String>,
        options: SearchOptions,
        total: u64,
        cap: usize,
    ) -> Self {
        Self {
            query: query.into(),
            options,
            total,
            loaded: 0,
            cap,
            seen: HashSet::new(),
            done: false,
            capped: false,
            failed: None,
        }
    }

    /// The request that returns every hit: the same query with `limit = 0`.
    pub fn request(&self) -> Request {
        DaemonClient::query_request(&self.query, 0, &self.options)
    }

    /// Fold one batch of reply records in, returning the paths worth showing.
    ///
    /// A duplicate is dropped: a hit is a path, and the same path twice in the
    /// list is a bug the user can see. The cap is enforced by counting only —
    /// the reply is still drained to its terminal record so `total` stays
    /// right and the socket is closed on schedule.
    pub fn absorb(&mut self, records: &[Response]) -> Vec<String> {
        let mut fresh = Vec::new();
        for r in records {
            match r {
                Response::Hit { path, .. } => {
                    if self.loaded >= self.cap {
                        self.capped = true;
                        continue;
                    }
                    if self.seen.insert(path.clone()) {
                        self.loaded += 1;
                        fresh.push(path.clone());
                    }
                }
                Response::Done { count, .. } => {
                    self.total = (*count).max(self.total);
                    self.done = true;
                }
                Response::Error { message } => {
                    self.failed = Some(message.clone());
                    self.done = true;
                }
                // A query reply carries Meta/Progress/Hit/Done only; anything
                // else is not this reply's business.
                _ => {}
            }
        }
        fresh
    }

    /// Record a transport failure (the reply never reached `Done`).
    pub fn fail(&mut self, message: &str) {
        self.failed = Some(message.to_string());
        self.done = true;
    }

    pub fn loaded(&self) -> usize {
        self.loaded
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn done(&self) -> bool {
        self.done
    }

    pub fn capped(&self) -> bool {
        self.capped
    }

    pub fn failed(&self) -> bool {
        self.failed.is_some()
    }

    /// The progress line, in the shapes the status bar uses.
    pub fn status_line(&self) -> String {
        if let Some(e) = &self.failed {
            return format!("sort load failed: {e}");
        }
        if self.done {
            // The cap is only the headline once the reply has ended: while it
            // streams, the progress count is the more useful number.
            if self.capped {
                return format!("stopped at {} rows", thousands(self.cap as u64));
            }
            return format!("sorted {} matches", thousands(self.total));
        }
        format!(
            "loading all matches: {} of {}",
            thousands(self.loaded as u64),
            thousands(self.total)
        )
    }

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
/// The daemon streams: `Meta` is emitted first, before the scan takes the
/// index guard, then each `Hit` as it is found (the daemon flushes per
/// record, so records leave the kernel immediately rather than waiting for
/// `Done`). A slow query — a cold trigram index on first search, a daemon
/// still crawling — therefore *is* distinguishable from a wedged daemon
/// before the scan finishes: `hits_so_far` and `progress` move as it goes.
/// This line is the honest signal for the gap that remains: the request
/// left, no `Hit` has arrived yet, and here is how long that has been true.
/// `hits_so_far` counts `Hit` records parsed from partial packets, so it is
/// real data from the daemon, not a local guess.
///
/// `phase`/`crawl_entries`/`crawl_dirs_pending` are the daemon's own words
/// about what it is doing, taken from the `Meta` preamble as soon as it lands.
/// When present they take precedence over the generic `catching_up` suffix: a
/// daemon that says "crawling — 1.2M entries" has explained itself, and
/// appending "catching up" on top would be redundant.
#[allow(clippy::too_many_arguments)]
pub fn searching_status_line(
    query: &str,
    elapsed: std::time::Duration,
    hits_so_far: usize,
    catching_up: bool,
    progress: Option<u8>,
    lag_ms: Option<u64>,
    phase: Option<fflocate::health::Phase>,
    crawl_entries: Option<u64>,
    crawl_dirs_pending: Option<u64>,
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
    // The daemon's own phase wins over the bare catching-up flag; fall back to
    // the flag only for a daemon that sends no phase (an older one).
    if let Some(suffix) = phase_suffix(phase, crawl_entries, crawl_dirs_pending) {
        s.push_str(&suffix);
    } else if let Some(suffix) = catching_up_suffix(catching_up, lag_ms) {
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
            // `open` is macOS's dispatcher; xdg-open is the freedesktop one.
            // Without this branch the app spawns `xdg-open` on macOS and every
            // Open (context menu, double click) fails with ENOENT.
            #[cfg(target_os = "macos")]
            RowAction::Open => ("open", vec![path.to_string()]),
            // xdg-open dispatches on a file or a directory identically.
            #[cfg(not(target_os = "macos"))]
            RowAction::Open => ("xdg-open", vec![path.to_string()]),
            // `open -R` reveals the file in Finder.
            #[cfg(target_os = "macos")]
            RowAction::OpenContainingFolder => ("open", vec!["-R".into(), path.to_string()]),
            #[cfg(not(target_os = "macos"))]
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
            // Terminal.app opens in the directory it is given.
            #[cfg(target_os = "macos")]
            RowAction::OpenInTerminal => (
                "open",
                vec!["-a".into(), "Terminal".into(), containing_dir(path)],
            ),
            #[cfg(not(target_os = "macos"))]
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

    /// All actions, in menu order. This is the row menu, on every backend.
    ///
    /// The mission's requirement names four actions (Open, Open containing
    /// folder, Copy path, Open in terminal) and this list is a superset of them:
    /// `Copy folder path` and `Open with…` are real commands the app has shipped
    /// and whose launch/pasteboard behaviour is tested here, so they are kept
    /// rather than deleted to match a shorter list. The decision is recorded
    /// here, next to the definition, and pinned by `tests/row_menu_model.rs`:
    /// that test asserts this exact six-item list *and* the four required `label`s
    /// by name, all enabled and all dispatching, so removing a required item or
    /// renaming one fails the suite. A backend that draws a row menu draws this
    /// list — a surface missing one of the four cannot reach a user without that
    /// test failing.
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
/// Terminals "Open in terminal" knows how to start, most preferred first.
///
/// `x-terminal-emulator` is the Debian alternatives link; it does not exist on
/// Fedora, Arch or openSUSE, where a KDE desktop ships `konsole` and nothing
/// answers to that name. Listing the real binaries first is what makes the
/// action work outside Debian at all — the alternatives link stays as one
/// candidate rather than the only one.
pub const TERMINAL_CANDIDATES: &[&str] = &[
    "konsole",
    "kgx",
    "gnome-terminal",
    "xfce4-terminal",
    "kitty",
    "alacritty",
    "foot",
    "wezterm",
    "x-terminal-emulator",
    "xterm",
];

/// Programs that open a directory in the desktop's file manager, most
/// preferred first.
///
/// `xdg-open` delegates to whatever the desktop registered and is the right
/// first choice; the rest are fallbacks for a system without xdg-utils, where
/// the desktop's own file manager still is.
pub const FOLDER_OPENER_CANDIDATES: &[&str] = &[
    "xdg-open",
    "kde-open5",
    "kde-open",
    "dolphin",
    "nautilus",
    "thunar",
    "nemo",
    "pcmanfm",
];

/// The entries of `candidates` that exist on `PATH`, in the same order.
pub fn available_programs(candidates: &[&'static str]) -> Vec<&'static str> {
    candidates
        .iter()
        .copied()
        .filter(|p| find_on_path(p).is_some())
        .collect()
}

/// The terminal to start: `$TERMINAL` when it names a program, else the first
/// candidate the system has.
///
/// `available` is `TERMINAL_CANDIDATES` filtered to what is on `PATH`, so
/// "first" means first in preference order, not first alphabetically.
pub fn pick_terminal(env_terminal: Option<&str>, available: &[&str]) -> Option<String> {
    if let Some(t) = env_terminal.map(str::trim).filter(|t| !t.is_empty()) {
        return Some(t.to_string());
    }
    available.first().map(|p| (*p).to_string())
}

/// The arguments that make `program` start in `dir`.
///
/// Terminals disagree on the flag: KDE's konsole wants `--workdir`, GNOME's
/// kgx and gnome-terminal want `--working-directory`, kitty `--directory`,
/// wezterm `start --cwd`. Anything else — the Debian alternatives link, xterm,
/// zutty — gets the portable `-e sh -c 'cd …; exec $SHELL'`, which also works
/// for a terminal whose flag is unknown.
pub fn terminal_argv(program: &str, dir: &str) -> Vec<String> {
    let name = Path::new(program)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string())
        .to_ascii_lowercase();
    if name.starts_with("konsole") {
        vec!["--workdir".to_string(), dir.to_string()]
    } else if name.starts_with("kitty") {
        vec!["--directory".to_string(), dir.to_string()]
    } else if name.starts_with("wezterm") {
        vec!["start".to_string(), "--cwd".to_string(), dir.to_string()]
    } else if name.starts_with("kgx")
        || name.starts_with("gnome-terminal")
        || name.starts_with("xfce4-terminal")
        || name.starts_with("alacritty")
        || name.starts_with("foot")
    {
        vec!["--working-directory".to_string(), dir.to_string()]
    } else {
        vec![
            "-e".to_string(),
            "sh".to_string(),
            "-c".to_string(),
            terminal_shell_command(dir),
        ]
    }
}

/// The command "Open in terminal" runs.
///
/// macOS hands the directory to Terminal.app. Elsewhere the terminal is picked
/// from `$TERMINAL` or [`TERMINAL_CANDIDATES`]: the alternatives link alone is
/// absent on a KDE system, which is why the action did nothing there.
pub fn open_in_terminal_command(path: &str) -> (String, Vec<String>) {
    // macOS and Windows have one dispatcher each (`open -a`, `cmd /c start`);
    // the terminal hunting below is the unix-without-a-dispatcher case.
    #[cfg(any(windows, target_os = "macos"))]
    {
        let (prog, base) = RowAction::OpenInTerminal.command(path);
        return (prog.to_string(), base);
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let available = available_programs(TERMINAL_CANDIDATES);
        let env_terminal = std::env::var("TERMINAL").ok();
        let program = pick_terminal(env_terminal.as_deref(), &available)
            .unwrap_or_else(|| "x-terminal-emulator".to_string());
        let dir = containing_dir(path);
        let args = with_terminal_font_args(
            terminal_argv(&program, &dir),
            terminal_font_arg_for(&program),
        );
        (program, args)
    }
}

/// The command "Open containing folder" runs.
///
/// macOS reveals the file in Finder. Elsewhere the directory is opened by the
/// first available opener in [`FOLDER_OPENER_CANDIDATES`], so a system without
/// xdg-utils still opens it in its own file manager.
pub fn open_containing_folder_command(path: &str) -> (String, Vec<String>) {
    #[cfg(any(windows, target_os = "macos"))]
    {
        let (prog, base) = RowAction::OpenContainingFolder.command(path);
        return (prog.to_string(), base);
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
        let order = folder_opener_order(desktop.as_deref());
        let available: Vec<&'static str> = order
            .into_iter()
            .filter(|p| find_on_path(p).is_some())
            .collect();
        let program = available
            .first()
            .map(|p| (*p).to_string())
            .unwrap_or_else(|| "xdg-open".to_string());
        let args = folder_opener_argv(&program, path);
        (program, args)
    }
}

/// The openers to try, best first, for the desktop named in
/// `$XDG_CURRENT_DESKTOP`.
///
/// `xdg-open` is a shell script that hands off to `kde-open`/`gio`, which on
/// KDE keeps running after the window is up; going straight to the desktop's
/// own file manager is one process, and it can highlight the file — which is
/// what "open containing folder" is for. On a desktop that is not recognised,
/// `xdg-open` stays first because it is the one name that dispatches correctly
/// everywhere.
pub fn folder_opener_order(desktop: Option<&str>) -> Vec<&'static str> {
    let desktop = desktop.unwrap_or("").to_ascii_uppercase();
    let preferred: &[&str] = if desktop.contains("KDE") {
        &["dolphin", "kde-open5", "kde-open"]
    } else if desktop.contains("GNOME") || desktop.contains("UNITY") || desktop.contains("CINNAMON") {
        &["nautilus"]
    } else if desktop.contains("XFCE") {
        &["thunar"]
    } else if desktop.contains("MATE") {
        &["caja"]
    } else {
        &[]
    };
    // The desktop's own openers first, then the generic list, each name once:
    // `Vec::dedup` only drops neighbours, and xdg-open sits in the middle of
    // the generic list rather than at the end of the preferred one.
    let mut order: Vec<&'static str> = Vec::new();
    for program in preferred.iter().chain(FOLDER_OPENER_CANDIDATES.iter()) {
        if !order.contains(program) {
            order.push(program);
        }
    }
    order
}

/// The arguments that open `path`'s directory in `program`.
///
/// `dolphin --select <file>` opens the folder *with the file highlighted*,
/// which is the whole point of the action; everything else gets the directory
/// and shows it, without a selection.
pub fn folder_opener_argv(program: &str, path: &str) -> Vec<String> {
    let name = Path::new(program)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string())
        .to_ascii_lowercase();
    if name.starts_with("dolphin") {
        vec!["--select".to_string(), path.to_string()]
    } else {
        vec![containing_dir(path)]
    }
}

/// The `-font` family to pass, if any: only for zutty (whose `9x18` default
/// aborts when `xfonts-base` is missing) and when an installed monospace
/// family was found.
pub fn terminal_font_arg_for(program: &str) -> Option<&'static str> {
    let base = Path::new(program)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string());
    if is_zutty_basename(&base) {
        terminal_monospace_font()
    } else {
        None
    }
}

/// The `-font` family for the Debian alternatives target, if it is zutty.
pub fn terminal_font_arg() -> Option<&'static str> {
    match terminal_emulator_basename() {
        Some(base) if is_zutty_basename(&base) => terminal_monospace_font(),
        _ => None,
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
    /// Icon *name* for the file kind (e.g. `folder`, `text-x-generic`). The
    /// canvas list paints a pictogram from it; a text backend prints a glyph.
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
/// Run a row action for `path` and return the status line the app shows.
///
/// `copy` is how the caller puts text on the clipboard (the app passes the
/// list's own clipboard, the probe passes the compat one), so the same runner
/// serves both and the probe exercises the handler the app really uses.
/// The program and arguments that carry out `action` on `path`, resolved
/// against this host (which terminal exists, which opener exists).
pub fn launch_command(action: RowAction, path: &str) -> (String, Vec<String>) {
    match action {
        RowAction::OpenInTerminal => open_in_terminal_command(path),
        RowAction::OpenContainingFolder => open_containing_folder_command(path),
        _ => {
            let (prog, args) = action.command(path);
            (prog.to_string(), args)
        }
    }
}

pub fn run_row_action<F>(action: RowAction, path: &str, copy: F) -> String
where
    F: Fn(&str) -> Result<(), String>,
{
    run_row_action_with(action, path, copy, |program, args| {
        // The children own their own windows and are never read from: detaching
        // stdio keeps them from holding this process's pipes open (a `dolphin`
        // or `kde-open` that outlives the app would otherwise keep a shell
        // waiting on the pipe for as long as the window is up) and keeps their
        // diagnostics out of the app's output.
        std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
}

/// [`run_row_action`] with the launcher supplied by the caller.
///
/// The one thing a caller cannot inject into [`run_row_action`] is the spawn
/// itself, which makes the dispatch untestable on a machine where launching a
/// file manager would open a window. This takes it as a closure, so a test can
/// record which command each menu item runs; the app uses the spawn above, and
/// the status line is the same either way.
/// The rows an as-you-type (streaming) tick paints, plus every hit it kept.
///
/// `main.rs` paints partial hits into the same `TreeView` the settled answer
/// uses — one row list, one hit test, one index mapping — so the partial paint
/// follows the settled rules: rows outside the created range are dropped (a hit
/// must not flash up and then vanish when the final render filters it) and the
/// paint is bounded to `max_rows`. Returning both lets the status line count
/// every kept hit while the list shows the first page. The row-menu tests paint
/// through this function, so "the row the pointer is on" mid-stream is resolved
/// by the app's rule rather than a re-implementation of it.
pub fn streaming_rows(
    hits: &[String],
    range: Option<CreatedRange>,
    max_rows: usize,
) -> (Vec<String>, Vec<String>) {
    let kept = filter_partial_paths(hits, range);
    let shown = kept.iter().take(max_rows).cloned().collect();
    (kept, shown)
}

pub fn run_row_action_with<F, L>(action: RowAction, path: &str, copy: F, launch: L) -> String
where
    F: Fn(&str) -> Result<(), String>,
    L: Fn(&str, &[String]) -> Result<(), String>,
{
    let (program, args) = launch_command(action, path);
    if program.is_empty() {
        let text = match action {
            RowAction::CopyFolderPath => containing_dir(path),
            _ => path.to_string(),
        };
        return match copy(&text) {
            // Name what is now on the clipboard: the whole path, because
            // "copied notes.txt" says nothing about *which* notes.txt when a
            // search returns several.
            Ok(()) => format!("copied {text}"),
            Err(e) => format!("copy failed: {e}"),
        };
    }
    match launch(&program, &args) {
        // Name the program: "Open containing folder → photo.png (dolphin)"
        // says which helper actually ran, which is the difference between
        // "the app did nothing" and "the app started a file manager".
        Ok(()) => format!(
            "{} → {} ({program})",
            action.label(),
            short_path(path)
        ),
        Err(e) => format!("{} failed to start {program}: {e}", action.label()),
    }
}

/// One item of the row context menu, described without a toolkit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowMenuItem {
    /// The action choosing this item runs.
    pub action: RowAction,
    /// The label every desktop backend draws for it.
    pub label: &'static str,
    /// Whether it can be chosen. False when the pointer was not over a row:
    /// the app then opens no menu at all, and a backend that shows one anyway
    /// must show these greyed out.
    pub enabled: bool,
}

/// The row context menu as a list, independent of any toolkit.
///
/// The items are the same on every backend, in the same order (the app's
/// [`RowAction::all`], whose doc records why the list is the six-item superset of
/// the mission's four); only their drawing differs per platform. `target` is the
/// path the menu would act on — [`context_menu_target`]'s answer — so the
/// enabled rule is testable, and asserted, without a display.
///
/// The settled list and the as-you-type (streaming/partial) list feed this same
/// function from the same painted rows: `main.rs` paints both into one
/// `TreeView`, so there is no second row list whose indices could drift.
pub fn row_menu_model(target: Option<&str>) -> Vec<RowMenuItem> {
    let enabled = target.is_some();
    RowAction::all()
        .iter()
        .map(|&action| RowMenuItem {
            action,
            label: action.label(),
            enabled,
        })
        .collect()
}

/// The last component of `path`, for a status line.
pub fn short_path(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[i + 1..].to_string(),
        None => path.to_string(),
    }
}

/// Wire `list` so double-clicking a row opens that row's path.
///
/// The app installs this on its results list, and the macOS probe installs the
/// same function, so the probe drives the handler the app really uses instead
/// of a stand-in. `report` receives the status line the app would show.
pub fn connect_open_on_double_click<F>(list: &rswidgets::compat::TreeView, path_column: usize, mut report: F)
where
    F: FnMut(String) + 'static,
{
    let rows = list.clone();
    list.connect_row_activated(Box::new(move |row| {
        let Some(path) = rows.cell_text(row, path_column) else {
            report(format!("Open failed: row {row} has no path"));
            return;
        };
        // The same runner the context menu uses, so a double click and the
        // menu's Open cannot drift apart.
        let line = run_row_action(RowAction::Open, &path, |text| {
            rswidgets::compat::clipboard_set_text(text).map_err(|e| e.to_string())
        });
        report(line);
    }));
}

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
        "sh" | "bash" | "zsh" | "fish" | "ksh" | "csh" | "tcsh" | "bat" | "cmd" | "ps1"
        | "psm1" | "vbs" => "text-x-script",
        "md" | "markdown" => "text-markdown",
        "json" => "application-json",
        "toml" | "yaml" | "yml" | "ini" | "cfg" => "text-x-generic",
        "html" | "htm" => "text-html",
        "css" => "text-css",
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "bmp" => "image-x-generic",
        "mp3" | "wav" | "flac" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "wma" | "aiff"
        | "aif" | "alac" | "ape" | "mid" | "midi" | "amr" => "audio-x-generic",
        // `.ts` is TypeScript here, so the video set leaves it out (as do
        // `.m2ts` and `.ogv`, which no other kind claims).
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" | "mpg" | "mpeg" | "m4v"
        | "3gp" | "m2ts" | "vob" | "ogv" => "video-x-generic",
        "csv" | "tsv" | "xls" | "xlsx" | "ods" => "x-office-spreadsheet",
        // Machine code: programs, and the object/library files they are built
        // from. Both draw the same pictogram; the name still says which it is.
        "exe" | "com" | "dll" | "so" | "dylib" | "bin" | "elf" | "out" | "app"
        | "appimage" | "ko" | "sys" | "run" | "msc" => "application-x-executable",
        "o" | "obj" | "a" | "lib" => "application-x-object",
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
        assert_eq!(icon_name_for("song.mp3", false), "audio-x-generic");
        assert_eq!(icon_name_for("clip.MKV", false), "video-x-generic");
        assert_eq!(icon_name_for("sheet.csv", false), "x-office-spreadsheet");
        // A `.ts` file is TypeScript, not a video transport stream.
        assert_eq!(icon_name_for("app.ts", false), "text-x-typescript");
        assert_eq!(icon_name_for("setup.exe", false), "application-x-executable");
        // `.dll` and `.so` are shared libraries, but they are still code, not
        // text: they group with the programs rather than with documents.
        assert_eq!(icon_name_for("ffi.DLL", false), "application-x-executable");
        assert_eq!(icon_name_for("libff.so", false), "application-x-executable");
        assert_eq!(icon_name_for("main.o", false), "application-x-object");
        assert_eq!(icon_name_for("libff.a", false), "application-x-object");
        assert_eq!(icon_name_for("build.sh", false), "text-x-script");
        assert_eq!(icon_name_for("run.BAT", false), "text-x-script");
        assert_eq!(icon_name_for("deploy.ps1", false), "text-x-script");
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
            // Open launches the file itself: `open` on macOS, xdg-open on the
            // freedesktop platforms.
            let (prog, args) = RowAction::Open.command("/a/b.rs");
            #[cfg(target_os = "macos")]
            assert_eq!(prog, "open");
            #[cfg(not(target_os = "macos"))]
            assert_eq!(prog, "xdg-open");
            assert_eq!(args, vec!["/a/b.rs".to_string()]);
            // ...while "containing folder" launches the parent directory.
            let (prog, args) = RowAction::OpenContainingFolder.command("/a/b.rs");
            #[cfg(target_os = "macos")]
            assert_eq!(prog, "open");
            #[cfg(not(target_os = "macos"))]
            assert_eq!(prog, "xdg-open");
            if cfg!(target_os = "macos") {
                assert_eq!(args, vec!["-R".to_string(), "/a/b.rs".to_string()]);
            } else {
                assert_eq!(args, vec!["/a".to_string()]);
            }
            // "Open with" asks mimeopen to list handlers for this very file.
            let (prog, args) = RowAction::OpenWith.command("/a/b.rs");
            assert_eq!(prog, "mimeopen");
            assert_eq!(args, vec!["-a".to_string(), "/a/b.rs".to_string()]);
            // A terminal is launched in the file's directory, with a shell
            // (on macOS: Terminal.app, given the directory as its argument).
            let (prog, args) = RowAction::OpenInTerminal.command("/a/b.rs");
            #[cfg(target_os = "macos")]
            assert_eq!(prog, "open");
            #[cfg(not(target_os = "macos"))]
            assert_eq!(prog, "x-terminal-emulator");
            if cfg!(target_os = "macos") {
                assert_eq!(
                    args,
                    vec!["-a".to_string(), "Terminal".to_string(), "/a".to_string()]
                );
                return;
            }
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
        #[cfg(target_os = "macos")]
        {
            let (_, args) = RowAction::OpenInTerminal.command("/home/me/src/main.rs");
            assert_eq!(args.last().map(String::as_str), Some("/home/me/src"));
            // A bare name has no usable parent; the name itself is the answer.
            let (_, args) = RowAction::OpenInTerminal.command("b.rs");
            assert_eq!(args.last().map(String::as_str), Some("b.rs"));
        }
        #[cfg(all(not(windows), not(target_os = "macos")))]
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
        // macOS has no alternatives link to splice: `open -a Terminal <dir>`.
        #[cfg(target_os = "macos")]
        {
            let (prog, args) = open_in_terminal_command("/a/b.rs");
            assert_eq!(prog, "open");
            assert_eq!(
                args,
                vec!["-a".to_string(), "Terminal".to_string(), "/a".to_string()]
            );
        }
        // Unix: whichever terminal this host has, launched in the file's
        // directory. Which one is host-dependent ($TERMINAL, then what is
        // installed), so the test pins the *contract*: a real terminal, and the
        // directory reaching it either through the terminal's own flag or the
        // portable `cd` body.
        #[cfg(all(not(windows), not(target_os = "macos")))]
        {
            let (prog, args) = open_in_terminal_command("/a/b.rs");
            let env_terminal = std::env::var("TERMINAL").ok();
            let available = available_programs(TERMINAL_CANDIDATES);
            if env_terminal.is_some() || !available.is_empty() {
                assert!(
                    env_terminal.as_deref() == Some(prog.as_str())
                        || available.contains(&prog.as_str()),
                    "{prog} is neither $TERMINAL nor an installed candidate"
                );
            }
            assert_eq!(
                args,
                with_terminal_font_args(
                    terminal_argv(&prog, "/a"),
                    terminal_font_arg_for(&prog)
                ),
                "{args:?}"
            );
            // The directory is in there, one way or the other.
            assert!(
                args.iter().any(|a| a == "/a") || args.join(" ").contains("cd '/a'"),
                "{args:?}"
            );
            // And nothing that starts a terminal in the wrong place: a trailing
            // `-e` with no command, or an empty argument, would launch a bare
            // shell in the GUI's own directory.
            assert!(!args.is_empty());
            assert!(args.iter().all(|a| !a.is_empty()), "{args:?}");
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
    fn terminal_argv_uses_each_terminals_own_flag() {
        // The KDE case: konsole has no -e-with-a-command convention to rely on,
        // and --workdir is the flag that actually lands in the directory.
        assert_eq!(
            terminal_argv("konsole", "/home/me/src"),
            vec!["--workdir".to_string(), "/home/me/src".to_string()]
        );
        // A full path to the binary is the same terminal.
        assert_eq!(
            terminal_argv("/usr/bin/konsole", "/home/me/src"),
            vec!["--workdir".to_string(), "/home/me/src".to_string()]
        );
        for program in ["kgx", "gnome-terminal", "xfce4-terminal", "alacritty", "foot"] {
            assert_eq!(
                terminal_argv(program, "/src"),
                vec!["--working-directory".to_string(), "/src".to_string()],
                "{program}"
            );
        }
        assert_eq!(
            terminal_argv("kitty", "/src"),
            vec!["--directory".to_string(), "/src".to_string()]
        );
        assert_eq!(
            terminal_argv("wezterm", "/src"),
            vec![
                "start".to_string(),
                "--cwd".to_string(),
                "/src".to_string()
            ]
        );
        // Anything else gets the portable form, with the directory quoted so a
        // path holding a quote or a $ cannot turn into shell code.
        for program in ["x-terminal-emulator", "xterm", "zutty", "some-term"] {
            let args = terminal_argv(program, "/a b");
            assert_eq!(args[..3], ["-e", "sh", "-c"], "{program}: {args:?}");
            assert_eq!(args[3], "cd '/a b'; exec \"${SHELL:-sh}\"", "{program}");
        }
    }

    #[test]
    fn pick_terminal_prefers_the_environment_then_the_list() {
        // $TERMINAL wins even when another terminal comes first in the list: it
        // is the user saying which one they want.
        assert_eq!(
            pick_terminal(Some("kitty"), &["konsole", "xterm"]).as_deref(),
            Some("kitty")
        );
        // Blank and whitespace-only values are not a choice.
        assert_eq!(
            pick_terminal(Some("  "), &["konsole", "xterm"]).as_deref(),
            Some("konsole")
        );
        assert_eq!(pick_terminal(None, &["konsole"]).as_deref(), Some("konsole"));
        // Nothing installed and no $TERMINAL: the caller falls back to the
        // alternatives name, which is what a Debian system needs.
        assert_eq!(pick_terminal(None, &[]), None);
    }

    #[test]
    fn containing_folder_action_is_resolved_against_this_host() {
        #[cfg(all(not(windows), not(target_os = "macos")))]
        {
            let (prog, args) = open_containing_folder_command("/a/b.rs");
            let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
            let available: Vec<&'static str> = folder_opener_order(desktop.as_deref())
                .into_iter()
                .filter(|p| find_on_path(p).is_some())
                .collect();
            if available.is_empty() {
                // Nothing on PATH: the name is still xdg-open, which is right
                // for every freedesktop system that has it installed.
                assert_eq!(prog, "xdg-open");
            } else {
                // Otherwise it is the most preferred one present for this
                // desktop — dolphin on KDE, xdg-open when that is all there is.
                assert_eq!(prog, available[0]);
                assert!(FOLDER_OPENER_CANDIDATES.contains(&prog.as_str()));
            }
            // The arguments open that program on this very file: diamond
            // selects it, the rest get its directory.
            assert_eq!(args, folder_opener_argv(&prog, "/a/b.rs"));
            if prog.starts_with("dolphin") {
                assert_eq!(args, vec!["--select".to_string(), "/a/b.rs".to_string()]);
            } else {
                assert_eq!(args, vec!["/a".to_string()]);
            }
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            // The dispatcher platforms keep their single, native form.
            let (prog, args) = open_containing_folder_command("/a/b.rs");
            let (base_prog, base_args) = RowAction::OpenContainingFolder.command("/a/b.rs");
            assert_eq!(prog, base_prog);
            assert_eq!(args, base_args);
        }
    }

    #[test]
    fn folder_opener_order_follows_the_desktop() {
        // KDE first, and not through xdg-open's shell wrapper: on KDE that
        // wrapper keeps a kde-open process alive after the window opens.
        let kde = folder_opener_order(Some("KDE"));
        assert_eq!(kde[0], "dolphin");
        assert_eq!(kde[1], "kde-open5");
        assert!(kde.contains(&"xdg-open"), "{kde:?}");
        // The case matters not: the variable is not spelled consistently.
        assert_eq!(folder_opener_order(Some("kde")).first(), Some(&"dolphin"));
        // GNOME's file manager, ahead of the generic dispatcher.
        assert_eq!(folder_opener_order(Some("ubuntu:GNOME")).first(), Some(&"nautilus"));
        assert_eq!(folder_opener_order(Some("XFCE")).first(), Some(&"thunar"));
        // An unrecognised (or absent) desktop keeps xdg-open first: it is the
        // one name that dispatches correctly everywhere.
        assert_eq!(folder_opener_order(None).first(), Some(&"xdg-open"));
        assert_eq!(folder_opener_order(Some("sway")).first(), Some(&"xdg-open"));
        // No duplicates, so "first available" is unambiguous.
        for desktop in [None, Some("KDE"), Some("GNOME"), Some("XFCE")] {
            let order = folder_opener_order(desktop);
            let mut sorted = order.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), order.len(), "{desktop:?}: {order:?}");
        }
    }

    #[test]
    fn folder_opener_argv_selects_the_file_where_it_can() {
        // dolphin selects the row's file, which is what the action promises.
        assert_eq!(
            folder_opener_argv("dolphin", "/a/b.rs"),
            vec!["--select".to_string(), "/a/b.rs".to_string()]
        );
        assert_eq!(
            folder_opener_argv("/usr/bin/dolphin", "/a/b.rs"),
            vec!["--select".to_string(), "/a/b.rs".to_string()]
        );
        // A dispatcher takes the directory; it has no selection flag to give.
        for program in ["xdg-open", "kde-open5", "nautilus", "thunar"] {
            assert_eq!(
                folder_opener_argv(program, "/a/b.rs"),
                vec!["/a".to_string()],
                "{program}"
            );
        }
        // A bare name has no usable parent, so its own directory is the answer.
        assert_eq!(folder_opener_argv("xdg-open", "b.rs"), vec!["b.rs".to_string()]);
    }

    #[test]
    fn launch_command_routes_the_two_environment_actions() {
        // Both "Open containing folder" and "Open in terminal" must be resolved
        // per host — a regression that sent them back through `RowAction::command`
        // would launch the Debian-only alternatives name on a KDE box again.
        for action in [RowAction::OpenContainingFolder, RowAction::OpenInTerminal] {
            let (prog, args) = launch_command(action, "/a/b.rs");
            let expected = match action {
                RowAction::OpenContainingFolder => open_containing_folder_command("/a/b.rs"),
                _ => open_in_terminal_command("/a/b.rs"),
            };
            assert_eq!((prog, args), expected, "{:?}", action.label());
        }
        // The other actions are exactly `RowAction::command`.
        let (prog, args) = launch_command(RowAction::Open, "/a/b.rs");
        let (base_prog, base_args) = RowAction::Open.command("/a/b.rs");
        assert_eq!(prog, base_prog);
        assert_eq!(args, base_args);
    }

    // Only the desktop backends route these anywhere; a `zork` build has no
    // toolkit and says so, which is the arm this test exists to keep honest.
    #[cfg(any(
        all(feature = "gtk", target_os = "linux", not(feature = "zork")),
        all(feature = "macos", target_os = "macos", not(feature = "zork"))
    ))]
    #[test]
    fn the_row_menu_and_clipboard_route_into_a_real_backend() {
        // A build with no desktop toolkit answers these with "no ... without a
        // desktop backend". Every other answer -- "GTK is not initialised", "no
        // NSPasteboard on this system" -- means the call reached the real
        // implementation, which is what this pins: neither facility may fall
        // back to a stub on the platforms the app ships. Runs without a
        // display, so it guards the routing on CI's macOS and Linux jobs.
        let stubbed = |msg: &str| msg.contains("without a desktop backend");

        if let Err(e) = rswidgets::compat::clipboard_set_text("/tmp/row.txt") {
            assert!(!stubbed(&e.to_string()), "clipboard write is stubbed: {e}");
        }
        if let Err(e) = rswidgets::compat::clipboard_get_text() {
            assert!(!stubbed(&e.to_string()), "clipboard read is stubbed: {e}");
        }
        if let Err(e) = rswidgets::compat::ContextMenu::new().build() {
            assert!(!stubbed(&e.to_string()), "the row menu is stubbed: {e}");
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
        #[cfg(target_os = "macos")]
        {
            let (_, args) = RowAction::OpenContainingFolder.command("b.rs");
            assert_eq!(args, vec!["-R".to_string(), "b.rs".to_string()]);
        }
        #[cfg(all(not(windows), not(target_os = "macos")))]
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
            None,
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
            None,
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
            None,
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
            None,
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
    fn phase_suffix_names_what_the_daemon_is_doing() {
        use fflocate::health::Phase;
        // Crawling with progress: the daemon's own numbers, grouped for
        // readability.
        assert_eq!(
            phase_suffix(Some(Phase::Crawling), Some(1_234_567), Some(41)).as_deref(),
            Some("  [daemon crawling — 1 234 567 entries, 41 dirs left]")
        );
        // Crawling without a count yet: no invented numbers.
        assert_eq!(
            phase_suffix(Some(Phase::Crawling), None, None).as_deref(),
            Some("  [daemon crawling]")
        );
        assert_eq!(
            phase_suffix(Some(Phase::Replaying), None, None).as_deref(),
            Some("  [daemon replaying journal]")
        );
        assert_eq!(
            phase_suffix(Some(Phase::Rescanning), None, None).as_deref(),
            Some("  [daemon re-verifying]")
        );
        // Idle: nothing to excuse.
        assert_eq!(phase_suffix(Some(Phase::Idle), None, None), None);
        // Old daemon: no phase, no invention.
        assert_eq!(phase_suffix(None, None, None), None);
    }

    #[test]
    fn thousands_groups_digits_for_readability() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1 000");
        assert_eq!(thousands(1_234_567), "1 234 567");
        assert_eq!(thousands(1_000_000_000), "1 000 000 000");
    }

    #[test]
    fn in_flight_line_prefers_the_daemons_phase_over_catching_up() {
        use fflocate::health::Phase;
        // A crawling daemon that also flags catching_up: the specific phase
        // wins, and the bare "catching up" suffix is not appended twice.
        let s = searching_status_line(
            "main",
            std::time::Duration::from_millis(4200),
            0,
            true,
            None,
            Some(9000),
            Some(Phase::Crawling),
            Some(2_000_000),
            Some(7),
        );
        assert_eq!(
            s,
            "searching for main… 4.2s  [daemon crawling — 2 000 000 entries, 7 dirs left]"
        );
        assert!(!s.contains("catching up"), "phase must not double up: {s}");
        // No phase (old daemon): fall back to the catching-up flag.
        let s = searching_status_line(
            "main",
            std::time::Duration::from_millis(4200),
            0,
            true,
            None,
            Some(9000),
            None,
            None,
            None,
        );
        assert!(s.ends_with("[daemon catching up, 9s behind]"), "{s}");
        // Idle, caught up: nothing appended at all — this is the case that
        // used to say "starting fflocated… 44.0s".
        let s = searching_status_line(
            "main",
            std::time::Duration::from_millis(300),
            0,
            false,
            None,
            None,
            Some(Phase::Idle),
            None,
            None,
        );
        assert_eq!(s, "searching for main… 0.3s");
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
            None,
            None,
            None,
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
                phase: Some("crawling".into()),
                crawl_entries: Some(900),
                crawl_dirs_pending: Some(12),
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
        assert_eq!(o.phase, Some(fflocate::health::Phase::Crawling));
        assert_eq!(o.crawl_entries, Some(900));
        assert_eq!(o.crawl_dirs_pending, Some(12));
        // The daemon's own phase is the headline; lag rides along only when
        // there is no phase to name (see the fallback test below).
        assert!(
            o.status_line(0).contains("[daemon crawling — 900 entries, 12 dirs left]"),
            "{}",
            o.status_line(0)
        );

        // With no phase (an old daemon), the lag suffix is the fallback.
        let recs_old = vec![
            Response::Meta {
                gen: 1,
                lsn: 2,
                fresh: true,
                elapsed_ms: 3.0,
                stale_roots: vec![],
                catching_up: Some(true),
                lag_ms: Some(2500),
                phase: None,
                crawl_entries: None,
                crawl_dirs_pending: None,
                backend: None,
                entries: None,
            },
            Response::Done {
                count: 0,
                cursor: "c:1:2".into(),
            },
        ];
        let o = DaemonClient::new()
            .outcome_from_records(&recs_old)
            .unwrap();
        assert_eq!(o.phase, None);
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
            None,
            None,
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
            None,
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
    #[test]
    fn the_debounce_waits_for_its_window_and_rearms_on_a_new_keystroke() {
        // A long window: "not due yet" must not depend on how fast the runner is.
        let slow = Debounce::new(std::time::Duration::from_secs(30));
        assert!(!slow.pending());
        assert!(!slow.due());
        slow.arm();
        assert!(slow.pending());
        assert!(!slow.due(), "a request was due the instant a key was pressed");
        // A second keystroke re-arms the same way.
        slow.arm();
        assert!(slow.pending());
        assert!(!slow.due());
        // Issuing the request clears it: a quiet pump must not re-issue.
        slow.clear();
        assert!(!slow.pending());
        assert!(!slow.due());

        // A window that has already passed is due with no sleep at all.
        let now = Debounce::new(std::time::Duration::ZERO);
        now.arm();
        assert!(now.pending());
        assert!(now.due(), "a zero-length window was not due");
        now.clear();
        assert!(!now.due(), "a cleared debounce is still due");

        // A short window does elapse; the test only demands that it has by 60ms.
        let short = Debounce::new(std::time::Duration::from_millis(5));
        short.arm();
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(short.due(), "a 5ms window had not elapsed after 60ms");
    }

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
                phase: None,
                crawl_entries: None,
                crawl_dirs_pending: None,
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
                created: None,
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

/// The keystroke debounce: every edit pushes the deadline out, and the pump
/// asks once per tick whether it has elapsed.
///
/// Both the GUI's own pump and the interaction tests (the macOS probe and
/// `tests/interaction_gtk.rs`) drive this type, so "typing waits for the
/// window before a request goes out" is asserted on the code the app runs
/// rather than on a copy of it.
#[derive(Debug)]
pub struct Debounce {
    window: std::time::Duration,
    /// When the pending edit may be turned into a request; `None` when no
    /// keystroke is waiting.
    due_at: std::cell::RefCell<Option<std::time::Instant>>,
}

impl Debounce {
    pub fn new(window: std::time::Duration) -> Debounce {
        Debounce {
            window,
            due_at: std::cell::RefCell::new(None),
        }
    }

    /// A keystroke: (re)arm the deadline to `window` from now.
    pub fn arm(&self) {
        *self.due_at.borrow_mut() = Some(std::time::Instant::now() + self.window);
    }

    /// Drop the deadline because its request has been issued.
    pub fn clear(&self) {
        *self.due_at.borrow_mut() = None;
    }

    /// Whether an edit is waiting for its window to elapse.
    pub fn pending(&self) -> bool {
        self.due_at.borrow().is_some()
    }

    /// The deadline itself, for a pump that wants to skip waking up early.
    pub fn deadline(&self) -> Option<std::time::Instant> {
        *self.due_at.borrow()
    }

    /// Whether the window has elapsed. A pump that keeps ticking asks this;
    /// nothing is cleared, so the caller decides when the request is issued.
    pub fn due(&self) -> bool {
        match *self.due_at.borrow() {
            Some(t) => std::time::Instant::now() >= t,
            None => false,
        }
    }
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
            phase: None,
            crawl_entries: None,
            crawl_dirs_pending: None,
            backend: None,
            entries: None,
        }
    }

    fn hit(path: &str, id: u32) -> Response {
        Response::Hit {
            path: path.into(),
            id,
            created: None,
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

#[cfg(test)]
mod full_set_tests {
    //! The whole-set load a header click starts: the query it sends, the hits it
    //! folds in, and the lines the status bar shows.

    use super::*;

    fn hit(id: u32, path: &str) -> Response {
        Response::Hit {
            path: path.to_string(),
            id,
            created: None,
        }
    }

    fn done(count: u64) -> Response {
        Response::Done {
            count,
            cursor: "c:1:1".to_string(),
        }
    }

    #[test]
    fn the_request_is_the_same_query_without_a_limit() {
        // `limit == 0` is the daemon's "everything", the same as `ff -l 0`:
        // that is what makes one request cover the whole set.
        let opts = SearchOptions {
            case_sensitive: true,
            ..Default::default()
        };
        let set = FullSet::new("main", opts, 629_665, MAX_SORTED_ROWS);
        match set.request() {
            Request::Query {
                q,
                limit,
                count,
                case_sensitive,
                progress,
                fuzzy,
                ..
            } => {
                assert_eq!(q, "main");
                assert_eq!(limit, 0, "limit 0 asks for every match");
                assert!(!count, "a sort load wants the paths, not just a count");
                assert_eq!(case_sensitive, Some(true), "the search's options carry over");
                assert!(!fuzzy, "the plain box is not a fuzzy search");
                assert!(progress, "the load still reports scan progress");
            }
            _ => panic!("a whole-set load must send a query"),
        }
    }

    #[test]
    fn the_status_line_reports_progress_then_the_finished_count() {
        let mut set = FullSet::new("rs", SearchOptions::default(), 629_665, MAX_SORTED_ROWS);
        assert_eq!(set.status_line(), "loading all matches: 0 of 629 665");
        let fresh = set.absorb(&[hit(1, "/a.rs"), hit(2, "/b.rs")]);
        assert_eq!(fresh, ["/a.rs", "/b.rs"], "the caller gets what to show");
        set.absorb(&[hit(3, "/c.rs")]);
        assert_eq!(set.status_line(), "loading all matches: 3 of 629 665");
        // `Done` carries the authoritative count.
        set.absorb(&[done(629_665)]);
        assert!(set.done());
        assert_eq!(set.status_line(), "sorted 629 665 matches");
    }

    #[test]
    fn a_repeated_path_is_absorbed_once() {
        // A path twice in the list would be a bug the user can see.
        let mut set = FullSet::new("rs", SearchOptions::default(), 2, MAX_SORTED_ROWS);
        let fresh = set.absorb(&[hit(1, "/a"), hit(1, "/a"), hit(2, "/b"), hit(2, "/b")]);
        assert_eq!(fresh, ["/a", "/b"]);
        assert_eq!(set.loaded(), 2);
    }

    #[test]
    fn the_ceiling_stops_counting_but_still_reaches_done() {
        // Past the cap the paths are dropped, but the reply is read to its
        // terminal record so the total stays honest and the socket closes.
        let mut set = FullSet::new("rs", SearchOptions::default(), 5, 2);
        let fresh = set.absorb(&[hit(1, "/a"), hit(2, "/b"), hit(3, "/c")]);
        assert_eq!(fresh, ["/a", "/b"]);
        assert_eq!(set.loaded(), 2);
        assert!(set.capped());
        assert_eq!(set.status_line(), "loading all matches: 2 of 5");
        set.absorb(&[done(5)]);
        assert!(set.done());
        assert_eq!(set.status_line(), "stopped at 2 rows");
    }

    #[test]
    fn a_failure_is_reported_rather_than_looking_empty() {
        let mut set = FullSet::new("rs", SearchOptions::default(), 9, MAX_SORTED_ROWS);
        set.fail("connection reset");
        assert!(set.failed());
        assert!(set.done(), "a failed load is over");
        assert_eq!(set.status_line(), "sort load failed: connection reset");
    }
}

#[cfg(test)]
mod created_range_tests {
    use super::*;

    #[test]
    fn a_full_day_is_inclusive_at_both_ends() {
        let r = parse_created_range("2026-01-02", "2026-01-03").unwrap().unwrap();
        let start = days_from_civil(2026, 1, 2) * 86_400;
        assert!(r.contains(start), "the from day's first second");
        assert!(r.contains(start + 172_799), "the to day's last second");
        assert!(!r.contains(start + 172_800), "the next day");
        assert!(!r.contains(start - 1), "the day before");
        assert_eq!(r.label(), "2026-01-02–2026-01-03");
    }

    #[test]
    fn either_end_may_be_open() {
        let from_only = parse_created_range("2026-01-02", " ").unwrap().unwrap();
        assert!(from_only.contains(days_from_civil(2030, 5, 5) * 86_400));
        assert_eq!(from_only.label(), "since 2026-01-02");
        let to_only = parse_created_range("", "2026-01-02").unwrap().unwrap();
        assert!(to_only.contains(0));
        assert_eq!(to_only.label(), "until 2026-01-02");
        assert_eq!(parse_created_range("  ", "\t").unwrap(), None);
    }

    #[test]
    fn bad_and_inverted_dates_name_the_field() {
        let e = parse_created_range("yesterday", "").unwrap_err();
        assert!(e.starts_with("created from:"), "{e}");
        let e = parse_created_range("", "2026-02-30").unwrap_err();
        assert!(e.starts_with("created to:"), "{e}");
        let e = parse_created_range("2026-2-1", "").unwrap_err();
        assert!(e.contains("YYYY-MM-DD"), "{e}");
        let e = parse_created_range("2026-03-01", "2026-02-01").unwrap_err();
        assert_eq!(e, "created range: from 2026-03-01 is after to 2026-02-01");
    }

    #[test]
    fn leap_days_are_dates_and_the_rest_of_february_is_not() {
        assert!(parse_created_range("2024-02-29", "").is_ok());
        assert!(parse_created_range("2026-02-29", "").is_err());
        assert!(parse_created_range("2026-13-01", "").is_err());
    }

    #[test]
    fn filtering_keeps_in_range_paths_in_order() {
        let range = parse_created_range("2026-01-02", "2026-01-03").unwrap().unwrap();
        let jan2 = days_from_civil(2026, 1, 2) * 86_400;
        let mut paths = vec![
            "/old".to_string(),
            "/in".to_string(),
            "/undated".to_string(),
            "/new".to_string(),
        ];
        let stats = filter_by_created(&mut paths, &range, |p| match p {
            "/old" => Some(jan2 - 1),
            "/in" => Some(jan2 + 60),
            "/new" => Some(jan2 + 200_000),
            _ => None,
        });
        assert_eq!(paths, ["/in"]);
        assert_eq!(
            stats,
            CreatedStats {
                examined: 4,
                kept: 1,
                undated: 1
            }
        );
        assert_eq!(stats.outside(), 2);
    }

    #[test]
    fn created_range_prefers_the_daemons_minute_over_a_stat() {
        // These paths do not exist, so a `stat` would report "undated". Keeping
        // the dated one proves the answer came from the hit's `created`.
        let mut o = SearchOutcome {
            paths: vec![
                "/nonexistent/ff-gui/dated".into(),
                "/nonexistent/ff-gui/other".into(),
            ],
            total: 2,
            fresh: true,
            ..Default::default()
        };
        let day = 29_491_680u32; // 2026-01-02, in minutes since the epoch
        o.created.insert("/nonexistent/ff-gui/dated".into(), day);
        let range = CreatedRange {
            from: Some(day as i64 * 60),
            to: Some(day as i64 * 60 + 86_399),
        };
        let note = apply_created_range(&mut o, Some(range), false);
        assert_eq!(o.paths, vec!["/nonexistent/ff-gui/dated"]);
        assert!(note.contains("1 of 2 matched"), "{note}");
        assert!(note.contains("1 had no creation time"), "{note}");
        // The map survives the filter, so a later pass can reuse it.
        assert_eq!(o.created.get("/nonexistent/ff-gui/dated"), Some(&day));
    }

    #[test]
    fn the_note_says_what_it_checked() {
        let range = parse_created_range("2026-01-02", "2026-01-03").unwrap().unwrap();
        let all = CreatedStats {
            examined: 3,
            kept: 3,
            undated: 0,
        };
        assert_eq!(
            created_note(&range, &all, 3, 3, false),
            "  [created 2026-01-02–2026-01-03]"
        );
        let some = CreatedStats {
            examined: 50,
            kept: 12,
            undated: 2,
        };
        assert_eq!(
            created_note(&range, &some, 50, 50, false),
            "  [created 2026-01-02–2026-01-03: 12 of 50 matched; 2 had no creation time]"
        );
        assert_eq!(
            created_note(&range, &some, 50, 900, false),
            "  [created 2026-01-02–2026-01-03: 12 of 50 matched; 2 had no creation time; 850 more matches not checked]"
        );
        assert_eq!(
            created_note(&range, &all, 0, 900, true),
            "  [created range not applied to a count-only search]"
        );
    }

    #[test]
    fn a_real_file_is_dated_or_the_filesystem_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probe.txt");
        std::fs::write(&path, b"x").unwrap();
        match created_secs(path.to_str().unwrap()) {
            Some(secs) => assert!(secs > 0),
            // A filesystem without birth times is not an error: the status
            // line counts those rows as undated.
            None => eprintln!("no creation time on this filesystem; range cannot match files here"),
        }
        assert_eq!(created_secs("/nonexistent/ff-gui/created"), None);
    }
}
