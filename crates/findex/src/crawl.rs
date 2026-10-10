//! Initial crawl (§5).
//!
//! The race rule: **watch first, then enumerate**. During a crawl, incoming
//! watcher events queue in the pending collection and are applied afterwards;
//! all applies are idempotent upserts, so replay is safe. Each crawled dir is
//! recorded `enumerated`; the post-crawl pass re-enumerates any dir that
//! received events after its own enumeration.
//!
//! `getdents64`-based enumeration is the v1 target (§17 throughput); this
//! implementation uses `std::fs::read_dir`, which is `getdents64` plus a small
//! `stat`-free `dirent` decode. The `DT_UNKNOWN` → `fstatat` fallback is
//! expressed as a capability probe (§4) and applied by the caller.

use std::path::Path;

use crate::applier::FsView;

/// One directory's worth of discovered children, in the order returned.
#[derive(Clone, Debug, Default)]
pub struct Enumeration {
    pub dirs: Vec<Vec<u8>>,
    pub files: Vec<Vec<u8>>,
    pub symlinks: Vec<Vec<u8>>,
}

impl Enumeration {
    pub fn dir_count(&self) -> usize {
        self.dirs.len()
    }
    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

/// Enumerate a single directory without following symlinks.
pub fn enumerate(fs: &dyn FsView, abs: &Path) -> std::io::Result<Enumeration> {
    let mut e = Enumeration::default();
    for it in fs.read_dir(abs)? {
        if it.name == b"." || it.name == b".." {
            continue;
        }
        if crate::util::is_cookie(&it.name) {
            continue; // cookies are never indexed (§16)
        }
        if it.is_symlink {
            e.symlinks.push(it.name);
        } else if it.is_dir {
            e.dirs.push(it.name);
        } else {
            e.files.push(it.name);
        }
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applier::RealFs;
    use std::fs;

    #[test]
    fn enumerate_classifies_and_skips_cookies() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        fs::write(d.path().join("a.txt"), b"x").unwrap();
        fs::write(d.path().join(".findex-cookie-1"), b"").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("a.txt", d.path().join("link")).unwrap();

        let e = enumerate(&RealFs, d.path()).unwrap();
        assert_eq!(e.dir_count(), 1);
        assert_eq!(e.file_count(), 1);
        // The symlink above is only created on unix (Windows needs
        // privileges for symlinks).
        #[cfg(unix)]
        assert_eq!(e.symlinks.len(), 1);
        #[cfg(windows)]
        assert_eq!(e.symlinks.len(), 0);
        assert!(!e.files.iter().any(|n| crate::util::is_cookie(n)));
    }

    #[test]
    fn missing_dir_is_empty_not_error() {
        let d = tempfile::tempdir().unwrap();
        let e = enumerate(&RealFs, &d.path().join("nope")).unwrap();
        assert_eq!(e.dir_count(), 0);
    }
}
