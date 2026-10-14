//! Bounded edit-distance matching for fuzzy search (§10/§17 v2).
//!
//! Two primitives, both case-insensitive over ASCII and both **bounded**: they
//! give up as soon as the distance is known to exceed `k`, so a query never pays
//! for a full O(m·n) table when the answer is "no".
//!
//! * [`levenshtein_within`] — the classic whole-string distance, `None` when it
//!   exceeds `k`.
//! * [`substring_within`] — the search-relevant one: does *some* substring of
//!   `hay` lie within `k` edits of `needle`? This is what a name search wants
//!   (the query term appears somewhere in the name, approximately), and it is
//!   computed with a first row of zeros (free prefix) and the minimum taken over
//!   the last row (free suffix) — the standard approximate-substring DP.
//!
//! Both return the **distance** (smaller = better) so callers can rank. The
//! result is a plain `u32`, not a float, so ordering is exact and deterministic
//! — no floating-point ties to break.

/// Case-insensitive bound wrapper: lowercase both sides so `Foo`/`foo` compare
/// equal without allocating per call site's concern.
#[inline]
fn lower(b: &[u8]) -> Vec<u8> {
    b.iter().map(|c| c.to_ascii_lowercase()).collect()
}

/// Edit distance between `a` and `b`, or `None` when it exceeds `k`.
///
/// ASCII-case-insensitive. Uses two rolling rows; the band around the diagonal
/// is not exploited (a further optimization the design may add), but the early
/// `min > k` exit keeps the common "far apart" case cheap.
pub fn levenshtein_within(a: &[u8], b: &[u8], k: u32) -> Option<u32> {
    let al = lower(a);
    let bl = lower(b);
    levenshtein_lower(&al, &bl, k)
}

