//! `locate` compatibility mode (§12).
//!
//! When this binary is invoked under the name `locate` (argv\[0\], e.g. via a
//! symlink), it acts as a drop-in replacement for GNU locate: the same flags,
//! the same stdout (absolute paths, one per line), the same exit statuses
//! (0 = matches, 1 = none, 2 = error). The "database" is the live findex
//! index instead of an updatedb snapshot, so results are always current;
//! `-d/--database` is accepted and ignored for script compatibility.
//!
//! Pattern semantics follow GNU locate: a pattern without glob characters
//! behaves as `*PATTERN*`; otherwise it is a shell glob matched against the
//! whole path (`*` crosses `/`). `-r/--regex` patterns are regexes matched
//! anywhere (unanchored), like `locate -r`. Multiple patterns are ORed;
//! `-A` requires all of them.

use std::ffi::OsStr;
use std::process::ExitCode;

use crate::client::{Client, CliError};

const USAGE: &str = "\
locate — find files by name in the findex index (locate-compatible)

USAGE:
    locate [OPTIONS] PATTERN...

OPTIONS:
    -b, --basename      match only the basename (default: whole path)
    -c, --count         print only the number of matches
    -d, --database DB   accepted and ignored (the live index replaces it)
    -e, --existing      only print paths that still exist
    -A, --all           all patterns must match (default: any matches)
    -i, --ignore-case   case-insensitive matching
    -l, --limit N       stop after N results (0 = unlimited)
    -L, --follow        follow symlinks when checking -e (default: -P)
    -P, --nofollow      do not follow symlinks when checking -e (default)
    -0, --null          separate paths with NUL (for xargs -0)
    -q, --quiet         suppress warnings on stderr
    -r, --regexp RE     patterns are regexes matched anywhere (like -r)
        --regex         same as -r (patterns are regexes)
    -S, --statistics    print index statistics instead of searching
    -V, --version       print version
    -h, --help          show this help

EXIT STATUS:
    0  matches found (or --statistics/--version succeeded)
    1  no matches
    2  error
";

/// True when argv\[0\] names this binary `locate` (directly or via symlink).
pub fn invoked_as_locate() -> bool {
    std::env::args_os()
        .next()
        .and_then(|a| std::path::Path::new(&a).file_name().map(|n| n.to_owned()))
        .is_some_and(|n| n == OsStr::new("locate"))
}

#[derive(Debug, Default)]
pub struct LocateArgs {
    pub patterns: Vec<String>,
    pub ignore_case: bool,
    pub count: bool,
    pub limit: usize,
    pub zero: bool,
    pub regexp: bool,
    pub basename: bool,
    pub match_all: bool,
    pub existing: bool,
    pub follow: bool,
    pub quiet: bool,
    pub stats: bool,
}

pub fn run(argv: &[String]) -> Result<ExitCode, CliError> {
    let args = parse_args(argv.iter().cloned())?;

    if args.stats {
        let mut c = Client::connect(None)?;
        return c.stats(false, false);
    }

    if args.patterns.is_empty() {
        return Err(CliError::Usage("locate: no pattern to search for".into()));
    }

    let expr = build_expression(&args);
    let mut c = Client::connect(None)?;
    c.locate(&args, expr)
}

/// Does the pattern contain unescaped glob characters?
fn has_glob_chars(pat: &str) -> bool {
    let mut it = pat.bytes();
    while let Some(c) = it.next() {
        match c {
            b'\\' => {
                it.next();
            }
            b'*' | b'?' | b'[' => return true,
            _ => {}
        }
    }
    false
}

/// Translate a shell glob to a regex body (the caller anchors it). `*`
/// crosses `/`: locate matches globs against the whole path with no
/// `FNM_PATHNAME`. Only `!` negates a class (`[^...]` keeps a literal `^`,
/// as in fnmatch); a `[` without a closer is literal.
pub fn glob_to_regex(pat: &str) -> String {
    let chars: Vec<char> = pat.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                out.push_str(".*");
                i += 1;
            }
            '?' => {
                out.push('.');
                i += 1;
            }
            '\\' => {
                if i + 1 < chars.len() {
                    out.push_str(&regex::escape(&chars[i + 1].to_string()));
                    i += 2;
                } else {
                    out.push_str("\\\\");
                    i += 1;
                }
            }
            '[' => {
                // Find the closer: skip a leading '!' and a literal ']'.
                let mut j = i + 1;
                if j < chars.len() && chars[j] == '!' {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ']' {
                    j += 1;
                }
                while j < chars.len() && chars[j] != ']' {
                    j += 1;
                }
                if j >= chars.len() {
                    // No closer: literal '['.
                    out.push_str("\\[");
                    i += 1;
                } else {
                    out.push('[');
                    let mut k = i + 1;
                    if chars[k] == '!' {
                        out.push('^');
                        k += 1;
                    }
                    while k <= j {
                        let c = chars[k];
                        if c == '^' && k == i + 1 {
                            // Literal leading '^' (fnmatch does not negate
                            // on '^'); a regex would read it as negation.
                            out.push_str("\\^");
                        } else if c == '\\' && k < j {
                            out.push('\\');
                            out.push(chars[k + 1]);
                            k += 1;
                        } else {
                            out.push(c);
                        }
                        k += 1;
                    }
                    i = j + 1;
                }
            }
            c => {
                out.push_str(&regex::escape(&c.to_string()));
                i += 1;
            }
        }
    }
    out
}

