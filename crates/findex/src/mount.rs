//! Per-mount probing (§4).
//!
//! At root-add (and on mount events) classify the mount from
//! `/proc/self/mountinfo`, then **probe** rather than assume. Everything probed
//! is surfaced via `ff stats --roots`: a mis-probed mount should be visible, not
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

/// Native Linux filesystems: indexed by default on Linux (§4). Everything else
/// needs an explicit root — pseudo-FS never, network/fuse/removable media only
/// when asked for — and the mount-aware crawl enforces that boundary even when
/// such a mount sits inside an indexed tree.
const NATIVE_FSTYPES: &[&str] = &[
    "ext2", "ext3", "ext4", "btrfs", "xfs", "f2fs", "jfs", "reiserfs", "nilfs2", "zfs", "bcachefs",
];

pub fn is_native_fstype(fstype: &str) -> bool {
    NATIVE_FSTYPES.contains(&fstype.to_ascii_lowercase().as_str())
}

/// Default roots on Linux: the mount points of native filesystems (§4).
///
/// Pure over parsed mountinfo so it is unit-testable; the caller filters by
/// readability (a personal daemon skips what it cannot read, loudly) and falls
/// back to `$HOME` when discovery yields nothing (containers, odd fstypes).
/// Nested mounts are included — each filesystem gets its own root, and the
/// mount-aware crawl indexes each exactly once. Exact-duplicate mount points
/// (a bind mount listed twice) collapse to one. Sorted for determinism.
pub fn default_root_candidates(mounts: &[MountInfo]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    for m in mounts {
        if !is_native_fstype(&m.fstype) {
            continue;
        }
        if is_excluded_fstype(&m.fstype) {
            continue; // belt and braces: exclusion beats nativity
        }
        seen.insert(normalize_mount_point(&m.mount_point));
    }
    seen.into_iter().collect()
}

/// Canonicalize a mount-point spelling: unescaped already by the parser, so
/// only a trailing slash is stripped (`/` keeps its own). Config paths and
/// mountinfo spellings must compare equal for root merging and the
/// mountpoint set the crawl consults.
pub fn normalize_mount_point(p: &str) -> String {
    if p.len() > 1 {
        p.trim_end_matches('/').to_string()
    } else {
        p.to_string()
    }
}
/// Filesystems treated as case-insensitive by default (the fstype table; the
/// optional temp-file-pair probe overrides this when the dir is writable).
const CASE_INSENSITIVE_FSTYPES: &[&str] =
    &["vfat", "exfat", "msdos", "ntfs", "ntfs3", "cifs", "smbfs"];

pub fn is_excluded_fstype(fstype: &str) -> bool {
    let f = fstype.to_ascii_lowercase();
    EXCLUDED_FSTYPES.iter().any(|e| f == *e)
        || f.starts_with("cgroup")
        || (f == "overlay" && !overlay_opt_in())
}

fn overlay_opt_in() -> bool {
    std::env::var("FINDEX_ALLOW_OVERLAY")
        .map(|v| v == "1")
        .unwrap_or(false)
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
        let Some(dash) = parts.iter().position(|p| *p == "-") else {
            continue;
        };
        if parts.len() < dash + 3 || parts.len() < 5 {
            continue;
        }
        let mount_point = unescape_mountinfo_field(parts[4]);
        let fstype = unescape_mountinfo_field(parts[dash + 1]);
        let source = unescape_mountinfo_field(parts[dash + 2]);
        out.push(MountInfo {
            mount_point,
            fstype,
            source,
        });
    }
    out
}

