//! The `fx` client: connect to the daemon (auto-spawning it), speak the NDJSON
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
    pub fn query(
        &mut self,
        q: &str,
        limit: usize,
        json: bool,
        count: bool,
        zero: bool,
    ) -> Result<ExitCode, CliError> {
        let resp = self.send(&Request::Query {
            q: q.to_string(),
            limit,
            json,
            count,
        })?;
        let mut printed = 0u64;
        let mut total = 0u64;
        let mut stdout = std::io::stdout().lock();
        for r in &resp {
            match r {
                Response::Meta { stale_roots, catching_up, .. } => {
                    if !stale_roots.is_empty() {
                        eprintln!("fx: warning: stale roots: {}", stale_roots.join(", "));
                    }
                    if catching_up == &Some(true) {
                        eprintln!("fx: warning: daemon is still catching up; results may be incomplete");
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
            return Ok(ExitCode::from(1));
        }
        Ok(ExitCode::SUCCESS)
    }

    /// Settle the index (§8). Returns per-root status.
    pub fn sync(
        &mut self,
        _socket: Option<String>,
    ) -> Result<std::collections::BTreeMap<String, String>, CliError> {
        let resp = self.send(&Request::Sync { timeout_ms: 2000, roots: None })?;
        for r in resp {
            if let Response::Synced { roots } = r {
                return Ok(roots);
            }
        }
        Ok(Default::default())
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
        let resp = self.send(&Request::Subscribe { name: Some("fx-watch".into()), cursor: None })?;
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

/// Locate the `findexd` binary: next to `fx`, else on `$PATH`.
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
        "findexd not found next to fx or on PATH (set FINDEXD_BIN)".into(),
    ))
}
