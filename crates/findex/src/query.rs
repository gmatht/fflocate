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
        f.debug_struct("Query").field("raw", &self.raw).field("terms", &self.terms).finish()
    }
}

impl Query {
    pub fn compile(raw: &str) -> Result<Query, String> {
        let terms = parse_terms(raw)?;
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
        Ok(Query { terms, literals, literal_case, ac, is_empty, raw: raw.to_string() })
    }

    /// Does a candidate entry (by name, with its materialized path lazily
    /// supplied) pass every term? `path` is only computed when a `path:` term
    /// exists; `path_provider` is called at most once.
    #[allow(clippy::redundant_closure)] // closure needed: FnMut cannot be moved into FnOnce
    pub fn matches<F>(&self, name: &[u8], mut path_provider: F) -> bool
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
                Term::Not(inner) => !self.match_inner(inner, name, &mut path_cache, &mut path_provider),
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
            Term::Not(inner) => !self.match_inner(inner, name, path_cache, path_provider),
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
        if bytes[i] == b'-' && i + 1 < bytes.len() && !(bytes[i + 1] as char).is_ascii_whitespace() {
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
        out.push(if negate { Term::Not(Box::new(term)) } else { term });
    }
    Ok(out)
}

fn compile_token(tok: &[u8], quoted: bool) -> Result<Term, String> {
    // Modifiers are only recognized on unquoted tokens.
    if !quoted {
        if let Some(rest) = strip_prefix_ci(tok, b"ext:") {
            let case = has_upper(rest);
            return Ok(Term::Ext { ext: rest.to_vec(), case });
        }
        if let Some(rest) = strip_prefix_ci(tok, b"path:") {
            let case = has_upper(rest);
            return Ok(Term::Path { needle: rest.to_vec(), case });
        }
        if let Some(rest) = strip_prefix_ci(tok, b"case:") {
            // Force case-sensitive substring of the name.
            return Ok(Term::Substr { needle: rest.to_vec(), case: true });
        }
        if let Some(rest) = strip_prefix_ci(tok, b"regex:") {
            let re = Regex::new(std::str::from_utf8(rest).map_err(|_| "regex must be UTF-8")?)
                .map_err(|e| format!("bad regex: {e}"))?;
            return Ok(Term::Regex { re: Box::new(re) });
        }
    }
    if !quoted && (tok.contains(&b'*') || tok.contains(&b'?')) {
        let case = has_upper(tok);
        return Ok(Term::Glob { pattern: GlobPattern::parse(tok, case) });
    }
    let case = has_upper(tok);
    Ok(Term::Substr { needle: tok.to_vec(), case })
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

fn contains(hay: &[u8], needle: &[u8], case: bool) -> bool {
    if needle.is_empty() {
        return true;
    }
    if case {
        memchr::memmem::find(hay, needle).is_some()
    } else {
        // ASCII-case-insensitive substring: lowercase a copy of the haystack.
        let lower: Vec<u8> = hay.iter().map(|c| c.to_ascii_lowercase()).collect();
        memchr::memmem::find(&lower, &needle.iter().map(|c| c.to_ascii_lowercase()).collect::<Vec<_>>())
            .is_some()
    }
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
}

/// Run `q` over `index`, attributing paths using each root dir's stored path.
///
/// `limit == 0` means unlimited. When `count_only`, paths are never
/// materialized and the returned vec is empty with `total` set.
pub fn execute(index: &Index, root_paths: &[(u32, Vec<u8>)], q: &Query, limit: usize, count_only: bool) -> Results {
    let mut results = Results { hits: Vec::new(), total: 0 };
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let use_prefilter = q.ac.is_some();

    for (id, entry) in index.iter_live() {
        if entry.flags().is_root_dir() {
            continue; // the root dir itself is not a search hit
        }
        let name = index.name(id);
        // aho-corasick prefilter over the lowercased name (cheap, one pass)
        if use_prefilter {
            let lower: Vec<u8> = name.iter().map(|c| c.to_ascii_lowercase()).collect();
            if !q.prefilter(&lower) {
                continue;
            }
        }
        // Which root does this belong to? Needed only for path materialization.
        let root_id = index.root_of(id);
        let root_path = root_id.and_then(|r| root_paths.iter().find(|(rid, _)| *rid == r).map(|(_, p)| p.clone()));
        let matched = q.matches(name, || {
            root_path.as_ref().and_then(|rp| index.path_of(id, rp))
        });
        if !matched {
            continue;
        }
        results.total += 1;
        if count_only {
            continue;
        }
        if limit != 0 && results.hits.len() >= limit {
            // Keep counting for `total`, but stop materializing.
            continue;
        }
        let path = match &root_path {
            Some(rp) => match index.materialize_into(id, rp, &mut buf) {
                Some(_) => buf.clone(),
                None => continue,
            },
            None => name.to_vec(),
        };
        results.hits.push(Hit { id, path });
    }
    results
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

    let mut results = Results { hits: Vec::new(), total: 0 };
    let mut buf: Vec<u8> = Vec::with_capacity(256);

    // The anchor path we materialize against is `root_path/rel_root`; the
    // reported name is then everything after it.
    for (id, entry) in index.iter_live() {
        if entry.flags().is_root_dir() {
            continue;
        }
        let Some(root_id) = index.root_of(id) else { continue };
        let Some((_, root_path)) = root_paths.iter().find(|(rid, _)| *rid == root_id) else {
            continue;
        };

        // Absolute path of the candidate under its root.
        buf.clear();
        let Some(abs) = index.path_of(id, root_path) else { continue };

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
        } else if abs.len() > anchor.len()
            && abs.starts_with(&anchor)
            && abs[anchor.len()] == b'/'
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
        results.hits.push(Hit { id, path: abs });
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{EntryFlags, ROOT_ID};

    fn tree() -> (Index, Vec<(u32, Vec<u8>)>) {
        let mut ix = Index::new();
        let r = ix.insert(ROOT_ID, b"proj", EntryFlags::EMPTY.with_dir(true).with_root_dir(true));
        let src = ix.insert(r, b"src", EntryFlags::EMPTY.with_dir(true));
        ix.insert(src, b"main.rs", EntryFlags::EMPTY);
        ix.insert(src, b"lib.rs", EntryFlags::EMPTY);
        ix.insert(r, b"README.md", EntryFlags::EMPTY);
        ix.insert(r, b"Notes.TXT", EntryFlags::EMPTY);
        let root_path = b"/home/me/proj".to_vec();
        (ix, vec![(r, root_path)])
    }

    fn paths(res: &Results) -> Vec<String> {
        res.hits.iter().map(|h| String::from_utf8_lossy(&h.path).into_owned()).collect()
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
        assert_eq!(paths(&execute(&ix, &rp, &q, 0, false))[0], "/home/me/proj/src/lib.rs");
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
}