/// Unescape mountinfo octal escapes (`\040` space, `\011` tab, `\012`
/// newline, `\134` backslash). Without this a mount point containing a space
/// never matches the path it names, and longest-prefix classification quietly
/// picks the wrong mount. A backslash not followed by three octal digits is
/// kept literally. All slicing is on ASCII boundaries (`\` never appears
/// inside a UTF-8 multibyte sequence), so non-UTF-8-hostile bytes pass
/// through untouched.
pub fn unescape_mountinfo_field(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        let is_esc = b[i] == b'\\'
            && i + 4 <= b.len()
            && b[i + 1..i + 4].iter().all(|c| matches!(c, b'0'..=b'7'));
        if is_esc {
            out.push_str(&s[start..i]);
            let n = ((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0')) as u32;
            match char::from_u32(n) {
                Some(c) => out.push(c),
                None => out.push_str(&s[i..i + 4]),
            }
            i += 4;
            start = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&s[start..]);
    out
}

pub fn read_mountinfo() -> Vec<MountInfo> {
    // Lossy: a non-UTF-8 mount point must not make the whole table unreadable
    // (it degrades to a replacement character and may miss its boundary, but
    // every other mount still resolves).
    match std::fs::read("/proc/self/mountinfo") {
        Ok(bytes) => parse_mountinfo(&String::from_utf8_lossy(&bytes)),
        Err(_) => Vec::new(),
    }
}

/// The mountpoint set a mount-aware crawl consults: every mount point as
/// bytes (mountinfo spellings, unescaped, trailing slash stripped).
pub fn mountpoint_set(mounts: &[MountInfo]) -> std::collections::HashSet<Vec<u8>> {
    mounts
        .iter()
        .map(|m| normalize_mount_point(&m.mount_point).into_bytes())
        .collect()
}

/// Can the daemon enumerate `path` at all? Used to skip unreadable default
/// roots loudly at discovery time rather than degrading every status line. A
/// successful open (not a full listing) is the probe — cheap, one syscall.
pub fn is_readable_dir(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok()
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
    let case_insensitive = inp.case_probe.unwrap_or_else(|| {
        CASE_INSENSITIVE_FSTYPES.contains(&inp.fstype.to_ascii_lowercase().as_str())
    });
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
    let fstype = mount_for(&mounts, path)
        .map(|m| m.fstype.clone())
        .unwrap_or_default();
    let writable = is_writable(path);
    let file_handles = has_file_handle(path);
    let dtype_unreliable = dtype_probe(path);
    let case_probe = if writable {
        Some(case_probe(path))
    } else {
        None
    };
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
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
    }
    // Best-effort: the readonly attribute. ACLs may still deny writes; the
    // classifier treats this as a hint, not a promise.
    #[cfg(windows)]
    {
        std::fs::metadata(path)
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false)
    }
}

/// `st_dev` of a path, or `None` if it does not exist. Used by the daemon's
/// mount watchdog: a change in device id means the mount was replaced
/// underneath us (a remount or a different filesystem mounted on the same
/// point) — the portable signal, because `IN_UNMOUNT` is not delivered on every
/// platform (it is missing under WSL2, for instance).
pub fn probe_dev(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| m.dev())
    }
    // No stable volume/file IDs on stable std (`windows_by_handle` is
    // nightly-only). `None` keeps the mount watchdog inert — its `r.dev != 0`
    // guard already treats unknown as "don't alarm".
    #[cfg(windows)]
    {
        let _ = path;
        None
    }
}

/// The raw kernel file handle for `path`, as the kernel reports it.
///
/// This is the piece the fanotify-FID resolver needs (§6): during the crawl we
/// ask the kernel for each directory's handle and remember `handle → dir id`;
/// every fanotify event then names its parent by that same handle, so the two
/// line up without any path guessing.
///
/// Returns `None` when the syscall is unavailable or fails (unsupported
/// filesystem, no permission). Callers treat that as "this dir cannot be
/// resolved" and fall back rather than silently mis-attribute.
#[cfg(target_os = "linux")]
pub fn handle_of_path(path: &Path) -> Option<HandleId> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    // `struct file_handle { u32 handle_bytes; s32 handle_type; u8 f_handle[]; }`.
    // Over-allocate and let the kernel tell us the real size.
    let mut buf = vec![0u8; 8 + 128];
    let mut mount_id: libc::c_int = 0;
    // handle_bytes must be set to the buffer capacity before the call.
    let cap = (buf.len() - 8) as u32;
    buf[0..4].copy_from_slice(&cap.to_ne_bytes());
    let rc = unsafe {
        libc::syscall(
            libc::SYS_name_to_handle_at,
            fd,
            c.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut mount_id as *mut libc::c_int,
            0,
        )
    };
    unsafe {
        libc::close(fd);
    }
    if rc != 0 {
        return None;
    }
    let handle_bytes = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let handle_type = i32::from_ne_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let n = handle_bytes as usize;
    if 8 + n > buf.len() {
        return None;
    }
    Some(HandleId {
        mount_id,
        handle_type,
        handle: buf[8..8 + n].to_vec(),
    })
}

