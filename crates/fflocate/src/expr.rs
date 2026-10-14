//! Watchman's expression-tree query language, ported to fflocate.
//!
//! This is a second, *additive* query language alongside the Everything-style
//! string language in [`crate::query`]. It exists because upstream Watchman's
//! integration tests (and its users) speak JSON expression trees:
//!
//! ```text
//! {"expression": ["allof", ["type", "f"], ["suffix", "rs"]]}
//! {"glob": ["**/*.rs"], "relative_root": "src"}
//! ```
//!
//! The term grammar, scopes, case-sensitivity rules, and — importantly — the
//! **error strings** are a direct port of Watchman's `watchman/query/*.cpp`
//! (`parse.cpp`, `base.cpp`, `name.cpp`, `match.cpp`, `suffix.cpp`,
//! `type.cpp`, `dirname.cpp`, `intcompare.cpp`, `empty.cpp`, `pcre.cpp`,
//! `since.cpp`, `TermRegistry.cpp`), so that the ported tests in
//! `tests/watchman_port.rs` and this module's unit tests assert the same
//! behaviour Watchman does.
//!
//! ## Matching model
//!
//! A candidate is described by [`MatchCtx`]: its basename, its path relative to
//! the root (no leading slash), its type, size and depth. `fflocate`'s index holds
//! both files and directories, so directories are candidates — matching
//! Watchman's observable `glob`/`name` behaviour (e.g. a `match`ed directory
//! shows up).
//!
//! ## Generators
//!
//! Watchman's top-level `glob`, `suffix` and `path` keys are *generators*: they
//! pre-select a candidate set, then the `expression` filters it. `fflocate` scans
//! linearly, so a generator is compiled into an equivalent expression that is
//! ANDed with the user expression (and, for correctness of the empty-generator
//! cases, a generator that selects nothing yields a `false` term).

use std::fmt;

use regex::bytes::Regex;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Wildmatch
// ---------------------------------------------------------------------------

/// Flags for [`wildmatch`], mirroring Watchman's `wildmatch.h`.
pub mod wm {
    /// Case-insensitive matching.
    pub const CASEFOLD: u32 = 1;
    /// `*`/`?`/classes do not match `/`; `**` is special.
    pub const PATHNAME: u32 = 2;
    /// Leading `.` must be matched explicitly by a wildcard.
    pub const PERIOD: u32 = 4;
    /// `\` loses its escape meaning.
    pub const NOESCAPE: u32 = 8;
}

fn fold(c: u8, flags: u32) -> u8 {
    if flags & wm::CASEFOLD != 0 {
        c.to_ascii_lowercase()
    } else {
        c
    }
}

/// Shell-style glob match, ported from Watchman's `wildmatch.c`.
///
/// This is a recursive matcher preserving the `**`, `WM_PATHNAME`,
/// `WM_PERIOD`, `WM_CASEFOLD` and `WM_NOESCAPE` semantics the Watchman query
/// terms rely on. It deliberately mirrors the reference algorithm's structure
/// (star backtracking, `**/` matching zero directories, character classes) so
/// behaviour matches on the cases Watchman's tests exercise.
pub fn wildmatch(pattern: &[u8], text: &[u8], flags: u32) -> bool {
    let period = flags & wm::PERIOD != 0;
    // The "leading period rule": with WM_PERIOD, a leading dot in the subject
    // must be matched by an explicit `.` — a wildcard may not consume it.
    let needs_leading_period = period && text.first() == Some(&b'.');
    wi_match(pattern, text, flags, needs_leading_period)
}

/// POSIX `[:name:]` class test. Returns `None` for an unknown class name
/// (malformed — the whole match fails, as in `dowild`). Mirrors the
/// `CC_EQ` chain of Watchman's `dowild`.
fn posix_class_hit(name: &[u8], ch: u8, flags: u32) -> Option<bool> {
    // `dowild` folds an uppercase subject before the class tests.
    let c = if flags & wm::CASEFOLD != 0 {
        ch.to_ascii_lowercase()
    } else {
        ch
    };
    let hit = match name {
        b"alnum" => c.is_ascii_alphanumeric(),
        b"alpha" => c.is_ascii_alphabetic(),
        b"blank" => c == b' ' || c == b'\t',
        b"cntrl" => c.is_ascii_control(),
        b"digit" => c.is_ascii_digit(),
        b"graph" => c.is_ascii_graphic(),
        b"lower" => c.is_ascii_lowercase(),
        b"print" => c.is_ascii_graphic() || c == b' ',
        b"punct" => c.is_ascii_graphic() && !c.is_ascii_alphanumeric(),
        b"space" => c.is_ascii_whitespace(),
        b"upper" => c.is_ascii_uppercase() || (flags & wm::CASEFOLD != 0 && c.is_ascii_lowercase()),
        b"xdigit" => c.is_ascii_hexdigit(),
        _ => return None,
    };
    Some(hit)
}

/// Character-class matcher: `pattern[p0]` points at `[`. Returns `(matched,
/// next_p)`. Mirrors the `case '['` branch of Watchman's `dowild`.
fn class_match(pattern: &[u8], p0: usize, ch: u8, flags: u32) -> (bool, usize) {
    // `dowild` folds an uppercase subject before the class tests.
    let ch = fold(ch, flags);
    let mut p = p0 + 1; // skip '['
    let negated = p < pattern.len() && (pattern[p] == b'!' || pattern[p] == b'^');
    if negated {
        p += 1;
    }
    let mut matched = false;
    let mut prev: Option<u8> = None;
    let mut first = true;
    loop {
        if p >= pattern.len() {
            return (false, pattern.len());
        }
        let c = pattern[p];
        // ']' closes the class unless it is the first member.
        if c == b']' && !first {
            break;
        }
        first = false;
        // POSIX class `[[:alpha:]]`.
        if c == b'[' && pattern.get(p + 1) == Some(&b':') {
            let mut q = p + 2;
            while q < pattern.len() && pattern[q] != b']' {
                q += 1;
            }
            if q >= pattern.len() {
                return (false, pattern.len());
            }
            if q > p + 2 && pattern[q - 1] == b':' {
                match posix_class_hit(&pattern[p + 2..q - 1], ch, flags) {
                    Some(hit) => {
                        if hit {
                            matched = true;
                        }
                        // A class never extends a range (`p_ch = 0`).
                        prev = None;
                        p = q + 1;
                        continue;
                    }
                    // Unknown class name: malformed (WM_ABORT_ALL).
                    None => return (false, pattern.len()),
                }
            }
            // No ":]" — fall through and treat the '[' as a literal member.
        }
        // Escape inside a class (unless WM_NOESCAPE).
        if c == b'\\' && flags & wm::NOESCAPE == 0 {
            p += 1;
            if p >= pattern.len() {
                return (false, pattern.len());
            }
            let lit = pattern[p];
            if eq_ch(ch, lit, flags) {
                matched = true;
            }
            prev = Some(lit);
            p += 1;
            continue;
        }
        // Range `a-z`.
        if c == b'-' && prev.is_some() && p + 1 < pattern.len() && pattern[p + 1] != b']' {
            p += 1;
            let mut hi = pattern[p];
            if hi == b'\\' && flags & wm::NOESCAPE == 0 {
                p += 1;
                if p >= pattern.len() {
                    return (false, pattern.len());
                }
                hi = pattern[p];
            }
            let lo = prev.unwrap();
            let mut hit = ch >= lo && ch <= hi;
            if !hit && flags & wm::CASEFOLD != 0 && ch.is_ascii_lowercase() {
                let up = ch.to_ascii_uppercase();
                hit = up >= lo && up <= hi;
            }
            if hit {
                matched = true;
            }
            prev = None;
            p += 1;
            continue;
        }
        if eq_ch(ch, c, flags) {
            matched = true;
        }
        prev = Some(c);
        p += 1;
    }
    let mut ok = matched ^ negated;
    // Neither a normal nor a negated class matches '/' under WM_PATHNAME.
    if flags & wm::PATHNAME != 0 && ch == b'/' {
        ok = false;
    }
    (ok, p + 1)
}

fn eq_ch(a: u8, b: u8, flags: u32) -> bool {
    if flags & wm::CASEFOLD != 0 {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn wi_match(pattern: &[u8], text: &[u8], flags: u32, needs_period: bool) -> bool {
    let pathname = flags & wm::PATHNAME != 0;
    let period = flags & wm::PERIOD != 0;
    let mut p = 0usize;
    let mut t = 0usize;

    while p < pattern.len() {
        let pc = pattern[p];
        let tc = if t < text.len() { text[t] } else { 0 };

        // With WM_PERIOD, a `/` immediately followed by `.` in the subject must
        // be matched by an explicit `.` in the pattern.
        if period
            && pathname
            && tc == b'/'
            && text.get(t + 1) == Some(&b'.')
            && pattern.get(p + 1) != Some(&b'.')
            && pc != b'\\'
        {
            // only applies to wildcard consumption, handled per-branch below
        }

        match pc {
            b'\\' if flags & wm::NOESCAPE == 0 => {
                p += 1;
                if p >= pattern.len() {
                    return false;
                }
                let lit = fold(pattern[p], flags);
                if t >= text.len() || fold(tc, flags) != lit {
                    return false;
                }
                p += 1;
                t += 1;
            }
            b'?' => {
                if t >= text.len() {
                    return false;
                }
                if pathname && tc == b'/' {
                    return false;
                }
                if period && needs_period && t == 0 && tc == b'.' {
                    // '?' cannot consume a leading '.' under the period rule.
                    return false;
                }
                p += 1;
                t += 1;
            }
            b'*' => {
                // Count the run of '*'.
                let mut star = 1;
                while pattern.get(p + star) == Some(&b'*') {
                    star += 1;
                }
                let is_double = star >= 2;
                // For a single '*' with PATHNAME, it cannot cross '/'.
                // For '**', it can cross '/'.
                let mut match_slash = !pathname;
                // Under WM_PATHNAME a `**` run is only meaningful at a
                // segment start (pattern start or right after '/') followed
                // by end-of-pattern or '/'. Anything else (e.g. `foo**bar`,
                // `**[!te]`, a trailing `bar**`) is malformed and fails the
                // whole match — this is wildmatch.c `dowild` returning
                // WM_ABORT_MALFORMED, checked before the trailing pattern.
                if is_double {
                    if pathname {
                        let rest = p + star;
                        let at_segment_start = p == 0 || pattern[p - 1] == b'/';
                        let rest_ok = rest >= pattern.len()
                            || pattern[rest] == b'/'
                            || (pattern[rest] == b'\\' && pattern.get(rest + 1) == Some(&b'/'));
                        if !(at_segment_start && rest_ok) {
                            return false;
                        }
                    }
                    match_slash = true;
                }
                p += star;

                // If the pattern ends here, decide by slashes/dots.
                if p >= pattern.len() {
                    if needs_period && t == 0 {
                        return false;
                    }
                    if !match_slash {
                        if text[t..].contains(&b'/') {
                            return false;
                        }
                        if period && !is_double {
                            // '/'+'.' must not be swallowed by a plain '*'
                            let mut i = t;
                            while i + 1 < text.len() {
                                if text[i] == b'/' && text[i + 1] == b'.' {
                                    return false;
                                }
                                i += 1;
                            }
                        }
                    }
                    return true;
                }

                // `**/` may match zero directories: try skipping the '/'
                // first. (For the `**\/` spelling there is no zero-directory
                // attempt; the backslash is consumed as a literal '/' by the
                // loop below, as in `dowild`.)
                if is_double
                    && pathname
                    && pattern[p] == b'/'
                    && wi_match(&pattern[p + 1..], &text[t..], flags, needs_period)
                {
                    return true;
                }

                // Plain single-'*' followed by '/': consume up to and including
                // the next slash.
                if !match_slash && pattern[p] == b'/' {
                    match text[t..].iter().position(|&c| c == b'/') {
                        Some(idx) => {
                            // The '/' is consumed by the outer loop: continue
                            // matching after it.
                            if wi_match(&pattern[p..], &text[t + idx..], flags, false) {
                                return true;
                            }
                        }
                        None => return false,
                    }
                }

                // General case: try every split point.
                let mut i = t;
                loop {
                    if needs_period && i == 0 && text.get(i) == Some(&b'.') {
                        return false;
                    }
                    if wi_match(&pattern[p..], &text[i..], flags, false) {
                        return true;
                    }
                    if i >= text.len() {
                        return false;
                    }
                    if !match_slash && text[i] == b'/' {
                        return false;
                    }
                    // Under PERIOD+PATHNAME no star run may swallow a '/.'
                    // boundary unless the pattern itself continues with '/.'
                    // here (`pattern[p + 1]`, the char after the next pattern
                    // char, is '.'). Mirrors the trailing check in `dowild`'s
                    // star loop.
                    if period
                        && pathname
                        && text[i] == b'/'
                        && text.get(i + 1) == Some(&b'.')
                        && pattern.get(p + 1) != Some(&b'.')
                    {
                        return false;
                    }
                    i += 1;
                }
            }
            b'[' => {
                if t >= text.len() {
                    return false;
                }
                let (ok, np) = class_match(pattern, p, tc, flags);
                if !ok {
                    return false;
                }
                p = np;
                t += 1;
            }
            _ => {
                // Literal. Collapse consecutive slashes in the pattern.
                let mut pp = p;
                if pc == b'/' {
                    while pattern.get(pp + 1) == Some(&b'/') {
                        pp += 1;
                    }
                }
                if t >= text.len() || fold(tc, flags) != fold(pc, flags) {
                    return false;
                }
                // Under PERIOD+PATHNAME, a pattern '/' followed by a non-'.'
                // cannot match a subject '/' followed by '.'.
                if period
                    && pathname
                    && tc == b'/'
                    && text.get(t + 1) == Some(&b'.')
                    && pattern.get(pp + 1) != Some(&b'.')
                {
                    return false;
                }
                p = pp + 1;
                t += 1;
            }
        }
    }
    t >= text.len()
}

/// Convenience: does `pattern` match `text` under the given flags?
#[allow(dead_code)]
pub fn wm_match(pattern: &str, text: &str, flags: u32) -> bool {
    wildmatch(pattern.as_bytes(), text.as_bytes(), flags)
}

// ---------------------------------------------------------------------------
// Expression model
// ---------------------------------------------------------------------------

/// How `name`/`match`/`pcre` match against a path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Basename,
    Wholename,
}

/// One of Watchman's integer comparison operators.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

impl Cmp {
    fn eval(self, a: i64, b: i64) -> bool {
        match self {
            Cmp::Eq => a == b,
            Cmp::Ne => a != b,
            Cmp::Gt => a > b,
            Cmp::Ge => a >= b,
            Cmp::Lt => a < b,
            Cmp::Le => a <= b,
        }
    }
    fn parse(op: &str) -> Option<Cmp> {
        Some(match op {
            "eq" => Cmp::Eq,
            "ne" => Cmp::Ne,
            "gt" => Cmp::Gt,
            "ge" => Cmp::Ge,
            "lt" => Cmp::Lt,
            "le" => Cmp::Le,
            _ => return None,
        })
    }
}

/// The type letter accepted by the `type` term.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeKind {
    Block,
    Char,
    Dir,
    File,
    Fifo,
    Symlink,
    Socket,
    Door,
}

