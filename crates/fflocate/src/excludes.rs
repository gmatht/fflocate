//! Exclusion rules (§4/§6).
//!
//! The design draws an explicit line between two ways a watcher can avoid a
//! directory: **decline the watch** (inotify can; the kernel never tells us
//! about the subtree) or **filter the events** (fanotify-FID and FSEvents cannot
//! decline — they watch whole mounts, so the applier must drop the subtree).
//! That asymmetry is encoded once, as [`BackendCaps::whole_mount`], and this
//! module is the filtering half that a whole-mount backend needs.
//!
//! ## How filtering works here
//!
//! An excluded directory is **never inserted into the index**. That single rule
//! does the whole job: events name their parent by a [`DirKey`], and a directory
//! that was never inserted has no key, so every event about its children is
//! dropped for free — no per-event ancestor walk, and no way for one to slip
//! through.
//!
//! The check happens in two places, and they must agree:
//!
//! * the crawl, before inserting a directory entry;
//! * the event path, before an `Upsert` creates a directory.
//!
//! Both go through [`Excludes::excludes_dir_name`], so there is one rule.
//!
//! [`BackendCaps::whole_mount`]: crate::watch::BackendCaps::whole_mount
//! [`DirKey`]: crate::event::DirKey

/// A set of exclusions applied to directory *names* as they are discovered.
///
/// Name-based rather than path-based on purpose: an event carries a parent id
/// and a name, never a path, so a name rule is the only rule the event path can
/// evaluate without materializing the path (and it is why a directory rename
/// moves its whole exclusion with it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Excludes {
    /// Directory names excluded outright (`.git`, `node_modules`, …).
    pub names: Vec<Vec<u8>>,
    /// Suffixes excluded (`.pyc`, `~`). Kept separate so the intent is legible.
    pub suffixes: Vec<Vec<u8>>,
    /// Absolute path prefixes excluded (a mounted pseudo-FS, a scratch dir).
    pub prefixes: Vec<Vec<u8>>,
}

impl Excludes {
    pub fn new() -> Excludes {
        Excludes::default()
    }

    /// The rules fflocate has by default. Deliberately small: version-control and
    /// build outputs that are pure noise in a *name* index, and nothing that
    /// could surprise someone looking for a file they know exists.
    pub fn defaults() -> Excludes {
        Excludes {
            names: vec![
                b".git".to_vec(),
                b"node_modules".to_vec(),
                b".hg".to_vec(),
                b".svn".to_vec(),
            ],
            suffixes: vec![],
            prefixes: vec![],
        }
    }

    /// Parse a `FFLOCATE_EXCLUDE` value: comma- or separator-delimited names. An
    /// entry starting with `/` is a prefix, `*.x` / `.x` is a suffix, anything
    /// else is a name.
    pub fn parse_list(s: &str) -> Excludes {
        let mut e = Excludes::new();
        for raw in s.split([',', ':']).map(str::trim).filter(|t| !t.is_empty()) {
            if let Some(p) = raw.strip_prefix('/') {
                let mut v = vec![b'/'];
                v.extend_from_slice(p.as_bytes());
                e.prefixes.push(v);
            } else if let Some(suf) = raw.strip_prefix("*.") {
                e.suffixes.push(format!(".{suf}").into_bytes());
            } else if raw.starts_with('.')
                && raw.len() > 1
                && raw[1..].chars().all(|c| c.is_ascii_alphanumeric())
            {
                // ".git" — a leading-dot name, not a suffix.
                e.names.push(raw.as_bytes().to_vec());
            } else if let Some(suf) = raw.strip_prefix('.') {
                e.suffixes.push(format!(".{suf}").into_bytes());
            } else {
                e.names.push(raw.as_bytes().to_vec());
            }
        }
        e
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.suffixes.is_empty() && self.prefixes.is_empty()
    }

    /// Would a directory with this name be excluded?
    ///
    /// The one rule the crawl and the event path share.
    pub fn excludes_dir_name(&self, name: &[u8]) -> bool {
        if self.names.iter().any(|n| n.as_slice() == name) {
            return true;
        }
        if self.suffixes.iter().any(|s| name.ends_with(s.as_slice())) {
            return true;
        }
        false
    }

    /// Would this absolute path be excluded by a prefix rule? Only the crawl
    /// knows absolute paths; the event path relies on the fact that an excluded
    /// directory is never inserted, so its children never resolve.
    pub fn excludes_path(&self, path: &[u8]) -> bool {
        self.prefixes.iter().any(|p| path.starts_with(p.as_slice()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_excludes_cover_vcs_and_vendored_dirs() {
        let e = Excludes::defaults();
        assert!(e.excludes_dir_name(b".git"));
        assert!(e.excludes_dir_name(b"node_modules"));
        assert!(!e.excludes_dir_name(b"src"));
        assert!(
            !e.excludes_dir_name(b".gitignore"),
            "a file that merely starts with .git is not the dir"
        );
    }

    #[test]
    fn name_matching_is_exact_not_prefix() {
        let e = Excludes::parse_list("build");
        assert!(e.excludes_dir_name(b"build"));
        assert!(
            !e.excludes_dir_name(b"builds"),
            "exact name, so a sibling is not swallowed"
        );
        assert!(!e.excludes_dir_name(b"prebuild"));
    }

    #[test]
    fn suffix_rules() {
        let e = Excludes::parse_list("*.pyc,*.tmp");
        assert!(e.excludes_dir_name(b"x.pyc"));
        assert!(e.excludes_dir_name(b"x.tmp"));
        assert!(!e.excludes_dir_name(b"x.py"));
    }

    #[test]
    fn prefix_rules_are_paths() {
        let e = Excludes::parse_list("/proc,/sys");
        assert!(e.excludes_path(b"/proc/self/fd"));
        assert!(e.excludes_path(b"/sys/kernel"));
        assert!(!e.excludes_path(b"/home/proc"));
        assert!(
            !e.excludes_dir_name(b"proc"),
            "a prefix rule is not a name rule"
        );
    }

    #[test]
    fn leading_dot_entries_are_names_not_suffixes() {
        // ".cache" is a directory name, but ".o" reads as a suffix. Both are
        // expressible; the parse must not confuse them.
        let e = Excludes::parse_list(".cache");
        assert!(e.excludes_dir_name(b".cache"));
        assert!(e.suffixes.is_empty(), ".cache is a name, not a suffix");
    }

    #[test]
    fn empty_and_whitespace_entries_are_ignored() {
        let e = Excludes::parse_list(" , ,build ");
        assert_eq!(e.names, vec![b"build".to_vec()]);
        assert!(Excludes::parse_list("").is_empty());
        assert!(Excludes::new().is_empty());
    }
}
