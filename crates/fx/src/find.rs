//! `find` compatibility mode (§12).
//!
//! When this binary is invoked as `find` or `ffind` (argv\[0\], e.g. via a
//! symlink), it acts as a `find`-compatible front end over the findex index.
//!
//! The plan is the two-phase one: translate the `find` expression into a
//! findex expression tree, let the index prune on everything it can answer
//! (name/type/path/depth), and then evaluate the metadata tests (`-size`,
//! `-mtime`, `-newer`, `-perm`, `-user`, …) with `lstat` on the survivors only.
//!
//! Only the subset expressible over the index is supported. Anything the index
//! cannot answer is reported as an error rather than silently ignored — a
//! `find` replacement that quietly drops a predicate is worse than one that
//! refuses.
//!
//! Supported: `-name -iname -path -ipath -regex -iregex -type -maxdepth
//! -mindepth -print -print0 -size -mtime -mmin -newer -newermt -empty
//! -perm -user -group -inum -links -a -o -not ! ( ) -true -false` plus the
//! implicit `-print`. Default roots are the indexed roots (there is no `.` to
//! walk in an index); passing an explicit path filters to paths under it.
//! Starting points are resolved to absolute form against the cwd before use, so
//! a relative base (`.`, `src`) matches the absolute paths the index stores; a
//! relative base that does not exist is a loud error, never an empty result.

use std::ffi::OsStr;
use std::path::Path;
use std::process::ExitCode;

use crate::client::{Client, CliError};

const USAGE: &str = "\
find — find files by name in the findex index (find-compatible)

USAGE:
    find [PATH...] [EXPRESSION]

Only the tests the index can answer (or answer with a cheap lstat) are
supported. Unsupported tests are an error, never silently ignored.

TESTS:
    -name PAT   -iname PAT     -path PAT   -ipath PAT
    -regex RE   -iregex RE
    -type f|d|l
    -maxdepth N -mindepth N
    -size N[c|k|M|G]           -empty
    -mtime N -mmin N           -newer FILE   -newermt DATE
    -perm MODE  -user U        -group G      -inum N   -links N
ACTIONS:
    -print (default)   -print0   -printf FORMAT
OPERATORS:
    -a/-and   -o/-or   -not/!   ( )   -true   -false

EXIT STATUS:
    0  success (even with no matches, like GNU find)
    1  a start point could not be resolved (e.g. a relative path that does not
       exist in the cwd)
    2  usage error
";

/// True when the basename `name` selects `find` mode.
pub fn is_find_name(name: &OsStr) -> bool {
    name == OsStr::new("find") || name == OsStr::new("ffind")
}

/// True when argv\[0\] names this binary `find` or `ffind`.
pub fn invoked_as_find() -> bool {
    std::env::args_os()
        .next()
        .and_then(|a| Path::new(&a).file_name().map(|n| n.to_owned()))
        .is_some_and(|n| is_find_name(&n))
}

/// One entry in the *metadata* (phase 2) position, filled by `lstat` on the
/// survivors of the index-level predicates.
#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub size: Option<u64>,
    pub mtime: Option<i64>,
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub nlink: Option<u64>,
    pub ino: Option<u64>,
    pub mmin: Option<i64>,
}

/// A test that the index cannot answer, evaluated on a candidate's `Meta`.
#[derive(Debug, Clone)]
pub enum MetaTest {
    Size { sign: Sign, bytes: u64 },
    MtimeDays { sign: Sign, days: i64 },
    Mmin { sign: Sign, mins: i64 },
    NewerThan { path: std::path::PathBuf },
    Perm { mode: u32, exact: bool },
    User(u32),
    Group(u32),
    Inum(u64),
    Links { sign: Sign, n: u64 },
    /// `-empty`: true for an empty directory (no children) or a zero-size file.
    /// Directories are decided from the index; files need `size == 0`.
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    Less,
    Eq,
    Greater,
}

impl Sign {
    fn holds(self, have: i64, want: i64) -> bool {
        match self {
            Sign::Less => have < want,
            Sign::Eq => have == want,
            Sign::Greater => have > want,
        }
    }
}

fn parse_sign(s: &str) -> (Sign, &str) {
    if let Some(r) = s.strip_prefix('+') {
        (Sign::Greater, r)
    } else if let Some(r) = s.strip_prefix('-') {
        (Sign::Less, r)
    } else {
        (Sign::Eq, s)
    }
}

