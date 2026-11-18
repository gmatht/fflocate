//! Small helpers shared across the crate.

/// CRC32 (IEEE, reflected) over a byte slice. Used for journal record and
/// snapshot checksums (§9). A tiny table-driven implementation keeps the
/// dependency surface small and works identically on every platform.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    crc = !crc;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Canonicalize a root path for the index: `\` becomes `/`, so Windows
/// roots (`C:\tree`) and the `/`-joined index paths (§3) share one spelling.
/// `std::fs` accepts `/` on Windows, including UNC `//server/share`.
pub fn normalize_separators(path: &std::path::Path) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        std::path::PathBuf::from(s.replace('\\', "/"))
    } else {
        std::path::PathBuf::from(s.into_owned())
    }
}

/// True if `name` is a fflocate sync-cookie file name (§8). Cookies are never
/// indexed and never appear in subscriber feeds.
pub const COOKIE_PREFIX: &str = ".fflocate-cookie-";

pub fn is_cookie(name: &[u8]) -> bool {
    name.starts_with(COOKIE_PREFIX.as_bytes())
}

/// FNV-1a 64-bit, used for cheap hashing of names inside the index.
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// NFC-normalize a file name for the index.
///
/// macOS hands back *decomposed* names (HFS+ stores NFD, and APFS preserves
/// whatever it was given), while a user typing on a Mac produces *precomposed*
/// text. Indexing and querying both go through this helper so "café" typed with
/// one code point matches "café" stored as `e` + combining acute. Non-UTF-8
/// names are passed through untouched: they cannot be normalized, and mangling
/// them would make them unopenable.
#[cfg(target_os = "macos")]
pub fn normalize_name(name: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    use std::borrow::Cow;
    use unicode_normalization::UnicodeNormalization;
    match std::str::from_utf8(name) {
        Ok(s) => {
            let n: String = s.nfc().collect();
            if n.as_bytes() == name {
                Cow::Borrowed(name)
            } else {
                Cow::Owned(n.into_bytes())
            }
        }
        Err(_) => Cow::Borrowed(name),
    }
}

/// NFC-normalize a query string. Identity off macOS (see [`normalize_name`]).
#[cfg(target_os = "macos")]
pub fn normalize_text(s: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    use unicode_normalization::UnicodeNormalization;
    let n: String = s.nfc().collect();
    if n == s {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(n)
    }
}

/// Identity on platforms whose filenames are already precomposed (Linux,
/// Windows); the call sites stay platform-free.
#[cfg(not(target_os = "macos"))]
pub fn normalize_name(name: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    std::borrow::Cow::Borrowed(name)
}

#[cfg(not(target_os = "macos"))]
pub fn normalize_text(s: &str) -> std::borrow::Cow<'_, str> {
    std::borrow::Cow::Borrowed(s)
}

/// Escape a byte path for JSON output. Returns a `String`; invalid UTF-8 is
/// handled by serde_json's lossy conversion at the call site. Here we only
/// need equality/print behaviour for CLI, so keep bytes intact.
pub fn lossy(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_vectors() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"hello"), 0x3610_a686);
    }

    #[test]
    fn crc_incremental_matches() {
        let whole = crc32(b"the quick brown fox");
        let part = crc32_update(crc32_update(0, b"the quick "), b"brown fox");
        assert_eq!(whole, part);
    }

    #[test]
    fn normalization_is_identity_where_not_needed() {
        // Linux/Windows filenames are taken as-is; macOS folds to NFC.
        let nfd = "cafe\u{301}";
        let got = normalize_name(nfd.as_bytes());
        #[cfg(target_os = "macos")]
        assert_eq!(got.as_ref(), "caf\u{e9}".as_bytes());
        #[cfg(not(target_os = "macos"))]
        assert_eq!(got.as_ref(), nfd.as_bytes());
        assert_eq!(normalize_text("plain").as_ref(), "plain");
    }

    #[test]
    fn cookie_prefix() {
        assert!(is_cookie(b".fflocate-cookie-abc"));
        assert!(!is_cookie(b".fflocate-cookie"));
        assert!(!is_cookie(b"cookie"));
    }
}
