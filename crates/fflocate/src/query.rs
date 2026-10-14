//! The query engine (§10).
//!
//! A linear scan over the in-memory name arena — Everything's model, and fast
//! enough at v1 scale. The query is compiled once into a [`Query`]:
//!
//! * space-separated terms are ANDed substrings,
//! * `"..."` is a literal phrase (may contain spaces),
//! * `*` / `?` make a term a glob,
//! * `ext:rs` matches the file extension, `path:src` matches anywhere in the
//!   materialized path, `case:foo` forces case sensitivity, `regex:...` is a
//!   full regex.
//!
//! Matching is ASCII-case-insensitive by default ("smart-case": a term with an
//! uppercase letter is matched case-sensitively, like ripgrep/fzf).
//!
//! The engine does one `aho-corasick` pass over all literal tokens (rarest
//! first, AND short-circuit), and only materializes a path for entries that
//! survive the name-level predicates. `--count` skips materialization entirely
//! (§10).

use std::borrow::Cow;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use regex::bytes::Regex;

use crate::index::Index;

/// One ANDed predicate over an entry.
#[derive(Clone, Debug)]
enum Term {
    /// Substring of the *name* (the bytes of the entry's own name), or of the
    /// whole path when `path_only` is false and the term came from a plain
    /// query? No: plain terms match the name; `path:` terms match the path.
    Substr { needle: Vec<u8>, case: bool },
    /// Glob over the name.
    Glob { pattern: GlobPattern },
    /// `ext:rs` — matches `*.rs` suffixes.
    Ext { ext: Vec<u8>, case: bool },
    /// `path:src` — substring anywhere in the materialized path.
    Path { needle: Vec<u8>, case: bool },
    /// `regex:...` — full regex over the name.
    Regex { re: Box<Regex> },
    /// `type:dir` — entry is a directory (`-type:dir` = files only).
    IsDir,
    /// `type:exec` — entry has any execute bit set (`find -perm /111`).
    /// Reads the `IS_EXEC` flag, so it never stats: the crawl, the event
    /// path, the rescan diff and the ATTRIB refresh keep it current.
    IsExec,
    /// `-foo` — negation: entry must NOT match.
    Not(Box<Term>),
}

/// A compiled glob: literal segments interspersed with `*` (any) and `?` (one).
#[derive(Clone, Debug)]
pub struct GlobPattern {
    parts: Vec<GlobPart>,
    case: bool,
}

#[derive(Clone, Debug)]
enum GlobPart {
    Literal(Vec<u8>),
    AnySeq,
    AnyChar,
}

impl GlobPattern {
    pub fn parse(pattern: &[u8], case: bool) -> GlobPattern {
        let mut parts = Vec::new();
        let mut lit = Vec::new();
        for &b in pattern {
            match b {
                b'*' => {
                    if !lit.is_empty() {
                        parts.push(GlobPart::Literal(std::mem::take(&mut lit)));
                    }
                    parts.push(GlobPart::AnySeq);
                }
                b'?' => {
                    if !lit.is_empty() {
                        parts.push(GlobPart::Literal(std::mem::take(&mut lit)));
                    }
                    parts.push(GlobPart::AnyChar);
                }
                _ => lit.push(b),
            }
        }
        if !lit.is_empty() {
            parts.push(GlobPart::Literal(lit));
        }
        GlobPattern { parts, case }
    }

    pub fn is_literal(&self) -> bool {
        self.parts.iter().all(|p| matches!(p, GlobPart::Literal(_)))
    }

    pub fn matches(&self, hay: &[u8]) -> bool {
        let hay_norm = self.normalize(hay);
        let hay = &hay_norm[..];
        // Classic iterative glob match with backtracking on the last `*`.
        let mut px = 0usize;
        let mut hx = 0usize;
        let mut star_px: Option<usize> = None;
        let mut star_hx = 0usize;
        while hx < hay.len() {
            match self.parts.get(px) {
                Some(GlobPart::Literal(l)) => {
                    let ln = self.normalize(l);
                    if hay[hx..].starts_with(&ln) {
                        hx += ln.len();
                        px += 1;
                    } else if let Some(sp) = star_px {
                        star_hx += 1;
                        hx = star_hx;
                        px = sp;
                    } else {
                        return false;
                    }
                }
                Some(GlobPart::AnyChar) => {
                    hx += 1;
                    px += 1;
                }
                Some(GlobPart::AnySeq) => {
                    star_px = Some(px + 1);
                    star_hx = hx;
                    px += 1;
                }
                None => {
                    if let Some(sp) = star_px {
                        star_hx += 1;
                        hx = star_hx;
                        px = sp;
                    } else {
                        return false;
                    }
                }
            }
        }
        // Consume trailing AnySeq parts.
        while let Some(GlobPart::AnySeq) = self.parts.get(px) {
            px += 1;
        }
        px == self.parts.len()
    }

    fn normalize<'a>(&self, b: &'a [u8]) -> Cow<'a, [u8]> {
        if self.case {
            Cow::Borrowed(b)
        } else {
            Cow::Owned(b.iter().map(|c| c.to_ascii_lowercase()).collect())
        }
    }
}

/// A compiled query.
pub struct Query {
    /// Which root paths to attribute to. Populated by the caller from the
    /// index's roots table, not part of the parse.
    terms: Vec<Term>,
    /// All literal needles, for the aho-corasick prefilter. Each is a required
    /// substring (name or path semantics preserved by `prefilter_terms`).
    literals: Vec<Vec<u8>>,
    #[allow(dead_code)]
    literal_case: Vec<bool>,
    ac: Option<AhoCorasick>,
    /// Whether the query has any predicate at all (empty query matches all).
    pub is_empty: bool,
    pub raw: String,
}

impl std::fmt::Debug for Query {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Query")
            .field("raw", &self.raw)
            .field("terms", &self.terms)
            .finish()
    }
}

impl Query {
    pub fn compile(raw: &str) -> Result<Query, String> {
        Self::compile_with_case(raw, false)
    }

    /// Compile `raw`, optionally forcing every term to match case-sensitively.
    ///
    /// This is the daemon-side honouring of the protocol's `case_sensitive`
    /// field on the plain-string path (an expression query carries it in the
    /// expression instead). It overrides smart-case: a lowercase term that
    /// would normally match case-insensitively is made case-sensitive too.
    pub fn compile_with_case(raw: &str, force_case: bool) -> Result<Query, String> {
        let mut terms = parse_terms(raw)?;
        if force_case {
            for t in terms.iter_mut() {
                force_term_case(t);
            }
        }
        Self::from_terms(terms, raw)
    }

    fn from_terms(terms: Vec<Term>, raw: &str) -> Result<Query, String> {
        let mut literals = Vec::new();
        let mut literal_case = Vec::new();
        for t in &terms {
            match t {
                Term::Substr { needle, case } => {
                    literals.push(needle.clone());
                    literal_case.push(*case);
                }
                Term::Ext { ext, case } => {
                    // extension literal is required somewhere in the name
                    literals.push(ext.clone());
                    literal_case.push(*case);
                }
                _ => {}
            }
        }
        let ac = if literals.is_empty() {
            None
        } else {
            // Case-insensitive patterns must be lowercased; smart-case terms
            // keep their case but we cannot mix in one AC. Fall back to a
            // post-filter for mixed-case queries (still one AC pass when
            // possible).
            let all_ci = literal_case.iter().all(|c| !*c);
            if all_ci {
                let pats: Vec<Vec<u8>> = literals
                    .iter()
                    .map(|l| l.iter().map(|c| c.to_ascii_lowercase()).collect())
                    .collect();
                Some(
                    AhoCorasickBuilder::new()
                        .match_kind(MatchKind::LeftmostFirst)
                        .ascii_case_insensitive(true)
                        .build(&pats)
                        .map_err(|e| e.to_string())?,
                )
            } else {
                None
            }
        };
        let is_empty = terms.is_empty();
        Ok(Query {
            terms,
            literals,
            literal_case,
            ac,
            is_empty,
            raw: raw.to_string(),
        })
    }

    /// Does a candidate entry (by name and kind, with its materialized path
    /// lazily supplied) pass every term? `path` is only computed when a
    /// `path:` term exists; `path_provider` is called at most once.
    #[allow(clippy::redundant_closure)] // closure needed: FnMut cannot be moved into FnOnce
    pub fn matches<F>(&self, name: &[u8], is_dir: bool, is_exec: bool, mut path_provider: F) -> bool
    where
        F: FnMut() -> Option<Vec<u8>>,
    {
        let mut path_cache: Option<Option<Vec<u8>>> = None;
        for t in &self.terms {
            let ok = match t {
                Term::Substr { needle, case } => contains(name, needle, *case),
                Term::Glob { pattern } => pattern.matches(name),
                Term::Ext { ext, case } => ext_matches(name, ext, *case),
                Term::Path { needle, case } => {
                    let p = path_cache.get_or_insert_with(|| path_provider());
                    match p {
                        Some(p) => contains(p, needle, *case),
                        None => false,
                    }
                }
                Term::Regex { re } => re.is_match(name),
                Term::IsDir => is_dir,
                Term::IsExec => is_exec,
                Term::Not(inner) => !self.match_inner(
                    inner,
                    name,
                    is_dir,
                    is_exec,
                    &mut path_cache,
                    &mut path_provider,
                ),
            };
            if !ok {
                return false;
            }
        }
        true
    }

    #[allow(clippy::redundant_closure)] // closure needed: FnMut cannot be moved into FnOnce
    fn match_inner<F>(
        &self,
        t: &Term,
        name: &[u8],
        is_dir: bool,
        is_exec: bool,
        path_cache: &mut Option<Option<Vec<u8>>>,
        path_provider: &mut F,
    ) -> bool
    where
        F: FnMut() -> Option<Vec<u8>>,
    {
        match t {
            Term::Substr { needle, case } => contains(name, needle, *case),
            Term::Glob { pattern } => pattern.matches(name),
            Term::Ext { ext, case } => ext_matches(name, ext, *case),
            Term::Path { needle, case } => {
                let p = path_cache.get_or_insert_with(|| path_provider());
                match p {
                    Some(p) => contains(p, needle, *case),
                    None => false,
                }
            }
            Term::Regex { re } => re.is_match(name),
            Term::IsDir => is_dir,
            Term::IsExec => is_exec,
            Term::Not(inner) => {
                !self.match_inner(inner, name, is_dir, is_exec, path_cache, path_provider)
            }
        }
    }

