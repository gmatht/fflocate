//! Physical drive identity for dual-boot store sharing (§4, §15).
//!
//! A path tells you where a file is *on this OS*; it cannot tell you whether
//! `/mnt/d` (Linux) and `D:/` (Windows) are the same physical volume — or
//! whether `/mnt/usb` today is the same stick as yesterday. `DriveId` is the
//! OS-independent answer both sides can agree on:
//!
//! * `disk_serial` — the hardware serial: stable across reformats, identifies
//!   the physical drive.
//! * `volume_uuid` — the filesystem identity (NTFS volume serial, ext4 UUID):
//!   stable until reformat, identifies the volume.
//!
//! Either identifier agreeing is a match. Both missing means "unidentifiable"
//! (tmpfs, overlays, virtual devices) — never a match.
//!
//! Identity is re-probed every boot and recorded per root in the store stamp
//! (`persist::StoreStamp`); it is never part of the snapshot format, so no
//! format bump was needed to add it.

use std::path::Path;

/// A physical drive/volume identity. `None` fields mean "could not probe".
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DriveId {
    pub disk_serial: Option<String>,
    pub volume_uuid: Option<String>,
}

impl DriveId {
    pub fn is_empty(&self) -> bool {
        self.disk_serial.is_none() && self.volume_uuid.is_none()
    }

    /// Two ids name the same drive if any present identifier agrees
    /// (compared case-insensitively: serials surface in varying case).
    pub fn matches(&self, other: &DriveId) -> bool {
        match (&self.disk_serial, &other.disk_serial) {
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => return true,
            _ => {}
        }
        match (&self.volume_uuid, &other.volume_uuid) {
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => return true,
            _ => {}
        }
        false
    }
}

/// Probe the drive backing `path`. Best-effort: `None` when the path is
/// unresolvable or the drive is unidentifiable (never an error).
pub fn probe_drive(path: &Path) -> Option<DriveId> {
    #[cfg(target_os = "linux")]
    {
        probe_linux(path)
    }
    #[cfg(windows)]
    {
        probe_windows(path)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        // No prober ported here yet (macOS needs IOKit); identity stays
        // unknown rather than guessed.
        let _ = path;
        None
    }
}

// ---------------------------------------------------------------------------
// Linux: /dev/disk/by-uuid + /sys/block serials. No new dependencies.
// ---------------------------------------------------------------------------