impl TypeKind {
    fn parse(c: char) -> Option<TypeKind> {
        Some(match c {
            'b' => TypeKind::Block,
            'c' => TypeKind::Char,
            'd' => TypeKind::Dir,
            'f' => TypeKind::File,
            'p' => TypeKind::Fifo,
            'l' => TypeKind::Symlink,
            's' => TypeKind::Socket,
            'D' => TypeKind::Door,
            _ => return None,
        })
    }
}

/// Which stat/clock position the `since` term compares against. Ported from
/// Watchman's `enum since_what` in `watchman/query/since.cpp`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SinceField {
    Oclock,
    Cclock,
    Mtime,
    Ctime,
}

impl SinceField {
    fn parse(name: &str) -> Option<SinceField> {
        Some(match name {
            "oclock" => SinceField::Oclock,
            "cclock" => SinceField::Cclock,
            "mtime" => SinceField::Mtime,
            "ctime" => SinceField::Ctime,
            _ => return None,
        })
    }
}

/// A clockspec as accepted by the `since` term, ported from Watchman's
/// `ClockSpec` (`watchman/Clock.h`). Only the two forms the term admits are
/// represented: a coarse unix timestamp, or a clock position. An old-style
/// `c:pid:ticks` clock is a fresh instance (Watchman zeroes its start/root so
/// it always compares as fresh); a new-style `c:start:pid:root:ticks` clock
/// carries real ticks.
#[derive(Clone, Debug, PartialEq)]
pub enum ClockSpec {
    Timestamp(i64),
    Clock { ticks: i64, is_fresh_instance: bool },
}

fn parse_clock_string(s: &str) -> Option<ClockSpec> {
    let rest = s.strip_prefix("c:")?;
    let parts: Vec<&str> = rest.split(':').collect();
    match parts.as_slice() {
        [pid, ticks] => {
            let _ = pid.parse::<u64>().ok()?;
            let ticks = ticks.parse::<i64>().ok()?;
            Some(ClockSpec::Clock {
                ticks,
                is_fresh_instance: true,
            })
        }
        [start, pid, root, ticks] => {
            let _ = start.parse::<u64>().ok()?;
            let _ = pid.parse::<u64>().ok()?;
            let _ = root.parse::<u64>().ok()?;
            let ticks = ticks.parse::<i64>().ok()?;
            Some(ClockSpec::Clock {
                ticks,
                is_fresh_instance: false,
            })
        }
        _ => None,
    }
}

enum ClockSpecOutcome {
    Spec(ClockSpec),
    NamedCursor,
}

fn parse_clock_value(v: &Value) -> Result<ClockSpecOutcome, ()> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .map(ClockSpec::Timestamp)
            .map(ClockSpecOutcome::Spec)
            .ok_or(()),
        Value::String(s) => {
            if s.starts_with("n:") {
                return Ok(ClockSpecOutcome::NamedCursor);
            }
            parse_clock_string(s).map(ClockSpecOutcome::Spec).ok_or(())
        }
        Value::Object(o) => match o.get("clock") {
            None => Ok(ClockSpecOutcome::Spec(ClockSpec::Clock {
                ticks: 0,
                is_fresh_instance: true,
            })),
            Some(Value::String(s)) => parse_clock_string(s).map(ClockSpecOutcome::Spec).ok_or(()),
            Some(_) => Err(()),
        },
        _ => Err(()),
    }
}

/// A parsed Watchman expression tree.
#[derive(Clone, Debug)]
pub enum Expr {
    /// `["allof", ...]`
    AllOf(Vec<Expr>),
    /// `["anyof", ...]`
    AnyOf(Vec<Expr>),
    /// `["not", e]`
    Not(Box<Expr>),
    /// `true`
    True,
    /// `false`
    False,
    /// `["name", pattern(s), scope?]` / `["iname", ...]`
    Name {
        pats: Vec<Vec<u8>>,
        scope: Scope,
        case_sensitive: bool,
    },
    /// `["match", pattern, scope?, {noescape,includedotfiles}?]` / `["imatch", ...]`
    Match {
        pattern: Vec<u8>,
        scope: Scope,
        case_sensitive: bool,
        noescape: bool,
        include_dotfiles: bool,
    },
    /// `["suffix", suffix(es)]` — always case-insensitive, basename.
    Suffix { suffixes: Vec<Vec<u8>> },
    /// `["type", "x"]`
    Type(TypeKind),
    /// `["size", op, n]`
    Size { op: Cmp, n: i64 },
    /// `["dirname", name, ["depth", op, n]?]` / `["idirname", ...]`
    Dirname {
        name: Vec<u8>,
        depth: Option<(Cmp, i64)>,
        case_sensitive: bool,
    },
    /// `["pcre", pattern, scope?]` / `["ipcre", ...]`
    Pcre { re: Box<Regex>, scope: Scope },
    /// `["since", clockspec, field?]`
    Since { spec: ClockSpec, field: SinceField },
    /// `["empty"]`
    Empty,
    /// `["exists"]`
    Exists,
    /// `["executable"]` — fflocate extension, no Watchman equivalent: any
    /// execute bit set (`find -perm /111`). Parsed as its own term (not
    /// `["type", "x"]`) so the ported `type` grammar and its error strings
    /// stay byte-identical to Watchman's.
    Executable,
    /// The top-level `glob` generator: `glob: [patterns]`.
    GlobSet {
        patterns: Vec<Vec<u8>>,
        case_sensitive: bool,
        include_dotfiles: bool,
    },
    /// The top-level `path` generator.
    PathSet { paths: Vec<QueryPath> },
}

/// One entry of the `path` generator.
#[derive(Clone, Debug)]
pub struct QueryPath {
    pub name: Vec<u8>,
    /// `-1` means unlimited; `0` means just the named node.
    pub depth: i64,
}

/// A candidate presented to [`Expr::eval`].
pub struct MatchCtx<'a> {
    /// The entry's own name.
    pub name: &'a [u8],
    /// Path relative to the query root, no leading slash (e.g. `src/main.rs`).
    pub rel_path: &'a [u8],
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Any execute bit set (`find -perm /111`), from the `IS_EXEC` flag.
    pub is_exec: bool,
    /// Size in bytes; `None` when unknown (dirs).
    pub size: Option<u64>,
    /// Mount/change clock positions (seconds) and stat timestamps. The v1
    /// index carries none of these; `None` means "unknown", which the
    /// `size`/`empty`/`since` terms treat as non-matching.
    pub otime: Option<i64>,
    pub ctime: Option<i64>,
    pub mtime: Option<i64>,
    /// Filesystem case sensitivity default for terms without an explicit flag.
    pub fs_case_insensitive: bool,
}

impl<'a> MatchCtx<'a> {
    fn scope_str(&self, scope: Scope) -> &'a [u8] {
        match scope {
            Scope::Basename => self.name,
            Scope::Wholename => self.rel_path,
        }
    }

    /// Number of path separators strictly inside the dirname, i.e. how deep the
    /// file is below `dirname`. Mirrors `DirNameExpr::evaluate`.
    fn dirname_depth(&self, dirname_len: usize) -> i64 {
        let s = self.rel_path;
        if s.len() <= dirname_len {
            return 0;
        }
        // Skip the separator right after dirname, then count the rest.
        let mut depth = 0i64;
        for &c in &s[dirname_len + 1..] {
            if c == b'/' || c == b'\\' {
                depth += 1;
            }
        }
        depth
    }
}