    /// Cheap prefilter using the aho-corasick automaton: returns true when the
    /// candidate *may* match all literal terms. Only valid when all literals
    /// are case-insensitive (mixed-case queries leave `ac` as `None`).
    pub fn prefilter(&self, haystack_lower: &[u8]) -> bool {
        match &self.ac {
            None => true,
            Some(ac) => {
                let mut seen = vec![false; self.literals.len()];
                let mut remaining = seen.len();
                for m in ac.find_iter(haystack_lower) {
                    let i = m.pattern().as_usize();
                    if !seen[i] {
                        seen[i] = true;
                        remaining -= 1;
                        if remaining == 0 {
                            return true;
                        }
                    }
                }
                remaining == 0
            }
        }
    }

    pub fn has_path_term(&self) -> bool {
        fn has(t: &Term) -> bool {
            match t {
                Term::Path { .. } => true,
                Term::Not(inner) => has(inner),
                _ => false,
            }
        }
        self.terms.iter().any(has)
    }

    pub fn has_regex_term(&self) -> bool {
        fn has(t: &Term) -> bool {
            match t {
                Term::Regex { .. } => true,
                Term::Not(inner) => has(inner),
                _ => false,
            }
        }
        self.terms.iter().any(has)
    }

    /// The literals that a matching name is *required* to contain, as
    /// `(needle, case_sensitive)` pairs — the raw material for the §17 trigram
    /// accelerator.
    ///
    /// Only positive `Substr` and `Ext` terms qualify:
    ///
    /// * a negated term (`-foo`) is not required to be present, so it can only
    ///   ever *widen* the candidate set — it must not appear here;
    /// * a `Glob` or `Regex` term has no fixed substring to look up;
    /// * a `Path` term is a substring of the *materialized path*, not the name,
    ///   and the accelerator indexes names only (§3/§10) — it is verified the
    ///   usual way.
    ///
    /// A name matches the query only if it contains every one of these needles,
    /// which is exactly the property the trigram accelerator exploits — soundly,
    /// because it is a superset filter.
    pub fn required_literals(&self) -> Vec<(&[u8], bool)> {
        let mut out: Vec<(&[u8], bool)> = Vec::new();
        for t in &self.terms {
            match t {
                Term::Substr { needle, case } => out.push((needle.as_slice(), *case)),
                Term::Ext { ext, case } => out.push((ext.as_slice(), *case)),
                _ => {}
            }
        }
        out
    }

    /// A trigram plan for this query: the required literals reduced to their
    /// distinct trigrams, with too-short and case-sensitive literals dropped. An
    /// empty plan means "this query has no usable trigram constraint — scan as
    /// usual".
    ///
    /// Only available with the default-on `trigram` cargo feature.
    #[cfg(feature = "trigram")]
    pub fn trigram_plan(&self) -> crate::trigram::TrigramPlan {
        crate::trigram::TrigramIndex::plan_from_literals(self.required_literals())
    }

    /// The needles that fuzzy search makes **approximate**: the positive,
    /// case-insensitive plain substrings of the name.
    ///
    /// Deliberately excluded, and therefore still matched exactly:
    ///
    /// * `Ext` — `ext:rs` means the extension is exactly `rs`; a fuzzy extension
    ///   would match unrelated files and surprises more than it helps;
    /// * `Path` — a substring of the materialized path, not the name (and the
    ///   trigram index cannot see it);
    /// * `Glob`/`Regex` — already pattern languages of their own;
    /// * `Not(_)` — a negation must stay exact or it stops being a filter;
    /// * any explicitly case-sensitive term (`case:`, or a smart-case uppercase
    ///   needle) — fuzzy edits across case would make `case:` meaningless.
    pub fn approx_needles(&self) -> Vec<&[u8]> {
        let mut out = Vec::new();
        for t in &self.terms {
            if let Term::Substr { needle, case } = t {
                if !*case {
                    out.push(needle.as_slice());
                }
            }
        }
        out
    }

    /// Evaluate every term **except** the approximate ones (see
    /// [`Self::approx_needles`]), i.e. the exact part of a fuzzy query: `ext:`,
    /// `path:`, glob, regex and negation still apply in full.
    ///
    /// Fuzzy search runs this alongside the edit-distance scoring so a term
    /// never gets to be both.
    pub fn matches_exact_terms<F>(
        &self,
        name: &[u8],
        is_dir: bool,
        is_exec: bool,
        mut path_provider: F,
    ) -> bool
    where
        F: FnMut() -> Option<Vec<u8>>,
    {
        let mut path_cache: Option<Option<Vec<u8>>> = None;
        for t in &self.terms {
            // Skip the terms fuzzy search owns.
            if let Term::Substr { case, .. } = t {
                if !*case {
                    continue;
                }
            }
            let ok = match t {
                Term::Substr { needle, case } => contains(name, needle, *case),
                Term::Glob { pattern } => pattern.matches(name),
                Term::Ext { ext, case } => ext_matches(name, ext, *case),
                Term::Path { needle, case } => {
                    #[allow(clippy::redundant_closure)] // FnMut cannot be moved into FnOnce
                    let p = path_cache.get_or_insert_with(|| path_provider());
                    match p {
                        Some(p) => contains(p, needle, *case),
                        None => false,
                    }
                }
                Term::Regex { re } => re.is_match(name),
                Term::IsDir => is_dir,
                Term::IsExec => is_exec,
                Term::Not(inner) => !self.match_inner(
                    inner,
                    name,
                    is_dir,
                    is_exec,
                    &mut path_cache,
                    &mut path_provider,
                ),
            };
            if !ok {
                return false;
            }
        }
        true
    }
}

// ------------------------------------------------------------------ parsing

fn parse_terms(raw: &str) -> Result<Vec<Term>, String> {
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // skip whitespace
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let mut negate = false;
        if bytes[i] == b'-' && i + 1 < bytes.len() && !(bytes[i + 1] as char).is_ascii_whitespace()
        {
            negate = true;
            i += 1;
        }
        // Read one token, honouring quotes.
        let mut tok = Vec::new();
        let mut quoted = false;
        if bytes[i] == b'"' {
            quoted = true;
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                tok.push(bytes[i]);
                i += 1;
            }
            if i >= bytes.len() {
                return Err("unterminated quote".into());
            }
            i += 1; // closing quote
        } else {
            while i < bytes.len() && !(bytes[i] as char).is_ascii_whitespace() {
                tok.push(bytes[i]);
                i += 1;
            }
        }
        let term = compile_token(&tok, quoted)?;
        out.push(if negate {
            Term::Not(Box::new(term))
        } else {
            term
        });
    }
    Ok(out)
}

fn compile_token(tok: &[u8], quoted: bool) -> Result<Term, String> {
    // Modifiers are only recognized on unquoted tokens.
    if !quoted {
        if let Some(rest) = strip_prefix_ci(tok, b"ext:") {
            let case = has_upper(rest);
            return Ok(Term::Ext {
                ext: rest.to_vec(),
                case,
            });
        }
        if let Some(rest) = strip_prefix_ci(tok, b"path:") {
            let case = has_upper(rest);
            return Ok(Term::Path {
                needle: rest.to_vec(),
                case,
            });
        }
        if let Some(rest) = strip_prefix_ci(tok, b"case:") {
            // Force case-sensitive substring of the name.
            return Ok(Term::Substr {
                needle: rest.to_vec(),
                case: true,
            });
        }
        if let Some(rest) = strip_prefix_ci(tok, b"regex:") {
            let re = Regex::new(std::str::from_utf8(rest).map_err(|_| "regex must be UTF-8")?)
                .map_err(|e| format!("bad regex: {e}"))?;
            return Ok(Term::Regex { re: Box::new(re) });
        }
        if let Some(rest) = strip_prefix_ci(tok, b"type:") {
            // Directories only (`-type:dir` inverts to files), or executables
            // (`-type:exec` inverts to non-executables). Anything else is a
            // usage error, not a silent substring.
            if rest.eq_ignore_ascii_case(b"dir") {
                return Ok(Term::IsDir);
            }
            if rest.eq_ignore_ascii_case(b"file") {
                return Ok(Term::Not(Box::new(Term::IsDir)));
            }
            if rest.eq_ignore_ascii_case(b"exec") {
                return Ok(Term::IsExec);
            }
            return Err("type: want dir, file, or exec".into());
        }
    }
    if !quoted && (tok.contains(&b'*') || tok.contains(&b'?')) {
        let case = has_upper(tok);
        return Ok(Term::Glob {
            pattern: GlobPattern::parse(tok, case),
        });
    }
    let case = has_upper(tok);
    Ok(Term::Substr {
        needle: tok.to_vec(),
        case,
    })
}

fn strip_prefix_ci<'a>(tok: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if tok.len() >= prefix.len() && tok[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&tok[prefix.len()..])
    } else {
        None
    }
}

/// Smart-case: a token containing an ASCII uppercase letter is matched
/// case-sensitively.
fn has_upper(tok: &[u8]) -> bool {
    tok.iter().any(|b| b.is_ascii_uppercase())
}

/// Mark a term (and any negated term inside it) as case-sensitive.
fn force_term_case(t: &mut Term) {
    match t {
        Term::Substr { case, .. } | Term::Ext { case, .. } | Term::Path { case, .. } => {
            *case = true
        }
        Term::Glob { pattern } => pattern.case = true,
        Term::Not(inner) => force_term_case(inner),
        // A regex carries its own flags, and a kind test has no case:
        // nothing to force.
        Term::Regex { .. } | Term::IsDir | Term::IsExec => {}
    }
}

fn contains(hay: &[u8], needle: &[u8], case: bool) -> bool {
    if needle.is_empty() {
        return true;
    }
    if case {
        memchr::memmem::find(hay, needle).is_some()
    } else {
        contains_ci(hay, needle)
    }
}

