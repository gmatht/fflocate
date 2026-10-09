//! `fx` — the findex command-line client (§12).
//!
//! ```text
//! fx main.rs                       # instant search
//! fx -0 "test" | xargs -0 rg TODO  # pipe-friendly; exit 1 on no match
//! fx --count ext:pdf
//! fx --json --limit 1000 "2024"
//! fx --sync "notes"                # settle, then search — deterministic
//! fx --since c:7:481221            # what changed since
//! fx stats --roots
//! fx rescan /data
//! fx watch                         # tail live events
//! fx service install               # systemd user unit / launchd plist
//! ```
//!
//! `fx` auto-spawns the daemon if not running.

mod client;
mod service;

use std::process::ExitCode;

use client::{Client, CliError};

const USAGE: &str = "\
fx — search the findex index

USAGE:
    fx [OPTIONS] [QUERY...]
    fx stats [--roots] [--json]
    fx rescan <ROOT>
    fx watch [QUERY...]
    fx service install [--socket PATH]

OPTIONS:
    -c, --count         print only the number of matches
    -j, --json          emit one JSON object per hit
    -0                  separate paths with NUL (for xargs -0)
    -l, --limit N       stop after N results (0 = unlimited)
    -s, --sync          settle the index before searching (cookie sync)
        --since CURSOR  report what changed since a cursor
    -h, --help          show this help
    -V, --version       print version

QUERY:
    space-separated terms are ANDed; \"...\" is a phrase; * and ? are
    wildcards; modifiers: ext:rs  path:src  case:Foo  regex:^a.*z$

EXIT STATUS:
    0  matches found (or a non-search command succeeded)
    1  no matches
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
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("fx: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, CliError> {
    let args = parse_args(std::env::args().skip(1))?;

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
        let status = c.sync(args.socket.clone())?;
        // Report non-ok roots to stderr but keep going: loudly stale beats
        // silently wrong (§7), yet the search still answers.
        for (root, st) in &status {
            if st != "ok" {
                eprintln!("fx: warning: root {root} sync: {st}");
            }
        }
    }
    c.query(
        &q,
        args.limit,
        args.json,
        args.count,
        args.zero,
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
                println!("fx {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-c" | "--count" => a.count = true,
            "-j" | "--json" => a.json = true,
            "-0" => a.zero = true,
            "-s" | "--sync" => a.sync = true,
            "-l" | "--limit" => {
                let v = it.next().ok_or_else(|| CliError::Usage("--limit needs a number".into()))?;
                a.limit = v.parse().map_err(|_| CliError::Usage(format!("bad limit: {v}")))?;
            }
            "--since" => {
                a.since = Some(it.next().ok_or_else(|| CliError::Usage("--since needs a cursor".into()))?);
            }
            "--roots" => a.roots = true,
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

fn is_command(s: &str) -> bool {
    matches!(s, "stats" | "rescan" | "watch" | "service")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &[&str]) -> Args {
        parse_args(v.iter().map(|s| s.to_string())).unwrap()
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
    fn query_starting_with_dash_after_command() {
        // `-foo` as a query term must not be parsed as an option when it looks
        // like a flag but the user meant a negation term.
        let a = parse(&["rs", "-main"]);
        assert_eq!(a.query, vec!["rs", "-main"]);
    }
}