impl Expr {
    /// Evaluate against a candidate.
    pub fn eval(&self, ctx: &MatchCtx) -> bool {
        match self {
            Expr::AllOf(list) => list.iter().all(|e| e.eval(ctx)),
            Expr::AnyOf(list) => list.iter().any(|e| e.eval(ctx)),
            Expr::Not(e) => !e.eval(ctx),
            Expr::True => true,
            Expr::False => false,
            Expr::Name {
                pats,
                scope,
                case_sensitive,
            } => {
                let hay = ctx.scope_str(*scope);
                let fold = !*case_sensitive;
                pats.iter().any(|p| bytes_eq(hay, p, fold))
            }
            Expr::Match {
                pattern,
                scope,
                case_sensitive,
                noescape,
                include_dotfiles,
            } => {
                let hay = ctx.scope_str(*scope);
                let mut flags = 0u32;
                if scope == &Scope::Wholename {
                    flags |= wm::PATHNAME;
                }
                if !*include_dotfiles {
                    flags |= wm::PERIOD;
                }
                if !*case_sensitive {
                    flags |= wm::CASEFOLD;
                }
                if *noescape {
                    flags |= wm::NOESCAPE;
                }
                wildmatch(pattern, hay, flags)
            }
            Expr::Suffix { suffixes } => suffixes.iter().any(|s| {
                ctx.name.len() > s.len()
                    && ctx.name[ctx.name.len() - s.len()..].eq_ignore_ascii_case(s)
            }),
            Expr::Type(t) => match t {
                TypeKind::File => !ctx.is_dir && !ctx.is_symlink,
                TypeKind::Dir => ctx.is_dir,
                TypeKind::Symlink => ctx.is_symlink,
                // Block/Char/Fifo/Socket/Door are not distinguishable from the
                // v1 index; they never match (Watchman's `dtype == Unknown`
                // falls through to stat, which fflocate does not carry).
                _ => false,
            },
            // fflocate extension (no Watchman equivalent): any execute bit set
            // (`find -perm /111`), from the `IS_EXEC` flag — never a stat.
            Expr::Executable => ctx.is_exec,
            Expr::Size { op, n } => match ctx.size {
                Some(sz) => op.eval(sz as i64, *n),
                None => false,
            },
            Expr::Dirname {
                name,
                depth,
                case_sensitive,
            } => eval_dirname(ctx, name, *depth, *case_sensitive),
            Expr::Pcre { re, scope } => re.is_match(ctx.scope_str(*scope)),
            Expr::Since { spec, field } => eval_since(ctx, spec, *field),
            Expr::Empty => matches!(ctx.size, Some(0)),
            Expr::Exists => true,
            Expr::GlobSet {
                patterns,
                case_sensitive,
                include_dotfiles,
            } => glob_set_matches(patterns, ctx, *case_sensitive, *include_dotfiles),
            Expr::PathSet { paths } => paths.iter().any(|p| path_matches(p, ctx)),
        }
    }
}

/// Evaluate the `since` term, mirroring `SinceExpr::evaluate`: time
/// comparisons use `>=` (inclusive, so coarse-timestamp clients don't miss
/// boundary changes), clock-tick comparisons use strict `>`. A fresh-instance
/// clock matches everything that exists.
fn eval_since(ctx: &MatchCtx, spec: &ClockSpec, field: SinceField) -> bool {
    match field {
        SinceField::Oclock | SinceField::Cclock => {
            let pos = if field == SinceField::Oclock {
                ctx.otime
            } else {
                ctx.ctime
            };
            match spec {
                ClockSpec::Timestamp(ts) => matches!(pos, Some(t) if t >= *ts),
                ClockSpec::Clock {
                    ticks,
                    is_fresh_instance,
                } => {
                    if *is_fresh_instance {
                        // Every candidate the engine visits exists.
                        return true;
                    }
                    matches!(pos, Some(t) if t > *ticks)
                }
            }
        }
        SinceField::Mtime | SinceField::Ctime => {
            // Non-timestamp specs are rejected at parse time.
            let ClockSpec::Timestamp(ts) = spec else {
                return false;
            };
            let val = if field == SinceField::Mtime {
                ctx.mtime
            } else {
                ctx.ctime
            };
            matches!(val, Some(t) if t >= *ts)
        }
    }
}