/// ASCII-case-insensitive substring search, allocation-free.
///
/// The old code lowercased the whole haystack (and re-lowercased the needle)
/// into fresh `Vec<u8>`s on *every entry* — ~1.3 µs/entry measured, which is
/// 5–8 s on a 2 M-entry index for a query like `"e"` whose trigram plan is
/// empty and which therefore scans everything. This folds on compare instead:
/// `memchr` skips to the next candidate start byte (either case), then a byte
/// loop confirms the rest. No allocation on any path; semantics identical
/// (`to_ascii_lowercase` on both sides, byte-exact).
/// Allocation-free ASCII case-insensitive substring search (see `contains`).
/// `#[doc(hidden)]` public only so examples/benches outside the crate can
/// measure it; the engine uses it through `contains`.
#[doc(hidden)]
pub fn contains_ci_pub(hay: &[u8], needle: &[u8]) -> bool {
    contains_ci(hay, needle)
}

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    debug_assert!(!needle.is_empty());
    // Single-byte needle: one SIMD pass for both case variants.
    if needle.len() == 1 {
        let n = needle[0].to_ascii_lowercase();
        // Non-ASCII bytes have no case variant; one byte suffices.
        if !needle[0].is_ascii_alphabetic() {
            return memchr::memchr(needle[0], hay).is_some();
        }
        return memchr::memchr2(n, n.to_ascii_uppercase(), hay).is_some();
    }
    let first = needle[0].to_ascii_lowercase();
    let first_upper = first.to_ascii_uppercase();
    let mut pos = 0usize;
    while pos + needle.len() <= hay.len() {
        // Skip to the next position whose byte matches the first needle byte
        // in either case. One SIMD pass for both variants (`memchr2`), not
        // two scans: the skip runs per first-byte occurrence, so its constant
        // matters most on common letters.
        let rel = match memchr::memchr2(first, first_upper, &hay[pos..]) {
            Some(r) => r,
            None => return false,
        };
        pos += rel;
        // The skip may have landed where the full needle no longer fits
        // (e.g. a lone trailing byte matching a longer needle): that is a
        // miss, not a match — the `zip` below would compare zero bytes and
        // report `true` on an empty confirmation.
        if pos + needle.len() > hay.len() {
            return false;
        }
        // Confirm the remaining bytes with case folding.
        let mut ok = true;
        for (h, n) in hay[pos + 1..].iter().zip(needle[1..].iter()) {
            if h.to_ascii_lowercase() != n.to_ascii_lowercase() {
                ok = false;
                break;
            }
        }
        if ok {
            return true;
        }
        pos += 1;
    }
    false
}

/// `ext:rs` matches a name ending in `.rs` (and, for compatibility with
/// Everything-style queries, a bare trailing `rs` is *not* accepted — the dot
/// is required).
fn ext_matches(name: &[u8], ext: &[u8], case: bool) -> bool {
    if ext.is_empty() {
        return true;
    }
    if name.len() < ext.len() + 1 {
        return false;
    }
    let dot = name.len() - ext.len() - 1;
    if name[dot] != b'.' {
        return false;
    }
    let tail = &name[dot + 1..];
    if case {
        tail == ext
    } else {
        tail.eq_ignore_ascii_case(ext)
    }
}

/// A single result row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub id: u32,
    pub path: Vec<u8>,
    /// Fuzzy match cost: 0 for an exact match, otherwise the summed edit distance
    /// (§10/§17). Only fuzzy search sets it; every other path leaves it 0, which
    /// keeps `Hit` comparable and ordering in index order unchanged.
    pub score: u32,
}

/// Run `q` over `index`, attributing paths using each root dir's stored path.
///
/// `limit == 0` means unlimited. When `count_only`, paths are never
/// materialized and the returned vec is empty with `total` set.
///
/// With the default-on `trigram` cargo feature this consults the §17
/// accelerator (building/using the caller's cache is the daemon's job — this
/// entry point uses none, so it behaves exactly like the plain scan). Without
/// the feature it is a straight linear scan. Either way the results are
/// identical.
pub fn execute(
    index: &Index,
    root_paths: &[(u32, Vec<u8>)],
    q: &Query,
    limit: usize,
    count_only: bool,
) -> Results {
    #[cfg(feature = "trigram")]
    {
        execute_with_trigram(index, root_paths, q, limit, count_only, None)
    }
    #[cfg(not(feature = "trigram"))]
    {
        execute_scan(index, root_paths, q, limit, count_only)
    }
}

/// As [`execute`], but consulting the §17 trigram accelerator when one is
/// supplied.
///
/// The accelerator is a **filter only**: it narrows the set of entries whose
/// names are worth checking, and every candidate still goes through the full
/// [`Query::matches`] path. It can therefore never change the result set — only
/// the time taken to produce it. `None` (or a query with no usable trigram
/// constraint) restores the plain linear scan.
///
/// Only available with the `trigram` cargo feature (on by default).
#[cfg(feature = "trigram")]
pub fn execute_with_trigram(
    index: &Index,
    root_paths: &[(u32, Vec<u8>)],
    q: &Query,
    limit: usize,
    count_only: bool,
    trigram: Option<&crate::trigram::TrigramIndex>,
) -> Results {
    // Ask the accelerator which ids can possibly match. `None` = no usable
    // constraint (scan everything); `Some(empty)` = the required literals are
    // unsatisfiable, so nothing can match.
    let candidates: Option<Vec<u32>> = match trigram {
        Some(tg) if tg.is_useful() => tg.candidates(&q.trigram_plan()),
        _ => None,
    };
    if let Some(c) = &candidates {
        if c.is_empty() {
            return Results {
                hits: Vec::new(),
                total: 0,
            };
        }
    }
    match candidates {
        // Accelerated: ids are ascending, so result order matches the linear
        // scan exactly.
        Some(ids) => {
            let mut results = Results {
                hits: Vec::new(),
                total: 0,
            };
            let mut buf: Vec<u8> = Vec::with_capacity(256);
            for id in ids {
                // Guard the (impossible in practice) case of an id past the end
                // after a shrink; the scan body also rechecks liveness.
                if (id as usize) < index.len() {
                    consider(
                        index,
                        root_paths,
                        q,
                        limit,
                        count_only,
                        id,
                        &mut results,
                        &mut buf,
                    );
                }
            }
            results
        }
        None => execute_scan(index, root_paths, q, limit, count_only),
    }
}

/// Fuzzy (approximate) search (§10/§17 v2).
///
/// Each plain case-insensitive substring term (see [`Query::approx_needles`]) is
/// matched **approximately**: a name qualifies when some substring of it is
/// within `max_distance` edits of the needle. Every other term — `ext:`, `path:`,
/// glob, regex, negation and explicit `case:` — still filters *exactly*.
///
/// The score is the **sum of the per-needle edit distances**: 0 is a perfect
/// match, larger is looser. Results are sorted by `(score, path)` — the path
/// tie-break makes the order total and deterministic, so the same index and
/// query always yield the same sequence (nothing here depends on hash or
/// iteration order).
///
/// `trigram` is used only to generate a **superset** of candidates (see
/// [`crate::trigram::TrigramIndex::candidates_sharing_at_least`]); every candidate
/// is then scored for real, so the accelerator can never change the result set —
/// only the time. When the edit budget is too large for the trigram bound to say
/// anything, this scans every live entry, honestly.
///
/// Only available with the default-on `trigram` cargo feature.
#[cfg(feature = "trigram")]
pub fn execute_fuzzy(
    index: &Index,
    root_paths: &[(u32, Vec<u8>)],
    q: &Query,
    max_distance: u32,
    limit: usize,
    count_only: bool,
    trigram: Option<&crate::trigram::TrigramIndex>,
) -> Results {
    // Infallible: no abort source, no progress sink, so the `None` arm is
    // unreachable.
    execute_fuzzy_abortable(
        index,
        root_paths,
        q,
        max_distance,
        limit,
        count_only,
        trigram,
        None,
        &mut |_| true,
        false,
    )
    .expect("execute_fuzzy with no abort must complete")
}

