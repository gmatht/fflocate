//! `ff` — the findex command-line client (§12).
//!
//! ```text
//! ff main.rs                       # instant search
//! ff -0 "test" | xargs -0 rg TODO  # pipe-friendly; exit 1 on no match
//! ff --count ext:pdf
//! ff --json --limit 1000 "2024"
//! ff --sync "notes"                # settle, then search — deterministic
//! ff --since c:7:481221            # what changed since
//! ff stats --roots
//! ff rescan /data
//! ff update [--timeout 120] [ROOT...]  # reconcile all roots, then settle
//! ff watch                         # tail live events
//! ff service install               # systemd user unit / launchd plist
//! ```
//!
//! `ff` auto-spawns the daemon if not running.

mod client;
mod find;
mod locate;
mod service;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use client::{Client, CliError};

const USAGE: &str = "\
ff — search the findex index

USAGE:
    ff [OPTIONS] [QUERY...]
    ff stats [--roots] [--json]
    ff rescan <ROOT>
    ff update [--timeout SECS] [ROOT...]
    ff watch [QUERY...]
    ff service install [--socket PATH]
    ff --gui                         # start the GUI (ff-gui)
    find ...                         # via a `find` or `ffind` symlink

OPTIONS:
    -c, --count         print only the number of matches
    -j, --json          emit one JSON object per hit
    -0                  separate paths with NUL (for xargs -0)
    -l, --limit N       stop after N results (0 = unlimited)
    -s, --sync          settle the index before searching (cookie sync)
    -f, --fuzzy         approximate match: plain terms within edit distance,
                        results ranked by score (see --distance)
        --distance N    max edit distance for --fuzzy (default 1)
        --since CURSOR  report what changed since a cursor
        --expr JSON     Watchman-style expression/query (JSON object or tree)
        --timeout SECS  settle timeout for --sync/--update (default 2/120)
        --gui           start the GUI instead of searching; finds the
                        `ff-gui` binary next to ff or on PATH
                        (override with FF_GUI_BIN)
    -h, --help          show this help
    -V, --version       print version

QUERY:
    space-separated terms are ANDed; \"...\" is a phrase; * and ? are
    wildcards; modifiers: ext:rs  path:src  case:Foo  regex:^a.*z$

    --expr runs Watchman's expression-tree language instead, e.g.
      ff --expr '{\"expression\": [\"allof\", [\"type\",\"f\"], [\"suffix\",\"rs\"]]}'
      ff --expr '[\"match\", \"**/*.rs\", \"wholename\"]'
      ff --expr '{\"glob\": [\"**/*.rs\"], \"relative_root\": \"src\"}'

UPDATE:
    ff update reconciles every root with the filesystem (rescan, then
    settle) — the manual fix for changes the watcher missed: daemon
    downtime, offline edits, overflow gaps. With no ROOTs all roots are
    updated; otherwise only the named ones. One `status<TAB>root` line per
    root; exit 1 if any root is not `ok`.

EXIT STATUS:
    0  matches found (or a non-search command succeeded)
    1  no matches (or update left a root unreconciled)
    2  error
";