/// Assemble a [`DriveId`] for a mount source (e.g. `/dev/sda2`).
#[cfg(target_os = "linux")]
fn volume_drive_id(source: &str, by_uuid: &Path, sys_block: &Path) -> Option<DriveId> {
    if !source.starts_with("/dev/") {
        return None; // tmpfs, overlay, fuse, cifs...: no stable identity here
    }
    // Resolve mapper/symlink indirection (/dev/mapper/x → /dev/dm-0).
    let canon = std::fs::canonicalize(source).unwrap_or_else(|_| Path::new(source).to_path_buf());
    let dev = canon.file_name()?.to_string_lossy().into_owned();
    let id = DriveId {
        disk_serial: disk_serial_for(&dev, sys_block),
        volume_uuid: fs_uuid_for(&canon, by_uuid),
    };
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Filesystem UUID: the `/dev/disk/by-uuid/<uuid>` entry whose target is our
/// device. NTFS volume serials and ext4 UUIDs both appear here. `dev_path`
/// must exist (it is canonicalized for comparison).
#[cfg(target_os = "linux")]
fn fs_uuid_for(dev_path: &Path, by_uuid: &Path) -> Option<String> {
    let want = std::fs::canonicalize(dev_path).ok()?;
    let rd = std::fs::read_dir(by_uuid).ok()?;
    for ent in rd.flatten() {
        let uuid = ent.file_name().to_string_lossy().into_owned();
        let target = std::fs::read_link(ent.path()).ok()?;
        // Link targets are usually relative (`../../sda2`).
        let resolved = if target.is_absolute() { target } else { by_uuid.join(target) };
        if std::fs::canonicalize(resolved).ok().as_ref() == Some(&want) {
            return Some(uuid);
        }
    }
    None
}

/// Hardware serial for a device name (`sda2` → `sda`, `nvme0n1p2` →
/// `nvme0n1`); `None` for virtual devices and when sysfs has no serial.
#[cfg(target_os = "linux")]
fn disk_serial_for(dev: &str, sys_block: &Path) -> Option<String> {
    let disk = parent_disk(dev)?;
    let serial = std::fs::read_to_string(sys_block.join(&disk).join("serial")).ok()?;
    let serial = serial.trim().to_uppercase();
    if serial.is_empty() {
        None
    } else {
        Some(serial)
    }
}

/// Strip a partition suffix to the whole-disk name; `None` for virtual
/// devices (dm, md, loop, ram, zram), which have no stable serial.
#[cfg(target_os = "linux")]
fn parent_disk(dev: &str) -> Option<String> {
    // nvme0n1p2 / mmcblk0p1 style: strip a trailing `p<digits>`.
    for prefix in ["nvme", "mmcblk"] {
        if let Some(rest) = dev.strip_prefix(prefix) {
            let digits = rest.len() - rest.trim_end_matches(|c: char| c.is_ascii_digit()).len();
            let stem = &rest[..rest.len() - digits];
            if digits > 0 && stem.ends_with('p') && stem.len() > 1 {
                return Some(format!("{prefix}{}", &stem[..stem.len() - 1]));
            }
            return None;
        }
    }
    // sdX / hdX / vdX style: strip trailing digits, keep at least 3 chars.
    let stem = dev.trim_end_matches(|c: char| c.is_ascii_digit());
    if (stem.starts_with("sd") || stem.starts_with("hd") || stem.starts_with("vd"))
        && stem.len() >= 3
        && stem.len() < dev.len()
    {
        return Some(stem.to_string());
    }
    None
}

#[cfg(target_os = "linux")]
fn probe_linux(path: &Path) -> Option<DriveId> {
    let mounts = crate::mount::read_mountinfo();
    let m = crate::mount::mount_for(&mounts, path)?;
    volume_drive_id(&m.source, Path::new("/dev/disk/by-uuid"), Path::new("/sys/block"))
}

// ---------------------------------------------------------------------------
// Windows: volume GUID/serial + disk serial via windows-sys. Best-effort:
// any API failure degrades that field to `None`, never an error.
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn probe_windows(path: &Path) -> Option<DriveId> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Storage::FileSystem::*;
    use windows_sys::Win32::System::Ioctl::*;
    use windows_sys::Win32::System::IO::*;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    // The volume root for any path on it (`C:\x` → `C:\`).
    let mount = wide(path.as_os_str());
    let mut root = [0u16; 261];
    let ok =
        unsafe { GetVolumePathNameW(mount.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
    if ok == FALSE {
        return None;
    }

    // Volume GUID (`\\?\Volume{...}\`), stable until reformat.
    let mut guid = [0u16; 128];
    let volume_uuid = if unsafe {
        GetVolumeNameForVolumeMountPointW(root.as_ptr(), guid.as_mut_ptr(), guid.len() as u32)
    } != FALSE
    {
        let s: String =
            String::from_utf16_lossy(&guid).trim_end_matches('\0').to_string();
        let s = s.trim_start_matches(r"\\?\").trim_end_matches('\\').to_string();
        if s.is_empty() { None } else { Some(s) }
    } else {
        None
    };

    // NTFS volume serial as fallback identity.
    let volume_uuid = volume_uuid.or_else(|| {
        let mut serial: u32 = 0;
        let mut fs_flags: u32 = 0;
        let mut max_comp: u32 = 0;
        let mut vol = [0u16; 261];
        let mut fs = [0u16; 261];
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                vol.as_mut_ptr(),
                vol.len() as u32,
                &mut serial,
                &mut max_comp,
                &mut fs_flags,
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        };
        (ok != FALSE && serial != 0).then(|| format!("{serial:08X}"))
    });

    // Hardware disk serial via the volume's drive letter (`\\.\C:`).
    let disk_serial = (|| {
        let root_s = String::from_utf16_lossy(&root);
        let mut chars = root_s.chars();
        let letter = chars.next()?;
        if !letter.is_ascii_alphabetic() || chars.next() != Some(':') {
            return None;
        }
        let dev_path = wide(std::ffi::OsStr::new(&format!(r"\\.\{letter}:")));
        let handle = unsafe {
            CreateFileW(
                dev_path.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                0,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut desc = [0u8; 1024];
        let mut returned: u32 = 0;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                &mut query as *mut _ as *mut _,
                std::mem::size_of_val(&query) as u32,
                desc.as_mut_ptr() as *mut _,
                desc.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        unsafe { CloseHandle(handle) };
        if ok == FALSE {
            return None;
        }
        // STORAGE_DEVICE_DESCRIPTOR: SerialNumberOffset is a u32 at byte 8.
        let off = u32::from_le_bytes([desc[8], desc[9], desc[10], desc[11]]) as usize;
        if off == 0 || off >= desc.len() {
            return None;
        }
        let end = desc[off..]
            .iter()
            .position(|&b| b == 0)
            .map(|i| off + i)
            .unwrap_or(desc.len());
        let s = String::from_utf8_lossy(&desc[off..end]).trim().to_uppercase();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    })();

    let id = DriveId { disk_serial, volume_uuid };
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_needs_one_agreeing_identifier() {
        let a = DriveId { disk_serial: Some("ABC".into()), volume_uuid: Some("111".into()) };
        let b = DriveId { disk_serial: Some("abc".into()), volume_uuid: Some("222".into()) };
        assert!(a.matches(&b), "serial agrees (case-insensitive)");
        let c = DriveId { disk_serial: Some("ZZZ".into()), volume_uuid: Some("111".into()) };
        assert!(a.matches(&c), "uuid agrees");
        let d = DriveId { disk_serial: Some("ZZZ".into()), volume_uuid: Some("222".into()) };
        assert!(!a.matches(&d), "nothing agrees");
        let e = DriveId::default();
        assert!(!a.matches(&e) && !e.matches(&a), "empty never matches");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn parent_disk_strips_partitions() {
        assert_eq!(parent_disk("sda2"), Some("sda".to_string()));
        assert_eq!(parent_disk("sdaa12"), Some("sdaa".to_string()));
        assert_eq!(parent_disk("nvme0n1p2"), Some("nvme0n1".to_string()));
        assert_eq!(parent_disk("mmcblk0p1"), Some("mmcblk0".to_string()));
        assert_eq!(parent_disk("sda"), None, "whole disk has no parent");
        assert_eq!(parent_disk("dm-0"), None);
        assert_eq!(parent_disk("loop0"), None);
        assert_eq!(parent_disk("md0"), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn fixture_by_uuid_and_sysfs() {
        use std::io::Write;

        let d = tempfile::tempdir().unwrap();
        // A fake device file and a sysfs tree with a serial.
        let dev = d.path().join("dev/sda1");
        std::fs::create_dir_all(dev.parent().unwrap()).unwrap();
        std::fs::write(&dev, b"").unwrap();
        let sys = d.path().join("sys/block");
        std::fs::create_dir_all(sys.join("sda")).unwrap();
        let mut f = std::fs::File::create(sys.join("sda/serial")).unwrap();
        f.write_all(b"WD-XYZ\n").unwrap();

        // by-uuid dir with an absolute link: resolves.
        let abs_uuid = d.path().join("by-uuid-abs");
        std::fs::create_dir(&abs_uuid).unwrap();
        std::os::unix::fs::symlink(&dev, abs_uuid.join("ABCD-1234")).unwrap();
        assert_eq!(fs_uuid_for(&dev, &abs_uuid), Some("ABCD-1234".to_string()));

        // by-uuid dir with a relative link (`../dev/sda1`, like the real
        // `../../sda1` under /dev/disk): resolves against its own dir.
        let rel_uuid = d.path().join("by-uuid-rel");
        std::fs::create_dir(&rel_uuid).unwrap();
        std::os::unix::fs::symlink("../dev/sda1", rel_uuid.join("REL-UUID")).unwrap();
        assert_eq!(fs_uuid_for(&dev, &rel_uuid), Some("REL-UUID".to_string()));

        // Dangling link: skipped, not fatal.
        std::os::unix::fs::symlink("../dev/nope", rel_uuid.join("STALE")).unwrap();
        assert!(fs_uuid_for(&dev, &rel_uuid).is_some());

        assert_eq!(disk_serial_for("sda1", &sys), Some("WD-XYZ".to_string()));
        assert_eq!(disk_serial_for("dm-0", &sys), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn non_device_sources_have_no_identity() {
        let d = tempfile::tempdir().unwrap();
        let empty = d.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(volume_drive_id("tmpfs", &empty, &empty), None);
        assert_eq!(volume_drive_id("overlay", &empty, &empty), None);
        assert_eq!(volume_drive_id("/dev/sda1", &empty, &empty), None, "nothing backing it");
    }
}
