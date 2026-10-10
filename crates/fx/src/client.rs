//! The `ff` client: connect to the daemon (auto-spawning it), speak the NDJSON
//! protocol, and format results.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use findex::config_paths::{socket_path_from_env, state_dir_from_env};
use findex::proto::{Request, Response};

#[derive(Debug)]
pub enum CliError {
    Usage(String),
    Io(String),
    Protocol(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Usage(m) => write!(f, "{m}"),
            CliError::Io(m) => write!(f, "{m}"),
            CliError::Protocol(m) => write!(f, "{m}"),
        }
    }
}

pub struct Client {
    stream: std::os::unix::net::UnixStream,
    #[allow(dead_code)]
    socket: PathBuf,
}

impl Client {
    /// Connect to the daemon, auto-spawning it if it is not running.
    pub fn connect(socket_override: Option<&str>) -> Result<Client, CliError> {
        let socket = socket_override
            .map(PathBuf::from)
            .unwrap_or_else(socket_path_from_env);

        // Try once, then spawn and retry.
        match std::os::unix::net::UnixStream::connect(&socket) {
            Ok(stream) => Ok(Client { stream, socket }),
            Err(_) => {
                spawn_daemon(&socket)?;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    match std::os::unix::net::UnixStream::connect(&socket) {
                        Ok(stream) => return Ok(Client { stream, socket }),
                        Err(_) if std::time::Instant::now() < deadline => {
                            std::thread::sleep(std::time::Duration::from_millis(25));
                        }
                        Err(e) => {
                            return Err(CliError::Io(format!(
                                "cannot connect to findexd at {}: {e}",
                                socket.display()
                            )));
                        }
                    }
                }
            }
        }
    }

    fn send(&mut self, req: &Request) -> Result<Vec<Response>, CliError> {
        let line = serde_json::to_string(req).map_err(|e| CliError::Protocol(e.to_string()))?;
        writeln!(self.stream, "{line}").map_err(|e| CliError::Io(e.to_string()))?;
        self.stream.flush().map_err(|e| CliError::Io(e.to_string()))?;
        let mut out = Vec::new();
        let reader = BufReader::new(self.stream.try_clone().map_err(|e| CliError::Io(e.to_string()))?);
        for line in reader.lines() {
            let line = line.map_err(|e| CliError::Io(e.to_string()))?;
            if line.trim().is_empty() {
                continue;
            }
            let r: Response =
                serde_json::from_str(&line).map_err(|e| CliError::Protocol(format!("{e}: {line}")))?;
            let terminal = matches!(
                r,
                Response::Done { .. }
                    | Response::Error { .. }
                    | Response::Pong { .. }
                    | Response::Synced { .. }
                    | Response::Stats { .. }
                    | Response::Rescanning { .. }
            );
            out.push(r);
            if terminal {
                break;
            }
        }
        Ok(out)
    }

    /// Run a query and print results. Returns the process exit code.
    ///
    /// When `expr` is `Some`, it is a Watchman-style expression tree or a full
    /// query object; the JSON is sent as the query and the string `q` is
    /// ignored (§10, expr).
    pub fn query(
        &mut self,
        q: &str,
        limit: usize,
        json: bool,
        count: bool,
        zero: bool,
        expr: Option<serde_json::Value>,
        fuzzy: bool,
        distance: Option<u32>,
    ) -> Result<ExitCode, CliError> {
        let is_string_query = expr.is_none();
        let req = match expr {
            Some(v) => {
                // Accept either a bare expression tree (array/string) or a full
                // query object; the protocol takes the object's keys.
                let obj = match v {
                    serde_json::Value::Object(o) => serde_json::Value::Object(o),
                    other => {
                        let mut m = serde_json::Map::new();
                        m.insert("expression".into(), other);
                        serde_json::Value::Object(m)
                    }
                };
                let get = |k: &str| obj.get(k).cloned();
                Request::Query {
                    q: String::new(),
                    limit,
                    json,
                    count,
                    expression: get("expression"),
                    glob: get("glob"),
                    glob_includedotfiles: get("glob_includedotfiles").and_then(|v| v.as_bool()),
                    glob_noescape: get("glob_noescape").and_then(|v| v.as_bool()),
                    suffix: get("suffix"),
                    path: get("path"),
                    relative_root: get("relative_root").and_then(|v| v.as_str().map(|s| s.to_string())),
                    case_sensitive: get("case_sensitive").and_then(|v| v.as_bool()),
                    // Fuzzy applies to the string query language, not to
                    // expression trees (§10).
                    fuzzy: false,
                    max_distance: None,
                }
            }
            None => {
                let mut r = Request::query(q, limit, json, count);
                if let Request::Query { fuzzy: f, max_distance: d, .. } = &mut r {
                    *f = fuzzy;
                    *d = distance;
                }
                r
            }
        };
        let resp = self.send(&req)?;
        let mut printed = 0u64;
        let mut total = 0u64;
        let mut stdout = std::io::stdout().lock();
        for r in &resp {
            match r {
                Response::Meta { stale_roots, catching_up, .. } => {
                    if !stale_roots.is_empty() {
                        eprintln!("ff: warning: stale roots: {}", stale_roots.join(", "));
                    }
                    if catching_up == &Some(true) {
                        eprintln!("ff: warning: daemon is still catching up; results may be incomplete");
                    }
                }
                Response::Hit { path, id } => {
                    printed += 1;
                    if json {
                        let obj = serde_json::json!({"path": path, "id": id});
                        let _ = writeln!(stdout, "{obj}");
                    } else if zero {
                        let _ = stdout.write_all(path.as_bytes());
                        let _ = stdout.write_all(b"\0");
                    } else {
                        let _ = writeln!(stdout, "{path}");
                    }
                }
                Response::Done { count: c, .. } => total = *c,
                Response::Error { message } => {
                    return Err(CliError::Protocol(message.clone()));
                }
                _ => {}
            }
        }
        let _ = stdout.flush();
        if count {
            // In count mode the response carries only `done`; `printed` may be 0.
            println!("{}", total.max(printed));
        }
        if total == 0 {
            // Point of no return for results — but stderr may still help.
            if is_string_query {
                self.suggest_alternate(q);
            }
            return Ok(ExitCode::from(1));
        }
        Ok(ExitCode::SUCCESS)
    }

    /// Case-folded "did you mean" for zero-hit string queries. The engine is
    /// smart-case: an uppercase letter in the query forced sensitive matching,
    /// so a lowercased rerun may find what the user meant. Bounded to a few
    /// hits, stderr only — stdout, the exit code, and determinism are
    /// untouched. (True edit-distance fuzzy belongs to the v2 trigram
    /// accelerator, §17, not to a scan.)
    fn suggest_alternate(&mut self, q: &str) {
        let Some(folded) = folded_query(q) else { return };
        let req = Request::query(&folded, 5, false, false);
        let Ok(resp) = self.send(&req) else { return };
        let mut hits = Vec::new();
        for r in &resp {
            if let Response::Hit { path, .. } = r {
                hits.push(path.clone());
            }
        }
        if hits.is_empty() {
            return;
        }
        eprintln!("ff: no matches; did you mean:");
        for h in &hits {
            eprintln!("  {h}");
        }
    }

    /// Run a prebuilt expression query in `locate` mode: plain absolute paths
    /// on stdout, `locate` exit statuses (0 = matches, 1 = none).
    ///
    /// With `existing`, hits are filtered by the filesystem at print time and
    /// the limit is applied client-side (the server cannot know which hits
    /// survive the existence check).
    pub fn locate(
        &mut self,
        o: &crate::locate::LocateArgs,
        expr: serde_json::Value,
    ) -> Result<ExitCode, CliError> {
        let req = Request::Query {
            q: String::new(),
            // Over-fetch when filtering: the limit applies to printed hits.
            limit: if o.existing { 0 } else { o.limit },
            json: false,
            // Server-side count skips hits; with -e we must count survivors.
            count: o.count && !o.existing,
            expression: Some(expr),
            glob: None,
            glob_includedotfiles: None,
            glob_noescape: None,
            suffix: None,
            path: None,
            relative_root: None,
            case_sensitive: None,
            fuzzy: false,
            max_distance: None,
        };
        let resp = self.send(&req)?;
        let mut paths: Vec<String> = Vec::new();
        let mut total = 0u64;
        for r in &resp {
            match r {
                Response::Meta { stale_roots, catching_up, .. } => {
                    if o.quiet {
                        continue;
                    }
                    if !stale_roots.is_empty() {
                        eprintln!("locate: warning: stale roots: {}", stale_roots.join(", "));
                    }
                    if catching_up == &Some(true) {
                        eprintln!("locate: warning: daemon is still catching up; results may be incomplete");
                    }
                }
                Response::Hit { path, .. } => {
                    if o.existing && !path_exists(path, o.follow) {
                        continue;
                    }
                    if o.limit != 0 && paths.len() >= o.limit {
                        continue;
                    }
                    paths.push(path.clone());
                }
                Response::Done { count: c, .. } => total = *c,
                Response::Error { message } => {
                    return Err(CliError::Protocol(message.clone()));
                }
                _ => {}
            }
        }
        if o.count {
            // Server-side count (no -e) arrives in `done`; with -e we count
            // the survivors.
            println!("{}", total.max(paths.len() as u64));
        } else {
            let mut stdout = std::io::stdout().lock();
            for p in &paths {
                if o.zero {
                    let _ = stdout.write_all(p.as_bytes());
                    let _ = stdout.write_all(b"\0");
                } else {
                    let _ = writeln!(stdout, "{p}");
                }
            }
            let _ = stdout.flush();
        }
        // Server-side count arrives in `done`; otherwise the fetched (and
        // possibly existence-filtered) hits tell the story.
        let matched = if o.count && !o.existing { total } else { paths.len() as u64 };
        if matched == 0 {
            return Ok(ExitCode::from(1));
        }
        Ok(ExitCode::SUCCESS)
    }

    /// Run a `find`-mode plan: the index answers the name/type/path/depth
    /// predicates, then the surviving candidates are `lstat`ed for the metadata
    /// tests (§10, two-phase plan).
    ///
    /// `find` exits 0 even when nothing matches, so this never returns the
    /// "no matches" status.
    pub fn find(
        &mut self,
        query: &serde_json::Value,
        plan: &crate::find::Plan,
    ) -> Result<ExitCode, CliError> {
        // `-newer` needs the index to be settled, or the reference file's own
        // creation may not be visible yet.
        if plan.sync {
            let _ = self.sync_timeout(60_000)?;
        }

        let obj = query.as_object().cloned().unwrap_or_default();
        let get = |k: &str| obj.get(k).cloned();
        let req = Request::Query {
            q: String::new(),
            // Phase 2 filters, so the limit cannot apply server-side.
            limit: 0,
            json: false,
            count: false,
            expression: get("expression"),
            glob: get("glob"),
            glob_includedotfiles: get("glob_includedotfiles").and_then(|v| v.as_bool()),
            glob_noescape: get("glob_noescape").and_then(|v| v.as_bool()),
            suffix: get("suffix"),
            path: get("path"),
            relative_root: get("relative_root").and_then(|v| v.as_str().map(|s| s.to_string())),
            case_sensitive: get("case_sensitive").and_then(|v| v.as_bool()),
            // find mode is exact; fuzzy belongs to the ff string query.
            fuzzy: false,
            max_distance: None,
        };
        let resp = self.send(&req)?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Collect phase-1 survivors (the server has already applied the
        // index-level predicates), then apply path/depth filters and phase 2.
        let mut survivors: Vec<String> = Vec::new();
        for r in &resp {
            match r {
                Response::Meta { stale_roots, catching_up, .. } => {
                    if !stale_roots.is_empty() {
                        eprintln!("find: warning: stale roots: {}", stale_roots.join(", "));
                    }
                    if catching_up == &Some(true) {
                        eprintln!("find: warning: daemon is still catching up");
                    }
                }
                Response::Hit { path, .. } => survivors.push(path.clone()),
                Response::Error { message } => {
                    return Err(CliError::Protocol(message.clone()));
                }
                _ => {}
            }
        }

        let mut shown = 0u64;
        let mut stdout = std::io::stdout().lock();
        for path in &survivors {
            if !plan.paths.is_empty() {
                let under = plan.paths.iter().any(|base| path_under(path, base));
                if !under {
                    continue;
                }
            }
            let depth = depth_of(path, &plan.paths, &survivors);
            if let Some(d) = plan.max_depth {
                if depth > d {
                    continue;
                }
            }
            if let Some(d) = plan.min_depth {
                if depth < d {
                    continue;
                }
            }
            if !plan.meta.is_empty() {
                let p = std::path::Path::new(path);
                let Some(m) = crate::find::lstat(p, now) else { continue };
                let is_empty_dir = std::fs::read_dir(p).map(|mut d| d.next().is_none()).unwrap_or(false);
                if !crate::find::meta_matches(&plan.meta, &m, is_empty_dir, now) {
                    continue;
                }
            }
            shown += 1;
            if let Some(fmt) = &plan.printf {
                // `-printf` supplies its own newlines; do not add one.
                let _ = stdout.write_all(expand_printf(fmt, path).as_bytes());
            } else if plan.print0 {
                let _ = stdout.write_all(path.as_bytes());
                let _ = stdout.write_all(b"\0");
            } else {
                let _ = writeln!(stdout, "{path}");
            }
        }
        let _ = stdout.flush();
        let _ = shown;
        // `find` exits 0 even with no matches.
        Ok(ExitCode::SUCCESS)
    }

    /// Settle the index (§8), with an explicit timeout. `ff update` needs
    /// longer than the interactive `--sync` default: a full-tree rescan is
    /// behind the barrier.
    pub fn sync_timeout(
        &mut self,
        timeout_ms: u64,
    ) -> Result<std::collections::BTreeMap<String, String>, CliError> {
        let resp = self.send(&Request::Sync { timeout_ms, roots: None })?;
        for r in resp {
            if let Response::Synced { roots } = r {
                return Ok(roots);
            }
        }
        Ok(Default::default())
    }

    /// Root keys (absolute paths) currently configured on the daemon.
    pub fn root_paths(&mut self) -> Result<Vec<String>, CliError> {
        let resp = self.send(&Request::Stats { roots: true })?;
        for r in resp {
            if let Response::Stats { stats } = r {
                let mut out = Vec::new();
                if let Some(roots) = stats.get("roots").and_then(|v| v.as_array()) {
                    for x in roots {
                        if let Some(p) = x.get("path").and_then(|v| v.as_str()) {
                            out.push(p.to_string());
                        }
                    }
                }
                return Ok(out);
            }
        }
        Ok(Vec::new())
    }

    /// Ask for a rescan of one root. Fire-and-forget: pair with a sync, which
    /// cannot complete until the rescan drains (pending ordering, §7–§8).
    pub fn rescan_root(&mut self, root: &str) -> Result<(), CliError> {
        let resp = self.send(&Request::Rescan { root: root.to_string() })?;
        for r in resp {
            if let Response::Rescanning { .. } = r {
                return Ok(());
            }
            if let Response::Error { message } = r {
                return Err(CliError::Protocol(message));
            }
        }
        Ok(())
    }

    /// Full manual update (the updatedb equivalent): rescan every root — or
    /// just `only` when non-empty — then settle. A rescan enqueued before the
    /// sync cookie is necessarily applied before the cookie completes, so a
    /// root reporting `ok` is reconciled with the filesystem: this heals
    /// missed changes from daemon downtime or offline edits. Returns the
    /// per-root status map (`ok` / `weak` / `timeout`).
    pub fn update(
        &mut self,
        only: &[String],
        timeout_ms: u64,
    ) -> Result<std::collections::BTreeMap<String, String>, CliError> {
        let targets: Vec<String> =
            if only.is_empty() { self.root_paths()? } else { only.to_vec() };
        for r in &targets {
            self.rescan_root(r)?;
        }
        self.sync_timeout(timeout_ms)
    }

    pub fn since(&mut self, cursor: &str, q: &str) -> Result<ExitCode, CliError> {
        let resp = self.send(&Request::Since { cursor: cursor.to_string(), q: q.to_string() })?;
        let mut n = 0usize;
        for r in &resp {
            match r {
                Response::Meta { fresh, gen, lsn, .. } => {
                    if *fresh {
                        // Generation mismatch → a full listing stands in.
                        println!("# fresh: true (cursor stale) gen={gen} lsn={lsn}");
                    }
                }
                Response::Event { cursor, action, path } => {
                    n += 1;
                    if path.is_empty() {
                        println!("{cursor} {action}");
                    } else {
                        println!("{cursor} {action} {path}");
                    }
                }
                Response::Error { message } => return Err(CliError::Protocol(message.clone())),
                _ => {}
            }
        }
        if n == 0 {
            return Ok(ExitCode::from(1));
        }
        Ok(ExitCode::SUCCESS)
    }

    pub fn stats(&mut self, roots: bool, json: bool) -> Result<ExitCode, CliError> {
        let resp = self.send(&Request::Stats { roots })?;
        for r in resp {
            if let Response::Stats { stats } = r {
                if json {
                    println!("{}", serde_json::to_string_pretty(&stats).unwrap());
                } else {
                    print_stats(&stats);
                }
            }
        }
        Ok(ExitCode::SUCCESS)
    }

    pub fn rescan(&mut self, root: &str) -> Result<ExitCode, CliError> {
        let resp = self.send(&Request::Rescan { root: root.to_string() })?;
        for r in resp {
            match r {
                Response::Rescanning { root } => println!("rescanning {root}"),
                Response::Error { message } => return Err(CliError::Protocol(message)),
                _ => {}
            }
        }
        Ok(ExitCode::SUCCESS)
    }

    pub fn watch(&mut self, q: &str) -> Result<ExitCode, CliError> {
        let resp = self.send(&Request::Subscribe { name: Some("ff-watch".into()), cursor: None })?;
        for r in resp {
            match r {
                Response::Meta { gen, lsn, .. } => eprintln!("# watching from c:{gen}:{lsn} q={q:?}"),
                Response::Event { path, action, .. } => println!("{action} {path}"),
                Response::Done { .. } => {}
                Response::Error { message } => return Err(CliError::Protocol(message)),
                _ => {}
            }
        }
        Ok(ExitCode::SUCCESS)
    }

}