#[derive(Debug, Default)]
struct Args {
    count: bool,
    json: bool,
    zero: bool,
    limit: usize,
    sync: bool,
    since: Option<String>,
    roots: bool,
    command: Option<String>,
    query: Vec<String>,
    socket: Option<String>,
    expr: Option<serde_json::Value>,
    /// Settle timeout in ms (--timeout SECS; per-command defaults).
    timeout_ms: Option<u64>,
    /// Approximate (edit-distance) matching (§10/§17 v2).
    fuzzy: bool,
    /// Edit budget for --fuzzy (default 1).
    distance: Option<u32>,
    /// Launch the GUI (`ff-gui`) instead of searching.
    gui: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("ff: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, CliError> {
    // Drop-in `locate` replacement when invoked under that name (§12, locate).
    if locate::invoked_as_locate() {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        return locate::run(&argv);
    }

    // Drop-in `find` / `ffind` replacement when invoked under either name.
    if find::invoked_as_find() {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        return find::run(&argv);
    }

    let args = parse_args(std::env::args().skip(1))?;

    // `ff --gui` starts the GUI. The GUI is a separate binary (`ff-gui`), so
    // this is a launcher, not a mode of `ff` itself.
    if args.gui {
        return launch_gui();
    }

    match args.command.as_deref() {
        Some("service") => return service::run(&args.query),
        Some("stats") => {
            let mut c = Client::connect(args.socket.as_deref())?;
            return c.stats(args.roots, args.json);
        }
        Some("rescan") => {
            let root = args.query.first().ok_or_else(|| CliError::Usage("rescan needs a ROOT".into()))?;
            let mut c = Client::connect(args.socket.as_deref())?;
            return c.rescan(root);
        }
        Some("update") => {
            let timeout = args.timeout_ms.unwrap_or(120_000);
            let mut c = Client::connect(args.socket.as_deref())?;
            let status = c.update(&args.query, timeout)?;
            let mut all_ok = true;
            let mut roots: Vec<&String> = status.keys().collect();
            roots.sort();
            for root in roots {
                let st = &status[root];
                if st != "ok" {
                    all_ok = false;
                }
                println!("{st}\t{root}");
            }
            if !all_ok {
                eprintln!("ff: update left a root unreconciled (see statuses above)");
                return Ok(ExitCode::from(1));
            }
            return Ok(ExitCode::SUCCESS);
        }
        Some("watch") => {
            let q = args.query.join(" ");
            let mut c = Client::connect(args.socket.as_deref())?;
            return c.watch(&q);
        }
        Some(other) => return Err(CliError::Usage(format!("unknown command: {other}"))),
        None => {}
    }

    // Search path.
    let mut c = Client::connect(args.socket.as_deref())?;

    if let Some(cursor) = &args.since {
        return c.since(cursor, &args.query.join(" "));
    }
    let q = args.query.join(" ");
    if args.sync {
        let status = c.sync_timeout(args.timeout_ms.unwrap_or(2000))?;
        // Report non-ok roots to stderr but keep going: loudly stale beats
        // silently wrong (§7), yet the search still answers.
        for (root, st) in &status {
            if st != "ok" {
                eprintln!("ff: warning: root {root} sync: {st}");
            }
        }
    }
    c.query(
        &q,
        client::QueryOpts {
            limit: args.limit,
            json: args.json,
            count: args.count,
            zero: args.zero,
            expr: args.expr,
            fuzzy: args.fuzzy,
            distance: args.distance,
        },
    )
}

fn parse_args<I: Iterator<Item = String>>(mut it: I) -> Result<Args, CliError> {
    let mut a = Args::default();
    let mut command: Option<String> = None;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("ff {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-c" | "--count" => a.count = true,
            "-j" | "--json" => a.json = true,
            "-0" => a.zero = true,
            "-s" | "--sync" => a.sync = true,
            "--gui" => a.gui = true,
            "-l" | "--limit" => {
                let v = it.next().ok_or_else(|| CliError::Usage("--limit needs a number".into()))?;
                a.limit = v.parse().map_err(|_| CliError::Usage(format!("bad limit: {v}")))?;
            }
            "--since" => {
                a.since = Some(it.next().ok_or_else(|| CliError::Usage("--since needs a cursor".into()))?);
            }
            "--timeout" => {
                let v = it.next().ok_or_else(|| CliError::Usage("--timeout needs seconds".into()))?;
                let secs: u64 = v
                    .parse()
                    .map_err(|_| CliError::Usage(format!("bad timeout: {v}")))?;
                a.timeout_ms = Some(secs.saturating_mul(1000));
            }
            "--roots" => a.roots = true,
            "-f" | "--fuzzy" => a.fuzzy = true,
            "--distance" => {
                let v = it.next().ok_or_else(|| CliError::Usage("--distance needs a number".into()))?;
                let d: u32 = v
                    .parse()
                    .map_err(|_| CliError::Usage(format!("bad distance: {v}")))?;
                a.distance = Some(d);
            }
            "--expr" => {
                let v = it.next().ok_or_else(|| CliError::Usage("--expr needs JSON".into()))?;
                a.expr = Some(
                    serde_json::from_str(&v)
                        .map_err(|e| CliError::Usage(format!("bad --expr JSON: {e}")))?,
                );
            }
            "--socket" => {
                a.socket = Some(it.next().ok_or_else(|| CliError::Usage("--socket needs a path".into()))?);
            }
            "--" => {
                a.query.extend(it);
                break;
            }
            s if s.starts_with('-') && s.len() > 1 && command.is_none() => {
                // Unknown leading-dash token: treat it as a query term (a
                // negation like `-main` is legitimate query syntax, §10).
                a.query.push(s.to_string());
            }
            s => {
                if command.is_none() && is_command(s) {
                    command = Some(s.to_string());
                } else {
                    a.query.push(s.to_string());
                }
            }
        }
    }
    // Accept `--limit=100` style too.
    a.command = command;
    Ok(a)
}

