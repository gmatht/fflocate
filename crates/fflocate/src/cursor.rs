//! Cursors: the opaque `c:<gen>:<lsn>` handle that makes the journal a clock
//! (§9).
//!
//! A cursor is only valid while its `gen` matches the current index
//! generation. Generation bumps (snapshot load, a full-root rescan, compaction)
//! invalidate every cursor by design — a consumer holding a stale cursor must
//! resync from a full listing rather than apply deltas of uncertain provenance
//! (Watchman's `is_fresh_instance` contract).

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Cursor {
    pub gen: u64,
    pub lsn: u64,
}

impl Cursor {
    pub const ZERO: Cursor = Cursor { gen: 0, lsn: 0 };

    pub fn new(gen: u64, lsn: u64) -> Self {
        Cursor { gen, lsn }
    }

    /// Parse `c:<gen>:<lsn>`. Also accepts a bare integer as an LSN for
    /// convenience, treated as the current generation.
    pub fn parse(s: &str) -> Option<Cursor> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("c:") {
            let mut it = rest.split(':');
            let gen = it.next()?.parse().ok()?;
            let lsn = it.next()?.parse().ok()?;
            if it.next().is_some() {
                return None;
            }
            Some(Cursor { gen, lsn })
        } else {
            // Bare LSN is accepted for `ff --since <lsn>` convenience.
            None
        }
    }

    /// Parse a cursor, or a bare integer interpreted against `current_gen`.
    pub fn parse_with_gen(s: &str, current_gen: u64) -> Option<Cursor> {
        if let Some(c) = Cursor::parse(s) {
            return Some(c);
        }
        s.trim()
            .parse::<u64>()
            .ok()
            .map(|lsn| Cursor::new(current_gen, lsn))
    }

    pub fn to_string_repr(self) -> String {
        format!("c:{}:{}", self.gen, self.lsn)
    }

    /// A cursor is stale when its generation does not match the index.
    pub fn is_stale(self, current_gen: u64) -> bool {
        self.gen != current_gen
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "c:{}:{}", self.gen, self.lsn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let c = Cursor::new(7, 481221);
        assert_eq!(c.to_string_repr(), "c:7:481221");
        assert_eq!(Cursor::parse("c:7:481221"), Some(c));
        assert_eq!(Cursor::parse(" c:0:0 "), Some(Cursor::ZERO));
    }

    #[test]
    fn malformed() {
        assert_eq!(Cursor::parse("c:7"), None);
        assert_eq!(Cursor::parse("c:7:1:2"), None);
        assert_eq!(Cursor::parse("481221"), None);
    }

    #[test]
    fn bare_lsn_with_gen() {
        assert_eq!(Cursor::parse_with_gen("42", 3), Some(Cursor::new(3, 42)));
        assert_eq!(Cursor::parse_with_gen("c:1:2", 3), Some(Cursor::new(1, 2)));
    }

    #[test]
    fn staleness() {
        assert!(Cursor::new(6, 9001).is_stale(7));
        assert!(!Cursor::new(7, 9001).is_stale(7));
    }
}