/// Abortable form of [`execute_fuzzy`]: `abort` is polled per chunk during
/// scoring (hole 3 — fuzzy computes the whole candidate set before the first
/// emit, so write errors alone never stop it). Returns `None` when `abort`
/// fired mid-compute; the caller skips ranking *and* `Done` (a partial total
/// would lie).
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "trigram")]
pub fn execute_fuzzy_abortable(
    index: &Index,
    root_paths: &[(u32, Vec<u8>)],
    q: &Query,
    max_distance: u32,
    limit: usize,
    count_only: bool,
    trigram: Option<&crate::trigram::TrigramIndex>,
    abort: ScanAbort<'_>,
    emit: &mut dyn FnMut(ScanEvent) -> bool,
    progress: bool,
) -> Option<Results> {
    use crate::fuzzy::substring_within;

    let needles: Vec<Vec<u8>> = q.approx_needles().iter().map(|n| n.to_vec()).collect();
    // No approximate terms at all: this is just the exact query.
    if needles.is_empty() {
        return Some(match trigram {
            Some(tg) => execute_with_trigram(index, root_paths, q, limit, count_only, Some(tg)),
            None => execute_scan(index, root_paths, q, limit, count_only),
        });
    }

    // Candidate generation. Each needle contributes a *necessary* condition: a
    // name within `max_distance` of it must share enough of its trigrams. If
    // every needle can be constrained we intersect their candidate sets; if any
    // one cannot (too short, or the edit budget makes the bound vacuous) we fall
    // back to a full scan for the whole query. That keeps the generator a
    // superset filter in all cases — it may do more work than needed, never less.
    let candidates: Option<Vec<u32>> = match trigram {
        Some(tg) if tg.is_useful() => {
            let mut acc: Option<Vec<u32>> = None;
            let mut all_constrained = true;
            for n in &needles {
                let constrained = crate::trigram::TrigramIndex::literal_trigrams(n, false)
                    .and_then(|tris| {
                        let need = crate::trigram::TrigramIndex::required_shared_after_edits(
                            tris.len(),
                            max_distance,
                        );
                        tg.candidates_sharing_at_least(&tris, need)
                    });
                match constrained {
                    Some(ids) => {
                        acc = Some(match acc {
                            None => ids,
                            Some(prev) => crate::trigram::intersect(&prev, &ids),
                        });
                    }
                    None => {
                        all_constrained = false;
                        break;
                    }
                }
            }
            if all_constrained {
                acc // Some(list); empty means genuinely unsatisfiable
            } else {
                None // scan everything
            }
        }
        _ => None,
    };

    let mut scored: Vec<(u32, Vec<u8>, u32)> = Vec::new();
    let mut total = 0u64;
    let mut buf: Vec<u8> = Vec::with_capacity(256);

    let mut consider_id = |id: u32| {
        if !index.is_alive(id) {
            return;
        }
        let entry = &index.entries[id as usize];
        if entry.flags().is_root_dir() {
            return;
        }
        let name = index.name(id);
        // Exact terms still apply in full.
        let root_id = index.root_of(id);
        let root_path = root_id.and_then(|r| {
            root_paths
                .iter()
                .find(|(rid, _)| *rid == r)
                .map(|(_, p)| p.clone())
        });
        // Unattributable entries (foreign roots excluded upstream) are
        // skipped before matching, as in the plain scan (§15).
        let root_path = match root_path {
            Some(rp) => rp,
            None => return,
        };
        if !q.matches_exact_terms(
            name,
            entry.flags().is_dir(),
            entry.flags().is_exec(),
            || index.path_of(id, &root_path),
        ) {
            return;
        }
        // Approximate terms: every needle must be within budget; sum the costs.
        let mut score = 0u32;
        for n in &needles {
            match substring_within(name, n, max_distance) {
                Some(d) => score = score.saturating_add(d),
                None => return,
            }
        }
        total += 1;
        if count_only {
            return;
        }
        let path = match index.materialize_into(id, &root_path, &mut buf) {
            Some(_) => buf.clone(),
            None => return,
        };
        scored.push((score, path, id));
    };

    match &candidates {
        Some(ids) => {
            // Denominator known once generation finishes (fast); the GUI
            // shows nothing until this first call lands — documented, not
            // faked with an estimate.
            let pass_total = ids.len() as u64;
            let mut scanned = 0u64;
            if progress && !emit(ScanEvent::Progress { scanned: 0, total: pass_total }) {
                return None;
            }
            let mut since_check = 0usize;
            for &id in ids {
                if (id as usize) < index.len() {
                    consider_id(id);
                }
                scanned += 1;
                since_check += 1;
                if since_check >= ABORT_CHECK_EVERY {
                    since_check = 0;
                    if !keep_going(abort) {
                        return None;
                    }
                    if progress && !emit(ScanEvent::Progress { scanned, total: pass_total }) {
                        return None;
                    }
                }
            }
        }
        None => {
            let pass_total = index.live_count();
            let mut scanned = 0u64;
            if progress && !emit(ScanEvent::Progress { scanned: 0, total: pass_total }) {
                return None;
            }
            let mut since_check = 0usize;
            for (id, _) in index.iter_live() {
                consider_id(id);
                scanned += 1;
                since_check += 1;
                if since_check >= ABORT_CHECK_EVERY {
                    since_check = 0;
                    if !keep_going(abort) {
                        return None;
                    }
                    if progress && !emit(ScanEvent::Progress { scanned, total: pass_total }) {
                        return None;
                    }
                }
            }
        }
    }

    // Deterministic ranking: by score, then by path (bytes), then id.
    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    let hits = scored
        .into_iter()
        .take(if limit == 0 { usize::MAX } else { limit })
        .map(|(score, path, id)| Hit { id, path, score })
        .collect();
    Some(Results { hits, total })
}

/// The plain linear scan — the shared body both the unaccelerated and the
/// accelerated (`None`-candidate) paths run.
fn execute_scan(
    index: &Index,
    root_paths: &[(u32, Vec<u8>)],
    q: &Query,
    limit: usize,
    count_only: bool,
) -> Results {
    let mut results = Results {
        hits: Vec::new(),
        total: 0,
    };
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    for (id, _) in index.iter_live() {
        consider(
            index,
            root_paths,
            q,
            limit,
            count_only,
            id,
            &mut results,
            &mut buf,
        );
    }
    results
}

/// A predicate the scan engines poll to abort early: return `true` while the
/// scan should continue, `false` to stop. The daemon's socket layer wires this
/// to a zero-timeout `poll` + `MSG_PEEK` on the client fd, so a disconnected
/// client halts even a zero-hit scan — no write error is needed. `None` means
/// "never abort" (batched paths, tests); the check is one predicted branch per
/// chunk, so `None` costs nothing measurable.
///
/// Poll granularity is per-4096-entry chunk, not per entry: a syscall per
/// entry would dominate the scan (~1 µs/entry measured for the match itself).
/// 4096 entries ≈ 4 ms of scan, so the worst overshoot after a disconnect is
/// one chunk — bounded and cheap.
pub type ScanAbort<'a> = Option<&'a dyn Fn() -> bool>;

/// How often the engines poll `abort` (entries between checks).
const ABORT_CHECK_EVERY: usize = 4096;

/// Poll `abort` (if any): `true` = keep going.
#[inline]
fn keep_going(abort: ScanAbort<'_>) -> bool {
    match abort {
        None => true,
        Some(f) => f(),
    }
}

/// One event out of a streaming scan engine: either a matched hit or a
/// progress report. A single callback carries both so call sites hold one
/// `&mut` borrow, not two aliasing closures (the engine emits them
/// interleaved — Hits as found, Progress per chunk — and two `&mut` captures
/// of the same socket emitter would not compile).
///
/// `Progress` counts entries *visited* (loop iterations, including tombstones
/// and skips — not matches), so it advances steadily even on zero-hit scans;
/// `total` is the pass length known up front (live entries for linear arms,
/// candidate count for the trigram arm). The first event-adjacent call is
/// always `Progress { scanned: 0, .. }` at scan start so the client learns
/// the denominator immediately.
///
/// Refusing a `Progress` (returning `false`) aborts the scan with `None`,
/// exactly like refusing a `Hit`: a partial total would lie in `Done`. The
/// server rate-gates Progress into sparse `Response::Progress` records;
/// batched/test sinks ignore it. Progress fires at the same
/// `ABORT_CHECK_EVERY` boundaries where `abort` is polled: no extra syscalls,
/// no per-entry cost.
pub enum ScanEvent {
    Hit(Hit),
    Progress { scanned: u64, total: u64 },
}

/// Streaming form of the scan: `emit` receives each materialized hit as found
/// (up to `limit`); the returned `total` still counts every match, so the
/// caller emits the exact `Done` count after the pass. Order is index order,
/// identical to [`execute_scan`].
///
/// `emit` returns `false` to abort: the scan stops at the first refusal and
/// this returns `None` (a partial `total` would lie in `Done`, so there is
/// nothing honest to return). `abort` is polled per chunk independently of
/// hits, so even a zero-hit scan stops for a gone client — see [`ScanAbort`].
#[allow(clippy::too_many_arguments)]
pub fn execute_scan_emit(
    index: &Index,
    root_paths: &[(u32, Vec<u8>)],
    q: &Query,
    limit: usize,
    count_only: bool,
    emit: &mut dyn FnMut(ScanEvent) -> bool,
    abort: ScanAbort<'_>,
    progress: bool,
) -> Option<u64> {
    let mut total = 0u64;
    let mut emitted = 0usize;
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let mut since_check = 0usize;
    // `iter_live` yields exactly the live entries, and the scan holds the
    // read guard (no mutation mid-pass), so `live_count` is the exact pass
    // length: the final `scanned` equals it, and percent tops out at 100%.
    let pass_total = index.live_count();
    let mut scanned = 0u64;
    if progress && !emit(ScanEvent::Progress { scanned: 0, total: pass_total }) {
        return None;
    }
    for (id, _) in index.iter_live() {
        if !consider_emit(
            index, root_paths, q, limit, count_only, id, emit, &mut total, &mut emitted,
            &mut buf,
        ) {
            return None;
        }
        scanned += 1;
        since_check += 1;
        if since_check >= ABORT_CHECK_EVERY {
            since_check = 0;
            if !keep_going(abort) {
                return None;
            }
            if progress && !emit(ScanEvent::Progress { scanned, total: pass_total }) {
                return None;
            }
        }
    }
    Some(total)
}

/// Streaming form of [`execute_with_trigram`]: candidates are visited in the
/// same ascending order, hits emitted as found, exact total returned. `None`
/// trigram (or a plan with no constraint) falls through to the plain
/// streaming scan, as the batched form does.
///
/// Abort semantics are [`execute_scan_emit`]'s: the first refused hit stops
/// the scan and this returns `None`.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "trigram")]
pub fn execute_with_trigram_emit(
    index: &Index,
    root_paths: &[(u32, Vec<u8>)],
    q: &Query,
    limit: usize,
    count_only: bool,
    trigram: Option<&crate::trigram::TrigramIndex>,
    emit: &mut dyn FnMut(ScanEvent) -> bool,
    abort: ScanAbort<'_>,
    progress: bool,
) -> Option<u64> {
    let candidates: Option<Vec<u32>> = match trigram {
        Some(tg) if tg.is_useful() => tg.candidates(&q.trigram_plan()),
        _ => None,
    };
    if let Some(c) = &candidates {
        if c.is_empty() {
            return Some(0);
        }
    }
    let mut total = 0u64;
    let mut emitted = 0usize;
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    // One chunk counter across both arms: candidates are a single pass either
    // way, and the abort granularity must not depend on which arm ran.
    let mut since_check = 0usize;
    // Denominator: candidates visited on the accelerated arm (a sparse,
    // coarse-grained pass — percent jumps, honestly), live entries on the
    // fall-through arm. Announced up front via the initial `(0, total)` call.
    let pass_total: u64 = match &candidates {
        Some(ids) => ids.len() as u64,
        None => index.live_count(),
    };
    let mut scanned = 0u64;
    if progress && !emit(ScanEvent::Progress { scanned: 0, total: pass_total }) {
        return None;
    }
    // Returns `false` when the scan must stop (refused event or `abort`
    // fired). Inlined as a closure (not a helper fn) so both arms share the
    // counter.
    let mut step = |id: u32,
                    total: &mut u64,
                    emitted: &mut usize,
                    buf: &mut Vec<u8>,
                    since_check: &mut usize,
                    scanned: &mut u64| -> bool {
        if (id as usize) >= index.len() {
            return true;
        }
        if !consider_emit(
            index, root_paths, q, limit, count_only, id, emit, total, emitted, buf,
        ) {
            return false;
        }
        *scanned += 1;
        *since_check += 1;
        if *since_check >= ABORT_CHECK_EVERY {
            *since_check = 0;
            if !keep_going(abort) {
                return false;
            }
            if progress && !emit(ScanEvent::Progress { scanned: *scanned, total: pass_total }) {
                return false;
            }
        }
        true
    };
    match candidates {
        Some(ids) => {
            for id in ids {
                if !step(id, &mut total, &mut emitted, &mut buf, &mut since_check, &mut scanned)
                {
                    return None;
                }
            }
        }
        None => {
            for (id, _) in index.iter_live() {
                if !step(id, &mut total, &mut emitted, &mut buf, &mut since_check, &mut scanned)
                {
                    return None;
                }
            }
        }
    }
    Some(total)
}