fn bytes_eq(a: &[u8], b: &[u8], fold: bool) -> bool {
    if a.len() != b.len() {
        return false;
    }
    if fold {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn eval_dirname(
    ctx: &MatchCtx,
    dirname: &[u8],
    depth: Option<(Cmp, i64)>,
    case_sensitive: bool,
) -> bool {
    let s = ctx.rel_path;
    if s.len() <= dirname.len() {
        return false;
    }
    if !dirname.is_empty() && s[dirname.len()] != b'/' {
        return false;
    }
    let starts = if case_sensitive {
        s.starts_with(dirname)
    } else {
        s.len() >= dirname.len() && s[..dirname.len()].eq_ignore_ascii_case(dirname)
    };
    if !starts {
        return false;
    }
    let actual = ctx.dirname_depth(dirname.len());
    match depth {
        Some((op, n)) => op.eval(actual, n),
        None => true,
    }
}

/// `glob:` generator: match the whole relative path with `**` semantics.
fn glob_set_matches(
    patterns: &[Vec<u8>],
    ctx: &MatchCtx,
    case_sensitive: bool,
    include_dotfiles: bool,
) -> bool {
    let mut flags = wm::PATHNAME;
    if !include_dotfiles {
        flags |= wm::PERIOD;
    }
    if !case_sensitive {
        flags |= wm::CASEFOLD;
    }
    patterns.iter().any(|p| wildmatch(p, ctx.rel_path, flags))
}

/// `path:` generator: the named path itself and (optionally, per depth) its
/// subtree. Match is on path components, case-sensitively (Watchman notes
/// case-insensitive matching is *not* implemented for the path generator).
fn path_matches(p: &QueryPath, ctx: &MatchCtx) -> bool {
    let s = ctx.rel_path;
    if s == p.name.as_slice() {
        return true;
    }
    if s.len() > p.name.len() && s.starts_with(&p.name) && s[p.name.len()] == b'/' {
        if p.depth < 0 {
            return true;
        }
        // depth counts components below the named path.
        let below = &s[p.name.len() + 1..];
        let comps = below.split(|&c| c == b'/').count() as i64;
        return comps <= p.depth;
    }
    false
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a full Watchman query object (`{"expression":..., "glob":..., ...}`)
/// into a single [`Expr`] (generators folded into the expression).
pub fn parse_query(query: &Value) -> Result<Expr, String> {
    let obj = query
        .as_object()
        .ok_or("expected a JSON object for a query")?;

    let case_sensitive = match obj.get("case_sensitive") {
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("case_sensitive must be a boolean".into()),
        // No filesystem probe in the query path: default to case-sensitive,
        // which is the Linux norm and matches the ported tests' expectation when
        // `case_sensitive` is not given (the tests branch on the host FS).
        None => true,
    };

    // Case sensitivity is threaded into terms via a Query-like context; we pass
    // it down explicitly.
    let mut exprs: Vec<Expr> = Vec::new();

    // --- Generators -------------------------------------------------------
    if let Some(g) = obj.get("glob") {
        let arr = g.as_array().ok_or("'glob' must be an array")?;
        let include_dotfiles = match obj.get("glob_includedotfiles") {
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err("glob_includedotfiles must be a boolean".into()),
            None => false,
        };
        if !include_dotfiles
            && obj
                .get("glob_noescape")
                .map(|v| !v.is_boolean())
                .unwrap_or(false)
        {
            return Err("glob_noescape must be a boolean".into());
        }
        let mut patterns = Vec::new();
        for ele in arr {
            let s = ele.as_str().ok_or("expected json string object")?;
            if s.starts_with('/') {
                return Err(format!(
                    "glob `{s}` is an absolute path.  All globs must be relative paths"
                ));
            }
            patterns.push(s.as_bytes().to_vec());
        }
        // An empty glob generator selects nothing.
        if patterns.is_empty() {
            exprs.push(Expr::False);
        } else {
            exprs.push(Expr::GlobSet {
                patterns,
                case_sensitive,
                include_dotfiles,
            });
        }
    }

    if let Some(s) = obj.get("suffix") {
        let suffixes = parse_suffix_list(s)?;
        if obj.get("glob").is_some() {
            return Err("'suffix' cannot be used together with the 'glob' generator".into());
        }
        if suffixes.is_empty() {
            exprs.push(Expr::False);
        } else {
            exprs.push(Expr::Suffix { suffixes });
        }
    }

    if let Some(p) = obj.get("path") {
        let paths = parse_paths(p)?;
        if paths.is_empty() {
            exprs.push(Expr::False);
        } else {
            exprs.push(Expr::PathSet { paths });
        }
    }

    // --- The expression itself -------------------------------------------
    if let Some(exp) = obj.get("expression") {
        exprs.push(parse_expr(exp, case_sensitive)?);
    }

    match exprs.len() {
        0 => Ok(Expr::True),
        1 => Ok(exprs.pop().unwrap()),
        _ => Ok(Expr::AllOf(exprs)),
    }
}

/// Parse just an expression value (as returned by `parse_expr`).
pub fn parse_expression(exp: &Value, case_sensitive: bool) -> Result<Expr, String> {
    parse_expr(exp, case_sensitive)
}

fn parse_paths(v: &Value) -> Result<Vec<QueryPath>, String> {
    let arr = v.as_array().ok_or("'path' must be an array")?;
    let mut out = Vec::new();
    for ele in arr {
        if let Some(s) = ele.as_str() {
            out.push(QueryPath {
                name: normalize_separators(s.as_bytes()),
                depth: -1,
            });
        } else if let Some(o) = ele.as_object() {
            let name = o
                .get("path")
                .and_then(|p| p.as_str())
                .ok_or("expected object with 'path' and 'depth' properties")?;
            let depth = o
                .get("depth")
                .and_then(|d| d.as_i64())
                .ok_or("path.depth must be an integer")?;
            out.push(QueryPath {
                name: normalize_separators(name.as_bytes()),
                depth,
            });
        } else {
            return Err("expected object with 'path' and 'depth' properties".into());
        }
    }
    Ok(out)
}

fn parse_suffix_list(v: &Value) -> Result<Vec<Vec<u8>>, String> {
    let mk = |s: &str| s.as_bytes().to_ascii_lowercase();
    if let Some(s) = v.as_str() {
        Ok(vec![mk(s)])
    } else if let Some(arr) = v.as_array() {
        let mut out = Vec::new();
        for ele in arr {
            match ele.as_str() {
                Some(s) => out.push(mk(s)),
                None => return Err("'suffix' must be a string or an array of strings".into()),
            }
        }
        Ok(out)
    } else {
        Err("'suffix' must be a string or an array of strings".into())
    }
}

fn normalize_separators(b: &[u8]) -> Vec<u8> {
    b.iter()
        .map(|&c| if c == b'\\' { b'/' } else { c })
        .collect()
}

/// The core term dispatcher, mirroring `parseQueryExpr` + `TermRegistry`.
fn parse_expr(exp: &Value, case_sensitive: bool) -> Result<Expr, String> {
    let name: String = if let Some(s) = exp.as_str() {
        s.to_string()
    } else if let Some(arr) = exp.as_array() {
        let first = arr
            .first()
            .ok_or("expected array or string for an expression")?;
        first
            .as_str()
            .ok_or("first element of an expression must be a string")?
            .to_string()
    } else {
        return Err("expected array or string for an expression".into());
    };

    match name.as_str() {
        "allof" => parse_list(exp, case_sensitive, true),
        "anyof" => parse_list(exp, case_sensitive, false),
        "not" => {
            let arr = exp.as_array().ok_or("must use [\"not\", expr]")?;
            if arr.len() != 2 {
                return Err("must use [\"not\", expr]".into());
            }
            Ok(Expr::Not(Box::new(parse_expr(&arr[1], case_sensitive)?)))
        }
        "true" => Ok(Expr::True),
        "false" => Ok(Expr::False),
        "exists" => Ok(Expr::Exists),
        "empty" => Ok(Expr::Empty),
        "name" => parse_name(exp, case_sensitive),
        "iname" => parse_name(exp, false),
        "match" => parse_match(exp, case_sensitive),
        "imatch" => parse_match(exp, false),
        "suffix" => parse_suffix_term(exp),
        "type" => parse_type(exp),
        "size" => parse_size(exp),
        "dirname" => parse_dirname(exp, case_sensitive),
        "idirname" => parse_dirname(exp, false),
        "pcre" => parse_pcre(exp, case_sensitive),
        "ipcre" => parse_pcre(exp, false),
        "since" => parse_since(exp),
        other => Err(format!("unknown expression term '{}'", other)),
    }
}

fn parse_list(exp: &Value, case_sensitive: bool, allof: bool) -> Result<Expr, String> {
    let arr = exp.as_array().ok_or(if allof {
        "must use [\"allof\", expr...]"
    } else {
        "must use [\"anyof\", expr...]"
    })?;
    if arr.len() < 2 {
        return Err(if allof {
            "must use [\"allof\", expr...]"
        } else {
            "must use [\"anyof\", expr...]"
        }
        .into());
    }
    let mut list = Vec::new();
    for e in &arr[1..] {
        list.push(parse_expr(e, case_sensitive)?);
    }
    Ok(if allof {
        Expr::AllOf(list)
    } else {
        Expr::AnyOf(list)
    })
}

/// `["name", pat | [pats], scope?]`. `which` selects the error-message wording.
fn parse_name(exp: &Value, case_sensitive: bool) -> Result<Expr, String> {
    let which = if case_sensitive { "name" } else { "iname" };
    let arr = exp
        .as_array()
        .ok_or_else(|| format!("Expected array for '{which}' term"))?;
    if arr.len() > 3 {
        return Err(format!("Invalid number of arguments for '{which}' term"));
    }
    let mut scope = Scope::Basename;
    if arr.len() == 3 {
        let s = arr[2]
            .as_str()
            .ok_or_else(|| format!("Argument 3 to '{which}' must be a string"))?;
        scope =
            parse_scope(s).ok_or_else(|| format!("Invalid scope '{s}' for {which} expression"))?;
    }
    let mut pats = Vec::new();
    match &arr[1] {
        Value::Array(items) => {
            for it in items {
                let s = it.as_str().ok_or_else(|| {
                    format!("Argument 2 to '{which}' must be either a string or an array of string")
                })?;
                pats.push(normalize_separators(s.as_bytes()));
            }
        }
        Value::String(s) => pats.push(normalize_separators(s.as_bytes())),
        _ => {
            return Err(format!(
                "Argument 2 to '{which}' must be either a string or an array of string"
            ))
        }
    }
    Ok(Expr::Name {
        pats,
        scope,
        case_sensitive,
    })
}

fn parse_match(exp: &Value, case_sensitive: bool) -> Result<Expr, String> {
    let which = if case_sensitive { "match" } else { "imatch" };
    let arr = exp
        .as_array()
        .ok_or_else(|| format!("First parameter to \"{which}\" term must be a pattern string"))?;
    let pattern = arr
        .get(1)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("First parameter to \"{which}\" term must be a pattern string"))?;
    let mut scope = Scope::Basename;
    if arr.len() > 2 {
        let s = arr[2].as_str().ok_or_else(|| {
            format!("Second parameter to \"{which}\" term must be an optional scope string")
        })?;
        scope =
            parse_scope(s).ok_or_else(|| format!("Invalid scope '{s}' for {which} expression"))?;
    }
    let (mut noescape, mut include_dotfiles) = (false, false);
    if arr.len() > 3 {
        let o = arr[3].as_object().ok_or_else(|| {
            format!("Third parameter to \"{which}\" term must be an optional object")
        })?;
        if let Some(v) = o.get("noescape") {
            noescape = v
                .as_bool()
                .ok_or_else(|| format!("noescape option for \"{which}\" term must be a boolean"))?;
        }
        if let Some(v) = o.get("includedotfiles") {
            include_dotfiles = v.as_bool().ok_or_else(|| {
                format!("includedotfiles option for \"{which}\" term must be a boolean")
            })?;
        }
    }
    if arr.len() > 4 {
        return Err(format!(
            "too many parameters passed to \"{which}\" expression"
        ));
    }
    Ok(Expr::Match {
        pattern: pattern.as_bytes().to_vec(),
        scope,
        case_sensitive,
        noescape,
        include_dotfiles,
    })
}

fn parse_scope(s: &str) -> Option<Scope> {
    match s {
        "basename" => Some(Scope::Basename),
        "wholename" => Some(Scope::Wholename),
        _ => None,
    }
}

fn parse_suffix_term(exp: &Value) -> Result<Expr, String> {
    let arr = exp.as_array().ok_or("Expected array for 'suffix' term")?;
    if arr.len() > 2 {
        return Err("Invalid number of arguments for 'suffix' term".into());
    }
    let suffixes = parse_suffix_list(
        arr.get(1)
            .ok_or("Invalid number of arguments for 'suffix' term")?,
    )
    .map_err(|_| {
        "Argument 2 to 'suffix' must be either a string or an array of string".to_string()
    })?;
    Ok(Expr::Suffix { suffixes })
}

fn parse_type(exp: &Value) -> Result<Expr, String> {
    let arr = exp
        .as_array()
        .ok_or("\"type\" term requires a type string parameter")?;
    let s = arr
        .get(1)
        .and_then(|v| v.as_str())
        .ok_or("First parameter to \"type\" term must be a type string")?;
    let mut chars = s.chars();
    let kind = match (chars.next(), chars.next()) {
        (Some(c), None) => {
            TypeKind::parse(c).ok_or_else(|| format!("invalid type string '{s}'"))?
        }
        _ => return Err(format!("invalid type string '{s}'")),
    };
    Ok(Expr::Type(kind))
}

fn parse_size(exp: &Value) -> Result<Expr, String> {
    let arr = exp.as_array().ok_or("Expected array for 'size' term")?;
    let (op, n) = parse_int_compare(arr)?;
    Ok(Expr::Size { op, n })
}

fn parse_int_compare(arr: &[Value]) -> Result<(Cmp, i64), String> {
    if arr.len() != 3 {
        return Err("integer comparator must have 3 elements".into());
    }
    let opname = arr[1]
        .as_str()
        .ok_or("integer comparator op must be a string")?;
    let operand = arr[2]
        .as_i64()
        .ok_or("integer comparator operand must be an integer")?;
    let op = Cmp::parse(opname)
        .ok_or_else(|| format!("integer comparator opname `{opname}' is invalid"))?;
    Ok((op, operand))
}

fn parse_dirname(exp: &Value, case_sensitive: bool) -> Result<Expr, String> {
    let which = if case_sensitive {
        "dirname"
    } else {
        "idirname"
    };
    let arr = exp
        .as_array()
        .ok_or_else(|| format!("Expected array for '{which}' term"))?;
    if arr.len() < 2 || arr.len() > 3 {
        return Err(format!("Invalid number of arguments for '{which}' term"));
    }
    let name = arr[1]
        .as_str()
        .ok_or_else(|| format!("Argument 2 to '{which}' must be a string"))?;
    let depth = if arr.len() == 3 {
        let d = arr[2]
            .as_array()
            .ok_or_else(|| format!("Invalid number of arguments for '{which}' term"))?;
        let (op, n) = parse_int_compare(d)?;
        if d.first().and_then(|v| v.as_str()) != Some("depth") {
            return Err(format!(
                "Third parameter to '{which}' should be a relational depth term"
            ));
        }
        Some((op, n))
    } else {
        Some((Cmp::Ge, 0))
    };
    // Strip trailing slashes from dirname.
    let mut dn = name.as_bytes();
    while let Some((&last, rest)) = dn.split_last() {
        if last == b'/' {
            dn = rest;
        } else {
            break;
        }
    }
    Ok(Expr::Dirname {
        name: dn.to_vec(),
        depth,
        case_sensitive,
    })
}

fn parse_pcre(exp: &Value, case_sensitive: bool) -> Result<Expr, String> {
    let which = if case_sensitive { "pcre" } else { "ipcre" };
    let arr = exp
        .as_array()
        .ok_or_else(|| format!("First parameter to \"{which}\" term must be a pattern string"))?;
    let pattern = arr
        .get(1)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("First parameter to \"{which}\" term must be a pattern string"))?;
    let mut scope = Scope::Basename;
    if arr.len() > 2 {
        let s = arr[2].as_str().ok_or_else(|| {
            format!("Second parameter to \"{which}\" term must be an optional scope string")
        })?;
        scope =
            parse_scope(s).ok_or_else(|| format!("Invalid scope '{s}' for {which} expression"))?;
    }
    let re = if case_sensitive {
        Regex::new(pattern)
    } else {
        Regex::new(&format!("(?i){pattern}"))
    }
    .map_err(|e| format!("invalid {which}: {e} at offset in {pattern}"))?;
    Ok(Expr::Pcre {
        re: Box::new(re),
        scope,
    })
}

fn parse_since(exp: &Value) -> Result<Expr, String> {
    let arr = exp.as_array().ok_or("\"since\" term must be an array")?;
    if arr.len() < 2 || arr.len() > 3 {
        return Err("\"since\" term has invalid number of parameters".into());
    }
    let spec = match parse_clock_value(&arr[1]) {
        Ok(ClockSpecOutcome::Spec(spec)) => spec,
        Ok(ClockSpecOutcome::NamedCursor) => {
            return Err("named cursors are not allowed in \"since\" terms".into())
        }
        Err(()) => return Err("invalid clockspec for \"since\" term".into()),
    };
    let mut field = SinceField::Oclock;
    if arr.len() == 3 {
        let name = arr[2]
            .as_str()
            .ok_or("field name for \"since\" term must be a string")?;
        field = SinceField::parse(name)
            .ok_or_else(|| format!("invalid field name \"{name}\" for \"since\" term"))?;
        if matches!(field, SinceField::Mtime | SinceField::Ctime)
            && !matches!(spec, ClockSpec::Timestamp(_))
        {
            return Err(format!(
                "field \"{name}\" requires a timestamp value for comparison in \"since\" term"
            ));
        }
    }
    Ok(Expr::Since { spec, field })
}

/// What an expression tells the suffix-array optimisation about itself.
/// Ported from Watchman's `SimpleSuffixType` (`watchman/query/QueryExpr.h`):
/// `IsSimpleSuffix` means the query is exactly `type f` intersected with a
/// `suffix` set and can be answered from the suffix index directly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SimpleSuffixType {
    Excluded,
    Suffix,
    IsSimpleSuffix,
    Type,
}

impl Expr {
    /// Which suffix-optimisation class this expression falls into (port of
    /// `QueryExpr::evaluateSimpleSuffix`).
    pub fn simple_suffix_type(&self) -> SimpleSuffixType {
        match self {
            Expr::AllOf(list) if list.len() == 2 => {
                let a = list[0].simple_suffix_type();
                let b = list[1].simple_suffix_type();
                if (a == SimpleSuffixType::Type && b == SimpleSuffixType::Suffix)
                    || (a == SimpleSuffixType::Suffix && b == SimpleSuffixType::Type)
                {
                    return SimpleSuffixType::IsSimpleSuffix;
                }
                SimpleSuffixType::Excluded
            }
            Expr::Suffix { .. } => SimpleSuffixType::Suffix,
            Expr::Type(TypeKind::File) => SimpleSuffixType::Type,
            _ => SimpleSuffixType::Excluded,
        }
    }

