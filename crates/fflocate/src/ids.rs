//! User and group name → id resolution (§4, §15).
//!
//! Reading `/etc/passwd` and `/etc/group` directly is a Linux habit: on macOS
//! both files are usually absent (OpenDirectory serves the database), so a
//! name lookup that parsed them would report "unknown group" for a group that
//! plainly exists. `getpwnam(3)`/`getgrnam(3)` are the portable answer and are
//! what the system's own tools use.

/// The uid for `name`, or `None` when no such user exists.
#[cfg(unix)]
pub fn uid_for_name(name: &str) -> Option<u32> {
    use std::ffi::CString;
    let c = CString::new(name).ok()?;
    // Safety: getpwnam returns a pointer into a static buffer owned by libc;
    // it is read immediately and not retained.
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    Some(unsafe { (*pw).pw_uid })
}

/// The gid for `name`, or `None` when no such group exists.
#[cfg(unix)]
pub fn gid_for_name(name: &str) -> Option<u32> {
    use std::ffi::CString;
    let c = CString::new(name).ok()?;
    let gr = unsafe { libc::getgrnam(c.as_ptr()) };
    if gr.is_null() {
        return None;
    }
    Some(unsafe { (*gr).gr_gid })
}

#[cfg(not(unix))]
pub fn uid_for_name(_name: &str) -> Option<u32> {
    None
}

#[cfg(not(unix))]
pub fn gid_for_name(_name: &str) -> Option<u32> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn current_user_resolves_to_its_own_uid() {
        // Whatever the platform's account database is, the invoking user must
        // resolve — this is the case that broke on macOS when we parsed files.
        let me = std::env::var("USER").unwrap_or_else(|_| "root".to_string());
        let uid = unsafe { libc::getuid() };
        assert_eq!(uid_for_name(&me), Some(uid), "user {me}");
    }

    #[test]
    fn unknown_names_are_none_not_zero() {
        assert_eq!(uid_for_name("fflocate-definitely-not-a-user"), None);
        assert_eq!(gid_for_name("fflocate-definitely-not-a-group"), None);
    }
}