/// Lowercased re-query for "did you mean", or `None` when folding changes
/// nothing: an all-lowercase query never engaged smart-case, so a rerun
/// would be pointless. Mirrors the engine's ASCII rule (`has_upper`).
fn folded_query(q: &str) -> Option<String> {
    if q.bytes().any(|b| b.is_ascii_uppercase()) {
        Some(q.to_ascii_lowercase())
    } else {
        None
    }
}

/// Existence check for `locate -e`: follow symlinks only with `-L`
/// (default `-P`, like GNU locate).
fn path_exists(path: &str, follow: bool) -> bool {
    let p = std::path::Path::new(path);
    if follow {
        p.exists()
    } else {
        std::fs::symlink_metadata(p).is_ok()
    }
}

/// Is `path` the same as, or under, `base`?
fn path_under(path: &str, base: &str) -> bool {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        return true;
    }
    path == base || path.starts_with(&format!("{base}/"))
}

/// Depth of a result relative to the given start paths. With no start paths the
/// depth is measured from the root the path belongs to (i.e. path components
/// after the matched base); this is the closest analogue of `find`'s depth.
fn depth_of(path: &str, bases: &[String], _all: &[String]) -> u32 {
    let base = if bases.is_empty() {
        // Nothing to strip against; use the number of components as a stand-in.
        return path.split('/').filter(|c| !c.is_empty()).count() as u32;
    } else {
        bases
            .iter()
            .map(|b| b.trim_end_matches('/'))
            .filter(|b| path_under(path, b))
            .max_by_key(|b| b.len())
            .unwrap_or("")
    };
    let rest = path.strip_prefix(base).unwrap_or(path).trim_start_matches('/');
    rest.split('/').filter(|c| !c.is_empty()).count() as u32
}