/// Evaluate one candidate id and fold it into `results`. Shared by the
/// accelerated path and the linear scan so the two can never drift.
#[allow(clippy::too_many_arguments)]
fn consider(
    index: &Index,
    root_paths: &[(u32, Vec<u8>)],
    q: &Query,
    limit: usize,
    count_only: bool,
    id: u32,
    results: &mut Results,
    buf: &mut Vec<u8>,
) {
    // `emitted` is scratch for the streaming cap below; `consider` counts
    // it but only ever emits into `results.hits`, so it stays in sync.
    // The infallible push can never refuse, so the abort arm is unreachable.
    let mut emitted = results.hits.len();
    let kept = consider_emit(
        index,
        root_paths,
        q,
        limit,
        count_only,
        id,
        &mut |ev| {
            if let ScanEvent::Hit(hit) = ev {
                results.hits.push(hit);
            }
            true
        },
        &mut results.total,
        &mut emitted,
        buf,
    );
    debug_assert!(kept, "push-only emit must never refuse");
}

/// Streaming form of [`consider`]: matched-and-materialized hits are handed to
/// `emit` as found (up to `limit`), while `total` still counts every match.
/// The scan order is identical to [`consider`], so a streamed reply carries
/// exactly the hits the batched reply would — only earlier. Returns `false`
/// when `emit` refused the hit (the caller stops the scan); every other
/// path returns `true`, including skips, the count-only sink, and the limit
/// cap — none of those is an abort. `emit` runs while
/// the caller holds the index read lock: it must write+flush promptly, never
/// block on anything but the client socket.
#[allow(clippy::too_many_arguments)]
fn consider_emit(
    index: &Index,
    root_paths: &[(u32, Vec<u8>)],
    q: &Query,
    limit: usize,
    count_only: bool,
    id: u32,
    emit: &mut dyn FnMut(ScanEvent) -> bool,
    total: &mut u64,
    emitted: &mut usize,
    buf: &mut Vec<u8>,
) -> bool {
    // Liveness is the accelerator's one blind spot: postings built before a
    // deletion still name the dead id. The authoritative scan skips tombstones,
    // so this must too, or a stale posting becomes a phantom hit.
    if !index.is_alive(id) {
        return true;
    }
    let entry = &index.entries[id as usize];
    if entry.flags().is_root_dir() {
        return true;
    }
    let name = index.name(id);
    if q.ac.is_some() {
        // The automaton is built with `ascii_case_insensitive(true)`, so it
        // folds internally: feeding it the raw name is correct, and the old
        // per-entry lowercased copy was pure waste (same ~1 µs/entry cost as
        // `contains`, paid on every entry even when the prefilter rejects).
        if !q.prefilter(name) {
            return true;
        }
    }
    let root_id = index.root_of(id);
    let root_path = root_id.and_then(|r| {
        root_paths
            .iter()
            .find(|(rid, _)| *rid == r)
            .map(|(_, p)| p.clone())
    });
    // No root in the table (foreign roots excluded upstream, or orphans):
    // unattributable. A bare basename is not a valid path, and counting it
    // would leak foreign files into totals (§15) — skip before matching.
    let root_path = match root_path {
        Some(rp) => rp,
        None => return true,
    };
    let matched = q.matches(
        name,
        entry.flags().is_dir(),
        entry.flags().is_exec(),
        || index.path_of(id, &root_path),
    );
    if !matched {
        return true;
    }
    // NOTE: no ownership filter here. `visible_chain` takes the index read
    // lock, and this runs under the scan's held read guard — nesting it
    // self-deadlocks against a pending applier write (write-preferring
    // RwLock). System mode collects, drops guards, and filters guard-free in
    // the server layer instead (same TOCTOU as the batched path).
    *total += 1;
    if count_only {
        return true;
    }
    // The cap counts materialized hits, not matches: entries that fail
    // materialization (tombstoned mid-scan, orphaned chains) must not consume
    // it, exactly as in the batched path.
    if limit != 0 && *emitted >= limit {
        return true;
    }
    let path = match index.materialize_into(id, &root_path, buf) {
        Some(_) => buf.clone(),
        None => return true,
    };
    *emitted += 1;
    emit(ScanEvent::Hit(Hit { id, path, score: 0 }))
}

#[derive(Clone, Debug, Default)]
pub struct Results {
    pub hits: Vec<Hit>,
    /// Total matches, even when truncated by `limit` or `count_only`.
    pub total: u64,
}

/// Run a Watchman-style [`crate::expr::Expr`] over `index`.
///
/// `rel_root` scopes the search to a subtree (the `relative_root` parameter) and
/// is a path relative to the root prefix (empty = the whole root). Candidate
/// paths are built relative to `root_path` with `rel_root` prepended as the
/// anchor; a candidate not under `rel_root` is skipped. The `MatchCtx::rel_path`
/// handed to the expression is relative to `rel_root` (so `name ... wholename`
/// and `dirname` see the same names Watchman would).
pub fn execute_expr(
    index: &Index,
    root_paths: &[(u32, Vec<u8>)],
    rel_root: &[u8],
    expr: &crate::expr::Expr,
    limit: usize,
    count_only: bool,
) -> Results {
    use crate::expr::MatchCtx;

    let mut results = Results {
        hits: Vec::new(),
        total: 0,
    };
    let mut buf: Vec<u8> = Vec::with_capacity(256);

    // The anchor path we materialize against is `root_path/rel_root`; the
    // reported name is then everything after it.
    for (id, entry) in index.iter_live() {
        if entry.flags().is_root_dir() {
            continue;
        }
        let Some(root_id) = index.root_of(id) else {
            continue;
        };
        let Some((_, root_path)) = root_paths.iter().find(|(rid, _)| *rid == root_id) else {
            continue;
        };

        // Absolute path of the candidate under its root.
        buf.clear();
        let Some(abs) = index.path_of(id, root_path) else {
            continue;
        };

        // Anchor = root_path + '/' + rel_root.
        let mut anchor: Vec<u8> = root_path.clone();
        if !rel_root.is_empty() {
            if anchor.last() != Some(&b'/') {
                anchor.push(b'/');
            }
            anchor.extend_from_slice(rel_root);
        }
        // Must be under the anchor.
        let rel = if anchor.is_empty() {
            abs.as_slice()
        } else if abs.len() > anchor.len() && abs.starts_with(&anchor) && abs[anchor.len()] == b'/'
        {
            &abs[anchor.len() + 1..]
        } else {
            continue;
        };

        let ctx = MatchCtx {
            name: index.name(id),
            rel_path: rel,
            is_dir: entry.flags().is_dir(),
            is_symlink: entry.flags().is_symlink(),
            is_exec: entry.flags().is_exec(),
            // v1 does not carry sizes or timestamps; `size`/`empty`/`since`
            // terms see `None`.
            size: None,
            otime: None,
            ctime: None,
            mtime: None,
            fs_case_insensitive: false,
        };
        if !expr.eval(&ctx) {
            continue;
        }
        results.total += 1;
        if count_only {
            continue;
        }
        if limit != 0 && results.hits.len() >= limit {
            continue;
        }
        // Report the path exactly as `abs` (root-anchored), matching the string
        // query path's behaviour.
        results.hits.push(Hit {
            id,
            path: abs,
            score: 0,
        });
    }
    results
}