/// As [`levenshtein_within`] but assumes both inputs are already lowercase.
fn levenshtein_lower(a: &[u8], b: &[u8], k: u32) -> Option<u32> {
    // Trim the common prefix/suffix first: a big win on similar names, and it
    // keeps the DP table small.
    let mut lo = 0usize;
    let min_len = a.len().min(b.len());
    while lo < min_len && a[lo] == b[lo] {
        lo += 1;
    }
    let mut a_hi = a.len();
    let mut b_hi = b.len();
    while a_hi > lo && b_hi > lo && a[a_hi - 1] == b[b_hi - 1] {
        a_hi -= 1;
        b_hi -= 1;
    }
    let a = &a[lo..a_hi];
    let b = &b[lo..b_hi];

    let (n, m) = (a.len(), b.len());
    // Whichever is shorter goes on the "column" axis to keep the row small.
    if n.abs_diff(m) as u32 > k {
        return None;
    }
    if n == 0 {
        return (m as u32 <= k).then_some(m as u32);
    }

    let mut prev: Vec<u32> = (0..=m as u32).collect();
    let mut cur: Vec<u32> = vec![0; m + 1];
    for i in 1..=n {
        cur[0] = i as u32;
        let mut row_min = cur[0];
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            let v = (prev[j - 1] + cost).min(prev[j] + 1).min(cur[j - 1] + 1);
            cur[j] = v;
            row_min = row_min.min(v);
        }
        if row_min > k {
            return None; // every path from here on is already too expensive
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let d = prev[m];
    (d <= k).then_some(d)
}

/// The smallest edit distance between `needle` and **any substring** of `hay`,
/// or `None` when that exceeds `k`.
///
/// A first DP row of zeros means a match may start anywhere in `hay` (free
/// prefix); taking the minimum of the final row means it may end anywhere (free
/// suffix). That is precisely "the needle occurs in the haystack, approximately".
///
/// ASCII-case-insensitive.
pub fn substring_within(hay: &[u8], needle: &[u8], k: u32) -> Option<u32> {
    let h = lower(hay);
    let nd = lower(needle);
    substring_lower(&h, &nd, k)
}

/// As [`substring_within`] but assumes both inputs are already lowercase.
pub fn substring_lower(hay: &[u8], needle: &[u8], k: u32) -> Option<u32> {
    let m = needle.len();
    if m == 0 {
        // The empty needle matches at distance 0 anywhere.
        return Some(0);
    }
    if hay.is_empty() {
        return (m as u32 <= k).then_some(m as u32);
    }
    // Exact substring is the common case and O(n·m) with a memchr-backed find is
    // far cheaper than a full DP; only pay for the DP when there is no exact hit.
    if memchr::memmem::find(hay, needle).is_some() {
        return Some(0);
    }
    if k == 0 {
        return None;
    }

    let n = hay.len();
    // prev[j] = distance from needle[..i] to some substring of hay ending at j-1,
    // with the start free. Row 0 = all zeros.
    let mut prev: Vec<u32> = vec![0; n + 1];
    let mut cur: Vec<u32> = vec![0; n + 1];
    let mut best = u32::MAX;
    for i in 1..=m {
        cur[0] = i as u32;
        let mut row_min = cur[0];
        for j in 1..=n {
            let cost = if needle[i - 1] == hay[j - 1] { 0 } else { 1 };
            let v = (prev[j - 1] + cost).min(prev[j] + 1).min(cur[j - 1] + 1);
            cur[j] = v;
            row_min = row_min.min(v);
        }
        if row_min > k {
            // Even the best continuation cannot come back under k.
            return None;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    // Free suffix: the answer is the best over every end position.
    for v in prev.iter().take(n + 1) {
        best = best.min(*v);
    }
    (best <= k).then_some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_distance_zero() {
        assert_eq!(levenshtein_within(b"main.rs", b"main.rs", 0), Some(0));
        assert_eq!(
            levenshtein_within(b"MAIN.RS", b"main.rs", 0),
            Some(0),
            "case-insensitive"
        );
    }

    #[test]
    fn known_levenshtein_distances() {
        // classic examples
        assert_eq!(levenshtein_within(b"kitten", b"sitting", 3), Some(3));
        assert_eq!(levenshtein_within(b"flaw", b"lawn", 2), Some(2));
        assert_eq!(levenshtein_within(b"", b"abc", 3), Some(3));
        assert_eq!(levenshtein_within(b"abc", b"", 3), Some(3));
        // one substitution / one insertion / one deletion
        assert_eq!(levenshtein_within(b"main", b"moin", 1), Some(1));
        assert_eq!(levenshtein_within(b"main", b"mainx", 1), Some(1));
        assert_eq!(levenshtein_within(b"main", b"man", 1), Some(1));
        // a transposition is two edits without a Damerau variant — assert that
        // rather than pretend otherwise.
        assert_eq!(levenshtein_within(b"main", b"mian", 2), Some(2));
        assert_eq!(levenshtein_within(b"main", b"mian", 1), None);
    }

    #[test]
    fn bound_is_enforced() {
        // distance 3, budget 2 -> None
        assert_eq!(levenshtein_within(b"kitten", b"sitting", 2), None);
        assert_eq!(levenshtein_within(b"abc", b"xyz", 2), None);
        assert_eq!(levenshtein_within(b"abc", b"xyz", 3), Some(3));
        // length difference alone can exceed the budget
        assert_eq!(levenshtein_within(b"a", b"abcd", 2), None);
    }

    #[test]
    fn substring_match_anywhere() {
        // "ain" occurs inside "main.rs" exactly
        assert_eq!(substring_within(b"main.rs", b"ain", 0), Some(0));
        // substring, not whole-string: prefix/suffix are free
        assert_eq!(substring_within(b"main.rs", b"main.rs", 0), Some(0));
        assert_eq!(substring_within(b"xxxmain.rs", b"main.rs", 0), Some(0));
        assert_eq!(substring_within(b"main.rsxxx", b"main.rs", 0), Some(0));
        // one edit away inside
        assert_eq!(substring_within(b"main.rs", b"main.js", 1), Some(1));
    }

    #[test]
    fn substring_respects_bound() {
        assert_eq!(substring_within(b"main.rs", b"zzzz", 2), None);
        assert_eq!(substring_within(b"abc", b"xyz", 2), None);
        assert_eq!(substring_within(b"abc", b"xyz", 3), Some(3));
    }

    #[test]
    fn substring_smaller_than_whole_string_distance() {
        // Whole-string distance of "lib" vs "lib.rs" is 3, but "lib" is a
        // substring, so the substring distance is 0.
        assert_eq!(levenshtein_within(b"lib.rs", b"lib", 1), None);
        assert_eq!(substring_within(b"lib.rs", b"lib", 0), Some(0));
    }

    #[test]
    fn empty_needle_matches_everything() {
        assert_eq!(substring_within(b"anything", b"", 0), Some(0));
    }

    #[test]
    fn deterministic_ties_are_integers() {
        // The score is an integer; equal-distance candidates tie exactly and the
        // caller breaks ties by path. Nothing here can produce an f64.
        let d1 = substring_within(b"abcd", b"abce", 1).unwrap();
        let d2 = substring_within(b"abcf", b"abce", 1).unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn prefix_suffix_trim_does_not_change_result() {
        // A long shared prefix must not fool the trimmed DP.
        assert_eq!(
            levenshtein_within(b"aaaaaaaaaab", b"aaaaaaaaaac", 1),
            Some(1)
        );
        assert_eq!(levenshtein_within(b"aaaaaaaaaab", b"aaaaaaaaaac", 0), None);
    }
}