/// Build the `pcre`/`ipcre` expression for the parsed arguments.
fn build_expression(o: &LocateArgs) -> serde_json::Value {
    let scope = if o.basename { "basename" } else { "wholename" };
    let term = if o.ignore_case { "ipcre" } else { "pcre" };
    let mut terms = Vec::new();
    for pat in &o.patterns {
        let body = if o.regexp {
            // User regex, unanchored like `locate -r`.
            pat.clone()
        } else if has_glob_chars(pat) {
            // Full-path glob: anchor both ends (fnmatch is a full match).
            format!("^(?:{})\\z", glob_to_regex(pat))
        } else {
            // No glob characters: substring match (`*PATTERN*`).
            regex::escape(pat)
        };
        terms.push(serde_json::json!([term, body, scope]));
    }
    if terms.len() == 1 {
        terms.pop().unwrap()
    } else if o.match_all {
        serde_json::Value::Array(
            std::iter::once(serde_json::json!("allof")).chain(terms).collect(),
        )
    } else {
        serde_json::Value::Array(
            std::iter::once(serde_json::json!("anyof")).chain(terms).collect(),
        )
    }
}

fn parse_args<I: Iterator<Item = String>>(mut it: I) -> Result<LocateArgs, CliError> {
    let mut a = LocateArgs::default();
    let mut positional_only = false;
    while let Some(arg) = it.next() {
        if positional_only {
            a.patterns.push(arg);
            continue;
        }
        if arg == "--" {
            positional_only = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match name {
                "basename" => a.basename = true,
                "count" => a.count = true,
                "database" => {
                    let _ = val.or_else(|| it.next()).ok_or_else(|| {
                        CliError::Usage("locate: --database needs a path".into())
                    })?;
                }
                "existing" => a.existing = true,
                "all" => a.match_all = true,
                "ignore-case" => a.ignore_case = true,
                "limit" => {
                    let v = val.or_else(|| it.next()).ok_or_else(|| {
                        CliError::Usage("locate: --limit needs a number".into())
                    })?;
                    a.limit = v
                        .parse()
                        .map_err(|_| CliError::Usage(format!("locate: bad limit: {v}")))?;
                }
                "follow" => a.follow = true,
                "nofollow" => a.follow = false,
                "null" => a.zero = true,
                "quiet" => a.quiet = true,
                "regexp" | "regex" => a.regexp = true,
                "statistics" => a.stats = true,
                "version" => {
                    println!("ff {} (locate compatible)", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                "help" => {
                    print!("{USAGE}");
                    std::process::exit(0);
                }
                other => {
                    return Err(CliError::Usage(format!("locate: unknown option --{other}")));
                }
            }
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            let mut cs = arg[1..].chars();
            while let Some(c) = cs.next() {
                match c {
                    'b' => a.basename = true,
                    'c' => a.count = true,
                    'e' => a.existing = true,
                    'A' => a.match_all = true,
                    'i' => a.ignore_case = true,
                    '0' => a.zero = true,
                    'q' => a.quiet = true,
                    'S' => a.stats = true,
                    'L' => a.follow = true,
                    'P' => a.follow = false,
                    'V' => {
                        println!("ff {} (locate compatible)", env!("CARGO_PKG_VERSION"));
                        std::process::exit(0);
                    }
                    'h' => {
                        print!("{USAGE}");
                        std::process::exit(0);
                    }
                    'd' | 'l' | 'r' => {
                        // Attached (`-l10`, `-rPAT`) or separate argument.
                        let rest: String = cs.collect();
                        let v = if rest.is_empty() {
                            it.next().ok_or_else(|| {
                                CliError::Usage(format!("locate: -{c} needs an argument"))
                            })?
                        } else {
                            rest
                        };
                        match c {
                            'l' => {
                                a.limit = v.parse().map_err(|_| {
                                    CliError::Usage(format!("locate: bad limit: {v}"))
                                })?;
                            }
                            'r' => {
                                a.regexp = true;
                                a.patterns.push(v);
                            }
                            // -d is accepted and ignored (live index).
                            _ => {}
                        }
                        break;
                    }
                    other => {
                        return Err(CliError::Usage(format!(
                            "locate: unknown option -{other}"
                        )));
                    }
                }
            }
            continue;
        }
        a.patterns.push(arg);
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &[&str]) -> LocateArgs {
        parse_args(v.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn flags_and_combined_shorts() {
        let a = parse(&["-ic", "foo"]);
        assert!(a.ignore_case && a.count);
        assert_eq!(a.patterns, vec!["foo"]);
        let a = parse(&["-A", "-b", "-0", "-e", "-q", "x", "y"]);
        assert!(a.match_all && a.basename && a.zero && a.existing && a.quiet);
        assert_eq!(a.patterns, vec!["x", "y"]);
    }

    #[test]
    fn limits_and_attached_args() {
        assert_eq!(parse(&["-l", "10", "x"]).limit, 10);
        assert_eq!(parse(&["-l10", "x"]).limit, 10);
        assert_eq!(parse(&["--limit=5", "x"]).limit, 5);
        let a = parse(&["-r", ".*rs$", "x"]);
        assert!(a.regexp);
        assert_eq!(a.patterns, vec![".*rs$", "x"]);
        let a = parse(&["-r.*rs$"]);
        assert!(a.regexp && a.patterns == vec![".*rs$"]);
        // -d is accepted and ignored.
        let a = parse(&["-d", "/var/lib/mlocate/mlocate.db", "x"]);
        assert_eq!(a.patterns, vec!["x"]);
        assert!(parse_args(["--bogus"].iter().map(|s| s.to_string())).is_err());
        assert!(parse_args(["-Z"].iter().map(|s| s.to_string())).is_err());
    }

    #[test]
    fn dash_dash_ends_options() {
        let a = parse(&["--", "-c"]);
        assert_eq!(a.patterns, vec!["-c"]);
        assert!(!a.count);
    }

    #[test]
    fn glob_detection() {
        assert!(!has_glob_chars("foo.rs"));
        assert!(!has_glob_chars("a.b"));
        assert!(has_glob_chars("*.rs"));
        assert!(has_glob_chars("foo?bar"));
        assert!(has_glob_chars("a[bc]"));
        assert!(!has_glob_chars("a\\*b")); // escaped star is literal
        assert!(!has_glob_chars("100%"));
    }

    #[test]
    fn glob_translation() {
        assert_eq!(glob_to_regex("*.rs"), ".*\\.rs");
        assert_eq!(glob_to_regex("foo?bar"), "foo.bar");
        assert_eq!(glob_to_regex("a[bc]d"), "a[bc]d");
        assert_eq!(glob_to_regex("a[!bc]d"), "a[^bc]d");
        // '[^...]' keeps a literal caret (fnmatch has no ^-negation).
        assert_eq!(glob_to_regex("a[^bc]d"), "a[\\^bc]d");
        // Unclosed bracket is literal.
        assert_eq!(glob_to_regex("ab["), "ab\\[");
        // Escaped star stays literal.
        assert_eq!(glob_to_regex("a\\*b"), "a\\*b");
        // Plain text is regex-escaped (space needs no escape).
        assert_eq!(glob_to_regex("a+b (c)"), "a\\+b \\(c\\)");
    }

    #[test]
    fn expression_shapes() {
        let o = LocateArgs { patterns: vec!["foo".into()], ..Default::default() };
        assert_eq!(build_expression(&o), serde_json::json!(["pcre", "foo", "wholename"]));
        let o = LocateArgs {
            patterns: vec!["*.rs".into()],
            ignore_case: true,
            basename: true,
            ..Default::default()
        };
        assert_eq!(
            build_expression(&o),
            serde_json::json!(["ipcre", "^(?:.*\\.rs)\\z", "basename"])
        );
        let o = LocateArgs {
            patterns: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        assert_eq!(
            build_expression(&o),
            serde_json::json!(["anyof", ["pcre", "a", "wholename"], ["pcre", "b", "wholename"]])
        );
        let o = LocateArgs {
            patterns: vec!["a".into(), "b".into()],
            match_all: true,
            regexp: true,
            ..Default::default()
        };
        assert_eq!(
            build_expression(&o),
            serde_json::json!(["allof", ["pcre", "a", "wholename"], ["pcre", "b", "wholename"]])
        );
    }
}