/// The identity of a file handle as the kernel reports it, independent of any
/// path. `mount_id` comes from `name_to_handle_at`.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HandleId {
    pub mount_id: i32,
    pub handle_type: i32,
    pub handle: Vec<u8>,
}

#[cfg(not(target_os = "linux"))]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HandleId {
    pub mount_id: i32,
    pub handle_type: i32,
    pub handle: Vec<u8>,
}

#[cfg(not(target_os = "linux"))]
pub fn handle_of_path(_path: &Path) -> Option<HandleId> {
    None
}

/// Can this system run the fanotify-FID backend at all?
///
/// Probes by *doing*: open a fanotify fd with the flags the backend needs, and
/// close it. This is the honest test — a kernel may support `fanotify_init` but
/// not `FAN_REPORT_DFID_NAME` (pre-5.13), and a container may deny the syscall
/// outright (seccomp). Either way the answer is `false` and the caller falls back
/// to inotify, rather than discovering it when the first event never arrives.
#[cfg(target_os = "linux")]
pub fn fanotify_available() -> bool {
    // The same flag word the backend uses; keep them in sync.
    const FAN_CLASS_NOTIF: u32 = 0x0000_0000;
    const FAN_CLOEXEC: u32 = 0x0000_0001;
    const FAN_NONBLOCK: u32 = 0x0000_0002;
    const FAN_REPORT_DFID_NAME: u32 = 0x0000_0c00; // DIR_FID | NAME
    let flags = FAN_CLASS_NOTIF | FAN_REPORT_DFID_NAME | FAN_CLOEXEC | FAN_NONBLOCK;
    let fd = unsafe {
        libc::fanotify_init(
            flags as libc::c_uint,
            (libc::O_RDONLY | libc::O_LARGEFILE) as libc::c_uint,
        )
    };
    if fd < 0 {
        return false;
    }
    unsafe {
        libc::close(fd);
    }
    true
}

#[cfg(not(target_os = "linux"))]
pub fn fanotify_available() -> bool {
    false
}

/// A one-line explanation for `ff stats` / startup logs of *why* fanotify is or
/// is not usable, so a fallback is visible rather than mysterious (§4).
pub fn fanotify_availability_note() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        if fanotify_available() {
            "fanotify-FID available (FAN_REPORT_DFID_NAME)"
        } else {
            "fanotify-FID unavailable: fanotify_init(FAN_REPORT_DFID_NAME) failed (kernel < 5.13 or restricted)"
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        "fanotify-FID unavailable: Linux-only"
    }
}