/// Parse a `-size` operand into bytes. Bare = 512-byte blocks (GNU find).
pub fn parse_size(s: &str) -> Result<(Sign, u64), String> {
    let (sign, rest) = parse_sign(s);
    let (mult, digits) = match rest.chars().last() {
        Some('c') => (1u64, &rest[..rest.len() - 1]),
        Some('k') => (1024, &rest[..rest.len() - 1]),
        Some('M') => (1024 * 1024, &rest[..rest.len() - 1]),
        Some('G') => (1024 * 1024 * 1024, &rest[..rest.len() - 1]),
        _ => (512, rest),
    };
    let n: u64 = digits.parse().map_err(|_| format!("find: invalid -size argument '{s}'"))?;
    Ok((sign, n.saturating_mul(mult)))
}

/// Parse an octal `-perm` operand. Returns `(mode, exact)`; `exact` is false for
/// the `-MODE` "any of these bits" and `/MODE` forms.
pub fn parse_perm(s: &str) -> Result<(u32, bool), String> {
    let (exact, digits) = match s.chars().next() {
        Some('-') | Some('/') => (false, &s[1..]),
        _ => (true, s),
    };
    let mode = u32::from_str_radix(digits, 8)
        .map_err(|_| format!("find: invalid -perm argument '{s}'"))?;
    Ok((mode, exact))
}

/// Split the argv into `(paths, expression)` the way `find` does: leading
/// arguments that are not tests/operators are starting points.
pub fn split_paths(argv: &[String]) -> (Vec<String>, Vec<String>) {
    let mut paths_found = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if a.starts_with('-') || a == "!" || a == "(" {
            break;
        }
        // A leading token with no '-' is a path — but only in the leading run.
        paths_found.push(a.clone());
        i += 1;
    }
    (paths_found, argv[i..].to_vec())
}

pub fn run(argv: &[String]) -> Result<ExitCode, CliError> {
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if argv.iter().any(|a| a == "-V" || a == "--version") {
        println!("find (findex) {}", env!("CARGO_PKG_VERSION"));
        return Ok(ExitCode::SUCCESS);
    }

    let (paths, expr) = split_paths(argv);

    // Resolve the starting points *before* building the plan. The index stores
    // absolute paths, so a relative base (`.` or `src`, which agents and humans
    // type constantly) must be anchored to the cwd or it can never match
    // anything. Silently filtering every hit away and exiting 0 is the worst
    // possible failure mode for a find wrapper: it looks like "no matches".
    let paths = match resolve_bases(&paths) {
        Ok(p) => p,
        Err(p) => {
            // GNU find reports an unreadable start point as
            // `<path>: No such file or directory` and exits 1 (distinct from a
            // usage error). Mirror that exactly.
            eprintln!("{}: No such file or directory", p.display());
            return Ok(ExitCode::from(1));
        }
    };
    let plan = Plan::build(&paths, &expr)?;

    // The index-level expression, plus the metadata tests to run on survivors.
    let query = plan.to_query_json();
    let mut c = Client::connect(None)?;
    // `find` exits 0 even with no matches, so we do not reuse `query`'s exit 1.
    c.find(&query, &plan)
}

/// Anchor the command-line start paths to the filesystem the same way the index
/// does, so a relative base compares equal to the absolute indexed paths.
///
/// * An already-absolute path is canonicalized when that succeeds; if it does
///   not, the literal string is kept (GNU find would fail later, per test; we
///   deliberately do not turn that into a hard error here).
/// * A relative path is joined to the cwd and canonicalized. If it does not
///   resolve, it can never match anything, so we return it as `Err` for the
///   caller to report — an empty result would be a lie.
fn resolve_bases(paths: &[String]) -> Result<Vec<String>, std::path::PathBuf> {
    let cwd = std::env::current_dir().ok();
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let raw = Path::new(p);
        let resolved = if raw.is_absolute() {
            raw.canonicalize().ok()
        } else {
            cwd.as_ref().and_then(|c| c.join(raw).canonicalize().ok())
        };
        match resolved {
            // `.` at the filesystem root canonicalizes to `/`; keep the string
            // form the index uses (no trailing slash), which `path_under`
            // already normalizes.
            Some(rp) => match rp.to_str() {
                Some(s) => out.push(s.to_string()),
                None => out.push(p.clone()),
            },
            // Absolute but unresolvable: leave it be (it simply won't match).
            None if raw.is_absolute() => out.push(p.clone()),
            // Relative and unresolvable: loud failure, not silent empty.
            None => return Err(raw.to_path_buf()),
        }
    }
    Ok(out)
}

