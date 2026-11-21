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
        //
        // The name is taken from the *effective* uid, not from `$USER`. `$USER`
        // is inherited and can name a different account than the one running
        // the tests (a sandbox that re-exports the login shell's environment, a
        // `su`/`setpriv` wrapper, an agent runner): this box has `john` at uid
        // 1000 and `ai` at 1001, so `USER=ai` made the old test fail with
        // `Some(1001)` vs `Some(1000)` — a mismatch in the environment, not in
        // the lookup it is meant to cover.
        let uid = unsafe { libc::getuid() };
        // SAFETY: `getpwuid` returns a pointer into a static buffer owned by
        // libc; it is read immediately and not retained.
        let pw = unsafe { libc::getpwuid(uid) };
        if pw.is_null() {
            // No account is named for this uid (a bare container, a mapped
            // uid): there is no name to resolve, so the round trip below
            // cannot be exercised.
            return;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }
            .to_str()
            .expect("a user name is utf-8 on this platform");
        assert_eq!(uid_for_name(name), Some(uid), "user {name} (uid {uid})");
    }

    #[test]
    fn unknown_names_are_none_not_zero() {
        assert_eq!(uid_for_name("fflocate-definitely-not-a-user"), None);
        assert_eq!(gid_for_name("fflocate-definitely-not-a-group"), None);
    }
}