/// Does `name_to_handle_at` work on `path`? (fanotify-FID viability, §4.)
pub fn has_file_handle(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
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
    // `name_to_handle_at(2)` is Linux-only; without it there are no file
    // handles to have (fanotify-FID is v1.5 anyway, §17).
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
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
        let inp = ProbeInputs {
            fstype: "vfat".into(),
            ..Default::default()
        };
        assert!(caps_from_probe(&inp).case_insensitive);
        let inp = ProbeInputs {
            fstype: "ext4".into(),
            ..Default::default()
        };
        assert!(!caps_from_probe(&inp).case_insensitive);
        // An explicit probe beats the table.
        let inp = ProbeInputs {
            fstype: "ext4".into(),
            case_probe: Some(true),
            ..Default::default()
        };
        assert!(caps_from_probe(&inp).case_insensitive);
    }

    #[test]
    fn probe_real_dir() {
        let d = tempfile::tempdir().unwrap();
        let caps = probe_root(d.path());
        assert!(caps.writable, "a tempdir must be writable");
        // Ground truth from a direct variant-spelling check: Linux
        // tmpfs/ext4 tempdirs are case-sensitive, macOS APFS ones usually
        // are not. Assert the classifier agrees with reality either way.
        let upper = d.path().join(".findex-ground-ABC");
        let lower = d.path().join(".findex-ground-abc");
        std::fs::write(&upper, b"").unwrap();
        let insensitive = lower.exists();
        let _ = std::fs::remove_file(&upper);
        assert_eq!(
            caps.case_insensitive, insensitive,
            "classifier must match the direct probe"
        );
    }

    #[test]
    fn writable_detection() {
        let d = tempfile::tempdir().unwrap();
        assert!(is_writable(d.path()));
        assert!(!is_writable(Path::new("/nonexistent-xyz")));
    }

    #[test]
    fn mountinfo_escapes_decode() {
        assert_eq!(unescape_mountinfo_field("/plain"), "/plain");
        assert_eq!(unescape_mountinfo_field("/mnt/my\\040disk"), "/mnt/my disk");
        assert_eq!(
            unescape_mountinfo_field("a\\011b\\012c\\134d"),
            "a\tb\nc\\d"
        );
        // Not an escape: kept literally.
        assert_eq!(unescape_mountinfo_field("/mnt/back\\"), "/mnt/back\\");
        assert_eq!(unescape_mountinfo_field("/mnt/\\d41"), "/mnt/\\d41");
        // ...and through the parser onto the mount point.
        let mounts =
            parse_mountinfo("99 81 8:1 / /mnt/my\\040disk rw,relatime - ext4 /dev/sda1 rw\n");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].mount_point, "/mnt/my disk");
    }

    const NATIVE_SAMPLE: &str = "\
23 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw
24 1 8:2 / /home rw,relatime - ext4 /dev/sda2 rw
25 1 8:3 / /var rw,relatime - xfs /dev/sda3 rw
26 1 8:4 / /mnt/data rw,relatime - btrfs /dev/sdb1 rw
27 1 8:4 /sub /mnt/data/sub rw,relatime - btrfs /dev/sdb1 rw
28 1 8:1 / /mnt/data rw,relatime - ext4 /dev/sda1 rw
29 1 0:5 / /proc rw,nosuid - proc proc rw
30 1 0:6 / /sys rw,nosuid - sysfs sysfs rw
31 1 8:5 / /boot/efi rw,relatime - vfat /dev/sda4 rw
32 1 8:6 / /mnt/usb rw,relatime - ntfs3 /dev/sdc1 rw
33 1 7:7 / /snap/core ro,relatime - squashfs /dev/loop0 ro
34 1 0:8 / /mnt/nfs rw,relatime - nfs server:/export rw
35 1 0:9 / /dev/shm rw,nosuid - tmpfs shm rw
36 1 0:10 / / rw,relatime - overlay overlay rw
";

    #[test]
    fn native_fstype_table() {
        for fs in [
            "ext4", "ext3", "ext2", "btrfs", "xfs", "f2fs", "zfs", "bcachefs", "EXT4",
        ] {
            assert!(is_native_fstype(fs), "{fs} must count as native");
        }
        for fs in [
            "vfat", "ntfs", "ntfs3", "nfs", "cifs", "tmpfs", "squashfs", "overlay", "proc",
            "fuseblk",
        ] {
            assert!(!is_native_fstype(fs), "{fs} must not count as native");
        }
    }

    #[test]
    fn default_roots_are_native_mounts_only() {
        let mounts = parse_mountinfo(NATIVE_SAMPLE);
        assert_eq!(mounts.len(), 14);
        // `/`, `/home`, `/var`, `/mnt/data` and its nested `/mnt/data/sub`
        // subvolume mount — each filesystem gets its own root, so nesting is
        // kept. vfat/ntfs removable media, squashfs snaps, NFS, tmpfs, proc,
        // sysfs and overlay are not native and stay out.
        assert_eq!(
            default_root_candidates(&mounts),
            vec!["/", "/home", "/mnt/data", "/mnt/data/sub", "/var"],
        );
    }

    #[test]
    fn duplicate_mount_points_collapse() {
        // The same mount listed twice (bind) yields one root.
        let mounts = parse_mountinfo(
            "\
23 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw
28 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw
",
        );
        assert_eq!(default_root_candidates(&mounts), vec!["/"]);
    }

    #[test]
    fn normalize_mount_point_strips_trailing_slash() {
        assert_eq!(normalize_mount_point("/mnt/data/"), "/mnt/data");
        assert_eq!(normalize_mount_point("/"), "/");
        assert_eq!(normalize_mount_point("/home"), "/home");
    }
}