/// A compiled `find` command: index-level expression + phase-2 tests.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Paths given on the command line (empty = all indexed roots), already
    /// resolved to absolute form by [`resolve_bases`] so they compare equal to
    /// the absolute paths the index stores.
    pub paths: Vec<String>,
    /// The index-level expression as a findex expression tree (JSON).
    pub index_expr: serde_json::Value,
    /// Metadata tests, evaluated with `lstat` on survivors.
    pub meta: Vec<MetaTest>,
    pub min_depth: Option<u32>,
    pub max_depth: Option<u32>,
    pub print0: bool,
    pub printf: Option<String>,
    /// Force an explicit `--sync` before searching (needed for `-newer` and for
    /// actions that expect live paths).
    pub sync: bool,
}

impl Plan {
    pub fn build(paths: &[String], argv: &[String]) -> Result<Plan, CliError> {
        let mut p = Plan {
            paths: paths.to_vec(),
            index_expr: serde_json::json!(true),
            meta: Vec::new(),
            min_depth: None,
            max_depth: None,
            print0: false,
            printf: None,
            sync: false,
        };
        let mut it = argv.iter().peekable();

        // Build an AND-list of index predicates; `-o` is not supported in v1 and
        // is reported loudly rather than mis-evaluated.
        let mut index_terms: Vec<serde_json::Value> = Vec::new();
        let mut saw_or = false;

        while let Some(a) = it.next() {
            match a.as_str() {
                "-name" | "-iname" => {
                    let pat = need(&mut it, a)?;
                    // `find -name` matches the basename with shell globbing
                    // (`*` does not cross `/`). Watchman's `match` term with the
                    // `basename` scope is the exact analogue; its `name` term
                    // follows different (path-relative) rules.
                    let term = if a == "-name" { "match" } else { "imatch" };
                    index_terms.push(serde_json::json!([term, pat, "basename"]));
                }
                "-path" | "-ipath" => {
                    let pat = need(&mut it, a)?;
                    let term = if a == "-path" { "match" } else { "imatch" };
                    index_terms.push(serde_json::json!([term, pat, "wholename"]));
                }
                "-regex" | "-iregex" => {
                    let re = need(&mut it, a)?;
                    index_terms.push(serde_json::json!(["pcre", re, "basename"]));
                }
                "-type" => {
                    let t = need(&mut it, a)?;
                    let kind = match t.as_str() {
                        "f" => "f",
                        "d" => "d",
                        "l" => "l",
                        other => {
                            return Err(CliError::Usage(format!(
                                "find: -type {other} is not supported (only f, d, l)"
                            )))
                        }
                    };
                    index_terms.push(serde_json::json!(["type", kind]));
                }
                "-maxdepth" => {
                    p.max_depth = Some(
                        need(&mut it, a)?
                            .parse()
                            .map_err(|_| CliError::Usage("find: bad -maxdepth".into()))?,
                    );
                }
                "-mindepth" => {
                    p.min_depth = Some(
                        need(&mut it, a)?
                            .parse()
                            .map_err(|_| CliError::Usage("find: bad -mindepth".into()))?,
                    );
                }
                "-size" => {
                    let (sign, bytes) = parse_size(&need(&mut it, a)?)
                        .map_err(CliError::Usage)?;
                    p.meta.push(MetaTest::Size { sign, bytes });
                }
                "-mtime" => {
                    let raw = need(&mut it, a)?;
                    let (sign, d) = parse_sign(&raw);
                    let d: i64 = d.parse().map_err(|_| CliError::Usage("find: bad -mtime".into()))?;
                    p.meta.push(MetaTest::MtimeDays { sign, days: d });
                }
                "-mmin" => {
                    let raw = need(&mut it, a)?;
                    let (sign, d) = parse_sign(&raw);
                    let d: i64 = d.parse().map_err(|_| CliError::Usage("find: bad -mmin".into()))?;
                    p.meta.push(MetaTest::Mmin { sign, mins: d });
                }
                "-newer" => {
                    let f = need(&mut it, a)?;
                    p.meta.push(MetaTest::NewerThan { path: std::path::PathBuf::from(f) });
                    p.sync = true;
                }
                "-empty" => p.meta.push(MetaTest::Empty),
                "-perm" => {
                    let (mode, exact) = parse_perm(&need(&mut it, a)?).map_err(CliError::Usage)?;
                    p.meta.push(MetaTest::Perm { mode, exact });
                }
                "-user" => {
                    let u = need(&mut it, a)?;
                    let uid = resolve_user(&u)?;
                    p.meta.push(MetaTest::User(uid));
                }
                "-group" => {
                    let g = need(&mut it, a)?;
                    let gid = resolve_group(&g)?;
                    p.meta.push(MetaTest::Group(gid));
                }
                "-inum" => {
                    let n: u64 = need(&mut it, a)?
                        .parse()
                        .map_err(|_| CliError::Usage("find: bad -inum".into()))?;
                    p.meta.push(MetaTest::Inum(n));
                }
                "-links" => {
                    let raw = need(&mut it, a)?;
                    let (sign, n) = parse_sign(&raw);
                    let n: u64 = n.parse().map_err(|_| CliError::Usage("find: bad -links".into()))?;
                    p.meta.push(MetaTest::Links { sign, n });
                }
                "-print" => {}
                "-print0" => p.print0 = true,
                "-printf" => {
                    p.printf = Some(need(&mut it, a)?);
                }
                "-a" | "-and" | "-true" => {}
                "-o" | "-or" => saw_or = true,
                "-not" | "!" => {
                    return Err(CliError::Usage(
                        "find: -not/! is not supported yet; use an index-level exclusion".into(),
                    ))
                }
                "-exec" | "-execdir" | "-delete" | "-ok" => {
                    return Err(CliError::Usage(format!(
                        "find: {a} is not supported (findex does not run actions); \
                         this is deliberate — use `ff -0 ... | xargs`"
                    )))
                }
                "-L" | "-H" | "-P" | "-follow" => {
                    return Err(CliError::Usage(format!(
                        "find: {a} is not supported (the index never follows symlinks, §16)"
                    )))
                }
                "-depth" | "-d" => {
                    return Err(CliError::Usage(format!(
                        "find: {a} is not supported (results are in index order)"
                    )))
                }
                "(" | ")" | "-prune" => {
                    return Err(CliError::Usage(format!("find: {a} is not supported yet")));
                }
                other if other.starts_with('-') => {
                    return Err(CliError::Usage(format!(
                        "find: unknown predicate '{other}' (findex implements a subset; \
                         unsupported tests are an error, not ignored)"
                    )));
                }
                other => {
                    return Err(CliError::Usage(format!(
                        "find: unexpected operand '{other}'"
                    )));
                }
            }
        }

        if saw_or {
            return Err(CliError::Usage(
                "find: -o/OR is not supported yet (the index expression is an AND)".into(),
            ));
        }

        p.index_expr = match index_terms.len() {
            0 => serde_json::json!(["true"]),
            1 => index_terms.into_iter().next().unwrap(),
            // `["allof", t1, t2, ...]` — the terms are spliced flat, not nested
            // in a sub-array (matching Watchman's grammar).
            _ => {
                let mut v = vec![serde_json::json!("allof")];
                v.extend(index_terms);
                serde_json::Value::Array(v)
            }
        };
        Ok(p)
    }