/// Locate a sibling binary (a `findexd`/`ff-gui` next to `ff`).
///
/// Mirrors the daemon lookup: `$<OVERRIDE>`, then the directory of `argv[0]`
/// (deliberately *not* resolving a symlink, so a `ff` symlinked into a bin dir
/// finds its siblings there), then `current_exe()`'s directory, then `$PATH`.
fn sibling_binary(name: &str, override_var: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(override_var) {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(arg0) = std::env::args_os().next() {
        if let Some(dir) = Path::new(&arg0).parent() {
            if !dir.as_os_str().is_empty() {
                dirs.push(dir.to_path_buf());
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    for dir in dirs {
        let cand = dir.join(name);
        if cand.exists() {
            return Some(cand);
        }
    }
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        if dir.is_empty() {
            continue;
        }
        let cand = Path::new(dir).join(name);
        if cand.exists() {
            return Some(cand);
        }
    }
    None
}

/// `ff --gui`: hand over to the `ff-gui` binary.
///
/// The GUI is a separate process (it links a widget toolkit `ff` does not).
/// We `exec` it so it replaces this process — no extra shell, and its exit
/// status is ours.
fn launch_gui() -> Result<ExitCode, CliError> {
    let exe = sibling_binary("ff-gui", "FF_GUI_BIN").ok_or_else(|| {
        CliError::Usage(
            "ff-gui not found: looked next to ff, next to the invoked path, and on \
             PATH (set FF_GUI_BIN to override, or build it with `cargo build -p ff-gui`)"
                .into(),
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // `exec` only returns on failure.
        let err = std::process::Command::new(&exe).exec();
        Err(CliError::Io(format!("cannot exec {}: {err}", exe.display())))
    }
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new(&exe)
            .status()
            .map_err(|e| CliError::Io(format!("cannot run {}: {e}", exe.display())))?;
        Ok(ExitCode::from(status.code().unwrap_or(0) as u8))
    }
}

fn is_command(s: &str) -> bool {
    matches!(s, "stats" | "rescan" | "update" | "watch" | "service")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &[&str]) -> Args {
        parse_args(v.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn gui_flag_is_a_flag_not_a_query_term() {
        let a = parse(&["--gui"]);
        assert!(a.gui);
        assert!(a.query.is_empty(), "--gui must not leak into the query: {:?}", a.query);
    }

    #[test]
    fn gui_flag_can_combine_with_other_flags() {
        let a = parse(&["--gui", "-j"]);
        assert!(a.gui && a.json);
        assert!(a.query.is_empty());
    }

    #[test]
    fn plain_query() {
        let a = parse(&["main.rs"]);
        assert_eq!(a.query, vec!["main.rs"]);
        assert!(a.command.is_none());
    }

    #[test]
    fn multiword_query_joins() {
        let a = parse(&["src ext:rs"]);
        assert_eq!(a.query.join(" "), "src ext:rs");
    }

    #[test]
    fn flags_parsed() {
        let a = parse(&["-c", "--json", "-0", "-l", "10", "foo"]);
        assert!(a.count && a.json && a.zero);
        assert_eq!(a.limit, 10);
        assert_eq!(a.query, vec!["foo"]);
    }

    #[test]
    fn sync_and_since() {
        let a = parse(&["--sync", "notes"]);
        assert!(a.sync);
        let a = parse(&["--since", "c:7:1"]);
        assert_eq!(a.since.as_deref(), Some("c:7:1"));
    }

    #[test]
    fn subcommands() {
        assert_eq!(parse(&["stats", "--roots"]).command.as_deref(), Some("stats"));
        assert!(parse(&["stats", "--roots"]).roots);
        assert_eq!(parse(&["rescan", "/data"]).command.as_deref(), Some("rescan"));
        assert_eq!(parse(&["rescan", "/data"]).query, vec!["/data"]);
    }

    #[test]
    fn update_command_and_timeout() {
        let a = parse(&["update"]);
        assert_eq!(a.command.as_deref(), Some("update"));
        assert!(a.query.is_empty());
        assert_eq!(a.timeout_ms, None);
        let a = parse(&["update", "--timeout", "30", "/data", "/srv"]);
        assert_eq!(a.command.as_deref(), Some("update"));
        assert_eq!(a.timeout_ms, Some(30_000));
        assert_eq!(a.query, vec!["/data", "/srv"]);
        // --timeout also applies to --sync.
        let a = parse(&["--timeout", "5", "--sync", "x"]);
        assert_eq!(a.timeout_ms, Some(5_000));
        assert!(a.sync);
        assert!(parse_args(["--timeout", "xx"].iter().map(|s| s.to_string())).is_err());
    }

    #[test]
    fn fuzzy_flags_parsed() {
        let a = parse(&["--fuzzy", "main"]);
        assert!(a.fuzzy);
        assert_eq!(a.distance, None, "distance defaults server-side");
        assert_eq!(a.query, vec!["main"]);

        let a = parse(&["-f", "main"]);
        assert!(a.fuzzy, "-f is the short form");

        let a = parse(&["--fuzzy", "--distance", "2", "main"]);
        assert!(a.fuzzy);
        assert_eq!(a.distance, Some(2));

        // Without --fuzzy the distance is inert but still parsed.
        let a = parse(&["--distance", "3", "main"]);
        assert!(!a.fuzzy);
        assert_eq!(a.distance, Some(3));

        assert!(parse_args(["--distance", "xx"].iter().map(|s| s.to_string())).is_err());
        assert!(parse_args(["--distance"].iter().map(|s| s.to_string())).is_err());
    }

    #[test]
    fn query_starting_with_dash_after_command() {
        // `-foo` as a query term must not be parsed as an option when it looks
        // like a flag but the user meant a negation term.
        let a = parse(&["rs", "-main"]);
        assert_eq!(a.query, vec!["rs", "-main"]);
    }

    #[test]
    fn expr_flag_parses_json() {
        let a = parse(&["--expr", r#"["suffix","rs"]"#]);
        assert_eq!(a.expr, Some(serde_json::json!(["suffix", "rs"])));
        let a = parse(&["--expr", r#"{"glob":["**/*.rs"]}"#]);
        assert_eq!(a.expr, Some(serde_json::json!({"glob": ["**/*.rs"]})));
        // Bad JSON is a usage error.
        assert!(parse_args(["--expr", "not json"].iter().map(|s| s.to_string())).is_err());
    }
}