/// Streaming form of [`execute_expr`]: hits emitted as found (up to `limit`),
/// exact total returned. Same visit order and same skip rules, so the streamed
/// hits are exactly the batched reply's hits.
///
/// Abort semantics are [`execute_scan_emit`]'s: the first refused hit stops
/// the scan and this returns `None`.
#[allow(clippy::too_many_arguments)]
pub fn execute_expr_emit(
    index: &Index,
    root_paths: &[(u32, Vec<u8>)],
    rel_root: &[u8],
    expr: &crate::expr::Expr,
    limit: usize,
    count_only: bool,
    emit: &mut dyn FnMut(ScanEvent) -> bool,
    abort: ScanAbort<'_>,
    progress: bool,
) -> Option<u64> {
    use crate::expr::MatchCtx;

    let mut total = 0u64;
    let mut emitted = 0usize;
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let mut since_check = 0usize;
    let pass_total = index.live_count();
    let mut scanned = 0u64;
    if progress && !emit(ScanEvent::Progress { scanned: 0, total: pass_total }) {
        return None;
    }

    for (id, entry) in index.iter_live() {
        scanned += 1;
        // Poll per chunk regardless of matching: a zero-hit scan must abort
        // too — no writes means no write errors to notice.
        since_check += 1;
        if since_check >= ABORT_CHECK_EVERY {
            since_check = 0;
            if !keep_going(abort) {
                return None;
            }
            if progress && !emit(ScanEvent::Progress { scanned, total: pass_total }) {
                return None;
            }
        }
        if entry.flags().is_root_dir() {
            continue;
        }
        let Some(root_id) = index.root_of(id) else {
            continue;
        };
        let Some((_, root_path)) = root_paths.iter().find(|(rid, _)| *rid == root_id) else {
            continue;
        };

        buf.clear();
        let Some(abs) = index.path_of(id, root_path) else {
            continue;
        };

        let mut anchor: Vec<u8> = root_path.clone();
        if !rel_root.is_empty() {
            if anchor.last() != Some(&b'/') {
                anchor.push(b'/');
            }
            anchor.extend_from_slice(rel_root);
        }
        let rel = if anchor.is_empty() {
            abs.as_slice()
        } else if abs.len() > anchor.len() && abs.starts_with(&anchor) && abs[anchor.len()] == b'/'
        {
            &abs[anchor.len() + 1..]
        } else {
            continue;
        };

        let ctx = MatchCtx {
            name: index.name(id),
            rel_path: rel,
            is_dir: entry.flags().is_dir(),
            is_symlink: entry.flags().is_symlink(),
            is_exec: entry.flags().is_exec(),
            size: None,
            otime: None,
            ctime: None,
            mtime: None,
            fs_case_insensitive: false,
        };
        if !expr.eval(&ctx) {
            continue;
        }
        total += 1;
        if count_only {
            continue;
        }
        if limit != 0 && emitted >= limit {
            continue;
        }
        emitted += 1;
        if !emit(ScanEvent::Hit(Hit {
            id,
            path: abs,
            score: 0,
        })) {
            return None;
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{EntryFlags, ROOT_ID};
    #[cfg(feature = "trigram")]
    use crate::trigram::TrigramIndex;

    fn tree() -> (Index, Vec<(u32, Vec<u8>)>) {
        let mut ix = Index::new();
        let r = ix.insert(
            ROOT_ID,
            b"proj",
            EntryFlags::EMPTY.with_dir(true).with_root_dir(true),
        );
        let src = ix.insert(r, b"src", EntryFlags::EMPTY.with_dir(true));
        ix.insert(src, b"main.rs", EntryFlags::EMPTY);
        ix.insert(src, b"lib.rs", EntryFlags::EMPTY);
        ix.insert(r, b"README.md", EntryFlags::EMPTY);
        ix.insert(r, b"Notes.TXT", EntryFlags::EMPTY);
        let root_path = b"/home/me/proj".to_vec();
        (ix, vec![(r, root_path)])
    }

    #[test]
    fn compile_with_case_forces_case_sensitivity() {
        let (ix, roots) = tree();
        // Smart-case: a lowercase term matches case-insensitively, so "notes"
        // finds Notes.TXT.
        let q = Query::compile("notes").unwrap();
        let res = execute(&ix, &roots, &q, 100, false);
        assert_eq!(paths(&res), vec!["/home/me/proj/Notes.TXT"]);

        // Forcing case (the protocol's case_sensitive field) must not.
        let q = Query::compile_with_case("notes", true).unwrap();
        let res = execute(&ix, &roots, &q, 100, false);
        assert!(paths(&res).is_empty(), "{:?}", paths(&res));

        // Uppercase already matches case-sensitively either way.
        let q = Query::compile_with_case("Notes", true).unwrap();
        let res = execute(&ix, &roots, &q, 100, false);
        assert_eq!(paths(&res), vec!["/home/me/proj/Notes.TXT"]);
    }

    #[test]
    fn compile_with_case_applies_to_modifiers_and_negation() {
        let (ix, roots) = tree();
        // ext: is case-insensitive by default, so ext:txt finds Notes.TXT.
        let q = Query::compile_with_case("ext:rs", false).unwrap();
        let res = execute(&ix, &roots, &q, 100, false);
        assert_eq!(paths(&res).len(), 2);

        // Forced case on a negated term still applies to the inner term.
        let q = Query::compile_with_case("-main ext:rs", true).unwrap();
        let res = execute(&ix, &roots, &q, 100, false);
        assert_eq!(paths(&res), vec!["/home/me/proj/src/lib.rs"]);
    }

    fn paths(res: &Results) -> Vec<String> {
        res.hits
            .iter()
            .map(|h| String::from_utf8_lossy(&h.path).into_owned())
            .collect()
    }

    #[test]
    fn plain_substring_ci() {
        let (ix, rp) = tree();
        let q = Query::compile("rs").unwrap();
        let res = execute(&ix, &rp, &q, 0, false);
        assert_eq!(res.total, 2);
        let p = paths(&res);
        assert!(p.contains(&"/home/me/proj/src/main.rs".to_string()));
        assert!(p.contains(&"/home/me/proj/src/lib.rs".to_string()));
    }

    #[test]
    fn smart_case() {
        let (ix, rp) = tree();
        // lowercase "notes" matches "Notes.TXT"
        let q = Query::compile("notes").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 1);
        // uppercase "NO" matches nothing
        let q = Query::compile("NO").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 0);
        // "case:Notes" is explicitly case-sensitive
        let q = Query::compile("case:Notes").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 1);
    }

    #[test]
    fn ext_modifier() {
        let (ix, rp) = tree();
        let q = Query::compile("ext:rs").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 2);
        let q = Query::compile("ext:TXT").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 1);
        let q = Query::compile("ext:nope").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 0);
    }

    #[test]
    fn path_modifier() {
        let (ix, rp) = tree();
        let q = Query::compile("path:src").unwrap();
        // the `src` directory itself, plus main.rs and lib.rs
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 3);
        let q = Query::compile("path:proj").unwrap();
        // everything under /home/me/proj except the root dir itself
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 5);
    }

    #[test]
    fn type_modifier() {
        let (ix, rp) = tree();
        let q = Query::compile("type:dir").unwrap();
        let res = execute(&ix, &rp, &q, 0, false);
        assert_eq!(res.total, 1);
        assert_eq!(paths(&res), vec!["/home/me/proj/src".to_string()]);
        // Explicit `file` and negation both select the four files.
        for raw in ["type:file", "-type:dir"] {
            let q = Query::compile(raw).unwrap();
            let res = execute(&ix, &rp, &q, 0, false);
            assert_eq!(res.total, 4, "{raw}");
        }
        // Composes with other terms.
        let q = Query::compile("type:dir src").unwrap();
        let res = execute(&ix, &rp, &q, 0, false);
        assert_eq!(paths(&res), vec!["/home/me/proj/src".to_string()]);
        // Anything else is a usage error, not a silent substring.
        assert!(Query::compile("type:x").is_err());
        assert!(Query::compile("type:").is_err());
    }

    #[test]
    #[cfg(feature = "trigram")]
    fn type_dir_survives_the_fuzzy_path() {
        // The fuzzy engine evaluates exact terms separately: `type:dir`
        // must filter there too, not just in the plain scan.
        let (ix, rp) = tree();
        let tg = TrigramIndex::build(&ix);
        let q = Query::compile("type:dir").unwrap();
        let res = execute_fuzzy(&ix, &rp, &q, 1, 0, false, Some(&tg));
        assert_eq!(res.total, 1);
        assert_eq!(res.hits[0].path, b"/home/me/proj/src".to_vec());
    }

    #[test]
    fn glob_wildcards() {
        let (ix, rp) = tree();
        let q = Query::compile("*.rs").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 2);
        let q = Query::compile("main.*").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 1);
        let q = Query::compile("?ib.rs").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 1);
    }

    #[test]
    fn regex_modifier() {
        let (ix, rp) = tree();
        let q = Query::compile("regex:^m.*rs$").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 1);
        assert!(Query::compile("regex:[").is_err());
    }

    #[test]
    fn phrases_and_negation() {
        let (ix, rp) = tree();
        let q = Query::compile("\"README.md\"").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 1);
        let q = Query::compile("rs -main").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 1);
        assert_eq!(
            paths(&execute(&ix, &rp, &q, 0, false))[0],
            "/home/me/proj/src/lib.rs"
        );
    }

    #[test]
    fn count_only_skips_materialization() {
        let (ix, rp) = tree();
        let q = Query::compile("rs").unwrap();
        let res = execute(&ix, &rp, &q, 0, true);
        assert_eq!(res.total, 2);
        assert!(res.hits.is_empty());
    }

    #[test]
    fn limit_counts_but_truncates() {
        let (ix, rp) = tree();
        let q = Query::compile("rs").unwrap();
        let res = execute(&ix, &rp, &q, 1, false);
        assert_eq!(res.total, 2);
        assert_eq!(res.hits.len(), 1);
    }

    #[test]
    fn empty_query_matches_everything() {
        let (ix, rp) = tree();
        let q = Query::compile("").unwrap();
        assert!(q.is_empty);
        // all entries except the root dir itself: src, main.rs, lib.rs,
        // README.md, Notes.TXT
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 5);
    }

    #[test]
    fn glob_unit() {
        assert!(GlobPattern::parse(b"*.rs", false).matches(b"a.rs"));
        assert!(!GlobPattern::parse(b"*.rs", false).matches(b"a.rss"));
        assert!(GlobPattern::parse(b"a*c", false).matches(b"abc"));
        assert!(GlobPattern::parse(b"a*c", false).matches(b"ac"));
        assert!(GlobPattern::parse(b"*", false).matches(b"anything"));
        assert!(GlobPattern::parse(b"a?c", false).matches(b"abc"));
        assert!(!GlobPattern::parse(b"a?c", false).matches(b"ac"));
        assert!(GlobPattern::parse(b"*.RS", false).matches(b"x.rs"));
    }

    #[test]
    fn contains_ci_matches_lowercased_memmem() {
        // Differential: the allocation-free path must agree byte-for-byte
        // with the old lowercase-then-memmem semantics, across cases the
        // optimizer in `contains_ci` treats specially.
        fn reference(hay: &[u8], needle: &[u8]) -> bool {
            let lower: Vec<u8> = hay.iter().map(|c| c.to_ascii_lowercase()).collect();
            let n: Vec<u8> = needle.iter().map(|c| c.to_ascii_lowercase()).collect();
            memchr::memmem::find(&lower, &n).is_some()
        }
        let hays: &[&[u8]] = &[&b""[..],
            &b"e"[..],
            &b"E"[..],
            &b"main.rs"[..],
            &b"MAIN.RS"[..],
            &b"mAiN.rS"[..],
            &b"eeeEEEeee"[..],
            &b"no match here"[..],
            &b"a"[..],
            &b"ab"[..],
            &b"abc"[..],
            &b"trailing E"[..],
            &b"E leading"[..],
            &b"\xc3\xa9e\xc3\x89"[..],
            &b"123!@#"[..],
            &b"EeEeEeEeEeEeEeEeEeEe"[..],
            &b"longer haystack with e buried at the very end E"[..]];
        let needles: &[&[u8]] = &[&b"e"[..],
            &b"E"[..],
            &b"main"[..],
            &b"MAIN"[..],
            &b"mAiN"[..],
            &b".rs"[..],
            &b".RS"[..],
            &b"xyz"[..],
            &b"a"[..],
            &b"ab"[..],
            &b"abc"[..],
            &b"abcd"[..],
            &b"\xc3\xa9"[..],
            &b"\xc3\x89"[..],
            &b"1"[..],
            &b"!"[..],
            &b"EEEE"[..],
            &b"eEe"[..],
            &b"end E"[..]];
        for h in hays {
            for n in needles {
                assert_eq!(
                    super::contains_ci(h, n),
                    reference(h, n),
                    "hay={h:?} needle={n:?}"
                );
                // And `contains` routes correctly on both case flags.
                assert_eq!(
                    super::contains(h, n, false),
                    reference(h, n),
                    "contains-ci hay={h:?} needle={n:?}"
                );
            }
        }
    }

    #[test]
    fn streamed_hits_equal_batched_hits() {
        // Result-identity for the streaming engine: same queries, same limits,
        // streamed hits must be byte-identical to the batched reply's hits,
        // and the streamed total must equal the batched total. This pins the
        // contract the wire change depends on.
        use crate::index::{EntryFlags, ROOT_ID};
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"root", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        let sub = ix.insert(r, b"src", EntryFlags::EMPTY.with_dir(true));
        for name in [
            "main.rs", "lib.rs", "main_test.rs", "README.md", "notes.txt",
            "MAIN.RS", "m.txt", "e.rs", "eee.md", "nope.py",
        ] {
            ix.insert(sub, name.as_bytes(), EntryFlags::EMPTY);
        }
        let roots = vec![(r, b"/root".to_vec())];
        for raw in ["e", "E", "main", "MAIN", "rs", "ext:rs", "ext:md", "zzz", "a e"] {
            let q = Query::compile(raw).unwrap();
            for limit in [0usize, 1, 2, 5, 200] {
                let batched = execute(&ix, &roots, &q, limit, false);
                let mut streamed_hits = Vec::new();
                let total = execute_scan_emit(
                    &ix,
                    &roots,
                    &q,
                    limit,
                    false,
                    &mut |ev| {
                        if let ScanEvent::Hit(h) = ev {
                            streamed_hits.push(h);
                        }
                        true
                    },
                    None,
                    false,
                )
                .expect("infallible test emit must not abort");
                assert_eq!(total, batched.total, "total raw={raw:?} limit={limit}");
                assert_eq!(streamed_hits, batched.hits, "hits raw={raw:?} limit={limit}");
                // Accelerated form agrees too (same order, same hits).
                #[cfg(feature = "trigram")]
                {
                    let tg = crate::trigram::TrigramIndex::build(&ix);
                    let mut accel_hits = Vec::new();
                    let atotal = execute_with_trigram_emit(
                        &ix,
                        &roots,
                        &q,
                        limit,
                        false,
                        Some(&tg),
                        &mut |ev| {
                            if let ScanEvent::Hit(h) = ev {
                                accel_hits.push(h);
                            }
                            true
                        },
                        None,
                        false,
                    )
                    .expect("infallible test emit must not abort");
                    assert_eq!(atotal, batched.total, "accel total raw={raw:?} limit={limit}");
                    assert_eq!(accel_hits, batched.hits, "accel hits raw={raw:?} limit={limit}");
                }
            }
        }
    }

    #[test]
    fn progress_reports_scan_fraction_then_completes() {
        // The progress contract: first call is (0, total) announcing the
        // denominator, `scanned` never decreases, and the scan's hits/total
        // are identical with or without the callback.
        use crate::index::{EntryFlags, ROOT_ID};
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"root", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        let sub = ix.insert(r, b"src", EntryFlags::EMPTY.with_dir(true));
        for i in 0..10_000 {
            ix.insert(sub, format!("file_{i:05}.txt").as_bytes(), EntryFlags::EMPTY);
        }
        let roots = vec![(r, b"/root".to_vec())];
        let q = Query::compile("txt").unwrap();
        let plain = execute(&ix, &roots, &q, 0, false);

        let mut calls: Vec<(u64, u64)> = Vec::new();
        let mut streamed_hits = Vec::new();
        let total = execute_scan_emit(
            &ix,
            &roots,
            &q,
            0,
            false,
            &mut |ev| match ev {
                ScanEvent::Hit(h) => {
                    streamed_hits.push(h);
                    true
                }
                ScanEvent::Progress { scanned, total } => {
                    calls.push((scanned, total));
                    true
                }
            },
            None,
            true,
        )
        .expect("progress must not abort an accepted scan");
        assert_eq!(total, plain.total);
        assert_eq!(streamed_hits, plain.hits);
        assert!(!calls.is_empty(), "a 10k scan must report progress");
        assert_eq!(calls[0], (0, ix.live_count()), "denominator announced first");
        for w in calls.windows(2) {
            assert!(w[1].0 >= w[0].0, "scanned never decreases: {calls:?}");
            assert_eq!(w[1].1, w[0].1, "denominator stable mid-scan: {calls:?}");
        }
        assert_eq!(calls.last().unwrap().1, ix.live_count());
    }

    #[test]
    fn progress_refusal_aborts_with_no_total() {
        // A `false` from the progress callback aborts like an emit refusal:
        // `None` out, no `Done`-worthy total.
        use crate::index::{EntryFlags, ROOT_ID};
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"root", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        let sub = ix.insert(r, b"src", EntryFlags::EMPTY.with_dir(true));
        for i in 0..10_000 {
            ix.insert(sub, format!("file_{i:05}.txt").as_bytes(), EntryFlags::EMPTY);
        }
        let roots = vec![(r, b"/root".to_vec())];
        let q = Query::compile("txt").unwrap();
        // Refuse the initial (0, total) announcement: the scan never starts.
        let got = execute_scan_emit(
            &ix,
            &roots,
            &q,
            0,
            false,
            &mut |ev| !matches!(ev, ScanEvent::Progress { .. }),
            None,
            true,
        );
        assert_eq!(got, None, "a refused progress report must abort with no total");
    }

    #[test]
    fn emit_refusal_aborts_the_scan() {
        // A `false` from `emit` stops the scan: this returns `None` (a
        // partial `total` would lie in `Done`), and only the hits before the
        // refusal were delivered. All three streaming engines share the
        // contract.
        use crate::index::{EntryFlags, ROOT_ID};
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"root", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        let sub = ix.insert(r, b"src", EntryFlags::EMPTY.with_dir(true));
        for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
            ix.insert(sub, name.as_bytes(), EntryFlags::EMPTY);
        }
        let roots = vec![(r, b"/root".to_vec())];
        let q = Query::compile("txt").unwrap();
        assert_eq!(execute(&ix, &roots, &q, 0, false).total, 5);

        // Refuse the third hit: two delivered, then `None`.
        let mut seen = 0usize;
        let got = execute_scan_emit(
            &ix,
            &roots,
            &q,
            0,
            false,
            &mut |ev| {
                if matches!(ev, ScanEvent::Hit(_)) {
                    seen += 1;
                    return seen < 3;
                }
                true
            },
            None,
            false,
        );
        assert_eq!(got, None, "a refused emit must abort with no total");
        assert_eq!(seen, 3, "scan stops at the refusal, not at the end");

        // Refuse immediately: nothing delivered, still `None`.
        let mut seen0 = 0usize;
        let got0 = execute_scan_emit(
            &ix,
            &roots,
            &q,
            0,
            false,
            &mut |ev| {
                if matches!(ev, ScanEvent::Hit(_)) {
                    seen0 += 1;
                    return false;
                }
                true
            },
            None,
            false,
        );
        assert_eq!(got0, None);
        assert_eq!(seen0, 1);

        // The accelerated engine aborts the same way.
        #[cfg(feature = "trigram")]
        {
            let tg = crate::trigram::TrigramIndex::build(&ix);
            let mut aseen = 0usize;
            let agot = execute_with_trigram_emit(
                &ix,
                &roots,
                &q,
                0,
                false,
                Some(&tg),
                &mut |ev| {
                    if matches!(ev, ScanEvent::Hit(_)) {
                        aseen += 1;
                        return aseen < 2;
                    }
                    true
                },
                None,
                false,
            );
            assert_eq!(agot, None);
            assert_eq!(aseen, 2, "accelerated scan stops at the refusal too");
        }
    }

    #[test]
    fn expr_emit_refusal_aborts_the_scan() {
        // Same contract through the expression engine: refuse the second
        // hit, get `None`, and the scan never visits the rest.
        use crate::index::{EntryFlags, ROOT_ID};
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"root", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        let sub = ix.insert(r, b"src", EntryFlags::EMPTY.with_dir(true));
        for name in ["a.rs", "b.rs", "c.rs", "d.rs"] {
            ix.insert(sub, name.as_bytes(), EntryFlags::EMPTY);
        }
        let roots = vec![(r, b"/root".to_vec())];
        let expr =
            crate::expr::parse_query(&serde_json::json!({"expression": ["suffix", "rs"]})).unwrap();
        let mut seen = 0usize;
        let got = execute_expr_emit(
            &ix,
            &roots,
            b"",
            &expr,
            0,
            false,
            &mut |ev| {
                if matches!(ev, ScanEvent::Hit(_)) {
                    seen += 1;
                    return seen < 2;
                }
                true
            },
            None,
            false,
        );
        assert_eq!(got, None, "a refused emit must abort with no total");
        assert_eq!(seen, 2);
    }

    #[test]
    fn abort_poll_stops_a_zero_hit_scan() {
        // Hole 1: a query matching nothing emits nothing, so no write ever
        // fails. The `abort` poll is the only thing that stops it. Build an
        // index large enough to span several poll chunks (> 4096 entries),
        // arm the poll to fire immediately, and prove the scan returns `None`
        // without visiting everything.
        use crate::index::{EntryFlags, ROOT_ID};
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"root", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        let sub = ix.insert(r, b"src", EntryFlags::EMPTY.with_dir(true));
        for i in 0..10_000 {
            ix.insert(sub, format!("file_{i:05}.txt").as_bytes(), EntryFlags::EMPTY);
        }
        let roots = vec![(r, b"/root".to_vec())];
        // Matches nothing: every entry is visited, none emitted.
        let q = Query::compile("zzz-no-such-file").unwrap();
        assert_eq!(execute(&ix, &roots, &q, 0, false).total, 0);

        // Poll aborts immediately: `None`, and the emit never ran.
        let mut emitted = 0usize;
        let got = execute_scan_emit(
            &ix,
            &roots,
            &q,
            0,
            false,
            &mut |ev| {
                if matches!(ev, ScanEvent::Hit(_)) {
                    emitted += 1;
                }
                true
            },
            Some(&|| false),
            false,
        );
        assert_eq!(got, None, "an abort poll must stop even a zero-hit scan");
        assert_eq!(emitted, 0, "zero hits, zero emits — the poll did the work");

        // Same through the expr engine.
        let expr =
            crate::expr::parse_query(&serde_json::json!({"expression": ["suffix", "zzz"]}))
                .unwrap();
        let got2 = execute_expr_emit(
            &ix,
            &roots,
            b"",
            &expr,
            0,
            false,
            &mut |_| true,
            Some(&|| false),
            false,
        );
        assert_eq!(got2, None);
    }

    #[test]
    #[cfg(feature = "trigram")]
    fn abort_poll_stops_fuzzy_mid_compute() {
        // Hole 3: fuzzy scores the whole candidate set before the first emit.
        // The poll must live in compute. Same 10k-entry index, fuzzy query
        // with real candidates, poll fires on the first chunk.
        use crate::index::{EntryFlags, ROOT_ID};
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"root", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        let sub = ix.insert(r, b"src", EntryFlags::EMPTY.with_dir(true));
        for i in 0..10_000 {
            ix.insert(sub, format!("file_{i:05}.txt").as_bytes(), EntryFlags::EMPTY);
        }
        let roots = vec![(r, b"/root".to_vec())];
        let tg = crate::trigram::TrigramIndex::build(&ix);
        let q = Query::compile("file").unwrap();
        // Sanity: without abort this finds everything.
        let full = execute_fuzzy(&ix, &roots, &q, 1, 0, false, Some(&tg));
        assert!(full.total > 9000, "sanity: fuzzy finds the corpus");

        let got = execute_fuzzy_abortable(
            &ix,
            &roots,
            &q,
            1,
            0,
            false,
            Some(&tg),
            Some(&|| false),
            &mut |_| true,
            false,
        );
        assert!(got.is_none(), "fuzzy compute must abort on the poll");
    }

    #[test]
    fn contains_ci_empty_needle_is_handled_by_caller() {
        // `contains` short-circuits empty needles to true; `contains_ci`
        // itself takes non-empty (debug-asserted) and must still not panic.
        assert!(super::contains(b"abc", b"", false));
        assert!(super::contains(b"", b"", false));
        assert!(!super::contains(b"", b"e", false));
    }

    #[test]
    fn required_literals_exclude_negation_glob_regex_path() {
        // Positive substrings and `ext:` are required; everything else is not.
        let q = Query::compile("main rs -skip *.tmp regex:^x ext:md path:src").unwrap();
        let lits: Vec<(String, bool)> = q
            .required_literals()
            .into_iter()
            .map(|(n, c)| (String::from_utf8_lossy(n).into_owned(), c))
            .collect();
        assert_eq!(
            lits,
            vec![
                ("main".to_string(), false),
                ("rs".to_string(), false),
                ("md".to_string(), false),
            ],
            "only positive substring/ext literals are required"
        );
    }

    #[test]
    fn required_literals_preserve_case_flag() {
        // `case:Notes` is an explicitly case-sensitive substring.
        let q = Query::compile("case:Notes").unwrap();
        let lits = q.required_literals();
        assert_eq!(lits.len(), 1);
        assert_eq!(lits[0].0, b"Notes");
        assert!(lits[0].1, "smart/explicit case must be preserved");
    }

    #[cfg(feature = "trigram")]
    #[test]
    fn trigram_plan_drops_short_and_case_sensitive() {
        // "rs" is 2 bytes -> no trigram; "Notes" is case-sensitive -> no
        // trigram; "main" survives.
        let q = Query::compile("main rs case:Notes").unwrap();
        let plan = q.trigram_plan();
        assert_eq!(plan.literals.len(), 1, "only `main` yields usable trigrams");
        // "main" -> {"mai", "ain"}, sorted.
        let mut expected: Vec<[u8; 3]> = vec![*b"mai", *b"ain"];
        expected.sort_unstable();
        assert_eq!(plan.literals[0], expected);

        // A query with nothing usable yields an empty plan.
        let q = Query::compile("ab cd").unwrap();
        assert!(q.trigram_plan().is_empty());
    }

    #[cfg(feature = "trigram")]
    #[test]
    fn fuzzy_finds_a_typo_that_exact_does_not() {
        let (ix, rp) = tree();
        let tg = TrigramIndex::build(&ix);
        // "main.js" is one substitution from "main.rs": exact finds nothing...
        let q = Query::compile("main.js").unwrap();
        assert_eq!(execute(&ix, &rp, &q, 0, false).total, 0);
        // ...fuzzy at distance 1 finds it.
        let res = execute_fuzzy(&ix, &rp, &q, 1, 0, false, Some(&tg));
        assert_eq!(res.total, 1);
        assert_eq!(res.hits[0].path, b"/home/me/proj/src/main.rs".to_vec());
        assert_eq!(res.hits[0].score, 1);
    }

    #[cfg(feature = "trigram")]
    #[test]
    fn zero_distance_fuzzy_equals_exact_scan() {
        // With k=0 fuzzy selects exactly the same entries as the plain scan.
        // The *order* differs by design: fuzzy is a ranked result list (by
        // score, then path) while a plain query streams in index order, so
        // compare as sets, not sequences.
        let (ix, rp) = tree();
        let tg = TrigramIndex::build(&ix);
        for raw in ["rs", "main", "ext:rs", "path:src"] {
            let q = Query::compile(raw).unwrap();
            let exact = execute(&ix, &rp, &q, 0, false);
            let fuzzy = execute_fuzzy(&ix, &rp, &q, 0, 0, false, Some(&tg));
            let mut a: Vec<u32> = exact.hits.iter().map(|h| h.id).collect();
            let mut b: Vec<u32> = fuzzy.hits.iter().map(|h| h.id).collect();
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "q={raw:?}: k=0 must select the same entries as exact");
            assert_eq!(exact.total, fuzzy.total, "q={raw:?}");
            assert!(fuzzy.hits.iter().all(|h| h.score == 0), "k=0 hits score 0");
        }
    }

    #[cfg(feature = "trigram")]
    #[test]
    fn fuzzy_ranking_is_by_score_then_path() {
        let (ix, rp) = tree();
        let tg = TrigramIndex::build(&ix);
        // "main.rs" scores 0; "main.js" scores 1 but is not in the tree, so pick a
        // query that hits both main.rs and lib.rs at different distances.
        let q = Query::compile("lib.rs").unwrap();
        let res = execute_fuzzy(&ix, &rp, &q, 2, 0, false, Some(&tg));
        // lib.rs is exact (0); main.rs is further but within 2.
        assert!(!res.hits.is_empty());
        assert_eq!(res.hits[0].path, b"/home/me/proj/src/lib.rs".to_vec());
        assert_eq!(res.hits[0].score, 0);
        // Scores are non-decreasing down the list.
        for w in res.hits.windows(2) {
            assert!(w[0].score <= w[1].score, "ranking must be by score");
        }
    }

    #[cfg(feature = "trigram")]
    #[test]
    fn fuzzy_respects_exact_terms() {
        let (ix, rp) = tree();
        let tg = TrigramIndex::build(&ix);
        // "main.js ext:rs" -> the ext: term is exact, so main.rs qualifies (its
        // extension really is rs) ...
        let q = Query::compile("main.js ext:rs").unwrap();
        assert_eq!(execute_fuzzy(&ix, &rp, &q, 1, 0, false, Some(&tg)).total, 1);
        // ... but ext:md does not, even though the name is only 1 edit away.
        let q = Query::compile("main.js ext:md").unwrap();
        assert_eq!(execute_fuzzy(&ix, &rp, &q, 1, 0, false, Some(&tg)).total, 0);
    }

    #[cfg(feature = "trigram")]
    #[test]
    fn fuzzy_negation_stays_exact() {
        let (ix, rp) = tree();
        let tg = TrigramIndex::build(&ix);
        // `-main` must exclude main.rs exactly; the positive term is fuzzy.
        let q = Query::compile("main.js -main").unwrap();
        let res = execute_fuzzy(&ix, &rp, &q, 1, 0, false, Some(&tg));
        assert_eq!(res.total, 0, "negation must not be fuzzed into matching");
    }

    #[cfg(feature = "trigram")]
    #[test]
    fn fuzzy_count_only_and_limit_agree() {
        let (ix, rp) = tree();
        let tg = TrigramIndex::build(&ix);
        let q = Query::compile("lib.rs").unwrap();
        let full = execute_fuzzy(&ix, &rp, &q, 2, 0, false, Some(&tg));
        let counted = execute_fuzzy(&ix, &rp, &q, 2, 0, true, Some(&tg));
        assert_eq!(full.total, counted.total);
        assert!(counted.hits.is_empty());
        let limited = execute_fuzzy(&ix, &rp, &q, 2, 1, false, Some(&tg));
        assert_eq!(limited.hits.len(), 1);
        assert_eq!(limited.total, full.total);
    }

    #[cfg(feature = "trigram")]
    #[test]
    fn approx_needles_exclude_ext_path_case_glob_regex() {
        let q = Query::compile("main.js ext:rs path:src case:Notes *.tmp regex:^x -skip").unwrap();
        let needles: Vec<String> = q
            .approx_needles()
            .iter()
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .collect();
        assert_eq!(
            needles,
            vec!["main.js".to_string()],
            "only the plain lowercase substring"
        );
    }
}