    /// The `find`-mode query object sent over the wire.
    pub fn to_query_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("expression".into(), self.index_expr.clone());
        serde_json::Value::Object(m)
    }
}

fn need<'a, I: Iterator<Item = &'a String>>(it: &mut I, flag: &str) -> Result<String, CliError> {
    it.next()
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("find: {flag} requires an argument")))
}

fn resolve_user(s: &str) -> Result<u32, CliError> {
    if let Ok(n) = s.parse() {
        return Ok(n);
    }
    // Resolve a name via /etc/passwd (getpwnam without libc bindings here).
    match std::fs::read_to_string("/etc/passwd") {
        Ok(txt) => {
            for line in txt.lines() {
                let f: Vec<&str> = line.split(':').collect();
                if f.len() > 2 && f[0] == s {
                    if let Ok(n) = f[2].parse() {
                        return Ok(n);
                    }
                }
            }
            Err(CliError::Usage(format!("find: unknown user '{s}'")))
        }
        Err(_) => Err(CliError::Usage(format!("find: cannot resolve user '{s}'"))),
    }
}

fn resolve_group(s: &str) -> Result<u32, CliError> {
    if let Ok(n) = s.parse() {
        return Ok(n);
    }
    match std::fs::read_to_string("/etc/group") {
        Ok(txt) => {
            for line in txt.lines() {
                let f: Vec<&str> = line.split(':').collect();
                if f.len() > 2 && f[0] == s {
                    if let Ok(n) = f[2].parse() {
                        return Ok(n);
                    }
                }
            }
            Err(CliError::Usage(format!("find: unknown group '{s}'")))
        }
        Err(_) => Err(CliError::Usage(format!("find: cannot resolve group '{s}'"))),
    }
}