/// Expand the small, high-value subset of `-printf` directives: `%p` (path),
/// `%f` (basename), `%h` (dirname), `%s` (size), `%P` (path minus the start
/// point), `%%` (a literal percent) and `\n`/`\t`/`\0` escapes.
fn expand_printf(fmt: &str, path: &str) -> String {
    let mut outout = String::with_capacity(fmt.len() + path.len());
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => outout.push('\n'),
                Some('t') => outout.push('\t'),
                Some('0') => outout.push('\0'),
                Some('\\') => outout.push('\\'),
                Some(other) => {
                    outout.push('\\');
                    outout.push(other);
                }
                None => outout.push('\\'),
            },
            '%' => match chars.next() {
                Some('p') => outout.push_str(path),
                Some('f') => outout.push_str(
                    std::path::Path::new(path)
                        .file_name()
                        .map(|s| s.to_string_lossy())
                        .as_deref()
                        .unwrap_or(""),
                ),
                Some('h') => outout.push_str(
                    std::path::Path::new(path)
                        .parent()
                        .map(|s| s.to_string_lossy())
                        .as_deref()
                        .unwrap_or(""),
                ),
                Some('P') => outout.push_str(path.trim_start_matches('/')),
                Some('s') => {
                    let sz = std::fs::symlink_metadata(path).map(|m| m.len()).unwrap_or(0);
                    outout.push_str(&sz.to_string());
                }
                Some('%') => outout.push('%'),
                Some(other) => {
                    outout.push('%');
                    outout.push(other);
                }
                None => outout.push('%'),
            },
            other => outout.push(other),
        }
    }
    outout
}

