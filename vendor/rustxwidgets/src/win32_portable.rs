//! Portable (platform-free) helpers for the NWG/Win32 backend.
//!
//! These run on every host (unit-tested on Linux); the Win32 API calls in
//! `backends_nwg_adapter` stay thin shells over these pure functions.
//!
//! Parity rule: the menu model speaks GTK (`_` mnemonics, see corro's
//! `menu::mnemonic_label`); each backend translates to its native marker.
//! Win32 uses `&`, so this conversion must run exactly once, here.

/// Convert a GTK-mnemonic label (`_` marks the mnemonic, `__` is a literal
/// underscore) to Win32 (`&` marks the mnemonic, `&&` is literal). A literal
/// `&` in the model is escaped so it can never become a marker.
#[cfg(any(windows, test))]
pub(crate) fn gtk_mnemonic_to_win32(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            // A literal `&` in the model must survive Win32 (where `&`
            // marks the mnemonic). Escape it; markers we introduce below
            // stay single.
            out.push_str("&&");
        } else if c == '_' {
            match chars.peek() {
                Some(&'_') => {
                    // GTK `__` = literal underscore shown without mnemonic.
                    // Win32 spells that `&&` (single `&` would eat it).
                    out.push_str("&&_");
                    chars.next();
                }
                Some(_) => {
                    // GTK `_X` mnemonic marker -> Win32 `&X`.
                    out.push('&');
                }
                None => {
                    // Trailing `_` has no marked char; show it literally.
                    out.push('_');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The `&`-marked character of a Win32 menu label, uppercased.
///
/// `None` for a label with no marker, or one whose only marker is an escape
/// (`&&` / `&&_`), so a caller asking "does this label answer to Alt+key?"
/// gets "no" rather than a mnemonic the OS does not actually install.
#[cfg(any(windows, test))]
pub(crate) fn win32_mnemonic_of(label: &str) -> Option<char> {
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        if c != '&' {
            continue;
        }
        match chars.next() {
            // `&&` (literal `&`) and `&&_` (literal `_`) both spell the
            // marker away, so nothing is registered — but the consumed `_` is
            // real text, so keep scanning past it.
            Some('&') => continue,
            Some(m) => return Some(m.to_ascii_uppercase()),
            // A trailing `&` marks nothing.
            None => return None,
        }
    }
    None
}

/// The zero-based position of the first top-level menu entry whose mnemonic is
/// `keyval` (already uppercased), or `None`.
///
/// Mirrors how the OS resolves a top-level mnemonic: it scans the menu bar
/// left to right and takes the first match, so this must too — matching a
/// later entry instead makes the wrong menu pop up. Entries with no marker
/// are skipped rather than treated as a match on their first letter.
///
/// Split out from the Win32 code so the "which entry?" decision is unit
/// testable on any host; the caller only supplies the labels.
#[cfg(any(windows, test))]
pub(crate) fn find_win32_top_level_mnemonic(
    labels: &[&str],
    keyval: u32,
) -> Option<usize> {
    let want = char::from_u32(keyval)?.to_ascii_uppercase();
    labels.iter().position(|l| win32_mnemonic_of(l) == Some(want))
}

/// Win32 virtual-key code for Return (also arrives for numpad Enter).
#[cfg(any(windows, test))]
pub(crate) const VK_RETURN_CODE: u32 = 0x0D;

/// True when pressing this Win32 virtual key (and consuming the KEYDOWN)
/// will be followed by exactly one WM_CHAR that the native control would
/// insert natively: Backspace, Tab, space, digits, letters, the numpad
/// (NumLock on) and OEM punctuation. Callers suppress that WM_CHAR after
/// consuming, so the character is not inserted a second time ("==" for
/// '='). Anything else — arrows, nav cluster, function keys, modifiers —
/// produces no WM_CHAR and must not arm suppression: a stale flag would
/// swallow the *next* genuine character. Callers therefore *set* (not
/// just arm) this value on every consumed KEYDOWN. Pure logic,
/// unit-tested below.
#[cfg(any(windows, test))]
pub(crate) fn vk_produces_wm_char(vk: u32) -> bool {
    matches!(
        vk,
        0x08 | 0x09                // Backspace, Tab
            | 0x20                 // Space
            | 0x30..=0x39          // digits
            | 0x41..=0x5A          // letters
            | 0x60..=0x6F          // numpad (caller adjusts for NumLock off)
            | 0xBA..=0xC0          // OEM ; = , - . / `
            | 0xDB..=0xDF          // OEM [ \ ] '
            | 0xE2                 // OEM <>
    )
}

/// Decide whether a key event must yield to OS menu/accelerator handling
/// instead of reaching the widget callback. Pure Alt (+key, no Ctrl) belongs
/// to menus (GTK parity: Alt never edits entry text); Ctrl+Alt (AltGr) is
/// international text input and must still reach the widget.
#[cfg(any(windows, test))]
pub(crate) fn should_yield_to_menu(alt_held: bool, ctrl_held: bool) -> bool {
    alt_held && !ctrl_held
}

/// Decide whether a WM_KEYDOWN in a text entry must fire the stored
/// `connect_activate` callback (GTK `activate` parity) instead of the
/// generic key callback. Mirrors GTK4, where the entry's CAPTURE-phase
/// handler consumes RETURN before `on_key_raw` ever fires: when an activate
/// callback is registered, Return belongs to it exclusively, so exactly one
/// submit path fires per press on every backend.
#[cfg(any(windows, test))]
pub(crate) fn entry_return_fires_activate(vk: u32, has_activate_cb: bool) -> bool {
    vk == VK_RETURN_CODE && has_activate_cb
}

/// Pure table match for [`is_nonprinting_win_vk`]: true for Win32
/// virtual-key codes that carry no character and must never reach the
/// printable-text path (F1–F24, Insert, PrintScreen, legacy OEM keys,
/// Windows/App menu keys). Unmapped keys would otherwise mistype: e.g.
/// VK_F1 (0x70) reads as ASCII 'p'. Platform-free so it is unit-tested
/// directly; see `is_nonprinting_win_vk` for the platform gate.
#[cfg(any(windows, test))]
fn is_nonprinting_win_vk_impl(vk: u32) -> bool {
    matches!(
        vk,
        // F1–F24.
        0x70..=0x87
        // Legacy OEM/control keys sitting in the ASCII printable range:
        // SELECT ')', PRINT '*', EXECUTE '+', SNAPSHOT ',', INSERT '-',
        // HELP '/'.
        | 0x29..=0x2D | 0x2F
        // Windows / context-menu keys ('[', '\\', ']')—the OS handles
        // these (Start menu, context menu); the app must not type them.
        | 0x5B..=0x5D
    )
}

/// True when `vk` is a non-character Win32 virtual key (see
/// `is_nonprinting_win_vk_impl`). Always false off Windows: there key
/// values are GDK keysyms where ASCII really is ASCII, so the same numeric
/// values (e.g. 0x70 'p') MUST stay printable.
#[cfg(windows)]
pub fn is_nonprinting_win_vk(vk: u32) -> bool {
    is_nonprinting_win_vk_impl(vk)
}

#[cfg(not(windows))]
pub fn is_nonprinting_win_vk(_vk: u32) -> bool {
    false
}

/// Build the Win32 file-dialog filter spec from `(display name, patterns)`
/// pairs. Win32 wants `Name (*.a;*.b)|*.a;*.b` entries joined by `|` (the
/// name's parenthesised list is cosmetic; the second field is what filters).
/// Empty input means "no filter" (the dialog then shows all files).
/// Pure + unit-tested (the NWG backend has no other way to test its
/// dialog strings on a Linux host).
#[cfg(any(windows, test))]
pub(crate) fn join_dialog_filters(filters: &[(&str, &[&str])]) -> Option<String> {
    if filters.is_empty() {
        return None;
    }
    Some(
        filters
            .iter()
            .map(|(name, pats)| format!("{} ({})|{}", name, pats.join(";"), pats.join(";")))
            .collect::<Vec<_>>()
            .join("|"),
    )
}

/// Distribute `avail` pixels over one box axis, GTK fill-parity semantics:
/// fixed children keep `desired` sizes, expand children split the remainder
/// (plus one extra pixel each for the first few, so no pixel is lost to
/// integer division). Returns `(position, size)` per child starting at
/// `start`, advancing by `spacing` between children.
#[cfg(any(windows, test))]
pub(crate) fn distribute_spans(
    start: i32,
    avail: i32,
    spacing: i32,
    desired: &[i32],
    expand: &[bool],
) -> Vec<(i32, i32)> {
    let n = desired.len();
    let mut out = Vec::with_capacity(n);
    if n == 0 {
        return out;
    }
    let spacing_total = spacing * (n as i32 - 1).max(0);
    let fixed_total: i32 = desired
        .iter()
        .zip(expand.iter())
        .map(|(&d, &e)| if e { 0 } else { d.max(0) })
        .sum();
    let remaining = (avail - fixed_total - spacing_total).max(0);
    let expand_count = expand.iter().filter(|&&e| e).count();
    let (each, mut extra) = if expand_count > 0 {
        (
            remaining / expand_count as i32,
            (remaining % expand_count as i32) as usize,
        )
    } else {
        (0, 0)
    };
    let mut pos = start;
    for i in 0..n {
        let size = if expand[i] {
            let bonus = if extra > 0 {
                extra -= 1;
                1
            } else {
                0
            };
            each + bonus
        } else {
            desired[i].max(0)
        };
        out.push((pos, size));
        pos += size + spacing;
    }
    out
}

/// Geometry for a dialog with stacked content over a bottom-right button
/// row (GtkDialog anatomy: content area + action area). `content` holds
/// `(natural_width, natural_height, is_box)` per content child; `buttons`
/// holds `(natural_width, natural_height)` per button. Returns content rects
/// then button rects. Boxes share leftover content height equally; leaves
/// keep natural heights; nothing overlaps the button row.
#[cfg(any(windows, test))]
pub(crate) struct DlgRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[cfg(any(windows, test))]
pub(crate) fn dialog_layout_geometry(
    client_w: i32,
    client_h: i32,
    content: &[(i32, i32, bool)],
    buttons: &[(i32, i32)],
) -> (Vec<DlgRect>, Vec<DlgRect>) {
    const MARGIN: i32 = 12;
    const GAP: i32 = 8;
    let btn_h: i32 = buttons.iter().map(|&(_, h)| h).max().unwrap_or(0);
    let btn_y = if buttons.is_empty() {
        client_h - MARGIN
    } else {
        client_h - MARGIN - btn_h
    };
    let content_w = (client_w - 2 * MARGIN).max(0);
    let content_bottom = if buttons.is_empty() {
        (client_h - MARGIN).max(MARGIN)
    } else {
        (btn_y - GAP).max(MARGIN)
    };
    let leaves_total: i32 = content
        .iter()
        .filter(|&&(_, _, is_box)| !is_box)
        .map(|&(_, h, _)| h.max(0))
        .sum();
    let gaps_total = GAP * (content.len() as i32 - 1).max(0);
    let box_count = content.iter().filter(|&&(_, _, is_box)| is_box).count();
    let box_each = if box_count > 0 {
        (content_bottom - MARGIN - leaves_total - gaps_total).max(0) / box_count as i32
    } else {
        0
    };
    let mut rects = Vec::with_capacity(content.len());
    let mut y = MARGIN;
    for &(_nw, nh, is_box) in content {
        let h = if is_box { box_each } else { nh.max(0) };
        rects.push(DlgRect {
            x: MARGIN,
            y,
            w: content_w,
            h,
        });
        y += h + GAP;
    }
    let mut btn_rects = Vec::with_capacity(buttons.len());
    let btns_total: i32 =
        buttons.iter().map(|&(w, _)| w).sum::<i32>() + GAP * (buttons.len() as i32 - 1).max(0);
    let mut bx = client_w - MARGIN - btns_total;
    for &(bw, bh) in buttons {
        btn_rects.push(DlgRect {
            x: bx,
            y: btn_y,
            w: bw,
            h: bh,
        });
        bx += bw + GAP;
    }
    (rects, btn_rects)
}

#[cfg(test)]
mod portable_win32_tests {
    use super::*;

    #[test]
    fn win32_mnemonic_finds_the_marked_character() {
        assert_eq!(win32_mnemonic_of("&File"), Some('F'));
        assert_eq!(win32_mnemonic_of("Save &As..."), Some('A'));
        // Only the FIRST marker is the mnemonic; the rest are literal text.
        assert_eq!(win32_mnemonic_of("&File &Edit"), Some('F'));
    }

    #[test]
    fn win32_mnemonic_ignores_escapes_and_bare_labels() {
        // `&&` spells a literal `&`, not a mnemonic.
        assert_eq!(win32_mnemonic_of("Copy && Paste"), None);
        // `&&_` spells a literal `_`.
        assert_eq!(win32_mnemonic_of("A&&_B"), None);
        // No marker at all: the OS installs no mnemonic, so none is reported.
        assert_eq!(win32_mnemonic_of("Help"), None);
        // A trailing `&` marks nothing.
        assert_eq!(win32_mnemonic_of("Odd&"), None);
    }

    #[test]
    fn win32_top_level_mnemonic_takes_the_first_match_left_to_right() {
        let labels = ["&File", "&Edit", "&View"];
        assert_eq!(find_win32_top_level_mnemonic(&labels, 'f' as u32), Some(0));
        // Case-insensitive, as the OS treats mnemonics.
        assert_eq!(find_win32_top_level_mnemonic(&labels, 'E' as u32), Some(1));
        assert_eq!(find_win32_top_level_mnemonic(&labels, 'e' as u32), Some(1));
        assert_eq!(find_win32_top_level_mnemonic(&labels, 'z' as u32), None);
    }

    #[test]
    fn win32_top_level_mnemonic_skips_unmarked_entries() {
        // An unmarked entry must not be matched on its first letter: 'P' is
        // plain text in "Print", not an Alt+P accelerator.
        let labels = ["File", "&Edit", "Print"];
        assert_eq!(find_win32_top_level_mnemonic(&labels, 'f' as u32), None);
        assert_eq!(find_win32_top_level_mnemonic(&labels, 'e' as u32), Some(1));
        assert_eq!(find_win32_top_level_mnemonic(&labels, 'p' as u32), None);
    }

    #[test]
    fn win32_top_level_mnemonic_handles_empty_and_garbage_input() {
        assert_eq!(find_win32_top_level_mnemonic(&[], 'f' as u32), None);
        // A keyval that is not a char (e.g. a VK_ constant) matches nothing
        // rather than panicking on `char::from_u32`.
        assert_eq!(find_win32_top_level_mnemonic(&["&File"], 0xF0), None);
    }

    #[test]
    fn mnemonic_converts_gtk_marker_to_win32() {
        assert_eq!(gtk_mnemonic_to_win32("_File"), "&File");
        assert_eq!(gtk_mnemonic_to_win32("Save _As..."), "Save &As...");
        assert_eq!(gtk_mnemonic_to_win32("_Rename Sheet"), "&Rename Sheet");
    }

    #[test]
    fn mnemonic_escapes_survive_round_trip() {
        // GTK `__` (literal underscore) must display literally on Win32.
        assert_eq!(gtk_mnemonic_to_win32("A__B"), "A&&_B");
        // A literal `&` in the model must not become a mnemonic marker.
        assert_eq!(gtk_mnemonic_to_win32("Copy & Paste"), "Copy && Paste");
        // Trailing `_` marks nothing; show it.
        assert_eq!(gtk_mnemonic_to_win32("abc_"), "abc_");
        // No marker: plain passthrough, no invented mnemonic.
        assert_eq!(gtk_mnemonic_to_win32("Help"), "Help");
        assert_eq!(gtk_mnemonic_to_win32(""), "");
    }

    #[test]
    fn pure_alt_yields_to_menu_but_altgr_does_not() {
        assert!(should_yield_to_menu(true, false));
        assert!(!should_yield_to_menu(false, false));
        assert!(!should_yield_to_menu(true, true));
        assert!(!should_yield_to_menu(false, true));
    }

    #[test]
    fn nonprinting_vk_table_covers_mistypers() {
        // Function keys read as ASCII letters/punct without this guard.
        assert!(is_nonprinting_win_vk_impl(0x70)); // F1, not 'p'
        assert!(is_nonprinting_win_vk_impl(0x71)); // F2, not 'q'
        assert!(is_nonprinting_win_vk_impl(0x7B)); // F12, not '{'
        assert!(is_nonprinting_win_vk_impl(0x87)); // F24
        assert!(is_nonprinting_win_vk_impl(0x2D)); // Insert, not '-'
        assert!(is_nonprinting_win_vk_impl(0x2C)); // PrintScreen, not ','
        assert!(is_nonprinting_win_vk_impl(0x5B)); // LWin, not '['
        assert!(is_nonprinting_win_vk_impl(0x5D)); // Apps, not ']'
        // Real text and mapped control keys stay printable/handled.
        for vk in ['a' as u32, 'Z' as u32, '0' as u32, ' ' as u32, 0x0D, 0x1B] {
            assert!(!is_nonprinting_win_vk_impl(vk), "vk={vk:#X}");
        }
    }

    #[test]
    fn wm_char_suppression_matches_producers() {
        // Consuming these KEYDOWNs is followed by a WM_CHAR: suppress it.
        for vk in [0x08u32, 0x09, 0x20, 0x30, 0x39, 0x41, 0x5A, 0xBB, 0xBC, 0xDB, 0xE2] {
            assert!(vk_produces_wm_char(vk), "vk={vk:#X}");
        }
        // These never produce WM_CHAR: arming suppression would eat the
        // *next* genuine character (stale flag).
        for vk in [
            0x0Du32, 0x1B, // Enter/Escape (swallowed explicitly elsewhere)
            0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, // nav cluster
            0x2D, 0x2E, // Insert, Delete
            0x70, 0x71, 0x7B, 0x87, // F-keys (their codes read as ASCII!)
            0x10, 0x11, 0x12, // modifiers
            0x5B, 0x5C, 0x5D, // Win/App keys
        ] {
            assert!(!vk_produces_wm_char(vk), "vk={vk:#X}");
        }
    }

    #[test]
    fn return_routes_to_activate_only_when_registered() {
        assert!(entry_return_fires_activate(0x0D, true));
        assert!(!entry_return_fires_activate(0x0D, false));
        assert!(!entry_return_fires_activate(0x1B, true));
        assert!(!entry_return_fires_activate(0x09, true));
        assert!(!entry_return_fires_activate(0x41, true));
    }

    #[test]
    fn distribute_gives_remainder_to_first_expanders() {
        // Formula-bar shape: two fixed labels + one expanding entry.
        // 246 = 300 - 20 - 30 - 2*2 spacing - 0 (start offset is outside).
        let got = distribute_spans(5, 300, 2, &[20, 30, 0], &[false, false, true]);
        assert_eq!(got, vec![(5, 20), (27, 30), (59, 246)]);
    }

    /// The fx-bar contract (addr label, fx label, expanding entry): the
    /// entry fills everything past the fitted labels, spans never overlap,
    /// and the trailing edge lands exactly on start+avail (no gap, no
    /// overrun). This is the reported nwg cram shape (labels left, entry
    /// squeezed): any regression here reopens it on every backend.
    #[test]
    fn fx_bar_entry_fills_remainder_without_overlap() {
        // 800px bar, labels fitted to text, entry expanding.
        let got = distribute_spans(5, 790, 2, &[24, 44, 0], &[false, false, true]);
        assert_eq!(got.len(), 3);
        // Strictly increasing, non-overlapping, in order.
        for w in got.windows(2) {
            assert!(
                w[0].0 + w[0].1 + 2 <= w[1].0,
                "spans must not overlap (spacing 2): {got:?}"
            );
        }
        // Entry takes the whole remainder: 790 - 24 - 44 - 2*2 spacing.
        assert_eq!(got[2].1, 790 - 24 - 44 - 4);
        // Trailing edge lands exactly on start+avail.
        assert_eq!(got[2].0 + got[2].1, 5 + 790);
        // Entry is usefully wide (not a sliver): most of the bar.
        assert!(got[2].1 > 600, "entry squeezed: {got:?}");
    }

    /// Span-math property sweep: for degenerate inputs (zero/narrow bars,
    /// zero-size or missing expanders, hidden zero-size children) spans
    /// must never overlap and must stay within [start, start+avail] unless
    /// fixed content provably overflows (documented: expander 0, fixed
    /// keep desired and run past the edge — still without overlap).
    /// A crammed/overlapping fx bar on any backend fails here.
    #[test]
    fn distribute_never_overlaps_or_loses_remainder() {
        let desired_sets: &[&[i32]] = &[
            &[24, 44, 0],
            &[0, 0, 0],
            &[60, 60, 60],
            &[24],
            &[0],
            &[100, 10, 0, 0],
        ];
        let flag_sets: &[&[bool]] = &[
            &[false, false, true],
            &[true, true, true],
            &[false, false, false],
            &[true],
            &[false],
            &[false, true, false, true],
        ];
        for &avail in &[0, 5, 10, 68, 69, 100, 674, 2000] {
            for ds in desired_sets {
                for fs in flag_sets {
                    if ds.len() != fs.len() {
                        continue;
                    }
                    let got = distribute_spans(5, avail, 2, ds, fs);
                    assert_eq!(got.len(), ds.len());
                    // No pairwise overlap (spacing respected).
                    for w in got.windows(2) {
                        assert!(
                            w[0].0 + w[0].1 + 2 <= w[1].0,
                            "overlap at avail={avail} desired={ds:?} flags={fs:?}: {got:?}"
                        );
                    }
                    // Expanders split exactly the remainder (no lost pixel).
                    let fixed: i32 = ds
                        .iter()
                        .zip(fs.iter())
                        .map(|(&d, &e)| if e { 0 } else { d.max(0) })
                        .sum();
                    let spacing = 2 * (ds.len() as i32 - 1).max(0);
                    let remainder = (avail - fixed - spacing).max(0);
                    let exp_total: i32 = got
                        .iter()
                        .zip(fs.iter())
                        .map(|(&( _, s), &e)| if e { s } else { 0 })
                        .sum();
                    // Expanders split exactly the remainder (no lost pixel);
                    // with no expanders the leftover trailing gap is correct.
                    let want = if fs.iter().any(|&e| e) {
                        remainder
                    } else {
                        0
                    };
                    assert_eq!(
                        exp_total, want,
                        "lost remainder at avail={avail} desired={ds:?} flags={fs:?}: {got:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn distribute_splits_remainder_pixel_fairly() {
        // 100px over two expanders: 50/50, no lost pixel.
        let got = distribute_spans(0, 100, 0, &[0, 0], &[true, true]);
        assert_eq!(got, vec![(0, 50), (50, 50)]);
        // 101px: first expander takes the odd pixel (GTK fill parity).
        let got = distribute_spans(0, 101, 0, &[0, 0], &[true, true]);
        assert_eq!(got, vec![(0, 51), (51, 50)]);
    }

    #[test]
    fn distribute_clamps_when_fixed_overflow() {
        let got = distribute_spans(5, 40, 2, &[30, 30, 0], &[false, false, true]);
        assert_eq!(got[2].1, 0);
        assert_eq!(got[0], (5, 30));
    }

    #[test]
    fn dialog_single_entry_sits_above_buttons() {
        // Prompt/rename shape: one 26px entry + Cancel/OK buttons.
        let (content, buttons) =
            dialog_layout_geometry(400, 140, &[(0, 26, false)], &[(96, 28), (96, 28)]);
        assert_eq!(content.len(), 1);
        assert_eq!(buttons.len(), 2);
        // Buttons bottom-right.
        assert_eq!(buttons[1].x + buttons[1].w, 400 - 12);
        assert_eq!(buttons[0].y, 140 - 12 - 28);
        // Entry full width, above the button row, no overlap.
        assert_eq!(content[0].x, 12);
        assert_eq!(content[0].w, 400 - 24);
        assert!(content[0].y + content[0].h + 8 <= buttons[0].y);
    }

    #[test]
    fn dialog_box_child_takes_leftover_height() {
        let (content, _) =
            dialog_layout_geometry(400, 300, &[(0, 26, false), (0, 0, true)], &[(96, 28)]);
        assert_eq!(content[0].h, 26);
        // Leftover: 300 - 12(top) - 26 - 8(gap) - 8(above buttons) - 28 - 12 = 206.
        assert_eq!(content[1].h, 206);
        assert_eq!(content[1].y, 12 + 26 + 8);
    }

    #[test]
    fn win32_filter_spec_formats_and_joins() {
        // No filters -> None (dialog unfiltered, not an empty spec).
        assert_eq!(join_dialog_filters(&[]), None);
        // Single filter: name list is cosmetic, second field filters.
        assert_eq!(
            join_dialog_filters(&[("Corro workbooks", &["*.corro"])]),
            Some("Corro workbooks (*.corro)|*.corro".to_string())
        );
        // Multiple patterns in one filter are semicolon-joined in both fields.
        assert_eq!(
            join_dialog_filters(&[("Spreadsheets", &["*.corro", "*.csv", "*.tsv", "*.ods"])]),
            Some(
                "Spreadsheets (*.corro;*.csv;*.tsv;*.ods)|*.corro;*.csv;*.tsv;*.ods"
                    .to_string()
            )
        );
        // Multiple filters are pipe-joined (Win32's separator).
        assert_eq!(
            join_dialog_filters(&[("Spreadsheets", &["*.corro"]), ("All files", &["*.*"])]),
            Some("Spreadsheets (*.corro)|*.corro|All files (*.*)|*.*".to_string())
        );
    }
}
