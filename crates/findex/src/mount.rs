//! Per-mount probing (§4).
//!
//! At root-add (and on mount events) classify the mount from
//! `/proc/self/mountinfo`, then **probe** rather than assume. Everything probed
//! is surfaced via `fx stats --roots`: a mis-probed mount should be visible, not
//! mysterious.

use std::path::Path;

use crate::roots::MountCaps;

/// Filesystems excluded by default (pseudo-FS and container overlays, §4/§16).
const EXCLUDED_FSTYPES: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "cgroup",
    "cgroup2",
    "tracefs",
    "debugfs",
    "securityfs",
    "pstore",
    "bpf",
    "mqueue",
    "hugetlbfs",
    "configfs",
    "fusectl",

    "rpc_pipefs",
];

/// Filesystems treated as case-insensitive by default (the fstype table; the
/// optional temp-file-pair probe overrides this when the dir is writable).
const CASE_INSENSITIVE_FSTYPES: &[&str] = &["vfat", "exfat", "msdos", "ntfs", "ntfs3", "cifs", "smbfs"];

pub fn is_excluded_fstype(fstype: &str) -> bool {
    let f = fstype.to_ascii_lowercase();
    EXCLUDED_FSTYPES.iter().any(|e| f == *e)
        || f.starts_with("cgroup")
        || (f == "overlay" && !overlay_opt_in())
}

fn overlay_opt_in() -> bool {
    std::env::var("FINDEX_ALLOW_OVERLAY").map(|v| v == "1").unwrap_or(false)
}

/// One parsed `/proc/self/mountinfo` line (the fields we care about).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountInfo {
    pub mount_point: String,
    pub fstype: String,
    pub source: String,
}

/// Parse `/proc/self/mountinfo`. Lenient: unparseable lines are skipped.
pub fn parse_mountinfo(text: &str) -> Vec<MountInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        // Fields: id parent major:minor root mount_point opts [optional...] - fstype source superopts
        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some(dash) = parts.iter().position(|p| *p == "-") else { continue };
        if parts.len() < dash + 3 || parts.len() < 5 {
            continue;
        }
        let mount_point = parts[4].to_string();
        let fstype = parts[dash + 1].to_string();
        let source = parts[dash + 2].to_string();
        out.push(MountInfo { mount_point, fstype, source });
    }
    out
}

pub fn read_mountinfo() -> Vec<MountInfo> {
    match std::fs::read_to_string("/proc/self/mountinfo") {
        Ok(t) => parse_mountinfo(&t),
        Err(_) => Vec::new(),
    }
}

/// Find the mount whose mount_point is the longest prefix of `path`.
pub fn mount_for<'a>(mounts: &'a [MountInfo], path: &Path) -> Option<&'a MountInfo> {
    let p = path.to_string_lossy();
    mounts
        .iter()
        .filter(|m| p.starts_with(&m.mount_point))
        .max_by_key(|m| m.mount_point.len())
}

/// Everything needed to build [`MountCaps`] — separated from the syscall so the
/// logic is unit-testable.
#[derive(Clone, Debug, Default)]
pub struct ProbeInputs {
    pub fstype: String,
    pub writable: bool,
    /// `getdents64` returned `DT_UNKNOWN` on the probe dir.
    pub dtype_unreliable: bool,
    /// `name_to_handle_at` succeeded on the root dir.
    pub file_handles: bool,
    /// The dir is on a case-insensitive fs by an explicit temp-file-pair probe
    /// (takes precedence over the fstype table when `Some`).
    pub case_probe: Option<bool>,
    /// Backend can apply exclusions at watch time.
    pub watch_time_ignore: bool,
}

/// Classify a mount from parsed inputs.
pub fn caps_from_probe(inp: &ProbeInputs) -> MountCaps {
    let case_insensitive = inp
        .case_probe
        .unwrap_or_else(|| CASE_INSENSITIVE_FSTYPES.contains(&inp.fstype.to_ascii_lowercase().as_str()));
    MountCaps {
        file_handles: inp.file_handles,
        dtype_unreliable: inp.dtype_unreliable,
        case_insensitive,
        writable: inp.writable,
        fstype: inp.fstype.clone(),
        watch_time_ignore: inp.watch_time_ignore,
    }
}

/// Probe a real root directory. Best-effort: each probe failure yields a
/// conservative value rather than an error.
pub fn probe_root(path: &Path) -> MountCaps {
    let mounts = read_mountinfo();
    let fstype = mount_for(&mounts, path).map(|m| m.fstype.clone()).unwrap_or_default();
    let writable = is_writable(path);
    let file_handles = has_file_handle(path);
    let dtype_unreliable = dtype_probe(path);
    let case_probe = if writable { Some(case_probe(path)) } else { None };
    caps_from_probe(&ProbeInputs {
        fstype,
        writable,
        dtype_unreliable,
        file_handles,
        case_probe,
        watch_time_ignore: true, // inotify backend is per-dir watch
    })
}