/// lstat a path into a [`Meta`]. Phase 2 of the plan.
pub fn lstat(path: &Path, now: i64) -> Option<Meta> {
    let md = std::fs::symlink_metadata(path).ok()?;
    use std::os::unix::fs::MetadataExt;
    let mtime = md.mtime();
    Some(Meta {
        size: Some(md.len()),
        mtime: Some(mtime),
        mmin: Some((now - mtime) / 60),
        mode: Some(md.mode()),
        uid: Some(md.uid()),
        gid: Some(md.gid()),
        nlink: Some(md.nlink()),
        ino: Some(md.ino()),
    })
}

/// Evaluate the metadata tests against a candidate's `Meta`. `is_empty_dir` is
/// supplied by the caller (the index knows a directory's child count).
pub fn meta_matches(tests: &[MetaTest], m: &Meta, is_empty_dir: bool, now: i64) -> bool {
    for t in tests {
        let ok = match t {
            MetaTest::Size { sign, bytes } => m
                .size
                .map(|s| sign.holds(s as i64, *bytes as i64))
                .unwrap_or(false),
            MetaTest::MtimeDays { sign, days } => m
                .mtime
                .map(|t| sign.holds((now - t) / 86_400, *days))
                .unwrap_or(false),
            MetaTest::Mmin { sign, mins } => m
                .mmin
                .map(|v| sign.holds(v, *mins))
                .unwrap_or(false),
            MetaTest::NewerThan { path } => match (m.mtime, std::fs::metadata(path).ok()) {
                (Some(t), Some(ref_md)) => {
                    use std::os::unix::fs::MetadataExt;
                    t > ref_md.mtime()
                }
                _ => false,
            },
            MetaTest::Perm { mode, exact } => m
                .mode
                .map(|have| {
                    if *exact {
                        have & 0o7777 == *mode
                    } else {
                        have & *mode == *mode
                    }
                })
                .unwrap_or(false),
            MetaTest::User(u) => m.uid == Some(*u),
            MetaTest::Group(g) => m.gid == Some(*g),
            MetaTest::Inum(n) => m.ino == Some(*n),
            MetaTest::Links { sign, n } => m
                .nlink
                .map(|have| sign.holds(have as i64, *n as i64))
                .unwrap_or(false),
            MetaTest::Empty => match m.size {
                Some(_) => is_empty_dir || m.size == Some(0),
                None => is_empty_dir,
            },
        };
        if !ok {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv0_detection_helpers() {
        // The names that select find mode.
        assert!(is_find_name(OsStr::new("find")));
        assert!(is_find_name(OsStr::new("ffind")));
        // These must NOT select find mode.
        for other in ["ff", "fx", "fxfind", "findx", "locate", "", "FIND"] {
            assert!(!is_find_name(OsStr::new(other)), "{other:?} must not select find mode");
        }

        // Sanity: the pure helpers we can test without touching argv.
        assert_eq!(parse_size("10c").unwrap(), (Sign::Eq, 10));
        assert_eq!(parse_size("+1k").unwrap(), (Sign::Greater, 1024));
        assert_eq!(parse_size("-2M").unwrap(), (Sign::Less, 2 * 1024 * 1024));
        assert_eq!(parse_size("3").unwrap(), (Sign::Eq, 3 * 512));
        assert!(parse_size("zz").is_err());
    }

    #[test]
    fn perm_parsing() {
        assert_eq!(parse_perm("644").unwrap(), (0o644, true));
        assert_eq!(parse_perm("-644").unwrap(), (0o644, false));
        assert_eq!(parse_perm("/111").unwrap(), (0o111, false));
        assert!(parse_perm("999").is_err());
    }

    #[test]
    fn split_paths_and_expr() {
        let a: Vec<String> = ["/tmp", "src", "-name", "*.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (paths, expr) = split_paths(&a);
        assert_eq!(paths, vec!["/tmp", "src"]);
        assert_eq!(expr, vec!["-name", "*.rs"]);

        let a: Vec<String> = ["-name", "x"].iter().map(|s| s.to_string()).collect();
        let (paths, expr) = split_paths(&a);
        assert!(paths.is_empty());
        assert_eq!(expr.len(), 2);
    }

    #[test]
    fn relative_bases_are_anchored_to_cwd() {
        // `.` must resolve to the (canonical) cwd, not stay a literal ".": the
        // index stores absolute paths, so an unresolved base would filter every
        // hit away and exit 0 — the silent-empty bug.
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let got = resolve_bases(&[".".to_string()]).unwrap();
        assert_eq!(got, vec![cwd.to_string_lossy().to_string()]);

        // An absolute path that exists round-trips through canonicalization.
        let abs = cwd.join("Cargo.toml");
        let got = resolve_bases(&[abs.to_string_lossy().to_string()]).unwrap();
        assert_eq!(got, vec![cwd.join("Cargo.toml").to_string_lossy().to_string()]);
    }

    #[test]
    fn unresolvable_relative_base_is_an_error() {
        // A relative base that does not exist cannot match anything; returning
        // an empty set and exit 0 would be a lie. This must be reported.
        let e = resolve_bases(&["definitely-not-a-real-dir-xyzzy".to_string()]);
        assert!(e.is_err(), "unresolvable relative base must be an error");

        // An absolute, unresolvable path is deliberately left untouched: it
        // simply will not match, matching the existing absolute-path behavior.
        let got = resolve_bases(&["/no/such/dir/xyzzy".to_string()]).unwrap();
        assert_eq!(got, vec!["/no/such/dir/xyzzy".to_string()]);
    }

    #[test]
    fn plan_builds_allof() {
        let argv: Vec<String> = ["-name", "*.rs", "-type", "f"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let p = Plan::build(&[], &argv).unwrap();
        assert_eq!(
            p.index_expr,
            serde_json::json!(["allof", ["match", "*.rs", "basename"], ["type", "f"]])
        );
    }

    #[test]
    fn unsupported_tests_are_errors_not_silence() {
        for bad in ["-exec", "-delete", "-follow", "-prune"] {
            let argv: Vec<String> = [bad, "x"].iter().map(|s| s.to_string()).collect();
            let e = Plan::build(&[], &argv);
            assert!(e.is_err(), "{bad} must be rejected, not ignored");
        }
        let argv: Vec<String> = ["-frobnicate"].iter().map(|s| s.to_string()).collect();
        assert!(Plan::build(&[], &argv).is_err());
    }

    #[test]
    fn metadata_tests_evaluate() {
        let now = 1_000_000;
        let m = Meta {
            size: Some(0),
            mtime: Some(now - 3 * 86_400),
            mode: Some(0o100_0644),
            uid: Some(1000),
            gid: Some(1000),
            nlink: Some(1),
            ino: Some(42),
            mmin: Some(3 * 1440),
        };
        assert!(meta_matches(&[MetaTest::Size { sign: Sign::Eq, bytes: 0 }], &m, false, now));
        assert!(meta_matches(&[MetaTest::MtimeDays { sign: Sign::Eq, days: 3 }], &m, false, now));
        assert!(meta_matches(&[MetaTest::Perm { mode: 0o644, exact: true }], &m, false, now));
        assert!(meta_matches(&[MetaTest::User(1000)], &m, false, now));
        assert!(meta_matches(&[MetaTest::Inum(42)], &m, false, now));
        assert!(meta_matches(&[MetaTest::Empty], &m, false, now), "size 0 file is empty");
        assert!(!meta_matches(&[MetaTest::User(0)], &m, false, now));
    }

    #[test]
    fn empty_dir_is_decided_by_the_index() {
        let now = 0;
        let m = Meta { size: Some(4096), ..Default::default() };
        assert!(meta_matches(&[MetaTest::Empty], &m, true, now));
        assert!(!meta_matches(&[MetaTest::Empty], &m, false, now));
    }
}