    /// The glob patterns a suffix-array scan would use (port of
    /// `QueryExpr::getSuffixQueryGlobPatterns`): `**/*.<suffix>` per suffix.
    /// Non-suffix expressions contribute nothing.
    pub fn suffix_glob_patterns(&self) -> Vec<Vec<u8>> {
        match self {
            Expr::Suffix { suffixes } => suffixes
                .iter()
                .map(|s| {
                    let mut p = b"**/*.".to_vec();
                    p.extend_from_slice(s);
                    p
                })
                .collect(),
            Expr::AllOf(list) => list
                .iter()
                .find(|e| e.simple_suffix_type() == SimpleSuffixType::Suffix)
                .map(|e| e.suffix_glob_patterns())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{EntryFlags, Index, ROOT_ID};
    use serde_json::json;

    /// Build a fixture index with a root dir `proj` and the given relative
    /// entries. `dir` flags are inferred from a trailing marker in the name?
    /// No — callers pass explicit `(path, is_dir)`.
    struct Fixture {
        ix: Index,
    }

    impl Fixture {
        fn new(entries: &[(&str, bool)]) -> Fixture {
            let mut ix = Index::new();
            let root = ix.insert(
                ROOT_ID,
                b"proj",
                EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
            );
            for (path, is_dir) in entries {
                let mut parent = root;
                let comps: Vec<&str> = path.split('/').collect();
                for (i, comp) in comps.iter().enumerate() {
                    let last = i == comps.len() - 1;
                    let is_d = if last { *is_dir } else { true };
                    let flags = EntryFlags::EMPTY.with_dir(is_d);
                    match ix.lookup(parent, comp.as_bytes()) {
                        Some(id) => parent = id,
                        None => parent = ix.insert(parent, comp.as_bytes(), flags),
                    }
                }
            }
            Fixture { ix }
        }

        /// Evaluate a full query object, returning the matching relative paths.
        fn query(&self, q: &Value) -> Vec<String> {
            let expr = parse_query(q).expect("parse");
            self.run(&expr)
        }

        /// Evaluate a full query object with Watchman's `relative_root`
        /// applied: candidates outside `rel_root` are skipped and the
        /// expression sees paths relative to it (as `execute_expr` does).
        fn query_in(&self, q: &Value, rel_root: &str) -> Vec<String> {
            let expr = parse_query(q).expect("parse");
            self.run_in(&expr, rel_root)
        }

        fn run(&self, expr: &Expr) -> Vec<String> {
            self.run_with(expr, |_, _| {})
        }

        /// Like [`Fixture::run`] but the closure may adjust each candidate's
        /// context (e.g. synthesize sizes/timestamps the v1 index lacks).
        fn run_with(&self, expr: &Expr, fill: impl Fn(&[u8], &mut MatchCtx)) -> Vec<String> {
            let mut out = Vec::new();
            for (id, e) in self.ix.iter_live() {
                if e.flags().is_root_dir() {
                    continue;
                }
                let abs = self.ix.path_of(id, b"/").unwrap();
                let rel = &abs[1..]; // strip leading '/'
                let dir_name = self.ix.name(id);
                let mut ctx = MatchCtx {
                    name: dir_name,
                    rel_path: rel,
                    is_dir: e.flags().is_dir(),
                    is_symlink: e.flags().is_symlink(),
                    is_exec: e.flags().is_exec(),
                    size: None,
                    otime: None,
                    ctime: None,
                    mtime: None,
                    fs_case_insensitive: false,
                };
                fill(rel, &mut ctx);
                if expr.eval(&ctx) {
                    out.push(String::from_utf8_lossy(rel).into_owned());
                }
            }
            out.sort();
            out
        }

        /// Like [`Fixture::run`] but scoped to `rel_root` (see
        /// [`Fixture::query_in`]).
        fn run_in(&self, expr: &Expr, rel_root: &str) -> Vec<String> {
            let prefix: Vec<u8> = rel_root
                .bytes()
                .map(|c| if c == b'\\' { b'/' } else { c })
                .collect();
            let mut out = Vec::new();
            for (id, e) in self.ix.iter_live() {
                if e.flags().is_root_dir() {
                    continue;
                }
                let abs = self.ix.path_of(id, b"/").unwrap();
                let rel = &abs[1..]; // strip leading '/'
                let rel = if prefix.is_empty() {
                    rel
                } else if rel.len() > prefix.len()
                    && rel.starts_with(&prefix)
                    && rel[prefix.len()] == b'/'
                {
                    &rel[prefix.len() + 1..]
                } else {
                    continue;
                };
                let ctx = MatchCtx {
                    name: self.ix.name(id),
                    rel_path: rel,
                    is_dir: e.flags().is_dir(),
                    is_symlink: e.flags().is_symlink(),
                    is_exec: e.flags().is_exec(),
                    size: None,
                    otime: None,
                    ctime: None,
                    mtime: None,
                    fs_case_insensitive: false,
                };
                if expr.eval(&ctx) {
                    out.push(String::from_utf8_lossy(rel).into_owned());
                }
            }
            out.sort();
            out
        }

        fn expr(&self, e: Value) -> Vec<String> {
            let ex = parse_expr(&e, true).expect("parse expr");
            let mut v = self.run(&ex);
            v.sort();
            v
        }
    }

    fn sorted(v: &[&str]) -> Vec<String> {
        let mut x: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        x.sort();
        x
    }

    // ---- wildmatch ------------------------------------------------------

    #[test]
    fn wildmatch_basics() {
        // From WildmatchTest.json (flags: 2 = WM_PATHNAME).
        let p = wm::PATHNAME;
        assert!(wm_match("foo", "foo", p));
        assert!(!wm_match("foo", "bar", p));
        assert!(wm_match("*", "foo", p));
        assert!(wm_match("f*", "foo", p));
        assert!(!wm_match("*f", "foo", p));
        assert!(wm_match("*ob*a*r*", "foobar", p));
        assert!(!wm_match("foo*bar", "foo/baz/bar", p)); // '*' cannot cross '/'
                                                         // A `**` run outside a segment boundary is malformed and never
                                                         // matches under WM_PATHNAME (WildmatchTest.json: `foo**bar` is false).
        assert!(!wm_match("foo**bar", "foo/baz/bar", p));
        assert!(!wm_match("foo?bar", "foo/bar", p));
        assert!(wm_match("t[a-g]n", "ten", p));
        assert!(!wm_match("t[!a-g]n", "ten", p));
        assert!(wm_match("t[!a-g]n", "ton", p));
    }

    #[test]
    fn wildmatch_doublestar() {
        let p = wm::PATHNAME;
        // '**/foo' matches 'foo' itself and anything ending in /foo.
        assert!(wm_match("**/foo", "foo", p));
        assert!(wm_match("**/foo", "/foo", p));
        assert!(wm_match("**/foo", "bar/baz/foo", p));
        assert!(!wm_match("*/foo", "bar/baz/foo", p));
        assert!(wm_match("**/*.h", "foo/a.h", p));
        assert!(wm_match("**/*.h", "a/b/c.h", p));
        assert!(!wm_match("*/*.h", "a/b/c.h", p)); // exactly one dir
        assert!(wm_match("*/*.h", "a/b.h", p));
    }

    #[test]
    fn wildmatch_period() {
        // WM_PERIOD: a leading '.' must be matched explicitly.
        let p = wm::PATHNAME | wm::PERIOD;
        assert!(!wm_match("*.c", ".a.c", p));
        assert!(wm_match("*.c", "a.c", p));
        assert!(!wm_match("**/*.c", "foo/.bar.c", p));
        // without WM_PERIOD, dotfiles match.
        assert!(wm_match("*.c", ".a.c", wm::PATHNAME));
    }

    #[test]
    fn wildmatch_casefold() {
        let p = wm::PATHNAME | wm::CASEFOLD;
        assert!(wm_match("Hello*", "hello.c", p));
        assert!(wm_match("*.RS", "x.rs", p));
    }

    /// The whole `watchman/test/WildmatchTest.json` table: each row is
    /// `[should_match, flags, text, pattern]`. (Inlined rather than read from
    /// disk so the crate builds without the upstream checkout present.)
    const WILDMATCH_TABLE: &str = r#"[
[true, 2, "foo", "foo"],
[false, 2, "foo", "bar"],
[true, 2, "", ""],
[true, 2, "foo", "???"],
[false, 2, "foo", "??"],
[true, 2, "foo", "*"],
[true, 2, "foo", "f*"],
[false, 2, "foo", "*f"],
[true, 2, "foo", "*foo*"],
[true, 2, "foobar", "*ob*a*r*"],
[true, 2, "aaaaaaabababab", "*ab"],
[true, 2, "foo*", "foo\\*"],
[false, 2, "foobar", "foo\\*bar"],
[true, 2, "f\\oo", "f\\\\oo"],
[true, 2, "ball", "*[al]?"],
[false, 2, "ten", "[ten]"],
[true, 0, "ten", "**[!te]"],
[false, 0, "ten", "**[!ten]"],
[false, 2, "ten", "**[!te]"],
[false, 2, "ten", "**[!ten]"],
[true, 2, "ten", "t[a-g]n"],
[false, 2, "ten", "t[!a-g]n"],
[true, 2, "ton", "t[!a-g]n"],
[true, 2, "ton", "t[^a-g]n"],
[true, 2, "a]b", "a[]]b"],
[true, 2, "a-b", "a[]-]b"],
[true, 2, "a]b", "a[]-]b"],
[false, 2, "aab", "a[]-]b"],
[true, 2, "aab", "a[]a-]b"],
[true, 2, "]", "]"],
[false, 2, "foo/baz/bar", "foo*bar"],
[true, 0, "foo/baz/bar", "foo**bar"],
[false, 2, "foo/baz/bar", "foo**bar"],
[false, 2, "foo/bar", "foo?bar"],
[false, 2, "foo/bar", "foo[/]bar"],
[true, 0, "foo/bar", "f[^eiu][^eiu][^eiu][^eiu][^eiu]r"],
[false, 2, "foo/bar", "f[^eiu][^eiu][^eiu][^eiu][^eiu]r"],
[true, 0, "foo-bar", "f[^eiu][^eiu][^eiu][^eiu][^eiu]r"],
[true, 2, "foo-bar", "f[^eiu][^eiu][^eiu][^eiu][^eiu]r"],
[false, 0, "foo", "**/foo"],
[true, 2, "foo", "**/foo"],
[true, 2, "/foo", "**/foo"],
[true, 2, "bar/baz/foo", "**/foo"],
[false, 2, "bar/baz/foo", "*/foo"],
[false, 2, "foo/bar/baz", "**/bar*"],
[true, 2, "deep/foo/bar/baz", "**/bar/*"],
[false, 2, "deep/foo/bar/baz/", "**/bar/*"],
[true, 2, "deep/foo/bar/baz/", "**/bar/**"],
[false, 2, "deep/foo/bar", "**/bar/*"],
[true, 2, "deep/foo/bar/", "**/bar/**"],
[true, 0, "foo/bar/baz", "**/bar**"],
[false, 2, "foo/bar/baz", "**/bar**"],
[true, 2, "foo/bar/baz/x", "*/bar/**"],
[false, 2, "deep/foo/bar/baz/x", "*/bar/**"],
[true, 2, "deep/foo/bar/baz/x", "**/bar/*/*"],
[false, 2, "acrt", "a[c-c]st"],
[true, 2, "acrt", "a[c-c]rt"],
[false, 2, "]", "[!]-]"],
[true, 2, "a", "[!]-]"],
[false, 2, "", "\\"],
[false, 2, "\\", "\\"],
[false, 2, "/\\", "*/\\"],
[true, 2, "/\\", "*/\\\\"],
[true, 2, "foo", "foo"],
[true, 2, "@foo", "@foo"],
[false, 2, "foo", "@foo"],
[true, 2, "[ab]", "\\[ab]"],
[true, 2, "[ab]", "[[]ab]"],
[true, 2, "[ab]", "[[:]ab]"],
[false, 2, "[ab]", "[[::]ab]"],
[true, 2, "[ab]", "[[:digit]ab]"],
[true, 2, "[ab]", "[\\[:]ab]"],
[true, 2, "?a?b", "\\??\\?b"],
[true, 2, "abc", "\\a\\b\\c"],
[false, 2, "foo", ""],
[true, 2, "foo/bar/baz/to", "**/t[o]"],
[true, 2, "a1B", "[[:alpha:]][[:digit:]][[:upper:]]"],
[false, 2, "a", "[[:digit:][:upper:][:space:]]"],
[true, 2, "A", "[[:digit:][:upper:][:space:]]"],
[true, 2, "1", "[[:digit:][:upper:][:space:]]"],
[false, 2, "1", "[[:digit:][:upper:][:spaci:]]"],
[true, 2, " ", "[[:digit:][:upper:][:space:]]"],
[false, 2, ".", "[[:digit:][:upper:][:space:]]"],
[true, 2, ".", "[[:digit:][:punct:][:space:]]"],
[true, 2, "5", "[[:xdigit:]]"],
[true, 2, "f", "[[:xdigit:]]"],
[true, 2, "D", "[[:xdigit:]]"],
[true, 2, "_", "[[:alnum:][:alpha:][:blank:][:cntrl:][:digit:][:graph:][:lower:][:print:][:punct:][:space:][:upper:][:xdigit:]]"],
[true, 2, "\u007F", "[^[:alnum:][:alpha:][:blank:][:digit:][:graph:][:lower:][:print:][:punct:][:space:][:upper:][:xdigit:]]"],
[true, 2, ".", "[^[:alnum:][:alpha:][:blank:][:cntrl:][:digit:][:lower:][:space:][:upper:][:xdigit:]]"],
[true, 2, "5", "[a-c[:digit:]x-z]"],
[true, 2, "b", "[a-c[:digit:]x-z]"],
[true, 2, "y", "[a-c[:digit:]x-z]"],
[false, 2, "q", "[a-c[:digit:]x-z]"],
[true, 2, "]", "[\\\\-^]"],
[false, 2, "[", "[\\\\-^]"],
[true, 2, "-", "[\\-_]"],
[true, 2, "]", "[\\]]"],
[false, 2, "\\]", "[\\]]"],
[false, 2, "\\", "[\\]]"],
[false, 2, "ab", "a[]b"],
[false, 2, "a[]b", "a[]b"],
[false, 2, "ab[", "ab["],
[false, 2, "ab", "[!"],
[false, 2, "ab", "[-"],
[true, 2, "-", "[-]"],
[false, 2, "-", "[a-"],
[false, 2, "-", "[!a-"],
[true, 2, "-", "[--A]"],
[true, 2, "5", "[--A]"],
[true, 2, " ", "[ --]"],
[true, 2, "$", "[ --]"],
[true, 2, "-", "[ --]"],
[false, 2, "0", "[ --]"],
[true, 2, "-", "[---]"],
[true, 2, "-", "[------]"],
[false, 2, "j", "[a-e-n]"],
[true, 2, "-", "[a-e-n]"],
[true, 2, "a", "[!------]"],
[false, 2, "[", "[]-a]"],
[true, 2, "^", "[]-a]"],
[false, 2, "^", "[!]-a]"],
[true, 2, "[", "[!]-a]"],
[true, 2, "^", "[a^bc]"],
[true, 2, "-b]", "[a-]b]"],
[false, 2, "\\", "[\\]"],
[true, 2, "\\", "[\\\\]"],
[false, 2, "\\", "[!\\\\]"],
[true, 2, "G", "[A-\\\\]"],
[false, 2, "aaabbb", "b*a"],
[false, 2, "aabcaa", "*ba*"],
[true, 2, ",", "[,]"],
[true, 2, ",", "[\\\\,]"],
[true, 2, "\\", "[\\\\,]"],
[true, 2, "-", "[,-.]"],
[false, 2, "+", "[,-.]"],
[false, 2, "-.]", "[,-.]"],
[true, 2, "2", "[\\1-\\3]"],
[true, 2, "3", "[\\1-\\3]"],
[false, 2, "4", "[\\1-\\3]"],
[true, 2, "\\", "[[-\\]]"],
[true, 2, "[", "[[-\\]]"],
[true, 2, "]", "[[-\\]]"],
[false, 2, "-", "[[-\\]]"],
[true, 2, "-adobe-courier-bold-o-normal--12-120-75-75-m-70-iso8859-1", "-*-*-*-*-*-*-12-*-*-*-m-*-*-*"],
[false, 2, "-adobe-courier-bold-o-normal--12-120-75-75-X-70-iso8859-1", "-*-*-*-*-*-*-12-*-*-*-m-*-*-*"],
[false, 2, "-adobe-courier-bold-o-normal--12-120-75-75-/-70-iso8859-1", "-*-*-*-*-*-*-12-*-*-*-m-*-*-*"],
[true, 2, "/adobe/courier/bold/o/normal//12/120/75/75/m/70/iso8859/1", "/*/*/*/*/*/*/12/*/*/*/m/*/*/*"],
[false, 2, "/adobe/courier/bold/o/normal//12/120/75/75/X/70/iso8859/1", "/*/*/*/*/*/*/12/*/*/*/m/*/*/*"],
[true, 2, "abcd/abcdefg/abcdefghijk/abcdefghijklmnop.txt", "**/*a*b*g*n*t"],
[false, 2, "abcd/abcdefg/abcdefghijk/abcdefghijklmnop.txtz", "**/*a*b*g*n*t"],
[true, 0, ".bar", "*"],
[true, 2, ".bar", "*"],
[false, 4, ".bar", "*"],
[false, 6, ".bar", "*"],
[true, 0, ".bar", ".*"],
[true, 2, ".bar", ".*"],
[true, 4, ".bar", ".*"],
[true, 6, ".bar", ".*"],
[true, 0, ".bar.txt", "*.txt"],
[true, 2, ".bar.txt", "*.txt"],
[false, 4, ".bar.txt", "*.txt"],
[false, 6, ".bar.txt", "*.txt"],
[false, 0, ".bar", "**/*"],
[true, 2, ".bar", "**/*"],
[false, 4, ".bar", "**/*"],
[false, 6, ".bar", "**/*"],
[false, 0, ".bar", "**/.*"],
[true, 2, ".bar", "**/.*"],
[false, 4, ".bar", "**/.*"],
[true, 6, ".bar", "**/.*"],
[true, 0, "foo/.bar", "*"],
[false, 2, "foo/.bar", "*"],
[true, 4, "foo/.bar", "*"],
[false, 6, "foo/.bar", "*"],
[false, 0, "foo/.bar", ".*"],
[false, 2, "foo/.bar", ".*"],
[false, 4, "foo/.bar", ".*"],
[false, 6, "foo/.bar", ".*"],
[true, 0, "foo/.bar", "**/*"],
[true, 2, "foo/.bar", "**/*"],
[true, 4, "foo/.bar", "**/*"],
[false, 6, "foo/.bar", "**/*"],
[true, 0, "foo/.bar", "**/.*"],
[true, 2, "foo/.bar", "**/.*"],
[true, 4, "foo/.bar", "**/.*"],
[true, 6, "foo/.bar", "**/.*"],
[true, 0, "foo/.bar/baz", "**/*"],
[true, 2, "foo/.bar/baz", "**/*"],
[true, 4, "foo/.bar/baz", "**/*"],
[false, 6, "foo/.bar/baz", "**/*"],
[true, 0, "foo/.bar/baz", "**/.*"],
[false, 2, "foo/.bar/baz", "**/.*"],
[true, 4, "foo/.bar/baz", "**/.*"],
[false, 6, "foo/.bar/baz", "**/.*"],
[true, 0, "foo/bar/.baz", "**/*"],
[true, 2, "foo/bar/.baz", "**/*"],
[true, 4, "foo/bar/.baz", "**/*"],
[false, 6, "foo/bar/.baz", "**/*"],
[true, 0, "foo/bar/.baz", "**/.*"],
[true, 2, "foo/bar/.baz", "**/.*"],
[true, 4, "foo/bar/.baz", "**/.*"],
[true, 6, "foo/bar/.baz", "**/.*"],
[true, 0, "foobar", "foo\\bar"],
[false, 8, "foobar", "foo\\bar"],
[true, 8, "foo\\bar", "foo\\bar"],
[true, 2, "foo/bar/baz", "foo//bar/baz"],
[true, 2, "foo/bar/baz", "foo/////bar/////////baz"]
]"#;

    #[test]
    fn wildmatch_full_table() {
        let table: Vec<(bool, u32, String, String)> =
            serde_json::from_str(WILDMATCH_TABLE).expect("wildmatch table parses");
        assert_eq!(table.len(), 208, "table size matches WildmatchTest.json");
        let mut checked = 0;
        for (i, (expect, flags, text, pattern)) in table.iter().enumerate() {
            let got = wildmatch(pattern.as_bytes(), text.as_bytes(), *flags);
            assert_eq!(
                got, *expect,
                "case {i}: pattern [{pattern}] vs text [{text}] flags {flags}"
            );
            checked += 1;
        }
        assert!(checked > 200);
    }

    // ---- glob generator (test_glob.py) ----------------------------------

    fn glob_fixture() -> Fixture {
        Fixture::new(&[
            ("a.c", false),
            ("b.c", false),
            (".a.c", false),
            ("includes", true),
            ("includes/a.h", false),
            ("includes/b.h", false),
            ("includes/second", true),
            ("includes/second/foo.h", false),
            ("includes/second/bar.h", false),
        ])
    }

    #[test]
    fn glob_generator_matches_watchman() {
        let f = glob_fixture();
        // `*.h` at the root matches nothing: '*' does not cross '/'.
        assert!(f.query(&json!({"glob": ["*.h"]})).is_empty());

        // `**/*.h` matches every nested .h file.
        assert_eq!(
            f.query(&json!({"glob": ["**/*.h"]})),
            sorted(&[
                "includes/a.h",
                "includes/b.h",
                "includes/second/bar.h",
                "includes/second/foo.h",
            ])
        );

        // `*.c`: dotfiles excluded by default.
        assert_eq!(f.query(&json!({"glob": ["*.c"]})), sorted(&["a.c", "b.c"]));

        // ...but included with glob_includedotfiles.
        assert_eq!(
            f.query(&json!({"glob": ["*.c"], "glob_includedotfiles": true})),
            sorted(&[".a.c", "a.c", "b.c"])
        );

        // Overlapping patterns dedup to the same set.
        assert_eq!(
            f.query(&json!({"glob": ["**/*.h", "**/**/*.h", "includes/*.h"]})),
            f.query(&json!({"glob": ["**/*.h"]}))
        );

        // `*/*.h` is exactly one directory deep.
        assert_eq!(
            f.query(&json!({"glob": ["*/*.h"]})),
            sorted(&["includes/a.h", "includes/b.h"])
        );

        // relative_root re-anchors both the candidate set and the patterns
        // (test_glob.py).
        assert_eq!(
            f.query_in(&json!({"glob": ["*.h"]}), "includes"),
            sorted(&["a.h", "b.h"])
        );
        assert_eq!(
            f.query_in(&json!({"glob": ["**/*.h"]}), "includes/second"),
            sorted(&["bar.h", "foo.h"])
        );
        // Windows-style separators in relative_root are normalized.
        assert_eq!(
            f.query_in(&json!({"glob": ["**/*.h"]}), "includes\\second"),
            sorted(&["bar.h", "foo.h"])
        );
        assert_eq!(
            f.query_in(&json!({"glob": ["**/*.h"]}), "includes"),
            sorted(&["a.h", "b.h", "second/bar.h", "second/foo.h"])
        );
        // NOTE: Watchman errors on a bogus relative_root ("check your
        // relative_root"); fflocate's `execute_expr` instead scopes to an
        // anchor with no candidates, so there is no error to port here.
    }

    #[test]
    fn glob_generator_empty_and_absolute() {
        let f = glob_fixture();
        assert!(f.query(&json!({"glob": []})).is_empty());
        let err = parse_query(&json!({"glob": ["//fbandroid/*.cpp"]})).unwrap_err();
        assert!(err.contains("is an absolute path"), "{err}");
        let err = parse_query(&json!({"glob": [12345]})).unwrap_err();
        assert_eq!(err, "expected json string object");
    }

    #[test]
    fn glob_case_sensitive() {
        let f = Fixture::new(&[("Hello", true), ("hello.c", false)]);
        assert_eq!(
            f.query(&json!({"glob": ["Hello*"], "case_sensitive": true})),
            sorted(&["Hello"])
        );
        assert_eq!(
            f.query(&json!({"glob": ["Hello*"], "case_sensitive": false})),
            sorted(&["Hello", "hello.c"])
        );
    }

    // ---- name / iname (test_name.py) ------------------------------------

    #[test]
    fn name_expr() {
        let f = Fixture::new(&[
            ("foo.c", false),
            ("subdir", true),
            ("subdir/bar.txt", false),
        ]);
        assert_eq!(f.expr(json!(["iname", "FOO.c"])), sorted(&["foo.c"]));
        assert_eq!(
            f.expr(json!(["iname", ["FOO.c", "INVALID.txt"]])),
            sorted(&["foo.c"])
        );
        assert_eq!(f.expr(json!(["name", "foo.c"])), sorted(&["foo.c"]));
        assert_eq!(
            f.expr(json!(["name", ["foo.c", "invalid"]])),
            sorted(&["foo.c"])
        );
        assert_eq!(
            f.expr(json!(["name", "foo.c", "wholename"])),
            sorted(&["foo.c"])
        );
        // wholename is anchored: `bar.txt` != `subdir/bar.txt`.
        assert!(f.expr(json!(["name", "bar.txt", "wholename"])).is_empty());

        // With relative_root, wholename is anchored at the sub-root.
        assert_eq!(
            f.query_in(
                &json!({"expression": ["name", "bar.txt", "wholename"]}),
                "subdir"
            ),
            sorted(&["bar.txt"])
        );
        // ...and `foo.c` is not under `subdir`, so it no longer matches.
        assert!(f
            .query_in(
                &json!({"expression": ["name", "foo.c", "wholename"]}),
                "subdir"
            )
            .is_empty());
    }

    #[test]
    fn name_expr_errors() {
        let e = |v: Value| parse_expr(&v, true).unwrap_err();
        assert_eq!(e(json!("name")), "Expected array for 'name' term");
        assert_eq!(
            e(json!(["name", "one", "two", "three"])),
            "Invalid number of arguments for 'name' term"
        );
        assert_eq!(
            e(json!(["name", 2])),
            "Argument 2 to 'name' must be either a string or an array of string"
        );
        assert_eq!(
            e(json!(["name", "one", 2])),
            "Argument 3 to 'name' must be a string"
        );
        assert_eq!(
            e(json!(["name", "one", "invalid"])),
            "Invalid scope 'invalid' for name expression"
        );
    }

    // ---- match (test_match.py) ------------------------------------------

    fn match_fixture() -> Fixture {
        Fixture::new(&[
            ("foo.c", false),
            ("bar.txt", false),
            ("foo", true),
            ("foo/.bar.c", false),
            ("foo/baz.c", false),
        ])
    }

    #[test]
    fn match_expr() {
        let f = match_fixture();
        assert_eq!(
            f.expr(json!(["match", "*.c"])),
            sorted(&["foo.c", "foo/baz.c"])
        );
        assert_eq!(
            f.expr(json!(["match", "*.c", "wholename"])),
            sorted(&["foo.c"])
        );
        assert_eq!(
            f.expr(json!(["match", "foo/*.c", "wholename"])),
            sorted(&["foo/baz.c"])
        );
        assert_eq!(
            f.expr(json!(["match", "**/*.c", "wholename"])),
            sorted(&["foo.c", "foo/baz.c"])
        );
        assert_eq!(
            f.expr(json!(["match", "**/*.c", "wholename", {"includedotfiles": true}])),
            sorted(&["foo.c", "foo/.bar.c", "foo/baz.c"])
        );
        assert_eq!(
            f.expr(json!(["match", "foo/**/*.c", "wholename"])),
            sorted(&["foo/baz.c"])
        );

        // Case sensitivity follows the query, not the filesystem.
        assert!(f.expr(json!(["match", "FOO/*.c", "wholename"])).is_empty());
        assert_eq!(
            f.query(
                &json!({"expression": ["match", "FOO/*.c", "wholename"], "case_sensitive": true})
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            f.query(
                &json!({"expression": ["match", "FOO/*.c", "wholename"], "case_sensitive": false})
            ),
            sorted(&["foo/baz.c"])
        );
    }

    // ---- suffix (test_suffix.py, test_suffix_generator.py) -------------

    #[test]
    fn suffix_expr_and_generator() {
        let f = Fixture::new(&[
            ("foo.c", false),
            ("README.pdf", false),
            ("html", true),
            ("html/frames.html", false),
            ("win", true),
            ("win/ms.dll", false),
        ]);
        assert_eq!(
            f.expr(json!([
                "anyof",
                ["suffix", "pdf"],
                ["suffix", "nomatch"],
                ["suffix", "dll"]
            ])),
            sorted(&["README.pdf", "win/ms.dll"])
        );
        // Equivalent to the array form.
        assert_eq!(
            f.expr(json!(["anyof", ["suffix", ["pdf", "nomatch", "dll"]]])),
            sorted(&["README.pdf", "win/ms.dll"])
        );
        // Empty boundary.
        assert!(f
            .expr(json!(["anyof", ["suffix", "nomatch"], ["suffix", "none"]]))
            .is_empty());

        // Case-insensitive: uppercase '.C' matches lower-case generator 'c'.
        let g = Fixture::new(&[
            ("foo.C", false),
            ("subdir", true),
            ("subdir/bar.txt", false),
        ]);
        assert_eq!(g.query(&json!({"suffix": "c"})), sorted(&["foo.C"]));
        assert_eq!(
            g.query(&json!({"suffix": ["c", "txt"]})),
            sorted(&["foo.C", "subdir/bar.txt"])
        );
        assert_eq!(
            g.query_in(&json!({"suffix": ["c", "txt"]}), "subdir"),
            sorted(&["bar.txt"])
        );
        assert!(g.query(&json!({"suffix": []})).is_empty());
    }

    #[test]
    fn suffix_errors() {
        assert_eq!(
            parse_expr(&json!("suffix"), true).unwrap_err(),
            "Expected array for 'suffix' term"
        );
        assert_eq!(
            parse_query(&json!({"suffix": {"a": "b"}})).unwrap_err(),
            "'suffix' must be a string or an array of strings"
        );
    }

    // ---- type (test_type.py) --------------------------------------------

    #[test]
    fn type_expr() {
        let f = Fixture::new(&[
            ("foo.c", false),
            ("subdir", true),
            ("subdir/bar.txt", false),
        ]);
        assert_eq!(
            f.expr(json!(["type", "f"])),
            sorted(&["foo.c", "subdir/bar.txt"])
        );
        assert_eq!(f.expr(json!(["type", "d"])), sorted(&["subdir"]));
        assert_eq!(
            parse_expr(&json!(["type", "x"]), true).unwrap_err(),
            "invalid type string 'x'"
        );
        assert_eq!(
            parse_expr(&json!("type"), true).unwrap_err(),
            "\"type\" term requires a type string parameter"
        );
        assert_eq!(
            parse_expr(&json!(["type", 123]), true).unwrap_err(),
            "First parameter to \"type\" term must be a type string"
        );
    }

    // ---- size (test_size.py) --------------------------------------------

    #[test]
    fn size_expr() {
        // Sizes are supplied directly since fflocate's v1 index does not carry
        // them; this exercises the comparison operators.
        let cases: &[(&str, i64, &[&str])] = &[
            ("eq", 0, &["empty"]),
            ("ne", 0, &["1k", "notempty"]),
            ("gt", 0, &["1k", "notempty"]),
            ("gt", 2, &["1k", "notempty"]),
            ("ge", 3, &["1k", "notempty"]),
            ("gt", 3, &["1k"]),
            ("le", 3, &["empty", "notempty"]),
            ("lt", 3, &["empty"]),
        ];
        let sizes: &[(&str, u64)] = &[("empty", 0), ("notempty", 3), ("1k", 1024)];
        for (op, n, expect) in cases {
            let ex = parse_expr(&json!(["size", op, n]), true).unwrap();
            let mut got = Vec::new();
            for (name, sz) in sizes {
                let ctx = MatchCtx {
                    name: name.as_bytes(),
                    rel_path: name.as_bytes(),
                    is_dir: false,
                    is_symlink: false,
                    is_exec: false,
                    size: Some(*sz),
                    otime: None,
                    ctime: None,
                    mtime: None,
                    fs_case_insensitive: false,
                };
                if ex.eval(&ctx) {
                    got.push(name.to_string());
                }
            }
            got.sort();
            assert_eq!(got, sorted(expect), "size {op} {n}");
        }
    }

    // ---- dirname (test_dirname.py) --------------------------------------

    #[test]
    fn dirname_expr() {
        // Build the deep `i/i/i/i/i/a` tree for i in 0..4, plus root `a`.
        let mut entries: Vec<(String, bool)> = Vec::new();
        for i in 0..5 {
            let mut p = String::new();
            for _ in 0..5 {
                p.push_str(&i.to_string());
                p.push('/');
            }
            // files `a` at every depth under i
            let mut acc = String::new();
            for _ in 0..5 {
                acc.push_str(&i.to_string());
                entries.push((format!("{acc}/a"), false));
                acc.push('/');
            }
        }
        entries.push(("a".to_string(), false));
        let refs: Vec<(&str, bool)> = entries.iter().map(|(s, d)| (s.as_str(), *d)).collect();
        let f = Fixture::new(&refs);

        // ["dirname", ""] + ["name","a"] -> all files named a.
        let all_a = f.expr(json!(["allof", ["dirname", ""], ["name", "a"]]));
        assert_eq!(all_a.len(), 26, "all `a` files: {all_a:?}");

        // ["dirname","", ["depth","gt",4]] -> only depth-5 files.
        let d5 = f.expr(json!([
            "allof",
            ["dirname", "", ["depth", "gt", 4]],
            ["name", "a"]
        ]));
        assert_eq!(
            d5,
            sorted(&[
                "0/0/0/0/0/a",
                "1/1/1/1/1/a",
                "2/2/2/2/2/a",
                "3/3/3/3/3/a",
                "4/4/4/4/4/a"
            ])
        );

        // ["dirname","1"] -> only files under 1/.
        assert_eq!(
            f.expr(json!(["allof", ["dirname", "1"], ["name", "a"]])),
            sorted(&["1/1/1/1/1/a", "1/1/1/1/a", "1/1/1/a", "1/1/a", "1/a"])
        );

        // ["dirname","1", ["depth","gt",0]] excludes depth-0 `1/a`.
        assert_eq!(
            f.expr(json!([
                "allof",
                ["dirname", "1", ["depth", "gt", 0]],
                ["name", "a"]
            ])),
            sorted(&["1/1/1/1/1/a", "1/1/1/1/a", "1/1/1/a", "1/1/a"])
        );
        assert_eq!(
            f.expr(json!([
                "allof",
                ["dirname", "1", ["depth", "gt", 4]],
                ["name", "a"]
            ])),
            Vec::<String>::new()
        );
    }

    // ---- path generator (test_path_generator.py) -----------------------

    #[test]
    fn path_generator() {
        let f = Fixture::new(&[("foo", true), ("foo/bar", false)]);
        assert_eq!(
            f.query(&json!({"path": ["foo"]})),
            sorted(&["foo", "foo/bar"])
        );
        assert!(f.query(&json!({"path": ["."]})).is_empty()); // '.' never resolves
        assert!(f.query(&json!({"path": []})).is_empty()); // empty generator

        // depth variants (test_path_generator.py).
        assert_eq!(
            f.query(&json!({"path": [{"path": "foo", "depth": 0}]})),
            sorted(&["foo"])
        );
        assert_eq!(
            f.query(&json!({"path": [{"path": "foo/bar", "depth": -1}]})),
            sorted(&["foo/bar"])
        );

        // The path generator is case-sensitive even on case-insensitive
        // filesystems (Watchman: "Case insensitive matching not implemented
        // for path generator").
        let g = Fixture::new(&[("Foo", true), ("Foo/bar", false)]);
        assert_eq!(
            g.query(&json!({"path": ["Foo"]})),
            sorted(&["Foo", "Foo/bar"])
        );
        assert!(g.query(&json!({"path": ["foo"]})).is_empty());
    }

    // ---- invalid expressions (test_invalid_expr.py) --------------------

    #[test]
    fn invalid_expr_terms() {
        let e = parse_expr(
            &json!([
                "allof",
                "dont-implement-this-term",
                ["anyof", ["suffix", "apcarc"]]
            ]),
            true,
        )
        .unwrap_err();
        assert_eq!(e, "unknown expression term 'dont-implement-this-term'");

        let e = parse_expr(&json!(["not", "a", "b"]), true).unwrap_err();
        assert_eq!(e, "must use [\"not\", expr]");

        let e = parse_expr(&json!(["allof"]), true).unwrap_err();
        assert_eq!(e, "must use [\"allof\", expr...]");
    }

    // ---- empty / exists (test_empty.py) ---------------------------------

    #[test]
    fn empty_and_exists_terms() {
        // Sizes are synthesized: the v1 index does not carry them, as with
        // the size tests above.
        let f = Fixture::new(&[("empty", false), ("notempty", false)]);
        let sizes = |rel: &[u8]| if rel == b"empty" { Some(0) } else { Some(3) };

        let got = f.run_with(&parse_expr(&json!("empty"), true).unwrap(), |rel, ctx| {
            ctx.size = sizes(rel);
        });
        assert_eq!(got, sorted(&["empty"]));

        let got = f.run_with(&parse_expr(&json!("exists"), true).unwrap(), |rel, ctx| {
            ctx.size = sizes(rel);
        });
        assert_eq!(got, sorted(&["empty", "notempty"]));

        // `not exists` matches nothing live; Watchman only reports tombstones
        // for it together with a `since`, which is a different code path.
        let got = f.run_with(
            &parse_expr(&json!(["not", "exists"]), true).unwrap(),
            |rel, ctx| ctx.size = sizes(rel),
        );
        assert!(got.is_empty());
    }

    // ---- pcre / ipcre (test_pcre.py) -----------------------------------

    #[test]
    fn pcre_expr() {
        let f = Fixture::new(&[("foo.c", false), ("bar.txt", false)]);
        assert_eq!(f.expr(json!(["pcre", ".*c$"])), sorted(&["foo.c"]));
        assert_eq!(f.expr(json!(["pcre", ".*txt$"])), sorted(&["bar.txt"]));
        // `pcre` is case-sensitive by default.
        assert!(f.expr(json!(["pcre", ".*C$"])).is_empty());
        // ...and matches the filesystem default would, case-insensitively
        // with `ipcre`.
        assert_eq!(f.expr(json!(["ipcre", ".*C$"])), sorted(&["foo.c"]));
        assert_eq!(
            f.expr(json!(["pcre", ".*c$", "wholename"])),
            sorted(&["foo.c"])
        );

        let err = parse_expr(&json!(["pcre", "("]), true).unwrap_err();
        assert!(err.contains("invalid pcre"), "{err}");
        // Watchman reports "at offset 1 in (" from PCRE2; the `regex` crate
        // reports no numeric offset, but the term and pattern are named.
        assert!(err.ends_with("in ("), "{err}");
    }

    // ---- suffix optimisation (test/SuffixQueryTest.cpp) -----------------

    fn sst(e: Value) -> SimpleSuffixType {
        parse_expr(&e, true).unwrap().simple_suffix_type()
    }

    fn ssg(e: Value) -> Vec<String> {
        parse_expr(&e, true)
            .unwrap()
            .suffix_glob_patterns()
            .iter()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect()
    }

    #[test]
    fn suffix_query_optimisation() {
        assert_eq!(sst(json!(["false"])), SimpleSuffixType::Excluded);
        assert!(ssg(json!(["false"])).is_empty());

        assert_eq!(sst(json!(["type", "d"])), SimpleSuffixType::Excluded);
        assert!(ssg(json!(["type", "d"])).is_empty());

        assert_eq!(sst(json!(["type", "f"])), SimpleSuffixType::Type);
        assert!(ssg(json!(["type", "f"])).is_empty());

        assert_eq!(sst(json!(["suffix", ["a", "f"]])), SimpleSuffixType::Suffix);
        assert_eq!(
            ssg(json!(["suffix", ["a", "f"]])),
            sorted(&["**/*.a", "**/*.f"])
        );

        assert_eq!(
            sst(json!(["allof", ["type", "f"], ["exists"]])),
            SimpleSuffixType::Excluded
        );
        assert!(ssg(json!(["allof", ["type", "f"], ["exists"]])).is_empty());

        assert_eq!(
            sst(json!(["allof", ["type", "f"], ["suffix", ["a"]]])),
            SimpleSuffixType::IsSimpleSuffix
        );
        assert_eq!(
            ssg(json!(["allof", ["type", "f"], ["suffix", ["a"]]])),
            sorted(&["**/*.a"])
        );
    }

    // ---- since term (test_since_term.py) ---------------------------------

    #[test]
    fn since_term() {
        let base: i64 = 1_700_000_000;
        let f = Fixture::new(&[
            ("foo.c", false),
            ("subdir", true),
            ("subdir/bar.txt", false),
        ]);

        // `mtime` with a timestamp: `>=` (boundary inclusive).
        let e = parse_expr(&json!(["since", base, "mtime"]), true).unwrap();
        let got = f.run_with(&e, |rel, ctx| {
            ctx.mtime = Some(if rel == b"subdir/bar.txt" {
                base - 1
            } else {
                base
            });
        });
        assert_eq!(got, sorted(&["foo.c", "subdir"]));

        // Composed with other terms, as in test_since_term.
        let e = parse_expr(
            &json!(["allof", ["since", base - 1, "mtime"], ["name", "foo.c"]]),
            true,
        )
        .unwrap();
        let got = f.run_with(&e, |_, ctx| ctx.mtime = Some(base));
        assert_eq!(got, sorted(&["foo.c"]));

        // Bare `since` with a timestamp defaults to `oclock`, also `>=`.
        let e = parse_expr(&json!(["since", base]), true).unwrap();
        let got = f.run_with(&e, |_, ctx| ctx.otime = Some(base));
        assert_eq!(got, sorted(&["foo.c", "subdir", "subdir/bar.txt"]));
        let got = f.run_with(&e, |_, ctx| ctx.otime = Some(base - 1));
        assert!(got.is_empty());

        // `cclock`/`ctime` select the change-clock/stat fields.
        let e = parse_expr(&json!(["since", base, "cclock"]), true).unwrap();
        let got = f.run_with(&e, |_, ctx| ctx.ctime = Some(base + 1));
        assert_eq!(got, sorted(&["foo.c", "subdir", "subdir/bar.txt"]));
        let e = parse_expr(&json!(["since", base, "ctime"]), true).unwrap();
        let got = f.run_with(&e, |_, ctx| ctx.ctime = Some(base));
        assert_eq!(got, sorted(&["foo.c", "subdir", "subdir/bar.txt"]));

        // New-style clock `c:start:pid:root:ticks`: strict `>` on ticks.
        let e = parse_expr(&json!(["since", "c:1000:42:3:7"]), true).unwrap();
        let got = f.run_with(&e, |_, ctx| ctx.otime = Some(8));
        assert_eq!(got, sorted(&["foo.c", "subdir", "subdir/bar.txt"]));
        let got = f.run_with(&e, |_, ctx| ctx.otime = Some(7));
        assert!(got.is_empty());

        // Old-style clock `c:pid:ticks` is a fresh instance: everything
        // that exists matches.
        let e = parse_expr(&json!(["since", "c:999:7"]), true).unwrap();
        let got = f.run_with(&e, |_, ctx| ctx.otime = Some(1));
        assert_eq!(got, sorted(&["foo.c", "subdir", "subdir/bar.txt"]));

        // Object clockspecs: `{"clock": ...}` unwraps the clock string, `{}`
        // is a fresh instance.
        let e = parse_expr(&json!(["since", {"clock": "c:1000:42:3:7"}]), true).unwrap();
        let got = f.run_with(&e, |_, ctx| ctx.otime = Some(8));
        assert_eq!(got, sorted(&["foo.c", "subdir", "subdir/bar.txt"]));
        let got = f.run_with(&e, |_, ctx| ctx.otime = Some(7));
        assert!(got.is_empty());
        let e = parse_expr(&json!(["since", {}]), true).unwrap();
        let got = f.run_with(&e, |_, ctx| ctx.otime = Some(1));
        assert_eq!(got, sorted(&["foo.c", "subdir", "subdir/bar.txt"]));
    }

    #[test]
    fn since_term_errors() {
        let e = |v: Value| parse_expr(&v, true).unwrap_err();
        assert_eq!(e(json!("since")), "\"since\" term must be an array");
        assert_eq!(
            e(json!(["since"])),
            "\"since\" term has invalid number of parameters"
        );
        assert_eq!(
            e(json!(["since", 1, "mtime", 2])),
            "\"since\" term has invalid number of parameters"
        );
        assert_eq!(
            e(json!(["since", "bogus"])),
            "invalid clockspec for \"since\" term"
        );
        assert_eq!(
            e(json!(["since", "n:foo"])),
            "named cursors are not allowed in \"since\" terms"
        );
        assert_eq!(
            e(json!(["since", 1, 2])),
            "field name for \"since\" term must be a string"
        );
        assert_eq!(
            e(json!(["since", 1, "bogus"])),
            "invalid field name \"bogus\" for \"since\" term"
        );
        assert_eq!(
            e(json!(["since", "c:1000:42:3:7", "mtime"])),
            "field \"mtime\" requires a timestamp value for comparison in \"since\" term"
        );
    }

    // ---- compound suffix (test_suffix.py test_match_suffix) ------------

    #[test]
    fn compound_suffix_allof() {
        let f = Fixture::new(&[
            ("foo.c", false),
            ("README.pdf", false),
            ("html", true),
            ("html/frames.html", false),
            ("html/mov.mp4", false),
            ("html/ignore.xxx", false),
            ("win", true),
            ("win/ms.dll", false),
            ("win/ignore.txt", false),
        ]);
        let res1 = f.expr(json!([
            "anyof",
            [
                "allof",
                ["dirname", "html"],
                ["type", "f"],
                [
                    "anyof",
                    ["suffix", "pdf"],
                    ["suffix", "html"],
                    ["suffix", "nomatch"]
                ]
            ],
            ["name", ".never-match-this", "wholename"]
        ]));
        assert_eq!(res1, sorted(&["html/frames.html"]));
        let res2 = f.expr(json!([
            "anyof",
            [
                "allof",
                ["dirname", "html"],
                ["type", "f"],
                ["anyof", ["suffix", ["pdf", "html", "nomatch"]]]
            ],
            ["name", ".never-match-this", "wholename"]
        ]));
        assert_eq!(res1, res2);
    }
}