pub fn is_writable(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return false };
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// `st_dev` of a path, or `None` if it does not exist. Used by the daemon's
/// mount watchdog: a change in device id means the mount was replaced
/// underneath us (a remount or a different filesystem mounted on the same
/// point) — the portable signal, because `IN_UNMOUNT` is not delivered on every
/// platform (it is missing under WSL2, for instance).
pub fn probe_dev(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.dev())
}

/// Does `name_to_handle_at` work on `path`? (fanotify-FID viability, §4.)
pub fn has_file_handle(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return false };
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if fd < 0 {
        return false;
    }
    // A 128-byte handle buffer is enough for every in-tree fs.
    let mut handle = [0u8; 128];
    let mut mount_id: libc::c_int = 0;
    let rc = unsafe {
        libc::syscall(
            libc::SYS_name_to_handle_at,
            fd,
            c.as_ptr(),
            handle.as_mut_ptr() as *mut libc::c_void,
            &mut mount_id as *mut libc::c_int,
            0,
        )
    };
    unsafe {
        libc::close(fd);
    }
    rc == 0
}

/// Do we need `fstatat` fallback during crawls? We cannot observe `DT_UNKNOWN`
/// after `std::fs` has already resolved the type, so this is a conservative
/// heuristic: report unreliable on filesystems known to sometimes omit d_type.
fn dtype_probe(path: &Path) -> bool {
    let mounts = read_mountinfo();
    matches!(
        mount_for(&mounts, path).map(|m| m.fstype.as_str()),
        Some("xfs") | Some("9p") | Some("fuse") | Some("fuseblk")
    )
}

/// Write a temp file and check whether a variant spelling resolves to it. Only
/// meaningful on writable directories.
pub fn case_probe(dir: &Path) -> bool {
    let probe_a = dir.join(".findex-caseprobe-A");
    let probe_b = dir.join(".findex-caseprobe-a");
    if std::fs::write(&probe_a, b"").is_err() {
        return false;
    }
    let insensitive = probe_b.exists();
    let _ = std::fs::remove_file(&probe_a);
    insensitive
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
76 81 0:30 / /usr/lib/modules rw,nosuid,nodev - overlay none rw
77 81 0:33 / /mnt/wsl rw,relatime shared:1 - tmpfs none rw
78 81 0:35 / /usr/lib/wsl/drivers ro,nosuid - 9p drivers ro,aname=x
99 81 8:1 / /home rw,relatime - ext4 /dev/sda1 rw
100 81 8:1 / /home/me/data rw,relatime - vfat /dev/sdb1 rw
";

    #[test]
    fn parse_and_select_longest_mount() {
        let mounts = parse_mountinfo(SAMPLE);
        assert_eq!(mounts.len(), 5);
        let m = mount_for(&mounts, Path::new("/home/me/data/x")).unwrap();
        assert_eq!(m.fstype, "vfat");
        let m = mount_for(&mounts, Path::new("/home/me/x")).unwrap();
        assert_eq!(m.fstype, "ext4");
    }

    #[test]
    fn excluded_fstypes() {
        assert!(is_excluded_fstype("proc"));
        assert!(is_excluded_fstype("sysfs"));
        assert!(is_excluded_fstype("cgroup2"));
        assert!(is_excluded_fstype("tracefs"));
        // overlay is excluded unless opted in
        assert!(is_excluded_fstype("overlay"));
        assert!(!is_excluded_fstype("ext4"));
        assert!(!is_excluded_fstype("vfat"));
    }

    #[test]
    fn case_insensitive_table_and_probe_precedence() {
        let inp = ProbeInputs { fstype: "vfat".into(), ..Default::default() };
        assert!(caps_from_probe(&inp).case_insensitive);
        let inp = ProbeInputs { fstype: "ext4".into(), ..Default::default() };
        assert!(!caps_from_probe(&inp).case_insensitive);
        // An explicit probe beats the table.
        let inp = ProbeInputs { fstype: "ext4".into(), case_probe: Some(true), ..Default::default() };
        assert!(caps_from_probe(&inp).case_insensitive);
    }

    #[test]
    fn probe_real_dir() {
        let d = tempfile::tempdir().unwrap();
        let caps = probe_root(d.path());
        assert!(caps.writable, "a tempdir must be writable");
        // case probe on a normal ext4/tmpfs dir should say case-sensitive
        assert!(!caps.case_insensitive, "tempdirs are case-sensitive here");
    }

    #[test]
    fn writable_detection() {
        let d = tempfile::tempdir().unwrap();
        assert!(is_writable(d.path()));
        assert!(!is_writable(Path::new("/nonexistent-xyz")));
    }
}