fn print_stats(stats: &serde_json::Value) {
    macro_rules! p {
        ($k:literal) => {
            if let Some(v) = stats.get($k) {
                println!("{:<20} {}", $k, v);
            }
        };
    }
    p!("gen");
    p!("lsn");
    p!("entries");
    p!("tombstones");
    p!("backend");
    p!("recrawl_count");
    p!("last_rescan_cause");
    p!("rescan_count");
    p!("rescans_deferred");
    p!("generation_bumps");
    p!("journal_corrupt");
    p!("catching_up");
    p!("queue_depth");
    p!("dropped_events");
    p!("escalated_events");
    if let Some(roots) = stats.get("roots").and_then(|r| r.as_array()) {
        println!("\nroots:");
        for r in roots {
            let path = r.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let state = r.get("state").and_then(|v| v.as_str()).unwrap_or("?");
            let recrawl = r.get("recrawl_count").and_then(|v| v.as_u64()).unwrap_or(0);
            let cause = r.get("last_rescan_cause").and_then(|v| v.as_str()).unwrap_or("-");
            let fstype = r
                .get("caps")
                .and_then(|c| c.get("fstype"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let writable = r
                .get("caps")
                .and_then(|c| c.get("writable"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            println!(
                "  {path}\n    state={state} fstype={fstype} writable={writable} recrawl_count={recrawl} last_rescan_cause={cause}"
            );
        }
    }
}

/// Spawn `findexd` detached, inheriting our socket/state overrides.
fn spawn_daemon(socket: &Path) -> Result<(), CliError> {
    let exe = daemon_binary_path()?;
    let state = state_dir_from_env();
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--socket").arg(socket);
    cmd.arg("--state-dir").arg(&state);
    // Detach: redirect stdio so the child does not hold the terminal.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    // Index $HOME by default (the daemon's own default).
    let child = cmd
        .spawn()
        .map_err(|e| CliError::Io(format!("cannot spawn {}: {e}", exe.display())))?;
    let _ = child.id();
    Ok(())
}

/// Locate the `findexd` binary: next to `ff`, else on `$PATH`.
fn daemon_binary_path() -> Result<PathBuf, CliError> {
    if let Ok(p) = std::env::var("FINDEXD_BIN") {
        return Ok(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("findexd");
            if cand.exists() {
                return Ok(cand);
            }
        }
    }
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let cand = PathBuf::from(dir).join("findexd");
        if cand.exists() {
            return Ok(cand);
        }
    }
    Err(CliError::Io(
        "findexd not found next to ff or on PATH (set FINDEXD_BIN)".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folded_query_only_fires_on_uppercase() {
        assert_eq!(folded_query("README"), Some("readme".to_string()));
        assert_eq!(folded_query("MakeFile"), Some("makefile".to_string()));
        // Already lowercase: smart-case never engaged, rerun pointless.
        assert_eq!(folded_query("readme"), None);
        assert_eq!(folded_query(""), None);
        assert_eq!(folded_query("123-_.rs"), None);
    }
}
